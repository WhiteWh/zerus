//! `--check [--json]`: a read-only report for cutover and support. It creates
//! and modifies nothing. The configuration is read and validated, the state
//! directory and its files are inspected with `lstat`, the lock is probed
//! with the shared `flock` that `hgs status` uses (only when the file exists),
//! and the journal is read in one short read-only transaction, never with
//! `integrity_check` or `quick_check`. A hot rollback journal is reported, not
//! recovered. A running connector is never disturbed: the read takes only a
//! shared SQLite lock for milliseconds, well inside its 5 s busy timeout.
use crate::config::{self, Environment, Finding};
use crate::error::{FOREIGN_BINDING, PRIVATE_DIRECTORY};
use crate::fsguard::{self, LockState};
use crate::journal::{ADDED_COLUMNS, INDEXES, JOURNAL, LOCK, SIDECARS};
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

/// SQLITE_READONLY_ROLLBACK: a hot journal that a read-only connection must
/// not roll back.
const READONLY_ROLLBACK: i32 = 776;

/// Builds the report. `ok` is true when the connector would accept this
/// configuration and journal; a held lock only means one is running.
pub fn report(
    path: &Path,
    allow_insecure_localhost: bool,
    environment: &Environment,
    euid: u32,
) -> Value {
    let mut ok = true;
    let mut configuration = Map::new();
    configuration.insert("path".into(), json!(path.display().to_string()));
    let mut findings: Vec<Finding> = Vec::new();
    let validated = config::read_file(path, euid).and_then(|(text, file)| {
        findings.extend(file);
        let value = config::parse(&text)?;
        findings.extend(config::findings(&value, allow_insecure_localhost));
        let mut validated = config::validate(&value, allow_insecure_localhost, environment)?;
        config::bind_workspace(&mut validated, path)?;
        Ok(validated)
    });
    let refusal = findings
        .iter()
        .find(|finding| finding.refusal)
        .map(|finding| finding.message);
    let error = match &validated {
        Err(error) => Some(error.message().to_owned()),
        Ok(_) => refusal.map(str::to_owned),
    };
    ok &= error.is_none();
    configuration.insert("valid".into(), json!(error.is_none()));
    configuration.insert("error".into(), json!(error));
    let listed: Vec<Value> = findings
        .iter()
        .map(|finding| {
            json!({"divergence": finding.divergence.to_string(),
                   "level": if finding.refusal { "refusal" } else { "warning" },
                   "message": finding.message})
        })
        .collect();
    configuration.insert("findings".into(), Value::Array(listed));
    let mut report = Map::new();
    report.insert(
        "capabilities".into(),
        json!({"workspace_stop": crate::workspace_lifecycle::STOP_PREFLIGHT_ABI}),
    );
    if let Ok(config) = &validated {
        if config.encrypted_workspace.is_some() {
            configuration.insert("encrypted_workspace".into(), json!(true));
        }
        for (key, value) in [
            ("server_url", json!(config.server_url)),
            ("identity_url", json!(config.identity_url)),
            ("hgs_path", json!(config.hgs_path)),
            // Whole seconds read as the file spells them.
            (
                "poll_interval",
                if config.poll_interval.fract() == 0.0 && config.poll_interval < 1e15 {
                    json!(config.poll_interval as i64)
                } else {
                    json!(config.poll_interval)
                },
            ),
            ("state_dir", json!(config.state_dir.display().to_string())),
        ] {
            configuration.insert(key.into(), value);
        }
        let state = state_dir(&config.state_dir, &config.binding);
        ok &= state["ok"] == true;
        report.insert("state".into(), state);
    }
    report.insert("configuration".into(), Value::Object(configuration));
    report.insert("ok".into(), json!(ok));
    Value::Object(report)
}

