//! What `hgs watch` (src/watch) reads from session state: inspections and the
//! Activity journal exactly as `hgs inspect` reads them, as values instead of
//! printed output. Nothing here writes session state beyond what `hgs inspect`
//! and `hgs ls` already do, apart from the lock files of the git topic's shared
//! checks.
use super::*;
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeSet;
use std::time::Duration;

pub(crate) use super::provider_messages::{Tail as TranscriptTail, TailRead};

/// The session state folder.
pub(crate) fn state_root() -> PathBuf {
    root()
}

/// Whether `name` is a DeepSeek or OpenCode session. Their Activity comes from
/// the agent's own host and each read replaces the previous window.
pub(crate) fn native(name: &str) -> bool {
    dsh::exists(name) || opencode::exists(name)
}

/// `hgs inspect NAME --after AFTER --skip-processes`.
pub(crate) fn inspect(name: &str, after: i64) -> Result<Value> {
    local_sessions()?;
    if opencode::exists(name) {
        return opencode::inspect(name, None);
    }
    if dsh::exists(name) {
        return dsh::inspect(name, None);
    }
    journal::inspection(name, after, None, false)
}

/// What `hgs inspect NAME` reports about a session's processes: `processes`,
/// `process_work` and the `run_id` they belong to.
pub(crate) fn processes(name: &str) -> Result<Value> {
    local_sessions()?;
    let inspection = if opencode::exists(name) {
        opencode::inspect(name, None)?
    } else if dsh::exists(name) {
        dsh::inspect_processes(name)?
    } else {
        // No journal row is newer than i64::MAX: the summary without events.
        journal::inspection(name, i64::MAX, None, true)?
    };
    Ok(json!({"run_id": inspection["run_id"], "processes": inspection["processes"], "process_work": inspection["process_work"]}))
}

/// A read-only connection to the Activity journal, once it exists. It never
/// creates the file or its schema, and holds no lock between statements.
pub(crate) fn journal() -> Option<Connection> {
    local_sessions().ok()?;
    let path = root().join("events.sqlite3");
    if !path.is_file() {
        return None;
    }
    let db = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    db.busy_timeout(Duration::from_millis(250)).ok()?;
    Some(db)
}

/// Changes whenever another connection commits to the journal.
pub(crate) fn data_version(db: &Connection) -> Result<i64> {
    db.query_row("PRAGMA data_version", [], |row| row.get(0))
        .map_err(|e| e.to_string())
}

