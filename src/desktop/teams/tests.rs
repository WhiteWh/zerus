use super::{
    launch::*,
    ledger::{Ledger, State, BRANCH},
    ports::*,
};
use crate::teams::ops::{self, tests::Harness};
use std::collections::BTreeMap;
use zerus_team_plan::problem::code;
use zerus_team_plan::{
    accounts::{AccountUsage, UsageWindow},
    catalog::AccountChoice,
    model::MemberState,
};
const MACHINE: &str = "00000000-0000-4000-8000-0000000000aa";
const EPOCH: &str = "00000000-0000-4000-8000-000000000001";
mod planning;
#[cfg(unix)]
mod deepseek_cards;

struct FakeEffects {
    events: Vec<String>,
    plans: Vec<Plan>,
    sessions: BTreeMap<String, ObservedSession>,
    accounts: Vec<AccountUsage>,
    exists: bool,
    empty_conversation: bool,
    unknown: bool,
    not_started: bool,
    mismatch: bool,
    account_reads: usize,
    placements: usize,
    account_error: bool,
    placement_error: bool,
    read_error: bool,
    parent_pending: bool,
    worktree_unverified: bool,
    /// Verification fails for good: the row is Blocked.
    verify_uncertain: bool,
    launch_verification_unavailable: bool,
    hosted_refusal: Option<bool>,
    #[cfg(unix)]
    older_deepseek_host: bool,
    placement_override: Option<Placement>,
}
impl Default for FakeEffects {
    fn default() -> Self {
        Self {
            events: vec![],
            plans: vec![],
            sessions: BTreeMap::new(),
            accounts: vec![account("a"), account("b")],
            exists: false,
            empty_conversation: false,
            unknown: false,
            not_started: false,
            mismatch: false,
            account_reads: 0,
            placements: 0,
            account_error: false,
            placement_error: false,
            read_error: false,
            parent_pending: false,
            worktree_unverified: false,
            verify_uncertain: false,
            launch_verification_unavailable: false,
            hosted_refusal: None,
            #[cfg(unix)]
            older_deepseek_host: false,
            placement_override: None,
        }
    }
}
fn account(id: &str) -> AccountUsage {
    AccountUsage {
        id: id.into(),
        agent: "claude".into(),
        signed_in: true,
        windows: vec![UsageWindow {
            used_percent: 30.,
            resets_at: Some(1800000000),
        }],
    }
}
impl Effects for FakeEffects {
    fn hosted_ready(&mut self, plan: &Plan, bound: bool) -> Result<()> {
        #[cfg(unix)]
        if self.older_deepseek_host && plan.agent == "dsh" {
            return Err(crate::teams::hosted::older("/work/example/.dsh"));
        }
        let _ = plan;
        if self.hosted_refusal == Some(bound) {
            return Err(invalid("The resident host cannot serve a team member"));
        }
        Ok(())
    }
    fn placement(&mut self, _: &str, launch: &str) -> Result<Placement> {
        self.placements += 1;
        if self.placement_error {
            return Err(invalid("The project has two local checkouts"));
        }
        if let Some(placement) = &self.placement_override {
            return Ok(placement.clone());
        }
        Ok(Placement {
            source: "/workspace/example".into(),
            git_dir: "/workspace/example/.git".into(),
            branch: format!("team-{launch}"),
            destination: format!("/workspace/zerus-team-{launch}"),
        })
    }
    fn member_branch_available(&mut self, placement: &Placement) -> Result<()> {
        if self.placement_override.is_some() {
            crate::teams::integrate::member_branch_available(
                &crate::teams::integrate::Repo { git_dir: placement.git_dir.clone().into() },
                &placement.branch,
            )?;
        }
        Ok(())
    }
    fn branch_exists(&mut self, _: &super::ledger::BranchPlan) -> Result<bool> {
        Ok(self.exists)
    }
    fn create_branch(&mut self, _: &super::ledger::BranchPlan) -> Effect<()> {
        self.events.push("branch".into());
        self.exists = true;
        Effect::Submitted(())
    }
    fn accounts(&mut self, _: &str) -> Result<Vec<AccountUsage>> {
        self.account_reads += 1;
        if self.account_error {
            return Err(invalid("Account inspection failed"));
        }
        Ok(self.accounts.clone())
    }
    fn worktree(&mut self, plan: &Plan, _: &str) -> Effect<Option<Worktree>> {
        self.events.push(format!("worktree:{}", plan.member));
        if self.worktree_unverified {
            return Effect::Submitted(None);
        }
        Effect::Submitted(Some(Worktree {
            path: format!("/workspace/example-{}", plan.member),
            ownership_nonce: format!("owned-{}", plan.member),
        }))
    }
    fn verify_worktree(&mut self, plan: &Plan) -> Result<Worktree> {
        if self.read_error {
            return Err(invalid("Ownership read unavailable"));
        }
        if self.verify_uncertain {
            return Err(uncertain("Worktree ownership changed"));
        }
        Ok(Worktree {
            path: plan.placement.destination.clone(),
            ownership_nonce: format!("owned-{}", plan.member),
        })
    }
    fn launch(&mut self, plan: &Plan, tree: &Worktree, _: &str) -> Effect<()> {
        if self.launch_verification_unavailable {
            return Effect::Deferred(invalid("Ownership read unavailable"));
        }
        if std::mem::take(&mut self.not_started) {
            return Effect::NotStarted;
        }
        self.events.push(format!("launch:{}", plan.member));
        self.plans.push(plan.clone());
        if self.unknown {
            return Effect::Unknown(uncertain("Synthetic lost launch result"));
        }
        let s = ObservedSession {
            machine: plan.machine.clone(),
            launch_id: plan.launch_id.clone(),
            agent: plan.agent.clone(),
            account: plan.account.clone(),
            worktree: tree.clone(),
            parent: None,
            name: format!("{}/example/{}", plan.agent, plan.member),
            session_id: uuid::Uuid::new_v4().to_string(),
            run_id: uuid::Uuid::new_v4().to_string(),
            conversation_id: if self.empty_conversation {
                String::new()
            } else {
                uuid::Uuid::new_v4().to_string()
            },
        };
        self.sessions.insert(plan.launch_id.clone(), s);
        Effect::Submitted(())
    }
    fn observe(&mut self, plan: &Plan) -> Result<LaunchRead> {
        if self.read_error {
            return Err(invalid("Launch observation failed"));
        }
        let mut s = self.sessions.get(&plan.launch_id).cloned();
        if self.mismatch {
            if let Some(s) = &mut s {
                s.launch_id = uuid::Uuid::new_v4().to_string();
            }
        }
        Ok(s.map(|s| LaunchRead::Session(Box::new(s)))
            .unwrap_or(LaunchRead::Absent { legacy: false }))
    }
    fn parent(&mut self, plan: &Plan, _: &ObservedSession, _: &str) -> String {
        self.events.push(format!("parent:{}", plan.member));
        if self.parent_pending {
            "identity_pending"
        } else {
            "manual"
        }
        .into()
    }
}
fn runtime(h: &Harness) -> Runtime {
    Runtime::open(h.temp.path(), MACHINE, EPOCH).unwrap()
}
fn tick(r: &mut Runtime, h: &mut Harness, e: &mut FakeEffects) -> bool {
    r.tick(&mut h.store, e, ops::tests::TEAM, 1234).unwrap()
}
fn advance(r: &mut Runtime, h: &mut Harness, e: &mut FakeEffects, count: usize) {
    for _ in 0..count {
        tick(r, h, e);
    }
}
// No fixture may spin on a broken or mutated transition on the shared computer.
fn until(
    r: &mut Runtime,
    h: &mut Harness,
    e: &mut FakeEffects,
    reached: impl Fn(&Harness, &FakeEffects) -> bool,
) {
    for _ in 0..100 {
        if reached(h, e) {
            return;
        }
        tick(r, h, e);
    }
    assert!(
        reached(h, e),
        "expected fixture transition was not reached in 100 steps"
    );
}
fn cards(h: &Harness) -> Vec<crate::teams::store::Card> {
    h.store.cards(ops::tests::TEAM, 200, None).unwrap().rows
}
fn row(h: &Harness, member: &str) -> super::ledger::Row {
    Ledger::open(h.temp.path())
        .unwrap()
        .get(ops::tests::TEAM, member)
        .unwrap()
        .unwrap()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn launch_codex_reviewer(
    h: &mut Harness,
    member: &str,
) -> (zerus_team_plan::model::Member, ObservedSession) {
    let mut account = account("review-account");
    account.agent = "codex".into();
    let mut effects = FakeEffects {
        accounts: vec![account],
        ..Default::default()
    };
    let mut runtime = runtime(h);
    until(&mut runtime, h, &mut effects, |h, _| {
        h.store
            .member(ops::tests::TEAM, member)
            .unwrap()
            .unwrap()
            .state
            == MemberState::Active
    });
    assert_eq!(effects.plans.len(), 1);
    let plan = &effects.plans[0];
    assert_eq!(plan.member, member);
    assert_eq!(plan.role, "reviewer");
    assert_eq!(plan.agent, "codex");
    assert_eq!(plan.model, "seat-model");
    assert_eq!(plan.effort, "high");
    assert_eq!(plan.account, "review-account");
    assert!(plan.parent.is_some());
    let bound = h.store.member(ops::tests::TEAM, member).unwrap().unwrap();
    (bound, row(h, member).observed.unwrap())
}

#[test]
fn branch_then_lead_then_members() {
    let mut h = Harness::new();
    let mut e = FakeEffects::default();
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 60);
    assert_eq!(
        e.events,
        [
            "branch",
            "worktree:lead",
            "launch:lead",
            "worktree:m1",
            "launch:m1",
            "parent:m1",
            "worktree:m2",
            "launch:m2",
            "parent:m2",
            "worktree:m3",
            "launch:m3",
            "parent:m3"
        ]
    );
    let lead = h
        .store
        .member(ops::tests::TEAM, "lead")
        .unwrap()
        .unwrap()
        .session
        .unwrap();
    assert!(e.plans[0].parent.is_none());
    assert!(e.plans[1..]
        .iter()
        .all(|p| p.parent.as_ref() == Some(&lead)));
    assert_eq!(e.plans[0].account, "a");
    assert_eq!(e.plans[1].account, "b");
    assert!(cards(&h).is_empty());
}

