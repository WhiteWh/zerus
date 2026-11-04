//! A physical rollback exclusion, not a serializable ownership capability.
//! The original child holds the same handoff lock across service replacement.
use super::*;
use crate::{config::Config, desktop::ui_authority, platform};
use rusqlite::{Connection, OpenFlags};
use std::{
    io::{self, Read, Write},
    time::{Duration, Instant},
};

pub(super) const ABI: &str = "desktop-revoke-hold-v1";
const ACQUIRE: Duration = Duration::from_secs(2);

struct Held {
    lock: HandoffLock,
    _native: Option<platform::FileLock>,
    receipt: Value,
}

fn database_file(path: &std::path::Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    platform::no_follow(&mut options);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        platform::refuse_link(&mut options).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    #[cfg(windows)]
    platform::never_block(&mut options);
    let file = options.open(path)?;
    ensure!(
        platform::verify_private_file(&file)?,
        "unsafe rollback database"
    );
    Ok(file)
}

fn database(location: &Location) -> Result<(Connection, std::fs::File)> {
    location.check_existing()?;
    super::super::store::database_files(location)?;
    let path = location.root.join("state.sqlite3");
    let file = database_file(&path)?;
    let before = platform::FileIdentity::of(&file)?;
    #[cfg(windows)]
    ensure!(
        before.inode != 0,
        "rollback database object identity unavailable"
    );
    let db = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(Duration::from_millis(250))?;
    db.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA cache_size=-512;")?;
    let after = platform::FileIdentity::of(&database_file(&path)?)?;
    ensure!(
        before.device == after.device && before.inode == after.inode,
        "rollback database changed"
    );
    Ok((db, file))
}