/// The oldest and newest retained `seq` (0 when the journal is empty).
pub(crate) fn bounds(db: &Connection) -> Result<(i64, i64)> {
    db.query_row(
        "SELECT COALESCE(MIN(seq),0), COALESCE(MAX(seq),0) FROM events",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .map_err(|e| e.to_string())
}

/// The journal names with rows after `after`, from at most `limit` rows.
pub(crate) fn changed_names(db: &Connection, after: i64, limit: usize) -> Result<Vec<String>> {
    let mut statement = db
        .prepare("SELECT DISTINCT name FROM (SELECT name FROM events WHERE seq>?1 ORDER BY seq LIMIT ?2)")
        .map_err(|e| e.to_string())?;
    let names = statement
        .query_map(rusqlite::params![after, limit as i64], |row| row.get::<_, Option<String>>(0))
        .map_err(|e| e.to_string())?
        .filter_map(|row| row.ok().flatten())
        .collect();
    Ok(names)
}

/// The journal names whose rows after `after`, from at most `limit` rows,
/// record a finished call of a tool that can change a checkout's files, with
/// the latest such row's time. A session that ran one makes the git topic
/// check its folders again.
pub(crate) fn writing_names(db: &Connection, after: i64, limit: usize) -> Result<Vec<(String, Option<f64>)>> {
    const QUERY: &str = "SELECT name, MAX(CASE WHEN json_valid(payload) THEN json_extract(payload, '$.at') END) \
        FROM (SELECT name, payload FROM events WHERE seq>?1 ORDER BY seq LIMIT ?2) \
        WHERE CASE WHEN json_valid(payload) THEN json_extract(payload, '$.type') END IN ('PostToolUse','PostToolUseFailure') \
        AND CASE WHEN json_valid(payload) THEN json_extract(payload, '$.tool') END \
        IN ('Edit','Write','MultiEdit','NotebookEdit','apply_patch','Bash') GROUP BY name";
    let mut statement = db.prepare(QUERY).map_err(|e| e.to_string())?;
    let names = statement
        .query_map(rusqlite::params![after, limit as i64], |row| {
            Ok(row.get::<_, Option<String>>(0)?.map(|name| (name, row.get::<_, Option<f64>>(1).ok().flatten())))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|row| row.ok().flatten())
        .collect();
    Ok(names)
}

/// An exclusive lock on an auxiliary file, or None when another process
/// still holds it at `deadline`.
pub(crate) fn lock_until(path: &Path, deadline: std::time::Instant) -> Result<Option<crate::platform::FileLock>> {
    storage::lock_until(path, deadline)
}

/// `hgs git-status --path PATH --json` as a value: the same read-only checks,
/// remote cache and Git options, without a child `hgs`.
pub(crate) fn git_status(path: &Path) -> Result<Value> {
    session_operations()?;
    Ok(git_status::inspect(path))
}

/// The rows a tracked session's Activity shows: its journal name, the
/// conversations its timeline spans and, for a fork, its sources up to where
/// it began, taken from its saved binding.
pub(crate) struct Timeline {
    record: Value,
}

impl Timeline {
    /// The journal name, which a rename keeps.
    pub(crate) fn journal(&self) -> &str {
        journal::journal_name(&self.record)
    }

    pub(crate) fn run_id(&self) -> &str {
        string(&self.record, "run_id")
    }

    pub(crate) fn conversation(&self) -> &str {
        string(&self.record, "conversation_id")
    }

    /// The Codex or Claude transcript of the current conversation, followed
    /// from its end for messages between tools; None for other agents.
    pub(crate) fn transcript(&self) -> Option<TranscriptTail> {
        TranscriptTail::open(&self.record)
    }

    /// Events after `after` as `inspect` shapes them, oldest first: the first
    /// `limit`, or with `newest` the last `limit`.
    pub(crate) fn events(&self, db: &Connection, after: i64, limit: usize, newest: bool) -> Result<Vec<Value>> {
        let scope = journal::activity_scope(db, &self.record);
        let rows = journal::timeline_rows(
            db,
            self.journal(),
            self.record["conversation_id"].as_str(),
            &scope,
            after,
            limit,
            newest,
        )?;
        rows.iter().map(|row| row.event(&self.record)).collect()
    }
}

/// The timeline of a tracked session; None for a session without a saved
/// binding (DeepSeek, OpenCode and untracked sessions).
pub(crate) fn timeline(name: &str) -> Result<Option<Timeline>> {
    local_sessions()?;
    if !record_path(name).exists() {
        return Ok(None);
    }
    let record = read(name)?;
    Ok(Some(Timeline { record }))
}

/// Whether a run is in Archive. Rare: only for a catalog row that disappeared.
pub(crate) fn archived(run_id: &str) -> bool {
    if run_id.is_empty() || local_sessions().is_err() || !root().join("archives").is_dir() {
        return false;
    }
    let Ok(_guard) = lock(None) else { return false };
    let Ok(db) = archive_catalog::catalog() else { return false };
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM archive_rows WHERE run=?1)",
        [run_id],
        |row| row.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

/// Whether `name` still has a saved binding of any agent.
pub(crate) fn saved(name: &str) -> bool {
    local_sessions().is_ok() && (record_path(name).exists() || native(name))
}

// ---- Rows between listings (zerus-604d.6) ------------------------------------
//
// A whole `ls --json --local --no-archives` summarizes every session. Between
// such listings `hgs watch` derives again only the live tracked rows whose
// inputs changed, through the same steps (`live_row`, `finish_rows`) and under
// the same lock, and probes what no file reports.

/// What the terminal showed a derivation: its listed sessions with their IDs,
/// attached clients, creation and launch, and every pane with its run marker.
/// A session that starts, ends, is renamed, attached or detached changes it.
fn structure(sessions: &[Value], ids: &BTreeMap<String, String>, snapshot: &Snapshot, names: &BTreeMap<String, String>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    json!([sessions, ids, snapshot, names]).to_string().hash(&mut hasher);
    hasher.finish()
}

/// The terminal's rows and session IDs, as a listing starts from them.
fn terminal() -> Result<(Vec<Value>, BTreeMap<String, String>)> {
    let listing = backend::current().listing()?;
    Ok((listed_rows(&listing), listed_ids(&listing)))
}

/// The terminal's structure now. A listing that starts later sees this one or
/// a newer one, so taking it first never hides a change.
pub(crate) fn structure_now() -> Result<u64> {
    local_sessions()?;
    let (sessions, ids) = terminal()?;
    let (snapshot, names) = live_sessions()?;
    Ok(structure(&sessions, &ids, &snapshot, &names))
}

/// The rows `ls --json --local --no-archives` lists now for the live tracked
/// sessions in `wanted`, with the terminal structure they saw. None when only
/// a whole listing can tell: the panes no longer show exactly the listed
/// sessions under their listed names (one started, ended or was renamed), or
/// a wanted session does not run its saved binding in its pane.
pub(crate) fn rows(wanted: &BTreeSet<String>) -> Result<Option<(Vec<Value>, u64)>> {
    local_sessions()?;
    if !root().exists() {
        return Ok(None);
    }
    let (sessions, ids) = terminal()?;
    // As in a listing: the panes and the bindings are read under the lock, so
    // a rename is seen before it began or after it finished (zerus-gd02).
    let (snapshot, mut names, records) = {
        let _guard = lock(None)?;
        let (snapshot, names) = live_sessions()?;
        let mut records = Vec::with_capacity(wanted.len());
        for name in wanted {
            match read(name) {
                Ok(record) if record["rename_shadow"] != true => records.push(record),
                _ => return Ok(None),
            }
        }
        (snapshot, names, records)
    };
    let shape = structure(&sessions, &ids, &snapshot, &names);
    let consistent = sessions.len() == snapshot.len()
        && sessions.iter().all(|session| {
            let listed = string(session, "name");
            match ids.get(listed).and_then(|id| names.get(id)) {
                Some(name) => name == listed,
                None => snapshot.contains_key(listed),
            }
        });
    if !consistent {
        return Ok(None);
    }
    let marks = read_marks::load();
    let preferences = notifications::load();
    let keep_warm_settings = keep_warm::load();
    let question_routing = question_routing::load();
    let shared = Shared { marks: &marks, preferences: preferences.as_ref(), keep_warm: keep_warm_settings.as_ref(), question_routing: question_routing.as_ref(), budget: read_marks::question_budget() };
    let mut process_work = processes::WorkSample::default();
    let mut rows = Vec::with_capacity(records.len());
    for record in records {
        // Its row is its own terminal's (P3b), listed under its label.
        let terminal = terminal_name(&record);
        let Some(session) = sessions.iter().find(|session| string(session, "name") == terminal) else {
            return Ok(None);
        };
        if !matches(&record, panes_of(&snapshot, &record)) {
            return Ok(None);
        }
        let mut session = session.clone();
        label_row(&mut session, &record);
        workspace::relabel(&mut names, terminal, string(&record, "name"));
        session
            .as_object_mut()
            .ok_or("invalid session")?
            .extend(telemetry::unavailable().as_object().unwrap().clone());
        live_row(&mut session, record, &shared, &mut process_work)?;
        rows.push(session);
    }
    finish_rows(&mut rows, &marks, &names);
    Ok(Some((rows, shape)))
}

/// The DeepSeek and OpenCode rows of `ls --json --local --no-archives`. Their
/// hosts answer them, so no file shows their changes.
pub(crate) fn native_rows() -> Result<Vec<Value>> {
    local_sessions()?;
    let mut rows = dsh::snapshots()?;
    rows.extend(opencode::snapshots()?);
    if !rows.is_empty() {
        let (_, names) = live_sessions()?;
        finish_rows(&mut rows, &read_marks::load(), &names);
    }
    Ok(rows)
}

/// What a probe between listings found.
pub(crate) struct Probe {
    /// The terminal's structure (`structure_now`).
    pub(crate) structure: u64,
    /// Live tracked rows to derive again.
    pub(crate) stale: Vec<String>,
}

/// `process_work` without its sampling time.
fn work(mut value: Value) -> Value {
    if let Some(fields) = value.as_object_mut() {
        fields.remove("sampled_at");
    }
    value
}

/// Compares the listed `rows` with what no file reports: the terminal's
/// structure and, for each live tracked session, its agent process and the
/// work that process owns, from one process sample. A session that has not
/// reported a hook event yet is always derived again: startup choosers have
/// no hook. It takes no lock; what it finds only asks for rows that do.
pub(crate) fn probe(rows: &[&Value]) -> Result<Probe> {
    local_sessions()?;
    let (sessions, ids) = terminal()?;
    let (snapshot, names) = live_sessions()?;
    let mut sample = processes::WorkSample::default();
    let mut stale = Vec::new();
    for row in rows {
        let name = string(row, "name");
        if row["tracked"] != true || native(name) {
            continue;
        }
        // A row lists its label; its liveness is its own terminal's (P3b).
        let Ok(record) = read(name) else {
            if snapshot.contains_key(name) {
                stale.push(name.to_owned());
            }
            continue;
        };
        if !terminal_live(&snapshot, &record).unwrap_or(true) {
            continue;
        }
        let quiet = string(&record, "conversation_id").is_empty()
            || record["last_event_at"].as_f64().unwrap_or(0.) == 0.;
        let idle = row["activity"] == "idle" && row["phase"] == "idle";
        // As the summary decides `terminal_supported` of a live pane.
        let terminal = string(&record, "archive_id").is_empty() && process_alive(&record);
        if quiet
            || !matches(&record, panes_of(&snapshot, &record))
            || row["terminal_supported"] != json!(terminal)
            || work(row["process_work"].clone()) != work(sample.summary(&record, idle))
        {
            stale.push(name.to_owned());
        }
    }
    Ok(Probe { structure: structure(&sessions, &ids, &snapshot, &names), stale })
}

/// The session a binding file holds and the journal name its Activity rows
/// carry; None for a rename's shadow or a file that is no binding.
pub(crate) fn binding_identity(path: &Path) -> Option<(String, String)> {
    let value: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let name = string(&value, "name");
    if value["rename_shadow"] == true || name.is_empty() {
        return None;
    }
    Some((name.to_owned(), journal::journal_name(&value).to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_names_carry_their_latest_file_changing_call() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE events (seq INTEGER PRIMARY KEY, name TEXT, conversation TEXT, payload TEXT);").unwrap();
        let rows = [
            ("codex/a", json!({"type":"PostToolUse","tool":"Bash","at":10.5})),
            ("codex/a", json!({"type":"PostToolUse","tool":"Edit","at":12.25})),
            ("codex/a", json!({"type":"PostToolUse","tool":"Read","at":20.0})),
            ("codex/b", json!({"type":"PostToolUseFailure","tool":"apply_patch"})),
            ("codex/c", json!({"type":"Stop","at":30.0})),
        ];
        for (name, payload) in rows {
            db.execute("INSERT INTO events(name,conversation,payload) VALUES(?,'c',?)", rusqlite::params![name, payload.to_string()]).unwrap();
        }
        db.execute("INSERT INTO events(name,conversation,payload) VALUES('codex/d','c','not json')", []).unwrap();
        let mut names = writing_names(&db, 0, 100).unwrap();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(names, vec![("codex/a".to_owned(), Some(12.25)), ("codex/b".to_owned(), None)]);
        assert_eq!(writing_names(&db, 2, 100).unwrap(), vec![("codex/b".to_owned(), None)]);
    }
}
