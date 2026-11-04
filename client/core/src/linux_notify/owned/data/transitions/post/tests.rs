use super::super::tests::*;
use super::*;
use crate::linux_notify::owned::{
    decode_owned_for,
    inspection::{inspect_selected, Inputs},
};
use serde_json::json;

pub(super) fn ready(m: &mut Model, owner: Owner, n: u64) -> BatchKey {
    let key = m.prepare_batch(owner, batch(owner, n)).unwrap();
    let mut write = m.next_write().unwrap().unwrap();
    ack(m, &mut write, Disposition::Saved).unwrap();
    key
}
fn delivered(p: &PostProposal, id: u32) -> PostReport {
    p.report(
        namespace(),
        PostOutcome::Delivered(NonZeroU32::new(id).unwrap()),
    )
    .unwrap()
}
fn save_result(m: &mut Model) -> Vec<u8> {
    let mut write = m.next_write().unwrap().unwrap();
    let bytes = write.bytes().unwrap().to_vec();
    ack(m, &mut write, Disposition::Saved).unwrap();
    bytes
}
pub(super) fn retained(owner: Owner, state: &str, bus: &str, id: u32) -> Vec<u8> {
    let mut o = input(owner, 8);
    o.namespace.bus_id = bus.into();
    let o = Original::new(owner, o).unwrap();
    let mut row = wire::possible_value(&o);
    row["state"] = json!(state);
    row["native_id"] = json!(id);
    let raw = if owner == Owner::AgentAttention {
        envelope()
    } else {
        update()
    };
    let mut v: Value = serde_json::from_slice(&raw).unwrap();
    v["tokens"] = json!({id.to_string():row});
    serde_json::to_vec(&v).unwrap()
}

#[test]
fn proposal_waits_for_barrier_and_result_ack_blocks_the_entire_chain() {
    let mut m = model();
    let key = m
        .prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
        .unwrap();
    assert!(m.take_next_native().unwrap().is_none());
    let mut barrier = m.next_write().unwrap().unwrap();
    assert!(m.take_next_native().unwrap().is_none());
    ack(&mut m, &mut barrier, Disposition::Saved).unwrap();
    let mut p = m.take_next_native().unwrap().unwrap();
    assert_eq!(p.original().unwrap().request_id(), "request-1");
    assert_eq!(p.title(), Some("Title"));
    assert_eq!(p.body(), Some("Private body"));
    assert!(m.take_next_native().unwrap().is_none());
    assert!(m.next_write().unwrap().is_none());
    let before = m.snapshot();
    let report = delivered(&p, 42);
    m.finish_post(&mut p, report).unwrap();
    assert!(p.original().is_none());
    assert!(p.body().is_none());
    assert_eq!(
        p.report(namespace(), PostOutcome::RefusedBeforeCall)
            .unwrap_err(),
        Rejection::StaleKey
    );
    assert_eq!(m.snapshot().queued_utf8_bytes, before.queued_utf8_bytes);
    assert_eq!(m.snapshot().pending_preparations, 1);
    m.prepare_batch(Owner::SoftwareUpdate, batch(Owner::SoftwareUpdate, 2))
        .unwrap();
    assert!(m.take_next_native().unwrap().is_none());
    let mut result = m.next_write().unwrap().unwrap();
    assert_eq!(result.attempt().owner(), Owner::AgentAttention); // result priority
    assert!(m.take_next_native().unwrap().is_none());
    assert!(m.next_write().unwrap().is_none());
    let raw = result.bytes().unwrap().to_vec();
    let rows = decode_owned_for(&raw, Owner::AgentAttention).unwrap();
    let ClaimData::Retained(claim) = rows.rows().unwrap().claims()[0].data() else {
        panic!("retained result")
    };
    assert_eq!(claim.original().request_id(), "request-1");
    ack(&mut m, &mut result, Disposition::Saved).unwrap();
    assert_eq!(m.batch_status(&key).unwrap_err(), Rejection::StaleKey);
    assert_eq!(m.snapshot().pending_preparations, 1);
    assert!(m.namespace_blocked(&namespace())); // queued update remains
    assert!(m.take_next_native().unwrap().is_none()); // update barrier not saved
    assert_eq!(
        m.next_write().unwrap().unwrap().attempt().owner(),
        Owner::SoftwareUpdate
    );
}

