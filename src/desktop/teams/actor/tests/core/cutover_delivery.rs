//! Real actor passes and team writers; only the terminal/transport port is fake.
use super::*;
use zerus_team_plan::problem::code;

#[test]
fn a_cutover_refusal_is_replaced_once_by_a_new_effect_after_the_fence_opens() {
    let (mut h, mut a, mut p) = setup(1);
    p.reply = HandoutOutcome::CuttingOver {
        reason: "This task is cutting over (held). Nothing was sent.".into(),
    };
    tick(&mut h, &mut a, &mut p, 100, false);
    assert_eq!(p.sends, 1);
    tick(&mut h, &mut a, &mut p, 101, false);
    assert_eq!(
        p.sends, 1,
        "a closed fence uses the existing refusal backoff"
    );
    let effects = h.store.effects(TEAM, 200, None).unwrap().rows;
    assert_eq!(effects.len(), 1);
    let old = effects[0].request_id.clone();
    assert_eq!(effects[0].state, "refused");
    assert!(h
        .store
        .open_effects(TEAM, 200, None)
        .unwrap()
        .rows
        .is_empty());
    assert!(h
        .store
        .inbox(TEAM, 200, None)
        .unwrap()
        .rows
        .iter()
        .all(|i| i.kind != "delivery_uncertain"));
    drop(a);
    let home = h.store.team(TEAM).unwrap().unwrap().home;
    let mut a = super::super::super::Runtime::open(h.temp.path(), &home).unwrap();
    p.reply = HandoutOutcome::Submitted {
        receipt: "new admitted hand-out".into(),
    };
    tick(&mut h, &mut a, &mut p, 60_100, false);
    let effects = h.store.effects(TEAM, 200, None).unwrap().rows;
    assert_eq!(effects.len(), 2);
    assert_ne!(effects[1].request_id, old);
    assert_eq!(effects[1].state, "submitted");
    tick(&mut h, &mut a, &mut p, 120_100, false);
    assert_eq!(p.sends, 2, "the old effect is never replayed");
}