#[test]
fn a_hosted_capability_refusal_prevents_checkout_creation_or_binding() {
    for bound in [false, true] {
        let mut h = Harness::new();
        let mut e = FakeEffects { hosted_refusal: Some(bound), ..Default::default() };
        let mut r = runtime(&h);
        advance(&mut r, &mut h, &mut e, 30);
        assert_eq!(row(&h, "lead").state, State::Blocked);
        assert!(h.store.member(ops::tests::TEAM, "lead").unwrap().unwrap().session.is_none());
        assert_eq!(e.events.iter().any(|event| event == "worktree:lead"), bound);
    }
}
#[test]
fn unknown_launch_opens_exactly_one_card_and_never_relaunches_after_restart() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        unknown: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 20);
    for _ in 0..5 {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, 301_234)
            .unwrap();
    }
    drop(r);
    advance(&mut runtime(&h), &mut h, &mut e, 20);
    assert_eq!(e.events, ["branch", "worktree:lead", "launch:lead"]);
    assert_eq!(cards(&h).len(), 1);
    assert_eq!(cards(&h)[0].kind, "launch_uncertain");
    offered_as_read(&cards(&h)[0]);
}
#[test]
fn restart_at_every_durable_transition_never_continues_an_original() {
    for stop_after in 1..32 {
        let mut h = Harness::new();
        let mut e = FakeEffects::default();
        let mut r = runtime(&h);
        advance(&mut r, &mut h, &mut e, stop_after);
        let before = e.plans.len();
        let interrupted: [Option<String>; 4] = ["lead", "m1", "m2", "m3"].map(|id| {
            row_if(&h, id)
                .filter(|row| matches!(row.state, State::WorktreeIntent | State::LaunchIntent))
                .map(|_| id.to_string())
        });
        let events_before = e.events.clone();
        drop(r);
        let mut replacement = runtime(&h);
        advance(&mut replacement, &mut h, &mut e, 60);
        let mut ids = std::collections::BTreeSet::new();
        assert!(
            e.plans.iter().all(|p| ids.insert(p.member.clone())),
            "duplicate at {stop_after}"
        );
        assert!(e.plans.len() >= before);
        for id in interrupted.into_iter().flatten() {
            for kind in ["worktree", "launch"] {
                let event = format!("{kind}:{id}");
                assert_eq!(
                    e.events.iter().filter(|v| **v == event).count(),
                    events_before.iter().filter(|v| **v == event).count(),
                    "restart continued an original at transition {stop_after}"
                );
            }
        }
        for id in ["lead", "m1", "m2", "m3"] {
            assert!(
                e.events
                    .iter()
                    .filter(|s| **s == format!("worktree:{id}"))
                    .count()
                    <= 1,
                "duplicate worktree at {stop_after}"
            );
        }
    }
}
#[test]
fn restart_between_intent_and_effect_does_not_create_even_the_first_child() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    tick(&mut r, &mut h, &mut e);
    assert_eq!(row(&h, "lead").state, State::WorktreeIntent);
    drop(r);
    advance(&mut runtime(&h), &mut h, &mut e, 10);
    assert!(e.events.is_empty());
    assert_eq!(cards(&h).len(), 1);
}
#[test]
fn launch_id_binds_after_rename_and_the_same_name_with_another_id_is_refused() {
    for mismatch in [false, true] {
        let mut h = Harness::new();
        let mut e = FakeEffects {
            exists: true,
            mismatch,
            ..Default::default()
        };
        let mut r = runtime(&h);
        until(&mut r, &mut h, &mut e, |_, e| !e.plans.is_empty());
        for s in e.sessions.values_mut() {
            s.name = "claude/example/renamed".into();
        }
        tick(&mut r, &mut h, &mut e);
        let bound = h
            .store
            .member(ops::tests::TEAM, "lead")
            .unwrap()
            .unwrap()
            .session;
        assert_eq!(bound.is_some(), !mismatch);
        if let Some(bound) = bound {
            assert_eq!(
                bound.session_id,
                format!("launch_id:{}", e.plans[0].launch_id)
            );
        }
    }
}
#[test]
fn identity_also_checks_machine_account_agent_and_owned_worktree() {
    for field in ["machine", "account", "agent", "path", "nonce"] {
        let mut h = Harness::new();
        let mut e = FakeEffects {
            exists: true,
            ..Default::default()
        };
        let mut r = runtime(&h);
        until(&mut r, &mut h, &mut e, |_, e| !e.plans.is_empty());
        let s = e.sessions.values_mut().next().unwrap();
        match field {
            "machine" => s.machine = "other".into(),
            "account" => s.account = "other".into(),
            "agent" => s.agent = "codex".into(),
            "path" => s.worktree.path = "/workspace/other".into(),
            _ => s.worktree.ownership_nonce = "other".into(),
        }
        advance(&mut r, &mut h, &mut e, 5);
        assert!(h
            .store
            .member(ops::tests::TEAM, "lead")
            .unwrap()
            .unwrap()
            .session
            .is_none());
        assert_eq!(cards(&h).len(), 1);
    }
}
#[test]
fn unavailable_accounts_name_the_reset_time_without_creating_a_worktree() {
    let mut h = Harness::new();
    let mut a = account("a");
    a.windows[0].used_percent = 100.;
    let mut e = FakeEffects {
        accounts: vec![a],
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 20);
    advance(&mut runtime(&h), &mut h, &mut e, 10);
    assert!(e.events.is_empty());
    assert_eq!(cards(&h).len(), 1);
    assert!(cards(&h)[0].text.contains("1800000000"));
}
#[test]
fn named_account_is_resolved_from_the_member_role() {
    let mut h = Harness::empty();
    let mut create = ops::tests::create();
    if let ops::Operation::Create { bundle, .. } = &mut create {
        bundle.template.lead.account = AccountChoice::Named {
            account: "b".into(),
        };
    }
    h.run(&ops::tests::person(), create).unwrap();
    let mut e = FakeEffects::default();
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 15);
    assert_eq!(e.plans[0].account, "b");
}
#[test]
fn not_started_is_retryable_but_does_not_spin_inside_one_pass() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        not_started: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |h, _| {
        row_if(h, "lead").is_some_and(|r| r.state == State::LaunchIntent)
    });
    assert!(!tick(&mut r, &mut h, &mut e));
    assert_eq!(row(&h, "lead").state, State::LaunchReady);
    drop(r);
    advance(&mut runtime(&h), &mut h, &mut e, 50);
    assert_eq!(
        e.events
            .iter()
            .filter(|s| s.as_str() == "launch:lead")
            .count(),
        1
    );
    assert!(cards(&h).is_empty());
}
fn row_if(h: &Harness, member: &str) -> Option<super::ledger::Row> {
    Ledger::open(h.temp.path())
        .unwrap()
        .get(ops::tests::TEAM, member)
        .unwrap()
}
#[test]
fn preview_off_and_absent_team_create_no_launch_store() {
    let temp = tempfile::tempdir().unwrap();
    let mut e = FakeEffects::default();
    let mut r = Runtime::open(temp.path(), MACHINE, EPOCH).unwrap();
    let mut h = Harness::empty();
    r.tick(&mut h.store, &mut e, ops::tests::TEAM, 1234)
        .unwrap();
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    let mut r = runtime(&h);
    tick(&mut r, &mut h, &mut e);
    assert!(!h
        .temp
        .path()
        .join("teams/member-launch-v1.sqlite3")
        .exists());
}
#[test]
fn paused_stopped_and_foreign_teams_have_no_effects() {
    for op in [ops::Operation::Pause, ops::Operation::Stop] {
        let mut h = Harness::new();
        h.run(&ops::tests::person(), op).unwrap();
        let mut e = FakeEffects::default();
        let mut r = runtime(&h);
        advance(&mut r, &mut h, &mut e, 5);
        assert!(e.events.is_empty());
        assert!(!h
            .temp
            .path()
            .join("teams/member-launch-v1.sqlite3")
            .exists());
    }
    let mut h = Harness::new();
    let mut e = FakeEffects::default();
    let mut r = Runtime::open(h.temp.path(), "another-machine", EPOCH).unwrap();
    advance(&mut r, &mut h, &mut e, 5);
    assert!(e.events.is_empty());
}
#[test]
fn concurrent_runtime_cannot_acquire_an_already_reserved_launch() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut one = runtime(&h);
    let mut two = runtime(&h);
    tick(&mut one, &mut h, &mut e);
    advance(&mut two, &mut h, &mut e, 5);
    advance(&mut one, &mut h, &mut e, 5);
    assert!(e.events.is_empty());
    assert_eq!(cards(&h).len(), 1);
}
#[test]
fn branch_observation_is_not_a_new_branch_effect() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 50);
    assert!(!e.events.contains(&"branch".into()));
    assert_eq!(row(&h, BRANCH).state, State::BranchDone);
}
#[test]
fn owner_card_replay_after_store_commit_is_idempotent() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        mismatch: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 15);
    assert_eq!(cards(&h).len(), 1);
    let mut ledger = Ledger::open(h.temp.path()).unwrap();
    let mut saved = row(&h, "lead");
    saved.card_done = false;
    ledger.save(&mut saved, 1234).unwrap();
    advance(&mut runtime(&h), &mut h, &mut e, 10);
    assert_eq!(cards(&h).len(), 1);
}
#[test]
fn newer_launch_ledger_schema_is_refused() {
    let h = Harness::new();
    drop(Ledger::open(h.temp.path()).unwrap());
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    db.execute_batch("PRAGMA user_version=99").unwrap();
    drop(db);
    assert!(Ledger::open(h.temp.path()).is_err());
}

pub(super) fn plan_fixture() -> Plan {
    let mut h = Harness::new();
    let mut effects = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    tick(&mut r, &mut h, &mut effects);
    row(&h, "lead").plan.unwrap()
}
#[test]
fn presentation_parent_never_delays_binding_or_opens_an_owner_card() {
    let mut h = Harness::new();
    let mut e = FakeEffects::default();
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |h, _| {
        row_if(h, "m1").is_some_and(|r| r.state == State::ParentIntent)
    });
    assert_eq!(
        h.store
            .member(ops::tests::TEAM, "m1")
            .unwrap()
            .unwrap()
            .state,
        MemberState::Active
    );
    drop(r);
    advance(&mut runtime(&h), &mut h, &mut e, 60);
    assert_eq!(
        row(&h, "m1").parent_result.as_deref(),
        Some("unknown_after_restart")
    );
    assert!(!e.events.contains(&"parent:m1".into()));
    assert!(cards(&h).is_empty());
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn final_native_admission_rechecks_pause_and_exact_member_binding() {
    let mut h = Harness::new();
    let plan = plan_fixture();
    let config = crate::config::Config {
        home: String::new(),
        dir: h.temp.path().join("config"),
        state: h.temp.path().into(),
        selves: vec![],
        peers: vec![],
        base_peers: vec![],
        peers_from_environment: false,
        machines: Default::default(),
        tab: false,
        tab_colors: String::new(),
    };
    assert!(super::child::admission(&config, &plan, false).is_ok());
    h.run(&ops::tests::person(), ops::Operation::Pause).unwrap();
    assert!(super::child::admission(&config, &plan, false).is_err());
    h.run(&ops::tests::person(), ops::Operation::Resume)
        .unwrap();
    h.bind("lead");
    assert!(super::child::admission(&config, &plan, false).is_err());
    assert!(super::child::admission(&config, &plan, true).is_err());
}

#[test]
fn pausing_an_earlier_member_does_not_withdraw_another_members_original() {
    let mut h = Harness::new();
    let mut e = FakeEffects::default();
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |h, _| {
        row_if(h, "m1").is_some_and(|r| r.state == State::ParentIntent)
    });
    h.run(
        &ops::tests::person(),
        ops::Operation::PauseMember {
            member: "m1".into(),
        },
    )
    .unwrap();
    advance(&mut r, &mut h, &mut e, 30);
    assert_eq!(
        h.store
            .member(ops::tests::TEAM, "m2")
            .unwrap()
            .unwrap()
            .state,
        MemberState::Active
    );
    assert!(cards(&h).is_empty());
}

#[test]
fn another_team_can_progress_while_a_member_is_unfinished() {
    let mut h = Harness::new();
    let mut create = ops::tests::create();
    if let ops::Operation::Create {
        integration_branch, ..
    } = &mut create
    {
        integration_branch.push_str("-second")
    }
    h.run_for(ops::tests::TEAM_B, &ops::tests::person(), create)
        .unwrap();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    tick(&mut r, &mut h, &mut e);
    assert!(r.busy());
    assert!(r
        .tick(&mut h.store, &mut e, ops::tests::TEAM_B, 1234)
        .unwrap());
}

