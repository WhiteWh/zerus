use super::*;
use super::support::*;

/// N5: a finished team's inbox still carries the owner's decision on the
/// lead's proposal (spec 4), so the lead hears the answer.
#[test]
fn a_done_team_reports_the_owners_proposal_decision_in_its_inbox() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open(true).unwrap();
        let mut team = running_team();
        team.status = TeamStatus::Done;
        team.proposals.push(json!({"id":"p1","kind":"next-goal","by":"Lead","at":10.0,"state":"accepted",
            "decided_at":20.0,"note":"Go on","goal":"v2"}));
        let id = team.id.clone();
        store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
        let value = run("inbox", &json!({"team":id,"by":"human"}), false).unwrap();
        let items = value["items"].as_array().unwrap();
        let item = items.iter().find(|item| item["kind"] == "proposal_decided").expect("the decision is missing");
        assert!(
            item["summary"].as_str().unwrap_or_default().contains("accepted your next-goal proposal p1: Go on"),
            "{item}"
        );
        // A running team with the same proposal lists nothing stale twice.
        let mut running = running_team();
        running.proposals.push(json!({"id":"p2","kind":"next-goal","by":"Lead","at":10.0,"state":"rejected","decided_at":20.0,"note":"No"}));
        assert_eq!(super::super::inbox::items(&running).iter().filter(|item| item.kind == "proposal_decided").count(), 1);
    });
}

/// N3: a finished team starts one next goal, however many requests ask for
/// it; a replay of the first answers its receipt.
#[test]
fn a_finished_team_starts_only_one_next_goal() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        super::live_sessions_override(Some(Vec::new()));
        let _worker = hold_worker_lock();
        let mut store = Store::open(true).unwrap();
        let mut team = running_team();
        team.status = TeamStatus::Done;
        let id = team.id.clone();
        store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
        let first = run("next-goal", &json!({"team":id,"goal":"v2","by":"human","request_id":"22222222-2222-4222-8222-222222222222"}), false).unwrap();
        assert_eq!(first["status"], "planned", "{first}");
        let error = run("next-goal", &json!({"team":id,"goal":"v3","by":"human","request_id":"33333333-3333-4333-8333-333333333333"}), false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("already started"), "{error}");
        let again = run("next-goal", &json!({"team":id,"goal":"v2","by":"human","request_id":"22222222-2222-4222-8222-222222222222"}), false).unwrap();
        assert_eq!(again["team"], first["team"]);
    });
    super::live_sessions_override(None);
}

/// N3: an accepted proposal's change runs under the proposal's own request
/// id, so a decision interrupted between its writes replays instead of
/// running the change twice.
#[test]
fn an_accepted_proposal_replays_instead_of_running_twice() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open(true).unwrap();
        let mut team = running_team();
        team.members.push(Member { id: "ops".into(), role: "Ops".into(), agent: "codex".into(), ..Default::default() });
        team.proposals.push(json!({"id":"p1","kind":"remove-member","by":"Lead","at":10.0,"state":"open","reason":"bye",
            "member":"ops","operation_request":"44444444-4444-4444-8444-444444444444"}));
        let id = team.id.clone();
        store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
        let decide = |store: &mut Store| {
            super::decide_proposal_in(
                store,
                &json!({"request_id":"55555555-5555-4555-8555-555555555555","team":id,"proposal":"p1","accept":true,"reason":"bye"}),
                &Actor::Human,
            )
            .unwrap()
        };
        let first = decide(&mut store);
        assert_eq!(first["status"], "accepted", "{first}");
        let removals = |store: &Store| {
            let events = store.events(&id, None, 50).unwrap();
            events["items"].as_array().unwrap().iter().filter(|event| event["kind"] == "member_removed").count()
        };
        assert_eq!(removals(&store), 1);
        // The change went through but the acceptance was lost (a crash
        // between the two writes): the proposal is open again.
        store
            .mutate(None, &id, None, |team, _| {
                if let Some(proposal) = team.proposals.iter_mut().find(|proposal| proposal["id"] == "p1") {
                    proposal["state"] = json!("open");
                }
                Ok(Value::Null)
            })
            .unwrap();
        let again = decide(&mut store);
        assert_eq!(again["status"], "accepted", "{again}");
        assert_eq!(removals(&store), 1, "the change ran twice");
    });
}

