use super::*;
use std::cell::Cell;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn held_original_rollback_excludes_a_real_rearm_and_refuses_when_rearm_already_won() {
    let mut f = Fixture::new();
    let request = f.request(Action::Grant);
    assert!(f.apply(request).service_granted());
    let id = uuid::Uuid::new_v4().to_string();
    let result = super::super::revoke::request(
        &f.location,
        &mut f.store,
        &serde_json::json!({"op":"revoke","request_id":id,"reason":"rollback"}),
    );
    assert_eq!(result["checkpointed"], true);
    let held = super::super::revoke::hold::acquire_for_test(&f.location, &id).unwrap();
    let request = f.request(Action::Rearm);
    assert!(apply(
        &f.location,
        &mut f.store,
        &f.epoch,
        request,
        f.origin.clone(),
        None,
        |_| Ok(())
    )
    .is_err());
    assert_eq!(f.current().state, State::Revoked);
    drop(held);
    let request = f.request(Action::Rearm);
    assert!(f.apply(request).service_granted());
    assert!(super::super::revoke::hold::acquire_for_test(&f.location, &id).is_err());
    assert!(service_granted(&f.store, &f.location.configuration).unwrap());
}

#[test]
fn reconnect_keeps_ownership_epoch_but_new_image_and_explicit_rearm_rotate_it() {
    let mut f = Fixture::new();
    let request = f.request(Action::Grant);
    f.apply(request);
    let first = f.current().owner_epoch.unwrap();
    let request = f.request(Action::Grant);
    f.apply(request);
    assert_eq!(f.current().owner_epoch.as_deref(), Some(first.as_str()));
    std::fs::write(&f.origin.image, b"updated selected native frontend image").unwrap();
    assert!(!service_granted(&f.store, &f.location.configuration).unwrap());
    f.origin = Origin::capture(&f.origin.image, f.origin.build.clone()).unwrap();
    let request = f.request(Action::Grant);
    assert!(f.apply(request).service_granted());
    let updated = f.current().owner_epoch.unwrap();
    assert_ne!(updated, first);
    let request = f.request(Action::Rearm);
    assert!(f.apply(request).service_granted());
    assert_ne!(f.current().owner_epoch.unwrap(), updated);
}

