//! A live native original, never a serialized actor or saved receipt.
use super::{prepared::Prepared, required};
use crate::{
    config::Config,
    desktop::ui_authority::Validated,
    teams::{ops, store},
};
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use zerus_client_core::desktop::{creation_clone::Scope, team_person_actions as wire};
use zerus_team_plan::Problem;

type Result<T> = std::result::Result<T, Problem>;

pub(super) struct Admission {
    authority: Validated,
    prepared: Arc<Prepared>,
    context: Context,
    birth: Option<super::birth::Birth>,
}
enum Context {
    Service {
        config: Box<Config>,
        scope: Scope,
        stopped: Arc<AtomicBool>,
    },
    #[cfg(test)]
    Fixture,
}
pub(crate) struct Evidence {
    intent: wire::Intent,
    authority: Validated,
    prepared: Arc<Prepared>,
    config: Option<Box<Config>>,
    stopped: Option<Arc<AtomicBool>>,
    store: String,
}

impl Admission {
    pub(super) fn new(
        authority: Validated,
        prepared: Arc<Prepared>,
        config: Config,
        scope: Scope,
        stopped: Arc<AtomicBool>,
    ) -> Self {
        Self {
            authority,
            prepared,
            context: Context::Service {
                config: Box::new(config),
                scope,
                stopped,
            },
            birth: None,
        }
    }

    pub fn preflight(&self, who: &ops::Actor, request: &ops::Request) -> Result<()> {
        let intent = self
            .authority
            .team_person_action_intent()
            .ok_or_else(required)?;
        if !intent.valid() || !self.prepared.matches(who, request, intent) {
            return Err(required());
        }
        match &self.context {
            Context::Service {
                config,
                scope,
                stopped,
            } => {
                if stopped.load(Ordering::Acquire)
                    || &intent.scope != scope
                    || machine(config)? != intent.machine
                    || Prepared::resolve(config, self.prepared.plan.clone())?.request != *request
                {
                    return Err(required());
                }
            }
            #[cfg(test)]
            Context::Fixture => (),
        }
        self.authority
            .check_team_person_action()
            .map_err(|_| required())
    }

    pub fn prepare_store(&mut self, state: &Path) -> Result<()> {
        let intent = self
            .authority
            .team_person_action_intent()
            .ok_or_else(required)?;
        let wire::StoreBinding::Absent { root } = &intent.store else {
            return Ok(());
        };
        if !matches!(
            self.prepared.request.operation,
            ops::Operation::Create { .. } | ops::Operation::StartPlanning { .. }
        ) || self.prepared.request.expected_revision != 0
        {
            return Err(required());
        }
        match &self.context {
            Context::Service { config, .. } => {
                if crate::platform::canonical(&config.state).map_err(store::storage)? != state {
                    return Err(required());
                }
            }
            #[cfg(test)]
            Context::Fixture => (),
        }
        self.authority
            .check_team_person_action()
            .map_err(|_| required())?;
        if let Some(birth) = &self.birth {
            birth.verify(state)?;
        } else {
            self.birth = Some(super::birth::Birth::create(state, root)?);
        }
        // Finish initializing this original's private empty store even if
        // its consent expires now; the writer still refuses every team row.
        Ok(())
    }

    pub fn consume(
        self,
        db: &Connection,
        state: &Path,
        who: &ops::Actor,
        request: &ops::Request,
    ) -> Result<Evidence> {
        if db.is_autocommit() {
            return Err(required());
        }
        self.preflight(who, request)?;
        let intent = self
            .authority
            .team_person_action_intent()
            .ok_or_else(required)?;
        let fingerprint = super::super::person_gesture::identity(db, state)?;
        match &intent.store {
            wire::StoreBinding::Existing {
                fingerprint: expected,
            } if &fingerprint == expected => (),
            wire::StoreBinding::Absent { .. } => {
                self.birth.as_ref().ok_or_else(required)?.verify(state)?
            }
            _ => return Err(required()),
        }
        let (config, stopped) = match self.context {
            Context::Service {
                config, stopped, ..
            } => {
                if crate::platform::canonical(&config.state).map_err(store::storage)? != state {
                    return Err(required());
                }
                (Some(config), Some(stopped))
            }
            #[cfg(test)]
            Context::Fixture => (None, None),
        };
        receipt_bound(db, &intent.plan.request_id)?;
        // Retain the one opaque original through facts, policy and writes.
        // Final consumption happens before commit and rolls back every row
        // if any intervening work outlives the original's deadline.
        self.authority
            .check_team_person_action()
            .map_err(|_| required())?;
        let intent = self
            .authority
            .team_person_action_intent()
            .ok_or_else(required)?
            .clone();
        Ok(Evidence {
            intent,
            authority: self.authority,
            prepared: self.prepared,
            config,
            stopped,
            store: fingerprint,
        })
    }