// Regression controls supplied by the independent package review.
#[test]
fn review07_removed_member_does_not_starve_other_teams() {
    let mut h = Harness::new();
    let mut create = ops::tests::create();
    if let ops::Operation::Create {
        integration_branch, ..
    } = &mut create
    {
        integration_branch.push_str("-second");
    }
    h.run_for(ops::tests::TEAM_B, &ops::tests::person(), create)
        .unwrap();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |h, _| {
        row_if(h, "m3").is_some_and(|r| r.state == State::WorktreeIntent)
    });
    assert!(r.busy());
    h.run(
        &ops::tests::person(),
        ops::Operation::ArchiveMember {
            member: "m3".into(),
        },
    )
    .unwrap();
    advance(&mut r, &mut h, &mut e, 20);
    let before = e.events.len();
    let mut progressed = false;
    for _ in 0..50 {
        progressed |= r
            .tick(&mut h.store, &mut e, ops::tests::TEAM_B, 1234)
            .unwrap();
    }
    eprintln!(
        "busy={} progressed={} events_before={} after={}",
        r.busy(),
        progressed,
        before,
        e.events.len()
    );
    assert!(!r.busy(), "removed member releases its admission");
    assert!(e.events.len() > before, "team B progresses");
    assert!(cards(&h).is_empty());
}

struct LockingEffects {
    inner: FakeEffects,
    path: std::path::PathBuf,
    lock: Option<rusqlite::Connection>,
    lock_on: &'static str,
    fired: bool,
}
impl LockingEffects {
    fn after(&mut self, kind: &str) {
        if self.lock_on == kind && !self.fired {
            let db = rusqlite::Connection::open(&self.path).unwrap();
            db.execute_batch("BEGIN EXCLUSIVE").unwrap();
            self.lock = Some(db);
            self.fired = true;
        }
    }
}
impl Effects for LockingEffects {
    fn placement(&mut self, p: &str, l: &str) -> Result<Placement> {
        self.inner.placement(p, l)
    }
    fn branch_exists(&mut self, p: &super::ledger::BranchPlan) -> Result<bool> {
        self.inner.branch_exists(p)
    }
    fn create_branch(&mut self, p: &super::ledger::BranchPlan) -> Effect<()> {
        let result = self.inner.create_branch(p);
        self.after("branch");
        result
    }
    fn accounts(&mut self, a: &str) -> Result<Vec<AccountUsage>> {
        self.inner.accounts(a)
    }
    fn worktree(&mut self, p: &Plan, o: &str) -> Effect<Option<Worktree>> {
        let result = self.inner.worktree(p, o);
        self.after("worktree");
        result
    }
    fn verify_worktree(&mut self, plan: &Plan) -> Result<Worktree> {
        self.inner.verify_worktree(plan)
    }
    fn launch(&mut self, p: &Plan, t: &Worktree, o: &str) -> Effect<()> {
        let result = self.inner.launch(p, t, o);
        self.after("launch");
        result
    }
    fn observe(&mut self, p: &Plan) -> Result<LaunchRead> {
        self.inner.observe(p)
    }
    fn parent(&mut self, p: &Plan, s: &ObservedSession, o: &str) -> String {
        let result = self.inner.parent(p, s, o);
        self.after("parent");
        result
    }
}
#[test]
fn review07_lost_outcome_never_reruns_a_launch() {
    let mut h = Harness::new();
    let mut e = LockingEffects {
        inner: FakeEffects {
            exists: true,
            ..Default::default()
        },
        path: h.temp.path().join("teams/member-launch-v1.sqlite3"),
        lock: None,
        lock_on: "launch",
        fired: false,
    };
    let mut r = runtime(&h);
    let mut errors = 0;
    for _ in 0..10 {
        if r.tick(&mut h.store, &mut e, ops::tests::TEAM, 1234)
            .is_err()
        {
            errors += 1;
            e.lock = None; // the disk recovers
        }
        if errors > 0
            && e.inner
                .events
                .iter()
                .filter(|x| *x == "launch:lead")
                .count()
                > 1
        {
            break;
        }
    }
    eprintln!("errors={errors} events={:?}", e.inner.events);
    assert_eq!(errors, 1);
    assert_eq!(
        e.inner
            .events
            .iter()
            .filter(|x| *x == "launch:lead")
            .count(),
        1,
        "the launch must not run twice"
    );
    assert_eq!(cards(&h).len(), 1);
}

#[test]
fn a_changed_lead_invalidates_the_member_plan_before_an_effect() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |h, _| {
        row_if(h, "m1").is_some_and(|r| r.state == State::WorktreeIntent)
    });
    let mut saved = row(&h, "m1");
    saved.plan.as_mut().unwrap().parent = Some(zerus_team_plan::model::SessionKey {
        machine: MACHINE.into(),
        session_id: "launch_id:00000000-0000-4000-8000-000000000099".into(),
    });
    Ledger::open(h.temp.path())
        .unwrap()
        .save(&mut saved, 1234)
        .unwrap();
    advance(&mut r, &mut h, &mut e, 4);
    assert!(!e.events.contains(&"worktree:m1".into()));
    assert_eq!(row(&h, "m1").state, State::Blocked);
}
#[test]
fn a_member_paused_after_reservation_never_runs_its_effect() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |h, _| {
        row_if(h, "m1").is_some_and(|r| r.state == State::WorktreeIntent)
    });
    h.run(
        &ops::tests::service(),
        ops::Operation::SetMemberState {
            member: "m1".into(),
            state: MemberState::Paused,
        },
    )
    .unwrap();
    advance(&mut r, &mut h, &mut e, 20);
    assert!(!e.events.contains(&"worktree:m1".into()));
}
#[test]
fn a_saved_worktree_can_launch_after_restart_without_being_created_again() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |h, _| {
        row_if(h, "lead").is_some_and(|r| r.state == State::WorktreeDone)
    });
    drop(r);
    advance(&mut runtime(&h), &mut h, &mut e, 60);
    assert_eq!(
        e.events
            .iter()
            .filter(|e| e.as_str() == "worktree:lead")
            .count(),
        1
    );
    assert_eq!(
        e.events
            .iter()
            .filter(|e| e.as_str() == "launch:lead")
            .count(),
        1
    );
    assert!(cards(&h).is_empty());
}
#[test]
fn a_session_at_folder_trust_binds_without_a_conversation() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |_, e| !e.plans.is_empty());
    for s in e.sessions.values_mut() {
        s.conversation_id.clear();
    }
    tick(&mut r, &mut h, &mut e);
    assert!(h
        .store
        .member(ops::tests::TEAM, "lead")
        .unwrap()
        .unwrap()
        .session
        .is_some());
    assert!(cards(&h).is_empty());
}
#[test]
fn an_unknown_child_waits_five_minutes_for_identity_and_can_still_bind() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        unknown: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 12);
    assert!(cards(&h).is_empty());
    assert_eq!(row(&h, "lead").state, State::LaunchUnknown);
    let plan = e.plans[0].clone();
    e.unknown = false;
    e.sessions.insert(
        plan.launch_id.clone(),
        ObservedSession {
            machine: plan.machine.clone(),
            launch_id: plan.launch_id.clone(),
            agent: plan.agent.clone(),
            account: plan.account.clone(),
            worktree: row(&h, "lead").worktree.unwrap(),
            parent: None,
            name: "claude/example/trust".into(),
            session_id: String::new(),
            run_id: String::new(),
            conversation_id: String::new(),
        },
    );
    r.tick(&mut h.store, &mut e, ops::tests::TEAM, 61_234)
        .unwrap();
    assert!(h
        .store
        .member(ops::tests::TEAM, "lead")
        .unwrap()
        .unwrap()
        .session
        .is_some());
    assert!(cards(&h).is_empty());
}

#[test]
fn account_choice_retries_after_reset_and_closes_its_one_card_as_stale() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    for a in &mut e.accounts {
        a.windows[0].used_percent = 100.;
        a.windows[0].resets_at = Some(80);
    }
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 20);
    assert_eq!(cards(&h).len(), 1);
    assert!(cards(&h)[0]
        .text
        .contains("Automatic retry follows that account window reset"));
    let card = cards(&h)[0].seq;
    assert_eq!(e.account_reads, 1);
    for a in &mut e.accounts {
        a.windows[0].used_percent = 30.;
    }
    r.begin_pass();
    for _ in 0..8 {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, 80_000)
            .unwrap();
    }
    assert!(h
        .store
        .member(ops::tests::TEAM, "lead")
        .unwrap()
        .unwrap()
        .session
        .is_some());
    assert_eq!(
        h.store
            .card(ops::tests::TEAM, card)
            .unwrap()
            .unwrap()
            .decision
            .as_deref(),
        Some("stale")
    );
}
#[test]
fn a_persons_retry_decision_bypasses_the_backoff_without_impersonation() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        account_error: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 20);
    assert_eq!(cards(&h).len(), 1);
    let card = cards(&h)[0].seq;
    h.run(
        &ops::tests::person(),
        ops::Operation::DecideCard {
            card,
            decision: "retry".into(),
        },
    )
    .unwrap();
    e.account_error = false;
    r.begin_pass();
    tick(&mut r, &mut h, &mut e);
    assert_eq!(row(&h, "lead").state, State::WorktreeIntent);
    assert_eq!(e.account_reads, 2);
}
#[test]
fn placement_precedes_account_inspection_and_repeated_faults_are_backed_off() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        placement_error: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 20);
    assert_eq!(e.account_reads, 0);
    assert_eq!(e.placements, 1);
    assert_eq!(cards(&h).len(), 1);
    e.placement_error = false;
    r.begin_pass();
    for _ in 0..60 {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, 61_234)
            .unwrap();
    }
    assert_eq!(e.account_reads, 1);
    assert!(h
        .store
        .member(ops::tests::TEAM, "lead")
        .unwrap()
        .unwrap()
        .session
        .is_some());
}
#[test]
fn account_reads_are_cached_once_per_pass_and_failures_are_never_signouts() {
    let mut h = Harness::new();
    let mut e = FakeEffects::default();
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 60);
    assert_eq!(e.account_reads, 1);
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        account_error: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 20);
    assert_eq!(e.account_reads, 1);
    assert_eq!(row(&h, "lead").state, State::Retry);
    assert!(cards(&h)[0].text.contains("inspection failed"));
    assert!(!cards(&h)[0].text.contains("no_account_available"));
}
#[test]
fn observation_read_errors_do_not_turn_a_running_launch_into_uncertainty() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |_, e| !e.plans.is_empty());
    e.read_error = true;
    for now in [1234, 61_234, 181_234] {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, now).unwrap();
    }
    assert_eq!(row(&h, "lead").state, State::Submitted);
    assert_eq!(cards(&h).len(), 1);
    assert_eq!(cards(&h)[0].kind, "launch_blocked");
    offered_as_read(&cards(&h)[0]);
    e.read_error = false;
    r.tick(&mut h.store, &mut e, ops::tests::TEAM, 500_000)
        .unwrap();
    assert!(h
        .store
        .member(ops::tests::TEAM, "lead")
        .unwrap()
        .unwrap()
        .session
        .is_some());
}
#[test]
fn incomplete_parent_identity_retries_are_bounded_and_do_not_block_other_members() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        parent_pending: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 60);
    assert!(h
        .store
        .member(ops::tests::TEAM, "m3")
        .unwrap()
        .unwrap()
        .session
        .is_some());
    assert!(cards(&h).is_empty());
    for round in 1..=12 {
        for _ in 0..30 {
            r.tick(
                &mut h.store,
                &mut e,
                ops::tests::TEAM,
                1234 + round * 60_000,
            )
            .unwrap();
        }
    }
    let saved = row(&h, "m1");
    assert_eq!(saved.parent_tries, 10);
    assert_eq!(saved.parent_result.as_deref(), Some("identity_timeout"));
    assert!(cards(&h).is_empty());
}
#[test]
fn stopped_and_removed_pending_launches_are_noted_for_the_lifecycle_owner() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |_, e| !e.plans.is_empty());
    h.run(&ops::tests::person(), ops::Operation::Stop).unwrap();
    tick(&mut r, &mut h, &mut e);
    assert!(row(&h, "lead").late_after_stop);
}
#[test]
fn an_unknown_named_account_retries_without_a_second_card() {
    let mut h = Harness::empty();
    let mut create = ops::tests::create();
    if let ops::Operation::Create { bundle, .. } = &mut create {
        bundle.template.lead.account = AccountChoice::Named {
            account: "later".into(),
        };
    }
    h.run(&ops::tests::person(), create).unwrap();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 10);
    assert_eq!(cards(&h).len(), 1);
    assert!(cards(&h)[0].text.contains("unknown_account"));
    let card = cards(&h)[0].seq;
    e.accounts.push(account("later"));
    r.begin_pass();
    for _ in 0..10 {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, 61_234)
            .unwrap();
    }
    assert_eq!(e.plans[0].account, "later");
    assert_eq!(
        h.store
            .card(ops::tests::TEAM, card)
            .unwrap()
            .unwrap()
            .decision
            .as_deref(),
        Some("stale")
    );
}