#[test]
fn duplicate_foreign_and_wrong_original_reports_preserve_current_payload() {
    let mut a = model();
    let mut b = model();
    ready(&mut a, Owner::AgentAttention, 1);
    ready(&mut b, Owner::AgentAttention, 1);
    let mut pa = a.take_next_native().unwrap().unwrap();
    let mut pb = b.take_next_native().unwrap().unwrap();
    let good = delivered(&pa, 42);
    assert_eq!(
        b.finish_post(&mut pa, good).unwrap_err(),
        Rejection::ForeignKey
    );
    assert!(pa.body().is_some());
    assert!(pb.body().is_some());
    for mutation in 0..4 {
        let mut bad = delivered(&pa, 42);
        match mutation {
            0 => bad.key.owner = Owner::SoftwareUpdate,
            1 => bad.key.original_sha256 = "wrong".into(),
            2 => bad.key.item = 1,
            _ => bad.key.barrier.after.bytes = Some(1),
        }
        assert_eq!(
            a.finish_post(&mut pa, bad).unwrap_err(),
            Rejection::WrongAttempt
        );
        assert!(pa.body().is_some());
        assert!(a.snapshot().native_pending);
    }
    let stale = delivered(&pa, 43);
    let good = delivered(&pa, 42);
    a.finish_post(&mut pa, good).unwrap();
    save_result(&mut a);
    ready(&mut a, Owner::AgentAttention, 2);
    let mut current = a.take_next_native().unwrap().unwrap();
    assert_eq!(
        a.finish_post(&mut current, stale).unwrap_err(),
        Rejection::WrongAttempt
    );
    assert!(current.body().is_some());
    let good = delivered(&pb, 44);
    b.finish_post(&mut pb, good).unwrap();
}

#[test]
fn namespace_contradiction_precedes_before_call_classification() {
    for malformed in [false, true] {
        let mut m = model();
        ready(&mut m, Owner::AgentAttention, 1);
        let mut p = m.take_next_native().unwrap().unwrap();
        let late = delivered(&p, 42);
        let mut ns = namespace();
        ns.owner = if malformed {
            "x".repeat(20_000)
        } else {
            ":9.9".into()
        };
        let report = p.report(ns, PostOutcome::RefusedBeforeCall).unwrap();
        m.finish_post(&mut p, report).unwrap();
        assert!(m.snapshot().proof_invalidated);
        assert!(p.body().is_none());
        assert_eq!(
            m.finish_post(&mut p, late).unwrap_err(),
            Rejection::NeedsReview
        );
        assert_eq!(m.next_write().unwrap_err(), Rejection::NeedsReview);
        assert!(m.native.as_ref().unwrap().report.is_some());
    }
}

#[test]
fn terminal_uncertainty_keeps_observed_id_original_payload_and_crash_barrier() {
    for id in [None, NonZeroU32::new(42)] {
        let mut m = model();
        ready(&mut m, Owner::AgentAttention, 1);
        let mut p = m.take_next_native().unwrap().unwrap();
        let before = m.snapshot();
        let image = m.stores[0].image.clone();
        let report = p
            .report(namespace(), PostOutcome::Unknown { native_id: id })
            .unwrap();
        m.finish_post(&mut p, report).unwrap();
        let n = m.native.as_ref().unwrap();
        assert_eq!(n.payload.original.request_id(), "request-1");
        assert!(
            matches!(&n.report.as_ref().unwrap().outcome, PostOutcome::Unknown { native_id } if *native_id == id)
        );
        assert_eq!(m.snapshot().queued_utf8_bytes, before.queued_utf8_bytes);
        assert_eq!(m.stores[0].image, image);
        assert!(m.namespace_blocked(&namespace()));
        assert!(inspect_selected(Inputs {
            agent: Some(&serde_json::to_vec(&m.stores[0].value).unwrap()),
            update: Some(&update()),
            legacy: None
        })
        .is_err());
    }
    let mut m = model();
    ready(&mut m, Owner::AgentAttention, 1);
    let mut p = m.take_next_native().unwrap().unwrap();
    let r = p
        .report(namespace(), PostOutcome::RefusedBeforeCall)
        .unwrap();
    m.finish_post(&mut p, r).unwrap();
    assert!(m.snapshot().proof_invalidated);
}

