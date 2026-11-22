//! Bounded metadata workers keep filesystem, SQLite and Git reads off the
//! desktop actor. An original retains the service owner until it settles.
use super::{
    original::{self, Admission},
    plans::Cache,
    prepared::{self, Prepared},
    required,
};
use crate::{
    config::Config,
    desktop::{fanout::Queue, ui_authority::Validated},
    platform,
    teams::{
        ops,
        store::{self, Store},
    },
};
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
    thread::{self, JoinHandle},
};
use zerus_client_core::{
    desktop::{creation_clone::Scope, team_person_actions as wire},
    upstream_teams::action::TeamAction,
};
use zerus_team_plan::{problem::code, Problem};

const WORKERS: usize = 8;
type Result<T> = std::result::Result<T, Problem>;

pub(in crate::desktop) struct Runtime {
    config: Config,
    scope: Scope,
    plans: Cache,
    owner: Weak<platform::FileLock>,
    stopped: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}
#[derive(Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
enum Query {
    #[serde(rename = "team_person_action_prepare")]
    Prepare { scope: Scope, plan: wire::Reference },
    #[serde(rename = "team_person_action_result")]
    Result { intent: wire::Intent },
    #[serde(rename = "team_person_task_prepare")]
    TaskPrepare {
        scope: Scope,
        draft: wire::TaskDraft,
    },
}

impl Runtime {
    pub fn new(
        config: &Config,
        scope: Scope,
        owner: Arc<platform::FileLock>,
        stopped: Arc<AtomicBool>,
    ) -> Self {
        Self {
            config: config.clone(),
            plans: Cache::new(scope.clone()),
            scope,
            owner: Arc::downgrade(&owner),
            stopped,
            workers: Vec::new(),
        }
    }

    pub fn stage(&mut self, authority: Validated) -> Value {
        let id = authority.id().to_owned();
        let result = authority
            .team_person_plan_piece()
            .map_err(|_| required())
            .and_then(|piece| self.plans.stage(piece).map_err(store::storage));
        match result {
            Ok(complete) => json!({"ok":true,"request_id":id,"complete":complete}),
            Err(problem) => refusal(&id, problem),
        }
    }

    pub fn query(&mut self, value: &Value, queue: Arc<Queue>, seq: u64) -> Result<()> {
        let query: Query = serde_json::from_value(value.clone()).map_err(|_| required())?;
        let plan = match &query {
            Query::Prepare { scope, plan } if scope == &self.scope => {
                Some(self.plans.get(plan).map_err(store::storage)?)
            }
            Query::TaskPrepare { scope, draft } if scope == &self.scope && draft.valid() => None,
            Query::Result { intent }
                if intent.valid() && intent.scope.configuration == self.scope.configuration =>
            {
                None
            }
            _ => return Err(required()),
        };
        let config = self.config.clone();
        let scope = self.scope.clone();
        self.start(queue,seq, move || {
            let result = match query {
                Query::Prepare { .. } => review(&config,&scope,plan.expect("checked native plan")),
                Query::Result { intent } => lookup(&config,&scope,&intent),
                Query::TaskPrepare {draft,..} => super::intake::review(&config,&scope,draft),
            };
            result.unwrap_or_else(|_| json!({"ok":false,"code":"team_person_action_evidence_unavailable","message":"The exact native team action evidence is unavailable. Nothing was replayed."}))
        })
    }

    pub fn action(&mut self, authority: Validated, queue: Arc<Queue>, seq: u64) -> Result<()> {
        let intent = authority
            .team_person_action_intent()
            .ok_or_else(required)?
            .clone();
        if intent.scope != self.scope {
            return Err(required());
        }
        let plan = self.plans.get(&intent.plan).map_err(store::storage)?;
        let config = self.config.clone();
        let scope = self.scope.clone();
        let stopped = self.stopped.clone();
        let reference = intent.plan.clone();
        let started = self.start(queue, seq, move || {
            let result = (|| -> Result<Value> {
                if !cfg!(unix) || stopped.load(Ordering::Acquire) {
                    return Err(required());
                }
                let prepared = Arc::new(Prepared::resolve(&config, plan)?);
                let request = prepared.request.clone();
                if !matches!(prepared.plan.action, wire::Action::Team { .. }) {
                    return super::intake::execute(&config, scope, stopped, authority, prepared);
                }
                let admission = Admission::new(authority, prepared, config.clone(), scope, stopped);
                // Before opening a writer (which may migrate an existing
                // schema), check the exact original and canonical request.
                admission.preflight(&prepared::actor(), &request)?;
                let mut store = Store::open(&config.state)?;
                let result = ops::apply_with_native_context(
                    &mut store,
                    &prepared::actor(),
                    &request,
                    chrono::Utc::now().timestamp_millis(),
                    Some(&config),
                    &|team| crate::desktop::teams::repository(&config, team),
                    admission.into(),
                );
                Ok(json!({"intent":intent,"reply":reply(&request,result)}))
            })();
            result.unwrap_or_else(
                |problem| json!({"intent":intent,"reply":refusal(&intent.plan.request_id,problem)}),
            )
        });
        if started.is_ok() {
            self.plans.discard(&reference);
        }
        started
    }

