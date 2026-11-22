mod guards;

use super::real_rows::{self, Production};
use crate::state::listing_fixture as producer;
use crate::teams::ops::{tests::*, HandoutOutcome, Operation, StageTaskOutcome, TaskStage};
use serde_json::{json, Value};
use std::os::unix::fs::{symlink, PermissionsExt};
use zerus_team_plan::{
    catalog::StageKind,
    model::{Member, NodeKind, NodeState},
};

const CHILD: &str = "desktop::teams::actor::tests::provider_stop::quota_fixture";
const HANDOUT: i64 = 1_800_000_000_000;
const FAILURE: i64 = HANDOUT + 2_000;

#[test]
fn each_providers_quota_stop_hands_stage_21_back_once() {
    let output = real_rows::isolated(CHILD);
    if std::env::var_os("ZERUS_EXPORT_PROVIDER_NOTES").is_some() {
        print!("{output}");
    }
}

#[test]
#[ignore = "run by the private actor fixture"]
fn quota_fixture() {
    for agent in ["kimi", "claude", "codex"] {
        let body = match agent {
            "kimi" => {
                json!({"error_type":"APIProviderRateLimitError","error_message":"429 Your weekly quota has been exhausted."})
            }
            "claude" => {
                json!({"error":"rate_limit","error_details":"You've hit your weekly limit. Try after reset."})
            }
            _ => {
                json!({"error":"usage_limit","error_details":"Your weekly usage limit has been reached."})
            }
        };
        let (mut h, mut ports, record, other_effect) = stage(agent);
        let alias = tempfile::tempdir().unwrap();
        let parent = alias.path().join("state-parent");
        symlink(h.temp.path().parent().unwrap(), &parent).unwrap();
        ports.native.config.state = parent.join(h.temp.path().file_name().unwrap());
        let before = h.store.inbox(TEAM, 200, None).unwrap().rows.len();
        let failed = hook(&record, "StopFailure", body, FAILURE);
        assert_eq!(failed["phase"], "error");
        assert_eq!(failed["provider_error"]["type"], "StopFailure");
        assert_eq!(failed["provider_error"]["error_kind"], "quota");
        assert_eq!(failed["provider_error"]["at"], FAILURE as f64 / 1000.0);
        publish(&ports, &failed);
        pass(&mut h, &mut ports, FAILURE);
        let rows = h.store.inbox(TEAM, 200, None).unwrap().rows;
        assert_eq!(rows.len(), before + 1, "{agent}: first-pass inbox");
        let entry = rows.iter().max_by_key(|row| row.seq).unwrap();
        assert_eq!(entry.node, 20);
        assert_eq!(entry.kind, "report");
        assert_eq!(entry.state, "open");
        let label = match agent {
            "kimi" => "Kimi",
            "claude" => "Claude",
            _ => "Codex",
        };
        let note = entry
            .notes
            .iter()
            .find(|note| note.code == "provider_stop_quota")
            .unwrap();
        assert_eq!(note.parameters, ["m3", label, "20", "21"]);
        if agent == "kimi" {
            provider_projection("stage", entry);
        }
        assert_eq!(h.store.attempt_holder(TEAM, 21).unwrap(), None);
        assert!(h.store.attempt_holder(TEAM, 19).unwrap().is_some());
        let effects = h.store.effects(TEAM, 200, None).unwrap().rows;
        assert_eq!(
            effects
                .iter()
                .find(|e| e.request_id == other_effect)
                .unwrap()
                .state,
            "submitted"
        );
        assert_eq!(
            effects.iter().find(|e| e.node == 21).unwrap().state,
            "superseded"
        );
        assert!(h.store.cards(TEAM, 200, None).unwrap().rows.is_empty());
        pass(&mut h, &mut ports, FAILURE + 4_500);
        assert_eq!(
            h.store.inbox(TEAM, 200, None).unwrap().rows,
            rows,
            "{agent}: restart"
        );
        let recovered = hook(
            &record,
            "UserPromptSubmit",
            json!({"prompt":"Check team status"}),
            FAILURE + 5_000,
        );
        assert!(recovered["provider_error"].is_null());
        publish(&ports, &recovered);
        pass(&mut h, &mut ports, FAILURE + 5_000);
        assert_eq!(
            h.store.inbox(TEAM, 200, None).unwrap().rows,
            rows,
            "{agent}: recovered after release"
        );
        assert!(h.store.attempt_holder(TEAM, 21).unwrap().is_none());
    }
}

const OTHER_CHILD: &str = "desktop::teams::actor::tests::provider_stop::other_cases_fixture";

#[test]
fn provider_stop_controls_and_task_hand_back_use_native_records() {
    let output = real_rows::isolated(OTHER_CHILD);
    if std::env::var_os("ZERUS_EXPORT_PROVIDER_NOTES").is_some() {
        print!("{output}");
    }
}

