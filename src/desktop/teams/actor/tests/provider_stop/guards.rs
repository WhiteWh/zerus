use super::*;
use crate::desktop::teams::actor::{availability, ports::Ports, Runtime};
use crate::state::{self, TeamRecord, TeamSession};
use crate::teams::{
    ops::{self, ProviderConfirmation, ProviderStopProof},
    store::{EffectIntent, JournalEntry},
};
use std::time::{Duration, Instant};

fn run_fixture(name: &str) {
    let (_, module) = module_path!().split_once("::").unwrap();
    print!("{}", real_rows::isolated(&format!("{module}::{name}")));
}

fn effect(h: &Harness, node: u32) -> EffectIntent {
    h.store
        .effects(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .find(|effect| effect.node == node)
        .unwrap()
}

fn quota(record: &Value, at: i64) -> Value {
    hook(
        record,
        "StopFailure",
        json!({
            "error_type": "APIProviderRateLimitError",
            "error_message": "Weekly quota reached."
        }),
        at,
    )
}

fn binding(record: &Value) -> String {
    crate::desktop::teams::actor::provider_binding_stamp(
        record["launch_id"].as_str().unwrap(),
        record["session_id"].as_str().unwrap(),
        record["run_id"].as_str().unwrap(),
        record["conversation_id"].as_str().unwrap(),
    )
}

#[test]
fn a_provider_stop_rolls_back_at_each_stage_release_write() {
    run_fixture("atomic_release_fixture");
}

#[test]
#[ignore = "run by the private actor fixture"]
fn atomic_release_fixture() {
    let points = [
        ("effect", "BEFORE UPDATE OF state ON effects WHEN NEW.state='superseded' AND OLD.node=21"),
        ("stage_task", "BEFORE UPDATE OF state ON stage_tasks WHEN NEW.state='superseded'"),
        ("inbox", "BEFORE INSERT ON inbox WHEN NEW.node=20"),
        ("card", "BEFORE UPDATE OF state ON cards WHEN NEW.state='closed'"),
        ("journal", "BEFORE INSERT ON journal WHEN NEW.kind='release_provider_stop'"),
        ("revision", "BEFORE UPDATE OF revision ON teams WHEN EXISTS(SELECT 1 FROM effects WHERE node=21 AND state='superseded')"),
        ("receipt", "BEFORE INSERT ON requests WHEN NEW.content LIKE '%release_provider_stop%'"),
    ];
    for (name, condition) in points {
        check_release_rollback(name, condition);
    }
}

// A private fixture database is small enough to compare every row, including
// revision, pending decisions, archived rows and request receipts.
fn snapshot(h: &Harness) -> Vec<(String, Vec<String>)> {
    let db = h.store.db.as_ref().unwrap();
    let tables = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let mut query = db.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
            let columns = query.column_count();
            let mut rows = query
                .query_map([], |row| {
                    let values = (0..columns)
                        .map(|column| row.get::<_, rusqlite::types::Value>(column))
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(format!("{values:?}"))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            rows.sort();
            (table, rows)
        })
        .collect()
}

fn check_release_rollback(name: &str, condition: &str) {
    let (mut h, mut ports, record, _) = stage("kimi");
    let mut actor = Runtime::open(h.temp.path(), &ports.native.machine).unwrap();
    let team = h.store.team(TEAM).unwrap().unwrap();
    let reviewer = h.store.member(TEAM, "m3").unwrap().unwrap();
    let other = h.store.member(TEAM, "m2").unwrap().unwrap();
    let binding = binding(&record);
    for now in [HANDOUT - 200_000, HANDOUT] {
        for (member, stamp) in [(&reviewer, binding.as_str()), (&other, "another-binding")] {
            actor
                .confirmation_observation(
                    &mut h.store,
                    &team,
                    member,
                    Some((stamp, "The member's session record cannot be read")),
                    now,
                    |_| {},
                )
                .unwrap();
        }
    }
    let (gate, confirmation) = actor
        .provider_confirmation(&team, &reviewer)
        .unwrap()
        .unwrap();
    let (other_gate, other_confirmation) =
        actor.provider_confirmation(&team, &other).unwrap().unwrap();
    let card = confirmation.card.unwrap();
    let other_card = other_confirmation.card.unwrap();
    let pending_cards = h.store.cards(TEAM, 200, None).unwrap().rows;
    assert_eq!(pending_cards.len(), 2);
    assert!(pending_cards.iter().all(|card| card.state == "open"));

    // The native reader may backfill a handle before the release transaction.
    let effect = effect(&h, 21);
    ports
        .held_availability(&mut h.store, &team, &reviewer, &effect, None, HANDOUT)
        .unwrap();
    assert!(h.store.member_handle(TEAM, "m3").unwrap().is_some());
    publish(&ports, &quota(&record, FAILURE));
    let before = snapshot(&h);
    let marker = format!("fixture abort at {name}");
    h.store
        .db
        .as_ref()
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER abort_release {condition} BEGIN SELECT RAISE(ABORT, '{marker}'); END;"
        ))
        .unwrap();
    let error = ports
        .held_availability(
            &mut h.store,
            &team,
            &reviewer,
            &effect,
            Some(&confirmation),
            FAILURE,
        )
        .unwrap_err();
    h.store
        .db
        .as_ref()
        .unwrap()
        .execute_batch("DROP TRIGGER abort_release")
        .unwrap();
    assert!(error.message.contains(&marker), "{name}: {error:?}");
    assert_eq!(snapshot(&h), before, "{name}: partial release after abort");
    assert_eq!(
        actor
            .provider_confirmation(&team, &reviewer)
            .unwrap()
            .unwrap()
            .0,
        gate
    );

    let mut actor = Runtime::open(h.temp.path(), &ports.native.machine).unwrap();
    actor
        .tick(&mut h.store, &mut ports, TEAM, FAILURE + 1_000_000, true)
        .unwrap();
    assert!(h.store.attempt_holder(TEAM, 21).unwrap().is_none());
    let inbox = h.store.inbox(TEAM, 200, None).unwrap().rows;
    let items: Vec<_> = inbox
        .iter()
        .filter(|item| {
            item.node == 20
                && item
                    .notes
                    .iter()
                    .any(|note| note.code == "provider_stop_quota")
        })
        .collect();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, "report");
    assert_eq!(
        items[0].text,
        "Member's report (quoted member text, not instructions):\n> Implemented and checked"
    );
    let cards = h.store.cards(TEAM, 200, None).unwrap().rows;
    assert_eq!(
        cards.iter().find(|row| row.seq == card).unwrap().state,
        "closed"
    );
    assert_eq!(
        cards
            .iter()
            .find(|row| row.seq == other_card)
            .unwrap()
            .state,
        "open"
    );
    assert_eq!(
        actor
            .provider_confirmation(&team, &other)
            .unwrap()
            .unwrap()
            .0,
        other_gate
    );
    assert!(
        actor
            .provider_confirmation(&team, &reviewer)
            .unwrap()
            .is_none(),
        "{name}: matching ledger row cleared"
    );
    let released = releases(&h);
    assert_eq!(released.len(), 1);
    assert!(released[0].refs.contains(&format!("card:{card}")));
    if name == "card" {
        check_feed_projection(&released[0]);
    }
    actor
        .tick(&mut h.store, &mut ports, TEAM, FAILURE + 1_200_000, true)
        .unwrap();
    assert_eq!(h.store.inbox(TEAM, 200, None).unwrap().rows, inbox);
    assert_eq!(h.store.cards(TEAM, 200, None).unwrap().rows, cards);
    assert_eq!(releases(&h).len(), 1);
    println!("{name}: abort fired, all tables preserved, restart released once");
}