    pub fn poll(&mut self) {
        let mut index = 0;
        while index < self.workers.len() {
            if self.workers[index].is_finished() {
                let _ = self.workers.swap_remove(index).join();
            } else {
                index += 1;
            }
        }
    }

    fn start(
        &mut self,
        queue: Arc<Queue>,
        seq: u64,
        run: impl FnOnce() -> Value + Send + 'static,
    ) -> Result<()> {
        self.poll();
        if self.stopped.load(Ordering::Acquire) || self.workers.len() >= WORKERS {
            return Err(Problem::new(
                code::STORE_BUSY,
                "Native team action workers are busy. Nothing was admitted.",
            ));
        }
        let owner = self.owner.upgrade().ok_or_else(required)?;
        let scope = self.scope.clone();
        let worker = thread::Builder::new()
            .name("team-person-metadata".into())
            .spawn(move || {
                struct ReplyQueue(Arc<Queue>);
                impl Drop for ReplyQueue {
                    fn drop(&mut self) {
                        self.0.close();
                    }
                }
                let reply = ReplyQueue(queue);
                let _owner = owner;
                let value = run();
                if let Ok(frame) = crate::desktop::protocol::encode(&crate::desktop::frame(
                    &scope.service_epoch,
                    seq,
                    "response",
                    value,
                )) {
                    reply.0.offer(frame.into());
                }
            })
            .map_err(store::storage)?;
        self.workers.push(worker);
        Ok(())
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        // Every worker is one bounded metadata operation. Normal service
        // shutdown cannot relinquish its owner while a writer is still live.
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn review(config: &Config, scope: &Scope, plan: Arc<wire::Plan>) -> Result<Value> {
    let prepared = Prepared::resolve(config, plan.clone())?;
    let store = Store::person_evidence(&config.state)?;
    let phase_resources = ops::prepare_phase_controls(&store, &prepared.request);
    let tx = store
        .db
        .as_ref()
        .map(|db| db.unchecked_transaction())
        .transpose()
        .map_err(store::storage)?;
    let (team, facts) = if let Some(tx) = &tx {
        prepared.bound_facts_with_phase_resources(tx, Some(config), phase_resources.as_ref())?
    } else {
        (None, prepared.empty_facts()?)
    };
    let team_name = if let Some(team) = team {
        ops::authority(&prepared::actor(), &prepared.request.operation, &team)?;
        if team.revision != plan.expected_revision {
            return Err(Problem::new(
                code::STALE_REVISION,
                "Read the current team before confirming its action.",
            ));
        }
        team.name
    } else {
        match &prepared.request.operation {
            ops::Operation::Create { name, .. } | ops::Operation::StartPlanning { name, .. }
                if plan.expected_revision == 0 =>
            {
                name.clone()
            }
            _ => return Err(required()),
        }
    };
    let binding = if let Some(tx) = &tx {
        wire::StoreBinding::Existing {
            fingerprint: super::super::person_gesture::identity(tx, store.state())?,
        }
    } else {
        wire::StoreBinding::Absent {
            root: super::birth::root(store.state())?,
        }
    };
    let wire::Action::Team { change } = &plan.action else {
        return Err(required());
    };
    let mut details = review_details(change, true)?;
    details.extend(kept_details(&prepared.kept, true)?);
    let mut review = wire::Review {
        intent: wire::Intent {
            scope: scope.clone(),
            machine: original::machine(config)?,
            store: binding,
            facts,
            request: wire::digest(&prepared.request).ok_or_else(required)?,
            plan: plan.reference().ok_or_else(required)?,
        },
        team_name,
        title: "Confirm team action".into(),
        subject: change.op().replace('-', " "),
        details,
    };
    // Pretty whitespace can expand a valid dependency-rich draft beyond the
    // fixed review envelope. Compact only its display encoding, preserving
    // the complete payload and all the existing byte/part/identity bounds.
    if !review.valid() {
        review.details = review_details(change, false)?;
        review.details.extend(kept_details(&prepared.kept, false)?);
    }
    if !review.valid() {
        return Err(required());
    }
    Ok(json!({"ok":true,"review":review}))
}

/// What a window Route keeps from the stored task (zerus-3u5yu): the bound
/// dependencies and write paths, and the assignee it keeps or clears, in the
/// same bounded parts as the request's own fields. Text is shown as text.
fn kept_details(kept: &[(&str, Value)], pretty: bool) -> Result<Vec<(String, String)>> {
    let mut details = Vec::new();
    for (key, value) in kept {
        let text = match value {
            Value::String(text) => text.clone(),
            other if pretty => serde_json::to_string_pretty(other).map_err(store::storage)?,
            other => serde_json::to_string(other).map_err(store::storage)?,
        };
        push_parts(&mut details, key, &text);
    }
    Ok(details)
}

/// One field's text in parts of at most 8 KiB, each named `key (n)`.
fn push_parts(details: &mut Vec<(String, String)>, key: &str, text: &str) {
    let mut start = 0;
    let mut part = 1;
    while start < text.len() {
        let mut end = (start + 8192).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        details.push((
            format!("{} ({part})", key.replace('_', " ")),
            text[start..end].to_owned(),
        ));
        start = end;
        part += 1;
    }
}

fn review_details(change: &TeamAction, pretty: bool) -> Result<Vec<(String, String)>> {
    let mut details = Vec::new();
    for (key, value) in change.input() {
        if key == "keep_task_fields" {
            continue;
        }
        let text = if pretty {
            serde_json::to_string_pretty(&value)
        } else {
            serde_json::to_string(&value)
        }
        .map_err(store::storage)?;
        // The complete bounded payload is displayed; no truncation can hide
        // a permission, dependency or phase node from native confirmation.
        push_parts(&mut details, &key, &text);
    }
    if let TeamAction::Create { template, .. } | TeamAction::StartPlanning { template, .. } = change
    {
        details.push((
            "Template".into(),
            serde_json::to_string(template).map_err(store::storage)?,
        ));
    }
    Ok(details)
}

fn lookup(config: &Config, scope: &Scope, intent: &wire::Intent) -> Result<Value> {
    if !intent.valid()
        || intent.scope.configuration != scope.configuration
        || intent.machine != original::machine(config)?
    {
        return Err(required());
    }
    let store = Store::person_evidence(&config.state)?;
    let db = store.db.as_ref().ok_or_else(required)?;
    let tx = db.unchecked_transaction().map_err(store::storage)?;
    if let wire::StoreBinding::Existing { fingerprint } = &intent.store {
        if super::super::person_gesture::identity(&tx, store.state())? != *fingerprint {
            return Err(required());
        }
        if store::person_actions::reserved(&tx, &intent.plan.request_id)? {
            let saved = store::person_actions::original(&tx, intent)?;
            return Ok(
                json!({"intent":intent,"reply":saved.result.unwrap_or_else(||json!({"ok":false,"code":"team_person_action_unknown","request_id":intent.plan.request_id,"message":"The original task intake is unconfirmed. Nothing was replayed."}))}),
            );
        }
    }
    let original_store = original::original(&tx, intent)?;
    if super::super::person_gesture::identity(&tx, store.state())? != original_store {
        return Err(required());
    }
    original::receipt_bound(&tx, &intent.plan.request_id)?;
    let body: Option<String> = tx
        .query_row(
            "SELECT content FROM requests WHERE id=?1 AND length(CAST(content AS BLOB))<=262144",
            [&intent.plan.request_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(store::storage)?;
    let request: ops::Request = store::decode(&body.ok_or_else(required)?)?;
    if request.request_id != intent.plan.request_id
        || request.team != intent.plan.team
        || request.expected_revision != intent.plan.expected_revision
        || wire::digest(&request).as_deref() != Some(&intent.request)
    {
        return Err(required());
    }
    let result = ops::replay(&tx, &prepared::actor(), &request)?.ok_or_else(required)?;
    Ok(json!({"intent":intent,"reply":reply(&request,result)}))
}

fn reply(request: &ops::Request, result: Result<ops::Reply>) -> Value {
    match result {
        Ok(result) => {
            let mut value = serde_json::to_value(result).expect("fixed team receipt");
            value["actor"] = json!(prepared::actor());
            value["team"] = json!(request.team);
            value["request_id"] = json!(request.request_id);
            value
        }
        Err(problem) => refusal(&request.request_id, problem),
    }
}
fn refusal(id: &str, problem: Problem) -> Value {
    json!({"ok":false,"code":problem.code,"message":problem.message,"request_id":id})
}

#[cfg(all(test, unix))]
mod tests;
