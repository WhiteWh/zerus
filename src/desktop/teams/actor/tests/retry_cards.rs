//! A closed stale card is not a lasting dismissal of a renewed condition.
use super::super::Runtime;
use super::*;
use crate::teams::ops::{self, Operation};
struct Fixture {
    h: Harness,
    runtime: Runtime,
    key: String,
}
fn fixture(held: bool) -> Fixture {
    let mut h = Harness::new();
    h.add(&person(), 1);
    let team = h.store.team(TEAM).unwrap().unwrap();
    Fixture {
        runtime: Runtime::open(h.temp.path(), &team.home).unwrap(),
        h,
        key: if held { "held:finish:1" } else { "report:1" }.into(),
    }
}
fn fail(f: &mut Fixture, now: i64) {
    let team = f.h.store.team(TEAM).unwrap().unwrap();
    f.runtime
        .force_failure(
            &mut f.h.store,
            &team,
            &f.key,
            "revision",
            &super::super::problem("Task 1 waits for its accepted execution to become current."),
            now,
        )
        .unwrap();
}
fn restart(f: Fixture) -> Fixture {
    let Fixture { h, runtime, key } = f;
    drop(runtime);
    let team = h.store.team(TEAM).unwrap().unwrap();
    Fixture {
        runtime: Runtime::open(h.temp.path(), &team.home).unwrap(),
        h,
        key,
    }
}
fn cards(f: &Fixture) -> Vec<crate::teams::store::Card> {
    f.h.store.cards(TEAM, 200, None).unwrap().rows
}
fn open(f: &Fixture) -> Vec<crate::teams::store::Card> {
    cards(f).into_iter().filter(|c| c.state == "open").collect()
}
fn gate(f: &Fixture) -> super::super::ledger::Row {
    f.runtime
        .ledger
        .get(TEAM, &format!("gate:{}", f.key))
        .unwrap()
        .unwrap()
}
fn recover(f: &mut Fixture, now: i64) {
    let team = f.h.store.team(TEAM).unwrap().unwrap();
    f.runtime
        .recovered(&mut f.h.store, &team, &f.key, now)
        .unwrap();
}
pub(super) fn crash(point: &'static str, action: impl FnOnce()) {
    super::super::retry::CRASH.with(|s| s.set(Some(point)));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action));
    let remaining = super::super::retry::CRASH.with(|s| s.replace(None));
    assert!(result.is_err() && remaining.is_none(), "unvisited {point}");
}
fn decide(f: &mut Fixture, card: u64, decision: &str) {
    f.h.run(
        &person(),
        Operation::DecideCard {
            card,
            decision: decision.into(),
        },
    )
    .unwrap();
}
// A saved pre-upgrade opener is immutable. Keep its actual receipt and list,
// rather than editing a card in place to manufacture a replacement fixture.
fn legacy(f: &mut Fixture, apply: bool) -> crate::teams::ops::Request {
    fail(f, 100);
    recover(f, 101);
    fail(f, 102);
    let current = open(f).remove(0).seq;
    f.h.run(
        &service(),
        Operation::CloseCard {
            card: current,
            reason: "Replacing the fixture's opener".into(),
        },
    )
    .unwrap();
    let (_, request) = super::super::request(
        &f.h.store,
        TEAM,
        Operation::OpenCard {
            kind: "team_actor_blocked".into(),
            node: f.key.starts_with("held:").then_some(1),
            text: "The earlier service check failed".into(),
            decisions: Some(vec!["acknowledged".into(), "later".into()]),
                parameters: None,
        },
    )
    .unwrap();
    let id = apply.then(|| {
        ops::apply(&mut f.h.store, &service(), &request, 103)
            .unwrap()
            .item
            .unwrap()
    });
    let mut row = gate(f);
    row.data["card"] = json!(id);
    row.data["notice"] = serde_json::to_value(&request).unwrap();
    f.runtime.ledger.save(&mut row).unwrap();
    request
}
#[test]
fn both_actor_card_paths_list_retry_first_and_acknowledged_second() {
    for held in [false, true] {
        let mut f = fixture(held);
        fail(&mut f, 100);
        let current = open(&f).remove(0);
        assert_eq!(current.decisions, ["retry", "acknowledged"]);
        assert_eq!(current.node, held.then_some(1));
        assert!(!current.text.starts_with("held:"));
    }
}
#[test]
fn recovery_close_then_restart_before_gate_removal_is_not_a_dismissal_for_either_card() {
    for held in [false, true] {
        let mut f = fixture(held);
        fail(&mut f, 100);
        let old = open(&f).remove(0).seq;
        // This is exactly close_gate's durable cut: CloseCard committed,
        // while the old gate still names that card and its opener receipt.
        f.h.run(
            &service(),
            Operation::CloseCard {
                card: old,
                reason: "The service check recovered".into(),
            },
        )
        .unwrap();
        assert!(f
            .runtime
            .ledger
            .get(TEAM, &format!("gate:{}", f.key))
            .unwrap()
            .is_some());
        f = restart(f);
        fail(&mut f, 200);
        let current = open(&f);
        assert_eq!(
            current.len(),
            1,
            "a stale close is not a dismissal; held={held}"
        );
        assert_ne!(current[0].seq, old);
        assert_eq!(current[0].decisions, ["retry", "acknowledged"]);
        assert_eq!(
            f.h.store
                .card(TEAM, old)
                .unwrap()
                .unwrap()
                .decision
                .as_deref(),
            Some("stale")
        );
    }
}