#[test]
fn losing_an_outcome_never_repeats_branch_worktree_or_parent_effects() {
    for kind in ["branch", "worktree", "parent"] {
        let mut h = Harness::new();
        let mut e = LockingEffects {
            inner: FakeEffects {
                exists: kind != "branch",
                ..Default::default()
            },
            path: h.temp.path().join("teams/member-launch-v1.sqlite3"),
            lock: None,
            lock_on: kind,
            fired: false,
        };
        let mut r = runtime(&h);
        let mut errors = 0;
        for _ in 0..60 {
            if r.tick(&mut h.store, &mut e, ops::tests::TEAM, 1234)
                .is_err()
            {
                errors += 1;
                e.lock = None;
            }
        }
        assert_eq!(errors, 1, "{kind}");
        let event = match kind {
            "branch" => "branch",
            "worktree" => "worktree:lead",
            _ => "parent:m1",
        };
        assert_eq!(
            e.inner
                .events
                .iter()
                .filter(|e| e.as_str() == event)
                .count(),
            1,
            "{kind} was replayed"
        );
    }
}

// Regressions from the review07b reproductions, asserting the corrected behavior.
#[test]
fn review07b_a_failed_person_retry_opens_exactly_one_new_card() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        account_error: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 20);
    let open = |h: &Harness| cards(h).into_iter().filter(|c| c.state == "open").count();
    assert_eq!(open(&h), 1);
    let card = cards(&h)[0].seq;
    h.run(
        &ops::tests::person(),
        ops::Operation::DecideCard {
            card,
            decision: "retry".into(),
        },
    )
    .unwrap();
    // The condition still holds: the retry fails again.
    r.begin_pass();
    for _ in 0..5 {
        let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, 1234);
    }
    let lead = row(&h, "lead");
    eprintln!(
        "REVIEW07B state={:?} failures={} card_done={} open_cards={} all_cards={}",
        lead.state,
        lead.failures,
        lead.card_done,
        open(&h),
        cards(&h).len()
    );
    // Much later, still failing:
    r.begin_pass();
    for _ in 0..5 {
        let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, 10_000_000);
    }
    let lead = row(&h, "lead");
    eprintln!(
        "REVIEW07B later state={:?} failures={} open_cards={}",
        lead.state,
        lead.failures,
        open(&h)
    );
    assert_eq!(open(&h), 1, "the failed requested retry must be visible");
    assert_eq!(
        cards(&h).len(),
        2,
        "automatic retries must keep the new card"
    );
}
#[test]
fn review07b_a_full_ledger_budget_opens_one_durable_team_card() {
    let mut h = Harness::new();
    drop(Ledger::open(h.temp.path()).unwrap());
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    db.execute_batch("UPDATE launch_budget SET rows=16384 WHERE id=1")
        .unwrap();
    drop(db);
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    let mut errors = 0;
    for _ in 0..20 {
        if let Err(p) = r.tick(&mut h.store, &mut e, ops::tests::TEAM, 1234) {
            errors += 1;
            if errors == 1 {
                eprintln!("REVIEW07B tick error: {} {}", p.code, p.message);
            }
        }
    }
    let lead = Ledger::open(h.temp.path())
        .unwrap()
        .get(ops::tests::TEAM, "lead")
        .unwrap();
    eprintln!(
        "REVIEW07B errors={errors} lead_row={} cards={} events={:?}",
        lead.is_some(),
        cards(&h).len(),
        e.events
    );
    assert!(lead.is_none());
    assert_eq!(
        cards(&h).len(),
        1,
        "one card tells the person about the full ledger"
    );
    drop(r);
    advance(&mut runtime(&h), &mut h, &mut e, 20);
    assert_eq!(cards(&h).len(), 1, "restart must reuse the budget card");
}

#[test]
fn a_submitted_launch_with_missing_identity_expires_once_without_relaunching() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |_, e| !e.plans.is_empty());
    e.sessions.clear();
    for now in [1234, 61_234, 301_234, 500_000, 10_000_000] {
        for _ in 0..3 {
            r.tick(&mut h.store, &mut e, ops::tests::TEAM, now).unwrap();
        }
    }
    assert_eq!(row(&h, "lead").state, State::Blocked);
    assert_eq!(cards(&h).len(), 1);
    assert_eq!(cards(&h)[0].kind, "launch_uncertain");
    offered_as_read(&cards(&h)[0]);
    assert_eq!(e.plans.len(), 1);
}
#[test]
fn retry_card_distinguishes_automatic_backoff_from_a_persons_retry() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        account_error: true,
        ..Default::default()
    };
    advance(&mut runtime(&h), &mut h, &mut e, 10);
    let text = &cards(&h)[0].text;
    assert!(!text.contains("within 60 seconds"));
    assert!(text.contains("Automatic retry"));
    assert!(text.contains("Retry checks sooner"));
}

#[test]
fn a_created_worktree_can_finish_its_ownership_read_after_restart_without_recreation() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        read_error: true,
        worktree_unverified: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |h, _| {
        row_if(h, "lead").is_some_and(|r| r.state == State::WorktreeVerify)
    });
    assert_eq!(
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, 1234)
            .unwrap_err()
            .code,
        code::INVALID_STATE
    );
    assert_eq!(row(&h, "lead").state, State::WorktreeVerify);
    drop(r);
    let mut r = runtime(&h);
    e.read_error = false;
    for _ in 0..10 {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, 61_234)
            .unwrap();
    }
    assert!(h
        .store
        .member(ops::tests::TEAM, "lead")
        .unwrap()
        .unwrap()
        .session
        .is_some());
    assert_eq!(
        e.events
            .iter()
            .filter(|e| e.as_str() == "worktree:lead")
            .count(),
        1
    );
}
#[test]
fn ownership_verification_before_a_launch_backs_off_and_recovers_without_a_refusal() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        launch_verification_unavailable: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |h, _| {
        row_if(h, "lead").is_some_and(|r| r.state == State::LaunchIntent)
    });
    for now in [1234, 61_234, 181_234] {
        for _ in 0..2 {
            let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, now);
        }
        assert_eq!(row(&h, "lead").state, State::LaunchReady);
        assert!(row(&h, "lead").retry_at > now);
    }
    assert_eq!(cards(&h).len(), 1);
    assert_eq!(cards(&h)[0].kind, "launch_blocked");
    offered_as_read(&cards(&h)[0]);
    assert!(!e.events.iter().any(|e| e.starts_with("launch:")));
    e.launch_verification_unavailable = false;
    for _ in 0..4 {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, 500_000)
            .unwrap();
    }
    assert!(h
        .store
        .member(ops::tests::TEAM, "lead")
        .unwrap()
        .unwrap()
        .session
        .is_some());
    assert_eq!(
        e.events
            .iter()
            .filter(|e| e.as_str() == "launch:lead")
            .count(),
        1
    );
    assert_eq!(cards(&h)[0].state, "closed");
    assert_eq!(cards(&h)[0].decision.as_deref(), Some("stale"));
}

#[test]
fn a_budget_card_survives_its_reserved_outbox_and_closes_when_capacity_recovers() {
    let mut h = Harness::new();
    let team = h.store.team(ops::tests::TEAM).unwrap().unwrap();
    let mut ledger = Ledger::open(h.temp.path()).unwrap();
    // Crash after the independent notice row was inserted, before opening its card.
    let mut alert = super::ledger::Row::new(&team, super::ledger::BUDGET, EPOCH, State::Retry);
    alert.problem = Some(
        zerus_team_plan::Problem::new(
            code::OVER_CAP,
            "Team member creation is temporarily at capacity.",
        )
        .with_refs(["member_launch_ledger"]),
    );
    ledger.insert(&alert, 1234).unwrap();
    drop(ledger);
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    db.execute("UPDATE launch_budget SET rows=16384 WHERE id=1", [])
        .unwrap();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    for _ in 0..5 {
        let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, 1234);
    }
    assert_eq!(cards(&h).len(), 1);
    assert_eq!(cards(&h)[0].kind, "launch_capacity");
    offered_as_read(&cards(&h)[0]);
    assert!(cards(&h)[0].text.starts_with("Team member creation:"));
    assert!(!cards(&h)[0].text.contains("@budget"));
    assert!(!cards(&h)[0].text.contains("member_launch_ledger"));
    drop(r);
    db.execute(
        "UPDATE launch_budget SET rows=(SELECT COUNT(*) FROM originals) WHERE id=1",
        [],
    )
    .unwrap();
    drop(db);
    let mut r = runtime(&h);
    for _ in 0..8 {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, 61_234)
            .unwrap();
    }
    assert_eq!(cards(&h).len(), 1);
    assert_eq!(cards(&h)[0].state, "closed");
    assert_eq!(cards(&h)[0].decision.as_deref(), Some("stale"));
}

#[test]
fn missing_submitted_identity_replaces_an_earlier_read_error_card_with_uncertainty() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |_, e| !e.plans.is_empty());
    e.read_error = true;
    for now in [1234, 61_234, 181_234] {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, now).unwrap();
    }
    assert_eq!(cards(&h)[0].kind, "launch_blocked");
    offered_as_read(&cards(&h)[0]);
    e.read_error = false;
    e.sessions.clear();
    for _ in 0..3 {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, 600_000)
            .unwrap();
    }
    let all = cards(&h);
    let open: Vec<_> = all.iter().filter(|c| c.state == "open").collect();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, "launch_uncertain");
    offered_as_read(open[0]);
    assert_eq!(all[0].decision.as_deref(), Some("stale"));
}