#[test]
#[ignore = "run by the private actor fixture"]
fn other_cases_fixture() {
    let (mut h, mut ports, record, other_effect) = stage("codex");
    let before = h.store.inbox(TEAM, 200, None).unwrap().rows;
    let failure = hook(
        &record,
        "StopFailure",
        json!({"error":"usage_limit","error_details":"Weekly quota reached."}),
        HANDOUT - 1_000,
    );
    assert_eq!(failure["provider_error"]["error_kind"], "quota");
    publish(&ports, &failure);
    pass(&mut h, &mut ports, HANDOUT);
    assert_eq!(
        h.store.inbox(TEAM, 200, None).unwrap().rows,
        before,
        "old failure"
    );

    let rate = hook(
        &record,
        "StopFailure",
        json!({"error_details":"429 Too many requests"}),
        FAILURE,
    );
    assert_eq!(rate["provider_error"]["error_kind"], "rate_limit");
    publish(&ports, &rate);
    pass(&mut h, &mut ports, FAILURE);
    assert_eq!(
        h.store.inbox(TEAM, 200, None).unwrap().rows,
        before,
        "rate limit"
    );

    let capacity = hook(
        &record,
        "StopFailure",
        json!({"error_details":"API Error: 529 Overloaded"}),
        FAILURE + 1000,
    );
    assert_eq!(capacity["provider_error"]["error_kind"], "capacity");
    publish(&ports, &capacity);
    pass(&mut h, &mut ports, FAILURE + 1000);
    assert_eq!(
        h.store.inbox(TEAM, 200, None).unwrap().rows,
        before,
        "capacity"
    );

    let network = hook(
        &record,
        "StopFailure",
        json!({"error_details":"Network error: connection reset by peer"}),
        FAILURE + 2000,
    );
    assert_eq!(network["provider_error"]["error_kind"], "provider");
    publish(&ports, &network);
    pass(&mut h, &mut ports, FAILURE + 2000);
    assert_eq!(
        h.store.inbox(TEAM, 200, None).unwrap().rows,
        before,
        "network error"
    );
    let retry = hook(
        &record,
        "UserPromptSubmit",
        json!({"prompt":"[HGS automatic recovery] The previous turn stopped with a temporary provider error."}),
        FAILURE + 3000,
    );
    assert!(retry["provider_error"].is_null());
    pass(&mut h, &mut ports, FAILURE + 3000); // Fleet still carries the pre-retry row.
    assert_eq!(
        h.store.inbox(TEAM, 200, None).unwrap().rows,
        before,
        "retried network error"
    );

    let quota = hook(
        &record,
        "StopFailure",
        json!({"error_details":"Weekly quota reached."}),
        FAILURE + 4000,
    );
    publish(&ports, &quota);
    let recovery = hook(
        &record,
        "UserPromptSubmit",
        json!({"prompt":"Continue the review"}),
        FAILURE + 5000,
    );
    assert!(recovery["provider_error"].is_null());
    pass(&mut h, &mut ports, FAILURE + 5000);
    assert_eq!(
        h.store.inbox(TEAM, 200, None).unwrap().rows,
        before,
        "quota recovered before scan"
    );
    assert_eq!(
        h.store.attempt_holder(TEAM, 21).unwrap().unwrap().member,
        "m3"
    );

    // The standalone task follows ordinary hand-back, which holds the task
    // for the lead. The unrelated stage stays with its current reviewer.
    let other = producer::saved_record(&format!(
        "{}/fixture/m2",
        h.store.member(TEAM, "m2").unwrap().unwrap().agent
    ))
    .unwrap();
    let stopped = hook(
        &other,
        "StopFailure",
        json!({"error_details":"Invalid API key (401)"}),
        FAILURE + 6000,
    );
    assert_eq!(stopped["provider_error"]["error_kind"], "authentication");
    publish(&ports, &stopped);
    pass(&mut h, &mut ports, FAILURE + 6000);
    let rows = h.store.inbox(TEAM, 200, None).unwrap().rows;
    assert_eq!(rows.len(), before.len() + 1);
    let item = rows.iter().max_by_key(|row| row.seq).unwrap();
    assert_eq!(item.node, 19);
    assert_eq!(item.kind, format!("provider_stop:{other_effect}"));
    assert_eq!(item.notes[0].code, "provider_stop_authentication");
    assert_eq!(item.notes[0].parameters, ["m2", "Claude", "19"]);
    provider_projection("task", item);
    assert!(h.store.node(TEAM, 19).unwrap().unwrap().hold);
    assert!(h.store.attempt_holder(TEAM, 19).unwrap().is_none());
    assert_eq!(
        h.store.attempt_holder(TEAM, 21).unwrap().unwrap().member,
        "m3"
    );

    let (mut bare, mut ports, record, _) = stage("dsh");
    let before = bare.store.inbox(TEAM, 200, None).unwrap().rows;
    let mut waiting = record.clone();
    // This is the native permission-gate input to DeepSeek's producer.
    // The catalog receives its real projection, with phase=error but no
    // provider error. No persisted or catalog row is constructed by hand.
    waiting["permissions_pending"] = json!(true);
    let row = producer::listed_deepseek_row(
        &waiting,
        &json!({
            "items":[{"sessionId":record["conversation_id"],"state":"running","status":"idle"}]
        }),
    );
    assert_eq!(row["phase"], "error");
    assert!(row["provider_error"].is_null());
    real_rows::store(
        &ports.native.config.state,
        &format!("run:{}", record["run_id"].as_str().unwrap()),
        &row,
    );
    pass(&mut bare, &mut ports, FAILURE);
    assert_eq!(
        bare.store.inbox(TEAM, 200, None).unwrap().rows,
        before,
        "bare error"
    );
    assert_eq!(
        bare.store.attempt_holder(TEAM, 21).unwrap().unwrap().member,
        "m3"
    );
}