#[cfg(unix)]
#[test]
fn native_takeover_waits_for_an_original_host_lock_that_outlives_the_root_runtime() {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = Fixture::new();
    let request = f.request(Action::Grant);
    f.apply(request);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(f.location.root.join("notification-host.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&file).unwrap();
    // Model another fixture's fork retaining this open file description while
    // its exec is delayed. Closing our raw handle alone would leave it locked.
    let inherited = file.try_clone().unwrap();
    let file = platform::FileLock::held(file);
    let request = f.request(Action::NativeTakeover);
    let receipt = f.apply(request.clone());
    assert!(matches!(receipt.outcome, Outcome::Pending));
    assert!(!receipt.native_ready());
    drop(file);
    settle(&f.location, &mut f.store, None, |_| Ok(())).unwrap();
    let (receipt, _) = original(&f.store.db, request.id()).unwrap().unwrap();
    assert!(current_receipt(&f.store.db, receipt)
        .unwrap()
        .native_ready());
    // The same original request, never another takeover, now completes.
    assert_eq!(f.current().state, State::Native);
    drop(inherited);
}

struct Fixture {
    _temp: tempfile::TempDir,
    // Canonical: macOS temp dirs sit under the /var symlink, which the store refuses.
    base: std::path::PathBuf,
    location: Location,
    store: Store,
    epoch: String,
    origin: Origin,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let location = Location {
            root: base.join("state/desktop"),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let epoch = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&location, &epoch).unwrap();
        let image = base.join("hgs-tray");
        std::fs::write(&image, b"original selected native frontend").unwrap();
        let origin = Origin::capture(
            &image,
            Build {
                version: "1.2.3-dev.7".into(),
                source_commit: "a".repeat(40),
                source_dirty: Some(false),
            },
        )
        .unwrap();
        Self {
            _temp: temp,
            base,
            location,
            store,
            epoch,
            origin,
        }
    }
    fn request(&self, action: Action) -> Request {
        let s = status(&self.store, &self.location.configuration).unwrap();
        Request::DesktopOwnership {
            request_id: uuid::Uuid::new_v4().to_string(),
            action,
            journal: s.journal,
            revision: s.revision,
        }
    }
    fn apply(&mut self, request: Request) -> Receipt {
        apply(
            &self.location,
            &mut self.store,
            &self.epoch,
            request,
            self.origin.clone(),
            None,
            |_| Ok(()),
        )
        .unwrap()
    }
    fn current(&self) -> Status {
        status(&self.store, &self.location.configuration).unwrap()
    }
    fn count(&self) -> i64 {
        self.store
            .db
            .query_row("SELECT count(*) FROM desktop_ownership_requests", [], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn query(&self, original: &Request) -> Query {
        Query::for_request(
            uuid::Uuid::new_v4().to_string(),
            self.location.configuration.clone(),
            original,
        )
        .unwrap()
    }
}

#[test]
fn grant_is_exact_idempotent_and_never_imports_approval_consent() {
    let mut f = Fixture::new();
    let request = f.request(Action::Grant);
    let receipt = f.apply(request.clone());
    assert!(receipt.matches(&request, &f.location.configuration));
    assert!(matches!(receipt.outcome, Outcome::Granted { .. }));
    assert!(!receipt.native_ready());
    assert!(receipt.service_granted());
    assert_eq!(f.apply(request.clone()), receipt);
    assert_eq!(f.count(), 1);
    let policy: (bool, i64) = f
        .store
        .db
        .query_row(
            "SELECT enabled,revision FROM desktop_delivery_policy WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(policy, (false, 0));
    assert!(
        f.store.effect_status()["revoked"] == true,
        "a durable mode grant alone never authorizes an effect writer"
    );
    let mut different = request;
    if let Request::DesktopOwnership { action, .. } = &mut different {
        *action = Action::Rearm;
    }
    assert!(apply(
        &f.location,
        &mut f.store,
        &f.epoch,
        different,
        f.origin.clone(),
        None,
        |_| Ok(())
    )
    .is_err());
    assert_eq!(f.count(), 1);
}

#[test]
fn revoke_requires_explicit_rearm_even_after_restart_and_preserves_history() {
    let mut f = Fixture::new();
    let grant = f.request(Action::Grant);
    f.apply(grant);
    let id = uuid::Uuid::new_v4().to_string();
    let result = super::super::revoke::request(
        &f.location,
        &mut f.store,
        &serde_json::json!({"op":"revoke","request_id":id}),
    );
    assert_eq!(result["revoked"], true);
    assert!(!super::super::revoke::background_permitted(&f.location, &f.store).unwrap());
    let request = f.request(Action::Grant);
    let refused = f.apply(request.clone());
    assert!(refused.matches(&request, &f.location.configuration));
    assert!(
        matches!(refused.outcome,Outcome::Refused{ref code} if code == "ownership_rearm_required")
    );
    let next_epoch = uuid::Uuid::new_v4().to_string();
    // Reopen under a new process epoch without dropping the original temp root.
    let old = std::mem::replace(&mut f.store, Store::open(&f.location, &next_epoch).unwrap());
    drop(old);
    f.epoch = next_epoch;
    assert!(f.current().rearm_required);
    let rearm = f.request(Action::Rearm);
    let receipt = f.apply(rearm);
    assert!(matches!(receipt.outcome, Outcome::Granted { .. }));
    assert!(super::super::revoke::background_permitted(&f.location, &f.store).unwrap());
    let history: i64 = f
        .store
        .db
        .query_row(
            "SELECT count(*) FROM desktop_commands WHERE request_id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        history, 1,
        "rearm supersedes the fence without deleting the original revoke"
    );
}

#[test]
fn pending_takeover_is_not_ready_until_actual_drain_and_stale_settlement_never_releases_native() {
    let mut f = Fixture::new();
    let grant = f.request(Action::Grant);
    f.apply(grant);
    let request = f.request(Action::NativeTakeover);
    let calls = Cell::new(0);
    let pending = apply(
        &f.location,
        &mut f.store,
        &f.epoch,
        request.clone(),
        f.origin.clone(),
        None,
        |store| {
            calls.set(calls.get() + 1);
            assert_eq!(store.effect_status()["revoked"], true);
            anyhow::bail!("original worker still retained")
        },
    )
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert!(matches!(pending.outcome, Outcome::Pending));
    assert!(!pending.native_ready());
    assert!(f.current().rearm_required);
    assert!(matches!(
        f.apply(f.request(Action::Rearm)).outcome,
        Outcome::Refused { .. }
    ));
    let found = lookup(&f.store, &f.location.configuration, f.query(&request)).unwrap();
    assert!(found.found && !found.definitive);
    assert!(!found.receipt.unwrap().native_ready());
    settle(&f.location, &mut f.store, None, |_| {
        calls.set(calls.get() + 1);
        Ok(())
    })
    .unwrap();
    let settled = lookup(&f.store, &f.location.configuration, f.query(&request))
        .unwrap()
        .receipt
        .unwrap();
    assert!(settled.matches(&request, &f.location.configuration));
    assert!(settled.native_ready());
    assert_eq!(calls.get(), 2);
    let query = f.query(&request);
    let next = f.request(Action::Rearm);
    f.apply(next);
    let old = lookup(&f.store, &f.location.configuration, query)
        .unwrap()
        .receipt
        .unwrap();
    assert!(matches!(old.outcome, Outcome::NativeReady { .. }));
    assert!(
        !old.native_ready(),
        "the immutable old receipt cannot settle a new frontend after a grant"
    );
}

#[test]
fn replacement_image_refuses_launch_but_background_and_new_startup_grant_continue() {
    let mut f = Fixture::new();
    let request = f.request(Action::Grant);
    f.apply(request);
    let other = f.base.join("replacement");
    std::fs::write(&other, b"new selected native frontend").unwrap();
    std::fs::rename(&other, &f.origin.image).unwrap();
    assert_eq!(f.current().state, State::IdentityChanged);
    assert!(!service_granted(&f.store, &f.location.configuration).unwrap());
    f.origin = Origin::capture(&f.origin.image, f.origin.build.clone()).unwrap();
    assert!(!f.current().rearm_required);
    assert!(super::super::revoke::background_permitted(&f.location, &f.store).unwrap());
    let request = f.request(Action::Grant);
    let receipt = f.apply(request);
    assert!(receipt.service_granted());
    assert!(service_granted(&f.store, &f.location.configuration).unwrap());
}

#[test]
fn negative_lookup_is_definitive_only_for_the_unchanged_original_scope() {
    let mut f = Fixture::new();
    let absent = f.request(Action::Grant);
    let query = f.query(&absent);
    let result = lookup(&f.store, &f.location.configuration, query.clone()).unwrap();
    assert!(result.matches(&query));
    assert!(!result.found && result.definitive);
    let another = f.request(Action::Grant);
    f.apply(another);
    let result = lookup(&f.store, &f.location.configuration, query.clone()).unwrap();
    assert!(result.matches(&query));
    assert!(!result.found && !result.definitive);
    let stale = f.apply(absent);
    assert!(matches!(stale.outcome, Outcome::Refused { .. }));
    assert_eq!(f.count(), 1);
}

#[test]
fn read_only_absence_never_creates_state_and_incomplete_state_refuses() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let location = Location {
        root: base.join("absent/desktop"),
        configuration: "a".repeat(64),
    };
    let current: Status = serde_json::from_value(offline_read(&location, None).unwrap()).unwrap();
    assert!(current.matches(&location.configuration));
    assert_eq!(current.state, State::Passive);
    assert!(!location.root.exists());
    location.prepare().unwrap();
    std::fs::write(location.root.join("state.sqlite3-wal"), b"uncertain").unwrap();
    assert!(offline_read(&location, None).is_err());
}

#[test]
fn takeover_recovery_after_restart_only_finishes_the_original_reduction() {
    let mut f = Fixture::new();
    let request = f.request(Action::NativeTakeover);
    let receipt = apply(
        &f.location,
        &mut f.store,
        &f.epoch,
        request.clone(),
        f.origin.clone(),
        None,
        |_| anyhow::bail!("pending"),
    )
    .unwrap();
    assert!(!receipt.native_ready());
    let next_epoch = uuid::Uuid::new_v4().to_string();
    let old = std::mem::replace(&mut f.store, Store::open(&f.location, &next_epoch).unwrap());
    drop(old);
    assert_eq!(f.current().state, State::Draining);
    settle(&f.location, &mut f.store, None, |_| Ok(())).unwrap();
    let receipt = lookup(&f.store, &f.location.configuration, f.query(&request))
        .unwrap()
        .receipt
        .unwrap();
    assert!(receipt.native_ready());
    assert_eq!(f.count(), 1);
    assert_eq!(f.store.effect_status()["revoked"], true);
}

#[test]
fn settled_receipts_are_bounded_and_evicted_ids_cannot_regain_authority() {
    let mut f = Fixture::new();
    let oldest = f.request(Action::Grant);
    f.apply(oldest.clone());
    for _ in 0..RECEIPTS {
        let request = f.request(Action::Grant);
        f.apply(request);
    }
    assert_eq!(f.count(), RECEIPTS);
    let revision = f.current().revision;
    let old = lookup(&f.store, &f.location.configuration, f.query(&oldest)).unwrap();
    assert!(!old.found && !old.definitive);
    let refused = f.apply(oldest);
    assert!(
        matches!(refused.outcome,Outcome::Refused{ref code} if code == "ownership_scope_changed")
    );
    assert_eq!(f.current().revision, revision);
    assert_eq!(f.count(), RECEIPTS);
    let takeover = f.request(Action::NativeTakeover);
    assert!(f.apply(takeover).native_ready());
    assert_eq!(
        f.count(),
        RECEIPTS,
        "settlement also applies the completed-receipt bound"
    );
}

#[test]
fn revoking_a_pending_takeover_closes_its_receipt_without_ever_claiming_native_ready() {
    let mut f = Fixture::new();
    let request = f.request(Action::NativeTakeover);
    apply(
        &f.location,
        &mut f.store,
        &f.epoch,
        request.clone(),
        f.origin.clone(),
        None,
        |_| anyhow::bail!("pending"),
    )
    .unwrap();
    let tx = f.store.db.transaction().unwrap();
    invalidate(&tx).unwrap();
    zerus_desktop_ledger::revoke(&tx, "manual_revoke").unwrap();
    tx.commit().unwrap();
    let receipt = lookup(&f.store, &f.location.configuration, f.query(&request))
        .unwrap()
        .receipt
        .unwrap();
    assert!(receipt.matches(&request, &f.location.configuration));
    assert!(!receipt.native_ready());
    assert!(matches!(receipt.outcome,Outcome::Refused{ref code} if code == "ownership_revoked"));
    let count: i64 = f
        .store
        .db
        .query_row(
            "SELECT count(*) FROM desktop_ownership_requests WHERE final=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "a superseded reduction cannot consume a permanent pending slot"
    );
}

#[test]
fn rearm_cannot_replace_an_original_runtime_that_has_not_drained() {
    let mut f = Fixture::new();
    let grant = f.request(Action::Grant);
    f.apply(grant);
    let tx = f.store.db.transaction().unwrap();
    invalidate(&tx).unwrap();
    tx.commit().unwrap();
    let request = f.request(Action::Rearm);
    let count = f.count();
    let revision = f.current().revision;
    let refused = apply(
        &f.location,
        &mut f.store,
        &f.epoch,
        request.clone(),
        f.origin.clone(),
        None,
        |_| anyhow::bail!("original runtime retained"),
    )
    .unwrap();
    assert!(refused.matches(&request, &f.location.configuration));
    assert!(
        matches!(refused.outcome,Outcome::Refused{ref code} if code == "ownership_drain_pending")
    );
    assert_eq!(f.current().revision, revision);
    assert_eq!(f.count(), count);
    assert!(!service_granted(&f.store, &f.location.configuration).unwrap());
}

#[test]
fn final_native_proof_refusal_rolls_back_before_any_durable_intent() {
    let mut f = Fixture::new();
    let request = f.request(Action::Grant);
    let before = f.current();
    let result = apply_guarded(
        &f.location,
        &mut f.store,
        &f.epoch,
        (request, f.origin.clone()),
        None,
        |_| Ok(()),
        || anyhow::bail!("original native deadline expired after SQLite lock wait"),
    );
    assert!(result.is_err());
    assert_eq!(f.current(), before);
    assert_eq!(f.count(), 0);
}

#[test]
fn old_grant_receipt_cannot_hide_a_later_manual_revoke() {
    let mut f = Fixture::new();
    let grant = f.request(Action::Grant);
    assert!(f.apply(grant.clone()).service_granted());
    let tx = f.store.db.transaction().unwrap();
    invalidate(&tx).unwrap();
    tx.commit().unwrap();
    let receipt = lookup(&f.store, &f.location.configuration, f.query(&grant))
        .unwrap()
        .receipt
        .unwrap();
    assert!(receipt.matches(&grant, &f.location.configuration));
    assert!(matches!(receipt.outcome, Outcome::Granted { .. }));
    assert!(!receipt.service_granted());
}

#[test]
fn identity_update_after_an_explicit_rearm_does_not_resurrect_old_revoke_history() {
    let mut f = Fixture::new();
    let first = f.request(Action::Grant);
    f.apply(first);
    let id = uuid::Uuid::new_v4().to_string();
    super::super::revoke::request(
        &f.location,
        &mut f.store,
        &serde_json::json!({"op":"revoke","request_id":id}),
    );
    let rearm = f.request(Action::Rearm);
    f.apply(rearm);
    let old = f.current();
    let other = f.base.join("new-image");
    std::fs::write(&other, b"updated frontend").unwrap();
    std::fs::rename(other, &f.origin.image).unwrap();
    assert!(!f.current().rearm_required);
    assert_eq!(f.current().state, State::IdentityChanged);
    assert!(!service_granted(&f.store, &f.location.configuration).unwrap());
    assert!(super::super::revoke::background_permitted(&f.location, &f.store).unwrap());
    f.origin = Origin::capture(&f.origin.image, f.origin.build.clone()).unwrap();
    let request = f.request(Action::Grant);
    let receipt = f.apply(request);
    assert!(receipt.service_granted());
    assert!(receipt.revision > old.revision);
}

#[test]
fn absent_native_takeover_is_a_definitive_noop_without_creating_any_desktop_store() {
    let f = Fixture::new();
    // Keep the selected image but remove the private fixture's unused service
    // state to model a never-service native startup, not a live owner.
    let location = Location {
        root: f.base.join("never-service/desktop"),
        configuration: f.location.configuration.clone(),
    };
    let request = Request::DesktopOwnership {
        request_id: uuid::Uuid::new_v4().to_string(),
        action: Action::NativeTakeover,
        journal: None,
        revision: 0,
    };
    let absent = absent_takeover(&location, &request, &f.origin)
        .unwrap()
        .unwrap();
    assert!(absent.receipt.matches(&request, &location.configuration));
    assert!(absent.receipt.native_ready());
    assert!(!absent.receipt.durable);
    assert!(matches!(absent.receipt.outcome, Outcome::NativeAbsent));
    assert!(!location.root.parent().unwrap().exists());
    drop(absent);
    assert!(
        serde_json::from_value::<Status>(offline_read(&location, None).unwrap())
            .unwrap()
            .journal
            .is_none()
    );
    // No retained revoke/ownership row has been manufactured by this no-op.
    assert!(!location.root.exists());
    location.prepare().unwrap();
    let epoch = uuid::Uuid::new_v4().to_string();
    let mut first_service = Store::open(&location, &epoch).unwrap();
    let request = Request::DesktopOwnership {
        request_id: uuid::Uuid::new_v4().to_string(),
        action: Action::Grant,
        journal: None,
        revision: 0,
    };
    let granted = apply(
        &location,
        &mut first_service,
        &epoch,
        request,
        f.origin.clone(),
        None,
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        granted.service_granted(),
        "the native no-op did not manufacture a revoke requiring rearm"
    );
}

#[test]
fn unknown_absence_and_a_live_owner_cannot_supply_an_absent_takeover_receipt() {
    let f = Fixture::new();
    let request = Request::DesktopOwnership {
        request_id: uuid::Uuid::new_v4().to_string(),
        action: Action::NativeTakeover,
        journal: None,
        revision: 0,
    };
    let location = Location {
        root: f.base.join("unknown/desktop"),
        configuration: f.location.configuration.clone(),
    };
    location.prepare().unwrap();
    let owner = location.lock().unwrap();
    assert!(absent_takeover(&location, &request, &f.origin).is_err());
    drop(owner);
    std::fs::write(
        location.root.join("notification-host.lock"),
        b"unknown original native host",
    )
    .unwrap();
    assert!(absent_takeover(&location, &request, &f.origin).is_err());
    assert!(!location.root.join("state.sqlite3").exists());
    let mut old_scope = request;
    if let Request::DesktopOwnership { journal, .. } = &mut old_scope {
        *journal = Some(uuid::Uuid::new_v4().to_string());
    }
    std::fs::remove_file(location.root.join("notification-host.lock")).unwrap();
    assert!(absent_takeover(&location, &old_scope, &f.origin).is_err());
}