/// N4: an accepted add-member proposal runs the same adoption checks as the
/// owner's own change (C8's owner_change), not a bare ops::apply.
#[test]
fn an_accepted_add_member_runs_the_adoption_checks() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        super::live_sessions_override(Some(Vec::new()));
        let mut store = Store::open(true).unwrap();
        let mut team = running_team();
        team.proposals.push(json!({"id":"p1","kind":"add-member","by":"Lead","at":10.0,"state":"open","reason":"needs help",
            "member":{"id":"ops","role":"Ops","agent":"codex","session":{"name":"codex/x/ops","host":""}},
            "operation_request":"44444444-4444-4444-8444-444444444444"}));
        let id = team.id.clone();
        store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
        let error = super::decide_proposal_in(
            &mut store,
            &json!({"request_id":"55555555-5555-4555-8555-555555555555","team":id,"proposal":"p1","accept":true,"reason":"ok"}),
            &Actor::Human,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("is not running on this computer"), "{error}");
        let saved = store.load(&id).unwrap().unwrap();
        assert_eq!(saved.proposals[0]["state"], "open", "the proposal was accepted without the check");
        assert!(saved.member("ops").is_none());
    });
    super::live_sessions_override(None);
}

#[test]
fn a_done_team_starts_the_next_goal_with_the_same_sessions() {
    use crate::state::teams::model::{Member, SessionRef, Team, TeamStatus};
    use crate::state::teams::store::Store;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_at(&dir.path().join("teams.db"), true).unwrap();
    let old_id = "33333333-3333-4333-8333-333333333333";
    let mut done = Team {
        id: old_id.into(),
        name: "T".into(),
        lead: "lead".into(),
        status: TeamStatus::Done,
        folder: dir.path().display().to_string(),
        goal: "one".into(),
        ..Default::default()
    };
    done.members.push(Member { id: "lead".into(), role: "Lead".into(), agent: "claude".into(), ..Default::default() });
    done.members.push(Member {
        id: "dev".into(),
        role: "Backend".into(),
        agent: "claude".into(),
        session: Some(SessionRef { name: "claude/x/dev".into(), ..Default::default() }),
        ..Default::default()
    });
    store.create("44444444-4444-4444-8444-444444444444", &json!({"id":old_id}), done, Vec::new()).unwrap();
    // N8: the live sessions are read once; this test's listing has dev's.
    super::live_sessions_override(Some(vec![crate::state::teams::dispatcher::Seen {
        name: "claude/x/dev".into(),
        run_id: "r9".into(),
        conversation_id: "c9".into(),
        live: true,
        ..Default::default()
    }]));
    let value = super::next_goal_as(
        &mut store,
        "55555555-5555-4555-8555-555555555555",
        &json!({"request_id":"55555555-5555-4555-8555-555555555555","team":old_id,"goal":"two"}),
        &Actor::Human,
    )
    .unwrap();
    super::live_sessions_override(None);
    let next_id = value["team"].as_str().unwrap().to_owned();
    let next = store.load(&next_id).unwrap().unwrap();
    assert_eq!(next.status, TeamStatus::Planning);
    assert_eq!((next.previous.as_str(), next.goal.as_str()), (old_id, "two"));
    let dev = next.member("dev").unwrap();
    assert_eq!(dev.session.as_ref().map(|session| (session.name.as_str(), session.run_id.as_str())), Some(("claude/x/dev", "r9")));
    assert_eq!(dev.state, crate::state::teams::model::MemberState::Active);
    assert_eq!(store.load(old_id).unwrap().unwrap().next, next_id);
}