const CARD_CHILD: &str = "desktop::teams::actor::tests::provider_stop::card_fixture";

#[test]
fn confirmed_provider_stop_closes_only_its_unreadable_confirmation_card() {
    real_rows::isolated(CARD_CHILD);
}

#[test]
#[ignore = "run by the private actor fixture"]
fn card_fixture() {
    for lost_opener in [false, true] {
        check_confirmation_close(lost_opener);
    }
    confirmation_with_another_binding();
}

fn check_confirmation_close(lost_opener: bool) {
    let (mut h, mut ports, record, _) = stage("kimi");
    let mut actor = super::super::Runtime::open(h.temp.path(), &ports.native.machine).unwrap();
    let team = h.store.team(TEAM).unwrap().unwrap();
    let reviewer = h.store.member(TEAM, "m3").unwrap().unwrap();
    let other = h.store.member(TEAM, "m2").unwrap().unwrap();
    let binding = super::super::provider_binding_stamp(
        record["launch_id"].as_str().unwrap(),
        record["session_id"].as_str().unwrap(),
        record["run_id"].as_str().unwrap(),
        record["conversation_id"].as_str().unwrap(),
    );
    let fault = Some((
        binding.as_str(),
        "The member's session record cannot be read",
    ));
    let unrelated = Some((
        "another-binding",
        "The member's session record cannot be read",
    ));
    actor
        .confirmation_observation(
            &mut h.store,
            &team,
            &reviewer,
            fault,
            HANDOUT - 200_000,
            |_| {},
        )
        .unwrap();
    let mut open = || {
        actor
            .confirmation_observation(&mut h.store, &team, &reviewer, fault, HANDOUT, |_| {})
            .unwrap();
    };
    if lost_opener {
        super::retry_cards::crash("notice_applied", open);
        actor = super::super::Runtime::open(h.temp.path(), &ports.native.machine).unwrap();
    } else {
        open();
    }
    for now in [HANDOUT - 200_000, HANDOUT] {
        actor
            .confirmation_observation(&mut h.store, &team, &other, unrelated, now, |_| {})
            .unwrap();
    }
    let old = h.store.cards(TEAM, 200, None).unwrap().rows;
    assert_eq!(old.len(), 2);
    assert!(old.iter().all(|card| card.state == "open"));

    let stopped = hook(
        &record,
        "StopFailure",
        json!({"error_type":"APIProviderRateLimitError","error_message":"Weekly quota reached."}),
        FAILURE,
    );
    publish(&ports, &stopped);
    let card = old.iter().find(|card| card.text.contains("m3")).unwrap();
    h.store
        .db
        .as_ref()
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER reject_confirmation_close BEFORE UPDATE OF state ON cards
         WHEN OLD.seq={} AND NEW.state='closed'
         BEGIN SELECT RAISE(ABORT, 'fixture card close failed'); END;",
            card.seq
        ))
        .unwrap();
    let blocked = actor.tick(&mut h.store, &mut ports, TEAM, FAILURE, true);
    h.store
        .db
        .as_ref()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_confirmation_close")
        .unwrap();
    assert_eq!(
        h.store
            .attempt_holder(TEAM, 21)
            .unwrap()
            .map(|held| held.member)
            .as_deref(),
        Some("m3"),
        "the hand-back committed despite the failed confirmation close: {blocked:?}"
    );
    assert_eq!(h.store.cards(TEAM, 200, None).unwrap().rows, old);
    assert!(!h
        .store
        .inbox(TEAM, 200, None)
        .unwrap()
        .rows
        .iter()
        .any(|item| {
            item.node == 20
                && item
                    .notes
                    .iter()
                    .any(|note| note.code == "provider_stop_quota")
        }));

    // A restarted actor can commit the original hand-back after storage recovers.
    let mut actor = super::super::Runtime::open(h.temp.path(), &ports.native.machine).unwrap();
    actor
        .tick(&mut h.store, &mut ports, TEAM, FAILURE + 1_000_000, true)
        .unwrap();
    assert!(h.store.attempt_holder(TEAM, 21).unwrap().is_none());
    let cards = h.store.cards(TEAM, 200, None).unwrap().rows;
    assert_eq!(cards.len(), 2);
    assert_eq!(
        cards
            .iter()
            .find(|card| card.text.contains("m3"))
            .unwrap()
            .state,
        "closed"
    );
    assert_eq!(
        cards
            .iter()
            .find(|card| card.text.contains("m2"))
            .unwrap()
            .state,
        "open"
    );
    for now in [FAILURE + 1_000_000, FAILURE + 1_200_000] {
        actor
            .confirmation_observation(&mut h.store, &team, &reviewer, fault, now, |_| {})
            .unwrap();
    }
    assert_eq!(h.store.cards(TEAM, 200, None).unwrap().rows, cards);
    let journal = h.store.journal(TEAM, 200, None).unwrap().rows;
    assert!(journal.iter().any(|row| row.kind == "release_provider_stop"
        && row.refs.contains(&format!("card:{}", card.seq))
        && row
            .text
            .contains("The service confirmed that the member stopped after a provider error.")));
}

