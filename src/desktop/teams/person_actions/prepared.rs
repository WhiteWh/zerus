//! Resolve data without creating Person authority. The same operation encoder
//! is used by the CLI and the window; no renderer-supplied Actor is accepted.
use super::required;
use crate::{
    config::Config,
    teams::{
        catalog, ops,
        store::{self, rows},
    },
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::sync::Arc;
use zerus_client_core::{desktop::team_person_actions as wire, upstream_teams::action::TeamAction};
use zerus_team_plan::{model::Team, problem::code, Problem};

type Result<T> = std::result::Result<T, Problem>;
mod phases;

pub(super) struct Prepared {
    pub plan: Arc<wire::Plan>,
    pub request: ops::Request,
    /// What a window Route keeps from the stored task, for the native review
    /// (zerus-3u5yu): the dependencies and write paths it binds, and the
    /// assignee it keeps (`m1`) or clears (`m1 → none`). Empty otherwise.
    pub kept: Vec<(&'static str, Value)>,
}

pub(super) fn actor() -> ops::Actor {
    ops::Actor::Person {
        person: "local".into(),
        origin: ops::PersonOrigin::DesktopUi,
    }
}

impl Prepared {
    pub fn empty_facts(&self) -> Result<String> {
        wire::digest(&(&self.request, vec![Value::Null])).ok_or_else(required)
    }
    pub fn resolve(config: &Config, plan: Arc<wire::Plan>) -> Result<Self> {
        if !plan.valid() {
            return Err(required());
        }
        let wire::Action::Team { change } = &plan.action else {
            let (project, uuid, role, pipeline) = match &plan.action {
                wire::Action::NewTask { project, .. } => {
                    (project, plan.request_id.clone(), None, None)
                }
                wire::Action::Intake {
                    project,
                    task,
                    role,
                    pipeline,
                    ..
                } => (project, task.clone(), role.clone(), pipeline.clone()),
                wire::Action::Team { .. } => unreachable!(),
            };
            // Pure canonical encoding: catalog reads are done before project
            // custody, never while rechecking under a project/Teams writer.
            let request = ops::Request {
                request_id: plan.request_id.clone(),
                team: plan.team.clone(),
                expected_revision: plan.expected_revision,
                operation: ops::Operation::AddTask {
                    task: zerus_team_plan::model::TaskRef {
                        project: project.clone(),
                        uuid,
                    },
                    role,
                    pipeline,
                },
            };
            return Ok(Self {
                plan,
                request,
                kept: Vec::new(),
            });
        };
        let call = plan.call().ok_or_else(required)?;
        let (invocation, _) =
            zerus_client_core::upstream_teams::action::prepare(&call).map_err(|_| required())?;
        let mut fields: serde_json::Map<String, Value> =
            serde_json::from_slice(invocation.stdin.as_deref().ok_or_else(required)?)
                .map_err(store::storage)?;
        let variant = match change.as_ref() {
            TeamAction::Settings { .. } => "change_settings".into(),
            TeamAction::ValidationConfig { .. } => "set_validation_config".into(),
            TeamAction::Decide { .. } => "decide_card".into(),
            // Ask is member-only even when a Person could name a node.
            TeamAction::Ask { .. } => return Err(required()),
            _ => change.op().replace('-', "_"),
        };
        fields.insert("op".into(), json!(variant));
        let mut kept = Vec::new();
        if let TeamAction::Route {
            node,
            role,
            keep_task_fields: true,
            ..
        } = change.as_ref()
        {
            // Resolve the exact fields on the service. The request digest
            // covers this ordinary Route, and commit rechecks its revision
            // and the native review's facts while holding the writer.
            let store = store::Store::person_evidence(&config.state)?;
            let task = store.node(&plan.team, *node)?.ok_or_else(|| {
                Problem::new(code::INVALID_STATE, "The task is no longer on this team.")
            })?;
            fields.remove("keep_task_fields");
            fields.insert("deps".into(), json!(task.deps));
            fields.insert("writes".into(), json!(task.writes));
            // The stored assignee is kept only for the same role and only
            // while that member can take it: not the lead and not removed,
            // as the Route writer requires (zerus-3u5yu). Otherwise the
            // Route clears it, and the review says so.
            let lead = store.team(&plan.team)?.map(|team| team.lead);
            let bound = match &task.assignee {
                Some(id) if task.role == *role => store
                    .member(&plan.team, id)?
                    .filter(|member| {
                        member.role == *role
                            && Some(&member.id) != lead.as_ref()
                            && member.state != zerus_team_plan::model::MemberState::Removed
                    })
                    .map(|member| member.id),
                _ => None,
            };
            if let Some(id) = &bound {
                fields.insert("assignee".into(), json!(id));
            }
            kept.push(("deps", json!(task.deps)));
            kept.push(("writes", json!(task.writes)));
            if let Some(id) = &task.assignee {
                let shown = if bound.is_some() {
                    id.clone()
                } else {
                    format!("{id} → none")
                };
                kept.push(("assignee", Value::String(shown)));
            }
        }
        if let TeamAction::Create {
            template,
            expected_template,
            ..
        }
        | TeamAction::StartPlanning {
            template,
            expected_template,
            ..
        } = change.as_ref()
        {
            let catalog = catalog::load(&config.dir)?;
            let bundle = zerus_team_plan::bundle(&catalog, template)?;
            if catalog::template_digest(&bundle)? != *expected_template {
                return Err(Problem::new(
                    code::TEMPLATE_CHANGED,
                    "The template changed since it was shown. Review it again.",
                ));
            }
            fields.insert(
                "bundle".into(),
                serde_json::to_value(bundle).map_err(store::storage)?,
            );
        }
        let request: ops::Request =
            serde_json::from_value(Value::Object(fields)).map_err(store::storage)?;
        if store::encode(&request)?.len() > wire::PLAN_BYTES {
            return Err(required());
        }
        Ok(Self {
            plan,
            request,
            kept,
        })
    }

    pub fn matches(&self, who: &ops::Actor, request: &ops::Request, intent: &wire::Intent) -> bool {
        who == &actor()
            && self.request == *request
            && self.plan.reference().as_ref() == Some(&intent.plan)
            && wire::digest(request).as_deref() == Some(intent.request.as_str())
    }

    pub fn bound_facts(
        &self,
        db: &Connection,
        config: Option<&Config>,
    ) -> Result<(Option<Team>, String)> {
        self.bound_facts_with_phase_resources(db, config, None)
    }

    pub fn bound_facts_with_phase_resources(
        &self,
        db: &Connection,
        config: Option<&Config>,
        custody: Option<&Result<store::phase_controls::task_custody::TaskCustody>>,
    ) -> Result<(Option<Team>, String)> {
        let (team, mut ordinary) = self.facts(db)?;
        use store::phase_controls::task_custody::Selection;
        let selection = match &self.request.operation {
            ops::Operation::MovePhase {
                scope, relative_to, ..
            } => Some(Selection::Interval(scope.phase(), relative_to.phase())),
            ops::Operation::SetPhaseAcceptance { scope, .. } => Some(Selection::One(scope.phase())),
            _ => None,
        };
        if let Some(selection) = selection {
            let held = custody
                .ok_or_else(required)?
                .as_ref()
                .map_err(Clone::clone)?;
            let references =
                held.native_facts(db, team.as_ref().ok_or_else(required)?, selection)?;
            ordinary = wire::digest(&("phase-control-resource-v1", ordinary, references))
                .ok_or_else(required)?;
        }
        if let ops::Operation::AdvanceBase { .. } = &self.request.operation {
            let config = config.ok_or_else(required)?;
            let team = team.ok_or_else(required)?;
            let repo = crate::desktop::teams::repository(config, &team)?;
            let path = crate::platform::canonical(&repo.git_dir).map_err(store::storage)?;
            let identity = crate::platform::file_identity(&path).ok_or_else(required)?;
            let refs = crate::teams::integrate::main_refs(&repo)?;
            let digest = wire::digest(&(ordinary, path, identity.device, identity.inode, refs))
                .ok_or_else(required)?;
            Ok((Some(team), digest))
        } else {
            Ok((team, ordinary))
        }
    }

    /// Point reads only. SQL measures bodies before materializing them. The
    /// fixed projections include native facts which may change independently
    /// of the team's revision, such as an owner card or a task observation.
    pub fn facts(&self, db: &Connection) -> Result<(Option<Team>, String)> {
        let id = &self.request.team;
        let team = point(db, "teams", "id", id, None,
            "id,project,name,goal,home,owner,status,lead,settings,bundle,integration_branch,base_commit,revision,person_paused,next_node", 262_144)?;
        let fields = serde_json::to_value(&self.request).map_err(store::storage)?;
        let mut facts = vec![team.clone()];
        if let ops::Operation::AddTask { task, .. } = &self.request.operation {
            let project = json!(task.project);
            let live = point(
                db,
                "live_tasks",
                "task_uuid",
                &task.uuid,
                Some(("task_project", &project)),
                "team,node",
                512,
            )?;
            if let Some(values) = live.as_array() {
                let home = values
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(required)?;
                let node = values.get(1).ok_or_else(required)?;
                facts.push(point(
                    db,
                    "nodes",
                    "team",
                    home,
                    Some(("id", node)),
                    "data,task_revision,task_stage,task_cancelled,generation",
                    65536,
                )?);
                facts.push(point(
                    db,
                    "task_observations",
                    "team",
                    home,
                    Some(("node", node)),
                    "data,released,attempt,attempt_id,last_attempt_id,attempt_conflict",
                    65536,
                )?);
                // Pending cancellation is independently service-written.
                let pending:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM decisions d WHERE d.team=?1 AND d.node=?2 AND d.state<>'settled' AND d.kind='cancel' AND d.state='pending' AND d.generation=(SELECT generation FROM nodes WHERE team=d.team AND id=d.node) AND NOT EXISTS(SELECT 1 FROM cancel_outcomes c WHERE c.decision=d.seq))",params![home,key_number(node)?],|r|r.get(0)).map_err(store::storage)?;
                facts.push(json!(pending));
            }
            facts.push(live);
        }
        if let Some(node) = fields.get("node") {
            let live = point(db, "nodes", "team", id, Some(("id",node)),
                "data,rank,admitted,routed,hold,hold_owner,hand_back,task_revision,task_stage,task_cancelled,generation,decided_by,integrated,integration_commit,requested,integration_required", 65536)?;
            let archived = point(db, "node_archive", "team", id, Some(("id",node)),
                "data,rank,admitted,routed,hold,hold_owner,hand_back,task_revision,task_stage,task_cancelled,generation,decided_by,integrated,integration_commit,requested,integration_required", 65536)?;
            facts.extend([live, archived]);
            if let Some(node) = node.as_u64().and_then(|v| u32::try_from(v).ok()) {
                facts
                    .push(serde_json::to_value(rows::deps(db, id, node)?).map_err(store::storage)?);
            }
        }
        for (field, table, key, columns, bound) in [
            ("member","members","id","data,state,person_stopped,machine,session_id",65536),
            ("card","cards","seq","kind,node,actor,text,notes,state,decision,decided_actor,created_ms,decision_seq,parameters,decisions",49152),
            ("item","inbox","seq","kind,node,generation,actor,text,notes,state,reply,reply_actor,created_ms",49152),
        ] {
            if let Some(value) = fields.get(field) {
                facts.push(point(db,table,"team",id,Some((key,value)),columns,bound)?);
            }
        }
        if let wire::Action::Team { change } = &self.plan.action {
            facts.extend(phases::capture(db, id, change)?);
        }
        let current = if team.is_null() {
            None
        } else {
            rows::team(db, id)?
        };
        let digest = wire::digest(&(&self.request, facts)).ok_or_else(required)?;
        Ok((current, digest))
    }
}

/// All identifiers and projections are fixed by the calls above, never input.
fn point(
    db: &Connection,
    table: &str,
    first: &str,
    team: &str,
    second: Option<(&str, &Value)>,
    columns: &str,
    bound: u64,
) -> Result<Value> {
    let lengths = columns
        .split(',')
        .map(|c| format!("coalesce(length(CAST({c} AS BLOB)),0)"))
        .collect::<Vec<_>>()
        .join("+");
    let suffix = second.map_or(String::new(), |(key, _)| format!(" AND {key}=?2"));
    let sql = format!("SELECT {lengths},CASE WHEN {lengths}<=?3 THEN json_array({columns}) END FROM {table} WHERE {first}=?1{suffix}");
    let key = second.map_or(Value::Null, |(_, v)| v.clone());
    let key = match key {
        Value::String(s) => rusqlite::types::Value::Text(s),
        Value::Number(n) => rusqlite::types::Value::Integer(n.as_i64().ok_or_else(required)?),
        Value::Null => rusqlite::types::Value::Null,
        _ => return Err(required()),
    };
    let row: Option<(u64, Option<String>)> = db
        .query_row(&sql, params![team, key, bound], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(store::storage)?;
    match row {
        None => Ok(Value::Null),
        Some((bytes, Some(text))) if bytes <= bound => {
            serde_json::from_str(&text).map_err(store::storage)
        }
        _ => Err(Problem::new(
            code::OVER_CAP,
            "The exact team action exceeds its native review bound.",
        )),
    }
}

fn key_number(value: &Value) -> Result<i64> {
    value.as_i64().ok_or_else(required)
}