#[test]
fn a_lead_starts_the_next_goal_only_after_an_accepted_proposal() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_at(&dir.path().join("teams.sqlite3"), true).unwrap();
    let mut done = Team { id: "done".into(), name: "Done".into(), lead: "lead".into(), status: TeamStatus::Done, goal: "one".into(), ..Default::default() };
    done.members.push(Member { id: "lead".into(), role: "Lead".into(), agent: "claude".into(), ..Default::default() });
    store.create("66666666-6666-4666-8666-666666666666", &json!({}), done.clone(), Vec::new()).unwrap();
    // N1: the lead alone cannot start the next goal.
    let error = super::next_goal_as(
        &mut store,
        "77777777-7777-4777-8777-777777777777",
        &json!({"request_id":"77777777-7777-4777-8777-777777777777","team":"done","goal":"two"}),
        &Actor::Member("lead".into()),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("accepted next-goal proposal"), "{error}");
    // With the owner's accepted proposal it can.
    let mut with_proposal = done.clone();
    with_proposal.id = "done2".into();
    with_proposal.proposals.push(json!({"id":"p1","kind":"next-goal","state":"accepted","goal":"two"}));
    store.create("88888888-8888-4888-8888-888888888888", &json!({}), with_proposal, Vec::new()).unwrap();
    let value = super::next_goal_as(
        &mut store,
        "99999999-9999-4999-8999-999999999999",
        &json!({"request_id":"99999999-9999-4999-8999-999999999999","team":"done2","goal":"other"}),
        &Actor::Member("lead".into()),
    )
    .unwrap();
    assert_eq!(value["status"], "planned");
    // N12: the accepted proposal's goal is used, and the done team cannot
    // start a second next goal.
    let next = store.load(value["team"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(next.goal, "two");
    let error = super::next_goal_as(
        &mut store,
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        &json!({"request_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","team":"done2","goal":"three"}),
        &Actor::Member("lead".into()),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("already started"), "{error}");
}

/// U57: the polled list carries the open proposals with their change and
/// hash for the lead's cards, never a decided one or the replay request id.
#[test]
fn the_team_list_carries_the_open_proposals_for_their_cards() {
    let mut team = running_team();
    let lead = Actor::Member("lead".into());
    // #740: a proposal is checked when it is made: #2 and phase 2 exist.
    ops::apply(&mut team, "extend", &json!({"phase_title":"Next","tasks":[{"id":2,"title":"Later","assignee":"dev"}]}), &lead,
               &BTreeSet::new(), &mut Vec::new())
        .unwrap();
    for input in [
        json!({"kind":"cancel-tasks","reason":"drop","tasks":[2]}),
        json!({"kind":"discard-phase","reason":"nothing started","phase":2}),
    ] {
        ops::apply(&mut team, "propose", &input, &lead, &BTreeSet::new(), &mut Vec::new()).unwrap();
    }
    team.proposals[1]["state"] = json!("rejected");
    let listed = summary(&team, false);
    let proposals = listed["proposals"].as_array().unwrap();
    assert_eq!(proposals.len(), 1, "{listed}");
    assert_eq!(proposals[0]["id"], "p1");
    assert_eq!(proposals[0]["change"]["tasks"], json!([2]));
    assert_eq!(proposals[0]["hash"], team.proposals[0]["hash"]);
    assert!(proposals[0].get("operation_request").is_none(), "{}", proposals[0]);
}

/// U57: Allow applies exactly the proposed change once, bound to the id and
/// hash; a changed card is refused; Deny records the reason for the lead.
#[test]
fn an_allow_applies_the_proposed_change_once_and_a_changed_card_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
    let mut team = running_team();
    ops::apply(
        &mut team,
        "propose",
        &json!({"kind":"add-member","reason":"needs help",
            "members":[{"id":"x","role":"Ops","agent":"claude"},{"id":"y","role":"Ops","agent":"claude"}]}),
        &Actor::Member("lead".into()),
        &BTreeSet::new(),
        &mut Vec::new(),
    )
    .unwrap();
    let hash = team.proposals[0]["hash"].as_str().unwrap().to_owned();
    store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
    let error = decide_proposal_in(
        &mut store,
        &json!({"team":"t","proposal":"p1","accept":true,"by":"human","expected_hash":"bad"}),
        &Actor::Human,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("changed"), "{error}");
    let receipt = decide_proposal_in(&mut store, &json!({"team":"t","proposal":"p1","accept":true,"by":"human","expected_hash":hash}), &Actor::Human).unwrap();
    assert_eq!(receipt["status"], "accepted");
    let team = store.load("t").unwrap().unwrap();
    assert!(team.member("x").is_some() && team.member("y").is_some());
    // A replay returns the stored state; no member is added twice.
    let replay = decide_proposal_in(&mut store, &json!({"team":"t","proposal":"p1","accept":true,"by":"human","expected_hash":hash}), &Actor::Human).unwrap();
    assert_eq!(replay["status"], "accepted");
    assert_eq!(store.load("t").unwrap().unwrap().members.iter().filter(|member| member.id == "x").count(), 1);
    // Deny records its reason for the lead and runs nothing.
    store
        .mutate(None, "t", None, |team, events| {
            // #740: a proposal names work that exists (it is checked when made).
            ops::apply(team, "propose", &json!({"kind":"cancel-tasks","reason":"drop","tasks":[1]}), &Actor::Member("lead".into()), &BTreeSet::new(), events)
        })
        .unwrap();
    let team = store.load("t").unwrap().unwrap();
    let hash = team.proposals[1]["hash"].as_str().unwrap().to_owned();
    let before = team.task(1).unwrap().status;
    let denied = decide_proposal_in(
        &mut store,
        &json!({"team":"t","proposal":"p2","accept":false,"reason":"not now","expected_hash":hash}),
        &Actor::Human,
    )
    .unwrap();
    assert_eq!(denied["status"], "rejected");
    let team = store.load("t").unwrap().unwrap();
    assert_eq!(team.proposals[1]["note"], "not now");
    assert_eq!(team.task(1).unwrap().status, before, "a Deny runs nothing");
}

