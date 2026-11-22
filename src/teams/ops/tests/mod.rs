use super::*;
mod apply;
mod authority;
pub(crate) mod base_advance;
mod base_engines;
mod base_fence;
mod cards;
mod decisions;
mod delegation;
mod dependencies;
mod handout;
mod integration;
mod members;
mod observations;
mod person_gesture;
mod person_gesture_context;
mod pipeline;
mod queue;
mod readiness;
mod removed_handout;
mod replace;
mod roster;
mod settings_fields;
mod storage;
mod talk;
mod team;
mod unstaffed;

pub(crate) const TEAM: &str = "00000000-0000-4000-8000-00000000beef";
pub(crate) const TEAM_B: &str = "00000000-0000-4000-8000-00000000beee";
pub(crate) const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
pub(crate) fn fixture() -> TeamFixture {
    serde_json::from_str(include_str!(
        "../../../../crates/team-plan/fixtures/team-queue.json"
    ))
    .unwrap()
}
pub(crate) fn person() -> Actor {
    Actor::Person {
        person: "local".into(),
        origin: PersonOrigin::DesktopUi,
    }
}
pub(crate) fn service() -> Actor {
    Actor::Service {
        person: "local".into(),
        team: TEAM.into(),
    }
}
pub(crate) fn other() -> Actor {
    Actor::Other {
        person: "local".into(),
        session: SessionKey {
            machine: "00000000-0000-4000-8000-0000000000bb".into(),
            session_id: "run_id:other-example".into(),
        },
    }
}
pub(crate) fn request(rev: u64, op: Operation) -> Request {
    Request {
        request_id: uuid::Uuid::new_v4().to_string(),
        team: TEAM.into(),
        expected_revision: rev,
        operation: op,
    }
}
pub(crate) fn create() -> Operation {
    let t = fixture().team;
    Operation::Create {
        expected_template: None,
        delegations: None,
        project: t.project,
        name: t.name,
        goal: t.goal,
        home: t.home,
        bundle: Box::new(t.bundle),
        integration_branch: t.integration_branch,
        base_commit: t.base_commit,
    }
}
pub(crate) fn task(n: u32) -> TaskRef {
    TaskRef {
        project: "example-project".into(),
        uuid: format!("00000000-0000-4000-8000-{n:012x}"),
    }
}
pub(crate) fn route(node: u32) -> Operation {
    Operation::Route {
        node,
        role: "developer".into(),
        pipeline: "reviewed".into(),
        deps: vec![],
        writes: vec![],
        assignee: None,
    }
}
pub(crate) fn refused<T: std::fmt::Debug>(r: Result<T>, code: &str) {
    assert_eq!(r.unwrap_err().code, code);
}
pub(crate) struct Harness {
    pub temp: tempfile::TempDir,
    pub store: Store,
    clock: u32,
}
impl Harness {
    pub fn empty() -> Self {
        let temp = tempfile::tempdir().unwrap();
        // This is the session state root, which the CLI creates owner-only
        // (`private_dir`) and the desktop's receipt store refuses otherwise.
        // A temporary directory follows the umask instead: 0755 under the
        // hosted runners' 022 (zerus-63rru.188).
        #[cfg(unix)]
        std::fs::set_permissions(temp.path(), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        crate::platform::private_dir(&temp.path().join("teams")).unwrap();
        std::fs::write(temp.path().join("teams/preview"), "on").unwrap();
        let store = Store::open(temp.path()).unwrap();
        Self {
            temp,
            store,
            clock: 0,
        }
    }
    pub fn new() -> Self {
        let mut h = Self::empty();
        // These lifecycle tests isolate the store from pipeline progression.
        // Pipeline tests construct a real staged bundle explicitly.
        let mut create = create();
        if let Operation::Create { bundle, .. } = &mut create {
            bundle.pipelines[0].stages.clear();
        }
        h.run(&person(), create).unwrap();
        h
    }
    pub fn run(&mut self, actor: &Actor, op: Operation) -> Result<Reply> {
        self.run_for(TEAM, actor, op)
    }
    pub fn run_for(&mut self, team: &str, actor: &Actor, op: Operation) -> Result<Reply> {
        let rev = self.store.team(team)?.map_or(0, |t| t.revision);
        let mut r = request(rev, op);
        r.team = team.into();
        apply_fixture(&mut self.store, actor, &r, 1234)
    }
    pub fn bind(&mut self, id: &str) -> Actor {
        let m = fixture().members.into_iter().find(|m| m.id == id).unwrap();
        let session = m.session.unwrap();
        self.run(
            &service(),
            Operation::SetMemberSession {
                member: id.into(),
                session: session.clone(),
                account: m.account,
                worktree: m.worktree,
                ownership_nonce: m.ownership_nonce,
            },
        )
        .unwrap();
        Actor::Session {
            person: "local".into(),
            team: TEAM.into(),
            member: id.into(),
            session,
        }
    }
    pub fn enqueue(&mut self, actor: &Actor, n: u32) -> u32 {
        self.run(
            actor,
            Operation::AddTask {
                task: task(n),
                role: None,
                pipeline: None,
            },
        )
        .unwrap()
        .node
        .unwrap()
    }
    pub fn add(&mut self, actor: &Actor, n: u32) -> u32 {
        let node = self.enqueue(actor, n);
        self.observe(node, TaskStage::Todo);
        node
    }
    pub fn node(&self, n: u32) -> Node {
        self.store.node(TEAM, n).unwrap().unwrap()
    }
    /// The next observation of the task; its attempt stays the one the store
    /// holds, as the task document keeps reporting it.
    pub fn observation(&mut self, node: u32, stage: TaskStage) -> TaskObservation {
        self.clock += 1;
        let previous = self.store.task_observation(TEAM, node).unwrap();
        let (attempt, attempt_id) = previous
            .as_ref()
            .map_or((0, String::new()), |o| (o.attempt, o.attempt_id.clone()));
        TaskObservation {
            key: KeyState::Ours,
            revision: format!("token-{}", self.clock),
            previous_revision: previous.map_or(String::new(), |o| o.revision),
            stage,
            cancelled: false,
            current_cancelled: false,
            rank: format!("a{node:08}"),
            title: format!("Task {node}"),
            attempt,
            attempt_id,
            attempt_conflict: false,
        }
    }
    pub fn observe(&mut self, node: u32, stage: TaskStage) {
        let o = self.observation(node, stage);
        self.run(
            &service(),
            Operation::ObserveTask {
                node,
                observation: o,
            },
        )
        .unwrap();
    }
    pub fn cancel_fact(&mut self, node: u32) {
        let mut o = self.observation(node, TaskStage::Todo);
        o.cancelled = true;
        self.run(
            &service(),
            Operation::ObserveTask {
                node,
                observation: o,
            },
        )
        .unwrap();
    }
    pub fn accept_decision(&self, node: u32) -> u64 {
        self.store
            .decisions(TEAM, 200, None)
            .unwrap()
            .rows
            .into_iter()
            .rev()
            .find(|d| d.node == node && d.kind == "accept")
            .map_or(0, |d| d.seq)
    }
    pub fn intent(&mut self, node: u32, member: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.run(
            &service(),
            Operation::HandoutIntent {
                node,
                member: member.into(),
                effect_request_id: id.clone(),
            },
        )
        .unwrap();
        id
    }
    pub fn outcome(&mut self, id: &str, outcome: HandoutOutcome) {
        self.run(
            &service(),
            Operation::HandoutOutcome {
                effect_request_id: id.into(),
                outcome,
            },
        )
        .unwrap();
    }
    pub fn submit(&mut self, node: u32, member: &str) -> String {
        let id = self.intent(node, member);
        self.outcome(
            &id,
            HandoutOutcome::Submitted {
                receipt: "receipt-example".into(),
            },
        );
        self.observe(node, TaskStage::InProgress);
        id
    }
    pub fn report(&mut self, node: u32, member: &str) {
        self.observe(node, TaskStage::InReview);
        let generation = self.store.generation(TEAM, node).unwrap().unwrap();
        self.run(
            &service(),
            Operation::RecordReport {
                node,
                generation,
                member: member.into(),
                text: "Implemented and checked".into(),
                commit: COMMIT.into(),
                notes: Vec::new(),
            },
        )
        .unwrap();
    }
    pub fn reported(&mut self) -> (u32, Actor) {
        let lead = self.bind("lead");
        self.bind("m1");
        let node = self.add(&person(), 1);
        self.submit(node, "m1");
        self.report(node, "m1");
        (node, lead)
    }
    pub fn reopen(&mut self, node: u32, member: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.run(
            &service(),
            Operation::ReopenIntent {
                node,
                member: member.into(),
                effect_request_id: id.clone(),
            },
        )
        .unwrap();
        id
    }
    pub fn reopen_outcome(&mut self, id: &str, outcome: HandoutOutcome) {
        self.run(
            &service(),
            Operation::ReopenOutcome {
                effect_request_id: id.into(),
                outcome,
            },
        )
        .unwrap();
    }
    /// The cancel answer a retired stage gets from the cancel command when
    /// its bound task has nothing running: released, recorded as the service
    /// records it. The stage then ends, is archived, and its key waits for
    /// the clear in the released-stage frontier.
    pub fn release_retired(&mut self, stage: u32) {
        let row = self
            .store
            .pending_cancels(TEAM, 200, 0)
            .unwrap()
            .into_iter()
            .find(|r| r.node == stage)
            .expect("the retired stage keeps one pending cancel");
        self.run(
            &service(),
            Operation::RecordCancel {
                node: stage,
                decision: row.decision,
                state: crate::teams::ops::CancelState::Released,
                reason: String::new(),
                attempt: row.attempt,
                attempt_id: row.attempt_id,
            },
        )
        .unwrap();
    }
    pub fn settings(&mut self, change: impl FnOnce(&mut Settings)) {
        let mut s = self.store.team(TEAM).unwrap().unwrap().settings;
        change(&mut s);
        self.run(&person(), Operation::ChangeSettings { settings: s })
            .unwrap();
    }
}

/// Product writers create all rows; only native admission is modeled here.
/// No production actor/origin value supplies this permit.
pub(crate) fn apply_fixture(
    store: &mut Store,
    actor: &Actor,
    request: &Request,
    now: i64,
) -> Result<Reply> {
    if matches!(actor, Actor::Person { .. })
        && !matches!(request.operation, Operation::DecideCard { .. })
    {
        return super::apply_with_native(
            store,
            actor,
            request,
            now,
            Some(crate::desktop::teams::person_actions::kernel::Permit::fixture(actor, request)),
        );
    }
    let admission = if matches!(
        actor,
        Actor::Person {
            origin: PersonOrigin::DesktopUi,
            ..
        }
    ) && matches!(request.operation, Operation::DecideCard { .. })
    {
        Some(crate::desktop::teams::person_gesture::Admission::fixture(
            store, request,
        ))
    } else {
        None
    };
    super::apply_with_gesture(store, actor, request, now, admission)
}
mod names74;

mod editing;

mod cutover_delivery;
