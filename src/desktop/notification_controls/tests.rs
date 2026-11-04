use super::*;
use crate::desktop::store::Location;
use wire::NativeFocus;
struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    runtime: Runtime,
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let location = Location {
            root: dir.path().canonicalize().unwrap().join("state/desktop"),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let epoch = id();
        let store = Store::open(&location, &epoch).unwrap();
        let runtime = Runtime::new(&store, &location.configuration, &epoch).unwrap();
        Self {
            _dir: dir,
            store,
            runtime,
        }
    }
    fn command(&self, optional_questions: bool) -> Command {
        let current = status(&self.store.db, &self.runtime.scope).unwrap();
        Command::Configure {
            request_id: id(),
            journal: current.journal,
            revision: current.revision,
            preferences: Preferences { optional_questions },
        }
    }
    fn apply(&mut self, control: Command) -> Receipt {
        let permit = ui_authority::Validated::notification_fixture(
            self.runtime.scope.clone(),
            control,
            None,
        );
        serde_json::from_value(self.runtime.frontend(&mut self.store, permit)).unwrap()
    }
    fn lookup(&mut self, command: Command) -> Lookup {
        let query = Query {
            request_id: id(),
            scope: self.runtime.scope.clone(),
            original: command,
        };
        let reply: Lookup = serde_json::from_value(self.runtime.request(
            &self.store,
            &json!({"op":"notification_preferences_result","query":query}),
        ))
        .unwrap();
        assert!(reply.matches(&query));
        reply
    }
    fn observe(&mut self, observation: NativeFocus) -> FocusReply {
        let command = Command::Focus { request_id: id() };
        let permit = ui_authority::Validated::notification_fixture(
            self.runtime.scope.clone(),
            command.clone(),
            Some(observation.clone()),
        );
        let reply: FocusReply =
            serde_json::from_value(self.runtime.frontend(&mut self.store, permit)).unwrap();
        assert!(reply.matches(&command, &self.runtime.scope));
        assert_eq!(reply.observation, observation);
        reply
    }
}
#[test]
fn preferences_are_passive_idempotent_and_historical_receipts_never_reconfigure() {
    let mut f = Fixture::new();
    let first = f.command(true);
    let saved = f.apply(first.clone());
    assert_eq!(saved.outcome, Outcome::Saved);
    assert!(saved.matches(&first, &f.runtime.scope.configuration));
    assert_eq!(
        zerus_desktop_ledger::status(&f.store.db).unwrap()["revoked"],
        true
    );
    let second = f.command(false);
    f.apply(second);
    assert!(!f.runtime.preferences().optional_questions);
    let before = f.store.db.total_changes();
    assert_eq!(f.apply(first.clone()), saved);
    assert_eq!(f.store.db.total_changes(), before);
    assert!(!f.runtime.preferences().optional_questions);
    assert_eq!(f.lookup(first).receipt, Some(saved));
    let restarted = Runtime::new(&f.store, &f.runtime.scope.configuration, &id()).unwrap();
    assert!(!restarted.preferences().optional_questions);
}
#[test]
fn configure_scope_cas_and_original_uuid_conflicts_cannot_overwrite_settings() {
    let mut f = Fixture::new();
    let stale = f.command(false);
    let first = f.command(true);
    f.apply(first.clone());
    let refused = f.apply(stale.clone());
    assert!(matches!(refused.outcome, Outcome::Refused { .. }));
    assert!(refused.durable);
    assert_eq!(f.lookup(stale).receipt, Some(refused));
    assert!(f.runtime.preferences().optional_questions);
    let Command::Configure { request_id, .. } = first else {
        unreachable!()
    };
    let current = f.command(false);
    let Command::Configure {
        journal,
        revision,
        preferences,
        ..
    } = current
    else {
        unreachable!()
    };
    let conflict = Command::Configure {
        request_id,
        journal,
        revision,
        preferences,
    };
    let permit =
        ui_authority::Validated::notification_fixture(f.runtime.scope.clone(), conflict, None);
    assert_eq!(f.runtime.frontend(&mut f.store, permit)["accepted"], false);
    let wrong = Scope {
        service_epoch: id(),
        ..f.runtime.scope.clone()
    };
    let permit = ui_authority::Validated::notification_fixture(wrong, f.command(false), None);
    let before = f.store.db.total_changes();
    assert_eq!(f.runtime.frontend(&mut f.store, permit)["accepted"], false);
    assert_eq!(f.store.db.total_changes(), before);
}
#[test]
fn preference_reads_are_pure_and_only_same_journal_unchanged_revision_is_definitive() {
    let mut f = Fixture::new();
    let missing = f.command(true);
    let before = f.store.db.total_changes();
    assert!(f.lookup(missing.clone()).definitive_not_applied);
    let Command::Configure {
        request_id,
        revision,
        preferences,
        ..
    } = missing
    else {
        unreachable!()
    };
    assert!(
        !f.lookup(Command::Configure {
            request_id,
            journal: None,
            revision,
            preferences
        })
        .definitive_not_applied
    );
    let read = f.runtime.request(
        &f.store,
        &json!({"op":"notification_preferences_status","request_id":id(),"scope":f.runtime.scope}),
    );
    assert!(serde_json::from_value::<Status>(read).unwrap().valid());
    assert_eq!(f.store.db.total_changes(), before);
    let original = f.command(true);
    f.apply(original.clone());
    let Command::Configure {
        journal,
        revision,
        preferences,
        ..
    } = original
    else {
        unreachable!()
    };
    assert!(
        !f.lookup(Command::Configure {
            request_id: id(),
            journal,
            revision,
            preferences
        })
        .definitive_not_applied
    );
}
#[test]
fn native_focus_is_ephemeral_app_wide_and_clears_on_loss_reconnect_and_expiry() {
    let mut f = Fixture::new();
    let connection = id();
    let observation = NativeFocus {
        connection_id: connection.clone(),
        revision: 1,
        state: FocusState::Foreground,
    };
    let before = f.store.db.total_changes();
    let reply = f.observe(observation);
    assert!(reply.ttl_ms > 0 && reply.ttl_ms <= 5000);
    assert!(f.runtime.foreground_until().is_some());
    f.observe(NativeFocus {
        connection_id: connection,
        revision: 2,
        state: FocusState::Background,
    });
    assert!(f.runtime.foreground_until().is_none());
    f.observe(NativeFocus {
        connection_id: id(),
        revision: 0,
        state: FocusState::Unknown,
    });
    assert!(f.runtime.foreground_until().is_none());
    f.observe(NativeFocus {
        connection_id: id(),
        revision: 1,
        state: FocusState::Foreground,
    });
    f.runtime.focus.as_mut().unwrap().peer.expire_for_test();
    assert!(f.runtime.foreground_until().is_none());
    assert_eq!(
        f.store.db.total_changes(),
        before,
        "focus never enters the preferences journal"
    );
}