fn confirmation_with_another_binding() {
    let (mut h, mut ports, record, _) = stage("kimi");
    let mut actor = super::super::Runtime::open(h.temp.path(), &ports.native.machine).unwrap();
    let team = h.store.team(TEAM).unwrap().unwrap();
    let reviewer = h.store.member(TEAM, "m3").unwrap().unwrap();
    let old = super::super::provider_binding_stamp(
        record["launch_id"].as_str().unwrap(),
        record["session_id"].as_str().unwrap(),
        "previous-run",
        record["conversation_id"].as_str().unwrap(),
    );
    for now in [HANDOUT - 200_000, HANDOUT] {
        actor
            .confirmation_observation(
                &mut h.store,
                &team,
                &reviewer,
                Some((&old, "The member's session record cannot be read")),
                now,
                |_| {},
            )
            .unwrap();
    }
    let cards = h.store.cards(TEAM, 200, None).unwrap().rows;
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].state, "open");
    let (gate, proof) = actor.provider_confirmation(&team, &reviewer).unwrap().unwrap();
    assert_eq!(proof.binding, old);
    assert_eq!(proof.card, Some(cards[0].seq));
    publish(
        &ports,
        &hook(
            &record,
            "StopFailure",
            json!({"error_type":"APIProviderRateLimitError","error_message":"Weekly quota reached."}),
            FAILURE,
        ),
    );
    actor
        .tick(&mut h.store, &mut ports, TEAM, FAILURE, true)
        .unwrap();
    assert!(h.store.attempt_holder(TEAM, 21).unwrap().is_none());
    assert_eq!(h.store.cards(TEAM, 200, None).unwrap().rows, cards);
    assert_eq!(
        actor.provider_confirmation(&team, &reviewer).unwrap().map(|(row, _)| row),
        Some(gate),
        "release discarded another binding's confirmation ledger row"
    );
    actor
        .confirmation_observation(&mut h.store, &team, &reviewer, None, FAILURE + 4_500, |_| {})
        .unwrap();
    let recovered = h.store.cards(TEAM, 200, None).unwrap().rows;
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].seq, cards[0].seq);
    assert_eq!(recovered[0].state, "closed");
    assert!(actor.provider_confirmation(&team, &reviewer).unwrap().is_none());
}

fn provider_projection(key: &str, item: &crate::teams::store::InboxItem) {
    use zerus_client_core::{
        actions::Exit,
        teams::drawers::{self, DrawerKind, DrawerRead},
    };
    let page = drawers::decode(
        &DrawerRead {
            team: TEAM.into(),
            kind: DrawerKind::Inbox,
            after: None,
            recorded_timings: false,
            timing_seqs: None,
            timing_next: None,
        },
        Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(&json!({
                "ok":true, "actor": {"kind":"person"}, "team":TEAM, "rows":[item], "next":null
            }))
            .unwrap(),
            stderr: String::new(),
        },
    )
    .unwrap();
    let row = serde_json::to_value(&page.rows[0]).unwrap();
    if std::env::var_os("ZERUS_EXPORT_PROVIDER_NOTES").is_some() {
        println!("PROVIDER_DRAWER {key} {row}");
    } else {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("client/core/src/upstream_teams/fixtures/provider-stop.json");
        let expected: Value = serde_json::from_slice(&std::fs::read(fixtures).unwrap()).unwrap();
        assert_eq!(
            row, expected[key],
            "the frontend fixture must come from the core inbox projection"
        );
    }
}

const RUN_CHILD: &str = "desktop::teams::actor::tests::provider_stop::run_bound_fixture";

#[test]
fn provider_stop_scans_a_run_bound_holder_and_rejects_a_replacement_run() {
    real_rows::isolated(RUN_CHILD);
}