fn legacy(case: &str) -> (tempfile::TempDir, Store, super::super::super::Runtime) {
    let temp = tempfile::tempdir().unwrap();
    crate::platform::private_dir(&temp.path().join("teams")).unwrap();
    std::fs::write(temp.path().join("teams/preview"), "on").unwrap();
    let sql = match case {
        "open" => include_str!("../../../../../teams/ops/tests/fixtures/cutover-c5/open.sql"),
        "recovered" => {
            include_str!("../../../../../teams/ops/tests/fixtures/cutover-c5/recovered.sql")
        }
        "generation" => {
            include_str!("../../../../../teams/ops/tests/fixtures/cutover-c5/generation.sql")
        }
        _ => unreachable!(),
    };
    let db = rusqlite::Connection::open(temp.path().join("teams/teams-v1.sqlite3")).unwrap();
    db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    db.execute_batch(sql).unwrap();
    assert!(!db
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .exists([])
        .unwrap());
    drop(db);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            temp.path().join("teams/teams-v1.sqlite3"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let store = Store::open(temp.path()).unwrap();
    let home = store.team(TEAM).unwrap().unwrap().home;
    let actor = super::super::super::Runtime::open(temp.path(), &home).unwrap();
    (temp, store, actor)
}

#[test]
fn old_open_cutover_is_settled_without_a_delivery_or_an_uncertain_item() {
    let (_temp, mut store, mut actor) = legacy("open");
    let team = store.team(TEAM).unwrap().unwrap();
    let mut ports = Fake::new();
    ports.free.clear();
    actor
        .tick(&mut store, &mut ports, &team.id, 2000, false)
        .unwrap();
    assert_eq!(
        ports.sends, 0,
        "compatibility settlement never delivers input"
    );
    let effects = store.effects(TEAM, 200, None).unwrap().rows;
    assert_eq!(effects[0].state, "refused");
    assert_eq!(effects[1].state, "uncertain");
    assert!(store
        .attempt_holder(TEAM, effects[0].node)
        .unwrap()
        .is_none());
    let items = store.inbox(TEAM, 200, None).unwrap().rows;
    assert_eq!(items[0].state, "closed");
    assert_eq!(items[1].state, "open");
    let revision = store.team(TEAM).unwrap().unwrap().revision;
    actor
        .tick(&mut store, &mut ports, &team.id, 3000, false)
        .unwrap();
    assert_eq!(
        store.team(TEAM).unwrap().unwrap().revision,
        revision,
        "no duplicate reconciliation"
    );
}

#[test]
fn old_recovery_notices_close_without_touching_the_new_holder() {
    let (_temp, mut store, mut actor) = legacy("recovered");
    let team = store.team(TEAM).unwrap().unwrap();
    let before = store.effects(TEAM, 200, None).unwrap().rows;
    let holder = store.attempt_holder(TEAM, before[0].node).unwrap();
    assert!(store
        .inbox(TEAM, 200, None)
        .unwrap()
        .rows
        .iter()
        .all(|i| i.state == "open"));
    actor.non_deliveries(&mut store, &team, 2000).unwrap();
    assert_eq!(store.attempt_holder(TEAM, before[0].node).unwrap(), holder);
    assert_eq!(store.effects(TEAM, 200, None).unwrap().rows, before);
    let items = store.inbox(TEAM, 200, None).unwrap().rows;
    assert_eq!(items[0].kind, "delivery_uncertain");
    assert_eq!(items[0].state, "closed");
    assert!(items
        .iter()
        .any(|i| i.kind.starts_with("reopen_refused:") && i.state == "closed"));
    let settled = store.team(TEAM).unwrap().unwrap().revision;
    actor.non_deliveries(&mut store, &team, 3000).unwrap();
    assert_eq!(store.team(TEAM).unwrap().unwrap().revision, settled);
}

#[test]
fn a_refused_reopen_notice_stays_open_until_the_lead_releases_the_hold() {
    let (mut h, mut actor, mut ports) = setup(0);
    let lead = h.bind("lead");
    let node = h.add(&person(), 1);
    h.submit(node, "m1");
    h.report(node, "m1");
    h.run(
        &lead,
        Operation::Return {
            node,
            reason: "Revise the result".into(),
        },
    )
    .unwrap();
    let reopen = h.reopen(node, "m1");
    let notice = format!("reopen_refused:{reopen}");
    h.reopen_outcome(
        &reopen,
        HandoutOutcome::CuttingOver {
            reason: "The fence is held. Nothing was sent.".into(),
        },
    );
    let other = h.add(&person(), 2);
    let unknown = h.intent(other, "m2");
    h.outcome(
        &unknown,
        HandoutOutcome::Uncertain {
            reason: "The original delivery receipt is unavailable.".into(),
        },
    );
    ports.free.clear();
    tick(&mut h, &mut actor, &mut ports, 2000, false);
    assert!(h.node(node).hold);
    assert!(h
        .store
        .inbox(TEAM, 200, None)
        .unwrap()
        .rows
        .iter()
        .any(|item| item.kind == notice && item.state == "open"));

    h.run(&lead, Operation::Release { node }).unwrap();
    tick(&mut h, &mut actor, &mut ports, 3000, false);
    assert!(!h.node(node).hold);
    assert_eq!(ports.sends, 0);
    let items = h.store.inbox(TEAM, 200, None).unwrap().rows;
    assert!(items
        .iter()
        .any(|item| item.kind == notice && item.state == "closed"));
    assert!(items
        .iter()
        .any(|item| item.kind == "delivery_uncertain" && item.state == "open"));
    let open = h.store.open_effects(TEAM, 200, None).unwrap().rows;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].request_id, unknown);
    assert_eq!(open[0].state, "uncertain");
}

#[test]
fn matching_words_and_time_do_not_close_another_generations_genuine_uncertainty() {
    let (_temp, mut store, mut actor) = legacy("generation");
    let team = store.team(TEAM).unwrap().unwrap();
    let before = store.inbox(TEAM, 200, None).unwrap().rows;
    assert_eq!(before.first().unwrap().text, before.last().unwrap().text);
    assert_eq!(
        before.first().unwrap().created_ms,
        before.last().unwrap().created_ms
    );
    actor.non_deliveries(&mut store, &team, 2000).unwrap();
    let items = store.inbox(TEAM, 200, None).unwrap().rows;
    assert_eq!(items[0].state, "open");
    assert_eq!(items.last().unwrap().state, "closed");
}

#[test]
fn non_delivery_history_advances_one_bounded_page_instead_of_rescanning() {
    let (mut h, mut actor, _) = setup(1);
    let node = h.store.queue(TEAM, 200, None).unwrap().rows[0].id;
    for _ in 0..205 {
        let id = h.intent(node, "m1");
        h.outcome(
            &id,
            HandoutOutcome::Refused {
                reason: "No delivery".into(),
            },
        );
    }
    assert_eq!(h.store.non_delivery_history(TEAM).unwrap().rows.len(), 200);
    let team = h.store.team(TEAM).unwrap().unwrap();
    actor.non_deliveries(&mut h.store, &team, 2000).unwrap();
    assert_eq!(h.store.non_delivery_history(TEAM).unwrap().rows.len(), 5);
    actor.non_deliveries(&mut h.store, &team, 3000).unwrap();
    assert!(h.store.non_delivery_history(TEAM).unwrap().rows.is_empty());
}