/// U57: Allow on an add-to-phase card for the running phase adds the tasks
/// at once: the Allow is the confirmation the CLI route asks for after its
/// warning (owner, 2026-10-30: the card was accepted and nothing was added).
#[test]
fn an_allowed_addition_to_the_running_phase_adds_its_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
    let mut team = running_team();
    let input = json!({"kind":"add-to-phase","reason":"a crash to hunt","phase":1,
        "tasks":[{"id":2,"title":"Find the crash","assignee":"dev","criteria":["Fixed"]}]});
    ops::apply(&mut team, "propose", &input, &Actor::Member("lead".into()), &BTreeSet::new(), &mut Vec::new()).unwrap();
    let hash = team.proposals[0]["hash"].as_str().unwrap().to_owned();
    store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
    let receipt = decide_proposal_in(
        &mut store,
        &json!({"team":"t","proposal":"p1","accept":true,"by":"human","expected_hash":hash}),
        &Actor::Human,
    )
    .unwrap();
    assert_eq!(receipt["status"], "accepted", "{receipt}");
    let team = store.load("t").unwrap().unwrap();
    let added = team.task(2).expect("the allowed task is in the plan");
    assert_eq!((added.phase, added.title.as_str()), (1, "Find the crash"));
}

/// U29: an Allow whose confirmation fails after its warning was stored
/// leaves no addition waiting that nobody can settle; the error is the
/// confirmation's.
#[test]
fn a_failed_allow_confirmation_leaves_no_addition_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
    store.create("11111111-1111-4111-8111-111111111111", &json!({}), running_team(), vec![]).unwrap();
    let warned = store
        .mutate(None, "t", None, |team, events| {
            let input = json!({"phase":1,"tasks":[{"id":2,"title":"Find the crash","assignee":"dev"}]});
            ops::apply(team, "add-to-phase", &input, &Actor::Human, &BTreeSet::new(), events)
        })
        .unwrap();
    let pending = warned["pending"].as_str().unwrap().to_owned();
    assert_eq!(store.load("t").unwrap().unwrap().pending_additions.len(), 1);
    let confirm = json!({"team":"t","request_id":"22222222-2222-4222-8222-222222222222","by":"human","phase":1,
        "confirm":pending,"confirmation":"sure"});
    let error = super::confirm_allowed_addition(&mut store, "t", &confirm).unwrap_err().to_string();
    assert!(error.contains("Type \"add\""), "{error}");
    let team = store.load("t").unwrap().unwrap();
    assert!(team.pending_additions.is_empty(), "{:?}", team.pending_additions);
    assert!(team.task(2).is_none());
    assert!(store.events("t", None, 50).unwrap().to_string().contains("phase_addition_declined"));
}

