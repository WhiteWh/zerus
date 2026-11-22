//! Hand-out state is produced only through the real store operations.
use super::*;

#[test]
fn cutting_over_is_not_delivered_and_frees_both_reservations() {
    let mut h = Harness::new();
    h.bind("m1");
    let node = h.add(&person(), 1);
    let old = h.intent(node, "m1");
    h.outcome(
        &old,
        HandoutOutcome::CuttingOver {
            reason: "This task is cutting over (held). Nothing was sent.".into(),
        },
    );
    assert!(h
        .store
        .open_effects(TEAM, 200, None)
        .unwrap()
        .rows
        .is_empty());
    assert!(h.store.attempt_holder(TEAM, node).unwrap().is_none());
    assert!(h.store.inbox(TEAM, 200, None).unwrap().rows.is_empty());
    refused(
        h.run(
            &service(),
            Operation::HandoutIntent {
                node,
                member: "m1".into(),
                effect_request_id: old.clone(),
            },
        ),
        code::INVALID_STATE,
    );
    let new = h.intent(node, "m1");
    assert_ne!(old, new);
    refused(
        h.run(
            &service(),
            Operation::HandoutOutcome {
                effect_request_id: old,
                outcome: HandoutOutcome::Submitted {
                    receipt: "late".into(),
                },
            },
        ),
        code::INVALID_STATE,
    );
}

#[test]
fn uncertain_words_never_prove_non_delivery() {
    for reason in [
        "Nothing was sent.",
        "Cutting over; nothing was sent, but the receipt is unknown",
    ] {
        let mut h = Harness::new();
        h.bind("m1");
        let node = h.add(&person(), 1);
        let other = h.add(&person(), 2);
        let id = h.intent(node, "m1");
        h.outcome(
            &id,
            HandoutOutcome::Uncertain {
                reason: reason.into(),
            },
        );
        assert_eq!(
            h.store.open_effects(TEAM, 200, None).unwrap().rows[0].state,
            "uncertain"
        );
        for target in [node, other] {
            refused(
                h.run(
                    &service(),
                    Operation::HandoutIntent {
                        node: target,
                        member: "m1".into(),
                        effect_request_id: uuid::Uuid::new_v4().to_string(),
                    },
                ),
                code::INVALID_STATE,
            );
        }
    }
}

#[test]
fn cutover_reopen_keeps_its_handback_with_one_definite_refusal_notice() {
    let mut h = Harness::new();
    let (node, lead) = h.reported();
    h.run(
        &lead,
        Operation::Return {
            node,
            reason: "Revise the result".into(),
        },
    )
    .unwrap();
    let id = h.reopen(node, "m1");
    h.reopen_outcome(
        &id,
        HandoutOutcome::CuttingOver {
            reason: "Fence held. Nothing was sent.".into(),
        },
    );
    assert!(
        h.node(node).hold,
        "a refused reopen still waits for the lead"
    );
    assert!(h
        .store
        .open_effects(TEAM, 200, None)
        .unwrap()
        .rows
        .is_empty());
    let items = h.store.inbox(TEAM, 200, None).unwrap().rows;
    assert!(items.iter().all(|i| i.kind != "delivery_uncertain"));
    let notices: Vec<_> = items
        .iter()
        .filter(|i| i.kind == format!("reopen_refused:{id}") && i.state == "open")
        .collect();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].text.contains("Fence held. Nothing was sent."));
    refused(
        h.run(
            &service(),
            Operation::ReopenOutcome {
                effect_request_id: id,
                outcome: HandoutOutcome::CuttingOver {
                    reason: "Fence held. Nothing was sent.".into(),
                },
            },
        ),
        code::INVALID_STATE,
    );
    assert_eq!(h.store.inbox(TEAM, 200, None).unwrap().rows, items);
}

fn refused_reopen(h: &mut Harness, lead: &Actor, number: u32) -> (u32, String) {
    let node = h.add(&person(), number);
    h.submit(node, "m1");
    h.report(node, "m1");
    h.run(
        lead,
        Operation::Return {
            node,
            reason: "Revise the result".into(),
        },
    )
    .unwrap();
    let reopen = h.reopen(node, "m1");
    h.reopen_outcome(
        &reopen,
        HandoutOutcome::CuttingOver {
            reason: "Fence held. Nothing was sent.".into(),
        },
    );
    (node, reopen)
}

