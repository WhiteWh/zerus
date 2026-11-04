use super::*;
use crate::linux_notify::owned::inspection::{inspect, inspect_selected, Inputs};
use serde_json::json;
pub(super) const HOST: &str = "aaaaaaaa-1111-4111-8111-111111111111";
pub(super) const BUS: &str = "00112233445566778899aabbccddeeff";
pub(super) fn namespace() -> Namespace {
    Namespace {
        bus_id: BUS.into(),
        owner: ":1.0".into(),
    }
}
pub(super) fn token(owner: Owner, n: u64) -> String {
    match owner {
        Owner::AgentAttention => crate::attention::encode_token(
            json!({"host":"","name":"codex/example/one","notice":format!("edge-{n}")})
                .as_object()
                .unwrap(),
        ),
        Owner::SoftwareUpdate => format!("zerus-update:v2:dev:{n}"),
    }
}
pub(super) fn input(owner: Owner, n: u64) -> OriginalInput {
    OriginalInput {
        token: token(owner, n),
        request_id: format!("request-{n}"),
        poster_instance: HOST.into(),
        post_nonce: n,
        namespace: namespace(),
        namespace_generation: 1,
        actions: vec![Action::Default, Action::OpenSession],
    }
}
pub(super) fn post(owner: Owner, n: u64) -> Post {
    Post {
        original: Original::new(owner, input(owner, n)).unwrap(),
        title: "Title".into(),
        body: "Private body".into(),
    }
}
pub(super) fn batch(owner: Owner, n: u64) -> PostBatch {
    PostBatch {
        raised: vec![post(owner, n)],
    }
}
pub(super) fn envelope() -> Vec<u8> {
    serde_json::to_vec(&json!({"version":2,"bus_id":BUS,"owner":":1.0","tokens":{},"uncertain":[],"legacy_uncertain":[]})).unwrap()
}
pub(super) fn update() -> Vec<u8> {
    let mut v: Value = serde_json::from_slice(&envelope()).unwrap();
    v["invalidations"] = json!([]);
    serde_json::to_vec(&v).unwrap()
}
pub(super) fn model_with(a: &[u8], u: &[u8], l: Option<&[u8]>) -> Model {
    Model::from_selected(
        inspect_selected(Inputs {
            agent: Some(a),
            update: Some(u),
            legacy: l,
        })
        .unwrap(),
        HOST.into(),
    )
    .unwrap()
}
pub(super) fn model() -> Model {
    model_with(&envelope(), &update(), None)
}
fn outcome(plan: &WritePlan, disposition: Disposition) -> WriteOutcome {
    WriteOutcome {
        attempt: plan.attempt(),
        disposition,
    }
}

pub(super) fn ack(
    m: &mut Model,
    plan: &mut WritePlan,
    disposition: Disposition,
) -> Result<(), Rejection> {
    let result = outcome(plan, disposition);
    m.ack_write(plan, result)
}

