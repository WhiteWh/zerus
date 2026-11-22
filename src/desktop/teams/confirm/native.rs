//! Production effects use the native CLI, the original child custodian and
//! receipts read through the original held store. Reads confer no authority.
mod binding;
#[cfg(test)]
pub(super) use binding::launch_binding;
mod observation;
mod original;
mod ownership;
mod receipt;
use super::{
    ledger::Ledger,
    ports::{Ports, Result},
    types::*,
};
use crate::{config::Config, teams::store::Store};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

pub(super) struct Native<'a> {
    pub config: &'a Config,
    pub machine: &'a str,
    pub stop: &'a Arc<AtomicBool>,
    pub ledger: Ledger,
    pub store: &'a mut Store,
    capture: Option<(Snapshot, Value)>,
    pub observation: Option<Result<()>>,
    pins: Vec<ownership::Pin>,
}
impl<'a> Native<'a> {
    pub fn new(
        config: &'a Config,
        machine: &'a str,
        stop: &'a Arc<AtomicBool>,
        store: &'a mut Store,
    ) -> Result<Self> {
        Ok(Self {
            config,
            machine,
            stop,
            ledger: Ledger::open(&config.state)?,
            store,
            capture: None,
            observation: None,
            pins: Vec::new(),
        })
    }
    pub fn reset(&mut self) {
        self.capture = None;
        self.observation = None;
        self.pins.clear();
    }
    fn read(&self, args: Vec<String>, input: Option<&[u8]>) -> Result<Value> {
        let mut scoped = vec!["--expect-machine".into(), self.machine.into()];
        scoped.extend(args);
        self.run(&scoped, input)
    }
    /// The send's effect-free observation of a resumed Codex member or a
    /// lead waiting for its first message, run as
    /// exactly the argv the CLI's own gate admits ([`observation`]).
    fn observe_send(&self, name: &str, input: &[u8]) -> Result<Value> {
        self.run(&observation(name), Some(input))
    }
    fn run(&self, args: &[String], input: Option<&[u8]>) -> Result<Value> {
        match super::super::child::run(
            self.config,
            args,
            input,
            None,
            false,
            Instant::now() + Duration::from_secs(10),
            self.stop,
        ) {
            super::super::child::Ended::Exited { code: 0, output } => {
                serde_json::from_slice(&output).map_err(|e| e.to_string())
            }
            _ => Err("The bounded member observation is unavailable".into()),
        }
    }
}
/// The argv of the send's effect-free observation (zerus-63rru.54): exactly
/// `send NAME --observe --json`, the only argv `cli::observe_gate` admits.
/// That gate runs before any machine guard and refuses every other argv with
/// `send` and `--observe`, a leading `--expect-machine` included, with exit
/// 2. No guard is lost: the view this observation follows was read under the
/// machine guard on this computer, and the gate refuses a remote name.
pub(super) fn observation(name: &str) -> Vec<String> {
    vec![
        "send".into(),
        name.into(),
        "--observe".into(),
        "--json".into(),
    ]
}
impl Ports for Native<'_> {
    fn facts(&mut self, b: &Binding) -> Result<Facts> {
        self.fresh_facts(b)
    }
    fn observe(&mut self, b: &Binding) -> Result<Snapshot> {
        self.snapshot(b)
    }
    fn observe_member(&mut self, b: &Binding) -> Result<Snapshot> {
        let result = self.snapshot(b);
        self.observation = Some(result.as_ref().map(|_| ()).map_err(String::clone));
        result
    }
    fn ownership(&mut self, b: &Binding) -> Result<Ownership> {
        self.owned(b)
    }
    fn unsettled(&mut self, b: &Binding) -> Result<Option<Record>> {
        self.ledger.unsettled(b)
    }
    fn pending_brief(&mut self, b: &Binding) -> Result<Option<Plan>> {
        let team = self
            .store
            .team(&b.team)
            .map_err(|e| e.to_string())?
            .ok_or("Team disappeared")?;
        if team.lead != b.member {
            return self.ledger.pending_brief(b);
        }
        if !self.ledger.lead_brief_needed(b)? {
            return Ok(None);
        }
        let snapshot = self.observe_member(b)?;
        let planning = self.store.planning(&team.id).map_err(|e| e.to_string())?;
        self.ledger
            .pending_lead_brief(b, &snapshot, &team, planning.as_ref())
    }
    fn claim(&mut self, p: &Plan, epoch: &str, now: i64) -> Result<Claim> {
        self.ledger.claim(p, epoch, now)
    }
    fn save(&mut self, row: &Record) -> Result<()> {
        self.ledger.save(row)
    }
    fn audit_begin(&mut self, plan: &Plan) -> Result<Audit> {
        let kind = if plan.kind == Kind::Trust {
            crate::approval_log::TeamKind::FolderTrust
        } else if plan.kind == Kind::Brief {
            crate::approval_log::TeamKind::Brief
        } else {
            crate::approval_log::TeamKind::Resume
        };
        // The scope is recorded only for a FolderTrust plan: it is the
        // extent of the trust grant, never of a resume or any other plan
        // (zerus-63rru.66 round 4).
        let mut payload = json!({"host":self.machine,"session":plan.snapshot.name,"provider":plan.binding.agent,"question":plan.question});
        if kind == crate::approval_log::TeamKind::FolderTrust {
            payload["scope"] = json!(super::policy::scope_for(&plan.binding.folder));
        }
        let mut db = crate::approval_log::open(&self.config.state.join("approval-audit"))
            .map_err(|e| e.to_string())?;
        let reserved =
            crate::approval_log::begin_team(&mut db, &payload, kind).map_err(|e| e.to_string())?;
        if reserved["eligible"] != true {
            return Err("The team audit refused this kind".into());
        }
        Ok(Audit {
            id: reserved["id"].as_i64().ok_or("Missing audit ID")?,
            request: reserved["request_id"]
                .as_str()
                .ok_or("Missing audit request")?
                .into(),
            answers: reserved["answers"].clone(),
            fresh: reserved["fresh"] == true,
        })
    }
    fn audit_finish(&mut self, row: &Record) -> Result<()> {
        let audit = row.audit.as_ref().ok_or("Missing audit")?;
        let db = crate::approval_log::open(&self.config.state.join("approval-audit"))
            .map_err(|e| e.to_string())?;
        crate::approval_log::finish(&db,&json!({"id":audit.id,"request_id":audit.request,"status":row.state,"detail":row.detail})).map_err(|e|e.to_string())?;
        Ok(())
    }
    fn card(&mut self, row: &Record) -> Result<()> {
        self.owner_card(row)
    }
    fn original(
        &mut self,
        row: &Record,
        check: &mut dyn FnMut(&mut Self) -> Result<()>,
    ) -> Outcome {
        if let Err(why) = check(self) {
            return Outcome::NotStarted(why);
        }
        self.deliver(row)
    }
}
impl Native<'_> {
    fn owner_card(&mut self, row: &Record) -> Result<()> {
        use crate::teams::ops::{self, Actor, Operation, Request};
        self.store.refresh().map_err(|e| e.to_string())?;
        let team = self
            .store
            .team(&row.plan.binding.team)
            .map_err(|e| e.to_string())?
            .ok_or("Team disappeared")?;
        let request = if let Some(mut request) = self.ledger.card_request(&row.plan.key)? {
            // The saved request keeps its pre-change shape and stores no
            // parameters (zerus-63rru.34, F1); the member is derived here from
            // the standing row the request already names.
            if let Operation::OpenCard {
                parameters: current,
                ..
            } = &mut request.operation
            {
                if current.is_none() {
                    *current = Some(vec![(
                        "member".into(),
                        row.plan.binding.member.clone(),
                    )]);
                }
            }
            request
        } else {
            let request = Request {
                request_id: uuid::Uuid::new_v4().to_string(),
                team: team.id.clone(),
                expected_revision: team.revision,
                operation: Operation::OpenCard {
                    kind: "standing_confirmation".into(),
                    parameters: Some(vec![("member".into(), row.plan.binding.member.clone())]),
                    text: format!(
                        "Member {} needs your attention: {}",
                        row.plan.binding.member,
                        row.detail.chars().take(2048).collect::<String>()
                    ),
                    node: None,
                    // Nothing reads its decision: acknowledged alone.
                    decisions: None,
                },
            };
            self.ledger.save_card(&row.plan.key, Some(&request))?;
            request
        };
        let actor = Actor::Service {
            person: row.plan.binding.person.clone(),
            team: team.id,
        };
        match ops::apply(
            self.store,
            &actor,
            &request,
            chrono::Utc::now().timestamp_millis(),
        ) {
            Ok(_) => Ok(()),
            Err(p) if p.code == zerus_team_plan::problem::code::STALE_REVISION => {
                self.ledger.save_card(&row.plan.key, None)?;
                Err(
                    "Owner card revision changed; its definite refusal will be checked next pass"
                        .into(),
                )
            }
            Err(p) => Err(p.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_resume_plans_audit_row_carries_no_scope() {
        // zerus-63rru.66 round 4: the scope is recorded only for a
        // FolderTrust plan; a resume's row reads as before.
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            home: String::new(),
            dir: temp.path().join("config"),
            state: temp.path().join("state"),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        crate::platform::private_dir(&config.state.join("teams")).unwrap();
        std::fs::write(config.state.join("teams/preview"), "on").unwrap();
        let mut store = Store::open(&config.state).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (b, _, mut s) = super::super::policy::tests::sample();
        s.ended = true;
        s.paused = false;
        s.self_exit = true;
        s.exit_identity = "exit".into();
        s.conversation = "conversation-1".into();
        s.questions.clear();
        let plan =
            super::super::engine::candidate(&b, &s, Kind::Resume, serde_json::Value::Null);
        let mut native = Native::new(&config, &b.machine, &stop, &mut store).unwrap();
        let audit = native.audit_begin(&plan).unwrap();
        let db = crate::approval_log::open(&config.state.join("approval-audit")).unwrap();
        let saved: String = db
            .query_row("SELECT question FROM attempts WHERE id=?1", [audit.id], |r| {
                r.get(0)
            })
            .unwrap();
        let saved: serde_json::Value = serde_json::from_str(&saved).unwrap();
        assert!(saved.get("scope").is_none(), "{saved}");
    }

    #[test]
    fn team_audit_is_durable_before_delivery_and_finishes_only_once() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::platform::canonical(temp.path()).unwrap();
        let config = Config {
            home: String::new(),
            dir: root.join("config"),
            state: root.join("state"),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        crate::platform::private_dir(&config.state.join("teams")).unwrap();
        std::fs::write(config.state.join("teams/preview"), "on").unwrap();
        let mut store = Store::open(&config.state).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (mut b, _, s) = super::super::policy::tests::sample();
        // A real scratch worktree so the granted scope is the repository's
        // main checkout, an independent known value (zerus-63rru.66 round 2).
        let scratch = root.join("repo");
        std::fs::create_dir_all(&scratch).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "review@example.invalid"],
            vec!["config", "user.name", "review"],
        ] {
            assert!(std::process::Command::new("git").arg("-C").arg(&scratch).args(args).output().unwrap().status.success());
        }
        std::fs::write(scratch.join("file.txt"), "fixture").unwrap();
        assert!(std::process::Command::new("git").arg("-C").arg(&scratch).args(["add", "file.txt"]).output().unwrap().status.success());
        assert!(std::process::Command::new("git").arg("-C").arg(&scratch).args(["commit", "-qm", "fixture"]).output().unwrap().status.success());
        assert!(std::process::Command::new("git").arg("-C").arg(&scratch).args(["worktree", "add", "-q", "--detach", "../member-wt"]).output().unwrap().status.success());
        let worktree = root.join("member-wt");
        b.folder = worktree.to_str().unwrap().to_owned();
        b.launch = b.folder.clone();
        let mut native = Native::new(&config, &b.machine, &stop, &mut store).unwrap();
        let plan = super::super::engine::candidate(&b, &s, Kind::Trust, s.questions[0].clone());
        let audit = native.audit_begin(&plan).unwrap();
        assert!(audit.fresh);
        let db = crate::approval_log::open(&config.state.join("approval-audit")).unwrap();
        let saved: String = db
            .query_row("SELECT question FROM attempts WHERE id=?1", [audit.id], |r| {
                r.get(0)
            })
            .unwrap();
        let saved: serde_json::Value = serde_json::from_str(&saved).unwrap();
        // The independent value: the scratch repository's main checkout,
        // the known path of the worktree the binding names.
        assert_eq!(saved["scope"], scratch.to_str().unwrap(), "{saved}");
        let status: String = db
            .query_row("SELECT status FROM attempts WHERE id=?1", [audit.id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "prepared");
        let row = Record {
            plan,
            original: uuid::Uuid::new_v4().to_string(),
            epoch: "epoch-1".into(),
            state: "unknown".into(),
            audit: Some(audit),
            audit_done: false,
            result: None,
            detail: "Lost receipt".into(),
            card_request: uuid::Uuid::new_v4().to_string(),
            card_done: false,
            at: 100,
        };
        native.audit_finish(&row).unwrap();
        native.audit_finish(&row).unwrap();
        drop(native);
        let mut native = Native::new(&config, &b.machine, &stop, &mut store).unwrap();
        assert!(!native.audit_begin(&row.plan).unwrap().fresh);
        let status: String = db
            .query_row(
                "SELECT status FROM attempts WHERE id=?1",
                [row.audit.unwrap().id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "unknown");
    }
}
#[cfg(test)]
mod ownership_launch;