#[test]
fn result_write_failure_or_loss_never_refunds_or_allows_late_success() {
    for disposition in [Disposition::Unknown, Disposition::RefusedBeforeWrite] {
        let mut m = model();
        ready(&mut m, Owner::AgentAttention, 1);
        let mut p = m.take_next_native().unwrap().unwrap();
        let r = delivered(&p, 42);
        m.finish_post(&mut p, r).unwrap();
        let before = m.snapshot();
        let mut w = m.next_write().unwrap().unwrap();
        assert!(m
            .image_report(Owner::AgentAttention)
            .attempted_unconfirmed
            .is_some());
        ack(&mut m, &mut w, disposition).unwrap();
        assert_eq!(m.result.as_ref().unwrap().id.get(), 42);
        assert!(m.snapshot().proof_invalidated && m.snapshot().write_pending);
        assert_eq!(m.snapshot().queued_utf8_bytes, before.queued_utf8_bytes);
        assert_eq!(
            ack(&mut m, &mut w, Disposition::Saved).unwrap_err(),
            Rejection::NeedsReview
        );
        m.close_agent_inputs();
        assert_eq!(m.freeze_agent_writes().unwrap_err(), Rejection::NeedsReview);
    }
    let mut m = model();
    ready(&mut m, Owner::AgentAttention, 1);
    let mut p = m.take_next_native().unwrap().unwrap();
    let r = delivered(&p, 42);
    m.finish_post(&mut p, r).unwrap();
    drop(m.next_write().unwrap().unwrap());
    assert!(m.snapshot().write_pending && m.snapshot().result_pending);
    assert!(!m.snapshot().write_unknown);
    assert!(m.take_next_native().unwrap().is_none());
    assert!(m.next_write().unwrap().is_none());
    let mut lost = model();
    ready(&mut lost, Owner::AgentAttention, 1);
    drop(lost.take_next_native().unwrap().unwrap());
    assert!(lost.snapshot().native_pending);
    assert!(lost.namespace_blocked(&namespace()));
    assert!(lost.next_write().unwrap().is_none());
}

#[test]
fn every_positive_claim_collides_in_both_directions_without_overwriting() {
    for old_owner in [Owner::AgentAttention, Owner::SoftwareUpdate] {
        for new_owner in [Owner::AgentAttention, Owner::SoftwareUpdate] {
            for state in [
                "delivered",
                "unknown_post",
                "withdrawal_pending",
                "unknown_withdrawal",
            ] {
                let old = retained(old_owner, state, BUS, 42);
                let a = if old_owner == Owner::AgentAttention {
                    old.clone()
                } else {
                    envelope()
                };
                let u = if old_owner == Owner::SoftwareUpdate {
                    old
                } else {
                    update()
                };
                let mut m = model_with(&a, &u, None);
                ready(&mut m, new_owner, 9);
                let before = [m.stores[0].image.clone(), m.stores[1].image.clone()];
                let mut p = m.take_next_native().unwrap().unwrap();
                let r = delivered(&p, 42);
                m.finish_post(&mut p, r).unwrap();
                assert!(m.snapshot().proof_invalidated);
                assert!(m.result.is_none());
                assert_eq!(
                    [m.stores[0].image.clone(), m.stores[1].image.clone()],
                    before
                );
                if old_owner == Owner::SoftwareUpdate && new_owner == Owner::SoftwareUpdate {
                    assert_eq!(m.next_write().unwrap_err(), Rejection::NeedsReview);
                } else {
                    let plan = m.next_write().unwrap().unwrap();
                    assert_eq!(plan.attempt().phase(), WritePhase::CollisionInvalidations);
                    assert_eq!(plan.attempt().owner(), Owner::SoftwareUpdate);
                }
            }
        }
    }
}

#[test]
fn occupied_slot_across_namespaces_and_same_owner_uncertainty_are_not_adopted() {
    let other = "ffffffffffffffffffffffffffffffff";
    let old = retained(Owner::AgentAttention, "delivered", other, 42);
    let mut m = model_with(&old, &update(), None);
    ready(&mut m, Owner::AgentAttention, 9);
    let mut p = m.take_next_native().unwrap().unwrap();
    let r = delivered(&p, 42);
    m.finish_post(&mut p, r).unwrap();
    assert!(m.snapshot().proof_invalidated);
    let mut a: Value =
        serde_json::from_slice(&retained(Owner::AgentAttention, "delivered", BUS, 42)).unwrap();
    let mut row = wire::possible_value(&post(Owner::AgentAttention, 9).original);
    row["state"] = json!("unknown_post");
    row["native_id"] = json!(42);
    a["uncertain"] = json!([row]);
    let raw = serde_json::to_vec(&a).unwrap();
    assert_eq!(
        decode_owned_for(&raw, Owner::AgentAttention)
            .unwrap()
            .rows()
            .unwrap()
            .claims()
            .len(),
        2
    );
    let selected = inspect_selected(Inputs {
        agent: Some(&raw),
        update: Some(&update()),
        legacy: None,
    })
    .unwrap();
    assert_eq!(
        Model::from_selected(selected, HOST.into()).unwrap_err(),
        Rejection::InvalidInput
    );
}

