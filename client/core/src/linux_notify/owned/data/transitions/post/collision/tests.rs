use super::super::super::tests::*;
use super::super::tests::{ready, retained};
use super::*;
use crate::linux_notify::owned::{
    decode_owned_for,
    inspection::{inspect_selected, Inputs},
};
use serde_json::json;

fn collision(owner: Owner) -> Model {
    let mut m = model_with(
        &retained(Owner::AgentAttention, "delivered", BUS, 42),
        &update(),
        None,
    );
    ready(&mut m, owner, 9);
    finish(&mut m, false);
    m
}
fn finish(m: &mut Model, unknown: bool) {
    let mut p = m.take_next_native().unwrap().unwrap();
    let outcome = if unknown {
        PostOutcome::Unknown {
            native_id: NonZeroU32::new(42),
        }
    } else {
        PostOutcome::Delivered(NonZeroU32::new(42).unwrap())
    };
    let report = p.report(namespace(), outcome).unwrap();
    m.finish_post(&mut p, report).unwrap();
    assert!(p.original().is_none());
    assert!(m.native.as_ref().unwrap().payload.body == "Private body");
}
fn raw(m: &Model, owner: Owner) -> Vec<u8> {
    serde_json::to_vec(&m.stores[index(owner)].value).unwrap()
}
fn rejects_restart(a: &[u8], u: &[u8]) {
    if let Ok(selected) = inspect_selected(Inputs {
        agent: Some(a),
        update: Some(u),
        legacy: None,
    }) {
        assert!(Model::from_selected(selected, HOST.into()).is_err());
    }
}
fn negative(m: &mut Model) {
    assert!(m.snapshot().proof_invalidated);
    assert!(m.namespace_blocked(&namespace()));
    assert_eq!(m.take_next_native().unwrap_err(), Rejection::NeedsReview);
    assert_eq!(
        m.prepare_batch(Owner::SoftwareUpdate, batch(Owner::SoftwareUpdate, 10))
            .unwrap_err(),
        Rejection::NeedsReview
    );
    m.close_agent_inputs();
    assert_eq!(m.freeze_agent_writes().unwrap_err(), Rejection::NeedsReview);
}