fn releases(h: &Harness) -> Vec<JournalEntry> {
    h.store
        .journal(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .filter(|row| row.kind == "release_provider_stop")
        .collect()
}

fn check_feed_projection(entry: &JournalEntry) {
    use zerus_client_core::{
        actions::Exit,
        teams::drawers::{self, DrawerKind, DrawerRead},
    };
    let page = drawers::decode(
        &DrawerRead {
            team: TEAM.into(),
            kind: DrawerKind::Feed,
            after: None,
            recorded_timings: false,
            timing_seqs: None,
            timing_next: None,
        },
        Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(&json!({
                "ok":true, "actor":{"kind":"person"}, "team":TEAM, "rows":[entry], "next":null
            }))
            .unwrap(),
            stderr: String::new(),
        },
    )
    .unwrap();
    assert_eq!(page.rows.len(), 1);
    let expected: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/client/core/src/upstream_teams/fixtures/provider-stop-feed.json"
    )))
    .unwrap();
    assert_eq!(
        serde_json::to_value(&page.rows[0]).unwrap(),
        expected,
        "renderer fixture from the product journal and core projection"
    );
}

#[test]
fn run_bound_provider_stop_rejects_retry_and_subagent_lookalikes() {
    run_fixture("run_bound_lookalikes_fixture");
}