// Regression probes from the independent follow-up review.
fn followup_ownership_fault_card(verify: bool) -> (Harness, FakeEffects, Runtime) {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        worktree_unverified: verify,
        read_error: verify,
        launch_verification_unavailable: !verify,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |h, _| {
        row_if(h, "lead").is_some_and(|r| {
            r.state
                == if verify {
                    State::WorktreeVerify
                } else {
                    State::LaunchIntent
                }
        })
    });
    for now in [1234, 61_234, 181_234] {
        for _ in 0..2 {
            let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, now);
        }
    }
    assert_eq!(cards(&h).len(), 1);
    assert_eq!(cards(&h)[0].state, "open");
    (h, e, r)
}
#[test]
fn followup_retry_after_ownership_fault_reopens_one_card() {
    for verify in [false, true] {
        let (mut h, mut e, r) = followup_ownership_fault_card(verify);
        let card = cards(&h)[0].seq;
        h.run(
            &ops::tests::person(),
            ops::Operation::DecideCard {
                card,
                decision: "retry".into(),
            },
        )
        .unwrap();
        drop(r);
        let mut r = runtime(&h);
        for now in [181_234, 500_000, 2_000_000, 10_000_000] {
            for _ in 0..5 {
                let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, now);
            }
        }
        eprintln!(
            "verify={verify} row={:?} failures={} retry_at={} retry_seen={} cards={:?}",
            row(&h, "lead").state,
            row(&h, "lead").failures,
            row(&h, "lead").retry_at,
            row(&h, "lead").retry_seen,
            cards(&h)
        );
        assert_eq!(
            cards(&h).iter().filter(|c| c.state == "open").count(),
            1,
            "verify={verify}: repeated failure of the requested retry must be visible"
        );
        assert_eq!(cards(&h).len(), 2);
    }
}
#[test]
fn followup_person_retry_bypasses_ownership_backoff() {
    for verify in [false, true] {
        let (mut h, mut e, mut r) = followup_ownership_fault_card(verify);
        let card = cards(&h)[0].seq;
        h.run(
            &ops::tests::person(),
            ops::Operation::DecideCard {
                card,
                decision: "retry".into(),
            },
        )
        .unwrap();
        let before = row(&h, "lead").failures;
        for _ in 0..5 {
            let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, 181_234);
        }
        assert!(
            row(&h, "lead").failures > before,
            "verify={verify}: person retry was ignored until {}",
            row(&h, "lead").retry_at
        );
    }
}
#[test]
fn followup_ownership_card_explains_when_it_retries() {
    for verify in [false, true] {
        let (h, _, _) = followup_ownership_fault_card(verify);
        eprintln!(
            "verify={verify} retry_at={} text={}",
            row(&h, "lead").retry_at,
            cards(&h)[0].text
        );
        assert!(
            cards(&h)[0].text.contains("retry") || cards(&h)[0].text.contains("Retry"),
            "retryable ownership fault has no timing or retry guidance"
        );
    }
}
#[test]
fn followup_stopped_budget_card_is_closed_before_outbox_is_pruned() {
    let mut h = Harness::new();
    drop(Ledger::open(h.temp.path()).unwrap());
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    db.execute_batch("UPDATE launch_budget SET rows=16384 WHERE id=1")
        .unwrap();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    for _ in 0..20 {
        let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, 1234);
    }
    assert_eq!(cards(&h).len(), 1);
    h.run(&ops::tests::person(), ops::Operation::Stop).unwrap();
    r.maintain(&h.store).unwrap();
    assert!(Ledger::open(h.temp.path())
        .unwrap()
        .get(ops::tests::TEAM, super::ledger::BUDGET)
        .unwrap()
        .is_none());
    eprintln!("card after stop and prune: {:?}", cards(&h));
    assert_eq!(
        cards(&h)[0].decision.as_deref(),
        Some("stale"),
        "the service discarded the only owner of its open capacity card"
    );
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn followup_full_notification_outbox_is_visible() {
    let mut h = Harness::new();
    drop(Ledger::open(h.temp.path()).unwrap());
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    db.execute_batch("UPDATE launch_budget SET rows=16384 WHERE id=1; UPDATE notice_budget SET rows=16384 WHERE id=1").unwrap();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    let mut cursor = None;
    let mut faulted = false;
    let stop = std::sync::atomic::AtomicBool::new(false);
    super::worker::pass(
        &mut r,
        &mut h.store,
        &mut e,
        &mut cursor,
        &mut faulted,
        &stop,
    )
    .unwrap();
    assert!(e.events.is_empty());
    assert!(cards(&h).is_empty());
    let first =
        super::health(h.temp.path()).expect("a full outbox must remain visible in service health");
    assert_eq!(first.code, "launch_notification_overflow");
    assert!(first.at_ms > 0);
    drop(r);
    assert_eq!(super::health(h.temp.path()), Some(first));
    // Both fixed budgets recover. A successful pass clears the health record.
    db.execute_batch("UPDATE launch_budget SET rows=(SELECT COUNT(*) FROM originals) WHERE id=1; UPDATE notice_budget SET rows=(SELECT COUNT(*) FROM budget_alerts) WHERE id=1").unwrap();
    drop(db);
    let mut r = runtime(&h);
    super::worker::pass(
        &mut r,
        &mut h.store,
        &mut e,
        &mut cursor,
        &mut faulted,
        &stop,
    )
    .unwrap();
    assert_eq!(super::health(h.temp.path()), None);
}

#[test]
fn followup_observation_failure_after_deadline_is_not_absence() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |_, e| !e.plans.is_empty());
    e.read_error = true;
    for now in [1_000_000, 2_000_000, 3_000_000] {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, now).unwrap();
    }
    assert_eq!(row(&h, "lead").state, State::Submitted);
    assert_eq!(cards(&h)[0].kind, "launch_blocked");
    offered_as_read(&cards(&h)[0]);
    assert_eq!(e.plans.len(), 1);
    e.read_error = false;
    r.tick(&mut h.store, &mut e, ops::tests::TEAM, 10_000_000)
        .unwrap();
    assert!(h
        .store
        .member(ops::tests::TEAM, "lead")
        .unwrap()
        .unwrap()
        .session
        .is_some());
}
#[test]
fn followup_retry_decision_survives_restart() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        account_error: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 20);
    let card = cards(&h)[0].seq;
    h.run(
        &ops::tests::person(),
        ops::Operation::DecideCard {
            card,
            decision: "retry".into(),
        },
    )
    .unwrap();
    drop(r);
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 5);
    assert_eq!(cards(&h).iter().filter(|c| c.state == "open").count(), 1);
    assert_eq!(cards(&h).len(), 2);
}
#[test]
fn followup_new_states_obey_pause_remove_and_stop() {
    for verify in [false, true] {
        for decision in ["pause", "remove", "stop"] {
            let mut h = Harness::new();
            let mut e = FakeEffects {
                exists: true,
                worktree_unverified: verify,
                ..Default::default()
            };
            let mut r = runtime(&h);
            until(&mut r, &mut h, &mut e, |h, _| {
                row_if(h, "m1").is_some_and(|r| r.state == State::WorktreeIntent)
            });
            tick(&mut r, &mut h, &mut e);
            assert_eq!(
                row(&h, "m1").state,
                if verify {
                    State::WorktreeVerify
                } else {
                    State::WorktreeDone
                }
            );
            let op = match decision {
                "pause" => ops::Operation::SetMemberState {
                    member: "m1".into(),
                    state: MemberState::Paused,
                },
                "remove" => ops::Operation::ArchiveMember {
                    member: "m1".into(),
                },
                _ => ops::Operation::Stop,
            };
            let actor = if decision == "pause" {
                ops::tests::service()
            } else {
                ops::tests::person()
            };
            h.run(&actor, op).unwrap();
            drop(r);
            let mut r = runtime(&h);
            advance(&mut r, &mut h, &mut e, 40);
            assert_eq!(
                e.events
                    .iter()
                    .filter(|e| e.as_str() == "worktree:m1")
                    .count(),
                1
            );
            assert!(
                !e.events.contains(&"launch:m1".into()),
                "{verify}/{decision}"
            );
        }
    }
}
#[test]
fn followup_failed_save_of_new_outcomes_never_replays_effect() {
    for kind in ["worktree", "launch"] {
        let mut h = Harness::new();
        let mut e = LockingEffects {
            inner: FakeEffects {
                exists: true,
                worktree_unverified: kind == "worktree",
                launch_verification_unavailable: kind == "launch",
                ..Default::default()
            },
            path: h.temp.path().join("teams/member-launch-v1.sqlite3"),
            lock: None,
            lock_on: kind,
            fired: false,
        };
        let mut r = runtime(&h);
        let mut failed = false;
        for _ in 0..15 {
            if r.tick(&mut h.store, &mut e, ops::tests::TEAM, 1234)
                .is_err()
            {
                e.lock = None;
                failed = true;
                break;
            }
        }
        assert!(failed);
        drop(r);
        let mut r = runtime(&h);
        for _ in 0..20 {
            r.tick(&mut h.store, &mut e, ops::tests::TEAM, 10_000_000)
                .unwrap();
        }
        assert_eq!(row(&h, "lead").state, State::Blocked);
        assert_eq!(cards(&h).len(), 1);
        assert_eq!(cards(&h)[0].kind, "launch_uncertain");
        offered_as_read(&cards(&h)[0]);
        assert_eq!(
            e.inner
                .events
                .iter()
                .filter(|e| e.as_str() == "worktree:lead")
                .count(),
            1
        );
        assert!(e.inner.plans.is_empty());
    }
}
#[test]
fn followup_v2_all_states_migrate_unchanged_and_failed_ddl_rolls_back() {
    use super::ledger::Row;
    let h = Harness::new();
    let team = h.store.team(ops::tests::TEAM).unwrap().unwrap();
    let mut ledger = Ledger::open(h.temp.path()).unwrap();
    let states = [
        State::Retry,
        State::LaunchUnknown,
        State::BranchReady,
        State::BranchIntent,
        State::BranchDone,
        State::WorktreeReady,
        State::WorktreeIntent,
        State::WorktreeDone,
        State::LaunchReady,
        State::LaunchIntent,
        State::Submitted,
        State::Bound,
        State::ParentIntent,
        State::Done,
        State::Blocked,
    ];
    let rows: Vec<_> = states
        .into_iter()
        .enumerate()
        .map(|(n, s)| Row::new(&team, &format!("fixture-{n}"), EPOCH, s))
        .collect();
    for row in &rows {
        ledger.insert(row, 1).unwrap();
    }
    drop(ledger);
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    db.execute_batch("DROP TABLE budget_alerts; DROP TABLE notice_budget; DROP TABLE launch_health; PRAGMA user_version=2; CREATE TABLE notice_budget (fixture TEXT)").unwrap();
    assert!(Ledger::open(h.temp.path()).is_err());
    let version: u32 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let alerts: u32 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='budget_alerts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(alerts, 0, "the partial DDL must roll back");
    db.execute_batch("DROP TABLE notice_budget").unwrap();
    let ledger = Ledger::open(h.temp.path()).unwrap();
    for saved in rows {
        assert_eq!(ledger.get(&saved.team, &saved.member).unwrap(), Some(saved));
    }
    let version: u32 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, super::ledger::SCHEMA);
}

#[test]
fn followup_retry_after_ownership_fault_reopens_one_card_verify_only() {
    {
        let verify = true;
        let (mut h, mut e, r) = followup_ownership_fault_card(verify);
        let card = cards(&h)[0].seq;
        h.run(
            &ops::tests::person(),
            ops::Operation::DecideCard {
                card,
                decision: "retry".into(),
            },
        )
        .unwrap();
        drop(r);
        let mut r = runtime(&h);
        for now in [181_234, 500_000, 2_000_000, 10_000_000] {
            for _ in 0..5 {
                let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, now);
            }
        }
        eprintln!(
            "verify={verify} row={:?} failures={} retry_at={} retry_seen={} cards={:?}",
            row(&h, "lead").state,
            row(&h, "lead").failures,
            row(&h, "lead").retry_at,
            row(&h, "lead").retry_seen,
            cards(&h)
        );
        assert_eq!(
            cards(&h).iter().filter(|c| c.state == "open").count(),
            1,
            "verify={verify}: repeated failure of the requested retry must be visible"
        );
        assert_eq!(cards(&h).len(), 2);
    }
}

#[test]
fn followup_person_retry_bypasses_ownership_backoff_verify_only() {
    {
        let verify = true;
        let (mut h, mut e, mut r) = followup_ownership_fault_card(verify);
        let card = cards(&h)[0].seq;
        h.run(
            &ops::tests::person(),
            ops::Operation::DecideCard {
                card,
                decision: "retry".into(),
            },
        )
        .unwrap();
        let before = row(&h, "lead").failures;
        for _ in 0..5 {
            let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, 181_234);
        }
        assert!(
            row(&h, "lead").failures > before,
            "verify={verify}: person retry was ignored until {}",
            row(&h, "lead").retry_at
        );
    }
}