#[test]
fn exact_update_saved_precedes_agent_image_for_both_observed_kinds_and_all_claim_states() {
    for old_owner in [Owner::AgentAttention, Owner::SoftwareUpdate] {
        for owner in [Owner::AgentAttention, Owner::SoftwareUpdate] {
            for state in [
                "delivered",
                "unknown_post",
                "withdrawal_pending",
                "unknown_withdrawal",
            ] {
                for unknown in [false, true] {
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
                    // Original pair is acceptable data, never fresh native work.
                    assert!(m.take_next_native().unwrap().is_none());
                    let key = ready(&mut m, owner, 9);
                    let barrier = raw(&m, owner);
                    rejects_restart(
                        if owner == Owner::AgentAttention {
                            &barrier
                        } else {
                            &a
                        },
                        if owner == Owner::SoftwareUpdate {
                            &barrier
                        } else {
                            &u
                        },
                    );
                    let before = m.snapshot();
                    let capacity = (m.reserved_entries, m.reserved_bytes);
                    finish(&mut m, unknown);
                    assert_eq!(
                        m.snapshot().invalidation_generation,
                        before.invalidation_generation + 1
                    );
                    assert_eq!(m.batch_status(&key).unwrap(), BatchStatus::Review);
                    assert_eq!(m.snapshot().queued_utf8_bytes, before.queued_utf8_bytes);
                    assert!(
                        matches!(
                            m.native.as_ref().unwrap().report.as_ref().unwrap().outcome,
                            PostOutcome::Unknown { .. }
                        ) == unknown
                    );
                    negative(&mut m);
                    if old_owner == Owner::SoftwareUpdate && owner == Owner::SoftwareUpdate {
                        assert!(m.collision.is_none());
                        assert_eq!(m.next_write().unwrap_err(), Rejection::NeedsReview);
                        continue;
                    }
                    let mut first = m.next_write().unwrap().unwrap();
                    let first_attempt = first.attempt();
                    let first_bytes = first.bytes().unwrap().to_vec();
                    assert_eq!(first_attempt.phase(), WritePhase::CollisionInvalidations);
                    assert_eq!(first_attempt.owner(), Owner::SoftwareUpdate);
                    assert_eq!(
                        first_attempt.before(),
                        m.image_report(Owner::SoftwareUpdate).last_acknowledged
                    );
                    assert!(m.next_write().unwrap().is_none());
                    let saved = decode_owned_for(&first_bytes, Owner::SoftwareUpdate).unwrap();
                    let rows = saved.rows().unwrap();
                    assert_eq!(
                        rows.invalidations().len(),
                        usize::from(old_owner == Owner::AgentAttention)
                            + usize::from(owner == Owner::AgentAttention)
                    );
                    let targets = rows
                        .invalidations()
                        .iter()
                        .map(|r| r.data().target().original().request_id())
                        .collect::<BTreeSet<_>>();
                    assert!(targets.contains("request-8") == (old_owner == Owner::AgentAttention));
                    assert!(targets.contains("request-9") == (owner == Owner::AgentAttention));
                    for target in rows.invalidations() {
                        assert_eq!(target.data().target().native_id().get(), 42);
                        assert_eq!(target.data().target().original().namespace(), &namespace());
                        assert!(target.encode().unwrap().len() <= 16384);
                    }
                    let unchanged_agent = raw(&m, Owner::AgentAttention);
                    rejects_restart(&unchanged_agent, &first_bytes);
                    ack(&mut m, &mut first, Disposition::Saved).unwrap();
                    assert!(first.bytes().is_none());
                    assert_eq!(
                        m.stores[0].value,
                        serde_json::from_slice::<Value>(&unchanged_agent).unwrap()
                    );
                    assert_eq!(
                        m.image_report(Owner::SoftwareUpdate).last_acknowledged,
                        first_attempt.after()
                    );
                    if owner == Owner::AgentAttention {
                        let mut second = m.next_write().unwrap().unwrap();
                        let attempt = second.attempt();
                        assert_eq!(attempt.phase(), WritePhase::CollisionResult);
                        assert_eq!(attempt.owner(), Owner::AgentAttention);
                        assert!(attempt
                            .predecessor
                            .as_ref()
                            .unwrap()
                            .matches(&first_attempt));
                        let second_bytes = second.bytes().unwrap().to_vec();
                        // The forbidden agent-first partial image also refuses, even
                        // when selected inspection alone permits same-owner duplicates.
                        rejects_restart(&second_bytes, &u);
                        rejects_restart(&second_bytes, &first_bytes);
                        ack(&mut m, &mut second, Disposition::Saved).unwrap();
                        assert!(second.bytes().is_none());
                    }
                    let result = decode_owned_for(&raw(&m, owner), owner).unwrap();
                    let claims = result.rows().unwrap().claims();
                    let claim = claims.iter().find(|r| matches!(r.data(), ClaimData::Retained(c) if c.original().request_id() == "request-9")).unwrap();
                    assert!(
                        matches!(claim.data(), ClaimData::Retained(RetainedClaim::UnknownPost { native_id: Some(id), .. }) if id.get() == 42)
                    );
                    let old_after = decode_owned_for(&raw(&m, old_owner), old_owner).unwrap();
                    assert!(old_after.rows().unwrap().claims().iter().any(|r|
                        matches!(r.data(), ClaimData::Retained(c) if c.original().request_id() == "request-8" && c.tag() == state)));
                    assert_eq!(m.snapshot().pending_preparations, 1);
                    assert_eq!(m.snapshot().queued_utf8_bytes, before.queued_utf8_bytes);
                    assert_eq!((m.reserved_entries, m.reserved_bytes), capacity);
                    assert!(m.snapshot().native_pending && !m.snapshot().write_pending);
                    assert!(m.native.as_ref().unwrap().report.is_some());
                    assert_eq!(m.next_write().unwrap_err(), Rejection::NeedsReview);
                    negative(&mut m);
                }
            }
        }
    }
}