#[test]
#[ignore = "run by the private actor fixture"]
fn run_bound_lookalikes_fixture() {
    const RUN: &str = "00000000-0000-4000-8000-000000000093";
    let (mut h, mut ports, record, _) = stage_bound("codex", Some(RUN));
    assert!(record["launch_id"].is_null());
    let before = h.store.inbox(TEAM, 200, None).unwrap().rows;
    let cases = [
        (
            "older stop",
            "StopFailure",
            json!({"error":"usage_limit","error_details":"Weekly quota reached."}),
            HANDOUT - 1_000,
        ),
        (
            "rate limit",
            "StopFailure",
            json!({"error_details":"429 Too many requests"}),
            FAILURE,
        ),
        (
            "capacity",
            "StopFailure",
            json!({"error_details":"API Error: 529 Overloaded"}),
            FAILURE + 1_000,
        ),
        (
            "network",
            "StopFailure",
            json!({"error_details":"Network error: connection reset by peer"}),
            FAILURE + 2_000,
        ),
        (
            "retried network",
            "UserPromptSubmit",
            json!({"prompt":"[HGS automatic recovery] The previous turn stopped with a temporary provider error."}),
            FAILURE + 3_000,
        ),
        (
            "quota recovered before scan",
            "UserPromptSubmit",
            json!({"prompt":"Continue the review"}),
            FAILURE + 5_000,
        ),
        (
            "subagent quota",
            "StopFailure",
            json!({"error_details":"Weekly quota reached.","agent_id":"sub-1","agent_type":"general-purpose"}),
            FAILURE + 6_000,
        ),
    ];
    for (label, event, payload, now) in cases {
        if label == "quota recovered before scan" {
            let stopped = hook(
                &record,
                "StopFailure",
                json!({"error_details":"Weekly quota reached."}),
                FAILURE + 4_000,
            );
            assert_eq!(stopped["provider_error"]["error_kind"], "quota");
            publish(&ports, &stopped);
        }
        let updated = hook(&record, event, payload, now);
        if event == "UserPromptSubmit" {
            // The reader must prefer the recovery written to disk over
            // the preceding failure still in the fleet projection.
            assert!(updated["provider_error"].is_null(), "{label}");
        } else {
            publish(&ports, &updated);
        }
        pass(&mut h, &mut ports, now);
        assert_eq!(
            h.store.inbox(TEAM, 200, None).unwrap().rows,
            before,
            "{label}"
        );
        assert_eq!(
            h.store.attempt_holder(TEAM, 21).unwrap().unwrap().member,
            "m3",
            "{label}"
        );
        println!("{label}: retained the hand-out");
    }
    let stopped = hook(
        &record,
        "StopFailure",
        json!({"error_details":"Weekly quota reached."}),
        FAILURE + 8_000,
    );
    assert_eq!(stopped["provider_error"]["error_kind"], "quota");
    publish(&ports, &stopped);
    pass(&mut h, &mut ports, FAILURE + 8_000);
    assert!(h.store.attempt_holder(TEAM, 21).unwrap().is_none());
    let items = h.store.inbox(TEAM, 200, None).unwrap().rows;
    assert_eq!(items.len(), before.len() + 1);
    pass(&mut h, &mut ports, FAILURE + 12_000);
    assert_eq!(h.store.inbox(TEAM, 200, None).unwrap().rows, items);
}