#[test]
fn stale_native_revision_or_scope_cannot_replace_a_newer_foreground_lease() {
    let mut f = Fixture::new();
    let connection = id();
    let newest = NativeFocus {
        connection_id: connection.clone(),
        revision: 2,
        state: FocusState::Foreground,
    };
    f.observe(newest.clone());
    for revision in [1, 2] {
        let permit = ui_authority::Validated::notification_fixture(
            f.runtime.scope.clone(),
            Command::Focus { request_id: id() },
            Some(NativeFocus {
                connection_id: connection.clone(),
                revision,
                state: FocusState::Background,
            }),
        );
        assert_eq!(f.runtime.frontend(&mut f.store, permit)["accepted"], false);
        assert_eq!(f.runtime.focus.as_ref().unwrap().observation, newest);
    }
    let wrong = Scope {
        service_epoch: id(),
        ..f.runtime.scope.clone()
    };
    let permit = ui_authority::Validated::notification_fixture(
        wrong,
        Command::Focus { request_id: id() },
        Some(NativeFocus {
            connection_id: connection,
            revision: 3,
            state: FocusState::Unknown,
        }),
    );
    assert_eq!(f.runtime.frontend(&mut f.store, permit)["accepted"], false);
    assert!(f.runtime.foreground_until().is_some());
}