#[test]
fn both_cards_replace_an_old_list_at_every_durable_step_after_restart() {
    for held in [false, true] {
        for point in [
            "gate_saved",
            "replacement_closed",
            "replacement_reset",
            "notice_reserved",
            "notice_applied",
            "notice_saved",
        ] {
            let mut f = fixture(held);
            legacy(&mut f, true);
            let old = open(&f).remove(0).seq;
            let before = cards(&f).len();
            crash(point, || fail(&mut f, 200));
            let reserved = gate(&f).data["notice"]["request_id"].clone();
            f = restart(f);
            fail(&mut f, 201);
            fail(&mut f, 202);
            let current = open(&f);
            assert_eq!(current.len(), 1, "{point}, held={held}");
            assert_ne!(current[0].seq, old);
            assert_eq!(current[0].decisions, ["retry", "acknowledged"]);
            assert_eq!(cards(&f).len(), before + 1, "{point}, held={held}");
            if point.starts_with("notice_") {
                assert_eq!(gate(&f).data["notice"]["request_id"], reserved);
            }
            assert_eq!(gate(&f).data["card"], current[0].seq);
        }
    }
}

#[test]
fn both_card_recoveries_finish_after_restart_at_each_durable_step() {
    for held in [false, true] {
        for point in [
            "recovery_before_close",
            "recovery_closed",
            "recovery_removed",
        ] {
            let mut f = fixture(held);
            fail(&mut f, 100);
            let old = open(&f).remove(0).seq;
            crash(point, || recover(&mut f, 200));
            f = restart(f);
            recover(&mut f, 201);
            assert!(open(&f).is_empty(), "{point}, held={held}");
            assert!(f
                .runtime
                .ledger
                .get(TEAM, &format!("gate:{}", f.key))
                .unwrap()
                .is_none());
            fail(&mut f, 202);
            assert_eq!(open(&f).len(), 1);
            assert_ne!(open(&f)[0].seq, old);
            assert_eq!(cards(&f).len(), 2);
        }
    }
}

#[test]
fn committed_recovery_close_then_restart_and_failure_replaces_both_stale_cards() {
    for held in [false, true] {
        let mut f = fixture(held);
        fail(&mut f, 100);
        let old = open(&f).remove(0).seq;
        crash("recovery_closed", || recover(&mut f, 200));
        assert_eq!(gate(&f).data["card"], old);
        assert_eq!(
            f.h.store
                .card(TEAM, old)
                .unwrap()
                .unwrap()
                .decision
                .as_deref(),
            Some("stale")
        );
        f = restart(f);
        fail(&mut f, 201);
        assert_eq!(open(&f).len(), 1);
        assert_ne!(open(&f)[0].seq, old);
        assert_eq!(cards(&f).len(), 2);
    }
}