fn verify(location: &Location, id: &str, lock: &HandoffLock) -> Result<Value> {
    lock.verify(location)?;
    let (mut db, verification_file) = database(location)?;
    let tx = db.transaction()?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    ensure!(
        (15..=super::super::store::SCHEMA).contains(&version),
        Declined("unsupported_schema")
    );
    let configuration: String = tx.query_row(
        "SELECT substr(configuration,1,65) FROM service WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        configuration == location.configuration,
        Declined("configuration_changed")
    );
    let saved = record_db(&tx, id)?.context(Declined("request_not_found"))?;
    let command = Command {
        op: "revoke".into(),
        request_id: id.into(),
        reason: Reason::Rollback,
    };
    ensure!(
        saved.reason == "rollback" && saved.fingerprint == command.fingerprint(location),
        Declined("request_conflict")
    );
    ensure!(saved.state == "settled", Declined("revocation_pending"));
    ensure!(
        saved.writer_epoch.as_deref().is_none_or(canonical_id),
        "invalid original writer epoch"
    );
    // The newest append-only command must still be this exact rollback. A
    // later revoke must not let this hold adopt another writer's receipt.
    let latest: String = tx.query_row(
        "SELECT substr(request_id,1,37) FROM desktop_commands ORDER BY rowid DESC LIMIT 1",
        [],
        |r| r.get(0),
    )?;
    let (state, rearm, request, owner): (String, bool, Option<String>, Option<String>) = tx.query_row(
        "SELECT substr(state,1,32),rearm_required,substr(request_id,1,37),substr(owner_epoch,1,37) FROM desktop_ownership WHERE id=1", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    ensure!(
        latest == id && state == "revoked" && rearm && request.is_none() && owner.is_none(),
        Declined("writer_authority_changed")
    );
    let (epoch, revoked): (Option<String>, bool) = tx.query_row(
        "SELECT substr(epoch,1,37),revoked FROM desktop_writer WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        revoked
            && epoch
                .as_ref()
                .is_none_or(|epoch| Some(epoch) == saved.writer_epoch.as_ref()),
        Declined("writer_epoch_changed")
    );
    let marker = handoff::marker(location, lock)?;
    ensure!(
        marker.as_ref().is_none_or(|marker| marker.phase
            == zerus_desktop_migration::Phase::Revoked
            && Some(&marker.writer_epoch) == saved.writer_epoch.as_ref()),
        Declined("writer_epoch_changed")
    );
    ensure!(
        crate::desktop::store::operation_undrained(&tx)? == 0,
        Declined("operation_resources_not_drained")
    );
    let result = receipt(location, &command, &saved)?;
    tx.commit()?;
    // In particular, do not retain a Windows verification handle or SQLite
    // connection into Ready: the passive replacement must be able to write.
    drop(db);
    drop(verification_file);
    Ok(result)
}

fn acquire(location: &Location, id: &str, until: Instant) -> Result<Held> {
    ensure!(canonical_id(id), Declined("invalid_request_id"));
    // Never create desktop state/locks for a never-used configuration.
    ensure!(
        location.root.join("state.sqlite3").try_exists()?,
        Declined("request_not_found")
    );
    let lock = loop {
        if let Some(lock) = location.try_handoff_lock()? {
            break lock;
        }
        ensure!(Instant::now() < until, Declined("handoff_unavailable"));
        std::thread::sleep(Duration::from_millis(10));
    };
    // This lock can outlive the service on Mac; its acquisition is independent
    // physical proof that the original native host/callback owner is gone.
    #[cfg(unix)]
    let native = location
        .notification_host_exclusion()
        .context(Declined("notification_host_not_drained"))?;
    // Windows COM/delegate work retains the actual Owner/handoff resource in
    // process. The acquired original handoff lock proves that resource drained;
    // there is no independent Mac-style native-host process lock to invent.
    #[cfg(windows)]
    let native = None;
    let receipt = verify(location, id, &lock)?;
    Ok(Held {
        lock,
        _native: native,
        receipt,
    })
}

fn write(output: &mut impl Write, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= 8192, "rollback hold reply exceeds bound");
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn serve(held: Held, input: &mut impl Read, output: &mut impl Write) -> Result<()> {
    let ready = json!({"abi":ABI,"held":true,"receipt":held.receipt});
    write(output, &ready)?;
    // No deadline may silently release the fence under a manager operation.
    // EOF of this original stdin is the only normal release. Unexpected bytes
    // are discarded with constant memory but still cannot release it early.
    let mut invalid = false;
    let mut bytes = [0; 64];
    loop {
        match input.read(&mut bytes) {
            Ok(0) => break,
            Ok(_) => invalid = true,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let receipt = held.receipt.clone();
    drop(held);
    write(
        output,
        &json!({"abi":ABI,"held":false,"released":true,"receipt":receipt}),
    )?;
    ensure!(!invalid, "rollback hold accepts stdin EOF only");
    Ok(())
}

pub(in crate::desktop) fn cli(config: &Config, location: &Location, id: &str) -> Result<()> {
    let scope = ui_authority::Scope::new(config, location);
    let held = (|| {
        #[cfg(windows)]
        let caller = ui_authority::registration::Caller::capture(&scope)?;
        #[cfg(unix)]
        ui_authority::registration::caller(&scope)?;
        let held = acquire(location, id, Instant::now() + ACQUIRE)?;
        #[cfg(windows)]
        caller.check(&scope)?;
        #[cfg(unix)]
        ui_authority::registration::caller(&scope)?;
        held.lock.verify(location)?;
        Ok::<_, anyhow::Error>(held)
    })();
    match held {
        Ok(held) => serve(held, &mut io::stdin().lock(), &mut io::stdout().lock()),
        Err(error) => {
            let code = error
                .downcast_ref::<Declined>()
                .map(|e| e.0)
                .unwrap_or("revoke_hold_refused");
            write(
                &mut io::stdout().lock(),
                &json!({"abi":ABI,"held":false,"configuration":location.configuration,
                "request_id":canonical_id(id).then_some(id),"code":code}),
            )?;
            anyhow::bail!("original rollback exclusion is unavailable; keep its request UUID and inspect status")
        }
    }
}

#[cfg(test)]
pub(in crate::desktop) fn acquire_for_test(location: &Location, id: &str) -> Result<impl Sized> {
    acquire(location, id, Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        net::{Shutdown, TcpListener, TcpStream},
    };

    fn streams() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let client =
            TcpStream::connect_timeout(&listener.local_addr().unwrap(), Duration::from_secs(2))
                .unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        let child = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < until =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("private hold stream accept failed: {error}"),
            }
        };
        // BSD can inherit O_NONBLOCK from the listener; both sides use the
        // same explicitly blocking, bounded protocol reads on every platform.
        for stream in [&client, &child] {
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
        }
        (client, child)
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        location: Location,
        store: Store,
        id: String,
        epoch: String,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap().join("state/desktop");
            let location = Location {
                root,
                configuration: "a".repeat(64),
            };
            location.prepare().unwrap();
            let epoch = uuid::Uuid::new_v4().to_string();
            let store = Store::open(&location, &epoch).unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            let mut f = Self {
                _temp: temp,
                location,
                store,
                id,
                epoch,
            };
            f.revoke();
            f
        }
        fn revoke(&mut self) {
            let command = Command {
                op: "revoke".into(),
                request_id: self.id.clone(),
                reason: Reason::Rollback,
            };
            execute_scoped(
                &self.location,
                &mut self.store,
                &command,
                None,
                Some(&self.epoch),
                |_| Ok(()),
            )
            .unwrap();
        }
        fn hold(&self) -> Result<Held> {
            acquire(&self.location, &self.id, Instant::now())
        }
    }

    #[test]
    fn a_settled_receipt_cannot_hide_an_original_cache_resource() {
        let mut f = Fixture::new();
        let original = zerus_desktop_ledger::operations::cache::Original {
            preparation_id: uuid::Uuid::new_v4().to_string(),
            operation_id: uuid::Uuid::new_v4().to_string(),
            configuration: f.location.configuration.clone(),
            root: "/srv/example/cache".into(),
            limit: 1,
            defer_ms: 0,
            policy_change: true,
            revision: 0,
            service_epoch: f.epoch.clone(),
            writer_epoch: uuid::Uuid::new_v4().to_string(),
            lease: uuid::Uuid::new_v4().to_string(),
        };
        let tx = f.store.db.transaction().unwrap();
        zerus_desktop_ledger::operations::cache::begin(&tx, &original, &"f".repeat(64)).unwrap();
        tx.commit().unwrap();
        let Err(error) = f.hold() else {
            panic!("cache resource must refuse held rollback");
        };
        assert!(
            error
                .to_string()
                .contains("operation_resources_not_drained"),
            "{error:#}"
        );
    }

    #[test]
    fn original_flock_survives_actor_restart_and_releases_only_on_original_eof_and_join() {
        let f = Fixture::new();
        let held = f.hold().unwrap();
        let (client, mut child) = streams();
        let mut output = child.try_clone().unwrap();
        let worker = std::thread::spawn(move || serve(held, &mut child, &mut output));
        let mut reader = BufReader::new(client);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["held"], true);
        assert_eq!(ready["receipt"]["writer_epoch"], f.epoch);
        assert!(f.location.handoff_lock().is_err());
        drop(f.store); // Manager stop/restart: the original holder is independent.
        let _restarted = Store::open(&f.location, &uuid::Uuid::new_v4().to_string()).unwrap();
        assert!(f.location.handoff_lock().is_err());
        reader.get_ref().shutdown(Shutdown::Write).unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["released"],
            true
        );
        worker.join().unwrap().unwrap();
        assert!(f.location.handoff_lock().is_ok());
    }

    #[test]
    fn a_newer_rollback_or_fresh_service_state_cannot_lend_its_receipt_to_this_id() {
        let mut f = Fixture::new();
        let original = f.id.clone();
        f.id = uuid::Uuid::new_v4().to_string();
        f.revoke();
        assert!(acquire(&f.location, &original, Instant::now()).is_err());
        assert!(f.hold().is_ok());
        f.store
            .db
            .execute(
                "UPDATE desktop_ownership SET state='service',rearm_required=0 WHERE id=1",
                [],
            )
            .unwrap();
        assert!(f.hold().is_err());
        assert!(
            f.location.handoff_lock().is_ok(),
            "refusal releases only its own temporary acquisition"
        );
    }

    #[test]
    fn malformed_receipt_wrong_configuration_and_active_native_resource_refuse() {
        let mut f = Fixture::new();
        let original_native_owner = f.location.handoff_lock().unwrap();
        assert!(f.hold().is_err());
        drop(original_native_owner);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(f.location.root.join("notification-host.lock"))
                .unwrap();
            fs2::FileExt::try_lock_exclusive(&file).unwrap();
            assert!(f.hold().is_err());
            fs2::FileExt::unlock(&file).unwrap();
        }
        assert!(f.hold().is_ok());
        f.store
            .db
            .execute(
                "UPDATE desktop_commands SET result='{}' WHERE request_id=?1",
                [&f.id],
            )
            .unwrap();
        assert!(f.hold().is_err());
        f.location.configuration = "b".repeat(64);
        assert!(f.hold().is_err());
    }

    #[test]
    fn a_linked_database_is_refused_through_the_actual_verification_handle() {
        let f = Fixture::new();
        let alias = f.location.root.join("database-alias");
        std::fs::hard_link(f.location.root.join("state.sqlite3"), &alias).unwrap();
        assert!(f.hold().is_err());
        std::fs::remove_file(alias).unwrap();
        assert!(f.hold().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn verification_open_refuses_a_symlink_to_an_otherwise_valid_database() {
        let f = Fixture::new();
        let path = f.location.root.join("state.sqlite3");
        assert!(database_file(&path).is_ok());
        let alias = f.location.root.join("database-symlink");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(database_file(&alias).is_err());
    }

    #[test]
    fn missing_database_creates_nothing_and_unknown_schema_is_not_repaired() {
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: temp.path().canonicalize().unwrap().join("missing/desktop"),
            configuration: "a".repeat(64),
        };
        assert!(acquire(&location, &uuid::Uuid::new_v4().to_string(), Instant::now()).is_err());
        assert!(!location.root.parent().unwrap().exists());
        let f = Fixture::new();
        f.store.db.pragma_update(None, "user_version", 999).unwrap();
        assert!(f.hold().is_err());
        assert_eq!(
            f.store
                .db
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            999
        );
    }

    #[test]
    fn unexpected_input_does_not_release_before_eof_and_broken_ready_never_grants_a_hold() {
        let f = Fixture::new();
        struct Input<'a> {
            location: &'a Location,
            first: bool,
        }
        impl Read for Input<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                assert!(self.location.handoff_lock().is_err());
                if self.first {
                    self.first = false;
                    bytes[0] = b'x';
                    Ok(1)
                } else {
                    Ok(0)
                }
            }
        }
        let mut output = Vec::new();
        assert!(serve(
            f.hold().unwrap(),
            &mut Input {
                location: &f.location,
                first: true
            },
            &mut output
        )
        .is_err());
        assert!(f.location.handoff_lock().is_ok());
        assert_eq!(String::from_utf8(output).unwrap().lines().count(), 2);
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(serve(f.hold().unwrap(), &mut io::empty(), &mut Broken).is_err());
        assert!(f.location.handoff_lock().is_ok());
    }
}