#[test]
fn followup_ownership_card_explains_when_it_retries_verify_only() {
    {
        let verify = true;
        let (h, _, _) = followup_ownership_fault_card(verify);
        eprintln!(
            "verify={verify} retry_at={} text={}",
            row(&h, "lead").retry_at,
            cards(&h)[0].text
        );
        assert!(
            cards(&h)[0].text.contains("retry") || cards(&h)[0].text.contains("Retry"),
            "retryable ownership fault has no timing or retry guidance"
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn worker_fault_health_survives_a_healthy_partial_sweep_and_clears_after_recovery() {
    let mut h = Harness::new();
    let team = h.store.team(ops::tests::TEAM).unwrap().unwrap();
    // A malformed persisted key fails before a readable row can own it.
    drop(Ledger::open(h.temp.path()).unwrap());
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO originals(team,member,version,data) VALUES(?1,'@branch',0,'{}')",
        [&team.id],
    )
    .unwrap();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    let mut cursor = None;
    let mut faulted = false;
    let stop = std::sync::atomic::AtomicBool::new(false);
    super::worker::pass(
        &mut r,
        &mut h.store,
        &mut e,
        &mut cursor,
        &mut faulted,
        &stop,
    )
    .unwrap();
    assert_eq!(
        super::health(h.temp.path()).unwrap().code,
        "launch_worker_fault"
    );
    assert!(e.plans.is_empty());
    db.execute(
        "DELETE FROM originals WHERE team=?1 AND member='@branch'",
        [&team.id],
    )
    .unwrap();
    drop(db);
    let repaired = super::ledger::Row::new(&team, "lead", EPOCH, State::Done);
    Ledger::open(h.temp.path())
        .unwrap()
        .insert(&repaired, 1235)
        .unwrap();
    // Model the last page of a sweep with a fault on an earlier page.
    faulted = true;
    super::worker::pass(
        &mut r,
        &mut h.store,
        &mut e,
        &mut cursor,
        &mut faulted,
        &stop,
    )
    .unwrap();
    assert!(super::health(h.temp.path()).is_some());
    super::worker::pass(
        &mut r,
        &mut h.store,
        &mut e,
        &mut cursor,
        &mut faulted,
        &stop,
    )
    .unwrap();
    assert_eq!(super::health(h.temp.path()), None);
}

// Health-narrowing regressions from the independent confirmation review.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn review07r2_backoff_is_not_evidence_that_a_fault_recovered() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        worktree_unverified: true,
        read_error: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |h, _| {
        row_if(h, "lead").is_some_and(|r| r.state == State::WorktreeVerify)
    });
    let mut cursor = None;
    let mut faulted = false;
    let stop = std::sync::atomic::AtomicBool::new(false);
    super::worker::pass(
        &mut r,
        &mut h.store,
        &mut e,
        &mut cursor,
        &mut faulted,
        &stop,
    )
    .unwrap();
    let before = super::health(h.temp.path());
    assert_eq!(
        before, None,
        "a row failure belongs to its row, not worker health"
    );
    let failed = row(&h, "lead");
    assert_eq!(failed.failures, 1);
    assert!(cards(&h).is_empty());
    super::worker::pass(
        &mut r,
        &mut h.store,
        &mut e,
        &mut cursor,
        &mut faulted,
        &stop,
    )
    .unwrap();
    assert_eq!(
        row(&h, "lead"),
        failed,
        "the read was skipped, not successfully repeated"
    );
    eprintln!(
        "backoff: before={before:?} after={:?}, failure={} retry_at={}",
        super::health(h.temp.path()),
        failed.failures,
        failed.retry_at
    );
    assert_eq!(
        super::health(h.temp.path()),
        before,
        "a skipped backed-off check cannot clear a still-present fault"
    );
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn review07r2_real_pages_do_not_clear_an_unresolved_earlier_team() {
    use super::ledger::Row;
    let mut h = Harness::empty();
    let mut ledger = Ledger::open(h.temp.path()).unwrap();
    let first = "00000000-0000-4000-8000-000000000100";
    for n in 0..17 {
        let id = format!("00000000-0000-4000-8000-{:012x}", 256 + n);
        let mut op = ops::tests::create();
        if let ops::Operation::Create {
            integration_branch, ..
        } = &mut op
        {
            *integration_branch = format!("review-branch-{n}");
        }
        h.run_for(&id, &ops::tests::person(), op).unwrap();
        let team = h.store.team(&id).unwrap().unwrap();
        let mut saved = Row::new(
            &team,
            "lead",
            EPOCH,
            if n == 0 {
                State::WorktreeVerify
            } else {
                State::Done
            },
        );
        if n == 0 {
            let mut p = plan_fixture();
            p.team = id.clone();
            saved.plan = Some(p);
        }
        ledger.insert(&saved, 1234).unwrap();
    }
    let mut e = FakeEffects {
        exists: true,
        read_error: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    let mut cursor = None;
    let mut faulted = false;
    let stop = std::sync::atomic::AtomicBool::new(false);
    super::worker::pass(
        &mut r,
        &mut h.store,
        &mut e,
        &mut cursor,
        &mut faulted,
        &stop,
    )
    .unwrap();
    assert!(cursor.is_some());
    assert!(!faulted, "a row failure is not a sweep failure");
    let before = super::health(h.temp.path());
    assert_eq!(
        before, None,
        "a row failure belongs to its row, not worker health"
    );
    let failed = ledger.get(first, "lead").unwrap().unwrap();
    super::worker::pass(
        &mut r,
        &mut h.store,
        &mut e,
        &mut cursor,
        &mut faulted,
        &stop,
    )
    .unwrap();
    assert!(cursor.is_none());
    assert_eq!(
        super::health(h.temp.path()),
        before,
        "last healthy page must preserve the earlier fault"
    );
    // The next two real pages include the same team, but its backoff skips
    // verification. No condition recovered anywhere in this fixture.
    for _ in 0..2 {
        super::worker::pass(
            &mut r,
            &mut h.store,
            &mut e,
            &mut cursor,
            &mut faulted,
            &stop,
        )
        .unwrap();
    }
    assert_eq!(ledger.get(first, "lead").unwrap().unwrap(), failed);
    eprintln!(
        "two sweeps: first={before:?} after={:?}",
        super::health(h.temp.path())
    );
    assert_eq!(
        super::health(h.temp.path()),
        before,
        "healthy other teams cannot clear the backed-off fault"
    );
}
#[test]
fn review07r2_one_free_byte_is_not_notice_capacity_recovery() {
    let h = Harness::empty();
    let mut r = runtime(&h);
    let problem = zerus_team_plan::Problem::new(code::OVER_CAP, "fixture overflow")
        .with_refs(["launch_notification_outbox"]);
    r.record_health(&problem, 1234).unwrap();
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    db.execute("UPDATE notice_budget SET bytes=67108863 WHERE id=1", [])
        .unwrap();
    r.clear_health().unwrap();
    assert!(
        super::health(h.temp.path()).is_some(),
        "one byte cannot admit or update a notification, so overflow is still present"
    );
}
#[test]
fn review07r2_v3_migration_preserves_every_state_and_is_atomic() {
    use super::ledger::Row;
    let h = Harness::new();
    let team = h.store.team(ops::tests::TEAM).unwrap().unwrap();
    let mut ledger = Ledger::open(h.temp.path()).unwrap();
    let states = [
        State::Retry,
        State::LaunchUnknown,
        State::BranchReady,
        State::BranchIntent,
        State::BranchDone,
        State::WorktreeReady,
        State::WorktreeIntent,
        State::WorktreeDone,
        State::WorktreeVerify,
        State::LaunchReady,
        State::LaunchIntent,
        State::Submitted,
        State::Bound,
        State::ParentIntent,
        State::Done,
        State::Blocked,
    ];
    let mut rows: Vec<_> = states
        .into_iter()
        .enumerate()
        .map(|(n, s)| Row::new(&team, &format!("fixture-{n}"), EPOCH, s))
        .collect();
    rows.push(Row::new(&team, super::ledger::BUDGET, EPOCH, State::Retry));
    for row in &rows {
        ledger.insert(row, 1).unwrap();
    }
    drop(ledger);
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    let before: String = db
        .query_row(
            "SELECT group_concat(data,'|') FROM (SELECT data FROM originals ORDER BY member)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.execute_batch("DROP TABLE launch_health; PRAGMA user_version=3; CREATE TRIGGER preserve_launch_health AFTER INSERT ON originals BEGIN SELECT 1; END;").unwrap();
    assert_eq!(super::health(h.temp.path()), None);
    assert!(Ledger::open(h.temp.path()).is_err());
    let version: u32 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let count: u32 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='launch_health'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    db.execute_batch("DROP TRIGGER preserve_launch_health")
        .unwrap();
    let ledger = Ledger::open(h.temp.path()).unwrap();
    for row in rows {
        assert_eq!(ledger.get(&row.team, &row.member).unwrap(), Some(row));
    }
    let after: String = db
        .query_row(
            "SELECT group_concat(data,'|') FROM (SELECT data FROM originals ORDER BY member)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, before);
    let version: u32 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 4);
    assert_eq!(super::health(h.temp.path()), None);
}
#[test]
fn review07r2_repeated_person_retries_survive_restart_and_never_duplicate_effects() {
    for verify in [false, true] {
        let (mut h, mut e, r) = followup_ownership_fault_card(verify);
        drop(r);
        for round in 0..3 {
            let open: Vec<_> = cards(&h)
                .into_iter()
                .filter(|c| c.state == "open")
                .collect();
            assert_eq!(open.len(), 1);
            h.run(
                &ops::tests::person(),
                ops::Operation::DecideCard {
                    card: open[0].seq,
                    decision: "retry".into(),
                },
            )
            .unwrap();
            let mut r = runtime(&h);
            for _ in 0..5 {
                let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, 181_234);
                assert!(cards(&h).iter().filter(|c| c.state == "open").count() <= 1);
            }
            assert_eq!(
                cards(&h).iter().filter(|c| c.state == "open").count(),
                1,
                "{verify}/{round}"
            );
            assert_eq!(cards(&h).len(), 2 + round);
            assert_eq!(
                row(&h, "lead").state,
                if verify {
                    State::WorktreeVerify
                } else {
                    State::LaunchReady
                }
            );
        }
        assert_eq!(
            e.events
                .iter()
                .filter(|e| e.as_str() == "worktree:lead")
                .count(),
            1
        );
        assert!(e.plans.is_empty());
    }
}
#[test]
fn review07r2_health_read_has_no_logical_writes_or_initialization() {
    let h = Harness::empty();
    assert_eq!(super::health(h.temp.path()), None);
    assert!(!h
        .temp
        .path()
        .join("teams/member-launch-v1.sqlite3")
        .exists());
    let mut r = runtime(&h);
    r.record_health(&invalid("fixture fault"), 1234).unwrap();
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let before: i64 = db
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap();
    for _ in 0..3 {
        assert_eq!(super::health(h.temp.path()).unwrap().at_ms, 1234);
    }
    let after: i64 = db
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
    let after_version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, after_version);
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM launch_health", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn notification_overflow_clears_after_its_notice_reservation_succeeds() {
    let mut h = Harness::new();
    drop(Ledger::open(h.temp.path()).unwrap());
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    db.execute_batch("UPDATE launch_budget SET rows=16384 WHERE id=1; UPDATE notice_budget SET rows=16384 WHERE id=1;").unwrap();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    let mut cursor = None;
    let mut faulted = false;
    let stop = std::sync::atomic::AtomicBool::new(false);
    super::worker::pass(
        &mut r,
        &mut h.store,
        &mut e,
        &mut cursor,
        &mut faulted,
        &stop,
    )
    .unwrap();
    assert_eq!(
        super::health(h.temp.path()).unwrap().code,
        "launch_notification_overflow"
    );
    db.execute(
        "UPDATE notice_budget SET rows=(SELECT COUNT(*) FROM budget_alerts) WHERE id=1",
        [],
    )
    .unwrap();
    super::worker::pass(
        &mut r,
        &mut h.store,
        &mut e,
        &mut cursor,
        &mut faulted,
        &stop,
    )
    .unwrap();
    assert_eq!(cards(&h).len(), 1);
    assert_eq!(cards(&h)[0].kind, "launch_capacity");
    offered_as_read(&cards(&h)[0]);
    let rows: i64 = db
        .query_row("SELECT rows FROM launch_budget WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 16384, "only notice capacity recovered");
    assert_eq!(super::health(h.temp.path()), None);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn tick_failure_before_a_new_row_is_not_owned_by_the_previous_step() {
    let mut h = Harness::new();
    drop(Ledger::open(h.temp.path()).unwrap());
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO originals(team,member,version,data) VALUES(?1,'lead',0,'{}')",
        [ops::tests::TEAM],
    )
    .unwrap();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    let mut cursor = None;
    let mut faulted = false;
    let stop = std::sync::atomic::AtomicBool::new(false);
    super::worker::pass(
        &mut r,
        &mut h.store,
        &mut e,
        &mut cursor,
        &mut faulted,
        &stop,
    )
    .unwrap();
    assert_eq!(
        super::health(h.temp.path()).unwrap().code,
        "launch_worker_fault"
    );
    assert_eq!(
        row(&h, BRANCH).failures,
        0,
        "the completed branch does not own a subsequent unreadable member"
    );
    assert!(e.events.is_empty());
    assert_eq!(e.account_reads, 0);
}

#[test]
fn updating_an_existing_capacity_card_is_not_a_new_notice_reservation() {
    let mut h = Harness::new();
    drop(Ledger::open(h.temp.path()).unwrap());
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    db.execute("UPDATE launch_budget SET rows=16384 WHERE id=1", [])
        .unwrap();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    r.tick(&mut h.store, &mut e, ops::tests::TEAM, 1234)
        .unwrap();
    let card = cards(&h)[0].seq;
    r.record_health(
        &zerus_team_plan::Problem::new(code::OVER_CAP, "fixture overflow")
            .with_refs(["launch_notification_outbox"]),
        1234,
    )
    .unwrap();
    db.execute("UPDATE notice_budget SET bytes=67107840 WHERE id=1", [])
        .unwrap();
    // Updating the old row's counter fits; a new notification was never reserved.
    r.tick(&mut h.store, &mut e, ops::tests::TEAM, 61_234)
        .unwrap();
    assert_eq!(cards(&h).len(), 1);
    assert_eq!(cards(&h)[0].seq, card);
    assert_eq!(
        super::health(h.temp.path()).unwrap().code,
        "launch_notification_overflow"
    );
}

/// A launch card lists retry exactly when something reads it: the budget
/// row on every pass, a member row while it waits on backoff, which is when
/// its text offers Retry. Retry comes first and acknowledged follows. Any
/// other launch card takes acknowledged alone.
fn offered_as_read(card: &crate::teams::store::Card) {
    let read = card.kind == "launch_capacity" || card.text.contains("Choosing Retry");
    let expected: &[&str] = if read {
        &["retry", "acknowledged"]
    } else {
        &["acknowledged"]
    };
    assert_eq!(card.decisions, expected, "{}: {}", card.kind, card.text);
}

/// Finding 1 of the cards review, the reviewer's sequence: a launch_blocked
/// card opened while worktree verification waits on backoff lists retry;
/// verification then fails for good and the row is Blocked. The next pass
/// closes that card and opens the current one, which lists acknowledged; a
/// Retry on the closed card is refused.
/// The reviewer's sequence up to the replacement: a launch_blocked card
/// opened while worktree verification waits on backoff lists retry, then
/// verification fails for good and the row is Blocked. The pass that would
/// replace the card has not run. Returns the old card and the time.
fn blocked_behind_a_retry_card() -> (Harness, FakeEffects, Runtime, u64, i64) {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        worktree_unverified: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    for _ in 0..30 {
        tick(&mut r, &mut h, &mut e);
        if row_if(&h, "lead").is_some_and(|x| x.state == State::WorktreeVerify) {
            break;
        }
    }
    assert_eq!(row(&h, "lead").state, State::WorktreeVerify);
    e.read_error = true;
    for _ in 0..3 {
        let old = row(&h, "lead");
        let now = old.retry_at.max(1234) + 1;
        assert!(r.tick(&mut h.store, &mut e, ops::tests::TEAM, now).is_err());
    }
    let old = row(&h, "lead");
    let first = old.card_id.unwrap();
    assert_eq!(
        h.store
            .card(ops::tests::TEAM, first)
            .unwrap()
            .unwrap()
            .decisions,
        ["retry", "acknowledged"]
    );
    e.read_error = false;
    e.verify_uncertain = true;
    let now = old.retry_at + 1;
    r.tick(&mut h.store, &mut e, ops::tests::TEAM, now).unwrap();
    assert_eq!(row(&h, "lead").state, State::Blocked);
    assert_eq!(row(&h, "lead").card_id, Some(first));
    assert_eq!(cards(&h).len(), 1);
    (h, e, r, first, now)
}

#[test]
fn a_launch_card_is_replaced_when_its_row_stops_reading_retry() {
    let (mut h, mut e, mut r, first, now) = blocked_behind_a_retry_card();
    r.tick(&mut h.store, &mut e, ops::tests::TEAM, now + 1)
        .unwrap();
    let closed = h.store.card(ops::tests::TEAM, first).unwrap().unwrap();
    assert_eq!(
        (closed.state.as_str(), closed.decision.as_deref()),
        ("closed", Some("stale")),
        "the card that offered retry is closed"
    );
    let open: Vec<_> = cards(&h)
        .into_iter()
        .filter(|c| c.state == "open")
        .collect();
    assert_eq!(open.len(), 1, "one current card");
    assert_eq!(open[0].decisions, ["acknowledged"]);
    offered_as_read(&open[0]);
    assert_eq!(row(&h, "lead").card_id, Some(open[0].seq));
    assert_eq!(
        h.run(
            &ops::tests::person(),
            ops::Operation::DecideCard {
                card: first,
                decision: "retry".into(),
            },
        )
        .unwrap_err()
        .code,
        code::INVALID_STATE
    );
    r.tick(&mut h.store, &mut e, ops::tests::TEAM, now + 2)
        .unwrap();
    assert_eq!(cards(&h).len(), 2, "kept while it lists what is read");
}

/// Decides a card as the person.
fn decide(h: &mut Harness, card: u64, decision: &str) {
    h.run(
        &ops::tests::person(),
        ops::Operation::DecideCard {
            card,
            decision: decision.into(),
        },
    )
    .unwrap();
}

/// The budget card lists retry, then acknowledged. Acknowledged closes it:
/// the row keeps the closed card, so no card opens again while the row
/// stays, and its automatic backoff goes on. After capacity recovers, a later
/// shortage opens a new card whose Retry is read as before.
#[test]
fn an_acknowledged_budget_card_stays_closed_while_its_row_backs_off() {
    let mut h = Harness::new();
    let team = h.store.team(ops::tests::TEAM).unwrap().unwrap();
    let mut ledger = Ledger::open(h.temp.path()).unwrap();
    let mut alert = super::ledger::Row::new(&team, super::ledger::BUDGET, EPOCH, State::Retry);
    alert.problem = Some(
        zerus_team_plan::Problem::new(
            code::OVER_CAP,
            "Team member creation is temporarily at capacity.",
        )
        .with_refs(["member_launch_ledger"]),
    );
    ledger.insert(&alert, 1234).unwrap();
    drop(ledger);
    let db =
        rusqlite::Connection::open(h.temp.path().join("teams/member-launch-v1.sqlite3")).unwrap();
    db.execute("UPDATE launch_budget SET rows=16384 WHERE id=1", [])
        .unwrap();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    for _ in 0..5 {
        let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, 1234);
    }
    assert_eq!(cards(&h).len(), 1);
    let first = cards(&h)[0].clone();
    assert_eq!(first.kind, "launch_capacity");
    assert_eq!(first.decisions, ["retry", "acknowledged"]);
    decide(&mut h, first.seq, "acknowledged");
    let budget = |h: &Harness| row(h, super::ledger::BUDGET);
    let before = budget(&h);
    for _ in 0..3 {
        let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, before.retry_at - 1);
    }
    assert_eq!(
        budget(&h).failures,
        before.failures,
        "acknowledged is not a retry"
    );
    for _ in 0..3 {
        let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, before.retry_at);
    }
    assert!(budget(&h).failures > before.failures, "the backoff's check");
    let after = budget(&h);
    assert!(after.retry_at > before.retry_at);
    for _ in 0..3 {
        let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, after.retry_at);
    }
    assert!(budget(&h).failures > after.failures, "the next check");
    assert_eq!(
        cards(&h).len(),
        1,
        "no card opens again while the row stays"
    );
    assert_eq!(
        (
            cards(&h)[0].state.as_str(),
            cards(&h)[0].decision.as_deref()
        ),
        ("closed", Some("acknowledged"))
    );
    // Capacity recovers: the lead's row is reserved and the budget row ends,
    // leaving the acknowledged card as it was.
    drop(r);
    db.execute(
        "UPDATE launch_budget SET rows=(SELECT COUNT(*) FROM originals) WHERE id=1",
        [],
    )
    .unwrap();
    let now = budget(&h).retry_at;
    let mut r = runtime(&h);
    for _ in 0..100 {
        if row_if(&h, "lead").is_some() {
            break;
        }
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, now).unwrap();
    }
    assert!(row_if(&h, "lead").is_some());
    assert_eq!(budget(&h).state, State::Done);
    assert_eq!(cards(&h).len(), 1);
    assert_eq!(cards(&h)[0].decision.as_deref(), Some("acknowledged"));
    for _ in 0..100 {
        if row(&h, "lead").state == State::Bound {
            break;
        }
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, now).unwrap();
    }
    assert_eq!(row(&h, "lead").state, State::Bound);
    // A later shortage, at the next member's reservation, opens a new card,
    // and its Retry is read as before: the row runs before its backoff.
    db.execute("UPDATE launch_budget SET rows=16384 WHERE id=1", [])
        .unwrap();
    for _ in 0..100 {
        if cards(&h).len() == 2 {
            break;
        }
        let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, now);
    }
    assert_eq!(cards(&h).len(), 2, "a later shortage opens a new card");
    let second = cards(&h).into_iter().find(|c| c.state == "open").unwrap();
    assert_eq!(second.decisions, ["retry", "acknowledged"]);
    decide(&mut h, second.seq, "retry");
    let before = budget(&h);
    assert!(before.retry_at > now);
    // Passes before the row's backoff ends.
    for _ in 0..3 {
        let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, now);
    }
    assert!(
        budget(&h).failures > before.failures,
        "the retry runs the row early"
    );
}