#[test]
fn lost_unknown_and_refused_collision_writes_never_replay_or_accept_late_success() {
    for second_phase in [false, true] {
        for disposition in [
            None,
            Some(Disposition::Unknown),
            Some(Disposition::RefusedBeforeWrite),
        ] {
            let mut m = collision(Owner::AgentAttention);
            let mut write = m.next_write().unwrap().unwrap();
            if second_phase {
                ack(&mut m, &mut write, Disposition::Saved).unwrap();
                write = m.next_write().unwrap().unwrap();
            }
            let before = [m.stores[0].image.clone(), m.stores[1].image.clone()];
            let attempted = write.attempt();
            if let Some(disposition) = disposition {
                ack(&mut m, &mut write, disposition).unwrap();
                assert_eq!(
                    ack(&mut m, &mut write, Disposition::Saved).unwrap_err(),
                    Rejection::NeedsReview
                );
                assert_eq!(m.next_write().unwrap_err(), Rejection::NeedsReview);
            } else {
                drop(write);
                assert!(m.next_write().unwrap().is_none());
            }
            assert_eq!(
                [m.stores[0].image.clone(), m.stores[1].image.clone()],
                before
            );
            assert_eq!(
                m.image_report(attempted.owner()).attempted_unconfirmed,
                Some(attempted.after())
            );
            assert!(m.snapshot().write_pending);
            assert_eq!(
                m.snapshot().write_unknown,
                disposition == Some(Disposition::Unknown)
            );
            assert!(m.native.as_ref().unwrap().report.is_some());
            assert!(m.snapshot().queued_utf8_bytes > 0);
            negative(&mut m);
        }
    }
}

#[test]
fn collision_ack_identity_includes_phase_original_and_actual_saved_predecessor() {
    let mut m = collision(Owner::AgentAttention);
    let mut write = m.next_write().unwrap().unwrap();
    let first = write.attempt();
    for second_phase in [false, true] {
        if second_phase {
            ack(&mut m, &mut write, Disposition::Saved).unwrap();
            write = m.next_write().unwrap().unwrap();
        }
        for changed in 0..11 {
            let mut attempt = write.attempt();
            match changed {
                0 => attempt.phase = WritePhase::PostResult,
                1 => {
                    attempt.owner = if attempt.owner == Owner::AgentAttention {
                        Owner::SoftwareUpdate
                    } else {
                        Owner::AgentAttention
                    }
                }
                2 => attempt.step += 1,
                3 => attempt.item = Some(99),
                4 => attempt.original_sha256 = Some("wrong-original-generation-and-actions".into()),
                5 => attempt.before.sha256 = None,
                6 => attempt.after.bytes = Some(1),
                7 => attempt.model = Arc::downgrade(&Arc::new(())),
                8 => attempt.predecessor = None,
                9 => {
                    let mut wrong = first.clone();
                    wrong.phase = WritePhase::PreIssue;
                    attempt.predecessor = Some(Box::new(wrong));
                }
                _ => {
                    let mut wrong = first.clone();
                    wrong.after.sha256 = Some("wrong-update-image".into());
                    attempt.predecessor = Some(Box::new(wrong));
                }
            }
            if !second_phase && changed == 8 {
                continue;
            }
            let before = m.snapshot();
            assert_eq!(
                m.ack_write(
                    &mut write,
                    WriteOutcome {
                        attempt,
                        disposition: Disposition::Saved
                    }
                )
                .unwrap_err(),
                Rejection::WrongAttempt
            );
            assert_eq!(m.snapshot(), before);
            assert!(write.bytes().is_some());
        }
        if second_phase {
            assert_eq!(
                m.ack_write(
                    &mut write,
                    WriteOutcome {
                        attempt: first.clone(),
                        disposition: Disposition::Saved
                    }
                )
                .unwrap_err(),
                Rejection::WrongAttempt
            );
        }
    }
    ack(&mut m, &mut write, Disposition::Saved).unwrap();
    negative(&mut m);
}