#[test]
fn run_bound_provider_stop_checks_seat_conversation_and_agent() {
    run_fixture("run_bound_identity_fixture");
}

#[test]
#[ignore = "run by the private actor fixture"]
fn run_bound_identity_fixture() {
    const RUN: &str = "00000000-0000-4000-8000-000000000094";
    for field in ["cwd", "account_id", "archive_id", "conversation_id"] {
        let (mut h, mut ports, record, _) = stage_bound("kimi", Some(RUN));
        let mut stopped = quota(&record, FAILURE);
        publish(&ports, &stopped);
        let mut moved = stopped.clone();
        match field {
            "cwd" => moved["cwd"] = json!("/fixture/elsewhere"),
            "account_id" => moved["account_id"] = json!("another-account"),
            "archive_id" => moved["archive_id"] = json!(producer::archive(&stopped).unwrap()),
            "conversation_id" => producer::confirmed(&mut moved, "another-conversation"),
            _ => unreachable!(),
        }
        producer::save(&mut moved).unwrap();
        if field == "conversation_id" {
            // The saved run really stopped in a different conversation. The
            // fleet still lists the earlier one under the same run and handle.
            moved = quota(&moved, FAILURE + 500);
            assert_eq!(moved["provider_error"]["error_kind"], "quota");
        }

        let member = h.store.member(TEAM, "m3").unwrap().unwrap();
        assert!(
            !reader_saw_stop(&ports, &member, FAILURE + 500),
            "{field}: reader accepted the mismatched stop"
        );
        if field == "cwd" {
            let TeamSession::Record(observed) = read_run(&h, &moved, "kimi") else {
                panic!("the pinned run reader must expose the moved worktree");
            };
            assert_ne!(observed.cwd, member.worktree);
            refuse_proof(
                &mut h,
                &member,
                &observed,
                "writer accepted the wrong worktree",
            );
        }
        pass(&mut h, &mut ports, FAILURE + 500);
        assert_eq!(
            h.store.attempt_holder(TEAM, 21).unwrap().unwrap().member,
            "m3",
            "{field}"
        );
        assert!(releases(&h).is_empty(), "{field}");

        producer::save(&mut stopped).unwrap();
        if field == "cwd" {
            assert!(
                matches!(read_run(&h, &stopped, "claude"), TeamSession::LaunchChanged),
                "pinned run reader accepted the wrong agent"
            );
            let TeamSession::Record(observed) = read_run(&h, &stopped, "kimi") else {
                panic!("pinned run reader refused the actual agent");
            };
            assert_eq!(observed.run_id, RUN);
            assert!(observed.provider.unwrap().stop.is_some());
        }
        assert!(
            reader_saw_stop(&ports, &member, FAILURE + 4_500),
            "{field}: restored seat"
        );
        pass(&mut h, &mut ports, FAILURE + 4_500);
        assert!(
            h.store.attempt_holder(TEAM, 21).unwrap().is_none(),
            "{field}: restored seat"
        );
        assert_eq!(releases(&h).len(), 1, "{field}");
        println!("{field}: retained the hand-out, then released once on exact match");
    }
}

