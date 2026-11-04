//! Durable ownership decisions. Only the original native frontend permit can
//! increase authority; completing a prepared takeover only decreases it.
use super::{
    handoff,
    store::{HandoffLock, Location, Store},
    ui_authority,
};
use crate::platform;
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use zerus_client_core::desktop::{
    ownership::{self as wire, Action, Lookup, Outcome, Query, Receipt, State, Status},
    ui_requests::Request,
    Build,
};

const RECEIPTS: i64 = 4096;
const RECORD_BYTES: usize = 8192;

/// Captured from the authenticated canonical process and cached native Build,
/// never decoded from a request. Serialization is private store data only.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Origin {
    image: PathBuf,
    device: u64,
    inode: u64,
    modified: i64,
    modified_nanos: i64,
    build: Build,
}
impl Origin {
    pub(super) fn capture(image: &Path, build: Build) -> Result<Self> {
        ensure!(
            image.is_absolute() && image.as_os_str().len() <= 4096,
            "invalid frontend image"
        );
        let file = platform::file_identity(image).context("frontend image unavailable")?;
        Ok(Self {
            image: image.to_owned(),
            device: file.device,
            inode: file.inode,
            modified: file.modified,
            modified_nanos: file.modified_nanos,
            build,
        })
    }
    pub(super) fn current(&self) -> bool {
        platform::file_identity(&self.image)
            == Some(platform::FileIdentity {
                device: self.device,
                inode: self.inode,
                modified: self.modified,
                modified_nanos: self.modified_nanos,
            })
    }
}