#[test]
fn explicit_host_constructor_reuses_all_wire_identity_guards() {
    for kind in 0..8 {
        let mut i = input(Owner::AgentAttention, 1);
        match kind {
            0 => i.poster_instance = "00000000-0000-0000-0000-000000000000".into(),
            1 => i.poster_instance = HOST.to_uppercase(),
            2 => i.request_id = "x".repeat(513),
            3 => i.request_id = "nul\0".into(),
            4 => i.post_nonce = 0,
            5 => i.namespace_generation = 0,
            6 => i.namespace.owner = "well.known".into(),
            _ => i.actions = vec![Action::Default, Action::Default],
        }
        assert!(Original::new(Owner::AgentAttention, i).is_err());
    }
    assert!(Original::new(Owner::SoftwareUpdate, input(Owner::AgentAttention, 1)).is_err());
    assert!(!format!("{:?}", input(Owner::AgentAttention, 1)).contains("request-1"));
}
#[test]
fn unselected_or_historical_inputs_cannot_create_a_runnable_model() {
    let a = envelope();
    let u = update();
    let unselected = inspect(Inputs {
        agent: Some(&a),
        update: Some(&u),
        legacy: None,
    });
    // The ordinary decoder sees update invalidations as unknown, as before.
    assert!(unselected.is_err());
    let ordinary = inspect(Inputs {
        agent: Some(&a),
        update: Some(&a),
        legacy: None,
    })
    .unwrap();
    assert_eq!(
        Model::from_selected(ordinary, HOST.into()).unwrap_err(),
        Rejection::InvalidInput
    );
}
#[test]
fn barrier_binds_original_bytes_and_ack_does_not_complete_the_preparation() {
    let mut a = envelope();
    a.extend_from_slice(b" \n");
    let mut m = model_with(&a, &update(), None);
    let key = m
        .prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
        .unwrap();
    assert!(m.namespace_blocked(&namespace())); // before write and before any native call
    let mut plan = m.next_write().unwrap().unwrap();
    assert!(m.next_write().unwrap().is_none());
    assert_eq!(plan.attempt().before().sha256(), Some(hash(&a).as_str()));
    assert_eq!(plan.attempt().before().bytes(), Some(a.len()));
    assert_eq!(
        plan.attempt().after().sha256(),
        Some(hash(plan.bytes().unwrap()).as_str())
    );
    let saved = super::super::super::decode_owned_for(plan.bytes().unwrap(), Owner::AgentAttention)
        .unwrap();
    let row = &saved.rows().unwrap().claims()[0];
    let ClaimData::Retained(RetainedClaim::PossiblePost(o)) = row.data() else {
        panic!("wrong barrier")
    };
    assert_eq!(o.request_id(), "request-1");
    assert_eq!(o.poster_instance(), HOST);
    assert_eq!(o.post_nonce().get(), 1);
    assert!(inspect_selected(Inputs {
        agent: Some(plan.bytes().unwrap()),
        update: Some(&update()),
        legacy: None
    })
    .is_err());
    ack(&mut m, &mut plan, Disposition::Saved).unwrap();
    assert_eq!(
        m.batch_status(&key).unwrap(),
        BatchStatus::BarrierAcknowledged
    );
    assert_eq!(
        m.image_report(Owner::AgentAttention)
            .last_acknowledged
            .sha256(),
        plan.attempt().after().sha256()
    );
    assert!(plan.bytes().is_none());
    assert!(m.namespace_blocked(&namespace()));
    assert_eq!(m.snapshot().pending_preparations, 1);
    assert!(m.next_write().unwrap().is_none()); // no native dispatch API/stub
    m.close_agent_inputs();
    assert_eq!(m.freeze_agent_writes().unwrap_err(), Rejection::Unsettled);
}
#[test]
fn foreign_keys_and_reports_do_not_consume_current_valid_work() {
    let (mut a, mut b) = (model(), model());
    let ka = a
        .prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
        .unwrap();
    b.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
        .unwrap();
    let mut pa = a.next_write().unwrap().unwrap();
    let mut pb = b.next_write().unwrap().unwrap();
    let before = b.snapshot();
    assert_eq!(b.batch_status(&ka), Err(Rejection::ForeignKey));
    assert_eq!(
        ack(&mut b, &mut pa, Disposition::Saved),
        Err(Rejection::ForeignKey)
    );
    assert_eq!(
        b.ack_write(&mut pb, outcome(&pa, Disposition::Saved)),
        Err(Rejection::WrongAttempt)
    );
    assert_eq!(b.snapshot(), before);
    ack(&mut b, &mut pb, Disposition::Saved).unwrap();
    assert!(!b.snapshot().write_pending);
}
#[test]
fn stale_and_wrong_owner_acknowledgements_preserve_the_next_step() {
    let mut m = model();
    m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
        .unwrap();
    let mut p = m.next_write().unwrap().unwrap();
    ack(&mut m, &mut p, Disposition::Saved).unwrap();
    m.prepare_batch(Owner::SoftwareUpdate, batch(Owner::SoftwareUpdate, 2))
        .unwrap();
    let mut next = m.next_write().unwrap().unwrap();
    let before = m.snapshot();
    assert_eq!(
        ack(&mut m, &mut p, Disposition::Saved),
        Err(Rejection::StaleKey)
    );
    let mut wrong = next.attempt();
    wrong.owner = Owner::AgentAttention;
    assert_eq!(
        m.ack_write(
            &mut next,
            WriteOutcome {
                attempt: wrong,
                disposition: Disposition::Saved
            }
        ),
        Err(Rejection::WrongAttempt)
    );
    assert_eq!(m.snapshot(), before);
    for field in ["hash", "length", "presence"] {
        let mut wrong = next.attempt();
        match field {
            "hash" => wrong.before.sha256 = Some("0".repeat(64)),
            "length" => wrong.after.bytes = Some(0),
            _ => {
                wrong.before.sha256 = None;
                wrong.before.bytes = None;
            }
        }
        assert_eq!(
            m.ack_write(
                &mut next,
                WriteOutcome {
                    attempt: wrong,
                    disposition: Disposition::Saved
                }
            ),
            Err(Rejection::WrongAttempt)
        );
        assert_eq!(m.snapshot(), before);
    }
    ack(&mut m, &mut next, Disposition::Saved).unwrap();
}
#[test]
fn unknown_commit_retains_exact_buffer_and_cannot_be_retried_or_late_settled() {
    let mut m = model();
    m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
        .unwrap();
    let mut p = m.next_write().unwrap().unwrap();
    let before = m
        .image_report(Owner::AgentAttention)
        .last_acknowledged
        .clone();
    ack(&mut m, &mut p, Disposition::Unknown).unwrap();
    let d = m.snapshot();
    assert!(d.proof_invalidated && d.write_unknown && d.write_pending);
    assert_eq!(d.invalidation_generation, 2);
    assert_eq!(d.pending_preparations, 1);
    let report = m.image_report(Owner::AgentAttention);
    assert_eq!(report.last_acknowledged, &before);
    assert_eq!(report.attempted_unconfirmed.unwrap(), p.attempt().after());
    assert_eq!(m.next_write().unwrap_err(), Rejection::NeedsReview);
    assert_eq!(
        ack(&mut m, &mut p, Disposition::Saved),
        Err(Rejection::NeedsReview)
    );
    assert_eq!(
        m.prepare_batch(Owner::SoftwareUpdate, batch(Owner::SoftwareUpdate, 2))
            .unwrap_err(),
        Rejection::NeedsReview
    );
    assert_eq!(m.snapshot(), d);
    drop(p);
    assert!(!m.outstanding.as_ref().unwrap()._bytes.is_empty());
}
#[test]
fn verified_before_write_refusal_stays_distinct_and_review_only() {
    let mut m = model();
    let k = m
        .prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
        .unwrap();
    let mut p = m.next_write().unwrap().unwrap();
    let before = m
        .image_report(Owner::AgentAttention)
        .last_acknowledged
        .clone();
    ack(&mut m, &mut p, Disposition::RefusedBeforeWrite).unwrap();
    assert_eq!(m.batch_status(&k).unwrap(), BatchStatus::Review);
    assert_eq!(
        m.image_report(Owner::AgentAttention).last_acknowledged,
        &before
    );
    assert!(!m.snapshot().write_unknown);
    assert!(m.namespace_blocked(&namespace()));
    assert!(m.next_write().unwrap().is_none());
}
#[test]
fn entire_batch_is_checked_before_any_admission_or_nonce_change() {
    let mut m = model();
    let before = m.snapshot();
    let bad = PostBatch {
        raised: vec![
            post(Owner::AgentAttention, 1),
            post(Owner::SoftwareUpdate, 2),
        ],
    };
    assert_eq!(
        m.prepare_batch(Owner::AgentAttention, bad).unwrap_err(),
        Rejection::WrongOwner
    );
    assert_eq!(m.snapshot(), before);
    m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
        .unwrap();
    assert_eq!(
        m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
            .unwrap_err(),
        Rejection::ReusedNonce
    );
    let mut changed = post(Owner::AgentAttention, 1);
    changed.title = "changed".into();
    assert_eq!(
        m.prepare_batch(
            Owner::AgentAttention,
            PostBatch {
                raised: vec![changed]
            }
        )
        .unwrap_err(),
        Rejection::ReusedNonce
    );
    assert_eq!(m.snapshot().pending_preparations, 1);
}
#[test]
fn pressure_and_counter_exhaustion_refuse_before_mutating_or_writing() {
    let mut m = model();
    let before = m.snapshot();
    let mut too_big = post(Owner::AgentAttention, 1);
    too_big.body = "я".repeat(BATCH_BYTES);
    assert_eq!(
        m.prepare_batch(
            Owner::AgentAttention,
            PostBatch {
                raised: vec![too_big]
            }
        )
        .unwrap_err(),
        Rejection::Pressure
    );
    assert_eq!(m.snapshot(), before);
    for n in 1..=64 {
        m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, n))
            .unwrap();
    }
    let before = m.snapshot();
    assert!(before.queued_utf8_bytes <= QUEUED_BYTES);
    assert_eq!(
        m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 65))
            .unwrap_err(),
        Rejection::Pressure
    );
    assert_eq!(m.snapshot(), before);
    let mut m = model();
    m.next_sequence = u64::MAX;
    assert_eq!(
        m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
            .unwrap_err(),
        Rejection::Pressure
    );
    assert_eq!(m.highest_nonce, 0);
}