fn read_run(h: &Harness, record: &Value, agent: &str) -> TeamSession {
    state::observe_team_run_for_provider(
        h.temp.path(),
        record["run_id"].as_str().unwrap(),
        record["session_id"].as_str().unwrap(),
        agent,
        record["name"].as_str().unwrap(),
        Instant::now() + Duration::from_secs(1),
    )
}

fn reader_saw_stop(ports: &Production, member: &Member, expected: i64) -> bool {
    let mut seen = false;
    availability::held(
        &ports.native.config,
        &ports.native.epoch,
        TEAM,
        member,
        |_, stop| {
            assert!(stop.at_ms <= expected);
            seen = true;
            Ok(())
        },
        |_| false,
    )
    .unwrap();
    seen
}

fn refuse_proof(h: &mut Harness, member: &Member, record: &TeamRecord, message: &str) {
    let stop = record.provider.as_ref().unwrap().stop.unwrap();
    let team = h.store.team(TEAM).unwrap().unwrap();
    let effect = effect(h, 21);
    let result = ops::apply_provider_stop(
        &mut h.store,
        &team,
        &effect,
        ProviderStopProof {
            member,
            record,
            stop,
            confirmation: None,
        },
        FAILURE + 500,
    );
    assert!(
        matches!(&result, Err(error) if error.code == ops::PROVIDER_STOP_CHANGED),
        "{message}: {result:?}"
    );
    assert_eq!(
        h.store.attempt_holder(TEAM, 21).unwrap().unwrap().member,
        "m3"
    );
    assert!(releases(h).is_empty());
}

fn read_launch(h: &Harness, record: &Value) -> TeamRecord {
    let TeamSession::Record(observed) = state::observe_team_session_for_availability(
        h.temp.path(),
        record["session_id"].as_str().unwrap(),
        record["launch_id"].as_str().unwrap(),
        "kimi",
        record["name"].as_str().unwrap(),
        Instant::now() + Duration::from_secs(1),
    ) else {
        panic!("expected the stopped launch");
    };
    observed
}

fn warm_handle(h: &mut Harness, ports: &mut Production, member: &Member) {
    let team = h.store.team(TEAM).unwrap().unwrap();
    let effect = effect(h, 21);
    ports
        .held_availability(&mut h.store, &team, member, &effect, None, HANDOUT)
        .unwrap();
    assert!(h.store.member_handle(TEAM, "m3").unwrap().is_some());
}

#[test]
fn provider_stop_rechecks_stale_proofs_pending_cancel_and_subagent_stops() {
    run_fixture("provider_stop_race_fixture");
}