#[test]
fn receipt_retention_is_bounded_and_eviction_does_not_restore_an_old_revision() {
    let mut f = Fixture::new();
    let old = f.command(false);
    f.store
        .db
        .execute_batch(
            "WITH RECURSIVE ids(n) AS(VALUES(1) UNION ALL SELECT n+1 FROM ids WHERE n<4097)
        INSERT INTO desktop_notification_preferences_requests(request_id,fingerprint,receipt)
        SELECT printf('%08x-0000-4000-8000-000000000000',n),'fixture','{}' FROM ids;",
        )
        .unwrap();
    f.apply(f.command(true));
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT COUNT(*) FROM desktop_notification_preferences_requests",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        RECEIPTS
    );
    assert!(!f.lookup(old).definitive_not_applied);
    assert_eq!(
        f.store
            .db
            .query_row("SELECT COUNT(*) FROM desktop_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn restart_keeps_preferences_and_receipts_but_uses_fresh_scope_without_focus() {
    let mut f = Fixture::new();
    let control = f.command(true);
    let receipt = f.apply(control.clone());
    f.observe(NativeFocus {
        connection_id: id(),
        revision: 1,
        state: FocusState::Foreground,
    });
    let location = Location {
        root: f._dir.path().canonicalize().unwrap().join("state/desktop"),
        configuration: f.runtime.scope.configuration.clone(),
    };
    drop(f.store);
    let epoch = id();
    let store = Store::open(&location, &epoch).unwrap();
    let mut runtime = Runtime::new(&store, &location.configuration, &epoch).unwrap();
    assert!(runtime.preferences().optional_questions);
    assert!(runtime.foreground_until().is_none());
    let query = Query {
        request_id: id(),
        scope: runtime.scope.clone(),
        original: control,
    };
    let lookup: Lookup = serde_json::from_value(runtime.request(
        &store,
        &json!({"op":"notification_preferences_result","query":query}),
    ))
    .unwrap();
    assert!(lookup.matches(&query));
    assert_eq!(lookup.receipt, Some(receipt));
    assert_eq!(lookup.scope.service_epoch, epoch);
    assert_ne!(
        lookup.receipt.unwrap().scope.service_epoch,
        epoch,
        "historical receipt is not current scope"
    );
}

#[test]
fn schema_fifteen_migration_is_atomic_and_creates_no_writer_permission() {
    let f = Fixture::new();
    crate::desktop::cache_maintenance::remove_from_downgraded_fixture(&f.store.db);
    let location = Location {
        root: f._dir.path().canonicalize().unwrap().join("state/desktop"),
        configuration: f.runtime.scope.configuration.clone(),
    };
    drop(f.store);
    let db = Connection::open(location.root.join("state.sqlite3")).unwrap();
    db.execute_batch("DROP TABLE desktop_creation_clones; DROP TABLE desktop_creation_clone_budget; DROP TABLE desktop_notification_preferences_requests; DROP TABLE desktop_notification_preferences;
        CREATE TABLE desktop_notification_preferences(fixture TEXT); INSERT INTO desktop_notification_preferences VALUES('retained'); PRAGMA user_version=15;").unwrap();
    assert!(Store::open(&location, &id()).is_err());
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        15
    );
    assert_eq!(
        db.query_row(
            "SELECT fixture FROM desktop_notification_preferences",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "retained"
    );
    db.execute("DROP TABLE desktop_notification_preferences", [])
        .unwrap();
    drop(db);
    let store = Store::open(&location, &id()).unwrap();
    assert_eq!(
        store
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        crate::desktop::store::SCHEMA
    );
    assert_eq!(
        zerus_desktop_ledger::status(&store.db).unwrap()["revoked"],
        true
    );
}