/// The state directory, its files, the lock and the journal.
fn state_dir(directory: &Path, binding: &str) -> Value {
    let mut state = Map::new();
    let directory_status = match directory.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "absent",
        _ if fsguard::check_private_directory(directory).is_ok() => "private",
        _ => "refused",
    };
    state.insert("directory".into(), json!(directory_status));
    match directory_status {
        "absent" => {
            // The connector creates it and binds a new journal on its first start.
            state.insert(
                "journal".into(),
                json!({"status": "absent", "binding": "unbound"}),
            );
            state.insert("lock".into(), json!(LockState::Absent.name()));
            state.insert("ok".into(), json!(true));
            return Value::Object(state);
        }
        "refused" => {
            state.insert("error".into(), json!(PRIVATE_DIRECTORY));
            state.insert("ok".into(), json!(false));
            return Value::Object(state);
        }
        _ => {}
    }
    let mut ok = true;
    let mut files = Map::new();
    let mut inspect = |name: &str| {
        let verdict = match fsguard::inspect_private_file(&directory.join(name)) {
            Ok(None) => "absent",
            Ok(Some(_)) => "private",
            Err(error) => error,
        };
        ok &= matches!(verdict, "absent" | "private");
        files.insert(name.into(), json!(verdict));
        verdict
    };
    let lock_file = inspect(LOCK);
    let journal_file = inspect(JOURNAL);
    for suffix in SIDECARS {
        inspect(&format!("{JOURNAL}{suffix}"));
    }
    state.insert("files".into(), Value::Object(files));
    let lock = if lock_file == "private" {
        fsguard::probe_lock(&directory.join(LOCK))
    } else {
        LockState::Absent
    };
    state.insert(
        "lock".into(),
        json!(if lock_file == "absent" || lock_file == "private" {
            lock.name()
        } else {
            "refused"
        }),
    );
    let journal = match journal_file {
        "absent" => json!({"status": "absent", "binding": "unbound"}),
        "private" => journal(&directory.join(JOURNAL), binding, lock),
        _ => json!({"status": "refused"}),
    };
    ok &= journal["ok"] != false;
    state.insert("journal".into(), journal);
    state.insert("ok".into(), json!(ok));
    Value::Object(state)
}

/// Reads the journal in one short read-only transaction.
fn journal(path: &Path, binding: &str, lock: LockState) -> Value {
    let mut header = [0u8; 100];
    #[cfg(unix)]
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path);
    #[cfg(windows)]
    let opened = zerus_windows_private::open_file(path, false, false);
    let read = opened.and_then(|mut file| file.read(&mut header));
    match read {
        // The connector pre-creates an empty file before SQLite writes to it.
        Ok(0) => return json!({"status": "empty", "binding": "unbound"}),
        Ok(n) if n < 100 || !header.starts_with(b"SQLite format 3\0") => {
            return json!({"status": "unreadable", "error": "journal is not a SQLite database", "ok": false});
        }
        // A WAL database would make even a reader create -shm and -wal files.
        Ok(_) if header[18] == 2 || header[19] == 2 => {
            return json!({"status": "unreadable", "error": "journal is in WAL mode, which the connector never sets",
                          "ok": false});
        }
        Ok(_) => {}
        Err(_) => {
            return json!({"status": "unreadable", "error": "journal could not be read", "ok": false})
        }
    }
    let code = |error: &rusqlite::Error| match error {
        rusqlite::Error::SqliteFailure(failure, _) => Some((failure.code, failure.extended_code)),
        _ => None,
    };
    // A writer that takes its reserved lock just after this reader looked for
    // one can make its live rollback journal look hot for a moment. SQLite
    // reports that to a read-only reader at once, so look again before
    // calling a journal hot; a real one stays.
    let mut outcome = read_journal(path);
    for _ in 0..20 {
        match &outcome {
            Err(error)
                if code(error).is_some_and(|(_, extended)| extended == READONLY_ROLLBACK) =>
            {
                std::thread::sleep(Duration::from_millis(25));
                outcome = read_journal(path);
            }
            _ => break,
        }
    }
    match outcome {
        Ok(contents) => contents.describe(binding),
        Err(error) => {
            match code(&error) {
                // A running connector rolls a hot journal back at its next read.
                Some((_, READONLY_ROLLBACK)) if lock != LockState::Held => json!({"status": "hot",
                    "error": "a hot rollback journal awaits recovery on the next start", "ok": true}),
                Some((_, READONLY_ROLLBACK))
                | Some((ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked, _)) => {
                    json!({"status": "busy",
                    "error": "journal stayed locked by a writer; run the check again", "ok": false})
                }
                _ => {
                    json!({"status": "unreadable", "error": "journal could not be read", "ok": false})
                }
            }
        }
    }
}

struct Contents {
    objects: Vec<(String, String)>,
    binding: Option<String>,
    columns: BTreeSet<String>,
    counts: Vec<(String, i64, i64)>,
    user_version: i64,
    journal_mode: String,
}