#[test]
#[ignore = "run by the private actor fixture"]
fn provider_stop_race_fixture() {
    let (mut h, mut ports, record, _) = stage("kimi");
    let m2 = h.store.member(TEAM, "m2").unwrap().unwrap();
    let m3 = h.store.member(TEAM, "m3").unwrap().unwrap();
    warm_handle(&mut h, &mut ports, &m3);

    let old = quota(&record, HANDOUT - 1);
    let stale = read_launch(&h, &old);
    let stopped = quota(&old, FAILURE);
    let current = read_launch(&h, &stopped);
    refuse_proof(&mut h, &m2, &current, "other member's proof");
    refuse_proof(&mut h, &m3, &stale, "a stop older than the hand-out");
    publish(&ports, &stopped);
    pass(&mut h, &mut ports, FAILURE);
    assert!(h.store.attempt_holder(TEAM, 21).unwrap().is_none());
    assert_eq!(releases(&h).len(), 1);
    println!("wrong member and older stop refused, exact stop released once");

    for node in [19, 21] {
        let (mut h, mut ports, record, _) = stage("kimi");
        h.run(
            &person(),
            Operation::Cancel {
                node,
                reason: "Task withdrawn.".into(),
            },
        )
        .unwrap();
        let pending = h.store.db.as_ref().unwrap().query_row(
            "SELECT count(*) FROM decisions WHERE team=?1 AND node=?2 AND kind='cancel' AND state<>'settled'",
            rusqlite::params![TEAM, node],
            |row| row.get::<_, u32>(0),
        ).unwrap();
        assert_eq!(
            pending, 1,
            "a real Cancel must precede the stop on node {node}"
        );
        let stopped = if node == 19 {
            let held = h.store.member(TEAM, "m2").unwrap().unwrap();
            let row = producer::saved_record(&format!("{}/fixture/m2", held.agent)).unwrap();
            hook(
                &row,
                "StopFailure",
                json!({"error_details":"Invalid API key (401)"}),
                FAILURE,
            )
        } else {
            quota(&record, FAILURE)
        };
        assert!(stopped["provider_error"]["error_kind"].is_string());
        publish(&ports, &stopped);
        pass(&mut h, &mut ports, FAILURE);
        assert!(
            releases(&h).is_empty(),
            "release raced past the pending Cancel on node {node}"
        );
        println!("pending Cancel kept its authority on node {node}");
    }

    for agent in ["kimi", "claude", "codex"] {
        let (mut h, mut ports, record, _) = stage(agent);
        let sub = hook(
            &record,
            "StopFailure",
            json!({
                "error":"rate_limit", "error_details":"You've hit your weekly limit.",
                "agent_id":"sub-1", "agent_type":"general-purpose"
            }),
            FAILURE,
        );
        assert!(sub["provider_error"].is_null(), "{agent}");
        publish(&ports, &sub);
        pass(&mut h, &mut ports, FAILURE);
        assert_eq!(
            h.store.attempt_holder(TEAM, 21).unwrap().unwrap().member,
            "m3"
        );
        assert!(releases(&h).is_empty(), "{agent}");
        println!("{agent}: subagent stop kept the hand-out");
    }
}

#[test]
fn provider_stop_preserves_other_service_card_kinds() {
    run_fixture("unrelated_card_kind_fixture");
}

#[test]
#[ignore = "run by the private actor fixture"]
fn unrelated_card_kind_fixture() {
    let (mut h, mut ports, record, _) = stage("kimi");
    let member = h.store.member(TEAM, "m3").unwrap().unwrap();
    warm_handle(&mut h, &mut ports, &member);
    let card = h
        .run(
            &service(),
            Operation::OpenCard {
                kind: "other_service_notice".into(),
                text: "The service opened a separate notice.".into(),
                node: None,
                decisions: None,
                parameters: None,
            },
        )
        .unwrap()
        .item
        .unwrap();
    let before = h.store.cards(TEAM, 200, None).unwrap().rows;
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].seq, card);
    let stopped = quota(&record, FAILURE);
    let observed = read_launch(&h, &stopped);
    let stop = observed.provider.as_ref().unwrap().stop.unwrap();
    let confirmation = ProviderConfirmation {
        binding: binding(&stopped),
        card: Some(card),
        notice: None,
    };
    let team = h.store.team(TEAM).unwrap().unwrap();
    let effect = effect(&h, 21);
    // Exercise the store's final check with an exact binding and a reference
    // to a real open card owned by the same service, of a different kind.
    ops::apply_provider_stop(
        &mut h.store,
        &team,
        &effect,
        ProviderStopProof {
            member: &member,
            record: &observed,
            stop,
            confirmation: Some(&confirmation),
        },
        FAILURE,
    )
    .unwrap();
    assert!(h.store.attempt_holder(TEAM, 21).unwrap().is_none());
    assert_eq!(
        h.store.cards(TEAM, 200, None).unwrap().rows,
        before,
        "release closed an unrelated service card"
    );
    let journal = releases(&h);
    assert_eq!(journal.len(), 1);
    assert!(!journal[0].refs.contains(&format!("card:{card}")));
}