/// U59: the desktop's switch dialog decides through decide-proposal; its
/// Allow switches the plan mode once, and a Deny keeps it and tells the lead.
#[test]
fn the_switch_dialog_switches_the_plan_mode_through_decide_proposal() {
    use super::super::model::PlanMode;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
    let mut team = running_team();
    let lead = Actor::Member("lead".into());
    for reason in ["short requests", "a stream"] {
        let input = json!({"kind":"switch-plan-mode","mode":"free","reason":reason});
        ops::apply(&mut team, "propose", &input, &lead, &BTreeSet::new(), &mut Vec::new()).unwrap();
    }
    let first = team.proposals[0]["hash"].as_str().unwrap().to_owned();
    let second = team.proposals[1]["hash"].as_str().unwrap().to_owned();
    store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
    // A guessed field never decides: no "accept" is an error, not a rejection.
    let error = decide_proposal_in(&mut store, &json!({"team":"t","proposal":"p1","decision":"allow","hash":first}), &Actor::Human)
        .unwrap_err()
        .to_string();
    assert!(error.contains("accept") && error.contains("nothing was decided"), "{error}");
    assert_eq!(store.load("t").unwrap().unwrap().proposals[0]["state"], "open");
    let denied = decide_proposal_in(
        &mut store,
        &json!({"team":"t","proposal":"p1","accept":false,"reason":"not yet","expected_hash":first}),
        &Actor::Human,
    )
    .unwrap();
    assert_eq!(denied["status"], "rejected");
    assert_eq!(store.load("t").unwrap().unwrap().plan_mode, PlanMode::Standard);
    let feed = store.events("t", None, 50).unwrap().to_string();
    assert!(feed.contains("plan_mode_denied") && feed.contains("not yet"), "{feed}");
    let receipt = decide_proposal_in(
        &mut store,
        &json!({"team":"t","proposal":"p2","accept":true,"by":"human","hash":second}),
        &Actor::Human,
    )
    .unwrap();
    assert_eq!(receipt["status"], "accepted", "{receipt}");
    assert_eq!(receipt["result"]["plan_mode"], "free");
    assert_eq!(store.load("t").unwrap().unwrap().plan_mode, PlanMode::Free);
    assert!(store.events("t", None, 50).unwrap().to_string().contains("plan_mode_switched"));
}

#[test]
fn an_accepted_swap_proposal_runs_with_its_fields() {
    // C8: the spec's shape (member id plus section 5 fields) is kept by
    // propose and run by the owner's decision without a panic.
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
    let mut team = running_team();
    let proposal = json!({"kind":"swap-member","member":"dev","agent":"claude","model":"opus","reason":"Stuck three times"});
    ops::apply(&mut team, "propose", &proposal, &Actor::Member("lead".into()), &BTreeSet::new(), &mut Vec::new()).unwrap();
    assert_eq!(team.proposals[0]["member"], json!({"id":"dev","agent":"claude","model":"opus"}));
    let hash = team.proposals[0]["hash"].as_str().unwrap().to_owned();
    store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
    let decide = json!({"team":"t","proposal":"p1","accept":true,"by":"human","expected_hash":hash});
    let error = decide_proposal_in(&mut store, &decide, &Actor::Member("lead".into())).unwrap_err().to_string();
    assert!(error.contains("Only you decide"), "{error}");
    let receipt = decide_proposal_in(&mut store, &decide, &Actor::Human).unwrap();
    assert_eq!((receipt["status"].as_str(), receipt["kind"].as_str()), (Some("accepted"), Some("swap-member")));
    let team = store.load("t").unwrap().unwrap();
    let dev = team.member("dev").unwrap();
    assert_eq!((dev.agent.as_str(), dev.model.as_str(), dev.session.is_none()), ("claude", "opus", true));
    assert_eq!(team.proposals[0]["state"], "accepted");
    assert!(team.proposals[0]["decided_at"].as_f64().is_some());
    // An old proposal stored with a bare member id runs as well.
    assert_eq!(proposal_input("t", "swap-member", &json!("dev"), "11111111-1111-4111-8111-111111111111").unwrap()["member"], "dev");
}

#[test]
fn an_accepted_next_goal_runs_as_the_owner() {
    // N2: the decision does not re-derive its caller from the input.
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
    let mut team = running_team();
    ops::apply(&mut team, "propose", &json!({"kind":"next-goal","goal":"Ship v2","reason":"Done here"}), &Actor::Member("lead".into()), &BTreeSet::new(), &mut Vec::new()).unwrap();
    let hash = team.proposals[0]["hash"].as_str().unwrap().to_owned();
    team.status = TeamStatus::Done;
    store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
    let receipt = decide_proposal_in(&mut store, &json!({"team":"t","proposal":"p1","accept":true,"by":"human","expected_hash":hash}), &Actor::Human).unwrap();
    let next = receipt["result"]["team"].as_str().unwrap().to_owned();
    assert_eq!(store.load(&next).unwrap().unwrap().goal, "Ship v2");
    assert_eq!(store.load("t").unwrap().unwrap().next, next);
}