#[test]
fn known_namespace_refusal_happens_before_proposal_and_only_empty_store_adopts() {
    let mut a: Value = serde_json::from_slice(&envelope()).unwrap();
    a["owner"] = json!(":9.9");
    let mut m = model_with(&serde_json::to_vec(&a).unwrap(), &update(), None);
    ready(&mut m, Owner::AgentAttention, 1);
    assert_eq!(m.take_next_native().unwrap_err(), Rejection::NeedsReview);
    assert!(!m.snapshot().native_pending);
    a["owner"] = json!("");
    a["bus_id"] = json!("");
    let mut m = model_with(&serde_json::to_vec(&a).unwrap(), &update(), None);
    ready(&mut m, Owner::AgentAttention, 1);
    assert!(envelope_matches(&m.stores[0].value, &namespace()));
    assert!(m.take_next_native().unwrap().is_some());
}

#[test]
fn retirement_waits_for_all_items_and_preserves_raw_capacity_and_anti_reuse() {
    let mut m = model();
    let key = m
        .prepare_batch(
            Owner::AgentAttention,
            PostBatch {
                raised: vec![
                    post(Owner::AgentAttention, 1),
                    post(Owner::AgentAttention, 2),
                ],
            },
        )
        .unwrap();
    let mut w = m.next_write().unwrap().unwrap();
    ack(&mut m, &mut w, Disposition::Saved).unwrap();
    let bytes = m.queued_bytes;
    let reserve = (m.reserved_entries, m.reserved_bytes);
    for n in [1, 2] {
        let mut p = m.take_next_native().unwrap().unwrap();
        let r = delivered(&p, 40 + n);
        m.finish_post(&mut p, r).unwrap();
        assert_eq!(m.queued_bytes, bytes);
        let raw = save_result(&mut m);
        if n == 1 {
            assert_eq!(m.queued_bytes, bytes);
        }
        let selected = inspect_selected(Inputs {
            agent: Some(&raw),
            update: Some(&update()),
            legacy: None,
        });
        if n == 1 {
            assert!(selected.is_err());
        } else {
            let mut restart = Model::from_selected(selected.unwrap(), HOST.into()).unwrap();
            assert!(restart.take_next_native().unwrap().is_none());
        }
    }
    assert_eq!(m.queued_bytes, 0);
    assert_eq!(m.snapshot().pending_preparations, 0);
    assert_eq!((m.reserved_entries, m.reserved_bytes), reserve);
    assert_eq!(m.batch_status(&key).unwrap_err(), Rejection::StaleKey);
    let mut reused = input(Owner::AgentAttention, 3);
    reused.request_id = "request-1".into();
    let mut p = post(Owner::AgentAttention, 3);
    p.original = Original::new(Owner::AgentAttention, reused).unwrap();
    assert_eq!(
        m.prepare_batch(Owner::AgentAttention, PostBatch { raised: vec![p] })
            .unwrap_err(),
        Rejection::ReusedRequest
    );
}