/// A member row's card while it waits on backoff lists retry, then
/// acknowledged. Acknowledged closes it: the row keeps the closed card, so
/// no card opens again while the row stays, and its automatic backoff goes
/// on. After the row moves on, a later backoff opens a new card whose Retry
/// is read as before.
#[test]
fn an_acknowledged_backoff_card_stays_closed_while_its_row_backs_off() {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        account_error: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 20);
    assert_eq!(cards(&h).len(), 1);
    let first = cards(&h)[0].clone();
    assert_eq!(first.kind, "launch_blocked");
    assert_eq!(row(&h, "lead").state, State::Retry);
    assert_eq!(first.decisions, ["retry", "acknowledged"]);
    offered_as_read(&first);
    decide(&mut h, first.seq, "acknowledged");
    let before = row(&h, "lead");
    for _ in 0..3 {
        r.begin_pass();
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, before.retry_at - 1)
            .unwrap();
    }
    assert_eq!(
        row(&h, "lead").failures,
        before.failures,
        "acknowledged is not a retry"
    );
    r.begin_pass();
    r.tick(&mut h.store, &mut e, ops::tests::TEAM, before.retry_at)
        .unwrap();
    let after = row(&h, "lead");
    assert!(after.failures > before.failures, "the backoff's check");
    r.begin_pass();
    r.tick(&mut h.store, &mut e, ops::tests::TEAM, after.retry_at)
        .unwrap();
    assert!(row(&h, "lead").failures > after.failures, "the next check");
    assert_eq!(
        cards(&h).len(),
        1,
        "no card opens again while the row stays"
    );
    assert_eq!(
        (
            cards(&h)[0].state.as_str(),
            cards(&h)[0].decision.as_deref()
        ),
        ("closed", Some("acknowledged"))
    );
    // The account recovers and the row moves on, leaving the acknowledged
    // card as it was; the launch's ownership read then backs off.
    e.account_error = false;
    e.launch_verification_unavailable = true;
    let mut now = row(&h, "lead").retry_at;
    for _ in 0..100 {
        if cards(&h).len() == 2 {
            break;
        }
        r.begin_pass();
        let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, now);
        if cards(&h).len() == 2 {
            break;
        }
        now = now.max(row(&h, "lead").retry_at);
    }
    assert_eq!(cards(&h)[0].decision.as_deref(), Some("acknowledged"));
    assert_eq!(cards(&h).len(), 2, "a later backoff opens a new card");
    let second = cards(&h).into_iter().find(|c| c.state == "open").unwrap();
    assert_eq!(second.decisions, ["retry", "acknowledged"]);
    offered_as_read(&second);
    decide(&mut h, second.seq, "retry");
    let before = row(&h, "lead");
    assert!(before.retry_at > now);
    // Passes before the row's backoff ends.
    for _ in 0..3 {
        r.begin_pass();
        let _ = r.tick(&mut h.store, &mut e, ops::tests::TEAM, now);
    }
    assert!(
        row(&h, "lead").failures > before.failures,
        "the retry runs the row early"
    );
}

