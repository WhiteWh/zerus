use super::{super::test_inspect, *};
use crate::{
    state::listing_fixture as producer,
    teams::ops::{
        tests::{person, TEAM},
        Operation,
    },
};
const FIXTURE: &str = "desktop::teams::confirm::scan::tests::unknown_fixture";

#[test]
fn a_changed_unreadable_binding_closes_the_old_card_and_starts_a_new_hold() {
    let mut h = crate::desktop::teams::tests::bound151_fixture();
    let state = h.temp.path();
    let team = h.store.team(TEAM).unwrap().unwrap();
    let member = h.store.member(TEAM, "m1").unwrap().unwrap();
    let (binding, _, _) = crate::desktop::teams::confirm::policy::tests::sample();
    let mut changed = binding.clone();
    changed.run = "a-different-run".into();
    let mut cards = actor::Runtime::open(state, &team.home).unwrap();
    assert_unreadable_hold_replacement(
        &mut h.store,
        &binding,
        &changed,
        |store, binding, now| {
            apply_observation(
                &mut cards,
                store,
                (&team, &member, binding),
                &Err("The member inspection is unavailable".into()),
                now,
                |_| {},
            )
            .unwrap();
        },
        "The member's session binding changed",
    );
}

#[test]
fn a_same_binding_generation_change_closes_the_old_card_and_starts_a_new_hold() {
    let mut h = crate::desktop::teams::tests::bound151_fixture();
    let team = h.store.team(TEAM).unwrap().unwrap();
    let mut cards = actor::Runtime::open(h.temp.path(), &team.home).unwrap();
    assert_unreadable_hold_replacement(
        &mut h.store,
        1,
        2,
        |store, generation, now| {
            cards
                .test_hold_scoped(
                    store,
                    &team,
                    (
                        "unreadable_confirmation:m1",
                        0,
                        generation,
                        Some("same-binding"),
                    ),
                    ("The member inspection is unavailable", now),
                )
                .unwrap();
        },
        "The decision or generation for this check changed",
    );
}

fn assert_unreadable_hold_replacement<T: Copy>(
    store: &mut Store,
    original: T,
    replacement: T,
    mut observe: impl FnMut(&mut Store, T, i64),
    expected: &str,
) {
    let open = |store: &Store| {
        store
            .cards(TEAM, 200, None)
            .unwrap()
            .rows
            .into_iter()
            .filter(|card| card.kind == "team_actor_blocked" && card.state == "open")
            .map(|card| card.seq)
            .collect::<Vec<_>>()
    };
    let start = 1_000_000;
    observe(store, original, start);
    observe(store, original, start + 180_000);
    let first = open(store);
    assert_eq!(first.len(), 1);
    let rebound = start + 181_000;
    observe(store, replacement, rebound);
    assert!(open(store).is_empty());
    assert_eq!(close_note(store, first[0]), expected);
    observe(store, replacement, rebound + 179_999);
    assert!(open(store).is_empty());
    observe(store, replacement, rebound + 180_000);
    let next = open(store);
    assert_eq!(next.len(), 1);
    assert_ne!(next[0], first[0]);
}

fn close_note(store: &Store, card: u64) -> String {
    let reference = format!("card:{card}");
    store
        .journal(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .find(|row| row.kind == "close_card" && row.refs.contains(&reference))
        .unwrap()
        .text
}

#[test]
fn a_persistently_unknown_end_gets_one_card_and_one_fault_log() {
    scan_fixture(false);
}

#[test]
fn a_readable_observation_closes_the_card_for_an_active_member() {
    scan_fixture(true);
}

fn scan_fixture(recover: bool) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            FIXTURE,
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("TEST_TEAMS_RECOVER", if recover { "yes" } else { "no" })
        .output()
        .unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "run in an isolated child"]
