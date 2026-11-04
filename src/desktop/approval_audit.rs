//! One bounded read-only connection to the selected service's private journal.
use super::store::Location;
use crate::{config::Config, platform};
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, ToSql};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};
use zerus_client_core::desktop::approval_audit::{self as wire, Request};

pub(super) mod worker;

struct Journal {
    file: File,
    identity: (u64, u64),
    token: String,
    db: Connection,
}
fn key(file: &File) -> Result<(u64, u64)> {
    let id = platform::FileIdentity::of(file)?;
    Ok((id.device, id.inode))
}
impl Journal {
    fn open(root: &Path) -> Result<Option<Self>> {
        match fs::symlink_metadata(root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
            Ok(_) => (),
        }
        ensure!(
            platform::verify_private(root, platform::Private::Directory)?,
            "unsafe audit directory"
        );
        let path = root.join("journal.sqlite3");
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
            Ok(_) => (),
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        platform::no_follow(&mut options);
        #[cfg(windows)]
        platform::refuse_link(&mut options);
        let file = options.open(&path)?;
        ensure!(platform::verify_private_file(&file)?, "unsafe audit file");
        let identity = key(&file)?;
        let db = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(Duration::from_millis(100))?;
        db.execute_batch(
            "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA cache_size=-2048;",
        )?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(version == 1, "audit schema requires migration");
        let result = Self {
            file,
            identity,
            token: uuid::Uuid::new_v4().to_string(),
            db,
        };
        result.check(root)?;
        Ok(Some(result))
    }
    fn check(&self, root: &Path) -> Result<()> {
        let path = root.join("journal.sqlite3");
        ensure!(
            platform::verify_private(root, platform::Private::Directory)?
                && platform::verify_private(&path, platform::Private::File)?
                && platform::verify_private_file(&self.file)?,
            "audit protection changed"
        );
        let current = platform::file_identity(&path)
            .ok_or_else(|| anyhow::anyhow!("audit file unavailable"))?;
        ensure!(
            (current.device, current.inode) == self.identity && key(&self.file)? == self.identity,
            "audit journal replaced"
        );
        self.connection_current()?;
        let version: i64 = self.db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(version == 1, "audit schema changed");
        Ok(())
    }
    #[cfg(unix)]
    fn connection_current(&self) -> Result<()> {
        use std::{ffi::CStr, os::unix::ffi::OsStrExt};
        // SQLite's public file control checks its OWN open file against its
        // filename. Also join that filename to our retained descriptor, so
        // swapping a parent path during open cannot select a different DB.
        let name =
            unsafe { rusqlite::ffi::sqlite3_db_filename(self.db.handle(), c"main".as_ptr()) };
        ensure!(!name.is_null(), "audit SQLite file unavailable");
        let path = Path::new(std::ffi::OsStr::from_bytes(
            unsafe { CStr::from_ptr(name) }.to_bytes(),
        ));
        let identity = platform::file_identity(path)
            .ok_or_else(|| anyhow::anyhow!("audit SQLite file unavailable"))?;
        let mut moved: std::ffi::c_int = 1;
        let code = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                self.db.handle(),
                c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
                (&mut moved as *mut std::ffi::c_int).cast(),
            )
        };
        ensure!(
            code == rusqlite::ffi::SQLITE_OK
                && moved == 0
                && (identity.device, identity.inode) == self.identity,
            "audit SQLite file changed"
        );
        Ok(())
    }
    #[cfg(windows)]
    fn connection_current(&self) -> Result<()> {
        use std::{
            mem::ManuallyDrop,
            os::windows::io::{FromRawHandle, RawHandle},
        };
        let mut handle: RawHandle = std::ptr::null_mut();
        let code = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                self.db.handle(),
                c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_WIN32_GET_HANDLE,
                (&mut handle as *mut RawHandle).cast(),
            )
        };
        ensure!(
            code == rusqlite::ffi::SQLITE_OK && !handle.is_null(),
            "audit SQLite handle unavailable"
        );
        // SQLite retains ownership; this temporary read-only metadata view
        // must never close or adopt its original handle, including on error.
        let borrowed = ManuallyDrop::new(unsafe { File::from_raw_handle(handle) });
        ensure!(
            key(&borrowed)? == self.identity,
            "audit SQLite file changed"
        );
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    fn connection_current(&self) -> Result<()> {
        anyhow::bail!("audit file identity unavailable")
    }
}