#[cfg(unix)]
#[test]
fn real_cutover_fence_delivers_exactly_the_new_effect() {
    let seen = crate::tasks::fence_delivery_cycle(|reply, id| {
        let raw = serde_json::to_vec(reply).unwrap();
        super::super::super::reply::decode_delivery(&raw, id)
    });
    assert_eq!(seen["sent"], 1);
    assert_ne!(seen["old"], seen["new"]);
}

#[test]
fn an_old_outcome_can_only_be_reconciled_with_its_exact_original_proof() {
    let (_temp, mut store, _actor) = legacy("open");
    let before = store.effects(TEAM, 200, None).unwrap().rows;
    let revision = store.team(TEAM).unwrap().unwrap().revision;
    for outcome in [
        HandoutOutcome::CuttingOver {
            reason: "A different refusal".into(),
        },
        HandoutOutcome::Uncertain {
            reason: "Nothing was sent.".into(),
        },
        HandoutOutcome::Submitted {
            receipt: "late receipt".into(),
        },
    ] {
        let request = crate::teams::ops::tests::request(
            revision,
            Operation::HandoutOutcome {
                effect_request_id: before[0].request_id.clone(),
                outcome,
            },
        );
        let error = crate::teams::ops::apply(
            &mut store,
            &crate::teams::ops::tests::service(),
            &request,
            2000,
        )
        .unwrap_err();
        assert_eq!(error.code, code::INVALID_STATE);
    }
    assert_eq!(store.effects(TEAM, 200, None).unwrap().rows, before);
    assert!(store
        .inbox(TEAM, 200, None)
        .unwrap()
        .rows
        .iter()
        .all(|i| i.state == "open"));
}

/// Only an exact cutting_over reply from a child that exited 0 is proof of
/// non-delivery; every unknown child end stays uncertain.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn only_an_exact_cutting_over_reply_from_a_settled_child_proves_non_delivery() {
    use super::super::super::native::delivery_reply;
    use crate::desktop::teams::child::Ended;
    let id = "11111111-1111-4111-8111-111111111111";
    let reply = |state: &str, schema: u64, for_id: &str| {
        serde_json::to_vec(&serde_json::json!({"schema":schema,"id":for_id,"team":TEAM,"node":1,"member":"m1","state":state,"error":"Nothing was sent."})).unwrap()
    };
    let cases: Vec<(&str, Ended)> = vec![
        (
            "exact cutting_over",
            Ended::Exited {
                code: 0,
                output: reply("cutting_over", 1, id),
            },
        ),
        (
            "cutting_over, failed exit",
            Ended::Exited {
                code: 1,
                output: reply("cutting_over", 1, id),
            },
        ),
        (
            "cutting_over, other id",
            Ended::Exited {
                code: 0,
                output: reply("cutting_over", 1, "22222222-2222-4222-8222-222222222222"),
            },
        ),
        (
            "cutting_over, schema 2",
            Ended::Exited {
                code: 0,
                output: reply("cutting_over", 2, id),
            },
        ),
        (
            "uncertain",
            Ended::Exited {
                code: 0,
                output: reply("uncertain", 1, id),
            },
        ),
        (
            "pending",
            Ended::Exited {
                code: 0,
                output: reply("pending", 1, id),
            },
        ),
        (
            "storage_failed",
            Ended::Exited {
                code: 0,
                output: reply("storage_failed", 1, id),
            },
        ),
        (
            "not JSON",
            Ended::Exited {
                code: 0,
                output: b"cutting_over".to_vec(),
            },
        ),
        ("timeout", Ended::Unknown("deadline")),
    ];
    for (name, ended) in cases {
        let outcome = delivery_reply(ended, id);
        eprintln!("DELIVERY-DECODE {name} -> {outcome:?}");
        let proof = matches!(outcome, HandoutOutcome::CuttingOver { .. });
        assert_eq!(proof, name == "exact cutting_over", "{name}");
        if !proof {
            assert!(
                matches!(outcome, HandoutOutcome::Uncertain { .. }),
                "{name}: {outcome:?}"
            );
        }
    }
}