#[test]
fn exact_possible_row_including_actions_and_generation_is_required_in_either_phase() {
    for owner in [Owner::AgentAttention, Owner::SoftwareUpdate] {
        for changed in 0..5 {
            let mut m = collision(owner);
            if owner == Owner::AgentAttention {
                let mut first = m.next_write().unwrap().unwrap();
                ack(&mut m, &mut first, Disposition::Saved).unwrap();
            }
            let rows = m.stores[index(owner)].value["uncertain"]
                .as_array_mut()
                .unwrap();
            match changed {
                0 => rows[0]["original"]["namespace_generation"] = json!(99),
                1 => rows[0]["original"]["actions"] = json!(["default"]),
                2 => rows[0]["original"]["bus_id"] = json!("ffffffffffffffffffffffffffffffff"),
                3 => rows.push(rows[0].clone()),
                _ => rows.clear(),
            }
            let image = m.stores[index(owner)].image.clone();
            assert!(m.next_write().is_err());
            assert!(!m.snapshot().write_pending);
            assert_eq!(m.stores[index(owner)].image, image);
            negative(&mut m);
        }
    }
}

#[test]
fn foreign_namespace_positive_ids_and_changed_reports_cannot_create_continuations() {
    for changed in 0..4 {
        let old_bus = if changed == 0 {
            "ffffffffffffffffffffffffffffffff"
        } else {
            BUS
        };
        let mut m = model_with(
            &retained(Owner::AgentAttention, "delivered", old_bus, 42),
            &update(),
            None,
        );
        ready(&mut m, Owner::AgentAttention, 9);
        let mut p = m.take_next_native().unwrap().unwrap();
        let mut ns = namespace();
        match changed {
            1 => ns.owner = ":1.99".into(),
            2 => ns.bus_id = "malformed".into(),
            3 => ns.bus_id = "ffffffffffffffffffffffffffffffff".into(),
            _ => (),
        }
        let report = p
            .report(
                ns,
                PostOutcome::Unknown {
                    native_id: NonZeroU32::new(42),
                },
            )
            .unwrap();
        m.finish_post(&mut p, report).unwrap();
        assert!(m.collision.is_none());
        assert_eq!(m.next_write().unwrap_err(), Rejection::NeedsReview);
        negative(&mut m);
    }
}

#[test]
fn loaded_and_admitted_agent_metadata_reserves_worst_escaped_invalidation_sizes() {
    let mut i = input(Owner::AgentAttention, 1);
    i.request_id = "\\\"".repeat(256);
    let original = Original::new(Owner::AgentAttention, i).unwrap();
    let maximum = NonZeroU32::new(u32::MAX).unwrap();
    let reservation = wire::invalidation_budget(&original, maximum).unwrap();
    let row = wire::invalidation_value(&original, maximum, "native_id_collision");
    let bytes = serde_json::to_vec(&row).unwrap();
    assert!(bytes.len() <= 16384 && reservation >= bytes.len() * 4);
    let mut m = model();
    let before = m.reserved_bytes;
    let mut p = post(Owner::AgentAttention, 1);
    p.original = original;
    m.prepare_batch(Owner::AgentAttention, PostBatch { raised: vec![p] })
        .unwrap();
    assert!(m.reserved_bytes >= before + reservation);
    let mut loaded = model_with(
        &retained(Owner::AgentAttention, "delivered", BUS, 1),
        &update(),
        None,
    );
    let saved =
        decode_owned_for(&raw(&loaded, Owner::AgentAttention), Owner::AgentAttention).unwrap();
    let (o, _) = saved.rows().unwrap().positive_claims().next().unwrap();
    assert!(
        loaded.reserved_bytes
            >= raw(&loaded, Owner::AgentAttention).len()
                + wire::invalidation_budget(o, maximum).unwrap()
    );
    loaded.reserved_entries = LIMIT + 1;
    assert_eq!(loaded.take_next_native().unwrap_err(), Rejection::Pressure);
}