#[test]
fn changing_a_nonce_cannot_reuse_an_admitted_original_request() {
    let mut m = model();
    m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
        .unwrap();
    let mut changed = post(Owner::AgentAttention, 2);
    let mut original = input(Owner::AgentAttention, 2);
    original.request_id = "request-1".into();
    changed.original = Original::new(Owner::AgentAttention, original).unwrap();
    let before = m.snapshot();
    assert_eq!(
        m.prepare_batch(
            Owner::AgentAttention,
            PostBatch {
                raised: vec![changed]
            }
        )
        .unwrap_err(),
        Rejection::ReusedRequest
    );
    assert_eq!(m.snapshot(), before);
}
#[test]
fn raw_legacy_suppression_is_shared_and_result_capacity_is_reserved_up_front() {
    let t = token(Owner::AgentAttention, 1);
    let legacy = serde_json::to_vec(&json!({"version":1,"owner":":1.0","tokens":{"1":t}})).unwrap();
    let mut m = model_with(&envelope(), &update(), Some(&legacy));
    let before = m.snapshot();
    assert_eq!(
        m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
            .unwrap_err(),
        Rejection::Suppressed
    );
    assert_eq!(m.snapshot(), before);
    assert_eq!(m.legacy_image().sha256(), Some(hash(&legacy).as_str()));
    let a = envelope();
    let u = update();
    let mut l = br#"{"version":1,"owner":":1.0","tokens":{}}"#.to_vec();
    l.resize(FILE_LIMIT as usize - a.len() - u.len() - 1, b' ');
    let mut m = model_with(&a, &u, Some(&l));
    let before = m.snapshot();
    assert_eq!(
        m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
            .unwrap_err(),
        Rejection::Pressure
    );
    assert_eq!(m.snapshot(), before);
    assert!(m.next_write().unwrap().is_none());
}
#[test]
fn close_is_sticky_but_preserves_original_admitted_barrier_continuations() {
    let mut m = model();
    let k = m
        .prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
        .unwrap();
    m.close_agent_inputs();
    m.close_agent_inputs();
    assert_eq!(
        m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 2))
            .unwrap_err(),
        Rejection::Closed
    );
    let mut p = m.next_write().unwrap().unwrap();
    ack(&mut m, &mut p, Disposition::Saved).unwrap();
    assert_eq!(
        m.batch_status(&k).unwrap(),
        BatchStatus::BarrierAcknowledged
    );
    assert_eq!(m.freeze_agent_writes().unwrap_err(), Rejection::Unsettled);
    let mut m = model();
    m.prepare_batch(Owner::SoftwareUpdate, batch(Owner::SoftwareUpdate, 1))
        .unwrap();
    m.close_agent_inputs();
    assert_eq!(m.freeze_agent_writes().unwrap_err(), Rejection::Unsettled); // cross-owner pending work
}
#[test]
fn model_fence_does_not_survive_unknown_update_commit_or_model_replacement() {
    let mut m = model();
    m.close_agent_inputs();
    let fence = m.freeze_agent_writes().unwrap();
    assert!(m.matches_fence(&fence));
    let mut other = model();
    other.close_agent_inputs();
    other.freeze_agent_writes().unwrap();
    assert!(!other.matches_fence(&fence));
    m.prepare_batch(Owner::SoftwareUpdate, batch(Owner::SoftwareUpdate, 1))
        .unwrap();
    let mut p = m.next_write().unwrap().unwrap();
    assert_eq!(p.attempt().owner(), Owner::SoftwareUpdate);
    assert_eq!(
        fence.agent_image(),
        m.image_report(Owner::AgentAttention).last_acknowledged
    );
    ack(&mut m, &mut p, Disposition::Unknown).unwrap();
    assert!(!m.matches_fence(&fence));
    assert!(m.snapshot().agent_writes_frozen);
    assert_eq!(
        fence.agent_image(),
        m.image_report(Owner::AgentAttention).last_acknowledged
    );
}
#[test]
fn a_lost_plan_never_makes_work_quiet_and_input_debug_stays_redacted() {
    let mut m = model();
    m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 1))
        .unwrap();
    let p = m.next_write().unwrap().unwrap();
    assert!(!format!("{p:?} {:?}", m).contains("Private body"));
    drop(p);
    assert!(m.snapshot().write_pending);
    assert!(m.next_write().unwrap().is_none());
    m.close_agent_inputs();
    assert_eq!(m.freeze_agent_writes().unwrap_err(), Rejection::Unsettled);
}