#[test]
fn preparation_and_queued_byte_limits_can_reopen_only_after_saved_results() {
    let mut m = model();
    for n in 1..=64 {
        let mut p = post(Owner::AgentAttention, n);
        let o = &p.original;
        let fixed = p.title.len()
            + o.scoped().token().len()
            + o.request_id().len()
            + o.poster_instance().len()
            + o.namespace().bus_id.len()
            + o.namespace().owner.len()
            + o.actions().iter().map(|a| a.key().len()).sum::<usize>()
            + o.post_nonce().to_string().len()
            + o.namespace_generation().to_string().len();
        p.body = "x".repeat(BATCH_BYTES - fixed);
        m.prepare_batch(Owner::AgentAttention, PostBatch { raised: vec![p] })
            .unwrap();
    }
    assert_eq!(m.queued_bytes, QUEUED_BYTES);
    assert_eq!(
        m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 65))
            .unwrap_err(),
        Rejection::Pressure
    );
    let mut w = m.next_write().unwrap().unwrap();
    ack(&mut m, &mut w, Disposition::Saved).unwrap();
    let mut p = m.take_next_native().unwrap().unwrap();
    let r = delivered(&p, 42);
    m.finish_post(&mut p, r).unwrap();
    assert_eq!(m.queued_bytes, QUEUED_BYTES);
    save_result(&mut m);
    assert_eq!(m.queued_bytes, QUEUED_BYTES - BATCH_BYTES);
    m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 65))
        .unwrap();
    let mut cap = model();
    cap.reserved_bytes = FILE_LIMIT as usize + 1;
    assert_eq!(cap.take_next_native().unwrap_err(), Rejection::Pressure); // no-new-post gate is still separate data
}

#[test]
fn admitted_result_finishes_after_close_and_post_fence_uncertainty_stays_negative() {
    let mut u: Value = serde_json::from_slice(&update()).unwrap();
    u["owner"] = json!(":9.9");
    let mut m = model_with(&envelope(), &serde_json::to_vec(&u).unwrap(), None);
    ready(&mut m, Owner::AgentAttention, 1);
    m.close_agent_inputs();
    let mut p = m.take_next_native().unwrap().unwrap();
    let r = delivered(&p, 42);
    m.finish_post(&mut p, r).unwrap();
    assert_eq!(m.freeze_agent_writes().unwrap_err(), Rejection::Unsettled);
    save_result(&mut m);
    let fence = m.freeze_agent_writes().unwrap();
    let agent = m.stores[0].image.clone();
    let mut p = post(Owner::SoftwareUpdate, 2);
    let mut i = input(Owner::SoftwareUpdate, 2);
    i.namespace.owner = ":9.9".into();
    p.original = Original::new(Owner::SoftwareUpdate, i).unwrap();
    m.prepare_batch(Owner::SoftwareUpdate, PostBatch { raised: vec![p] })
        .unwrap();
    let mut w = m.next_write().unwrap().unwrap();
    ack(&mut m, &mut w, Disposition::Saved).unwrap();
    let mut p = m.take_next_native().unwrap().unwrap();
    let mut ns = namespace();
    ns.owner = ":9.9".into();
    let r = p
        .report(ns, PostOutcome::Unknown { native_id: None })
        .unwrap();
    m.finish_post(&mut p, r).unwrap();
    assert!(!m.matches_fence(&fence));
    assert_eq!(m.stores[0].image, agent);
}

#[test]
fn result_replacement_requires_the_exact_original_not_just_its_token() {
    let mut m = model();
    ready(&mut m, Owner::AgentAttention, 1);
    let mut p = m.take_next_native().unwrap().unwrap();
    let report = delivered(&p, 42);
    m.finish_post(&mut p, report).unwrap();
    // A mismatched row with the same token must never satisfy membership.
    m.stores[0].value["uncertain"][0]["original"]["post_nonce"] = json!(999);
    let before = m.stores[0].image.clone();
    assert_eq!(m.next_write().unwrap_err(), Rejection::NeedsReview);
    assert!(m.snapshot().proof_invalidated);
    assert_eq!(m.stores[0].image, before);
    assert!(m.result.is_some());
}

#[test]
fn an_empty_namespace_with_retained_uncertain_or_withdrawal_claims_cannot_adopt() {
    for state in [
        "delivered",
        "unknown_post",
        "withdrawal_pending",
        "unknown_withdrawal",
    ] {
        let mut a: Value =
            serde_json::from_slice(&retained(Owner::AgentAttention, state, BUS, 42)).unwrap();
        let row = a["tokens"]["42"].take();
        a["tokens"] = json!({});
        a["uncertain"] = json!([row]);
        a["bus_id"] = json!("");
        a["owner"] = json!("");
        let mut m = model_with(&serde_json::to_vec(&a).unwrap(), &update(), None);
        ready(&mut m, Owner::AgentAttention, 9);
        assert_eq!(m.stores[0].value["bus_id"], "");
        assert_eq!(m.take_next_native().unwrap_err(), Rejection::NeedsReview);
        assert!(!m.snapshot().native_pending);
    }
}