#[test]
fn a_new_hand_back_does_not_keep_an_older_reopen_notice_in_its_generation() {
    let mut h = Harness::new();
    let lead = h.bind("lead");
    h.bind("m1");
    let (node, reopen) = refused_reopen(&mut h, &lead, 1);
    let old_kind = format!("reopen_refused:{reopen}");

    h.run(&lead, Operation::Release { node }).unwrap();
    h.observe(node, TaskStage::Todo);
    let handout = h.intent(node, "m1");
    h.outcome(
        &handout,
        HandoutOutcome::HandedBack {
            reason: "Member can no longer take this work.".into(),
        },
    );
    let new_kind = format!("handout_handed_back:{handout}");
    let effects = h.store.effects(TEAM, 200, None).unwrap().rows;
    assert_eq!(effects[1].request_id, reopen);
    assert_eq!(effects[2].request_id, handout);
    assert_eq!(effects[1].generation, effects[2].generation);
    assert!(h.node(node).hold);

    let notices = |h: &Harness| {
        h.store
            .inbox(TEAM, 200, None)
            .unwrap()
            .rows
            .into_iter()
            .filter(|item| item.kind == old_kind || item.kind == new_kind)
            .map(|item| (item.kind, item.state))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        notices(&h),
        [
            (old_kind.clone(), "open".to_owned()),
            (new_kind.clone(), "open".to_owned())
        ]
    );
    h.run(&service(), Operation::CloseStaleReopenNotices)
        .unwrap();
    assert_eq!(
        notices(&h),
        [
            (old_kind, "closed".to_owned()),
            (new_kind, "open".to_owned())
        ]
    );
    assert!(h.node(node).hold);
    assert_eq!(h.store.effects(TEAM, 200, None).unwrap().rows, effects);
}

#[test]
fn stale_reopen_notices_close_in_pages_of_two_hundred() {
    let mut h = Harness::new();
    let lead = h.bind("lead");
    h.bind("m1");
    let mut kinds = Vec::new();
    for number in 1..=201 {
        let (node, reopen) = refused_reopen(&mut h, &lead, number);
        h.run(&lead, Operation::Release { node }).unwrap();
        kinds.push(format!("reopen_refused:{reopen}"));
    }
    let notices = |h: &Harness| {
        let mut rows = Vec::new();
        let mut cursor = None;
        for _ in 0..3 {
            let page = h.store.inbox(TEAM, 200, cursor.as_ref()).unwrap();
            rows.extend(
                page.rows
                    .into_iter()
                    .filter(|row| row.kind.starts_with("reopen_refused:")),
            );
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        assert!(cursor.is_none());
        assert!(rows.iter().map(|row| &row.kind).eq(&kinds));
        rows.into_iter().map(|row| row.state).collect::<Vec<_>>()
    };
    assert_eq!(notices(&h), ["open"; 201]);
    h.run(&service(), Operation::CloseStaleReopenNotices)
        .unwrap();
    let after = notices(&h);
    assert_eq!(after[..200], ["closed"; 200]);
    assert_eq!(after[200], "open");
    h.run(&service(), Operation::CloseStaleReopenNotices)
        .unwrap();
    assert_eq!(notices(&h), ["closed"; 201]);
    refused(
        h.run(&service(), Operation::CloseStaleReopenNotices),
        code::INVALID_STATE,
    );
}

#[test]
fn only_the_service_can_close_stale_reopen_notices() {
    let mut h = Harness::new();
    let lead = h.bind("lead");
    let member = h.bind("m1");
    let (node, reopen) = refused_reopen(&mut h, &lead, 1);
    h.run(&lead, Operation::Release { node }).unwrap();
    let kind = format!("reopen_refused:{reopen}");
    let state = |h: &Harness| {
        h.store
            .inbox(TEAM, 200, None)
            .unwrap()
            .rows
            .into_iter()
            .find(|item| item.kind == kind)
            .unwrap()
            .state
    };
    for actor in [person(), lead, member, other()] {
        refused(
            h.run(&actor, Operation::CloseStaleReopenNotices),
            code::NOT_ALLOWED,
        );
        assert_eq!(state(&h), "open", "{actor:?}");
    }
    h.run(&service(), Operation::CloseStaleReopenNotices)
        .unwrap();
    assert_eq!(state(&h), "closed");
}