/// #754 (owner, 2026-10-31: "I press Allow and nothing happens"): p11 named
/// #747 and #748, already taken, and Allow failed every time. Added tasks take
/// the team's next free numbers when Allow applies them, their dependencies
/// with them; an earlier Allow that stopped after its warning never leaves a
/// stale addition behind; a proposal Allow could never apply (a number given
/// twice, a dependency on an unknown task) is refused when the lead makes it,
/// and a dependency named twice is kept once.
#[test]
fn allowed_tasks_take_the_next_free_numbers_and_a_retry_never_meets_a_stale_warning() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
    let mut team = running_team();
    let lead = Actor::Member("lead".into());
    // #1 is taken: the proposal names it, and #7 depends on it (twice).
    let input = json!({"kind":"add-to-phase","reason":"two follow-ups","phase":1,
        "tasks":[{"id":1,"title":"First","assignee":"dev"},{"id":7,"title":"Second","assignee":"dev","deps":[{"task":1},{"task":1}]}]});
    ops::apply(&mut team, "propose", &input, &lead, &BTreeSet::new(), &mut Vec::new()).unwrap();
    for (tasks, words) in [
        (json!([{"id":5,"title":"A","assignee":"dev"},{"id":5,"title":"B","assignee":"dev"}]), "Task #5 is given twice among the new tasks"),
        (json!([{"id":5,"title":"A","assignee":"dev","deps":[{"task":42}]}]), "Task #5 depends on #42, which is neither in the plan nor among the new tasks"),
    ] {
        let bad = json!({"kind":"add-to-phase","reason":"bad","phase":1,"tasks":tasks});
        let error = format!("{:#}", ops::apply(&mut team, "propose", &bad, &lead, &BTreeSet::new(), &mut Vec::new()).unwrap_err());
        assert!(error.contains("Allow could not apply") && error.contains(words), "{error}");
    }
    assert_eq!(team.proposals.len(), 1, "only the good proposal is made");
    let hash = team.proposals[0]["hash"].as_str().unwrap().to_owned();
    let operation = team.proposals[0]["operation_request"].as_str().unwrap().to_owned();
    store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
    // An earlier Allow's warning, stored under the proposal's request id with
    // an addition that no longer exists (it expired).
    let stale = json!({"team":"t","request_id":operation,"by":"human","phase":1,"tasks":input["tasks"]});
    store
        .mutate(Some((&operation, &stale)), "t", None, |_, _| Ok(json!({"status":"warning","pending":"gone-long-ago"})))
        .unwrap();
    let receipt = decide_proposal_in(
        &mut store,
        &json!({"team":"t","proposal":"p1","accept":true,"by":"human","expected_hash":hash}),
        &Actor::Human,
    )
    .unwrap();
    assert_eq!(receipt["status"], "accepted", "{receipt}");
    let team = store.load("t").unwrap().unwrap();
    let first = team.task(2).expect("#1 joined as #2");
    let second = team.task(3).expect("#7 joined as #3");
    assert_eq!((first.title.as_str(), first.phase), ("First", 1));
    assert_eq!(second.title, "Second");
    assert_eq!(second.deps.iter().map(|dep| dep.task).collect::<Vec<_>>(), vec![2], "the dependency moved with its task");
    assert_eq!(team.task(1).unwrap().title, "API", "the taken task is untouched");
    // The decision is not applied twice when asked again.
    let again = decide_proposal_in(
        &mut store,
        &json!({"team":"t","proposal":"p1","accept":true,"by":"human","expected_hash":hash}),
        &Actor::Human,
    )
    .unwrap();
    assert_eq!(again["status"], "accepted", "{again}");
    assert_eq!(store.load("t").unwrap().unwrap().tasks.len(), 3);
}

/// #754: the lead's extend takes the next free numbers too, with the
/// dependencies on its own tasks.
#[test]
fn an_extend_with_taken_numbers_takes_the_next_free_ones() {
    let mut team = running_team();
    let receipt = ops::apply(
        &mut team,
        "extend",
        &json!({"tasks":[{"id":1,"title":"Docs","assignee":"dev"},{"id":9,"title":"Review the docs","assignee":"dev","deps":[{"task":1}]}]}),
        &Actor::Member("lead".into()),
        &BTreeSet::new(),
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(receipt["renumbered"], json!({"1":2,"9":3}), "{receipt}");
    assert_eq!(team.task(2).unwrap().title, "Docs");
    assert_eq!(team.task(3).unwrap().deps.iter().map(|dep| dep.task).collect::<Vec<_>>(), vec![2]);
}