#[test]
fn loaded_claims_seed_nonce_high_water_and_unknowns_never_look_settled() {
    for state in [
        "delivered",
        "unknown_post",
        "withdrawal_pending",
        "unknown_withdrawal",
    ] {
        let original =
            Original::new(Owner::AgentAttention, input(Owner::AgentAttention, 8)).unwrap();
        let mut row = wire::possible_value(&original);
        row["state"] = json!(state);
        row["native_id"] = json!(42);
        let mut a: Value = serde_json::from_slice(&envelope()).unwrap();
        a["tokens"] = json!({"42":row});
        let raw = serde_json::to_vec(&a).unwrap();
        let mut m = model_with(&raw, &update(), None);
        assert!(m.next_write().unwrap().is_none());
        assert_eq!(
            m.prepare_batch(Owner::AgentAttention, batch(Owner::AgentAttention, 8))
                .unwrap_err(),
            Rejection::ReusedNonce
        );
        m.close_agent_inputs();
        if state == "delivered" {
            let fence = m.freeze_agent_writes().unwrap();
            assert!(m.matches_fence(&fence));
            let mut update = post(Owner::SoftwareUpdate, 9);
            let mut i = input(Owner::SoftwareUpdate, 9);
            i.namespace.bus_id = BUS.to_uppercase();
            update.original = Original::new(Owner::SoftwareUpdate, i).unwrap();
            let before = m.snapshot();
            assert_eq!(
                m.prepare_batch(
                    Owner::SoftwareUpdate,
                    PostBatch {
                        raised: vec![update]
                    }
                )
                .unwrap_err(),
                Rejection::CrossScope
            );
            assert_eq!(m.snapshot(), before);
            assert!(m.matches_fence(&fence));
        } else {
            assert_eq!(m.snapshot().retained_unsettled, 1);
            assert_eq!(m.freeze_agent_writes().unwrap_err(), Rejection::Unsettled);
        }
    }
}