fn read_journal(path: &Path) -> rusqlite::Result<Contents> {
    // As in the journal: a symlinked parent directory is fine, and the file
    // itself passed the lstat checks.
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let db = Connection::open_with_flags(path, flags)?;
    db.busy_timeout(Duration::from_secs(5))?;
    let tx = db.unchecked_transaction()?;
    let objects: Vec<(String, String)> = tx
        .prepare("SELECT type, name FROM sqlite_master ORDER BY rowid")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let has = |name: &str| {
        objects
            .iter()
            .any(|(kind, object)| kind == "table" && object == name)
    };
    let binding = if has("metadata") {
        tx.query_row(
            "SELECT value FROM metadata WHERE key='relay_binding'",
            [],
            |row| row.get(0),
        )
        .optional()?
    } else {
        None
    };
    let (columns, counts) = if has("requests") {
        let columns = crate::journal::columns(&tx)?.into_iter().collect();
        let counts = tx
            .prepare("SELECT state, delivered, count(*) FROM requests GROUP BY state, delivered")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        (columns, counts)
    } else {
        (BTreeSet::new(), Vec::new())
    };
    let user_version = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let journal_mode = tx.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    tx.commit()?;
    Ok(Contents {
        objects,
        binding,
        columns,
        counts,
        user_version,
        journal_mode,
    })
}

impl Contents {
    fn describe(&self, binding: &str) -> Value {
        let tables: BTreeSet<&str> = self
            .objects
            .iter()
            .filter(|(kind, _)| kind == "table")
            .map(|(_, name)| name.as_str())
            .collect();
        let indexes: BTreeSet<&str> = self
            .objects
            .iter()
            .filter(|(kind, _)| kind == "index")
            .map(|(_, name)| name.as_str())
            .collect();
        let mut pending = Vec::new();
        if !tables.contains("metadata") {
            pending.push("create table metadata".to_owned());
        }
        if !tables.contains("requests") {
            pending.push("create table requests".to_owned());
        } else {
            for (name, _) in ADDED_COLUMNS {
                if !self.columns.contains(name) {
                    pending.push(format!("add column {name}"));
                }
            }
        }
        for (name, _) in INDEXES {
            if !indexes.contains(name) {
                pending.push(format!("create index {name}"));
            }
        }
        let known: BTreeSet<&str> = [
            "metadata",
            "requests",
            "sqlite_autoindex_metadata_1",
            "sqlite_autoindex_requests_1",
        ]
        .into_iter()
        .chain(INDEXES.iter().map(|(name, _)| *name))
        .collect();
        let unexpected: Vec<&str> = self
            .objects
            .iter()
            .map(|(_, name)| name.as_str())
            .filter(|name| !known.contains(name))
            .collect();
        let schema = if !tables.contains("requests") {
            "uninitialized"
        } else if pending.is_empty() {
            "current"
        } else {
            "legacy"
        };
        let matched = match &self.binding {
            None => "unbound",
            Some(stored) if stored == binding => "matches",
            Some(_) => "differs",
        };
        let mut states: BTreeMap<&str, Map<String, Value>> = BTreeMap::new();
        let (mut total, mut running, mut undelivered) = (0, 0, 0);
        for (state, delivered, count) in &self.counts {
            let key = if *delivered == 0 {
                "undelivered"
            } else {
                "delivered"
            };
            let entry = states.entry(state.as_str()).or_default();
            let previous = entry.get(key).and_then(Value::as_i64).unwrap_or(0);
            entry.insert(key.into(), json!(previous + count));
            total += count;
            running += if state == "running" { *count } else { 0 };
            undelivered += if *delivered == 0 { *count } else { 0 };
        }
        let states: Map<String, Value> = states
            .into_iter()
            .map(|(state, counts)| (state.to_owned(), Value::Object(counts)))
            .collect();
        let ok = matched != "differs"
            && unexpected.is_empty()
            && self.user_version == 0
            && self.journal_mode == "delete";
        let mut result = json!({
            "status": "readable",
            "binding": matched,
            "schema": schema,
            "pending_migration": pending,
            "unexpected_objects": unexpected,
            "user_version": self.user_version,
            "journal_mode": self.journal_mode,
            "rows": {"total": total, "running": running, "undelivered": undelivered, "states": states},
            "ok": ok,
        });
        if matched == "differs" {
            result["error"] = json!(FOREIGN_BINDING);
        }
        result
    }
}