#[test]
#[ignore = "run by the private actor fixture"]
fn run_bound_fixture() {
    const RUN: &str = "00000000-0000-4000-8000-000000000083";
    for replaced in [true, false] {
        let (mut h, mut ports, record, _) = stage_bound("kimi", Some(RUN));
        assert!(record["launch_id"].is_null());
        let stopped = hook(
            &record,
            "StopFailure",
            json!({"error_type":"APIProviderRateLimitError","error_message":"The weekly quota was exhausted."}),
            FAILURE,
        );
        assert_eq!(stopped["provider_error"]["error_kind"], "quota");
        publish(&ports, &stopped);
        if replaced {
            let next_run = "00000000-0000-4000-8000-000000000084";
            let mut next = producer::resumed_record(&record, next_run);
            producer::confirmed(&mut next, record["conversation_id"].as_str().unwrap());
            producer::save(&mut next).unwrap();
            let next = hook(
                &next,
                "StopFailure",
                json!({"error_type":"APIProviderRateLimitError","error_message":"The weekly quota was exhausted."}),
                FAILURE,
            );
            let observe = |run| {
                crate::state::observe_team_run_for_provider(
                    h.temp.path(),
                    run,
                    next["session_id"].as_str().unwrap(),
                    "kimi",
                    next["name"].as_str().unwrap(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
            };
            assert!(matches!(
                observe(RUN),
                crate::state::TeamSession::LaunchChanged
            ));
            let crate::state::TeamSession::Record(next) = observe(next_run) else {
                panic!("the native producer must have written the stopped replacement");
            };
            let stop = next.provider.as_ref().unwrap().stop.unwrap();
            let member = h.store.member(TEAM, "m3").unwrap().unwrap();
            let team = h.store.team(TEAM).unwrap().unwrap();
            let effect = h
                .store
                .effects(TEAM, 200, None)
                .unwrap()
                .rows
                .into_iter()
                .find(|effect| effect.node == 21)
                .unwrap();
            refused(
                crate::teams::ops::apply_provider_stop(
                    &mut h.store,
                    &team,
                    &effect,
                    crate::teams::ops::ProviderStopProof {
                        member: &member,
                        record: &next,
                        stop,
                        confirmation: None,
                    },
                    FAILURE,
                ),
                crate::teams::ops::PROVIDER_STOP_CHANGED,
            );
        }
        pass(&mut h, &mut ports, FAILURE);
        let held = h.store.attempt_holder(TEAM, 21).unwrap();
        assert_eq!(
            held.is_some(),
            replaced,
            "the exact bound run determines the release"
        );
        assert!(h.store.member_handle(TEAM, "m3").unwrap().is_none());
        let items = h.store.inbox(TEAM, 200, None).unwrap().rows;
        assert_eq!(
            items
                .iter()
                .filter(|item| item.node == 20
                    && item
                        .notes
                        .iter()
                        .any(|note| note.code == "provider_stop_quota"))
                .count(),
            usize::from(!replaced)
        );
        pass(&mut h, &mut ports, FAILURE + 4_500);
        assert_eq!(h.store.inbox(TEAM, 200, None).unwrap().rows, items);
    }
}

#[test]
fn lead_staffs_and_reassigns_the_quota_stopped_stage_once() {
    real_rows::isolated("desktop::teams::actor::tests::provider_stop::lead_staffing_fixture");
}

#[test]
#[ignore = "run by the private actor fixture"]
fn lead_staffing_fixture() {
    let (mut h, mut ports, record, other_effect) = stage("kimi");
    crate::swarm::save_test_node_id(&ports.native.config, &ports.native.machine).unwrap();
    let lead = h.bind("lead");
    let stopped = hook(
        &record,
        "StopFailure",
        json!({"error_type":"APIProviderRateLimitError","error_message":"429 Your weekly quota has been exhausted."}),
        FAILURE,
    );
    assert_eq!(stopped["phase"], "error");
    assert_eq!(stopped["provider_error"]["type"], "StopFailure");
    assert_eq!(stopped["provider_error"]["error_kind"], "quota");
    assert_eq!(stopped["provider_error"]["at"], FAILURE as f64 / 1000.0);
    publish(&ports, &stopped);
    pass(&mut h, &mut ports, FAILURE);
    let inbox = h.store.inbox(TEAM, 200, None).unwrap().rows;
    let release = inbox
        .iter()
        .find(|item| item.node == 20 && item.kind == "report")
        .unwrap();
    assert_eq!(release.notes[0].parameters, ["m3", "Kimi", "20", "21"]);
    assert!(h.store.attempt_holder(TEAM, 21).unwrap().is_none());
    let old_stage = h.node(21);
    let add = Operation::LeadAddMember {
        role: "reviewer".into(),
        agent: "codex".into(),
        model: "seat-model".into(),
        effort: "high".into(),
        account: zerus_team_plan::catalog::AccountChoice::Named {
            account: "review-account".into(),
        },
    };
    let added = h.run(&lead, add).unwrap();
    let member = added.member.unwrap();
    assert_eq!(member, "m4");
    let reassign = |member: &str| Operation::Reassign {
        node: 21,
        member: member.to_owned(),
        reason: "Replace the quota-stopped reviewer.".into(),
    };
    assert!(
        h.run(&lead, reassign(&member)).is_err(),
        "launch and retirement are still pending"
    );
    h.cancel_fact(21);
    assert!(!h
        .store
        .queue(TEAM, 200, None)
        .unwrap()
        .rows
        .iter()
        .any(|n| n.id == 21));
    refused(h.run(&lead, reassign(&member)), "invalid_state");
    let (replacement, observed) =
        crate::desktop::teams::tests::launch_codex_reviewer(&mut h, &member);
    assert_eq!(replacement.agent, "codex");
    publish(
        &ports,
        &save_session_as(
            &replacement,
            &observed.name,
            &observed.session_id,
            &observed.run_id,
            &observed.conversation_id,
        ),
    );
    refused(h.run(&lead, reassign("m2")), "invalid_state");
    let assigned = h.run(&lead, reassign(&member)).unwrap().node.unwrap();
    assert_eq!(assigned, 22);
    let new_stage = h.node(assigned);
    assert_eq!(
        (new_stage.stage_of, new_stage.stage_index, new_stage.round),
        (old_stage.stage_of, old_stage.stage_index, old_stage.round)
    );
    assert_eq!(new_stage.assignee.as_deref(), Some("m4"));
    let journal = h.store.journal(TEAM, 200, None).unwrap().rows;
    assert!(journal.iter().any(|row| row.kind == "reassign"
        && row.refs.contains(&"node:21".to_owned())
        && row.refs.contains(&"node:22".to_owned())));
    refused(h.run(&lead, reassign(&member)), "invalid_state");

    let creation = uuid::Uuid::new_v4().to_string();
    h.run(
        &service(),
        Operation::StageTaskIntent {
            node: assigned,
            effect_request_id: creation.clone(),
        },
    )
    .unwrap();
    h.run(
        &service(),
        Operation::StageTaskOutcome {
            effect_request_id: creation,
            outcome: StageTaskOutcome::Created { task: task(22) },
        },
    )
    .unwrap();
    h.observe(assigned, TaskStage::Todo);
    let team = h.store.team(TEAM).unwrap().unwrap();
    let ready = crate::teams::ops::readiness(&h.store, TEAM).unwrap();
    assert!(
        zerus_team_plan::ready::ready(&zerus_team_plan::ready::PlanView {
            phases: &ready.phases,
            nodes: &ready.nodes,
            members: &[],
            lead: &team.lead,
            integration: team.settings.integration,
        })
        .contains(&assigned),
        "replacement stage is not ready: {:?}",
        h.node(assigned)
    );
    let availability =
        super::super::ports::Ports::availability(&mut ports, &team, &replacement).unwrap();
    assert_eq!(availability, super::super::ports::Availability::Free);
    pass(&mut h, &mut ports, FAILURE + 4_500);
    let deliveries = ports.deliveries.clone();
    assert_eq!(deliveries.len(), 1, "{deliveries:?}");
    assert!(deliveries[0].starts_with("m4 -> "), "{deliveries:?}");
    assert_eq!(
        h.store
            .attempt_holder(TEAM, assigned)
            .unwrap()
            .unwrap()
            .member,
        "m4"
    );
    h.store = crate::teams::store::Store::open(h.temp.path()).unwrap();
    pass(&mut h, &mut ports, FAILURE + 9_000);
    assert_eq!(
        ports.deliveries, deliveries,
        "a restart repeated the hand-out"
    );
    let effects = h.store.effects(TEAM, 200, None).unwrap().rows;
    let mut delivered = effects.iter().filter(|effect| effect.node == assigned);
    assert_eq!(delivered.next().unwrap().state, "submitted");
    assert!(delivered.next().is_none());
    assert_eq!(
        effects
            .iter()
            .find(|effect| effect.request_id == other_effect)
            .unwrap()
            .state,
        "submitted"
    );
    let rows = h.store.inbox(TEAM, 200, None).unwrap().rows;
    assert_eq!(
        rows.iter()
            .filter(|item| item.kind.starts_with("lead_member_added:"))
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .find(|item| item.seq == release.seq)
            .unwrap()
            .state,
        "closed"
    );
    assert!(h.store.cards(TEAM, 200, None).unwrap().rows.is_empty());
    // Preserve another report in the same generation and a different inbox
    // kind on the same parent and generation, all written by the product.
    h.report(19, "m2");
    let unrelated = h
        .store
        .queue(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .find(|node| node.stage_of == Some(19))
        .unwrap();
    let creation = uuid::Uuid::new_v4().to_string();
    h.run(
        &service(),
        Operation::StageTaskIntent {
            node: unrelated.id,
            effect_request_id: creation.clone(),
        },
    )
    .unwrap();
    let other_report = h
        .run(
            &service(),
            Operation::StageTaskOutcome {
                effect_request_id: creation,
                outcome: StageTaskOutcome::Refused {
                    reason: "The other review could not start.".into(),
                },
            },
        )
        .unwrap()
        .item
        .unwrap();
    let question = h
        .run(
            &person(),
            Operation::Ask {
                node: 20,
                generation: release.generation,
                text: "Is this review ready?".into(),
            },
        )
        .unwrap()
        .item
        .unwrap();
    h.run(
        &lead,
        Operation::Reply {
            item: question,
            text: "The replacement is reviewing it.".into(),
        },
    )
    .unwrap();
    let controls = |inbox: Vec<crate::teams::store::InboxItem>| {
        inbox
            .into_iter()
            .filter(|item| item.seq == other_report || item.seq == question)
            .collect::<Vec<_>>()
    };
    let before = controls(h.store.inbox(TEAM, 200, None).unwrap().rows);
    assert_eq!(before.len(), 2);
    assert!(before.iter().all(|item| item.generation == release.generation));
    let other = before.iter().find(|item| item.seq == other_report).unwrap();
    assert_eq!(
        (other.node, other.kind.as_str(), other.state.as_str()),
        (19, "report", "open")
    );
    let answered = before.iter().find(|item| item.seq == question).unwrap();
    assert_eq!(answered.node, 20);
    assert_eq!(
        answered.reply.as_deref(),
        Some("The replacement is reviewing it.")
    );

    // A separate end of the replacement reopens the parent's report.
    // That ordinary retirement grants no provider-stop reassignment.
    h.observe(assigned, TaskStage::InProgress);
    h.cancel_fact(assigned);
    let inbox = h.store.inbox(TEAM, 200, None).unwrap().rows;
    let reopened = inbox.iter().find(|item| item.seq == release.seq).unwrap();
    assert_eq!(reopened.state, "open");
    assert!(reopened.notes.is_empty());
    assert_eq!(controls(inbox), before, "another inbox item changed");
    for node in [21, assigned] {
        refused(
            h.run(
                &lead,
                Operation::Reassign {
                    node,
                    member: "m3".into(),
                    reason: "Attempting to reassign a stage with no current provider stop.".into(),
                },
            ),
            "invalid_state",
        );
    }
}

#[test]
fn landing_disabled_refuses_a_new_replacement_for_a_retired_land_stage() {
    real_rows::isolated("desktop::teams::actor::tests::provider_stop::retired_land_fixture");
}

#[test]
#[ignore = "run by the private actor fixture"]
fn retired_land_fixture() {
    let (mut h, mut ports, record, _) = stage_with_kind("kimi", StageKind::Land, None);
    let lead = h.bind("lead");
    let stopped = hook(
        &record,
        "StopFailure",
        json!({"error_type":"APIProviderRateLimitError","error_message":"Your weekly quota has been exhausted."}),
        FAILURE,
    );
    assert_eq!(stopped["provider_error"]["error_kind"], "quota");
    publish(&ports, &stopped);
    pass(&mut h, &mut ports, FAILURE);
    h.cancel_fact(21);
    assert!(!h.store.node_live(TEAM, 21).unwrap());

    h.settings(|settings| settings.land_allowed = false);
    let stages = h.store.stage_tasks(TEAM, 200, None).unwrap().rows;
    let inbox = h.store.inbox(TEAM, 200, None).unwrap().rows;
    assert!(inbox.iter().any(|item| item.node == 20 && item.state == "open"));
    let reassign = Operation::Reassign {
        node: 21,
        member: "m3".into(),
        reason: "Retry the retired stage.".into(),
    };
    let refusal = h.run(&lead, reassign.clone()).unwrap_err();
    assert_eq!(refusal.code, "invalid_state");
    assert!(refusal.message.contains("Landing is disabled in Team settings"));
    assert_eq!(h.store.stage_tasks(TEAM, 200, None).unwrap().rows, stages);
    assert_eq!(h.store.inbox(TEAM, 200, None).unwrap().rows, inbox);

    h.settings(|settings| settings.land_allowed = true);
    let replacement = h.run(&lead, reassign).unwrap().node.unwrap();
    assert!(h.node(replacement).title.starts_with("Land: "));
}

fn pass(h: &mut Harness, ports: &mut Production, now: i64) {
    // Opening the adapter on every call includes the first pass after a
    // service restart. The actual roster/effect scan and store writer run.
    let state = h.temp.path();
    let mut actor = super::super::Runtime::open(state, &ports.native.machine).unwrap();
    actor.tick(&mut h.store, ports, TEAM, now, true).unwrap();
}

fn stage(agent: &str) -> (Harness, Production, Value, String) {
    stage_bound(agent, None)
}

fn stage_bound(agent: &str, run: Option<&str>) -> (Harness, Production, Value, String) {
    stage_with_kind(agent, StageKind::Review, run)
}

fn stage_with_kind(
    agent: &str,
    kind: StageKind,
    run: Option<&str>,
) -> (Harness, Production, Value, String) {
    let mut h = Harness::empty();
    // The pinned native reader expects the product's private state root.
    std::fs::set_permissions(h.temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut create = create();
    if let Operation::Create { bundle, .. } = &mut create {
        if kind == StageKind::Land {
            bundle.template.settings.land_allowed = true;
            let pipeline = bundle
                .pipelines
                .iter_mut()
                .find(|pipeline| pipeline.id == bundle.template.default_pipeline)
                .unwrap();
            pipeline.stages[0].kind = kind;
        }
        bundle
            .roles
            .iter_mut()
            .find(|role| role.id == "reviewer")
            .unwrap()
            .agent = agent.into();
    }
    h.run(&person(), create).unwrap();
    for id in ["m1", "m2"] {
        h.bind(id);
    }
    if let Some(run) = run {
        let seat = fixture()
            .members
            .into_iter()
            .find(|m| m.id == "m3")
            .unwrap();
        let mut session = seat.session.unwrap();
        session.session_id = format!("run_id:{run}");
        h.run(
            &service(),
            Operation::SetMemberSession {
                member: seat.id,
                session,
                account: seat.account,
                worktree: seat.worktree,
                ownership_nonce: seat.ownership_nonce,
            },
        )
        .unwrap();
    } else {
        h.bind("m3");
    }
    for number in 1..19 {
        h.enqueue(&person(), number);
    }
    let other = h.add(&person(), 19);
    let other_effect = "00000000-0000-4000-8000-000000000019".to_owned();
    h.run(
        &service(),
        Operation::HandoutIntent {
            node: other,
            member: "m2".into(),
            effect_request_id: other_effect.clone(),
        },
    )
    .unwrap();
    h.outcome(
        &other_effect,
        HandoutOutcome::Submitted {
            receipt: "fixture-other-result".into(),
        },
    );
    h.observe(other, TaskStage::InProgress);
    let parent = h.add(&person(), 20);
    h.submit(parent, "m1");
    h.report(parent, "m1");
    let stage = h
        .store
        .queue(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .find(|node| node.kind == NodeKind::Stage && node.stage_of == Some(parent))
        .unwrap()
        .id;
    assert_eq!((parent, stage), (20, 21));
    let creation = uuid::Uuid::new_v4().to_string();
    h.run(
        &service(),
        Operation::StageTaskIntent {
            node: stage,
            effect_request_id: creation.clone(),
        },
    )
    .unwrap();
    h.run(
        &service(),
        Operation::StageTaskOutcome {
            effect_request_id: creation,
            outcome: StageTaskOutcome::Created { task: task(21) },
        },
    )
    .unwrap();
    h.observe(stage, TaskStage::Todo);
    let effect_request_id = uuid::Uuid::new_v4().to_string();
    let revision = h.store.team(TEAM).unwrap().unwrap().revision;
    apply_fixture(
        &mut h.store,
        &service(),
        &request(
            revision,
            Operation::HandoutIntent {
                node: stage,
                member: "m3".into(),
                effect_request_id: effect_request_id.clone(),
            },
        ),
        HANDOUT,
    )
    .unwrap();
    h.outcome(
        &effect_request_id,
        HandoutOutcome::Submitted {
            receipt: "fixture-original-result".into(),
        },
    );
    h.observe(stage, TaskStage::InProgress);
    assert_eq!(
        h.store.node(TEAM, stage).unwrap().unwrap().state,
        NodeState::InProgress
    );

    let state = h.temp.path();
    std::env::set_var("HGS_STATE_DIR", state);
    let ports = Production {
        native: real_rows::native(state, &state.join("config")),
        deliveries: vec![],
    };
    let reviewer = h.store.member(TEAM, "m3").unwrap().unwrap();
    assert_eq!(reviewer.agent, agent);
    let record = save_session(&reviewer);
    let other = save_session(&h.store.member(TEAM, "m2").unwrap().unwrap());
    publish(&ports, &other);
    (h, ports, record, other_effect)
}

fn save_session(member: &Member) -> Value {
    let session = member.session.as_ref().unwrap();
    let run = session
        .session_id
        .strip_prefix("run_id:")
        .map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned);
    save_session_as(
        member,
        &format!("{}/fixture/{}", member.agent, member.id),
        &uuid::Uuid::new_v4().to_string(),
        &run,
        &format!("conversation-{}", member.id),
    )
}

fn save_session_as(member: &Member, name: &str, sid: &str, run: &str, conversation: &str) -> Value {
    let binding = &member.session.as_ref().unwrap().session_id;
    let launch = binding.strip_prefix("launch_id:").unwrap_or_else(|| {
        assert_eq!(binding.strip_prefix("run_id:"), Some(run));
        ""
    });
    let mut record = producer::launched_record(
        &member.agent,
        name,
        launch,
        sid,
        run,
        &member.worktree,
        &member.account,
    );
    producer::confirmed(&mut record, conversation);
    producer::save(&mut record).unwrap();
    if !launch.is_empty() {
        producer::launch_started(launch, name, sid).unwrap();
    }
    record
}

fn hook(record: &Value, event: &str, mut body: Value, now: i64) -> Value {
    body["hook_event_name"] = json!(event);
    body["session_id"] = record["conversation_id"].clone();
    producer::record_activity_hook(record["name"].as_str().unwrap(), &body, now as f64 / 1000.0)
        .unwrap()
}

fn publish(ports: &Production, record: &Value) {
    let row = producer::listed_live_row(record.clone()).unwrap();
    real_rows::store(
        &ports.native.config.state,
        &format!("run:{}", record["run_id"].as_str().unwrap()),
        &row,
    );
}