#[test]
fn frozen_or_pressured_collision_context_never_exposes_an_agent_write() {
    for frozen in [false, true] {
        let mut m = collision(Owner::AgentAttention);
        let mut first = m.next_write().unwrap().unwrap();
        ack(&mut m, &mut first, Disposition::Saved).unwrap();
        let image = m.stores[0].image.clone();
        if frozen {
            // Public freeze already refuses this unsettled Model. Keep the
            // planner closed if a future host violates that gate.
            m.frozen = true;
        } else {
            m.reserved_bytes = FILE_LIMIT as usize + 1;
        }
        assert!(m.next_write().is_err());
        assert!(!m.snapshot().write_pending);
        assert_eq!(m.stores[0].image, image);
        negative(&mut m);
    }
}

#[test]
fn positive_uncertain_claims_are_full_collision_targets_without_token_map_membership() {
    for state in [
        "delivered",
        "unknown_post",
        "withdrawal_pending",
        "unknown_withdrawal",
    ] {
        for owner in [Owner::AgentAttention, Owner::SoftwareUpdate] {
            let mut a: Value =
                serde_json::from_slice(&retained(Owner::AgentAttention, state, BUS, 42)).unwrap();
            let row = a["tokens"].as_object_mut().unwrap().remove("42").unwrap();
            a["uncertain"] = json!([row]);
            let mut m = model_with(&serde_json::to_vec(&a).unwrap(), &update(), None);
            ready(&mut m, owner, 9);
            finish(&mut m, true);
            let mut first = m.next_write().unwrap().unwrap();
            let saved = decode_owned_for(first.bytes().unwrap(), Owner::SoftwareUpdate).unwrap();
            assert!(saved.rows().unwrap().invalidations().iter().any(|r| r
                .data()
                .target()
                .original()
                .request_id()
                == "request-8"));
            ack(&mut m, &mut first, Disposition::Saved).unwrap();
            if owner == Owner::AgentAttention {
                let mut second = m.next_write().unwrap().unwrap();
                ack(&mut m, &mut second, Disposition::Saved).unwrap();
            }
            assert_eq!(m.stores[0].value["uncertain"][0], a["uncertain"][0]);
            negative(&mut m);
        }
    }
}

#[test]
fn mixed_pair_foreign_occupied_slot_remains_terminal_despite_another_namespace_collider() {
    for owner in [Owner::AgentAttention, Owner::SoftwareUpdate] {
        for unknown in [false, true] {
            let a_bus = if owner == Owner::AgentAttention {
                "ffffffffffffffffffffffffffffffff"
            } else {
                BUS
            };
            let u_bus = if owner == Owner::SoftwareUpdate {
                "ffffffffffffffffffffffffffffffff"
            } else {
                BUS
            };
            let a = retained(Owner::AgentAttention, "delivered", a_bus, 42);
            let mut u: Value =
                serde_json::from_slice(&retained(Owner::SoftwareUpdate, "delivered", u_bus, 42))
                    .unwrap();
            // Distinct original identity; both envelope headers remain N while
            // the incoming owner's occupied slot retains original namespace M.
            u["tokens"]["42"]["original"]["poster_instance"] =
                json!("bbbbbbbb-2222-4222-8222-222222222222");
            let u = serde_json::to_vec(&u).unwrap();
            let mut m = model_with(&a, &u, None); // selected inspection AND Model accept this raw pair
            let key = ready(&mut m, owner, 9);
            let before = [
                raw(&m, Owner::AgentAttention),
                raw(&m, Owner::SoftwareUpdate),
            ];
            let images = [m.stores[0].image.clone(), m.stores[1].image.clone()];
            let capacity = (m.reserved_entries, m.reserved_bytes, m.queued_bytes);
            finish(&mut m, unknown);
            assert!(m.collision.is_none());
            assert_eq!(m.next_write().unwrap_err(), Rejection::NeedsReview);
            assert_eq!(
                [
                    raw(&m, Owner::AgentAttention),
                    raw(&m, Owner::SoftwareUpdate)
                ],
                before
            );
            assert_eq!(
                [m.stores[0].image.clone(), m.stores[1].image.clone()],
                images
            );
            assert_eq!(
                (m.reserved_entries, m.reserved_bytes, m.queued_bytes),
                capacity
            );
            assert_eq!(m.batch_status(&key).unwrap(), BatchStatus::Review);
            assert!(m.native.as_ref().unwrap().report.is_some());
            negative(&mut m);
        }
    }
}