pub(super) fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE desktop_ownership(
        id INTEGER PRIMARY KEY CHECK(id=1), journal TEXT NOT NULL,
        revision INTEGER NOT NULL CHECK(revision>=0), state TEXT NOT NULL,
        request_id TEXT, owner_epoch TEXT, origin TEXT CHECK(length(origin)<=8192),
        rearm_required INTEGER NOT NULL CHECK(rearm_required IN(0,1)));
        CREATE TABLE desktop_ownership_requests(
        request_id TEXT PRIMARY KEY, revision INTEGER NOT NULL UNIQUE,
        fingerprint TEXT NOT NULL, origin TEXT NOT NULL CHECK(length(origin)<=8192),
        receipt TEXT NOT NULL CHECK(length(receipt)<=16384), final INTEGER NOT NULL CHECK(final IN(0,1)));
        CREATE INDEX desktop_ownership_recent ON desktop_ownership_requests(final,revision);")?;
    tx.execute(
        "INSERT INTO desktop_ownership VALUES(1,?1,0,'passive',NULL,NULL,NULL,0)",
        [uuid::Uuid::new_v4().to_string()],
    )?;
    Ok(())
}
struct Row {
    status: Status,
    origin: Option<Origin>,
}
fn row(db: &Connection, configuration: &str) -> Result<Row> {
    let (journal, revision, state, request_id, owner_epoch, origin, rearm_required):
        (String,u64,String,Option<String>,Option<String>,Option<String>,bool) = db.query_row(
        "SELECT substr(journal,1,37),revision,substr(state,1,32),substr(request_id,1,37),substr(owner_epoch,1,37),substr(origin,1,8193),rearm_required FROM desktop_ownership WHERE id=1", [],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
    let mut state: State = serde_json::from_value(Value::String(state))?;
    let mut rearm_required = rearm_required;
    if state == State::Passive {
        let revoked: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM desktop_commands LIMIT 1)",
            [],
            |r| r.get(0),
        )?;
        let marker = db
            .path()
            .and_then(|p| Path::new(p).parent())
            .map(|p| p.join(zerus_desktop_migration::HANDOFF_FILE));
        let marker_exists = match marker.map(std::fs::symlink_metadata) {
            None => false,
            Some(Ok(_)) => true,
            Some(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => false,
            Some(Err(e)) => return Err(e.into()),
        };
        if revoked || marker_exists {
            state = State::Revoked;
            rearm_required = true;
        }
    }
    let origin: Option<Origin> = origin.map(|v| serde_json::from_str(&v)).transpose()?;
    let identity_current = origin.as_ref().is_some_and(Origin::current);
    let state = if state == State::Service && !identity_current {
        State::IdentityChanged
    } else {
        state
    };
    let status = Status {
        abi: wire::ABI.into(),
        configuration: configuration.into(),
        journal: Some(journal),
        revision,
        state,
        rearm_required,
        request_id,
        owner_epoch,
        identity_current,
    };
    ensure!(status.matches(configuration), "invalid ownership state");
    Ok(Row { status, origin })
}
pub(super) fn status(store: &Store, configuration: &str) -> Result<Status> {
    Ok(row(&store.db, configuration)?.status)
}
fn state_name(state: State) -> Result<String> {
    Ok(serde_json::to_value(state)?
        .as_str()
        .context("invalid ownership state")?
        .into())
}
/// Called in the SAME transaction as every manual revoke/rollback. Historical
/// receipts stay readable, but cannot carry a current NativeReady observation.
pub(super) fn invalidate(db: &Connection) -> Result<()> {
    let (revision, pending): (u64, Option<String>) = db.query_row(
        "SELECT revision,CASE WHEN state='draining' THEN request_id END FROM desktop_ownership WHERE id=1", [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if let Some(id) = pending {
        let (mut receipt, _) = original(db, &id)?.context("missing ownership reduction")?;
        receipt.outcome = Outcome::Refused {
            code: "ownership_revoked".into(),
        };
        db.execute(
            "UPDATE desktop_ownership_requests SET receipt=?2,final=1 WHERE request_id=?1",
            params![id, serde_json::to_string(&receipt)?],
        )?;
        prune(db)?;
    }
    ensure!(
        revision < wire::MAX_REVISION,
        "ownership revision exhausted"
    );
    db.execute("UPDATE desktop_ownership SET revision=revision+1,state='revoked',request_id=NULL,owner_epoch=NULL,rearm_required=1 WHERE id=1",[])?;
    Ok(())
}
pub(super) fn service_granted(store: &Store, configuration: &str) -> Result<bool> {
    let current = status(store, configuration)?;
    Ok(current.state == State::Service && current.identity_current && !current.rearm_required)
}
/// A retained grant read by the sole actor. It cannot be constructed from a
/// status/receipt or caller JSON. Runtime preparations retain this exact owner
/// incarnation through their original worker and recheck it before attachment.
#[derive(Clone)]
pub(super) struct Grant {
    configuration: String,
    journal: String,
    owner_epoch: String,
    origin: Origin,
}
impl Grant {
    pub(super) fn current(&self, store: &Store) -> Result<bool> {
        let now = row(&store.db, &self.configuration)?;
        Ok(
            matches!(now.status.state, State::Service | State::IdentityChanged)
                && !now.status.rearm_required
                && now.status.journal.as_ref() == Some(&self.journal)
                && now.status.owner_epoch.as_ref() == Some(&self.owner_epoch)
                && now.origin.as_ref() == Some(&self.origin),
        )
    }
    #[cfg(target_os = "linux")]
    pub(super) fn image(&self) -> &Path {
        &self.origin.image
    }
    pub(super) fn writer_epoch(&self) -> &str {
        &self.owner_epoch
    }
    pub(super) fn revalidate_image(&self) -> Result<()> {
        ensure!(self.origin.current(), "granted frontend image changed");
        Ok(())
    }
    pub(super) fn permits_launch(
        &self,
        store: &Store,
        configuration: &str,
        writer_epoch: &str,
    ) -> Result<()> {
        ensure!(
            self.configuration == configuration
                && self.owner_epoch == writer_epoch
                && self.current(store)?,
            "notification launch grant changed"
        );
        self.revalidate_image()
    }
}
pub(super) fn grant(store: &Store, configuration: &str) -> Result<Option<Grant>> {
    let row = row(&store.db, configuration)?;
    if !matches!(row.status.state, State::Service | State::IdentityChanged)
        || row.status.rearm_required
    {
        return Ok(None);
    }
    Ok(Some(Grant {
        configuration: configuration.into(),
        journal: row
            .status
            .journal
            .context("ownership journal unavailable")?,
        owner_epoch: row
            .status
            .owner_epoch
            .context("ownership epoch unavailable")?,
        origin: row.origin.context("ownership frontend unavailable")?,
    }))
}
fn original(db: &Connection, id: &str) -> Result<Option<(Receipt, Origin)>> {
    let record: Option<(String,String)> = db.query_row(
        "SELECT substr(receipt,1,16385),substr(origin,1,8193) FROM desktop_ownership_requests WHERE request_id=?1",[id],
        |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    record
        .map(|(receipt, origin)| {
            let receipt: Receipt = serde_json::from_str(&receipt)?;
            ensure!(
                receipt.valid()
                    && receipt.request_id == id
                    && receipt.durable
                    && receipt.journal.is_some(),
                "invalid ownership receipt"
            );
            Ok((receipt, serde_json::from_str(&origin)?))
        })
        .transpose()
}
fn current_receipt(db: &Connection, mut receipt: Receipt) -> Result<Receipt> {
    receipt.current = row(db, &receipt.configuration)?.status;
    ensure!(receipt.valid(), "invalid ownership result");
    Ok(receipt)
}
fn save(tx: &Connection, receipt: &Receipt, origin: &Origin, final_result: bool) -> Result<()> {
    let encoded = serde_json::to_string(receipt)?;
    let origin = serde_json::to_string(origin)?;
    ensure!(
        encoded.len() <= wire::BYTES && origin.len() <= RECORD_BYTES,
        "ownership record limit"
    );
    tx.execute("INSERT INTO desktop_ownership_requests(request_id,revision,fingerprint,origin,receipt,final) VALUES(?1,?2,?3,?4,?5,?6)",
        params![receipt.request_id,receipt.revision,receipt.fingerprint,origin,encoded,final_result])?;
    // Only settled receipts are evicted. Revision fencing means an evicted old
    // mutation cannot become a new grant; its missing lookup stays unknown.
    prune(tx)?;
    Ok(())
}
fn prune(db: &Connection) -> Result<()> {
    db.execute("DELETE FROM desktop_ownership_requests WHERE final=1 AND revision < COALESCE((SELECT revision FROM desktop_ownership_requests WHERE final=1 ORDER BY revision DESC LIMIT 1 OFFSET ?1),0)",[RECEIPTS-1])?;
    Ok(())
}
fn refused(request: &Request, current: Status, code: &str) -> Result<Receipt> {
    let Request::DesktopOwnership { action, .. } = request else {
        anyhow::bail!("not ownership")
    };
    Ok(Receipt {
        abi: wire::ABI.into(),
        request_id: request.id().into(),
        configuration: current.configuration.clone(),
        journal: current.journal.clone(),
        fingerprint: request.fingerprint(),
        action: *action,
        revision: current.revision,
        accepted: false,
        durable: false,
        outcome: Outcome::Refused { code: code.into() },
        current,
    })
}

/// The only increasing entry consumes a typed native permit. A grant records
/// consent for this exact image/build; it does not start a host or opt approvals in.
pub(super) fn frontend(
    location: &Location,
    store: &mut Store,
    epoch: &str,
    permit: ui_authority::Validated,
    held: Option<&HandoffLock>,
    drain: impl FnOnce(&mut Store) -> Result<()>,
) -> Result<Receipt> {
    let (request, origin) = permit.ownership_parts()?;
    apply_guarded(
        location,
        store,
        epoch,
        (request, origin),
        held,
        drain,
        || permit.check_ownership(),
    )
}
#[cfg(test)]
fn apply(
    location: &Location,
    store: &mut Store,
    epoch: &str,
    request: Request,
    origin: Origin,
    held: Option<&HandoffLock>,
    drain: impl FnOnce(&mut Store) -> Result<()>,
) -> Result<Receipt> {
    apply_guarded(
        location,
        store,
        epoch,
        (request, origin),
        held,
        drain,
        || Ok(()),
    )
}
fn apply_guarded(
    location: &Location,
    store: &mut Store,
    epoch: &str,
    command: (Request, Origin),
    held: Option<&HandoffLock>,
    drain: impl FnOnce(&mut Store) -> Result<()>,
    check: impl FnOnce() -> Result<()>,
) -> Result<Receipt> {
    let (request, origin) = command;
    let Request::DesktopOwnership {
        action,
        journal,
        revision,
        ..
    } = &request
    else {
        anyhow::bail!("not ownership")
    };
    let acquired = if held.is_none() {
        Some(location.handoff_lock()?)
    } else {
        None
    };
    let lock = held
        .or(acquired.as_ref())
        .context("ownership lock unavailable")?;
    lock.verify(location)?;
    if let Some((receipt, old_origin)) = original(&store.db, request.id())? {
        ensure!(
            receipt.fingerprint == request.fingerprint() && old_origin == origin,
            "ownership request conflict"
        );
        return current_receipt(&store.db, receipt);
    }
    let old = row(&store.db, &location.configuration)?;
    if old.status.revision != *revision
        || journal
            .as_ref()
            .is_some_and(|v| Some(v) != old.status.journal.as_ref())
        || (journal.is_none() && *revision != 0)
    {
        return refused(&request, old.status, "ownership_scope_changed");
    }
    if matches!(old.status.state, State::Draining | State::Preparing) {
        return refused(&request, old.status, "ownership_pending");
    }
    let marker = handoff::marker(location, lock)?;
    let historical_revoke: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM desktop_commands LIMIT 1)",
        [],
        |r| r.get(0),
    )?;
    if *action == Action::Grant
        && (old.status.rearm_required
            || (old.status.state == State::Passive && (historical_revoke || marker.is_some())))
    {
        return refused(&request, old.status, "ownership_rearm_required");
    }
    let mut drain = Some(drain);
    let replacing_image =
        *action == Action::Grant && old.origin.as_ref().is_some_and(|old| old != &origin);
    if (*action == Action::Rearm || replacing_image) && drain.take().unwrap()(store).is_err() {
        return refused(&request, old.status, "ownership_drain_pending");
    }
    if *action != Action::NativeTakeover && store.operation_undrained()? != 0 {
        return refused(&request, old.status, "ownership_resources_pending");
    }
    ensure!(origin.current(), "ownership frontend image changed");
    ensure!(
        old.status.revision < wire::MAX_REVISION,
        "ownership revision exhausted"
    );
    let next = old.status.revision + 1;
    let takeover = *action == Action::NativeTakeover;
    let state = if takeover {
        State::Draining
    } else {
        State::Service
    };
    let keep_epoch = takeover
        || (*action == Action::Grant
            && old.status.state == State::Service
            && old.origin.as_ref() == Some(&origin));
    let owner_epoch = if keep_epoch {
        old.status.owner_epoch.clone()
    } else {
        Some(uuid::Uuid::new_v4().to_string())
    };
    ensure!(
        store
            .db
            .query_row("SELECT epoch FROM service WHERE id=1", [], |r| r
                .get::<_, String>(0))?
            == epoch,
        "ownership service epoch changed"
    );
    let current = Status {
        abi: wire::ABI.into(),
        configuration: location.configuration.clone(),
        journal: old.status.journal.clone(),
        revision: next,
        state,
        rearm_required: takeover,
        request_id: Some(request.id().into()),
        owner_epoch: owner_epoch.clone(),
        identity_current: true,
    };
    let receipt = Receipt {
        abi: wire::ABI.into(),
        request_id: request.id().into(),
        configuration: location.configuration.clone(),
        journal: old.status.journal,
        fingerprint: request.fingerprint(),
        action: *action,
        revision: next,
        accepted: true,
        durable: true,
        outcome: if takeover {
            Outcome::Pending
        } else {
            Outcome::Granted {
                owner_epoch: owner_epoch.clone().context("ownership epoch unavailable")?,
            }
        },
        current,
    };
    let tx = store
        .db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    check()?; // Original deadline, kernel peer/ancestry and image rechecked after lock wait.
    if takeover {
        zerus_desktop_ledger::revoke(&tx, "native_takeover")?;
    }
    tx.execute("UPDATE desktop_ownership SET revision=?1,state=?2,request_id=?3,owner_epoch=?4,origin=?5,rearm_required=?6 WHERE id=1",
        params![next,state_name(state)?,request.id(),owner_epoch,serde_json::to_string(&origin)?,takeover])?;
    save(&tx, &receipt, &origin, !takeover)?;
    tx.commit()?;
    if takeover {
        settle_locked(location, store, lock, drain.take().unwrap())?;
    }
    current_receipt(
        &store.db,
        original(&store.db, request.id())?
            .context("missing ownership receipt")?
            .0,
    )
}

/// Reconcile only a retained authority reduction; never reissue native work or
/// resume an increasing action after a timeout/restart.
pub(super) fn settle(
    location: &Location,
    store: &mut Store,
    held: Option<&HandoffLock>,
    drain: impl FnOnce(&mut Store) -> Result<()>,
) -> Result<()> {
    if status(store, &location.configuration)?.state != State::Draining {
        return Ok(());
    }
    let acquired = if held.is_none() {
        Some(location.handoff_lock()?)
    } else {
        None
    };
    settle_locked(
        location,
        store,
        held.or(acquired.as_ref())
            .context("ownership lock unavailable")?,
        drain,
    )
}
fn settle_locked(
    location: &Location,
    store: &mut Store,
    lock: &HandoffLock,
    drain: impl FnOnce(&mut Store) -> Result<()>,
) -> Result<()> {
    lock.verify(location)?;
    let old = row(&store.db, &location.configuration)?;
    if old.status.state != State::Draining {
        return Ok(());
    }
    let id = old.status.request_id.context("missing takeover identity")?;
    let (mut receipt, _) = original(&store.db, &id)?.context("missing takeover reservation")?;
    ensure!(
        receipt.action == Action::NativeTakeover
            && matches!(receipt.outcome, Outcome::Pending)
            && receipt.revision == old.status.revision,
        "takeover reservation changed"
    );
    ensure!(
        zerus_desktop_ledger::status(&store.db)?["revoked"] == true,
        "takeover fence changed"
    );
    // A bounded wait is not a completed drain. The prepared receipt survives.
    if drain(store).is_err() || store.operation_undrained()? != 0 {
        return Ok(());
    }
    #[cfg(unix)]
    let _native_host = match location.notification_host_exclusion() {
        Ok(held) => held,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(platform::lock_contended) =>
        {
            return Ok(())
        }
        Err(error) => return Err(error),
    };
    if let Some(marker) = handoff::marker(location, lock)? {
        handoff::write_revoked(location, lock, &marker)?;
    }
    if store.checkpoint().is_err() {
        return Ok(());
    }
    receipt.outcome = Outcome::NativeReady {
        prior_owner_epoch: old.status.owner_epoch,
        revoked: true,
        checkpointed: true,
        drained: true,
    };
    receipt.current.state = State::Native;
    receipt.current.owner_epoch = None;
    let tx = store.db.transaction()?;
    tx.execute("UPDATE desktop_ownership SET state='native',owner_epoch=NULL WHERE id=1 AND revision=?1 AND request_id=?2",params![receipt.revision,id])?;
    tx.execute(
        "UPDATE desktop_ownership_requests SET receipt=?2,final=1 WHERE request_id=?1 AND final=0",
        params![id, serde_json::to_string(&receipt)?],
    )?;
    prune(&tx)?;
    tx.commit()?;
    Ok(())
}
pub(super) fn lookup(store: &Store, configuration: &str, query: Query) -> Result<Lookup> {
    lookup_db(&store.db, configuration, query)
}
fn lookup_db(db: &Connection, configuration: &str, query: Query) -> Result<Lookup> {
    ensure!(query.valid(), "invalid ownership query");
    let Query::DesktopOwnershipResult {
        request_id,
        configuration: scope,
        original_request_id,
        fingerprint,
        journal,
        revision,
    } = query;
    ensure!(scope == configuration, "ownership configuration mismatch");
    let current = row(db, configuration)?.status;
    let receipt = original(db, &original_request_id)?
        .map(|(receipt, _)| {
            ensure!(
                receipt.fingerprint == fingerprint
                    && journal
                        .as_ref()
                        .is_none_or(|id| Some(id) == receipt.journal.as_ref()),
                "ownership query conflict"
            );
            current_receipt(db, receipt)
        })
        .transpose()?;
    let definitive = receipt.is_none()
        && current.revision == revision
        && journal.is_some()
        && journal == current.journal;
    Ok(Lookup {
        abi: wire::ABI.into(),
        request_id,
        configuration: configuration.into(),
        found: receipt.is_some(),
        definitive,
        receipt,
    })
}

pub(super) fn result_request(store: &Store, configuration: &str, value: &Value) -> Value {
    let result = serde_json::from_value::<Query>(value.clone())
        .map_err(anyhow::Error::from)
        .and_then(|query| lookup(store, configuration, query))
        .and_then(|result| Ok(serde_json::to_value(result)?));
    result.unwrap_or_else(|_| serde_json::json!({"request_id":value.get("request_id"),"accepted":false,"durable":false,"code":"ownership_read_refused"}))
}

fn absent(configuration: &str) -> Status {
    Status {
        abi: wire::ABI.into(),
        configuration: configuration.into(),
        journal: None,
        revision: 0,
        state: State::Passive,
        rearm_required: false,
        request_id: None,
        owner_epoch: None,
        identity_current: false,
    }
}
/// Independent native-startup reads: no service launch, migration, policy write
/// or recovery. Exclude a running but unreachable owner before reading absence.
pub(super) fn offline_read(location: &Location, query: Option<Query>) -> Result<Value> {
    ensure!(
        query.as_ref().is_none_or(Query::valid),
        "invalid ownership query"
    );
    let _owner = location.existing_lock()?;
    super::store::database_files(location)?;
    let db_path = location.root.join("state.sqlite3");
    if !db_path.try_exists()? {
        ensure!(
            !location
                .root
                .join(zerus_desktop_migration::HANDOFF_FILE)
                .try_exists()?,
            "unknown handoff without database"
        );
        for name in [
            "state.sqlite3-wal",
            "state.sqlite3-shm",
            "state.sqlite3-journal",
        ] {
            ensure!(
                !location.root.join(name).try_exists()?,
                "incomplete ownership state"
            );
        }
        return match query {
            None => Ok(serde_json::to_value(absent(&location.configuration))?),
            Some(Query::DesktopOwnershipResult {
                request_id,
                configuration,
                ..
            }) => {
                ensure!(
                    configuration == location.configuration,
                    "ownership configuration mismatch"
                );
                Ok(serde_json::to_value(Lookup {
                    abi: wire::ABI.into(),
                    request_id,
                    configuration,
                    found: false,
                    definitive: false,
                    receipt: None,
                })?)
            }
        };
    }
    let db = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(std::time::Duration::from_millis(250))?;
    db.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;")?;
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    ensure!(
        (1..=super::store::SCHEMA).contains(&version),
        "unknown ownership schema"
    );
    let configuration: String = db.query_row(
        "SELECT substr(configuration,1,65) FROM service WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        configuration == location.configuration,
        "ownership configuration mismatch"
    );
    if version < 15 {
        ensure!(query.is_none(), "ownership journal unavailable");
        let mut status = absent(&configuration);
        // Old state has no native ownership grant, even if other writers ran.
        status.state = State::Unavailable;
        status.rearm_required = true;
        return Ok(serde_json::to_value(status)?);
    }
    match query {
        None => Ok(serde_json::to_value(row(&db, &configuration)?.status)?),
        Some(query) => Ok(serde_json::to_value(lookup_db(
            &db,
            &configuration,
            query,
        )?)?),
    }
}

/// An original native child can reduce ownership before Bootstrap even when
/// the service is absent. The owner lock excludes an unreachable live runtime.
pub(super) fn offline_frontend(
    config: &crate::config::Config,
    location: &Location,
    value: &Value,
) -> Result<Value> {
    let scope = ui_authority::Scope::new(config, location);
    let authority = ui_authority::authenticate(&scope, std::process::id(), value)?;
    ensure!(
        authority.operation() == "desktop_ownership",
        "service is not running"
    );
    let mut order = ui_authority::Order::default();
    let permit = authority.into_request(&mut order)?;
    let (request, origin) = permit.ownership_parts()?;
    ensure!(
        matches!(
            request,
            Request::DesktopOwnership {
                action: Action::NativeTakeover,
                ..
            }
        ),
        "service grant requires a running service"
    );
    if let Some(absent) = absent_takeover(location, &request, &origin)? {
        permit.check_ownership()?;
        // Serialize while the original existing-owner exclusion is still held.
        return Ok(serde_json::to_value(&absent.receipt)?);
    }
    location.prepare()?;
    let _owner = location.lock()?;
    let epoch = uuid::Uuid::new_v4().to_string();
    let mut store = Store::open(location, &epoch)?;
    // Native mode is checked before this path can create increasing authority.
    // Consume/recheck the original two-second permit immediately before commit.
    // Existing custody must be confirmed by its original recovery, not assumed
    // absent from this short-lived CLI's handle table.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        store.recover_operation_custody(location);
        if store.operation_status()["resource_custody"]["state"] != "probing"
            || std::time::Instant::now() >= until
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let (request, origin) = permit.ownership_parts()?;
    ensure!(
        matches!(
            request,
            Request::DesktopOwnership {
                action: Action::NativeTakeover,
                ..
            }
        ),
        "service grant requires a running service"
    );
    Ok(serde_json::to_value(apply_guarded(
        location,
        &mut store,
        &epoch,
        (request, origin),
        None,
        |_| Ok(()),
        || permit.check_ownership(),
    )?)?)
}

struct AbsentTakeover {
    receipt: Receipt,
    _owner: Option<platform::FileLock>,
}
/// No authority increase and no file/directory creation. Unknown retained state
/// or a live but unreachable service never becomes an absence observation.
fn absent_takeover(
    location: &Location,
    request: &Request,
    origin: &Origin,
) -> Result<Option<AbsentTakeover>> {
    let owner = location.existing_lock()?;
    let exists = |name: &str| -> Result<bool> {
        match std::fs::symlink_metadata(location.root.join(name)) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    };
    if exists("state.sqlite3")? {
        return Ok(None);
    }
    for name in [
        "state.sqlite3-wal",
        "state.sqlite3-shm",
        "state.sqlite3-journal",
        zerus_desktop_migration::HANDOFF_FILE,
        "notification-host.lock",
    ] {
        ensure!(!exists(name)?, "ownership state is incomplete");
    }
    ensure!(
        matches!(
            request,
            Request::DesktopOwnership {
                action: Action::NativeTakeover,
                journal: None,
                revision: 0,
                ..
            }
        ),
        "original ownership state is missing"
    );
    ensure!(origin.current(), "native frontend changed");
    let mut current = absent(&location.configuration);
    current.state = State::Native;
    current.request_id = Some(request.id().into());
    current.identity_current = true;
    let receipt = Receipt {
        abi: wire::ABI.into(),
        request_id: request.id().into(),
        configuration: location.configuration.clone(),
        journal: None,
        revision: 0,
        fingerprint: request.fingerprint(),
        action: Action::NativeTakeover,
        accepted: true,
        durable: false,
        outcome: Outcome::NativeAbsent,
        current,
    };
    ensure!(
        receipt.matches(request, &location.configuration) && receipt.native_ready(),
        "invalid absent takeover receipt"
    );
    Ok(Some(AbsentTakeover {
        receipt,
        _owner: owner,
    }))
}

#[cfg(test)]
mod tests;
