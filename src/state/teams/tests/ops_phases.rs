use super::*;
use super::support::*;

#[test]
fn lanes_are_visual_only_and_bounded() {
    // U27: a task's lane is drawn by the desktop and never changes hand-out.
    let mut team = running_team();
    team.tasks[0].lane = Some(super::super::model::Lane { n: 1, label: "Checkpoint 1".into() });
    team.tasks[1].lane = Some(super::super::model::Lane { n: 2, label: "Checkpoint 2".into() });
    let with_lanes = plan::ready(&team);
    let mut stripped = team.clone();
    for task in &mut stripped.tasks {
        task.lane = None;
    }
    assert_eq!(with_lanes, plan::ready(&stripped));
    // The plan keeps them, with the label trimmed.
    let (mut planned, _) = create(
        "t2",
        &json!({"name":"T","lead":{"id":"lead","agent":"claude","session":{"name":"claude/x/lead","run_id":"r0"}}}),
    )
    .unwrap();
    let mut events = Vec::new();
    let value = apply(
        &mut planned,
        "plan",
        &json!({"members":[{"id":"dev","role":"Backend","agent":"codex","session":{"name":"codex/x/dev","run_id":"r1"}}],
            "tasks":[
                {"id":1,"title":"A","assignee":"dev","lane":{"n":1,"label":" Checkpoint 1 "}},
                {"id":2,"title":"B","assignee":"dev","lane":{"n":2}},
                {"id":3,"title":"C","assignee":"dev"}]}),
        &Actor::Human,
        &BTreeSet::new(),
        &mut events,
    )
    .unwrap();
    assert!(value["validation"]["errors"].as_array().unwrap().is_empty(), "{value}");
    let lane = planned.task(1).unwrap().lane.as_ref().unwrap();
    assert_eq!((lane.n, lane.label.as_str()), (1, "Checkpoint 1"));
    assert_eq!(planned.task(2).unwrap().lane.as_ref().unwrap().n, 2);
    assert!(planned.task(3).unwrap().lane.is_none());
    // A label is bounded and a lane number is 1 to 99.
    let mut fresh = create(
        "t3",
        &json!({"name":"T","lead":{"id":"lead","agent":"claude","session":{"name":"claude/x/lead","run_id":"r0"}}}),
    )
    .unwrap()
    .0;
    let long = "x".repeat(41);
    let error = apply(
        &mut fresh,
        "plan",
        &json!({"tasks":[{"id":1,"title":"A","lane":{"n":1,"label":long}}]}),
        &Actor::Human,
        &BTreeSet::new(),
        &mut events,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("A lane label is too long"), "{error}");
    let error = apply(
        &mut fresh,
        "plan",
        &json!({"tasks":[{"id":1,"title":"A","lane":{"n":0}}]}),
        &Actor::Human,
        &BTreeSet::new(),
        &mut events,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("a lane number is 1 to 99"), "{error}");
    // A draft phase edits its tasks' lanes, and null takes one out.
    let lead_actor = Actor::Member("lead".into());
    extend(&mut planned, &json!({"tasks":[{"id":4,"title":"Later","assignee":"dev"}]}), &lead_actor, &BTreeSet::new(), &mut events).unwrap();
    let id = planned.tasks.iter().map(|task| task.id).max().unwrap();
    let phase = planned.task(id).unwrap().phase;
    apply(
        &mut planned,
        "edit-draft",
        &json!({"phase":phase,"tasks_update":[{"id":id,"lane":{"n":2,"label":"Review"}}]}),
        &lead_actor,
        &BTreeSet::new(),
        &mut events,
    )
    .unwrap();
    let lane = planned.task(id).unwrap().lane.as_ref().unwrap();
    assert_eq!((lane.n, lane.label.as_str()), (2, "Review"));
    apply(&mut planned, "edit-draft", &json!({"phase":phase,"tasks_update":[{"id":id,"lane":null}]}), &lead_actor, &BTreeSet::new(), &mut events)
        .unwrap();
    assert!(planned.task(id).unwrap().lane.is_none());
}

#[test]
fn a_team_plans_in_standard_mode_unless_the_owner_switches() {
    let team = Team::default();
    assert_eq!(team.plan_mode, PlanMode::Standard);
    // U59: an older state without plan_mode reads standard, and a standard
    // team does not store the field.
    let stored: Team = serde_json::from_value(json!({"id":"t","name":"T","lead":"lead","tasks":[]})).unwrap();
    assert_eq!(stored.plan_mode, PlanMode::Standard);
    let text = serde_json::to_string(&Team::default()).unwrap();
    assert!(!text.contains("plan_mode"), "standard is not stored: {text}");
    let free = Team { plan_mode: PlanMode::Free, ..Default::default() };
    assert!(serde_json::to_string(&free).unwrap().contains("\"plan_mode\":\"free\""));
}