    #[cfg(test)]
    fn fixture(store: &store::Store, prepared: Arc<Prepared>) -> Self {
        use serde_json::json;
        let intent = wire::Intent {
            scope: Scope {
                configuration: "a".repeat(64),
                service_epoch: uuid::Uuid::new_v4().to_string(),
            },
            machine: uuid::Uuid::new_v4().to_string(),
            store: wire::StoreBinding::Existing {
                fingerprint: super::super::person_gesture::identity(
                    store.db.as_ref().unwrap(),
                    store.state(),
                )
                .unwrap(),
            },
            facts: prepared.facts(store.db.as_ref().unwrap()).unwrap().1,
            request: wire::digest(&prepared.request).unwrap(),
            plan: prepared.plan.reference().unwrap(),
        };
        Self {
            authority: Validated::fixture(&json!({"op":"team_person_action","intent":intent})),
            prepared,
            context: Context::Fixture,
            birth: None,
        }
    }
}

impl Evidence {
    pub fn original(&self, db: &Connection) -> Result<()> {
        if original(db, &self.intent)? != self.store {
            return Err(required());
        }
        self.current()
    }
    pub fn fresh(&self, db: &Connection) -> Result<()> {
        self.fresh_with_phase_resources(db, None)
    }
    pub fn fresh_with_phase_resources(
        &self,
        db: &Connection,
        custody: Option<&Result<store::phase_controls::task_custody::TaskCustody>>,
    ) -> Result<()> {
        let (_, current) =
            self.prepared
                .bound_facts_with_phase_resources(db, self.config.as_deref(), custody)?;
        if current != self.intent.facts {
            return Err(Problem::new(
                zerus_team_plan::problem::code::STALE_REVISION,
                "The reviewed team action changed. Read it again before confirming.",
            ));
        }
        self.current()
    }
    pub(super) fn current(&self) -> Result<()> {
        if self
            .stopped
            .as_ref()
            .is_some_and(|s| s.load(Ordering::Acquire))
        {
            return Err(required());
        }
        self.authority
            .check_team_person_action()
            .map_err(|_| required())
    }
    /// A closed intake refusal has no operation request row and grants no
    /// execution custody. Its own immutable ledger is the exact receipt.
    pub(super) fn record_refusal(self, db: &Connection) -> Result<()> {
        if db.is_autocommit() {
            return Err(required());
        }
        let saved = store::person_actions::original(db, &self.intent)?;
        if saved.request != self.prepared.request
            || !saved
                .result
                .as_ref()
                .is_some_and(|reply| reply["team"]["queued"] == false)
        {
            return Err(required());
        }
        self.current()?;
        let consumed = self
            .authority
            .team_person_action()
            .map_err(|_| required())?;
        if consumed != self.intent {
            return Err(required());
        }
        Ok(())
    }
    pub fn record(self, db: &Connection) -> Result<()> {
        self.current()?;
        let intent = self
            .authority
            .team_person_action()
            .map_err(|_| required())?;
        db.execute(
            "INSERT INTO person_gestures(request_id,intent) VALUES(?1,?2)",
            params![
                intent.plan.request_id,
                store::encode(&Stored {
                    intent: intent.clone(),
                    store: self.store
                })?
            ],
        )
        .map_err(store::storage)?;
        Ok(())
    }
}

pub(super) fn machine(config: &Config) -> Result<String> {
    crate::state::lifecycle_receipt_machine(config).map_err(store::storage)
}