pub(super) struct Reader {
    root: PathBuf,
    configuration: String,
    epoch: String,
    journal: Option<Journal>,
}
impl Reader {
    pub fn new(config: &Config, location: &Location, epoch: &str) -> Self {
        Self {
            root: config.state.join("approval-audit"),
            configuration: location.configuration.clone(),
            epoch: epoch.into(),
            journal: None,
        }
    }
    pub fn request(&mut self, value: &Value) -> Value {
        let id = value["request_id"].as_str().unwrap_or("");
        let refused = |code| {
            json!({"abi":wire::ABI,"request_id":id,
            "configuration":self.configuration,"service_epoch":self.epoch,
            "accepted":false,"read_only":true,"code":code})
        };
        let Ok(request) = serde_json::from_value::<Request>(value.clone()) else {
            return refused("invalid_approval_audit_request");
        };
        if !request.valid() {
            return refused("invalid_approval_audit_request");
        }
        if request.configuration() != self.configuration || request.epoch() != self.epoch {
            return refused("approval_audit_scope_changed");
        }
        match self.read(&request) {
            Ok(value) => value,
            Err(_) => json!({"abi":wire::ABI,"request_id":id,
                "configuration":self.configuration,"service_epoch":self.epoch,
                "accepted":false,"read_only":true,"code":"approval_audit_unavailable"}),
        }
    }
    fn read(&mut self, request: &Request) -> Result<Value> {
        if self
            .journal
            .as_ref()
            .is_some_and(|j| j.check(&self.root).is_err())
        {
            self.journal = None;
        }
        if self.journal.is_none() {
            ensure!(request.journal().is_none(), "audit journal changed");
            self.journal = Journal::open(&self.root)?;
        }
        let (journal, result) = match self.journal.as_ref() {
            None => (
                Value::Null,
                json!({"entries":[],"next":null,"previous":null,"through":0}),
            ),
            Some(journal) => {
                ensure!(
                    request.journal().is_none_or(|id| id == journal.token),
                    "audit journal changed"
                );
                let result = match request {
                    Request::List {
                        host,
                        session,
                        before,
                        after,
                        through,
                        ..
                    } => list(&journal.db, host, session, *before, *after, *through)?,
                    Request::Detail { id, .. } => detail(&journal.db, *id)?,
                };
                journal.check(&self.root)?;
                (json!(journal.token), result)
            }
        };
        let value = json!({"abi":wire::ABI,"request_id":request.id(),"configuration":self.configuration,
            "service_epoch":self.epoch,"accepted":true,"read_only":true,"journal":journal,"result":result});
        ensure!(
            serde_json::to_vec(&value)?.len() <= wire::REPLY_BYTES,
            "audit reply too large"
        );
        Ok(value)
    }
}

fn list(
    db: &Connection,
    host: &str,
    session: &str,
    before: Option<i64>,
    after: Option<i64>,
    through: Option<i64>,
) -> Result<Value> {
    let through = match through {
        Some(n) => n,
        None => db.query_row("SELECT COALESCE(MAX(id),0) FROM attempts", [], |r| r.get(0))?,
    };
    let reverse = after.is_some();
    let cursor = after.or(before).unwrap_or(i64::MAX);
    let (index, filter, values): (&str, &str, Vec<&dyn ToSql>) =
        match (host.is_empty(), session.is_empty()) {
            (true, true) => ("", "", vec![&cursor, &through]),
            (true, false) => (
                " INDEXED BY attempts_session",
                " AND session=?3",
                vec![&cursor, &through, &session],
            ),
            (false, true) => (
                " INDEXED BY attempts_host",
                " AND host=?3",
                vec![&cursor, &through, &host],
            ),
            (false, false) => (
                " INDEXED BY attempts_host_session",
                " AND host=?3 AND session=?4",
                vec![&cursor, &through, &host, &session],
            ),
        };
    let operator = if reverse { ">" } else { "<" };
    let order = if reverse { "ASC" } else { "DESC" };
    let bounded = |column: &str, bytes: usize| {
        format!("CASE WHEN length(CAST({column} AS BLOB))<={bytes} THEN {column} END")
    };
    let columns = [
        ("host", 256),
        ("session", 1024),
        ("provider", 64),
        ("prompt", 1280),
        ("status", 32),
    ]
    .into_iter()
    .map(|(name, bytes)| bounded(name, bytes))
    .collect::<Vec<_>>()
    .join(",");
    let mut sql = db.prepare(&format!("SELECT id,at,{columns} FROM attempts{index} WHERE id{operator}?1 AND id<=?2{filter} ORDER BY id {order} LIMIT {}",wire::PAGE+1))?;
    let mut entries = sql.query_map(values.as_slice(),|r| Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,
        "host":r.get::<_,String>(2)?,"session":r.get::<_,String>(3)?,"provider":r.get::<_,String>(4)?,
        "prompt":r.get::<_,String>(5)?,"status":r.get::<_,String>(6)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = entries.len() > wire::PAGE;
    entries.truncate(wire::PAGE);
    if reverse {
        entries.reverse();
    }
    let next = if more || reverse {
        entries
            .last()
            .map(|v| v["id"].clone())
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let previous = if (!reverse && before.is_some()) || (reverse && more) {
        entries
            .first()
            .map(|v| v["id"].clone())
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    Ok(json!({"entries":entries,"next":next,"previous":previous,"through":through}))
}
fn detail(db: &Connection, id: i64) -> Result<Value> {
    let row: Option<(String, String, String)> = db
        .query_row(
            "SELECT CASE WHEN length(CAST(question AS BLOB))<=196608 THEN question END,
          CASE WHEN length(CAST(status AS BLOB))<=32 THEN status END,
          CASE WHEN length(CAST(detail AS BLOB))<=8192 THEN detail END FROM attempts WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (question, status, detail) =
        row.ok_or_else(|| anyhow::anyhow!("audit entry unavailable"))?;
    let saved: Value = serde_json::from_str(&question)?;
    ensure!(
        saved["question"].is_object() && saved["answers"].is_array(),
        "invalid saved approval"
    );
    Ok(
        json!({"id":id,"question":saved["question"],"answers":saved["answers"],"status":status,"detail":detail}),
    )
}

#[cfg(test)]
mod tests;