#[test]
fn a_free_phase_takes_added_tasks_and_never_closes_itself() {
    let mut team = running_team();
    team.plan_mode = PlanMode::Free;
    let mut events = Vec::new();
    let none = BTreeSet::new();
    let phases_before = team.phases.len();
    let added = extend(&mut team, &json!({"tasks":[{"id":3,"title":"Incoming","assignee":"dev"}]}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((added["phase"].as_u64(), added["tasks"].clone()), (Some(1), json!([3])));
    assert_eq!(team.task(3).unwrap().status, TaskStatus::Pending);
    assert_eq!(team.phases.len(), phases_before, "no new phase in free mode");
    // U59: the phase build is an ordinary lead task added on request.
    let built = apply(&mut team, "integrate-build", &json!({}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(built["status"], "build_added", "{built}");
    let build = u32::try_from(built["task"].as_u64().unwrap()).unwrap();
    assert!(team.task(build).unwrap().phase_build);
    for id in [1, 2, 3] {
        apply(&mut team, "cancel", &json!({"task":id}), &Actor::Human, &none, &mut events).unwrap();
    }
    assert!(!all_decided(&team), "an empty queue in free mode is idle, not done");
    assert!(!events.iter().any(|event| event.kind == "phase_done"));
}

#[test]
fn a_switch_to_free_needs_the_owners_allow_bound_to_the_requests_hash() {
    let mut team = running_team();
    let lead = Actor::Member("lead".into());
    let mut events = Vec::new();
    let none = BTreeSet::new();
    let proposed = propose(
        &mut team,
        &json!({"kind":"switch-plan-mode","mode":"free","reason":"a stream of small requests"}),
        &lead,
        &mut events,
    )
    .unwrap();
    let id = proposed["proposal"].as_str().unwrap().to_owned();
    let hash = proposed["hash"].as_str().unwrap().to_owned();
    assert!(!hash.is_empty());
    let error = decide_proposal(&mut team, &json!({"proposal":id,"accept":true}), &Actor::Human, &none, &mut events)
        .unwrap_err()
        .to_string();
    assert!(error.contains("hash"), "{error}");
    let error = decide_proposal(
        &mut team,
        &json!({"proposal":id,"accept":true,"expected_hash":"bad"}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("hash"), "{error}");
    // Deny keeps standard and its reason reaches the lead.
    let denied = decide_proposal(
        &mut team,
        &json!({"proposal":id,"accept":false,"reason":"keep releases","expected_hash":hash}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap();
    assert_eq!(denied["status"], "rejected");
    assert_eq!(team.plan_mode, PlanMode::Standard);
    let event = events.iter().find(|event| event.kind == "plan_mode_denied").unwrap();
    assert_eq!(event.payload["reason"], "keep releases");
    // Allow switches; a queued standard phase stays queued.
    let mut queued = running_team();
    queued.phases.push(Phase { n: 2, state: PhaseState::Queued, ..Default::default() });
    let proposed = propose(
        &mut queued,
        &json!({"kind":"switch-plan-mode","mode":"free","reason":"stream"}),
        &lead,
        &mut events,
    )
    .unwrap();
    let accepted = decide_proposal(
        &mut queued,
        &json!({"proposal":proposed["proposal"],"accept":true,"expected_hash":proposed["hash"]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap();
    assert_eq!(accepted["status"], "accepted");
    assert_eq!(queued.plan_mode, PlanMode::Free);
    assert_eq!(queued.phases[0].state, PhaseState::Queued, "a queued phase is left as it is");
}

/// U57: a proposal stores the exact change and a content hash; the owner's
/// Allow is bound to them.
#[test]
fn a_proposal_stores_the_exact_change_and_its_content_hash() {
    let mut team = running_team();
    let mut events = Vec::new();
    let lead = Actor::Member("lead".into());
    // #740: a proposal is checked when it is made, so phase 2 is a real draft.
    apply(&mut team, "extend", &json!({"phase_title":"Next","tasks":[{"id":3,"title":"Later","assignee":"dev"}]}), &lead, &BTreeSet::new(), &mut events)
        .unwrap();
    let cases = [
        (json!({"kind":"add-member","reason":"needs two",
            "members":[{"id":"x","role":"Ops","agent":"claude"},{"id":"y","role":"Ops","agent":"claude"}]}), "add-member"),
        (json!({"kind":"cancel-tasks","reason":"drop","tasks":[1]}), "cancel-tasks"),
        (json!({"kind":"discard-phase","reason":"empty","phase":2}), "discard-phase"),
    ];
    for (input, kind) in cases {
        let receipt = propose(&mut team, &input, &lead, &mut events).unwrap();
        let hash = receipt["hash"].as_str().unwrap().to_owned();
        assert!(!hash.is_empty(), "{kind}");
        let proposal = team.proposals.last().unwrap();
        assert_eq!(proposal["hash"], hash);
        assert_eq!(proposal["kind"], kind);
        assert!(proposal["change"].is_object(), "{kind}: {}", proposal["change"]);
    }
    // The multi-member change keeps every member exactly as proposed.
    assert_eq!(team.proposals[0]["change"]["members"].as_array().unwrap().len(), 2);
    assert_eq!(team.proposals[1]["change"]["tasks"], json!([1]));
    assert_eq!(team.proposals[2]["change"]["phase"], 2);
    // An unhashed or changed proposal is refused by the decision.
    let id = team.proposals[0]["id"].as_str().unwrap().to_owned();
    let error = apply(&mut team, "decide-proposal", &json!({"proposal":id,"accept":true,"expected_hash":"bad"}), &Actor::Human, &BTreeSet::new(), &mut events)
        .unwrap_err()
        .to_string();
    assert!(error.contains("changed"), "{error}");
    // A change that has gone stale is refused when it applies, never re-applied.
    let hash = team.proposals[1]["hash"].as_str().unwrap().to_owned();
    let id = team.proposals[1]["id"].as_str().unwrap().to_owned();
    apply(&mut team, "cancel", &json!({"task":1}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
    let error = apply(&mut team, "decide-proposal", &json!({"proposal":id,"accept":true,"expected_hash":hash}), &Actor::Human, &BTreeSet::new(), &mut events)
        .unwrap_err()
        .to_string();
    assert!(error.contains("already decided"), "a stale change is refused: {error}");
}

#[test]
fn the_owner_closes_a_free_phase_only_when_its_work_is_done() {
    let mut team = running_team();
    team.plan_mode = PlanMode::Free;
    let mut events = Vec::new();
    let none = BTreeSet::new();
    let error = apply(&mut team, "close-phase", &json!({"reason":"done"}), &Actor::Human, &none, &mut events)
        .unwrap_err()
        .to_string();
    assert!(error.contains("#1") && error.contains("#2"), "the running tasks are named: {error}");
    for id in [1, 2] {
        apply(&mut team, "cancel", &json!({"task":id}), &Actor::Human, &none, &mut events).unwrap();
    }
    // U59: the build is not self-asserted. With work accepted since the last
    // build, the close adds an ordinary lead build task and waits for it.
    team.free_accepted_since_build = 1;
    let pending = apply(&mut team, "close-phase", &json!({"integrate":true,"reason":"done"}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(pending["status"], "close_pending", "{pending}");
    let build = u32::try_from(pending["build"].as_u64().unwrap()).unwrap();
    let task = team.task(build).unwrap();
    assert!(task.phase_build && task.assignee == "lead", "{task:?}");
    assert_eq!(team.free_close_pending, Some(1));
    assert_eq!(team.plan_mode, PlanMode::Free);
    // A cancelled build fails the pending close and reaches the lead; the
    // phase stays open.
    apply(&mut team, "cancel", &json!({"task":build}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(team.plan_mode, PlanMode::Free);
    assert_eq!(team.free_close_pending, None);
    assert!(events.iter().any(|event| event.kind == "free_phase_build_failed"));
    // The next close adds a fresh build; accepting it closes the phase.
    let pending = apply(&mut team, "close-phase", &json!({"integrate":true}), &Actor::Human, &none, &mut events).unwrap();
    let build = u32::try_from(pending["build"].as_u64().unwrap()).unwrap();
    let accepted = apply(&mut team, "accept", &json!({"task":build}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(accepted["status"], "accepted");
    assert_eq!(team.plan_mode, PlanMode::Standard, "the accepted build closes the pending phase");
    assert!(team.last_build_at.is_some());
    assert_eq!(team.free_accepted_since_build, 0);
    assert!(events.iter().any(|event| event.kind == "free_phase_closed"));
    // With nothing accepted since the last build the close is immediate and
    // creates no task.
    team.plan_mode = PlanMode::Free;
    let tasks = team.tasks.len();
    let closed = apply(&mut team, "close-phase", &json!({"integrate":true}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((closed["status"].as_str(), team.plan_mode), (Some("closed"), PlanMode::Standard));
    assert_eq!(team.tasks.len(), tasks, "the immediate close creates no build task");
    assert!(team.phases.is_empty());
}

/// U59: the lead proposes a mode at the start; the owner's approval chooses
/// one of the two, and a lead's approval or an older client keeps standard.
#[test]
fn the_owner_chooses_the_plan_mode_when_approving_the_plan() {
    let (mut team, _) = create("t", &json!({"name":"T","lead":{"id":"lead","agent":"claude"}})).unwrap();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    let plan_input = json!({"members":[{"id":"dev","role":"Backend","agent":"codex"}],
        "tasks":[{"id":1,"title":"A","assignee":"dev"}],
        "plan_mode":"free","plan_mode_reason":"a stream of small requests"});
    plan(&mut team, &plan_input, &Actor::Member("lead".into()), &mut events).unwrap();
    assert_eq!(team.plan_mode_offer, Some(PlanMode::Free));
    assert_eq!(team.plan_mode_reason, "a stream of small requests");
    // A lead cannot approve into free.
    let word = json!({"owner_word_checked":{"text":"approve the plan","at":1.0,"event":1}});
    let error = approve(&mut team, &json!({"plan_mode":"free","owner_word_checked":word["owner_word_checked"]}), &Actor::Member("lead".into()), &none, &mut events)
        .unwrap_err()
        .to_string();
    assert!(error.contains("Only you choose the plan mode"), "{error}");
    // The owner approves standard even though free was proposed.
    approve(&mut team, &json!({}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(team.plan_mode, PlanMode::Standard);
    assert_eq!(team.plan_mode_offer, None);
    // Proposed free and the owner approves free.
    let (mut team, _) = create("t", &json!({"name":"T","lead":{"id":"lead","agent":"claude"}})).unwrap();
    plan(&mut team, &plan_input, &Actor::Member("lead".into()), &mut events).unwrap();
    approve(&mut team, &json!({"plan_mode":"free"}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(team.plan_mode, PlanMode::Free);
    // A lead approving on the owner's word without a mode keeps standard.
    let (mut team, _) = create("t", &json!({"name":"T","lead":{"id":"lead","agent":"claude"}})).unwrap();
    plan(&mut team, &plan_input, &Actor::Member("lead".into()), &mut events).unwrap();
    approve(&mut team, &json!({"owner_word_checked":word["owner_word_checked"]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!(team.plan_mode, PlanMode::Standard);
}

#[test]
fn plan_queues_the_roadmap_and_keeps_phase_one() {
    let (mut team, _) = create("t", &json!({"name":"T","lead":{"id":"lead","agent":"claude"}})).unwrap();
    let mut events = Vec::new();
    // A task in a phase the plan does not list is refused.
    let error = plan(&mut team, &json!({"tasks":[{"id":1,"title":"A","assignee":"lead","phase":7}]}), &Actor::Human, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Task #1 is in phase 7, which the plan does not list");
    // The roadmap is listed in order, whatever order it came in.
    plan(
        &mut team,
        &json!({"phases":[{"n":3,"title":"Later"},{"n":2,"title":"Next","barrier":true}],
            "tasks":[
                {"id":1,"title":"A","assignee":"lead","pinned":true},
                {"id":2,"title":"B","assignee":"lead","phase":2},
                {"id":3,"title":"C","assignee":"lead","phase":3}
            ]}),
        &Actor::Human,
        &mut events,
    )
    .unwrap();
    let phases: Vec<(u32, PhaseState, bool, Vec<u32>)> =
        team.phases.iter().map(|phase| (phase.n, phase.state, phase.barrier, phase.tasks.clone())).collect();
    assert_eq!(phases, vec![(2, PhaseState::Draft, true, vec![2]), (3, PhaseState::Draft, false, vec![3])]);
    assert!(team.task(1).unwrap().pinned);
    assert_eq!((team.task(1).unwrap().phase, team.task(2).unwrap().phase, team.task(3).unwrap().phase), (1, 2, 3));
    assert_eq!(events.last().unwrap().payload["phases"], 2);
    // Approving the plan opens phase 1 only; later phases stay Draft.
    approve(&mut team, &json!({}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
    assert_eq!(team.task(1).unwrap().status, TaskStatus::Pending);
    assert_eq!((team.task(2).unwrap().status, team.task(3).unwrap().status), (TaskStatus::Draft, TaskStatus::Draft));
}

#[test]
fn extend_takes_phases_while_planning_too() {
    let (mut planning, _) = create(
        "t",
        &json!({"name":"T","lead":{"id":"lead","agent":"claude"},"members":[{"id":"dev","role":"Backend","agent":"codex"}]}),
    )
    .unwrap();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    // While Planning the new phase joins the roadmap as a Draft.
    let added = extend(
        &mut planning,
        &json!({"phase_title":"Later","tasks":[{"id":1,"title":"A","assignee":"dev"}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap();
    assert_eq!((added["phase"].as_u64(), added["status"].as_str()), (Some(2), Some("added")));
    assert_eq!(planning.phases[0].state, PhaseState::Draft);
    assert_eq!(planning.task(1).unwrap().status, TaskStatus::Draft);
    let error = extend(
        &mut planning,
        &json!({"tasks":[{"id":2,"title":"B","assignee":"dev"}]}),
        &Actor::Member("dev".into()),
        &none,
        &mut events,
    )
    .unwrap_err()
    .to_string();
    assert_eq!(error, "Only the lead or you extend the team");
    // A team that is over takes no new phase; a paused one does.
    let mut team = running_team();
    for status in [TeamStatus::Done, TeamStatus::Stopped, TeamStatus::Archived] {
        team.status = status;
        let error = extend(&mut team, &json!({"tasks":[{"id":3,"title":"A","assignee":"dev"}]}), &Actor::Human, &none, &mut events)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "Only a planning, running or paused team takes a new phase", "{status:?}");
    }
    team.status = TeamStatus::Paused;
    let added = extend(&mut team, &json!({"tasks":[{"id":3,"title":"A","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!(added["status"], "added");
}

#[test]
fn queued_drafts_keep_their_order_and_auto_waits_behind_them() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    let added = extend(
        &mut team,
        &json!({"phase_title":"QA fixes","barrier":true,"tasks":[
            {"id":3,"title":"Fix","assignee":"dev","pinned":true},
            {"id":4,"title":"Retest","assignee":"qa","deps":[{"task":3,"kind":"soft"}]}
        ]}),
        &Actor::Member("lead".into()),
        &none,
        &mut events,
    )
    .unwrap();
    assert_eq!((added["phase"].as_u64(), added["approval"].as_str()), (Some(2), Some("review")));
    assert_eq!(added["tasks"], json!([3, 4]));
    assert_eq!(events.last().unwrap().kind, "phase_added");
    assert_eq!(events.last().unwrap().payload["by"], "Lead");
    assert_eq!(events.last().unwrap().payload["tasks"], 2);
    // A second phase queues behind the first draft instead of being refused.
    let added = extend(&mut team, &json!({"tasks":[{"id":5,"title":"More","assignee":"dev"}]}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(added["phase"], 3);
    assert_eq!(
        team.phases.iter().map(|phase| (phase.n, phase.state)).collect::<Vec<_>>(),
        vec![(2, PhaseState::Draft), (3, PhaseState::Draft)]
    );
    assert_eq!((team.task(3).unwrap().status, team.task(5).unwrap().status), (TaskStatus::Draft, TaskStatus::Draft));
    // With auto, a phase added behind a draft is a draft too.
    team.settings.phase_approval = Some(Approval::Auto);
    let added = extend(&mut team, &json!({"tasks":[{"id":6,"title":"After","assignee":"dev"}]}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((added["phase"].as_u64(), added["approval"].as_str()), (Some(4), Some("auto")));
    assert_eq!(team.phases.last().unwrap().state, PhaseState::Draft);
    assert_eq!(team.task(6).unwrap().status, TaskStatus::Draft);
    // With no draft left, an auto phase starts at once as before.
    team.phases.retain(|phase| phase.state != PhaseState::Draft);
    for task in &mut team.tasks {
        if task.phase > 1 && task.status == TaskStatus::Draft {
            task.status = TaskStatus::Pending;
        }
    }
    let added = extend(&mut team, &json!({"tasks":[{"id":7,"title":"Fresh","assignee":"dev"}]}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((added["phase"].as_u64(), added["approval"].as_str()), (Some(5), Some("auto")));
    assert_eq!(team.task(7).unwrap().status, TaskStatus::Pending);
    assert_eq!(team.phases.last().unwrap().state, PhaseState::Approved);
}

#[test]
fn approve_phase_waits_for_the_earlier_phases_approval() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    for id in [3, 4] {
        extend(&mut team, &json!({"tasks":[{"id":id,"title":"T","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    }
    // F12: phase 1's tasks are still open, and phase 2 opens anyway.
    let approved = apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(approved["status"], "phase_approved");
    // Phase 3 opens while phase 2's tasks are open too.
    let approved = apply(&mut team, "approve-phase", &json!({"phase":3}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((approved["status"].as_str(), approved["tasks"].clone()), (Some("phase_approved"), json!([4])));
    // A draft before the phase still refuses the approval.
    let mut review = running_team();
    review.settings.approval = Approval::Review;
    for id in [3, 4] {
        extend(&mut review, &json!({"tasks":[{"id":id,"title":"T","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    }
    let error = apply(&mut review, "approve-phase", &json!({"phase":3}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
    assert_eq!(error, "Phase 3 opens after the previous phase is approved");
}

/// U56/#645 N5: a draft whose tasks were all moved out is the lead's to
/// discard; an approved or queued phase stays on your word even when empty.
#[test]
fn the_lead_discards_an_empty_draft_only() {
    let none = BTreeSet::new();
    let lead = Actor::Member("lead".into());
    let mut events = Vec::new();
    let mut team = running_team();
    extend(&mut team, &json!({"tasks":[{"id":3,"title":"T","assignee":"dev"}]}), &lead, &none, &mut events).unwrap();
    apply(&mut team, "move-task", &json!({"task":3,"phase":1}), &lead, &none, &mut events).unwrap();
    assert!(team.phases.iter().any(|phase| phase.n == 2 && phase.state.is_draft() && phase.tasks.is_empty()));
    let discarded = apply(&mut team, "discard-phase", &json!({"phase":2}), &lead, &none, &mut events).unwrap();
    assert_eq!((discarded["status"].as_str(), discarded["tasks"].clone()), (Some("phase_discarded"), json!([])));
    assert!(team.task(3).is_some(), "the moved task stays");
    // An approved and a queued phase left empty stay on your word: the
    // lead alone is refused, the lead on your word and you discard them.
    let mut accepted = running_team();
    for id in [3, 4] {
        extend(&mut accepted, &json!({"tasks":[{"id":id,"title":"T","assignee":"dev"}]}), &lead, &none, &mut events).unwrap();
    }
    apply(&mut accepted, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    apply(&mut accepted, "approve-phase", &json!({"phase":3}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(accepted.phases.iter().find(|phase| phase.n == 3).unwrap().state, PhaseState::Queued);
    for id in [3, 4] {
        apply(&mut accepted, "move-task", &json!({"task":id,"phase":1}), &lead, &none, &mut events).unwrap();
    }
    for phase in [2, 3] {
        let error = apply(&mut accepted, "discard-phase", &json!({"phase":phase}), &lead, &none, &mut events).unwrap_err().to_string();
        assert!(error.contains("Only you discard an accepted phase"), "{phase}: {error}");
    }
    let word = json!({"phase":3,"owner_word_checked":{"text":"discard phase 3","at":1.0,"event":5}});
    apply(&mut accepted, "discard-phase", &word, &lead, &none, &mut events).unwrap();
    apply(&mut accepted, "discard-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert!(accepted.phases.iter().all(|phase| phase.n != 2 && phase.n != 3), "{:?}", accepted.phases);
    // A phase with a task in it still needs your word.
    let mut kept = running_team();
    extend(&mut kept, &json!({"tasks":[{"id":3,"title":"T","assignee":"dev"}]}), &lead, &none, &mut events).unwrap();
    apply(&mut kept, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    let error = apply(&mut kept, "discard-phase", &json!({"phase":2}), &lead, &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("Only you discard an accepted phase"), "{error}");
}

#[test]
fn an_accepted_phase_is_discarded_until_its_work_starts() {
    // Owner, 2026-10-28 (U39): every state but done can be discarded; an
    // accepted phase only by you or on your word, and only before its
    // work starts.
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    for id in [3, 4] {
        extend(&mut team, &json!({"tasks":[{"id":id,"title":"T","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    }
    apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    apply(&mut team, "approve-phase", &json!({"phase":3}), &Actor::Human, &none, &mut events).unwrap();
    assert!(team.phases.iter().all(|phase| phase.state.is_accepted()));
    // The lead alone cannot drop an accepted phase.
    let error = apply(&mut team, "discard-phase", &json!({"phase":3}), &Actor::Member("lead".into()), &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("Only you discard an accepted phase"), "{error}");
    // You can, while nothing of it has started.
    let discarded = apply(&mut team, "discard-phase", &json!({"phase":3}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(discarded["tasks"], json!([4]));
    assert!(team.task(4).is_none());
    assert_eq!(events.last().unwrap().payload["state"], "queued");
    // U57: running without a delivered attempt is still "no work done", so
    // it may be discarded; a delivered attempt blocks it.
    team.task_mut(3).unwrap().status = TaskStatus::InProgress;
    let discarded = apply(&mut team, "discard-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(discarded["tasks"], json!([3]));
    // A delivered attempt keeps the phase; cancel it first.
    let mut started = running_team();
    extend(&mut started, &json!({"tasks":[{"id":3,"title":"T","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    apply(&mut started, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    {
        let task = started.task_mut(3).unwrap();
        task.status = TaskStatus::InProgress;
        task.attempts.push(Attempt { n: 1, id: "a1".into(), member: "dev".into(), state: AttemptState::Delivered, sent_at: 1.0, delivered_at: Some(1.0), ..Default::default() });
    }
    let error = apply(&mut started, "discard-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("has delivered work (#3)"), "{error}");
    // A task cancelled after its delivered attempt does not block.
    apply(&mut started, "cancel", &json!({"task":3}), &Actor::Human, &none, &mut events).unwrap();
    apply(&mut started, "discard-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    // A phase with accepted work stays.
    let mut done = running_team();
    extend(&mut done, &json!({"tasks":[{"id":3,"title":"T","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    apply(&mut done, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    done.task_mut(3).unwrap().status = TaskStatus::Accepted;
    let error = apply(&mut done, "discard-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("has accepted work"), "{error}");
}

/// U42 (#587): an accepted phase with no started work returns to a draft:
/// by you, or by the lead on your word; its tasks go back to Draft and it
/// waits for approval again, numbered after the accepted phases.
#[test]
fn an_accepted_phase_returns_to_a_draft_until_its_work_starts() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    for id in [3, 4] {
        extend(&mut team, &json!({"tasks":[{"id":id,"title":"T","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    }
    apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    apply(&mut team, "approve-phase", &json!({"phase":3}), &Actor::Human, &none, &mut events).unwrap();
    // Both wait behind phase 1, which still runs.
    assert_eq!(team.phases.iter().map(|phase| phase.state).collect::<Vec<_>>(), vec![PhaseState::Queued, PhaseState::Queued]);
    // The lead alone cannot; a draft is not returned again.
    let error = apply(&mut team, "return-to-draft", &json!({"phase":2}), &Actor::Member("lead".into()), &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("Only you return a phase to a draft"), "{error}");
    // Auto-accept would take it again at once: refused while it is on.
    let mut auto = team.clone();
    auto.settings.auto_accept = true;
    let error = apply(&mut auto, "return-to-draft", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("Auto-accept is on"), "{error}");
    // Started work stays.
    let mut started = team.clone();
    started.task_mut(3).unwrap().status = TaskStatus::InProgress;
    let error = apply(&mut started, "return-to-draft", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("has started work (#3)"), "{error}");
    // You return the queued phase 2: it becomes the draft after phase 3,
    // its task Draft again, with a feed event.
    events.clear();
    let returned = apply(&mut team, "return-to-draft", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((returned["status"].as_str(), returned["from_phase"].as_u64(), returned["phase"].as_u64()), (Some("phase_returned_to_draft"), Some(2), Some(4)));
    let draft = team.phases.iter().find(|phase| phase.state == PhaseState::Draft).unwrap();
    assert_eq!((draft.n, draft.decided_at, draft.decided_by.as_str()), (4, None, ""));
    let ids = draft.tasks.clone();
    assert!(ids.iter().all(|id| team.task(*id).unwrap().status == TaskStatus::Draft), "{ids:?}");
    let event = events.iter().find(|event| event.kind == "phase_returned_to_draft").unwrap();
    assert_eq!((event.payload["by"].as_str(), event.payload["phase"].as_u64(), event.payload["from"].as_str()), (Some("You"), Some(2), Some("queued")));
    // It waits for your approval again, as any draft.
    let approved = apply(&mut team, "approve-phase", &json!({"phase":4}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(approved["status"], "phase_approved");
    // The lead on your word (the CLI's checked sentence) returns a queued one.
    let word = json!({"phase":4,"owner_word_checked":{"text":"return phase 4 to draft","at":1.0,"event":9},
        "owner_word_event":9,"owner_word_change":"the return of phase 4 to a draft"});
    let returned = apply(&mut team, "return-to-draft", &word, &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!(returned["status"], "phase_returned_to_draft");
    assert_eq!(events.last().map(|event| event.payload["owner_word"]["text"].clone()), Some(json!("return phase 4 to draft")));
    // A draft does not return again.
    let n = team.phases.iter().find(|phase| phase.state == PhaseState::Draft).unwrap().n;
    let error = apply(&mut team, "return-to-draft", &json!({"phase":n}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("not approved or queued"), "{error}");
}

/// U42 (#587): a plan edit keeps the phases it already had: their order,
/// proposer and time; a new phase goes after them.
#[test]
fn a_plan_edit_keeps_the_phases_it_already_had() {
    let (mut team, _) = create("t", &json!({"name":"Checkout","lead":{"id":"lead","agent":"claude"}})).unwrap();
    let mut events = Vec::new();
    let input = |phases: Value, tasks: Value| json!({"members":[{"id":"dev","role":"Backend","agent":"codex"}],"phases":phases,"tasks":tasks});
    let tasks = json!([{"id":1,"title":"A","assignee":"dev"},{"id":2,"title":"B","assignee":"dev","phase":2},{"id":3,"title":"C","assignee":"dev","phase":3}]);
    plan(&mut team, &input(json!([{"n":2,"title":"Two"},{"n":3,"title":"Three"}]), tasks.clone()), &Actor::Member("lead".into()), &mut events).unwrap();
    // The owner moved phase 3 before phase 2; both were proposed earlier.
    for phase in &mut team.phases {
        phase.added_at = 100.0 + phase.n as f64;
        phase.order = if phase.n == 3 { 2 } else { 3 };
    }
    let mut more = tasks.as_array().unwrap().clone();
    more.push(json!({"id":4,"title":"D","assignee":"dev","phase":4}));
    plan(&mut team, &input(json!([{"n":2,"title":"Two again"},{"n":3,"title":"Three"},{"n":4,"title":"Four"}]), json!(more)), &Actor::Human, &mut events).unwrap();
    let seen: Vec<(u32, u32, &str, f64)> = team.phases.iter().map(|phase| (phase.n, phase.order(), phase.added_by.as_str(), phase.added_at)).collect();
    assert_eq!(seen[0], (3, 2, "Lead", 103.0));
    assert_eq!(seen[1], (2, 3, "Lead", 102.0));
    assert_eq!((seen[2].0, seen[2].1, seen[2].2), (4, 4, "You"));
    assert!(seen[2].3 > 1000.0);
    assert_eq!(team.phases[1].title, "Two again");
}

#[test]
fn discarding_one_draft_keeps_the_later_ones() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    for id in [3, 4] {
        extend(&mut team, &json!({"tasks":[{"id":id,"title":"T","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    }
    let discarded = apply(&mut team, "discard-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(discarded["tasks"], json!([3]));
    // F285: the later draft closes the gap and takes the discarded number;
    // its task follows the approved tasks.
    assert_eq!(team.tasks.iter().map(|task| task.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(team.phases.iter().map(|phase| (phase.n, phase.state)).collect::<Vec<_>>(), vec![(2, PhaseState::Draft)]);
    assert_eq!((team.task(3).unwrap().phase, team.task(3).unwrap().title.as_str()), (2, "T"));
    // The next draft opens once phase 1 ended; the discarded phase is gone.
    for id in [1, 2] {
        apply(&mut team, "cancel", &json!({"task":id}), &Actor::Human, &none, &mut events).unwrap();
    }
    let approved = apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((approved["status"].as_str(), approved["tasks"].clone()), (Some("phase_approved"), json!([3])));
    // The numbers are live, not historical: the next phase is 3, its task #4.
    let added = extend(&mut team, &json!({"tasks":[{"id":4,"title":"New","assignee":"dev"}]}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(added["phase"], 3);
}

#[test]
fn an_old_team_with_one_draft_keeps_working() {
    // A team stored by the previous build: one Draft phase, its tasks Draft.
    let old = json!({
        "id":"t","name":"T","lead":"lead","status":"running",
        "settings":{"approval":"auto"},
        "members":[{"id":"lead","role":"Lead","agent":"claude","session":{"name":"claude/x/lead","run_id":"r0"}},
            {"id":"dev","role":"Backend","agent":"codex","session":{"name":"codex/x/dev","run_id":"r1"}}],
        "tasks":[
            {"id":1,"title":"API","assignee":"dev","status":"accepted"},
            {"id":2,"title":"Fix","assignee":"dev","status":"draft","phase":2}],
        "phases":[{"n":2,"title":"QA fixes","approval":"review","state":"draft","tasks":[2]}]
    });
    let mut team: Team = serde_json::from_value(old).unwrap();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    // The one draft approves as before, once the phase before it ended.
    let approved = apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((approved["status"].as_str(), approved["tasks"].clone()), (Some("phase_approved"), json!([2])));
    assert_eq!(team.task(2).unwrap().status, TaskStatus::Pending);
    // Extend still appends after it; nothing needed a new field.
    let added = extend(&mut team, &json!({"tasks":[{"id":3,"title":"More","assignee":"dev"}]}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(added["phase"], 3);
}

#[test]
fn discarding_a_phase_removes_its_tasks_and_frees_its_numbers() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    extend(
        &mut team,
        &json!({"tasks":[{"id":3,"title":"Fix","assignee":"dev"},{"id":4,"title":"Test","assignee":"qa"}]}),
        &Actor::Member("lead".into()),
        &none,
        &mut events,
    )
    .unwrap();
    let discarded = apply(&mut team, "discard-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((discarded["status"].as_str(), discarded["phase"].as_u64()), (Some("phase_discarded"), Some(2)));
    assert_eq!(discarded["tasks"], json!([3, 4]));
    assert_eq!(team.tasks.iter().map(|task| task.id).collect::<Vec<_>>(), vec![1, 2]);
    // The record is gone and the discarded event keeps the titles (F285).
    assert!(team.phases.is_empty());
    let event = events.iter().rev().find(|event| event.kind == "phase_discarded").unwrap();
    assert_eq!(event.payload["tasks"][0]["title"], "Fix");
    assert_eq!(event.payload["tasks"][1]["title"], "Test");
    // The numbers are live again: phase 2 and task #3 come back.
    let added = extend(
        &mut team,
        &json!({"tasks":[{"id":3,"title":"Again","assignee":"dev"}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap();
    assert_eq!((added["phase"].as_u64(), added["tasks"].clone()), (Some(2), json!([3])));
    assert_eq!(team.phases[0].state, PhaseState::Draft);
    assert_eq!(team.phases[0].tasks, vec![3]);
}

#[test]
fn later_drafts_renumber_and_their_deps_follow() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    extend(&mut team, &json!({"tasks":[{"id":3,"title":"A","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    extend(&mut team, &json!({"tasks":[{"id":4,"title":"B","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    extend(
        &mut team,
        &json!({"tasks":[{"id":5,"title":"C","assignee":"dev","deps":[{"task":4},{"task":2}]}]}),
        &Actor::Member("lead".into()),
        &none,
        &mut events,
    )
    .unwrap();
    // Discard the first draft: phases 3 and 4 become 2 and 3, their tasks 4
    // and 5 become 3 and 4, and the deps follow (the approved #2 stays).
    let discarded = apply(&mut team, "discard-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(discarded["tasks"], json!([3]));
    assert_eq!(team.phases.iter().map(|phase| (phase.n, phase.tasks.clone())).collect::<Vec<_>>(), vec![(2, vec![3]), (3, vec![4])]);
    assert_eq!(team.tasks.iter().map(|task| task.id).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
    let second = team.task(3).unwrap();
    assert_eq!((second.title.as_str(), second.phase), ("B", 2));
    let third = team.task(4).unwrap();
    assert_eq!((third.title.as_str(), third.phase), ("C", 3));
    assert_eq!(third.deps.iter().map(|dep| dep.task).collect::<Vec<_>>(), vec![3, 2]);
    // The renumbering event records old -> new (F285).
    let renumbered = events.iter().rev().find(|event| event.kind == "phase_renumbered").unwrap();
    assert_eq!(renumbered.payload["phases"], json!([{"from":3,"to":2},{"from":4,"to":3}]));
    assert_eq!(renumbered.payload["tasks"], json!([{"from":4,"to":3},{"from":5,"to":4}]));
}

#[test]
fn an_old_team_with_discarded_drafts_is_compacted_once() {
    // A team stored before F285: a Discarded record whose tasks are gone,
    // and a later draft whose number and task id follow it.
    let old = json!({
        "id":"t","name":"T","lead":"lead","status":"running",
        "settings":{"approval":"review"},
        "members":[{"id":"lead","role":"Lead","agent":"claude","session":{"name":"claude/x/lead","run_id":"r0"}},
            {"id":"dev","role":"Backend","agent":"codex","session":{"name":"codex/x/dev","run_id":"r1"}}],
        "tasks":[
            {"id":1,"title":"API","assignee":"dev","status":"accepted"},
            {"id":2,"title":"UI","assignee":"dev","status":"accepted"},
            {"id":5,"title":"Next","assignee":"dev","status":"draft","phase":4}],
        "phases":[
            {"n":2,"state":"discarded","tasks":[3]},
            {"n":4,"state":"draft","tasks":[5]}]
    });
    let mut team: Team = serde_json::from_value(old).unwrap();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    // The next change compacts: the dead record leaves, the draft becomes
    // phase 2 and its task follows the approved tasks.
    let added = apply(&mut team, "extend", &json!({"tasks":[{"id":4,"title":"More","assignee":"dev"}]}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((added["phase"].as_u64(), added["tasks"].clone()), (Some(3), json!([4])));
    assert_eq!(team.phases.iter().map(|phase| (phase.n, phase.tasks.clone())).collect::<Vec<_>>(), vec![(2, vec![3]), (3, vec![4])]);
    // The approved work is untouched.
    assert_eq!((team.task(1).unwrap().id, team.task(1).unwrap().title.as_str()), (1, "API"));
    assert_eq!((team.task(2).unwrap().id, team.task(2).unwrap().title.as_str()), (2, "UI"));
    let renumbered = events.iter().find(|event| event.kind == "phase_renumbered").unwrap();
    assert_eq!(renumbered.payload["phases"], json!([{"from":4,"to":2}]));
    assert_eq!(renumbered.payload["tasks"], json!([{"from":5,"to":3}]));
    // Compacted once: a later change finds no dead record.
    let before = events.iter().filter(|event| event.kind == "phase_renumbered").count();
    apply(&mut team, "pause", &json!({}), &Actor::Human, &none, &mut events).unwrap();
    let after = events.iter().filter(|event| event.kind == "phase_renumbered").count();
    assert_eq!((before, after), (1, 1));
}

#[test]
fn only_you_decide_a_phase_and_only_while_the_team_runs() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    extend(
        &mut team,
        &json!({"tasks":[{"id":3,"title":"Fix","assignee":"dev"}]}),
        &Actor::Member("lead".into()),
        &none,
        &mut events,
    )
    .unwrap();
    // The owner approves in person; the lead needs the owner's exact word (F13).
    let error = apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Member("lead".into()), &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "The lead approves a phase only on the owner's word: pass \"owner_word\"");
    // A finished team refuses both.
    for op in ["approve-phase", "discard-phase"] {
        let mut stopped = team.clone();
        stopped.status = TeamStatus::Done;
        let error = apply(&mut stopped, op, &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
        assert_eq!(error, "The team is not running");
    }
    // The lead discards its own draft.
    let discarded = apply(&mut team, "discard-phase", &json!({"phase":2}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!(discarded["status"], "phase_discarded");
    let error = apply(&mut team, "approve-phase", &json!({"phase":9}), &Actor::Human, &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Phase 9 is not waiting for approval");
}

#[test]
fn extend_limits_are_refused_clearly() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    let many: Vec<_> = (3..=53).map(|id| json!({"id":id,"title":"T","assignee":"dev"})).collect();
    let error = extend(&mut team, &json!({"tasks":many}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
    assert_eq!(error, "A phase has at most 50 tasks");
    let error = extend(&mut team, &json!({"tasks":[]}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
    assert_eq!(error, "A phase needs at least one task");
    let error = extend(&mut team, &json!({}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
    assert_eq!(error, "A phase needs at least one task");
    team.tasks = (0..500)
        .map(|n| Task { id: n + 1, title: format!("T{n}"), assignee: "dev".into(), ..Default::default() })
        .collect();
    let error = extend(
        &mut team,
        &json!({"tasks":[{"id":501,"title":"A","assignee":"dev"}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap_err()
    .to_string();
    assert_eq!(error, "A team has at most 500 tasks (it has 500)");
    let mut team = running_team();
    team.phases = (0..100).map(|i| Phase { n: i + 2, state: PhaseState::Approved, ..Default::default() }).collect();
    let error = extend(
        &mut team,
        &json!({"tasks":[{"id":3,"title":"A","assignee":"dev"}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap_err()
    .to_string();
    assert_eq!(error, "A team has at most 100 phases");
}

#[test]
fn extend_ids_continue_and_validate_dependencies_assignees_and_cycles() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    // #754: a number given twice is refused; any other numbering is renumbered.
    let error = extend(
        &mut team,
        &json!({"tasks":[{"id":4,"title":"Skip","assignee":"dev"},{"id":4,"title":"Again","assignee":"dev"}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap_err()
    .to_string();
    assert_eq!(error, "Task #4 is given twice among the new tasks");
    let error = extend(
        &mut team,
        &json!({"tasks":[{"id":3,"title":"A","assignee":"dev","deps":[{"task":9}]}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("Task #3 depends on #9, which is neither in the plan nor among the new tasks"), "{error}");
    let error = extend(
        &mut team,
        &json!({"tasks":[{"id":3,"title":"A","assignee":"ghost"}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("no assignee"), "{error}");
    let error = extend(
        &mut team,
        &json!({"tasks":[
            {"id":3,"title":"A","assignee":"dev","deps":[{"task":4}]},
            {"id":4,"title":"B","assignee":"dev","deps":[{"task":3}]}
        ]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("Dependency cycle"), "{error}");
    // A refused phase puts the tasks back: nothing changed.
    assert_eq!(team.tasks.len(), 2);
    assert!(team.phases.is_empty());
    let added = extend(
        &mut team,
        &json!({"tasks":[
            {"id":3,"title":"A","assignee":"dev","deps":[{"task":1}]},
            {"id":4,"title":"B","assignee":"qa","deps":[{"task":3,"kind":"soft"}]}
        ]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap();
    assert_eq!(added["tasks"], json!([3, 4]));
    assert_eq!(team.task(4).unwrap().deps[0].kind, DependencyKind::Soft);
}

#[test]
fn findings_about_older_tasks_never_block_an_extend() {
    let mut team = running_team();
    team.members.iter_mut().find(|member| member.id == "qa").unwrap().state = MemberState::Removed;
    let mut events = Vec::new();
    let added = extend(
        &mut team,
        &json!({"tasks":[{"id":3,"title":"A","assignee":"dev"}]}),
        &Actor::Human,
        &BTreeSet::new(),
        &mut events,
    )
    .unwrap();
    assert_eq!(added["status"], "added");
}

#[test]
fn extend_returns_offline_warnings_without_refusing() {
    let mut team = running_team();
    team.members.iter_mut().find(|member| member.id == "dev").unwrap().computer = "msi".into();
    let mut offline = BTreeSet::new();
    offline.insert("msi".to_string());
    let mut events = Vec::new();
    let added = extend(
        &mut team,
        &json!({"tasks":[{"id":3,"title":"A","assignee":"dev"}]}),
        &Actor::Human,
        &offline,
        &mut events,
    )
    .unwrap();
    assert_eq!(added["validation"]["warnings"][0]["kind"], "offline");
    assert_eq!(team.task(3).unwrap().phase, 2);
}

#[test]
fn approving_a_phase_revalidates_its_assignees() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    // Phase 1 ended, so phase 2 is next in line.
    for id in [1, 2] {
        apply(&mut team, "cancel", &json!({"task":id}), &Actor::Human, &none, &mut events).unwrap();
    }
    extend(
        &mut team,
        &json!({"tasks":[{"id":3,"title":"Fix","assignee":"qa"}]}),
        &Actor::Member("lead".into()),
        &none,
        &mut events,
    )
    .unwrap();
    team.members.iter_mut().find(|member| member.id == "qa").unwrap().state = MemberState::Removed;
    let error = apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events)
        .unwrap_err()
        .to_string();
    assert!(error.contains("no assignee"), "{error}");
    assert_eq!(team.phases[0].state, PhaseState::Draft);
    assert_eq!(team.task(3).unwrap().status, TaskStatus::Draft);
}

#[test]
fn extend_refuses_dependencies_on_cancelled_or_rejected_tasks() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    apply(&mut team, "cancel", &json!({"task":1}), &Actor::Human, &none, &mut events).unwrap();
    apply(&mut team, "cancel", &json!({"task":2}), &Actor::Human, &none, &mut events).unwrap();
    let error = apply(
        &mut team,
        "extend",
        &json!({"tasks":[{"id":3,"title":"A","assignee":"dev","deps":[{"task":1}]}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap_err()
    .to_string();
    assert_eq!(error, "#3 depends on #1, which is cancelled");
    team.task_mut(2).unwrap().status = TaskStatus::Rejected;
    let error = apply(
        &mut team,
        "extend",
        &json!({"tasks":[{"id":3,"title":"A","assignee":"dev","deps":[{"task":2,"kind":"soft"}]}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap_err()
    .to_string();
    assert_eq!(error, "#3 depends on #2, which is rejected");
}

#[test]
fn cancelling_a_draft_phases_last_task_discards_the_phase() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    for id in [1, 2] {
        apply(&mut team, "cancel", &json!({"task":id}), &Actor::Human, &none, &mut events).unwrap();
    }
    apply(
        &mut team,
        "extend",
        &json!({"tasks":[{"id":3,"title":"A","assignee":"dev"},{"id":4,"title":"B","assignee":"dev"}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap();
    assert_eq!(team.phases[0].state, PhaseState::Draft);
    apply(&mut team, "cancel", &json!({"task":3}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(team.phases[0].state, PhaseState::Draft);
    apply(&mut team, "cancel", &json!({"task":4}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(team.phases[0].state, PhaseState::Discarded);
    assert_eq!(team.phases[0].decided_by, "You");
    assert_eq!(team.idle, Notice::Pending);
    // The cancelled draft's tasks keep their phase number; the next phase
    // follows it (the record itself is compacted away).
    let added = apply(
        &mut team,
        "extend",
        &json!({"tasks":[{"id":5,"title":"C","assignee":"dev"}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap();
    assert_eq!(added["phase"], 3);
}

#[test]
fn stop_discards_a_draft_phase_record() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    apply(
        &mut team,
        "extend",
        &json!({"tasks":[{"id":3,"title":"A","assignee":"dev"}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap();
    apply(&mut team, "stop", &json!({}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(team.status, TeamStatus::Stopped);
    let record = &team.phases[0];
    assert_eq!((record.state, record.decided_by.as_str()), (PhaseState::Discarded, "You"));
    assert!(record.decided_at.is_some());
}

#[test]
fn finish_discards_a_leftover_draft_record() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    for id in [1, 2] {
        apply(&mut team, "cancel", &json!({"task":id}), &Actor::Human, &none, &mut events).unwrap();
    }
    team.phases.push(Phase { n: 2, state: PhaseState::Draft, ..Default::default() });
    let done = apply(&mut team, "finish", &json!({"summary":"Done"}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(done["status"], "finished");
    let record = &team.phases[0];
    assert_eq!((record.state, record.decided_by.as_str()), (PhaseState::Discarded, "You"));
}

#[test]
fn a_long_phase_title_is_refused_before_the_task_errors() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    let error = apply(
        &mut team,
        "extend",
        &json!({"phase_title":"x".repeat(121),"tasks":[{"id":9,"title":"A","assignee":"dev"}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap_err()
    .to_string();
    assert_eq!(error, "A phase title is too long");
}

#[test]
fn deciding_is_refused_once_the_team_is_done() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    for id in [1, 2] {
        apply(&mut team, "cancel", &json!({"task":id}), &Actor::Human, &none, &mut events).unwrap();
    }
    apply(&mut team, "finish", &json!({"summary":"Done"}), &Actor::Human, &none, &mut events).unwrap();
    for op in ["accept", "reject"] {
        let error = apply(&mut team, op, &json!({"task":1,"reason":"late"}), &Actor::Human, &none, &mut events)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "The team is not running", "{op}");
    }
}

#[test]
fn plan_and_extend_carry_the_plan_health() {
    let (mut team, _) = create("t", &json!({"name":"T","lead":{"id":"lead","agent":"claude"}})).unwrap();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    // C7: the planning lead sees the numbers before asking for approval.
    let receipt = plan(
        &mut team,
        &json!({"members":[{"id":"dev","role":"Backend","agent":"codex"}],"tasks":[
            {"id":1,"title":"API","assignee":"dev"},
            {"id":2,"title":"Tests","assignee":"dev","deps":[{"task":1}]}]}),
        &Actor::Member("lead".into()),
        &mut events,
    )
    .unwrap();
    assert_eq!(receipt["validation"]["health"]["tasks"], 2);
    assert_eq!(receipt["validation"]["health"]["critical_path"], 2);
    assert!(receipt["validation"]["health"]["per_member"].get("dev").is_some());
    approve(&mut team, &json!({}), &Actor::Human, &none, &mut events).unwrap();
    let added = extend(
        &mut team,
        &json!({"tasks":[{"id":3,"title":"Docs","assignee":"dev"}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap();
    assert_eq!(added["validation"]["health"]["tasks"], 3);
    // Every task is one member's work: the chain and the extra task take
    // three scheduling steps.
    assert_eq!(added["validation"]["health"]["critical_path"], 3);
    assert_eq!(added["validation"]["health"]["chain"], 2);
}

#[test]
fn a_stale_draft_revision_is_refused_until_status_is_read_again() {
    let mut team = running_team();
    let mut events = Vec::new();
    extend(&mut team, &json!({"phase_title":"A","tasks":[{"id":3,"title":"A","assignee":"dev"}]}), &Actor::Member("lead".into()), &BTreeSet::new(), &mut events).unwrap();
    // The caller read this revision; a renumber or discard moves the marker.
    let read = team.revision;
    team.phase_numbers_revision = team.revision + 1;
    for op in ["edit-draft", "move-phase"] {
        let input = if op == "edit-draft" {
            json!({"phase":2,"task":3,"title":"Changed","revision":read})
        } else {
            json!({"phase":2,"to":"after","revision":read})
        };
        let error = apply(&mut team, op, &input, &Actor::Human, &BTreeSet::new(), &mut events).unwrap_err().to_string();
        assert_eq!(error, format!("Draft numbers changed after revision {read}; read status again"), "{op}");
    }
    let error = apply(&mut team, "discard-phase", &json!({"phase":2,"revision":read}), &Actor::Human, &BTreeSet::new(), &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, format!("Draft numbers changed after revision {read}; read status again"));
    // The revision the change produced works, and no revision works as before.
    let current = team.phase_numbers_revision;
    apply(&mut team, "edit-draft", &json!({"phase":2,"task":3,"title":"Changed","revision":current}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
    apply(&mut team, "edit-draft", &json!({"phase":2,"task":3,"title":"Again"}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
    // A real discard moves the marker and refuses the revision it read.
    let mut fresh = running_team();
    extend(&mut fresh, &json!({"phase_title":"A","tasks":[{"id":3,"title":"A","assignee":"dev"}]}), &Actor::Member("lead".into()), &BTreeSet::new(), &mut events).unwrap();
    let before = fresh.revision;
    apply(&mut fresh, "discard-phase", &json!({"phase":2}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
    assert_eq!(fresh.phase_numbers_revision, before + 1);
    let error = apply(&mut fresh, "edit-draft", &json!({"phase":2,"task":3,"title":"X","revision":before}), &Actor::Human, &BTreeSet::new(), &mut events)
        .unwrap_err()
        .to_string();
    assert!(error.contains("Draft numbers changed"), "{error}");
}

#[test]
fn a_draft_phase_takes_added_changed_and_removed_tasks() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    extend(&mut team, &json!({"tasks":[{"id":3,"title":"Fix","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!(team.phases.len(), 1);
    // extend with "phase": 2 adds to the draft, not to a new phase (F10).
    let added = extend(&mut team, &json!({"phase":2,"tasks":[{"id":4,"title":"More","assignee":"qa"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!((added["status"].as_str(), added["phase"].as_u64()), (Some("edited"), Some(2)));
    assert_eq!((team.phases.len(), team.phases[0].tasks.clone()), (1, vec![3, 4]));
    assert_eq!(team.task(4).unwrap().phase, 2);
    // edit-draft changes the phase and its draft tasks, and removes one.
    let edited = apply(
        &mut team,
        "edit-draft",
        &json!({"phase":2,"title":"QA fixes","barrier":true,
            "tasks_update":[{"id":3,"title":"Fix the login crash","assignee":"qa","pinned":true}],
            "tasks_remove":[4]}),
        &Actor::Member("lead".into()),
        &none,
        &mut events,
    )
    .unwrap();
    assert_eq!(edited["status"], "draft_edited");
    assert_eq!((team.phases[0].title.as_str(), team.phases[0].barrier), ("QA fixes", true));
    let task = team.task(3).unwrap();
    assert_eq!((task.title.as_str(), task.assignee.as_str(), task.pinned), ("Fix the login crash", "qa", true));
    assert!(team.task(4).is_none() && team.phases[0].tasks == vec![3]);
    // The event carries the diff the owner sees, and the phase keeps its
    // edits for the draft view (F10).
    let diff = events.iter().rev().find(|event| event.kind == "phase_edited").unwrap();
    assert_eq!(diff.payload["diff"]["removed"], json!([4]));
    assert_eq!(diff.payload["diff"]["title"]["to"], "QA fixes");
    assert_eq!((diff.payload["diff"]["updated"][0]["id"].as_u64(), diff.payload["diff"]["updated"][0]["fields"].clone()), (Some(3), json!(["title","assignee","pinned"])));
    let changes = &team.phases[0].changes;
    assert_eq!(changes[0]["kind"], "added");
    assert_eq!((changes[0]["task"].as_u64(), changes[0]["title"].as_str()), (Some(4), Some("More")));
    assert!(changes.iter().any(|change| change["kind"] == "changed" && change["field"] == "title" && change["from"] == "Fix" && change["to"] == "Fix the login crash"), "{changes:?}");
    assert!(changes.iter().any(|change| change["kind"] == "changed" && change["field"] == "assignee" && change["from"] == "dev" && change["to"] == "qa"), "{changes:?}");
    assert_eq!(changes.last().unwrap()["kind"], "removed");
    assert_eq!(changes.last().unwrap()["task"].as_u64(), Some(4));
    // A removed id is free again: the next extend takes the highest live
    // number (F285).
    let next = extend(&mut team, &json!({"tasks":[{"id":4,"title":"Next","assignee":"dev"}]}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((next["phase"].as_u64(), next["tasks"].clone()), (Some(3), json!([4])));
}

#[test]
fn a_task_that_has_not_started_moves_between_phases_keeping_its_id() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    // A second phase, accepted while phase 1 runs: it queues, so none of its
    // tasks has started.
    let added = extend(
        &mut team,
        &json!({"phase_title":"Next","tasks":[{"id":3,"title":"D","assignee":"dev"},{"id":4,"title":"E","assignee":"dev"}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap();
    assert_eq!(added["phase"], 2);
    apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(team.phases[0].state, PhaseState::Queued);
    // #4 moves to phase 1, the plan's own phase (which has no record): the
    // id stays, the task is Pending there, and the phase record moves it.
    let moved = apply(&mut team, "move-task", &json!({"task":4,"phase":1}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!(
        (moved["status"].as_str(), moved["task"].as_u64(), moved["from"].as_u64(), moved["to"].as_u64()),
        (Some("moved"), Some(4), Some(2), Some(1))
    );
    let task = team.task(4).unwrap();
    assert_eq!((task.id, task.phase, task.status), (4, 1, TaskStatus::Pending));
    assert_eq!(team.phases[0].tasks, vec![3]);
    assert!(events.iter().any(|event| event.kind == "task_moved" && event.payload["task"] == 4));
    // It cannot move to the phase it is in.
    let error = apply(&mut team, "move-task", &json!({"task":4,"phase":1}), &Actor::Human, &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Task #4 is already in phase 1");
    // A task that has started never moves.
    team.task_mut(3).unwrap().attempts.push(Attempt { member: "dev".into(), state: AttemptState::Delivered, ..Default::default() });
    let error = apply(&mut team, "move-task", &json!({"task":3,"phase":1}), &Actor::Human, &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Task #3 has started; only a task with no delivered attempt moves");
    // A decided task never moves.
    apply(&mut team, "cancel", &json!({"task":1}), &Actor::Human, &none, &mut events).unwrap();
    let error = apply(&mut team, "move-task", &json!({"task":1,"phase":2}), &Actor::Human, &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Task #1 is already decided");
    // A member that is not the lead does not move tasks.
    let error = apply(&mut team, "move-task", &json!({"task":3,"phase":1}), &Actor::Member("dev".into()), &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Only the lead or you move a task");
    // An unknown target phase is refused.
    let error = apply(&mut team, "move-task", &json!({"task":4,"phase":9}), &Actor::Human, &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Unknown phase 9");
}

#[test]
fn moving_a_task_checks_its_dependencies_against_the_target_phase() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    // Phase 2: #3, and #4 depends on #3.
    extend(
        &mut team,
        &json!({"phase_title":"Next","tasks":[{"id":3,"title":"D","assignee":"dev"},{"id":4,"title":"E","assignee":"dev","deps":[{"task":3,"kind":"hard"}]}]}),
        &Actor::Human,
        &none,
        &mut events,
    )
    .unwrap();
    apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    // #4 cannot go before its dependency.
    let error = apply(&mut team, "move-task", &json!({"task":4,"phase":1}), &Actor::Human, &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Task #4 depends on #3, which stays in a later phase (2); move it first");
    // The dependency moves first, then its dependent.
    apply(&mut team, "move-task", &json!({"task":3,"phase":1}), &Actor::Human, &none, &mut events).unwrap();
    let moved = apply(&mut team, "move-task", &json!({"task":4,"phase":1}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(moved["status"], "moved");
    assert_eq!((team.task(3).unwrap().phase, team.task(4).unwrap().phase), (1, 1));
}

#[test]
fn a_queued_phase_with_nothing_started_edits_like_a_draft() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    extend(&mut team, &json!({"phase_title":"Next","tasks":[{"id":3,"title":"D","assignee":"dev"}]}), &Actor::Human, &none, &mut events).unwrap();
    apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(team.phases[0].state, PhaseState::Queued);
    // extend adds Pending tasks to the queued phase, with no new phase.
    let added = extend(&mut team, &json!({"phase":2,"tasks":[{"id":4,"title":"E","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!((added["status"].as_str(), added["phase"].as_u64()), (Some("edited"), Some(2)));
    assert_eq!(team.phases.len(), 1);
    assert_eq!((team.task(4).unwrap().phase, team.task(4).unwrap().status), (2, TaskStatus::Pending));
    // edit-draft changes and removes its waiting tasks.
    let edited = apply(
        &mut team,
        "edit-draft",
        &json!({"phase":2,"tasks_update":[{"id":3,"title":"D2"}],"tasks_remove":[4]}),
        &Actor::Member("lead".into()),
        &none,
        &mut events,
    )
    .unwrap();
    assert_eq!(edited["status"], "draft_edited");
    assert_eq!(team.task(3).unwrap().title, "D2");
    assert!(team.task(4).is_none());
    // Once a task of the phase has started, the phase no longer changes.
    team.task_mut(3).unwrap().attempts.push(Attempt { member: "dev".into(), state: AttemptState::Delivered, ..Default::default() });
    let error = extend(&mut team, &json!({"phase":2,"tasks":[{"id":5,"title":"F","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Phase 2 has started; only a phase with no delivered attempt can change");
    let error = apply(&mut team, "edit-draft", &json!({"phase":2,"tasks_update":[{"id":3,"title":"D3"}]}), &Actor::Member("lead".into()), &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Phase 2 has started; only a phase with no delivered attempt can change");
}

#[test]
fn moving_a_draft_changes_the_approval_order() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    for id in [3, 4] {
        extend(&mut team, &json!({"tasks":[{"id":id,"title":"T","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    }
    assert_eq!(team.phases.iter().map(|phase| (phase.n, phase.order())).collect::<Vec<_>>(), vec![(2, 2), (3, 3)]);
    // Move phase 3 before phase 2: the numbers and the tasks follow the new
    // order (F285), and the feed hears where it went.
    let moved = apply(&mut team, "move-phase", &json!({"phase":3,"to":2,"position":"before"}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!((moved["phase"].as_u64(), moved["to"].as_u64(), moved["renumbered"].as_bool()), (Some(3), Some(1), Some(true)));
    assert_eq!(moved["order"].clone(), json!([2, 3]));
    assert_eq!(team.phases.iter().map(|phase| (phase.n, phase.order())).collect::<Vec<_>>(), vec![(2, 2), (3, 3)]);
    // The moved draft took phase 2 and task #3; the other took 3 and #4.
    assert_eq!((team.phases[0].tasks.clone(), team.phases[1].tasks.clone()), (vec![3], vec![4]));
    assert_eq!(team.task(3).unwrap().title, "T");
    let moved_changes = &team.phases[0].changes;
    assert_eq!(moved_changes.last().unwrap()["kind"], "moved");
    assert_eq!((moved_changes.last().unwrap()["from"].as_u64(), moved_changes.last().unwrap()["to"].as_u64()), (Some(2), Some(1)));
    let event = events.iter().rev().find(|event| event.kind == "phase_moved").unwrap();
    assert_eq!((event.payload["phase"].as_u64(), event.payload["to"].as_u64(), event.payload["renumbered"].as_bool()), (Some(3), Some(1), Some(true)));
    // The renumbering is recorded old -> new (F285).
    let renumbered = events.iter().rev().find(|event| event.kind == "phase_renumbered").unwrap();
    assert_eq!(renumbered.payload["phases"], json!([{"from":3,"to":2},{"from":2,"to":3}]));
    assert_eq!(renumbered.payload["tasks"], json!([{"from":4,"to":3},{"from":3,"to":4}]));
    // Phase 2 opens first; phase 3 waits for it.
    for id in [1, 2] {
        apply(&mut team, "cancel", &json!({"task":id}), &Actor::Human, &none, &mut events).unwrap();
    }
    let error = apply(&mut team, "approve-phase", &json!({"phase":3}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
    assert_eq!(error, "Phase 3 opens after the previous phase is approved");
    let approved = apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((approved["status"].as_str(), approved["tasks"].clone()), (Some("phase_approved"), json!([3])));
    apply(&mut team, "cancel", &json!({"task":3}), &Actor::Human, &none, &mut events).unwrap();
    let approved = apply(&mut team, "approve-phase", &json!({"phase":3}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(approved["tasks"], json!([4]));
}

#[test]
fn a_draft_keeps_the_owners_edit_and_creates_after_the_preflight() {
    let (mut team, _) = create("t", &json!({"name":"T","lead":{"id":"lead","agent":"claude"}})).unwrap();
    team.status = TeamStatus::Draft;
    team.draft = Some(Draft { revision: 1, ..Default::default() });
    let mut events = Vec::new();
    let none = BTreeSet::new();
    // The lead edits the goal; the owner sees it later.
    apply(&mut team, "draft", &json!({"action":"update","revision":1,"set":{"goal":"Ship"}}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!((team.goal.as_str(), team.draft.as_ref().unwrap().revision), ("Ship", 2));
    assert_eq!(team.draft.as_ref().unwrap().unseen, vec!["goal".to_string()]);
    // Spec 3: the protocol addendum is the team's lead protocol.
    apply(&mut team, "draft", &json!({"action":"update","revision":2,"set":{"addendum":"Two waves"}}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!((team.lead_protocol.as_str(), team.draft.as_ref().unwrap().unseen.len()), ("Two waves", 2));
    // The owner's edit wins over a later lead edit of that field.
    apply(&mut team, "draft", &json!({"action":"update","revision":3,"set":{"members":[{"id":"dev","role":"Backend","agent":"claude"}]}}), &Actor::Human, &none, &mut events).unwrap();
    let error = apply(&mut team, "draft", &json!({"action":"update","revision":4,"set":{"members":[{"id":"qa","role":"Tester","agent":"claude"}]}}), &Actor::Member("lead".into()), &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Owner changed members");
    // A stale revision is refused with the current one, and seen clears it.
    let error = apply(&mut team, "draft", &json!({"action":"update","revision":1,"set":{"goal":"X"}}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("Draft revision is 4 now"), "{error}");
    let value = apply(&mut team, "draft", &json!({"action":"seen","fields":["goal","addendum"]}), &Actor::Human, &none, &mut events).unwrap();
    assert!(value["unseen"].as_array().unwrap().is_empty());
    // Create runs the preflight: an unknown assignee refuses, a good plan creates.
    team.tasks.push(Task { id: 1, title: "A".into(), assignee: "ghost".into(), ..Default::default() });
    let error = apply(&mut team, "draft", &json!({"action":"create"}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("no assignee"), "{error}");
    team.tasks[0].assignee = "dev".into();
    let value = apply(&mut team, "draft", &json!({"action":"create"}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(value["status"], "planning");
    assert_eq!(team.status, TeamStatus::Planning);
    assert!(team.draft.is_none());
    assert_eq!(team.tasks[0].status, TaskStatus::Pending);
    // Discard stops a draft and carries stop_lead.
    let mut fresh = running_team();
    fresh.status = TeamStatus::Draft;
    fresh.draft = Some(Draft::default());
    let value = apply(&mut fresh, "draft", &json!({"action":"discard","stop_lead":true}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((fresh.status, value["stop_lead"].as_bool()), (TeamStatus::Stopped, Some(true)));
}

#[test]
fn a_phase_opens_when_the_previous_is_approved_not_finished() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    extend(&mut team, &json!({"tasks":[{"id":3,"title":"Fix","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    extend(&mut team, &json!({"tasks":[{"id":4,"title":"Next","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    // F12: phase 2 opens while phase 1's tasks are still open, and phase 3
    // while phase 2's are: approval, not completion, orders the phases.
    let approved = apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(approved["phase"], 2);
    apply(&mut team, "approve-phase", &json!({"phase":3}), &Actor::Human, &none, &mut events).unwrap();
    // A draft before it still refuses the approval.
    let mut review = running_team();
    review.settings.approval = Approval::Review;
    extend(&mut review, &json!({"tasks":[{"id":3,"title":"Fix","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    extend(&mut review, &json!({"tasks":[{"id":4,"title":"Next","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    let error = apply(&mut review, "approve-phase", &json!({"phase":3}), &Actor::Human, &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("after the previous phase is approved"), "{error}");
}

#[test]
fn an_approved_phase_edits_until_its_work_starts() {
    let mut team = running_team();
    let mut events = Vec::new();
    let none = BTreeSet::new();
    extend(&mut team, &json!({"tasks":[{"id":3,"title":"Fix","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    for id in [1, 2] {
        apply(&mut team, "cancel", &json!({"task":id}), &Actor::Human, &none, &mut events).unwrap();
    }
    apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    // U58: an approved phase whose work has not started is edited like a
    // draft; move-phase stays the drafts' own operation.
    let edited = apply(&mut team, "edit-draft", &json!({"phase":2,"title":"Later"}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(edited["status"], "draft_edited");
    let added = extend(&mut team, &json!({"phase":2,"tasks":[{"id":4,"title":"More","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!((added["status"].as_str(), added["phase"].as_u64()), (Some("edited"), Some(2)));
    let error = apply(&mut team, "move-phase", &json!({"phase":2,"to":3,"position":"before"}), &Actor::Human, &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Phase 2 is approved or discarded; only a draft phase can change");
    // Once a task of the phase has started, the phase no longer changes.
    team.task_mut(3).unwrap().attempts.push(Attempt { member: "dev".into(), state: AttemptState::Delivered, ..Default::default() });
    let error = apply(&mut team, "edit-draft", &json!({"phase":2,"title":"Later"}), &Actor::Human, &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Phase 2 has started; only a phase with no delivered attempt can change");
    let error = extend(&mut team, &json!({"phase":2,"tasks":[{"id":5,"title":"More","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Phase 2 has started; only a phase with no delivered attempt can change");
    // The lead discards its own draft, and the owner still approves (F10).
    let added = extend(&mut team, &json!({"tasks":[{"id":5,"title":"Next","assignee":"dev"}]}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!(added["phase"], 3);
    let discarded = apply(&mut team, "discard-phase", &json!({"phase":3}), &Actor::Member("lead".into()), &none, &mut events).unwrap();
    assert_eq!(discarded["status"], "phase_discarded");
    let error = apply(&mut team, "approve-phase", &json!({"phase":3}), &Actor::Member("lead".into()), &none, &mut events).unwrap_err().to_string();
    assert_eq!(error, "The lead approves a phase only on the owner's word: pass \"owner_word\"");
}

#[test]
fn plan_health_counts_only_this_levels_tasks() {
    let parent = running_team();
    let mut child = Team { id: "child".into(), name: "Child".into(), lead: "dev".into(), ..Default::default() };
    child.parent = Some(Parent { team: parent.id.clone(), task: 1, lead: "lead".into(), member: "dev".into() });
    child.members.push(Member { id: "dev".into(), role: "Backend".into(), agent: "codex".into(), ..Default::default() });
    for id in 1..=5 {
        child.tasks.push(Task { id, title: format!("Child {id}"), assignee: "dev".into(), ..Default::default() });
    }
    // Section 9.4: each tree level keeps its own plan health.
    assert_eq!(plan::validate(&parent, &BTreeSet::new()).health.tasks, 2);
    assert_eq!(plan::validate(&child, &BTreeSet::new()).health.tasks, 5);
}

#[test]
fn a_send_back_written_before_the_drafts_were_renumbered_is_refused() {
    let mut team = running_team();
    team.settings.approval = Approval::Review;
    let mut events = Vec::new();
    let none = BTreeSet::new();
    let lead = Actor::Member("lead".into());
    extend(&mut team, &json!({"phase_title":"Build","tasks":[{"id":3,"title":"Fix","assignee":"dev"}]}), &lead, &none, &mut events).unwrap();
    let seen = team.revision;
    team.phase_numbers_revision = seen + 1;
    let error = apply(&mut team, "reject-phase", &json!({"phase":2,"comment":"Split it","revision":seen}), &Actor::Human, &none, &mut events)
        .unwrap_err();
    assert_eq!(error.to_string(), format!("Draft numbers changed after revision {seen}; read status again"));
    assert_eq!(team.phases[0].state, PhaseState::Draft);
    let receipt =
        apply(&mut team, "reject-phase", &json!({"phase":2,"comment":"Split it","revision":seen + 1}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(receipt, json!({"status":"phase_rejected","phase":2}));
}

#[test]
fn the_owner_sends_a_draft_back_and_the_lead_reworks_it() {
    let mut team = running_team();
    team.settings.approval = Approval::Review;
    let mut events = Vec::new();
    let none = BTreeSet::new();
    let lead = Actor::Member("lead".into());
    extend(&mut team, &json!({"phase_title":"Build","tasks":[{"id":3,"title":"Fix","assignee":"dev"}]}), &lead, &none, &mut events).unwrap();
    // Only the owner sends a phase back, and only with a comment.
    let error = apply(&mut team, "reject-phase", &json!({"phase":2,"comment":"Split it"}), &lead, &none, &mut events).unwrap_err();
    assert_eq!(error.to_string(), "Only you send a phase back");
    let error = apply(&mut team, "reject-phase", &json!({"phase":2,"comment":"  "}), &Actor::Human, &none, &mut events).unwrap_err();
    assert_eq!(error.to_string(), "Say what the lead should change in this phase");
    let receipt = apply(&mut team, "reject-phase", &json!({"phase":2,"comment":"Split the fix"}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(receipt, json!({"status":"phase_rejected","phase":2}));
    assert_eq!(team.phases[0].state, PhaseState::Rejected);
    assert_eq!(team.phases[0].rejection.as_ref().unwrap().comment, "Split the fix");
    // Its tasks stay drafts: nothing is handed out.
    assert_eq!(team.task(3).unwrap().status, TaskStatus::Draft);
    assert!(!plan::ready(&team).contains(&3));
    // The lead hears it as a review item with the rework command.
    let items = super::super::inbox::items(&team);
    let item = items.iter().find(|item| item.kind == "phase_rejected").expect("review item");
    assert!(item.summary.contains("Phase 2 \u{b7} Build back: Split the fix"), "{}", item.summary);
    assert!(item.command.contains("hgs team rework-phase --json"), "{}", item.command);
    // Accept waits for the rework or an Undo.
    let error = apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap_err();
    assert!(error.to_string().contains("was sent back"), "{error}");
    // Undo puts the draft back as it was, without a comment or item.
    let undone = apply(&mut team, "undo-reject-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(undone["status"], "phase_reject_undone");
    assert_eq!((team.phases[0].state, team.phases[0].rejection.is_none()), (PhaseState::Draft, true));
    assert!(super::super::inbox::items(&team).iter().all(|item| item.kind != "phase_rejected"));
    let error = apply(&mut team, "undo-reject-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap_err();
    assert_eq!(error.to_string(), "Phase 2 was not sent back");
    // Sent back again: the lead edits the phase, then reworks it.
    apply(&mut team, "reject-phase", &json!({"phase":2,"comment":"Split the fix"}), &Actor::Human, &none, &mut events).unwrap();
    apply(&mut team, "edit-draft", &json!({"phase":2,"title":"Build, split"}), &lead, &none, &mut events).unwrap();
    let error = apply(&mut team, "rework-phase", &json!({"phase":2}), &Actor::Member("dev".into()), &none, &mut events).unwrap_err();
    assert_eq!(error.to_string(), "Only the lead or you rework a phase");
    let reworked = apply(&mut team, "rework-phase", &json!({"phase":2}), &lead, &none, &mut events).unwrap();
    assert_eq!(reworked["note"], "Reworked after your comment: \u{201c}Split the fix\u{201d}");
    assert_eq!(team.phases[0].state, PhaseState::Draft);
    assert_eq!(team.phases[0].note, "Reworked after your comment: \u{201c}Split the fix\u{201d}");
    assert!(team.phases[0].rejection.as_ref().unwrap().reworked_at.is_some());
    // A reworked draft cannot be undone any more; the owner accepts it.
    assert!(apply(&mut team, "undo-reject-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).is_err());
    let accepted = apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(accepted["status"], "phase_approved");
    let kinds = kinds(&events);
    for kind in ["phase_rejected", "phase_reject_undone", "phase_reworked", "phase_approved"] {
        assert!(kinds.iter().any(|found| found == kind), "{kind}: {kinds:?}");
    }
}

#[test]
fn a_phase_accepted_while_the_previous_runs_queues_and_starts_after_it() {
    let mut team = running_team();
    team.settings.approval = Approval::Review;
    let mut events = Vec::new();
    let none = BTreeSet::new();
    let lead = Actor::Member("lead".into());
    extend(&mut team, &json!({"phase_title":"Build","tasks":[{"id":3,"title":"Fix","assignee":"dev"}]}), &lead, &none, &mut events).unwrap();
    let receipt = apply(&mut team, "approve-phase", &json!({"phase":2}), &Actor::Human, &none, &mut events).unwrap();
    // The receipt keeps its fields and names the state.
    assert_eq!((receipt["status"].as_str(), receipt["phase_state"].as_str()), (Some("phase_approved"), Some("queued")));
    assert_eq!(team.phases[0].state, PhaseState::Queued);
    assert_eq!(team.task(3).unwrap().status, TaskStatus::Pending);
    // Only the running phase is handed out (#2 waits for #1 softly).
    assert_eq!(plan::ready(&team), vec![1]);
    let summary = plan::phase_summary(&team);
    assert_eq!((summary[0]["state"].as_str(), summary[1]["state"].as_str()), (Some("running"), Some("queued")));
    // G9's status chip stays: approved, not started.
    assert_eq!(summary[1]["status"], "approved");
    assert_eq!(plan::stuck_scope(&team), vec![1, 2]);
    events.clear();
    apply(&mut team, "cancel", &json!({"task":1}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(team.phases[0].state, PhaseState::Queued);
    apply(&mut team, "cancel", &json!({"task":2}), &Actor::Human, &none, &mut events).unwrap();
    // Phase 1 ended: phase 2 starts at once and is handed out.
    assert_eq!(team.phases[0].state, PhaseState::Approved);
    assert_eq!(plan::ready(&team), vec![3]);
    assert_eq!(plan::stuck_scope(&team), vec![3]);
    let started: Vec<&Event> = events.iter().filter(|event| matches!(event.kind, "phase_done" | "phase_started")).collect();
    assert_eq!(started.len(), 2, "{:?}", kinds(&events));
    assert_eq!((started[0].kind, &started[0].payload["phase"]), ("phase_done", &json!(1)));
    assert_eq!((started[1].kind, &started[1].payload["tasks"]), ("phase_started", &json!(1)));
    // With nothing open before it, an accepted phase runs at once.
    extend(&mut team, &json!({"tasks":[{"id":4,"title":"Next","assignee":"dev","deps":[{"task":3}]}]}), &lead, &none, &mut events).unwrap();
    apply(&mut team, "cancel", &json!({"task":3}), &Actor::Human, &none, &mut events).unwrap();
    let receipt = apply(&mut team, "approve-phase", &json!({"phase":3}), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(receipt["phase_state"], "running");
}

/// U61: with Auto-accept on, a draft is not accepted when drafted: it stays
/// the lead's to edit until its first hand-out fixes it by Auto.
#[test]
fn a_draft_is_not_auto_accepted_until_its_first_hand_out() {
    let mut team = running_team();
    team.settings.approval = Approval::Auto;
    let mut events = Vec::new();
    let none = BTreeSet::new();
    let lead = Actor::Member("lead".into());
    apply(&mut team, "settings", &json!({"settings":{"auto_accept":true}}), &Actor::Human, &none, &mut events).unwrap();
    events.clear();
    team.phases.push(Phase {
        n: 2,
        order: 2,
        title: "Next".into(),
        barrier: true,
        approval: Approval::Auto,
        state: PhaseState::Draft,
        tasks: vec![3],
        ..Default::default()
    });
    team.tasks.push(Task { id: 3, title: "Next".into(), phase: 2, status: TaskStatus::Pending, assignee: "dev".into(), ..Default::default() });
    auto_accept(&mut team, &none, &mut events);
    assert_eq!(team.phases[0].state, PhaseState::Draft);
    assert!(!events.iter().any(|event| event.kind == "phase_auto_accepted"), "{events:?}");
    // It stays editable, even after the draft ahead is accepted.
    apply(&mut team, "edit-draft", &json!({"phase":2,"tasks_update":[{"id":3,"assignee":"qa"}]}), &lead, &none, &mut events).unwrap();
    assert_eq!(team.phases[0].state, PhaseState::Draft);
    assert_eq!(team.task(3).unwrap().assignee, "qa");
    // The lead starts it ahead for idle members; the first hand-out fixes it.
    let started = apply(&mut team, "start-ahead", &json!({"phase":2}), &lead, &none, &mut events).unwrap();
    assert_eq!(started["start_ahead"], json!(true));
    let summary = plan::phase_summary(&team);
    let phase = summary.iter().find(|phase| phase["n"] == 2).unwrap();
    assert_eq!(
        (phase["state"].as_str(), phase["auto"].as_bool(), phase["start_ahead"].as_bool()),
        (Some("draft"), Some(true), Some(true))
    );
    events.clear();
    accept_phase_on_hand_out(&mut team, 3, &mut events);
    assert!(team.phases[0].state.is_accepted(), "{:?}", team.phases[0].state);
    assert_eq!(team.phases[0].decided_by, "Auto");
    assert!(!team.phases[0].start_ahead);
    let auto: Vec<&Event> = events.iter().filter(|event| event.kind == "phase_auto_accepted").collect();
    assert_eq!((auto.len(), &auto[0].payload["phase"]), (1, &json!(2)));
}

#[test]
fn start_ahead_needs_an_auto_draft_and_the_lead_and_can_be_undone_before_a_hand_out() {
    let mut team = running_team();
    team.settings.auto_accept = true;
    team.settings.approval = Approval::Review;
    let mut events = Vec::new();
    let none = BTreeSet::new();
    let lead = Actor::Member("lead".into());
    extend(&mut team, &json!({"tasks":[{"id":3,"title":"Review","assignee":"dev"}]}), &lead, &none, &mut events).unwrap();
    // A review draft is the owner's, so the lead cannot start it ahead.
    let error = apply(&mut team, "start-ahead", &json!({"phase":2}), &lead, &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("review draft"), "{error}");
    // An Auto draft, but a member is not the lead.
    team.phases[0].approval = Approval::Auto;
    let error = apply(&mut team, "start-ahead", &json!({"phase":2}), &Actor::Member("dev".into()), &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("lead"), "{error}");
    // Auto-accept must be on: it is what accepts the phase at its hand-out.
    team.settings.auto_accept = false;
    let error = apply(&mut team, "start-ahead", &json!({"phase":2}), &lead, &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("Auto-accept"), "{error}");
    team.settings.auto_accept = true;
    let started = apply(&mut team, "start-ahead", &json!({"phase":2}), &lead, &none, &mut events).unwrap();
    assert_eq!(started["status"], json!("started"));
    let undone = apply(&mut team, "start-ahead", &json!({"phase":2,"undo":true}), &lead, &none, &mut events).unwrap();
    assert_eq!((undone["status"].clone(), team.phases[0].start_ahead), (json!("undone"), false));
    // Once a task is handed out, the phase is accepted and stays.
    apply(&mut team, "start-ahead", &json!({"phase":2}), &lead, &none, &mut events).unwrap();
    team.task_mut(3).unwrap().attempts.push(Attempt { n: 1, id: "a1".into(), member: "dev".into(), ..Default::default() });
    let error = apply(&mut team, "start-ahead", &json!({"phase":2,"undo":true}), &lead, &none, &mut events).unwrap_err().to_string();
    assert!(error.contains("handed-out"), "{error}");
}

/// U29: an addition the owner never confirmed leaves once it expires, on the
/// team's next change. The owner declines one at any time, also after it
/// expired or once its phase no longer runs; only the owner does.
#[test]
fn a_warned_addition_expires_or_is_declined() {
    let mut team = running_team();
    let none = BTreeSet::new();
    let mut events = Vec::new();
    let warn = |team: &mut Team, events: &mut Vec<Event>| {
        let input = json!({"phase":1,"tasks":[{"id":3,"title":"Late","assignee":"dev"}]});
        let warned = apply(team, "add-to-phase", &input, &Actor::Human, &none, events).unwrap();
        assert_eq!(warned["status"], "warning", "{warned}");
        warned["pending"].as_str().unwrap().to_owned()
    };
    let decline = |pending: &str| json!({"phase":1,"confirm":pending,"decline":true});
    // Expired: the next change of any kind drops it.
    let pending = warn(&mut team, &mut events);
    team.pending_additions[0].warned_at -= super::super::owner_word::FRESH_SECONDS + 1.0;
    apply(&mut team, "settings", &json!({"settings":{"remind_lead_minutes":9}}), &Actor::Human, &none, &mut events).unwrap();
    assert!(team.pending_additions.is_empty());
    // Declining it afterwards changes nothing and still succeeds.
    events.clear();
    let declined = apply(&mut team, "add-to-phase", &decline(&pending), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((declined["status"].as_str(), declined["found"].as_bool()), (Some("declined"), Some(false)), "{declined}");
    assert!(events.is_empty());
    // A fresh one: the lead never declines it; the owner does.
    let pending = warn(&mut team, &mut events);
    let error = apply(&mut team, "add-to-phase", &decline(&pending), &Actor::Member("lead".into()), &none, &mut events)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Only you decline an addition to a running phase");
    events.clear();
    let declined = apply(&mut team, "add-to-phase", &decline(&pending), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!((declined["found"].as_bool(), declined["phase"].as_u64()), (Some(true), Some(1)), "{declined}");
    assert!(team.pending_additions.is_empty() && team.task(3).is_none());
    assert_eq!(kinds(&events), ["phase_addition_declined"]);
    // Its phase no longer runs (the team stopped): still declined.
    let pending = warn(&mut team, &mut events);
    team.status = TeamStatus::Stopped;
    let declined = apply(&mut team, "add-to-phase", &decline(&pending), &Actor::Human, &none, &mut events).unwrap();
    assert_eq!(declined["found"], true, "{declined}");
    assert!(team.pending_additions.is_empty());
    // A confirmation of a declined addition is refused.
    team.status = TeamStatus::Running;
    let error = apply(&mut team, "add-to-phase", &json!({"phase":1,"confirm":pending,"confirmation":"add"}), &Actor::Human, &none, &mut events)
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown or expired"), "{error}");
}