#[test]
fn both_cards_keep_only_a_current_dismissal_and_consume_retry_after_restart() {
    for held in [false, true] {
        for decision in ["acknowledged", "retry"] {
            let mut f = fixture(held);
            fail(&mut f, 100);
            let old = open(&f).remove(0).seq;
            decide(&mut f, old, decision);
            f = restart(f);
            let team = f.h.store.team(TEAM).unwrap().unwrap();
            let due = f
                .runtime
                .due(&mut f.h.store, &team, &f.key, "revision", 101)
                .unwrap();
            assert_eq!(due, decision == "retry");
            for now in [102, 60_100, 180_100, 1_800_100] {
                fail(&mut f, now);
            }
            assert_eq!(cards(&f).len(), if decision == "retry" { 2 } else { 1 });
            assert_eq!(open(&f).len(), usize::from(decision == "retry"));
        }
    }
}

#[test]
fn an_old_list_or_an_unsupported_decision_does_not_dismiss_either_current_card() {
    for held in [false, true] {
        for decision in ["acknowledged", "later"] {
            let mut f = fixture(held);
            legacy(&mut f, true);
            let old = open(&f).remove(0).seq;
            decide(&mut f, old, decision);
            f = restart(f);
            fail(&mut f, 200);
            assert_eq!(open(&f).len(), 1);
            assert_ne!(open(&f)[0].seq, old);
            assert_eq!(open(&f)[0].decisions, ["retry", "acknowledged"]);
        }
    }
}

#[test]
fn an_interrupted_legacy_notice_is_replayed_then_replaced_for_both_cards() {
    for held in [false, true] {
        for applied in [false, true] {
            let mut f = fixture(held);
            let request = legacy(&mut f, applied);
            // The opener's saved reply was lost before its id reached the gate.
            let mut row = gate(&f);
            row.data["card"] = json!(null);
            f.runtime.ledger.save(&mut row).unwrap();
            let before = cards(&f).len();
            f = restart(f);
            fail(&mut f, 200);
            assert_eq!(open(&f).len(), 1, "applied={applied}, held={held}");
            assert_eq!(open(&f)[0].decisions, ["retry", "acknowledged"]);
            assert_ne!(gate(&f).data["notice"]["request_id"], request.request_id);
            assert_eq!(cards(&f).len(), before + if applied { 1 } else { 2 });
        }
    }
}

#[test]
fn recovery_replays_an_interrupted_notice_before_removing_either_gate() {
    for held in [false, true] {
        for point in ["notice_reserved", "notice_applied"] {
            let mut f = fixture(held);
            crash(point, || fail(&mut f, 100));
            let request = gate(&f).data["notice"]["request_id"].clone();
            f = restart(f);
            recover(&mut f, 200);
            assert!(open(&f).is_empty(), "{point}, held={held}");
            assert!(f
                .runtime
                .ledger
                .get(TEAM, &format!("gate:{}", f.key))
                .unwrap()
                .is_none());
            assert_eq!(cards(&f).len(), 1);
            let journal = f.h.store.journal(TEAM, 200, None).unwrap().rows;
            assert_eq!(
                journal
                    .iter()
                    .filter(|e| e.kind == "open_card" && e.request_id == request)
                    .count(),
                1
            );
        }
    }
}