pub(super) fn receipt_bound(db: &Connection, id: &str) -> Result<()> {
    let bytes: Option<u64> = db.query_row(
        "SELECT length(CAST(actor AS BLOB))+length(CAST(content AS BLOB))+length(CAST(result AS BLOB)) FROM requests WHERE id=?1",
        [id], |r| r.get(0)).optional().map_err(store::storage)?;
    if bytes.is_some_and(|n| n > 524288) {
        return Err(required());
    }
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    intent: wire::Intent,
    store: String,
}

pub(super) fn original(db: &Connection, intent: &wire::Intent) -> Result<String> {
    let saved: Option<String> = db.query_row(
        "SELECT intent FROM person_gestures WHERE request_id=?1 AND length(CAST(intent AS BLOB))<=65536",
        [&intent.plan.request_id], |r| r.get(0)).optional().map_err(store::storage)?;
    let saved: Option<Stored> = saved
        .as_deref()
        .and_then(|body| serde_json::from_str(body).ok());
    let saved = saved.filter(|saved| {
        &saved.intent == intent
            && saved.store.len() == 64
            && saved
                .store
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    });
    if saved.is_none() {
        return Err(Problem::new(
            zerus_team_plan::problem::code::DUPLICATE,
            "The original native team action evidence is unavailable or different.",
        ));
    }
    Ok(saved.expect("checked original").store)
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::prepared;
    use super::*;
    use crate::teams::ops::tests::{self as producers, Harness, TEAM};
    use zerus_client_core::upstream_teams::action::TeamAction;

    fn sample(h: &Harness) -> Arc<Prepared> {
        let revision = h.store.team(TEAM).unwrap().unwrap().revision;
        let request = producers::request(
            revision,
            ops::Operation::RenameTeam {
                name: "Native action rename".into(),
            },
        );
        let plan = wire::Plan {
            request_id: request.request_id.clone(),
            team: TEAM.into(),
            expected_revision: revision,
            action: wire::Action::Team {
                change: Box::new(TeamAction::RenameTeam {
                    name: "Native action rename".into(),
                }),
            },
        };
        Arc::new(Prepared {
            plan: Arc::new(plan),
            request,
            kept: Vec::new(),
        })
    }

    #[test]
    fn native_v2_model_renames_with_original_evidence_in_the_writer() {
        let mut h = Harness::new();
        let prepared = sample(&h);
        let admission = Admission::fixture(&h.store, prepared.clone());
        let intent = admission
            .authority
            .team_person_action_intent()
            .unwrap()
            .clone();
        let result = ops::apply_with_native(
            &mut h.store,
            &prepared::actor(),
            &prepared.request,
            1234,
            Some(admission.into()),
        )
        .unwrap();
        assert_eq!(result.revision, prepared.request.expected_revision + 1);
        assert_eq!(
            h.store.team(TEAM).unwrap().unwrap().name,
            "Native action rename"
        );
        let db = h.store.db.as_ref().unwrap();
        original(db, &intent).unwrap();
        assert_eq!(
            ops::replay(db, &prepared::actor(), &prepared.request)
                .unwrap()
                .unwrap()
                .unwrap(),
            result
        );
        let mut exact = Admission::fixture(&h.store, prepared.clone());
        exact.authority =
            Validated::fixture(&serde_json::json!({"op":"team_person_action","intent":intent}));
        assert_eq!(
            ops::apply_with_native(
                &mut h.store,
                &prepared::actor(),
                &prepared.request,
                1235,
                Some(exact.into())
            )
            .unwrap(),
            result
        );
        let changed = Admission::fixture(&h.store, prepared.clone());
        assert_eq!(
            ops::apply_with_native(
                &mut h.store,
                &prepared::actor(),
                &prepared.request,
                1235,
                Some(changed.into())
            )
            .unwrap_err()
            .code,
            zerus_team_plan::problem::code::DUPLICATE
        );
    }

    #[test]
    fn native_v2_model_binds_actor_payload_reference_and_request_digest() {
        for field in ["actor", "payload", "reference", "request"] {
            let mut h = Harness::new();
            let prepared = sample(&h);
            let mut admission = Admission::fixture(&h.store, prepared.clone());
            let mut actor = prepared::actor();
            let mut request = prepared.request.clone();
            match field {
                "actor" => actor = producers::person(),
                "payload" => {
                    request.operation = ops::Operation::RenameTeam {
                        name: "Unreviewed".into(),
                    }
                }
                _ => {
                    let mut intent = admission
                        .authority
                        .team_person_action_intent()
                        .unwrap()
                        .clone();
                    if field == "reference" {
                        intent.plan.sha256 = "f".repeat(64);
                    } else {
                        intent.request = "e".repeat(64);
                    }
                    admission.authority = Validated::fixture(
                        &serde_json::json!({"op":"team_person_action","intent":intent}),
                    );
                }
            }
            if field == "actor" {
                actor = ops::Actor::Person {
                    person: "local".into(),
                    origin: ops::PersonOrigin::CliInterim,
                };
            }
            assert_eq!(
                ops::apply_with_native(
                    &mut h.store,
                    &actor,
                    &request,
                    1234,
                    Some(admission.into())
                )
                .unwrap_err()
                .code,
                "person_gesture_required",
                "{field}"
            );
            assert_eq!(
                h.store.team(TEAM).unwrap().unwrap().revision,
                prepared.request.expected_revision
            );
        }
    }

    #[test]
    fn native_v2_model_expiry_after_writer_wait_changes_no_row() {
        let mut h = Harness::new();
        let prepared = sample(&h);
        let mut admission = Admission::fixture(&h.store, prepared.clone());
        let intent = admission.authority.team_person_action_intent().unwrap();
        admission.authority = Validated::fixture_expiring(
            &serde_json::json!({"op":"team_person_action","intent":intent}),
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        );
        assert_eq!(
            ops::apply_with_native(
                &mut h.store,
                &prepared::actor(),
                &prepared.request,
                1234,
                Some(admission.into())
            )
            .unwrap_err()
            .code,
            "person_gesture_required"
        );
        assert_eq!(
            h.store.team(TEAM).unwrap().unwrap().revision,
            prepared.request.expected_revision
        );
    }

    #[test]
    fn native_v2_model_changed_facts_and_oversize_bodies_are_refused() {
        for oversized in [false, true] {
            let mut h = Harness::new();
            let prepared = sample(&h);
            let admission = Admission::fixture(&h.store, prepared.clone());
            // Named corruption only, after a real producer created the team.
            h.store
                .db
                .as_ref()
                .unwrap()
                .execute(
                    "UPDATE teams SET goal=?1 WHERE id=?2",
                    params![
                        if oversized {
                            "x".repeat(300_000)
                        } else {
                            "Changed review facts".into()
                        },
                        TEAM
                    ],
                )
                .unwrap();
            let problem = ops::apply_with_native(
                &mut h.store,
                &prepared::actor(),
                &prepared.request,
                1234,
                Some(admission.into()),
            )
            .unwrap_err();
            assert_eq!(
                problem.code,
                if oversized {
                    zerus_team_plan::problem::code::OVER_CAP
                } else {
                    zerus_team_plan::problem::code::STALE_REVISION
                }
            );
            assert_eq!(
                h.store.team(TEAM).unwrap().unwrap().revision,
                prepared.request.expected_revision
            );
            let count: u32 = h
                .store
                .db
                .as_ref()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM requests WHERE id=?1",
                    [&prepared.request.request_id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 0);
        }
    }

    fn first_create(h: &Harness) -> (Arc<Prepared>, wire::Intent, Admission) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(h.temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut request = producers::request(0, producers::create());
        if let ops::Operation::Create {
            bundle,
            expected_template,
            delegations,
            ..
        } = &mut request.operation
        {
            *expected_template = Some(crate::teams::catalog::template_digest(bundle).unwrap());
            *delegations = Some(zerus_team_plan::model::Delegations::default());
        }
        let ops::Operation::Create {
            project,
            name,
            goal,
            home,
            bundle,
            integration_branch,
            base_commit,
            ..
        } = &request.operation
        else {
            unreachable!()
        };
        let delegations = serde_json::from_value(
            serde_json::to_value(zerus_team_plan::model::Delegations::default()).unwrap(),
        )
        .unwrap();
        let plan = wire::Plan {
            request_id: request.request_id.clone(),
            team: request.team.clone(),
            expected_revision: 0,
            action: wire::Action::Team {
                change: Box::new(TeamAction::Create {
                    template: bundle.template.id.clone(),
                    project: project.clone(),
                    name: name.clone(),
                    goal: goal.clone(),
                    home: home.clone(),
                    integration_branch: integration_branch.clone(),
                    base_commit: base_commit.clone(),
                    integration: bundle.template.settings.integration,
                    expected_template: crate::teams::catalog::template_digest(bundle).unwrap(),
                    delegations,
                }),
            },
        };
        assert!(plan.valid());
        let prepared = Arc::new(Prepared {
            plan: Arc::new(plan),
            request,
            kept: Vec::new(),
        });
        let intent = wire::Intent {
            scope: Scope {
                configuration: "a".repeat(64),
                service_epoch: uuid::Uuid::new_v4().to_string(),
            },
            machine: uuid::Uuid::new_v4().to_string(),
            store: wire::StoreBinding::Absent {
                root: super::super::birth::root(h.store.state()).unwrap(),
            },
            facts: prepared.empty_facts().unwrap(),
            request: wire::digest(&prepared.request).unwrap(),
            plan: prepared.plan.reference().unwrap(),
        };
        let admission = Admission {
            authority: Validated::fixture(
                &serde_json::json!({"op":"team_person_action","intent":intent}),
            ),
            prepared: prepared.clone(),
            context: Context::Fixture,
            birth: None,
        };
        (prepared, intent, admission)
    }

    #[test]
    fn native_v2_model_first_create_binds_absence_and_records_the_born_store() {
        let mut h = Harness::empty();
        let (prepared, intent, admission) = first_create(&h);
        assert!(!h.temp.path().join("teams/teams-v1.sqlite3").exists());
        let result = ops::apply_with_native(
            &mut h.store,
            &prepared::actor(),
            &prepared.request,
            1234,
            Some(admission.into()),
        )
        .unwrap();
        assert_eq!(result.revision, 1);
        assert_eq!(
            h.store.team(TEAM).unwrap().unwrap().name,
            producers::fixture().team.name
        );
        assert_eq!(
            original(h.store.db.as_ref().unwrap(), &intent).unwrap(),
            super::super::super::person_gesture::identity(
                h.store.db.as_ref().unwrap(),
                h.store.state()
            )
            .unwrap()
        );
    }

    #[test]
    fn native_v2_model_first_create_refuses_a_store_arriving_after_review() {
        let mut h = Harness::empty();
        let (prepared, _intent, admission) = first_create(&h);
        h.run_for(producers::TEAM_B, &producers::person(), producers::create())
            .unwrap();
        let before = super::super::super::person_gesture::identity(
            h.store.db.as_ref().unwrap(),
            h.store.state(),
        )
        .unwrap();
        let problem = ops::apply_with_native(
            &mut h.store,
            &prepared::actor(),
            &prepared.request,
            1234,
            Some(admission.into()),
        )
        .unwrap_err();
        assert_eq!(problem.code, "person_gesture_required");
        assert!(h.store.team(TEAM).unwrap().is_none());
        assert_eq!(
            super::super::super::person_gesture::identity(
                h.store.db.as_ref().unwrap(),
                h.store.state()
            )
            .unwrap(),
            before
        );
    }

    #[test]
    fn native_v2_model_first_create_refuses_a_replaced_directory() {
        let mut h = Harness::empty();
        let (prepared, _intent, admission) = first_create(&h);
        let teams = h.temp.path().join("teams");
        std::fs::rename(&teams, h.temp.path().join("original-teams")).unwrap();
        crate::platform::private_dir(&teams).unwrap();
        std::fs::write(teams.join("preview"), "on").unwrap();
        assert_eq!(
            ops::apply_with_native(
                &mut h.store,
                &prepared::actor(),
                &prepared.request,
                1234,
                Some(admission.into())
            )
            .unwrap_err()
            .code,
            "person_gesture_required"
        );
        assert!(!teams.join("teams-v1.sqlite3").exists());
    }

    #[test]
    fn native_v2_model_expiry_after_mutation_rolls_back_every_row() {
        use rusqlite::functions::FunctionFlags;
        let mut h = Harness::new();
        let prepared = sample(&h);
        let original_name = h.store.team(TEAM).unwrap().unwrap().name;
        let mut admission = Admission::fixture(&h.store, prepared.clone());
        let intent = admission
            .authority
            .team_person_action_intent()
            .unwrap()
            .clone();
        let expired = Arc::new(AtomicBool::new(false));
        let reached = Arc::new(AtomicBool::new(false));
        admission.authority = Validated::fixture_clock(
            &serde_json::json!({"op":"team_person_action","intent":intent}),
            expired.clone(),
        );
        let observed = reached.clone();
        h.store
            .db
            .as_ref()
            .unwrap()
            .create_scalar_function(
                "expire_native_original",
                0,
                FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
                move |_| {
                    observed.store(true, Ordering::Release);
                    expired.store(true, Ordering::Release);
                    Ok(0i64)
                },
            )
            .unwrap();
        h.store.db.as_ref().unwrap().execute_batch("CREATE TEMP TRIGGER expiry_after_saved_request AFTER INSERT ON main.requests BEGIN SELECT expire_native_original(); END;").unwrap();
        assert_eq!(
            ops::apply_with_native(
                &mut h.store,
                &prepared::actor(),
                &prepared.request,
                1234,
                Some(admission.into())
            )
            .unwrap_err()
            .code,
            "person_gesture_required"
        );
        assert!(
            reached.load(Ordering::Acquire),
            "expiry must occur after the real mutation and request insert"
        );
        let team = h.store.team(TEAM).unwrap().unwrap();
        assert_eq!(team.name, original_name);
        assert_eq!(team.revision, prepared.request.expected_revision);
        let db = h.store.db.as_ref().unwrap();
        for table in ["requests", "person_gestures"] {
            let key = if table == "requests" {
                "id"
            } else {
                "request_id"
            };
            let count: u32 = db
                .query_row(
                    &format!("SELECT count(*) FROM {table} WHERE {key}=?1"),
                    [&prepared.request.request_id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
    }

    type WriterGate = (
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
    );
    thread_local! { static WRITER:std::cell::RefCell<Option<WriterGate>>=const {std::cell::RefCell::new(None)}; }
    fn wait_for_writer(_: i32) -> bool {
        WRITER.with(|slot| {
            slot.borrow_mut().take().is_some_and(|(blocked, resume)| {
                blocked.send(()).is_ok()
                    && resume
                        .recv_timeout(std::time::Duration::from_secs(15))
                        .is_ok()
            })
        })
    }
    #[test]
    fn native_v2_model_expiry_during_writer_wait_is_rechecked_inside_the_writer() {
        let mut h = Harness::new();
        let prepared = sample(&h);
        let mut admission = Admission::fixture(&h.store, prepared.clone());
        let intent = admission
            .authority
            .team_person_action_intent()
            .unwrap()
            .clone();
        let expired = Arc::new(AtomicBool::new(false));
        admission.authority = Validated::fixture_clock(
            &serde_json::json!({"op":"team_person_action","intent":intent}),
            expired.clone(),
        );
        let writer = Connection::open(h.temp.path().join("teams/teams-v1.sqlite3")).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (blocked_tx, blocked_rx) = std::sync::mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
        let request = prepared.request.clone();
        let worker = std::thread::spawn(move || {
            WRITER.with(|slot| *slot.borrow_mut() = Some((blocked_tx, resume_rx)));
            h.store
                .db
                .as_ref()
                .unwrap()
                .busy_handler(Some(wait_for_writer))
                .unwrap();
            let result = ops::apply_with_native(
                &mut h.store,
                &prepared::actor(),
                &request,
                1234,
                Some(admission.into()),
            );
            (h, result)
        });
        let blocked = blocked_rx.recv_timeout(std::time::Duration::from_secs(15));
        expired.store(true, Ordering::Release);
        writer.execute_batch("COMMIT").unwrap();
        let _ = resume_tx.send(());
        let (h, result) = worker.join().unwrap();
        blocked.expect("native original must actually wait for the writer");
        assert_eq!(result.unwrap_err().code, "person_gesture_required");
        assert_eq!(
            h.store.team(TEAM).unwrap().unwrap().revision,
            prepared.request.expected_revision
        );
    }
}