/// The one current card of the lead's row: open, listing acknowledged, and
/// named by the row, with the old card closed beside it.
fn the_blocked_card_is_current(h: &Harness, why: &str) {
    let open: Vec<_> = cards(h).into_iter().filter(|c| c.state == "open").collect();
    assert_eq!(open.len(), 1, "{why}: one current card");
    assert_eq!(open[0].decisions, ["acknowledged"], "{why}");
    assert_eq!(cards(h).len(), 2, "{why}: the old card and the current one");
    let lead = row(h, "lead");
    assert_eq!(lead.card_id, Some(open[0].seq), "{why}");
    assert!(lead.card_done, "{why}");
    assert_eq!(lead.state, State::Blocked, "{why}");
}

/// Finding 4 of the cards review, first sequence, and every other durable
/// step of the replacement: a restart after the old card's CloseCard
/// committed (the row still names it), after the row's reset was saved,
/// after the new request was saved, and after it was applied. Each state is
/// written as that step leaves it, then a new runtime runs two passes: the
/// current card opens exactly once.
#[test]
fn a_restart_at_each_durable_step_of_a_card_replacement_opens_the_current_card_once() {
    for step in 1..=4 {
        let (mut h, mut e, r, first, now) = blocked_behind_a_retry_card();
        drop(r);
        h.run(
            &ops::tests::service(),
            ops::Operation::CloseCard {
                card: first,
                reason: "The launch's state changed what this card can take.".into(),
            },
        )
        .unwrap();
        if step >= 2 {
            let mut ledger = Ledger::open(h.temp.path()).unwrap();
            let mut lead = ledger.get(ops::tests::TEAM, "lead").unwrap().unwrap();
            lead.card_id = None;
            lead.card_request = None;
            lead.card_done = false;
            if step >= 3 {
                let team = h.store.team(ops::tests::TEAM).unwrap().unwrap();
                lead.card_request = Some(ops::Request {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    team: ops::tests::TEAM.into(),
                    expected_revision: team.revision,
                    operation: ops::Operation::OpenCard {
                        kind: "launch_uncertain".into(),
                        text: "Member lead: delivery_uncertain. Worktree ownership changed".into(),
                        node: None,
                        decisions: Some(vec!["acknowledged".into()]),
                parameters: None,
                    },
                });
            }
            ledger.save(&mut lead, now).unwrap();
            if step == 4 {
                ops::apply(
                    &mut h.store,
                    &ops::tests::service(),
                    lead.card_request.as_ref().unwrap(),
                    now,
                )
                .unwrap();
            }
        }
        let mut r = runtime(&h);
        for pass in 1..=2 {
            r.tick(&mut h.store, &mut e, ops::tests::TEAM, now + pass)
                .unwrap();
        }
        let old = h.store.card(ops::tests::TEAM, first).unwrap().unwrap();
        assert_eq!(old.decision.as_deref(), Some("stale"), "step {step}");
        the_blocked_card_is_current(&h, &format!("step {step}"));
    }
}

/// Finding 4, second sequence: the person decides on the old card after the
/// row became Blocked and before the replacement ran. A Retry that a Blocked
/// row never reads, and an acknowledged given to another list, are no
/// dismissal of the Blocked card: it opens on the next pass, and the Retry
/// starts nothing.
#[test]
fn a_decision_on_the_old_list_after_the_row_is_blocked_still_opens_the_blocked_card() {
    for decision in ["retry", "acknowledged"] {
        let (mut h, mut e, mut r, first, now) = blocked_behind_a_retry_card();
        decide(&mut h, first, decision);
        let events = e.events.len();
        for pass in 1..=2 {
            r.tick(&mut h.store, &mut e, ops::tests::TEAM, now + pass)
                .unwrap();
        }
        let old = h.store.card(ops::tests::TEAM, first).unwrap().unwrap();
        assert_eq!(old.decision.as_deref(), Some(decision));
        the_blocked_card_is_current(&h, decision);
        assert_eq!(e.events.len(), events, "{decision}: nothing was retried");
    }
}

/// A real dismissal lasts: the person acknowledges the Blocked row's own
/// card, from the list the row gives, and no card opens again while the row
/// stays Blocked.
#[test]
fn a_persons_dismissal_of_the_blocked_card_lasts() {
    let (mut h, mut e, mut r, _, now) = blocked_behind_a_retry_card();
    r.tick(&mut h.store, &mut e, ops::tests::TEAM, now + 1)
        .unwrap();
    let current = row(&h, "lead").card_id.unwrap();
    decide(&mut h, current, "acknowledged");
    for pass in 2..=5 {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, now + pass * 60_000)
            .unwrap();
    }
    assert_eq!(cards(&h).len(), 2, "no card opens again");
    assert_eq!(row(&h, "lead").card_id, Some(current));
    assert_eq!(
        h.store
            .card(ops::tests::TEAM, current)
            .unwrap()
            .unwrap()
            .decision
            .as_deref(),
        Some("acknowledged")
    );
}

/// zerus-63rru.53: binding records each member's durable handle, the session
/// ID the launch's observation verified, with its name as the label hint.
#[test]
fn binding_records_the_members_durable_handle() {
    let mut h = Harness::new();
    let mut e = FakeEffects::default();
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |h, _| {
        row_if(h, "m1").is_some_and(|r| r.state == State::ParentIntent)
    });
    for member in ["lead", "m1"] {
        let observed = row(&h, member).observed.unwrap();
        let handle = h
            .store
            .member_handle(ops::tests::TEAM, member)
            .unwrap()
            .unwrap_or_else(|| panic!("{member} has no handle"));
        assert_eq!(handle.session_id, observed.session_id, "{member}");
        assert_eq!(handle.label_hint, observed.name, "{member}");
        assert_eq!(handle.launch, observed.launch_id, "{member}");
    }
}

// Inputs go through Create and the launch runtime; no persisted team, member,
// plan or binding rows are assembled by this fixture.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn named_plans_fixture(
    names: &[&str],
    placement: Option<(Placement, String)>,
) -> (Harness, Vec<Plan>) {
    let result = named_plans_with_integration(names, placement, None);
    assert_eq!(result.1.len(), names.len() * 3);
    result
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn named_plans_with_integration(
    names: &[&str],
    placement: Option<(Placement, String)>,
    integration: Option<&str>,
) -> (Harness, Vec<Plan>) {
    let mut h = Harness::empty();
    let mut effects = FakeEffects {
        exists: true,
        placement_override: placement.as_ref().map(|(p, _)| p.clone()),
        ..Default::default()
    };
    let mut runtime = runtime(&h);
    for (index, name) in names.iter().enumerate() {
        let id = format!("00000000-0000-4000-8000-{:012x}", 0xbeef + index);
        let mut create = ops::tests::create();
        if let ops::Operation::Create {
            name: label,
            bundle,
            integration_branch,
            base_commit,
            ..
        } = &mut create
        {
            *label = (*name).into();
            if let Some((_, base)) = &placement {
                *base_commit = base.clone();
            }
            bundle.pipelines[0].stages.clear();
            bundle.template.roster[0].count = 1;
            *integration_branch = integration.map_or_else(|| format!("teams/fixture-{index}"), str::to_owned);
        }
        h.run_for(&id, &ops::tests::person(), create).unwrap();
        for step in 0..60 {
            runtime
                .tick(&mut h.store, &mut effects, &id, 1234 + step)
                .unwrap();
        }
    }
    (h, effects.plans)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn bound151_fixture() -> Harness {
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        empty_conversation: true,
        worktree_unverified: true,
        ..Default::default()
    };
    advance(&mut runtime(&h), &mut h, &mut e, 60);
    assert!(matches!(
        row(&h, "lead").state,
        State::Bound | State::ParentIntent | State::Done
    ));
    h
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn bound161_fixture() -> Harness {
    planning::bound()
}

#[path = "edit_tests.rs"]
mod edits159;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn bound159_fixture() -> Harness {
    edits159::started().0
}

#[test]
fn launch_card_writers_establish_list_stuck_for_blocked_and_uncertain_launches() {
    let stuck = |h: &Harness| {
        let rows = h.store.teams(1, None).unwrap().rows;
        h.store.team_progress(&rows).unwrap()[ops::tests::TEAM].stuck
    };
    let open = |h: &Harness| {
        cards(h)
            .into_iter()
            .filter(|c| c.state == "open")
            .map(|c| c.kind)
            .collect::<Vec<_>>()
    };
    let mut h = Harness::new();
    assert!(!stuck(&h));
    let mut e = FakeEffects {
        unknown: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    advance(&mut r, &mut h, &mut e, 20);
    for _ in 0..5 {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, 301_234)
            .unwrap();
    }
    assert_eq!(open(&h), ["launch_uncertain"]);
    assert!(stuck(&h));
    let mut h = Harness::new();
    let mut e = FakeEffects {
        exists: true,
        ..Default::default()
    };
    let mut r = runtime(&h);
    until(&mut r, &mut h, &mut e, |_, e| !e.plans.is_empty());
    e.read_error = true;
    for now in [1234, 61_234, 181_234] {
        r.tick(&mut h.store, &mut e, ops::tests::TEAM, now).unwrap();
    }
    assert_eq!(open(&h), ["launch_blocked"]);
    assert!(stuck(&h));
}