fn newer_failure_at(
    f: &Fixture,
    point: &'static str,
) -> std::rc::Rc<std::cell::RefCell<Option<super::super::ledger::Row>>> {
    let mut store = Store::open(f.h.temp.path()).unwrap();
    let team = store.team(TEAM).unwrap().unwrap();
    let mut runtime = Runtime::open(f.h.temp.path(), &team.home).unwrap();
    let key = f.key.clone();
    let observed = std::rc::Rc::new(std::cell::RefCell::new(None));
    let capture = observed.clone();
    super::super::retry::INTERLEAVE.with(|slot| {
        *slot.borrow_mut() = Some((
            point,
            Box::new(move || {
                runtime
                    .force_failure(
                        &mut store,
                        &team,
                        &key,
                        "revision",
                        &super::super::problem("The check failed in the newer runtime"),
                        200,
                    )
                    .unwrap();
                *capture.borrow_mut() = Some(
                    runtime
                        .ledger
                        .get(TEAM, &format!("gate:{key}"))
                        .unwrap()
                        .unwrap(),
                );
            }),
        ));
    });
    observed
}
fn recovery_keeps_a_newer_gate(held: bool) {
    let mut f = fixture(held);
    fail(&mut f, 100);
    let old = open(&f).remove(0).seq;
    let observed = newer_failure_at(&f, "recovery_closed");
    recover(&mut f, 201);
    let newer = observed.borrow_mut().take().expect("second runtime ran");
    assert_eq!(
        f.runtime.ledger.get(TEAM, &newer.key).unwrap(),
        Some(newer.clone())
    );
    let current = open(&f);
    assert_eq!(current.len(), 1);
    assert_ne!(current[0].seq, old);
    assert_eq!(newer.data["card"], current[0].seq);
    fail(&mut f, 202);
    assert_eq!(open(&f)[0].seq, current[0].seq);
    assert_eq!(cards(&f).len(), 2, "the newer card is still tracked");
    decide(&mut f, current[0].seq, "retry");
    let team = f.h.store.team(TEAM).unwrap().unwrap();
    assert!(f
        .runtime
        .due(&mut f.h.store, &team, &f.key, "revision", 203)
        .unwrap());
    fail(&mut f, 204);
    let card = open(&f).remove(0).seq;
    assert_ne!(card, current[0].seq);
    assert!(!f
        .runtime
        .due(&mut f.h.store, &team, &f.key, "revision", 205)
        .unwrap());
    assert_eq!(gate(&f).data["card"], card);
    assert_eq!(cards(&f).len(), 3, "Retry was consumed once");
}
#[test]
fn gate_recovery_keeps_the_row_and_card_written_by_a_second_runtime() {
    recovery_keeps_a_newer_gate(false);
}
#[test]
fn held_card_recovery_keeps_the_row_and_card_written_by_a_second_runtime() {
    recovery_keeps_a_newer_gate(true);
}

#[test]
fn card_replacement_and_notice_writes_never_adopt_a_newer_writers_version() {
    for held in [false, true] {
        for point in [
            "replacement_closed",
            "replacement_reset",
            "notice_reserved",
            "notice_applied",
        ] {
            let mut f = fixture(held);
            if point.starts_with("replacement_") {
                legacy(&mut f, true);
            }
            let observed = newer_failure_at(&f, point);
            fail(&mut f, 201);
            let newer = observed.borrow_mut().take().expect("second runtime ran");
            assert_eq!(
                f.runtime.ledger.get(TEAM, &newer.key).unwrap(),
                Some(newer.clone()),
                "{point}, held={held}"
            );
            let current = open(&f);
            assert_eq!(current.len(), 1, "{point}, held={held}");
            assert_eq!(newer.data["card"], current[0].seq);
            let count = cards(&f).len();
            fail(&mut f, 202);
            assert_eq!(open(&f)[0].seq, current[0].seq);
            assert_eq!(cards(&f).len(), count);
        }
    }
}

#[test]
fn an_owned_gate_write_advances_only_its_own_successful_version() {
    let mut f = fixture(false);
    fail(&mut f, 100);
    let mut owned = gate(&f);
    let mut newer = owned.clone();
    newer.data["revision"] = json!("newer observation");
    f.runtime.ledger.save(&mut newer).unwrap();
    assert!(!f.runtime.ledger.save_if_current(&mut owned).unwrap());
    assert_eq!(owned.version + 1, newer.version);
    assert_eq!(gate(&f), newer);
    assert!(f.runtime.ledger.save_if_current(&mut newer).unwrap());
    assert_eq!(gate(&f), newer);
    f.runtime.ledger.remove(&owned).unwrap();
    assert_eq!(gate(&f), newer, "a stale removal is a harmless non-write");
}

/// A reason that ends with a period is not followed by another one
/// (zerus-63rru.23): the sentence is built once, for every card.
#[test]
fn a_reason_that_ends_with_a_period_is_not_doubled() {
    let mut f = fixture(false);
    let team = f.h.store.team(TEAM).unwrap().unwrap();
    for now in [1000i64, 1000 + 60_000, 1000 + 180_000] {
        f.runtime
            .force_failure(
                &mut f.h.store,
                &team,
                &f.key,
                "revision",
                &super::super::problem("The task waits."),
                now,
            )
            .unwrap();
    }
    let card = open(&f).remove(0);
    assert!(card.text.contains("waits. Automatic checks"), "{:?}", card.text);
    assert!(!card.text.contains("waits.. Automatic"), "{:?}", card.text);
}