/// Renders the report for a terminal.
pub fn text(report: &Value) -> String {
    let mut out = String::new();
    let mut line = |indent: usize, text: String| {
        out.push_str(&" ".repeat(indent));
        out.push_str(&text);
        out.push('\n');
    };
    let configuration = &report["configuration"];
    line(
        0,
        format!(
            "configuration: {}",
            configuration["path"].as_str().unwrap_or_default()
        ),
    );
    match configuration["error"].as_str() {
        Some(error) => line(2, format!("refused: {error}")),
        None => line(2, "valid".into()),
    }
    for finding in configuration["findings"].as_array().into_iter().flatten() {
        line(
            2,
            format!(
                "{} (divergence {}): {}",
                finding["level"].as_str().unwrap_or_default(),
                finding["divergence"].as_str().unwrap_or_default(),
                finding["message"].as_str().unwrap_or_default()
            ),
        );
    }
    for key in [
        "server_url",
        "identity_url",
        "hgs_path",
        "poll_interval",
        "state_dir",
    ] {
        if let Some(value) = configuration.get(key) {
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            line(2, format!("{key}: {value}"));
        }
    }
    if let Some(state) = report.get("state") {
        line(
            0,
            format!(
                "state directory: {}",
                state["directory"].as_str().unwrap_or_default()
            ),
        );
        if let Some(error) = state["error"].as_str() {
            line(2, format!("refused: {error}"));
        }
        for (name, verdict) in state["files"].as_object().into_iter().flatten() {
            line(
                2,
                format!("{name}: {}", verdict.as_str().unwrap_or_default()),
            );
        }
        if let Some(lock) = state["lock"].as_str() {
            let meaning = match lock {
                "held" => " (a connector is running)",
                "free" => " (no connector is running)",
                _ => "",
            };
            line(0, format!("lock: {lock}{meaning}"));
        }
        let journal = &state["journal"];
        if journal.is_object() {
            line(
                0,
                format!(
                    "journal: {}",
                    journal["status"].as_str().unwrap_or_default()
                ),
            );
            if let Some(error) = journal["error"].as_str() {
                line(2, error.to_owned());
            }
            for key in ["binding", "schema", "journal_mode", "user_version"] {
                if let Some(value) = journal.get(key) {
                    line(
                        2,
                        format!(
                            "{key}: {}",
                            value
                                .as_str()
                                .map_or_else(|| value.to_string(), str::to_owned)
                        ),
                    );
                }
            }
            for step in journal["pending_migration"]
                .as_array()
                .into_iter()
                .flatten()
            {
                line(
                    2,
                    format!("next start: {}", step.as_str().unwrap_or_default()),
                );
            }
            for name in journal["unexpected_objects"]
                .as_array()
                .into_iter()
                .flatten()
            {
                line(
                    2,
                    format!("unexpected object: {}", name.as_str().unwrap_or_default()),
                );
            }
            let rows = &journal["rows"];
            if rows.is_object() {
                line(
                    2,
                    format!(
                        "rows: {} (running {}, undelivered {})",
                        rows["total"], rows["running"], rows["undelivered"]
                    ),
                );
                for (name, counts) in rows["states"].as_object().into_iter().flatten() {
                    line(
                        4,
                        format!(
                            "{name}: {} delivered, {} undelivered",
                            counts["delivered"].as_i64().unwrap_or(0),
                            counts["undelivered"].as_i64().unwrap_or(0)
                        ),
                    );
                }
            }
        }
    }
    line(
        0,
        format!(
            "result: {}",
            if report["ok"] == true {
                "ok"
            } else {
                "refused"
            }
        ),
    );
    out
}

/// The `--check` command: prints the report and returns the exit status.
pub fn run(path: &Path, allow_insecure_localhost: bool, as_json: bool) -> i32 {
    let report = match Environment::current() {
        Ok(environment) => report(
            path,
            allow_insecure_localhost,
            &environment,
            fsguard::euid(),
        ),
        Err(_) => {
            json!({"ok": false, "configuration": {"path": path.display().to_string(), "valid": false,
                         "error": "working directory is unavailable", "findings": []}})
        }
    };
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        print!("{}", text(&report));
    }
    if report["ok"] == true {
        0
    } else {
        1
    }
}