fn unknown_fixture() {
    let mut h = crate::desktop::teams::tests::bound151_fixture();
    let state = h.temp.path().to_owned();
    std::env::set_var("HGS_STATE_DIR", &state);
    let config = Config {
        home: String::new(),
        dir: state.join("config"),
        state: state.clone(),
        selves: vec![],
        peers: vec![],
        base_peers: vec![],
        peers_from_environment: false,
        machines: Default::default(),
        tab: false,
        tab_colors: String::new(),
    };
    let (binding, name) = super::super::native::launch_binding(&config, TEAM, "m1").unwrap();
    // This roster uses a fixed machine identity. Give its private CLI
    // config that identity so inspection runs with the original guard.
    crate::swarm::save_test_node_id(&config, &binding.machine).unwrap();
    test_inspect::save_then_exit(&config, &binding, &name);
    assert_eq!(
        crate::state::dispatch_test_archive_catalog_worker().unwrap(),
        0
    );
    producer::withhold_saved_record(&name, true).unwrap();
    test_inspect::use_cli_inspection();
    let machine = binding.machine;
    let stop = Arc::new(AtomicBool::new(false));
    let mut cards = actor::Runtime::open(&state, &machine).unwrap();
    let mut cursor = Scan::default();
    let mut logs = Vec::new();
    let mut pass = |store: &mut Store, cards: &mut actor::Runtime, cursor: &mut Scan, now| {
        step_at(
            &config,
            ("epoch", &machine),
            store,
            &stop,
            (cursor, cards),
            || now,
            |error| {
                if error.contains(&name) {
                    logs.push(error.to_owned());
                }
            },
        )
        .unwrap();
    };
    let existing = |store: &Store| {
        store
            .cards(TEAM, 200, None)
            .unwrap()
            .rows
            .into_iter()
            .filter(|card| card.kind == "team_actor_blocked" && card.text.contains(&name))
            .collect::<Vec<_>>()
    };
    let start = 1_000_000;
    for millis in [0, 4_500, 9_000, 179_999] {
        pass(&mut h.store, &mut cards, &mut cursor, start + millis);
        assert!(existing(&h.store).is_empty());
    }
    // The original read and its age survive recreation of the service adapter.
    cards = actor::Runtime::open(&state, &machine).unwrap();
    cursor = Scan::default();
    pass(&mut h.store, &mut cards, &mut cursor, start + 180_000);
    pass(&mut h.store, &mut cards, &mut cursor, start + 184_500);
    let first = existing(&h.store);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].state, "open");
    assert!(
        first[0]
            .text
            .contains("ended is unknown (No exact unique exit archive)"),
        "card text: {:?}",
        first[0].text
    );
    assert_eq!(first[0].decisions, ["retry", "acknowledged"]);
    assert!(!state.join("archive-catalog-v2/teams-attempt.json").exists());
    assert!(
        !state.join("approval-audit").exists(),
        "unknown did not reserve an original"
    );
    if std::env::var("TEST_TEAMS_RECOVER").unwrap() == "yes" {
        producer::withhold_saved_record(&name, false).unwrap();
        pass(&mut h.store, &mut cards, &mut cursor, start + 189_000);
        pass(&mut h.store, &mut cards, &mut cursor, start + 193_500);
        assert_eq!(
            h.store.member(TEAM, "m1").unwrap().unwrap().state,
            MemberState::Active
        );
        assert_eq!(
            h.store.card(TEAM, first[0].seq).unwrap().unwrap().state,
            "closed"
        );
        assert_eq!(
            close_note(&h.store, first[0].seq),
            "The service check recovered"
        );
        let ledger = actor::Ledger::open(&state).unwrap();
        assert!(ledger
            .get(TEAM, "gate:unreadable_confirmation:m1")
            .unwrap()
            .is_none());
        assert!(!state.join("approval-audit").exists());
        assert_eq!(logs.len(), 1, "{logs:?}");
        return;
    }

    let mut unrelated = producer::launched_record(
        "codex",
        "codex/example/unrelated",
        &uuid::Uuid::new_v4().to_string(),
        &uuid::Uuid::new_v4().to_string(),
        "different-run",
        "/fixture/worktree",
        "test-account",
    );
    producer::confirmed(&mut unrelated, "different-conversation");
    producer::save(&mut unrelated).unwrap();
    producer::archive_with_catalog_gap(&unrelated).unwrap();
    pass(&mut h.store, &mut cards, &mut cursor, start + 189_000);
    pass(&mut h.store, &mut cards, &mut cursor, start + 193_500);
    assert_eq!(existing(&h.store)[0].seq, first[0].seq);
    pass(&mut h.store, &mut cards, &mut cursor, start + 369_000);
    let replaced = existing(&h.store);
    assert!(replaced
        .iter()
        .any(|card| card.seq == first[0].seq && card.state == "closed"));
    assert_eq!(
        replaced.iter().filter(|card| card.state == "open").count(),
        1
    );

    h.run(
        &person(),
        Operation::StopMember {
            member: "m1".into(),
        },
    )
    .unwrap();
    // The bounded roster cursor can spend a pass reaching the end of the
    // team page before it starts a fresh sweep.
    pass(&mut h.store, &mut cards, &mut cursor, start + 373_500);
    pass(&mut h.store, &mut cards, &mut cursor, start + 378_000);
    assert!(existing(&h.store).iter().all(|card| card.state == "closed"));
    assert_eq!(logs.len(), 2, "{logs:?}");
    assert!(logs[0].contains("No exact unique exit archive"));
    assert!(logs[1].contains("Archive catalog coverage is incomplete"));
    producer::withhold_saved_record(&name, false).unwrap();
}
