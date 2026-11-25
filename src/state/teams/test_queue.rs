//! U62, #717: the team's test queue and its local results database. A request
//! names a provider's suite (command, folder, targets, recorded time and
//! resource class); the scheduler runs what the machine has room for, forced
//! and short suites first, never two heavy ones at once, and long suites only
//! at night. Only `run_due` writes results, each with its commit, input hash,
//! status, duration, a bounded excerpt and who asked.
use crate::test_discipline::config::{self, Resources};
use super::templates::{last_chars, run_tree, Outcome};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub(crate) const RESULT_LIMIT: i64 = 5000;
pub(crate) const EXCERPT_BYTES: usize = 4000;
pub(crate) const HEAVY_CPU: u32 = 4;
pub(crate) const HEAVY_RAM_MB: u32 = 4096;

/// A suite's declared class: the config's `[resources.NAME]`.
pub(crate) type Classes = BTreeMap<String, Resources>;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Load {
    pub cpu_free: u32,
    pub ram_free_mb: u32,
    pub gpu_free: bool,
    pub at_night: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Request {
    pub id: String,
    pub team: String,
    pub suite: String,
    pub task: Option<u32>,
    pub targets: Vec<String>,
    pub commit: String,
    pub input_hash: String,
    pub asked_by: String,
    pub forced: bool,
    pub at: f64,
    pub seconds: f64,
    pub budget_minutes: f64,
    pub resources: String,
    pub command: String,
    /// U62 (#732): the suite's platform classes (its provider's `platforms`);
    /// empty runs here. Another class goes only to a test runner of it.
    pub platforms: Vec<String>,
    /// Task #281: a performance measurement; its run checks the machine first
    /// and records the load, and waits for a quiet window (bounded).
    pub performance: bool,
    /// Task #281: a comparison group; its performance requests alternate the
    /// binaries in rounds, never one binary's rounds back to back.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub compare: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct Record {
    pub id: String,
    pub team: String,
    pub suite: String,
    pub commit: String,
    pub input_hash: String,
    pub status: String,
    pub duration_ms: u64,
    pub excerpt: String,
    pub asked_by: String,
    /// When the run started.
    pub at: f64,
    pub affected: Vec<String>,
    /// #719: the task that asked for it, so its drawer lists its runs.
    pub task: Option<u32>,
    /// #718: the measured record a reused result stands for; empty when it ran.
    pub reused_from: String,
    /// #718: reused, slower, over_time_budget, retried, crash.
    pub flags: Vec<String>,
    /// #718: a reused result is due for re-measuring.
    pub due: bool,
    /// #718: the suite's recorded time when it ran, to say how much it grew.
    pub recorded_ms: u64,
    /// #732: the test runner (a Machine's alias) it ran on; empty is here.
    pub runner: String,
    /// Task 88: one result per test from the provider's report, if it names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<super::run_results::Report>,
    /// Task #281: a performance measurement; its result carries the load.
    pub performance: bool,
    /// Task #281: the machine's load before and during the run, whether it
    /// waited, and whether it ran marked `loaded`; none for a correctness test.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load: Option<serde_json::Value>,
}

/// #719: a request still in the queue: waiting, or running since `started`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Pending {
    #[serde(flatten)]
    pub request: Request,
    pub state: String,
    pub started: Option<f64>,
    /// #732: why a queued request does not run yet; empty when it just waits.
    pub reason: String,
}

/// #719: at most this many queued and running requests in one read.
pub(crate) const PENDING_LIMIT: i64 = 100;

const REQUEST_COLUMNS: &str = "id,team,suite,task,targets,commit_id,input_hash,asked_by,forced,at,seconds,budget_minutes,resources,command,platforms,performance,compare";
// #718: reused_from, flags, due and recorded_ms after #719's task.
const RECORD_COLUMNS: &str = "id,team,suite,commit_id,input_hash,status,duration_ms,excerpt,asked_by,at,affected,task,reused_from,flags,due,recorded_ms,runner,report,performance,load";

fn add_column(db: &Connection, table: &str, column: &str, kind: &str) -> rusqlite::Result<()> {
    let present = db.prepare(&format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name=?1"))?.exists([column])?;
    if !present {
        db.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))?;
    }
    Ok(())
}


pub(crate) fn database_path() -> PathBuf {
    super::store::path().with_file_name("tests.sqlite3")
}

#[derive(Debug)]
pub(crate) struct TestStore {
    db: Connection,
}

impl TestStore {
    pub(crate) fn open(read_only: bool) -> anyhow::Result<Self> {
        Self::open_at(&database_path(), read_only)
    }

    pub(crate) fn open_at(path: &Path, read_only: bool) -> anyhow::Result<Self> {
        if !read_only {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let flags = if read_only { rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY } else { rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE };
        let db = Connection::open_with_flags(path, flags)?;
        db.busy_timeout(Duration::from_secs(5))?;
        if !read_only {
            db.execute_batch(
                "PRAGMA journal_mode=DELETE; PRAGMA max_page_count=262144;
                 CREATE TABLE IF NOT EXISTS queue(id TEXT PRIMARY KEY,team TEXT NOT NULL,suite TEXT NOT NULL,task INTEGER,
                    targets TEXT NOT NULL,commit_id TEXT NOT NULL,input_hash TEXT NOT NULL,asked_by TEXT NOT NULL,
                    forced INTEGER NOT NULL,at REAL NOT NULL,seconds REAL NOT NULL,budget_minutes REAL NOT NULL,
                    resources TEXT NOT NULL,command TEXT NOT NULL,state TEXT NOT NULL);
                 CREATE INDEX IF NOT EXISTS queue_team ON queue(team,state,at);
                 CREATE TABLE IF NOT EXISTS results(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,team TEXT NOT NULL,
                    suite TEXT NOT NULL,commit_id TEXT NOT NULL,input_hash TEXT NOT NULL,status TEXT NOT NULL,
                    duration_ms INTEGER NOT NULL,excerpt TEXT NOT NULL,asked_by TEXT NOT NULL,at REAL NOT NULL,
                    affected TEXT NOT NULL);
                 CREATE INDEX IF NOT EXISTS results_input ON results(suite,commit_id,input_hash);
                 CREATE TABLE IF NOT EXISTS flaky(seq INTEGER PRIMARY KEY AUTOINCREMENT,team TEXT NOT NULL,suite TEXT NOT NULL,
                    record TEXT NOT NULL,at REAL NOT NULL);
                 CREATE INDEX IF NOT EXISTS flaky_suite ON flaky(team,suite,at);
                 CREATE INDEX IF NOT EXISTS results_team ON results(team,at);
                 CREATE INDEX IF NOT EXISTS results_at ON results(at);",
            )?;
            // #719: a running request's start, and a result's task, in a
            // database #717 made without them.
            add_column(&db, "queue", "started", "REAL")?;
            // #732: a suite's platform classes, why it waits, and the runner of a result.
            add_column(&db, "queue", "platforms", "TEXT NOT NULL DEFAULT '[]'")?;
            add_column(&db, "queue", "reason", "TEXT NOT NULL DEFAULT ''")?;
            // Task #281: a performance run, its comparison group and its load.
            add_column(&db, "queue", "performance", "INTEGER NOT NULL DEFAULT 0")?;
            add_column(&db, "queue", "compare", "TEXT NOT NULL DEFAULT ''")?;
            add_column(&db, "results", "performance", "INTEGER NOT NULL DEFAULT 0")?;
            add_column(&db, "results", "load", "TEXT NOT NULL DEFAULT ''")?;
            add_column(&db, "results", "runner", "TEXT NOT NULL DEFAULT ''")?;
            add_column(&db, "results", "task", "INTEGER")?;
            // Task 88: one result per test, as JSON; empty for none.
            add_column(&db, "results", "report", "TEXT NOT NULL DEFAULT ''")?;
            db.execute_batch("CREATE INDEX IF NOT EXISTS results_task ON results(team,task,at);")?;
            // #718: the columns an older database lacks.
            let present: Vec<String> = db
                .prepare("SELECT name FROM pragma_table_info('results')")?
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            for (name, kind) in [("reused_from", "TEXT NOT NULL DEFAULT ''"), ("flags", "TEXT NOT NULL DEFAULT '[]'"), ("due", "INTEGER NOT NULL DEFAULT 0"), ("recorded_ms", "INTEGER NOT NULL DEFAULT 0")] {
                if !present.iter().any(|known| known == name) {
                    db.execute_batch(&format!("ALTER TABLE results ADD COLUMN {name} {kind};"))?;
                }
            }
            db.execute_batch("CREATE INDEX IF NOT EXISTS results_reuse ON results(team,at) WHERE reused_from<>'';")?;
        }
        Ok(Self { db })
    }

    pub(crate) fn enqueue(&self, request: &Request) -> anyhow::Result<()> {
        self.db.execute(
            &format!("INSERT OR REPLACE INTO queue({REQUEST_COLUMNS},state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,'queued')"),
            params![
                request.id,
                request.team,
                request.suite,
                request.task,
                serde_json::to_string(&request.targets)?,
                request.commit,
                request.input_hash,
                request.asked_by,
                request.forced as i64,
                request.at,
                request.seconds,
                request.budget_minutes,
                request.resources,
                request.command,
                serde_json::to_string(&request.platforms)?,
                request.performance as i64,
                request.compare
            ],
        )?;
        Ok(())
    }

    pub(crate) fn queued(&self, team: &str) -> anyhow::Result<Vec<Request>> {
        let mut statement = self.db.prepare(&format!(
            "SELECT {REQUEST_COLUMNS} FROM queue WHERE team=?1 AND state='queued' ORDER BY forced DESC, at ASC"
        ))?;
        let rows = statement.query_map([team], Self::request)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// #719: what the Tests view shows as the queue: the running requests
    /// first, then the waiting ones in the order the scheduler weighs them.
    pub(crate) fn pending(&self, team: &str) -> anyhow::Result<Vec<Pending>> {
        let mut statement = self.db.prepare(&format!(
            "SELECT {REQUEST_COLUMNS},state,started,reason FROM queue WHERE team=?1 AND state IN ('queued','running')
             ORDER BY state='running' DESC, forced DESC, at ASC LIMIT ?2"
        ))?;
        let rows = statement.query_map(params![team, PENDING_LIMIT], |row| {
            Ok(Pending { request: Self::request(row)?, state: row.get(17)?, started: row.get(18)?, reason: row.get(19)? })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn request(row: &rusqlite::Row<'_>) -> rusqlite::Result<Request> {
        Ok(Request {
            id: row.get(0)?,
            team: row.get(1)?,
            suite: row.get(2)?,
            task: row.get(3)?,
            targets: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
            commit: row.get(5)?,
            input_hash: row.get(6)?,
            asked_by: row.get(7)?,
            forced: row.get::<_, i64>(8)? != 0,
            at: row.get(9)?,
            seconds: row.get(10)?,
            budget_minutes: row.get(11)?,
            resources: row.get(12)?,
            command: row.get(13)?,
            platforms: serde_json::from_str(&row.get::<_, String>(14)?).unwrap_or_default(),
            performance: row.get::<_, i64>(15)? != 0,
            compare: row.get(16)?,
        })
    }

    /// #732: the request stays queued, with why it does not run (no runner of
    /// its class, or its runner unreachable).
    pub(crate) fn hold(&self, id: &str, reason: &str) -> anyhow::Result<()> {
        self.db.execute("UPDATE queue SET reason=?2 WHERE id=?1 AND state='queued'", params![id, reason])?;
        Ok(())
    }

    /// #719: the request runs now; the Tests view shows it as running.
    pub(crate) fn start(&self, id: &str, at: f64) -> anyhow::Result<()> {
        self.db.execute("UPDATE queue SET state='running', started=?2 WHERE id=?1", params![id, at])?;
        Ok(())
    }

    pub(crate) fn finish(&self, id: &str) -> anyhow::Result<()> {
        self.db.execute("UPDATE queue SET state='done' WHERE id=?1", [id])?;
        Ok(())
    }

    /// Only the runner calls this: a result names its suite, commit, input
    /// hash, status, duration, a bounded excerpt and who asked.
    pub(crate) fn write(&self, record: &Record) -> anyhow::Result<()> {
        self.db.execute(
            &format!("INSERT OR REPLACE INTO results({RECORD_COLUMNS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)"),
            params![
                record.id,
                record.team,
                record.suite,
                record.commit,
                record.input_hash,
                record.status,
                record.duration_ms as i64,
                record.excerpt,
                record.asked_by,
                record.at,
                serde_json::to_string(&record.affected)?,
                record.task,
                record.reused_from,
                serde_json::to_string(&record.flags)?,
                record.due as i64,
                record.recorded_ms as i64,
                record.runner,
                record.report.as_ref().map(serde_json::to_string).transpose()?.unwrap_or_default(),
                record.performance as i64,
                record.load.as_ref().map(serde_json::to_string).transpose()?.unwrap_or_default()
            ],
        )?;
        // Bounded: only the oldest records past the limit go.
        let count: i64 = self.db.query_row("SELECT COUNT(*) FROM results", [], |row| row.get(0))?;
        if count > RESULT_LIMIT {
            self.db.execute(
                "DELETE FROM results WHERE seq IN (SELECT seq FROM results ORDER BY seq ASC LIMIT ?1)",
                [count - RESULT_LIMIT],
            )?;
        }
        Ok(())
    }

    /// #760: the result a request produced, once the runner wrote it.
    /// Task 374: one result record by its exact id, the way a test card names
    /// it (its log and excerpt stay here, never on the card).
    pub(crate) fn record_by_id(&self, id: &str) -> anyhow::Result<Option<Record>> {
        let mut statement = self.db.prepare(&format!("SELECT {RECORD_COLUMNS} FROM results WHERE id = ?1 LIMIT 1"))?;
        Ok(statement.query_row(params![id], Self::record).optional()?)
    }

    pub(crate) fn result_of(&self, request: &str) -> anyhow::Result<Option<Record>> {
        let mut statement = self.db.prepare(&format!(
            "SELECT {RECORD_COLUMNS} FROM results WHERE id LIKE ?1 || '-%' ORDER BY seq DESC LIMIT 1"
        ))?;
        Ok(statement.query_row(params![request], Self::record).optional()?)
    }

    /// #718: the last green measured run of a suite with these input hashes,
    /// at any commit: unchanged inputs give the same result.
    pub(crate) fn last_green(&self, team: &str, suite: &str, input_hash: &str) -> anyhow::Result<Option<Record>> {
        let mut statement = self.db.prepare(&format!(
            "SELECT {RECORD_COLUMNS} FROM results WHERE team=?1 AND suite=?2 AND input_hash=?3 AND status='passed'
             AND reused_from='' ORDER BY seq DESC LIMIT 1"
        ))?;
        Ok(statement.query_row(params![team, suite, input_hash], Self::record).optional()?)
    }

    /// #718: records a reused result for `request`: from `source`, due for
    /// re-measuring.
    pub(crate) fn write_reused(&self, request: &Request, source: &Record, now: f64) -> anyhow::Result<Record> {
        let record = Record {
            id: format!("{}-reused-{}", request.id, now as i64),
            team: request.team.clone(),
            suite: request.suite.clone(),
            commit: request.commit.clone(),
            input_hash: request.input_hash.clone(),
            status: "reused".into(),
            duration_ms: 0,
            excerpt: format!("Reused from {} ({}): its inputs did not change. Due for re-measuring.", source.id, source.commit),
            asked_by: request.asked_by.clone(),
            at: now,
            affected: request.targets.clone(),
            reused_from: source.id.clone(),
            flags: vec!["reused".into()],
            due: true,
            recorded_ms: source.duration_ms,
            runner: String::new(),
            task: request.task,
            report: source.report.clone(),
            performance: request.performance,
            load: source.load.clone(),
        };
        self.write(&record)?;
        Ok(record)
    }

    /// #718: how many results of the team were reused since `since`, and when
    /// the oldest measurement they stand for ran.
    pub(crate) fn reused_since(&self, team: &str, since: f64) -> anyhow::Result<(usize, Option<f64>)> {
        let (count, oldest): (i64, Option<f64>) = self.db.query_row(
            "SELECT COUNT(*), MIN(source.at) FROM results AS reuse LEFT JOIN results AS source ON source.id=reuse.reused_from
             WHERE reuse.team=?1 AND reuse.reused_from<>'' AND reuse.at>?2",
            params![team, since],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((count as usize, oldest))
    }

    /// #718: a flaky episode: the record that passed only on its retry.
    pub(crate) fn add_flaky(&self, team: &str, suite: &str, record: &str, at: f64) -> anyhow::Result<()> {
        self.db.execute("INSERT INTO flaky(team,suite,record,at) VALUES(?1,?2,?3,?4)", params![team, suite, record, at])?;
        Ok(())
    }

    /// #718: a suite's flaky episodes after `since`, oldest first (bounded).
    pub(crate) fn flaky_history(&self, team: &str, suite: &str, since: f64) -> anyhow::Result<Vec<(String, f64)>> {
        let mut statement = self.db.prepare("SELECT record,at FROM flaky WHERE team=?1 AND suite=?2 AND at>=?3 ORDER BY at ASC LIMIT 200")?;
        let rows = statement.query_map(params![team, suite, since], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Newest first, a page before `before`; #719: only one task's runs, or
    /// only runs that started at `from` or later (the profiler's window).
    pub(crate) fn results(&self, team: Option<&str>, before: Option<f64>, limit: usize, task: Option<u32>, from: Option<f64>) -> anyhow::Result<Vec<Record>> {
        let limit = limit.clamp(1, 100) as i64;
        let mut statement = self.db.prepare(&format!(
            "SELECT {RECORD_COLUMNS} FROM results WHERE (?1 IS NULL OR team=?1) AND (?2 IS NULL OR at<?2)
             AND (?4 IS NULL OR task=?4) AND (?5 IS NULL OR at>=?5) ORDER BY at DESC, seq DESC LIMIT ?3"
        ))?;
        let rows = statement.query_map(params![team, before, limit, task, from], Self::record)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn record(row: &rusqlite::Row<'_>) -> rusqlite::Result<Record> {
        Ok(Record {
            id: row.get(0)?,
            team: row.get(1)?,
            suite: row.get(2)?,
            commit: row.get(3)?,
            input_hash: row.get(4)?,
            status: row.get(5)?,
            duration_ms: row.get::<_, i64>(6)? as u64,
            excerpt: row.get(7)?,
            asked_by: row.get(8)?,
            at: row.get(9)?,
            affected: serde_json::from_str(&row.get::<_, String>(10)?).unwrap_or_default(),
            task: row.get(11)?,
            reused_from: row.get(12)?,
            flags: serde_json::from_str(&row.get::<_, String>(13)?).unwrap_or_default(),
            due: row.get::<_, i64>(14)? != 0,
            recorded_ms: row.get::<_, i64>(15)? as u64,
            runner: row.get(16)?,
            report: row.get::<_, Option<String>>(17)?.filter(|text| !text.is_empty()).and_then(|text| serde_json::from_str(&text).ok()),
            performance: row.get::<_, i64>(18)? != 0,
            load: row.get::<_, Option<String>>(19)?.filter(|text| !text.is_empty()).and_then(|text| serde_json::from_str(&text).ok()),
        })
    }
}

fn is_heavy(class: Option<&Resources>) -> bool {
    class.is_some_and(|class| class.gpu || class.exclusive || class.cpu >= HEAVY_CPU || class.ram_mb >= HEAVY_RAM_MB)
}

fn long(request: &Request) -> bool {
    request.seconds > request.budget_minutes.max(0.0) * 60.0
}

/// U62 (#717): records the tick's load time and one notice per result (newest
/// last, at most 50); the results themselves stay in the database.
pub(crate) fn record_notices(team: &mut super::model::Team, records: &[Record], now: f64) {
    team.test_load_at = now;
    for record in records {
        team.test_notices.push(super::model::TestNotice {
            record: record.id.clone(),
            suite: record.suite.clone(),
            status: record.status.clone(),
            duration_ms: record.duration_ms,
            asked_by: record.asked_by.clone(),
            at: record.at,
            flags: record.flags.clone(),
            reused_from: record.reused_from.clone(),
            recorded_seconds: record.recorded_ms as f64 / 1000.0,
            task: record.task,
        });
    }
    let extra = team.test_notices.len().saturating_sub(50);
    team.test_notices.drain(0..extra);
}

/// U62 (#717): the machine's real free resources. The scheduler reads them at
/// most once per 30 s (see `tick`) and never when the queue is empty.
pub(crate) fn load_now() -> Load {
    let cpu_free = std::thread::available_parallelism().map(|count| count.get() as u32).unwrap_or(1);
    Load { cpu_free, ram_free_mb: free_memory_mb().unwrap_or(0), gpu_free: true, at_night: false }
}

/// Task #281: the machine's load for a performance run: busy CPU and disk,
/// and the Zerus builds running now. A perf run records it and waits for a
/// quiet machine (bounded), or runs marked `loaded` with these numbers.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct PerfLoad {
    pub cpu_percent: f64,
    pub disk_percent: f64,
    pub builds: u32,
    pub at: f64,
}

impl PerfLoad {
    /// Quiet: CPU, disk and builds all at or under the policy's limits.
    pub(crate) fn quiet(&self, policy: &config::Performance) -> bool {
        self.cpu_percent <= f64::from(policy.cpu_percent)
            && self.disk_percent <= f64::from(policy.disk_percent)
            && self.builds <= policy.builds
    }
}

/// Task #281: whether a performance run may start now. `Run(loaded)` starts
/// it; `loaded` is true when the bounded wait is over and the machine is still
/// busy, so the run is marked with the load.
pub(crate) enum PerfGate {
    Wait,
    Run(bool),
}

pub(crate) fn perf_gate(load: &PerfLoad, policy: &config::Performance, queued_at: f64, now: f64) -> PerfGate {
    if load.quiet(policy) {
        return PerfGate::Run(false);
    }
    if now - queued_at < policy.wait_seconds.max(0.0) {
        return PerfGate::Wait;
    }
    PerfGate::Run(true)
}

/// Task #281: samples the machine for a perf run. A test replaces it with a
/// fake load (`perf_load_override`).
pub(crate) fn perf_load(project: &Path) -> PerfLoad {
    #[cfg(test)]
    if let Some(load) = PERF_LOAD.with(|slot| slot.borrow().clone()) {
        return load;
    }
    PerfLoad {
        cpu_percent: cpu_percent(),
        disk_percent: disk_percent(project),
        builds: super::super::builds::running(super::super::now()).len() as u32,
        at: super::super::now(),
    }
}

#[cfg(test)]
thread_local! {
    static PERF_LOAD: std::cell::RefCell<Option<PerfLoad>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn perf_load_override(load: Option<PerfLoad>) {
    PERF_LOAD.with(|slot| *slot.borrow_mut() = load);
}

/// The busy share of the machine's CPU, sampled over a short window.
#[cfg(windows)]
fn cpu_percent() -> f64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetSystemTimes;
    let sample = || -> (u64, u64, u64) {
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
        let time = |value: &FILETIME| ((u64::from(value.dwHighDateTime)) << 32) | u64::from(value.dwLowDateTime);
        (time(&idle), time(&kernel), time(&user))
    };
    let (idle, kernel, user) = sample();
    std::thread::sleep(Duration::from_millis(75));
    let (idle_after, kernel_after, user_after) = sample();
    let total = (kernel_after.saturating_sub(kernel) + user_after.saturating_sub(user)) as f64;
    let idle = idle_after.saturating_sub(idle) as f64;
    if total <= 0.0 {
        0.0
    } else {
        ((total - idle) / total * 100.0).clamp(0.0, 100.0)
    }
}

#[cfg(unix)]
fn cpu_percent() -> f64 {
    let read = || -> Option<(f64, f64)> {
        let text = std::fs::read_to_string("/proc/stat").ok()?;
        let line = text.lines().find(|line| line.starts_with("cpu "))?;
        let values: Vec<f64> = line.split_whitespace().skip(1).map(|value| value.parse().unwrap_or(0.0)).collect();
        let idle = values.get(3).copied().unwrap_or(0.0) + values.get(4).copied().unwrap_or(0.0);
        Some((values.iter().sum(), idle))
    };
    let Some((total, idle)) = read() else { return 0.0 };
    std::thread::sleep(Duration::from_millis(75));
    let Some((total_after, idle_after)) = read() else { return 0.0 };
    let total = total_after - total;
    let idle = idle_after - idle;
    if total <= 0.0 {
        0.0
    } else {
        ((total - idle) / total * 100.0).clamp(0.0, 100.0)
    }
}

/// The used share of the volume `project` is on.
fn disk_percent(project: &Path) -> f64 {
    let existing = project.canonicalize().unwrap_or_else(|_| project.to_path_buf());
    let (Some(total), Some(free)) = (fs2::total_space(&existing).ok(), fs2::available_space(&existing).ok()) else {
        return 0.0;
    };
    if total == 0 {
        0.0
    } else {
        ((total.saturating_sub(free)) as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
    }
}

#[cfg(windows)]
fn free_memory_mb() -> Option<u32> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some((status.ullAvailPhys / (1024 * 1024)) as u32)
}

#[cfg(unix)]
fn free_memory_mb() -> Option<u32> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb: u64 = text
        .lines()
        .find(|line| line.starts_with("MemAvailable:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some((kb / 1024) as u32)
}

/// U62 (#717): one worker tick. An empty queue costs no load read; after a read
/// the next is 30 s away (the returned bool says a read happened); the night
/// window comes from the project's `[schedule]`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn tick(
    store: &TestStore,
    project: &Path,
    team: &str,
    classes: &Classes,
    night: &config::Schedule,
    local_hour: u32,
    last_load_at: f64,
    now: f64,
    read_load: impl FnOnce() -> Load,
    policy: &super::test_results::Policy,
    places: &Places,
    overrides: Option<&Value>,
) -> anyhow::Result<(Vec<Record>, bool)> {
    let queued = store.queued(team)?;
    if queued.is_empty() || now - last_load_at < 30.0 {
        return Ok((Vec::new(), false));
    }
    // #732: a suite of a class this computer is not goes only to a runner of
    // that class (or waits with the reason); the rest runs here as before.
    let (here, elsewhere): (Vec<Request>, Vec<Request>) =
        queued.into_iter().partition(|request| super::artifacts::profile_of(&request.command).is_some() || super::test_runners::runs_here(&request.platforms, &places.host));
    let mut records = Vec::new();
    if !here.is_empty() {
        let mut load = read_load();
        load.at_night = config::is_night(local_hour, night);
        let chosen = schedule(&here, classes, &load);
        records = run_due(store, project, &chosen, now, policy, overrides)?;
    }
    if !elsewhere.is_empty() {
        records.extend(super::test_runners::dispatch(store, project, &elsewhere, &places.runners, places.remote, now, policy)?);
    }
    Ok((records, true))
}

/// #732: where a tick may run suites: this computer's classes, the owner's
/// test runners and how they are reached.
pub(crate) struct Places<'a> {
    pub host: Vec<String>,
    pub runners: Vec<super::test_runners::Runner>,
    pub remote: &'a dyn super::test_runners::Remote,
}

#[cfg(test)]
impl Places<'static> {
    /// This computer only, as a team without test runners has it.
    pub(crate) fn here() -> Self {
        Places { host: super::test_runners::host_classes(), runners: Vec::new(), remote: &super::test_runners::Ssh }
    }
}

/// The requests a tick runs, in order: forced first, then short before long,
/// then by age; a long suite waits for the night (or the CI) unless forced, and
/// never two heavy suites at once. The machine's free CPU, RAM and GPU must fit.
pub(crate) fn schedule(requests: &[Request], classes: &Classes, load: &Load) -> Vec<Request> {
    let mut ordered: Vec<&Request> = requests.iter().collect();
    ordered.sort_by(|a, b| b.forced.cmp(&a.forced).then(a.seconds.total_cmp(&b.seconds)).then(a.at.total_cmp(&b.at)).then(a.id.cmp(&b.id)));
    let (mut cpu, mut ram, mut gpu, mut heavy) = (load.cpu_free, load.ram_free_mb, load.gpu_free, false);
    let mut chosen = Vec::new();
    for request in ordered {
        if !request.forced && long(request) && !load.at_night {
            continue;
        }
        let class = classes.get(&request.resources);
        let (need_cpu, need_ram, need_gpu) = class.map_or((1, 0, false), |class| (class.cpu.max(1), class.ram_mb, class.gpu));
        if need_cpu > cpu || need_ram > ram || (need_gpu && !gpu) {
            continue;
        }
        let this_heavy = is_heavy(class);
        if this_heavy && heavy {
            continue;
        }
        cpu -= need_cpu;
        ram = ram.saturating_sub(need_ram);
        if need_gpu {
            gpu = false;
        }
        heavy |= this_heavy;
        chosen.push(request.clone());
    }
    // Task #281: a comparison's binaries alternate in rounds by default.
    alternate_comparisons(chosen)
}

/// Task #281: a comparison group's requests alternate the binaries in rounds
/// (A, B, A, B), never one binary's rounds back to back; a request outside a
/// group keeps its place. The binary is the suite; the rounds are the order
/// the group's requests were queued in.
fn alternate_comparisons(chosen: Vec<Request>) -> Vec<Request> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut groups: BTreeMap<String, Vec<Request>> = BTreeMap::new();
    for request in &chosen {
        if !request.compare.is_empty() {
            groups.entry(request.compare.clone()).or_default().push(request.clone());
        }
    }
    if groups.is_empty() {
        return chosen;
    }
    let alternated: BTreeMap<String, Vec<Request>> = groups
        .into_iter()
        .map(|(name, list)| {
            let mut binaries: Vec<String> = Vec::new();
            for request in &list {
                if !binaries.contains(&request.suite) {
                    binaries.push(request.suite.clone());
                }
            }
            let mut buckets: Vec<Vec<Request>> = binaries
                .iter()
                .map(|suite| list.iter().filter(|request| &request.suite == suite).cloned().collect())
                .collect();
            let mut mixed = Vec::new();
            loop {
                let mut took = false;
                for bucket in &mut buckets {
                    if !bucket.is_empty() {
                        mixed.push(bucket.remove(0));
                        took = true;
                    }
                }
                if !took {
                    break;
                }
            }
            (name, mixed)
        })
        .collect();
    let mut emitted: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for request in chosen {
        if request.compare.is_empty() {
            out.push(request);
        } else if emitted.insert(request.compare.clone()) {
            out.extend(alternated.get(&request.compare).cloned().unwrap_or_default());
        }
    }
    out
}

/// Task 174: a queued suite gets what the members' templates get: the build
/// cache, and for CMake or CTest the tools and Qt's settings.
fn queued_shell(project: &Path, command: &str, cwd: &Path) -> Command {
    let mut process = shell(command, cwd);
    process.envs(super::templates::queue_environment(project, command));
    process
}

pub(crate) fn shell(command: &str, cwd: &Path) -> Command {
    #[cfg(windows)]
    let mut process = {
        use std::os::windows::process::CommandExt;
        let mut process = Command::new("cmd");
        process.args(["/D", "/S", "/C"]);
        // /S reads the whole command from one pair of quotes and keeps the
        // quotes inside it. A command such as `cd /d "tests" && ...` must
        // arrive as the project wrote it: Rust's arg quoting escapes the
        // inner quotes as \" and cmd fails with "The system cannot find the
        // path specified.".
        process.raw_arg(format!("\"{command}\""));
        process
    };
    #[cfg(not(windows))]
    let mut process = {
        let mut process = Command::new("sh");
        process.args(["-c", command]);
        process
    };
    process.current_dir(cwd);
    process
}

fn expand_stage(template: &str, build_dir: &str, targets: &str, local: &config::LocalProvider) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start + 1..].find('}').map(|end| start + 1 + end) else {
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let name = &rest[start + 1..end];
        if !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            let value = match name {
                "build_dir" => build_dir,
                "targets" => targets,
                _ => local.vars.get(name).map(String::as_str).ok_or_else(|| format!("unresolved local placeholder {{{name}}}"))?,
            };
            out.push_str(value);
        } else {
            out.push_str(&rest[start..=end]);
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[derive(PartialEq)]
enum PreparationFailure { NotRun, Build }

fn preparation_not_run(suite: &str, reason: &str) -> (Outcome, Option<PreparationFailure>) {
    let prefix = format!("not run here: {suite} runs on this computer; ");
    let tail = format!("{prefix}{}", last_chars(reason, EXCERPT_BYTES.saturating_sub(prefix.chars().count())));
    (Outcome { tail, ..Default::default() }, Some(PreparationFailure::NotRun))
}

/// Only local merge requests prepare CTest. All stages and retries share the
/// suite deadline; no new queue metadata or team-record read is needed.
fn run_request(request: &Request, project: &Path, limit: Duration, deadline: Instant, loaded: Option<&Result<config::Loaded, String>>) -> (Outcome, Option<PreparationFailure>) {
    if let Some(profile)=super::artifacts::profile_of(&request.command) {
        return match super::artifacts::queued(project,profile,request,limit) {
            Ok(outcome)=>(outcome,None),
            Err(error)=>(Outcome{error:error.to_string(),tail:error.to_string(),..Default::default()},Some(PreparationFailure::NotRun)),
        };
    }
    let loaded = match loaded {
        None => return (run_tree(&mut queued_shell(project, &request.command_for(project), project), limit, None), None),
        Some(Ok(loaded)) => loaded,
        Some(Err(error)) => return (Outcome { exit: Some(1), tail: format!("Test configuration refused: {error}"), ..Default::default() }, None),
    };
    let provider = loaded.config.providers.iter().find(|provider| provider.name == request.suite);
    let Some(provider) = provider.filter(|provider| provider.kind == config::Kind::Ctest && !provider.build.is_empty()) else {
        return (run_tree(&mut queued_shell(project, &request.command_for(project), project), limit, None), None);
    };
    let local = loaded.local.get(&provider.name).cloned().unwrap_or_default();
    let build_dir = crate::test_discipline::affected::relative_to(&provider.build_dir, &provider.root);
    let targets = if request.targets.iter().any(|target| target == crate::test_discipline::map::ALL) || request.targets.is_empty() {
        String::new()
    } else {
        format!("--target {}", request.targets.iter().map(|target| crate::test_discipline::affected::quote(target)).collect::<Vec<_>>().join(" "))
    };
    let cwd = project.join(&provider.root);
    let configure = !project.join(&provider.build_dir).join("CMakeCache.txt").is_file();
    let stages = [("probe", provider.probe.as_str(), cwd.as_path()),
        ("configure", if configure { &provider.configure } else { "" }, cwd.as_path()),
        ("build", provider.build.as_str(), cwd.as_path()), ("ctest", request.command.as_str(), project)];
    let expanded = stages.iter().map(|(stage, command, cwd)| expand_stage(command, &build_dir, &targets, &local).map(|command| (*stage, command, *cwd)))
        .collect::<Result<Vec<_>, _>>();
    let expanded = match expanded {
        Ok(expanded) => expanded,
        Err(reason) => return preparation_not_run(&request.suite, &reason),
    };
    let mut combined = String::new();
    let mut outcome = Outcome::default();
    for (stage, command, cwd) in expanded.into_iter().filter(|(_, command, _)| !command.is_empty()) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() { return (Outcome { timed_out: true, tail: last_chars(&combined, EXCERPT_BYTES), ..Default::default() }, None); }
        let mut process = queued_shell(project, &command, cwd);
        process.envs(&local.env);
        outcome = run_tree(&mut process, remaining, None);
        combined = last_chars(&format!("{combined}\n[{stage}]\n{}{}", outcome.tail, outcome.error), EXCERPT_BYTES);
        if outcome.exit != Some(0) || outcome.timed_out {
            if stage == "probe" {
                return preparation_not_run(&request.suite, &combined);
            }
            outcome.tail = combined;
            let failure = matches!(stage, "configure" | "build").then_some(PreparationFailure::Build);
            return (outcome, failure);
        }
    }
    outcome.tail = combined;
    (outcome, None)
}

/// Runs the scheduled requests in order and writes one result each. The command
/// comes from the project's configuration, never from a report; a timeout or a
/// nonzero exit is the result's status. #718: a failure is retried as the
/// policy says and a pass on a retry is flaky (with its episode recorded); a
/// crash is never retried; a run slower than its recorded time by more than the
/// growth share, or over its time budget, is flagged.
pub(crate) fn run_due(store: &TestStore, project: &Path, requests: &[Request], now: f64, policy: &super::test_results::Policy, overrides: Option<&Value>) -> anyhow::Result<Vec<Record>> {
    let mut written = Vec::new();
    let began = std::time::Instant::now();
    // Task #281: a perf run checks the machine first, and records what it saw.
    let performance = crate::test_discipline::load(project, None).map(|loaded| loaded.config.performance).unwrap_or_default();
    for request in requests {
        let started = std::time::Instant::now();
        let at = now + began.elapsed().as_secs_f64();
        let mut loaded = false;
        let mut before = None;
        if request.performance && super::artifacts::profile_of(&request.command).is_none() {
            let sample = perf_load(project);
            match perf_gate(&sample, &performance, request.at, at) {
                PerfGate::Wait => {
                    store.hold(&request.id, &format!(
                        "waiting for a quiet machine: cpu {:.0}% (limit {}%), disk {:.0}% (limit {}%), {} Zerus build(s) (limit {})",
                        sample.cpu_percent, performance.cpu_percent, sample.disk_percent, performance.disk_percent,
                        sample.builds, performance.builds
                    ))?;
                    continue;
                }
                PerfGate::Run(over) => {
                    loaded = over;
                    before = Some(sample);
                }
            }
        }
        store.start(&request.id, at)?;
        // Task 88: the provider's report, removed first so an older one is
        // never read as this run's.
        let report = if super::artifacts::profile_of(&request.command).is_some(){None}else{report_of(project, &request.team, &request.suite)};
        if let Some((path, _)) = &report {
            let _ = std::fs::remove_file(path);
        }
        let limit = Duration::from_secs((request.budget_minutes.max(1.0) * 60.0) as u64);
        let deadline = started + limit;
        let config_loaded = (request.asked_by == "merge queue").then(|| config::load(project, overrides).map_err(|error| error.to_string()));
        // Task #18: a suite whose programs Windows would start at Low
        // integrity (its folder carries a DeepSeek sandbox's Low label) is one
        // environment failure naming the owner's step: it is not run, and a
        // run it already failed that way is not retried.
        let command = request.command_for(project);
        let temp = std::env::temp_dir();
        let refused = if super::artifacts::profile_of(&request.command).is_some(){None}else{super::environment::preflight(&super::environment::build_folders(project, &command, ""), Some(&temp))};
        let (mut outcome, preparation) = match &refused {
            Some(text) => (Outcome { exit: Some(1), tail: text.clone(), ..Default::default() }, None),
            None => run_request(request, project, limit, deadline, config_loaded.as_ref()),
        };
        let environment = refused.or_else(|| {
            if super::artifacts::profile_of(&request.command).is_some() {return None;}
            (outcome.exit != Some(0) && !outcome.timed_out && preparation != Some(PreparationFailure::NotRun))
                .then(|| {
                    let text = format!("{}\n{}\n{}", outcome.tail, outcome.excerpt, outcome.error);
                    super::environment::diagnose(&text, &super::environment::build_folders(project, &command, &text), Some(&temp))
                })
                .flatten()
        });
        let mut flags = Vec::new();
        if let Some(text) = &environment {
            flags.push("environment".to_owned());
            let before = if outcome.tail == *text { String::new() } else { last_chars(&outcome.excerpt, EXCERPT_BYTES / 2) };
            outcome.excerpt = last_chars(&format!("{before}\n{text}"), EXCERPT_BYTES);
        }
        let crash = environment.is_none() && super::test_results::crashed(outcome.exit, &outcome.tail);
        let mut status = if environment.is_some() {
            "failed".to_owned()
        } else if preparation == Some(PreparationFailure::NotRun) {
            "not_run".to_owned()
        } else if preparation == Some(PreparationFailure::Build) {
            flags.push("build".to_owned());
            "failed".to_owned()
        } else if crash {
            flags.push("crash".to_owned());
            "crashed".to_owned()
        } else if outcome.timed_out {
            "timeout".to_owned()
        } else if outcome.exit == Some(0) {
            "passed".to_owned()
        } else {
            "failed".to_owned()
        };
        let first = outcome.clone();
        // Build failures, crashes and timeouts are not flaky test evidence.
        let mut retries = 0;
        if super::artifacts::profile_of(&request.command).is_some() {
            if let Some(remote)=super::artifacts::result_status(&format!("{}:{}",request.team,request.id)) {
                status=match remote.as_str(){"passed"|"failed"|"timeout"|"unknown"|"infrastructure_error"|"not_run"=>remote,_=>"unknown".into()};
            } else { status="not_run".into(); }
        }
        while status == "failed" && super::artifacts::profile_of(&request.command).is_none() && environment.is_none() && preparation != Some(PreparationFailure::Build) && retries < policy.retries {
            retries += 1;
            let (again, preparation) = run_request(request, project, limit, deadline, config_loaded.as_ref());
            if preparation == Some(PreparationFailure::NotRun) {
                status = "not_run".into();
                outcome = again;
                break;
            }
            if preparation == Some(PreparationFailure::Build) {
                flags.push("build".to_owned());
                outcome = again;
                break;
            }
            if super::test_results::crashed(again.exit, &again.tail) {
                flags.push("crash".to_owned());
                status = "crashed".into();
                outcome = again;
                break;
            }
            if again.exit == Some(0) && !again.timed_out {
                flags.push("retried".to_owned());
                status = "flaky".into();
                outcome = again;
                break;
            }
            outcome = again;
        }
        let remote_performance=request.performance&&super::artifacts::profile_of(&request.command).is_some();
        let remote_load=remote_performance.then(||super::artifacts::result_load(&format!("{}:{}",request.team,request.id))).flatten();
        let duration_ms = if remote_performance{
            match remote_load.as_ref().filter(|load|load["valid"]==true).and_then(|load|load["execution_ms"].as_u64()).filter(|ms|*ms>0){Some(ms)=>ms,None=>{flags.push("measurement_unavailable".into());0}}
        }else{u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)};
        let ran_ms = if status == "flaky" { duration_ms / u64::from(retries + 1) } else { duration_ms };
        if request.seconds > 0.0 && policy.grow_percent > 0 && ran_ms as f64 > request.seconds * 1000.0 * (1.0 + f64::from(policy.grow_percent) / 100.0) {
            flags.push("slower".to_owned());
        }
        if request.budget_minutes > 0.0 && (ran_ms as f64 > request.budget_minutes * 60_000.0 || outcome.timed_out) {
            flags.push("over_time_budget".to_owned());
        }
        // A flaky run keeps the failure it hid: that is the episode's evidence.
        let shown = if status == "flaky" { &first } else { &outcome };
        let excerpt = if shown.excerpt.is_empty() { last_chars(&shown.tail, EXCERPT_BYTES) } else { shown.excerpt.clone() };
        // Task #281: the load after the run too, so the result carries both.
        let load = if remote_performance{
            if remote_load.as_ref().is_some_and(|load|load["loaded"]==true){flags.push("loaded".into());}
            remote_load
        }else if request.performance {
            let during = perf_load(project);
            if !during.quiet(&performance) {
                loaded = true;
            }
            if loaded && !flags.iter().any(|flag| flag == "loaded") {
                flags.push("loaded".to_owned());
            }
            Some(serde_json::json!({
                "before": before,
                "during": during,
                "loaded": loaded,
                "limits": {"cpu_percent": performance.cpu_percent, "disk_percent": performance.disk_percent,
                    "builds": performance.builds, "wait_seconds": performance.wait_seconds},
                "waited_s": (at - request.at).max(0.0),
            }))
        } else {
            None
        };
        let record = Record {
            id: format!("{}-{}", request.id, now as i64),
            team: request.team.clone(),
            suite: request.suite.clone(),
            commit: request.commit.clone(),
            input_hash: request.input_hash.clone(),
            status,
            duration_ms,
            excerpt,
            asked_by: request.asked_by.clone(),
            at,
            affected: request.targets.clone(),
            reused_from: String::new(),
            flags,
            due: false,
            recorded_ms: (request.seconds * 1000.0) as u64,
            runner: super::artifacts::profile_of(&request.command).map(|profile|format!("executor:{profile}")).unwrap_or_default(),
            task: request.task,
            report: report.map(|(path, format)| super::run_results::read(&path, format)),
            performance: request.performance,
            load,
        };
        store.write(&record)?;
        if record.status == "flaky" {
            store.add_flaky(&record.team, &record.suite, &record.id, now)?;
        }
        store.finish(&request.id)?;
        written.push(record);
    }
    Ok(written)
}

/// Task 88: where a suite's provider says its run leaves its report.
pub(crate) fn report_of(project: &Path, team: &str, suite: &str) -> Option<(std::path::PathBuf, super::run_results::Format)> {
    let found = |overrides: Option<&serde_json::Value>| -> Option<Option<(std::path::PathBuf, super::run_results::Format)>> {
        let loaded = crate::test_discipline::config::load(project, overrides).ok()?;
        let provider = loaded.config.providers.iter().find(|provider| provider.name == suite)?;
        Some(provider.results.as_ref().and_then(|results| Some((project.join(&results.path), super::run_results::Format::parse(&results.format)?))))
    };
    // The project's own file first; the owner's override only for a suite the
    // project has, so no store is read for an unknown one.
    match found(None)? {
        Some(report) => Some(report),
        None if !team.is_empty() => {
            let overrides = super::store::Store::open(false).ok()?.load(team).ok()??.settings.tests.clone()?;
            found(Some(&overrides))?
        }
        None => None,
    }
}

impl Request {
    /// The command the runner executes: the project file's provider command.
    fn command_for(&self, _project: &Path) -> String {
        self.command.clone()
    }
}

/// A suite the CLI or a test builds from the configuration: its command, folder,
/// targets, recorded seconds, budget and resource class.
pub(crate) fn suite_from_config(project: &Path, team: Option<&str>, name: &str, targets: &[String]) -> anyhow::Result<Request> {
    let loaded = crate::test_discipline::load(project, team).map_err(|error| anyhow::anyhow!("{error}"))?;
    let config = &loaded.config;
    let provider = config.providers.iter().find(|provider| provider.name == name).ok_or_else(|| anyhow::anyhow!("No provider {name}"))?;
    let command = if provider.command.is_empty() {
        // The kind's own command is the config's job; refuse rather than guess.
        anyhow::bail!("Provider {name} has no command in {}", config::FILE)
    } else {
        provider.command.replace("{targets}", &targets.join(" ")).replace("{manifest}", &provider.manifest).replace("{build_dir}", &provider.build_dir)
    };
    let budget = if provider.budget_minutes > 0.0 { provider.budget_minutes } else { config.budgets.suite_minutes };
    Ok(Request {
        id: uuid::Uuid::new_v4().to_string(),
        team: String::new(),
        suite: name.to_owned(),
        task: None,
        targets: targets.to_vec(),
        commit: String::new(),
        input_hash: String::new(),
        asked_by: String::new(),
        forced: false,
        at: super::super::now(),
        seconds: 0.0,
        budget_minutes: budget,
        resources: provider.resources.clone(),
        command,
        platforms: suite_platforms(provider),
        // Task #281: a provider marked performance, or a bench/perf suite.
        performance: provider.performance
            || name.to_lowercase().contains("bench")
            || name.to_lowercase().contains("perf")
            || targets.iter().any(|target| target.to_lowercase().contains("bench") || target.to_lowercase().contains("perf")),
        compare: String::new(),
    })
}

/// #732: a suite's platform classes: its provider's `platforms`; Android's
/// Gradle suites are the android class unless the project names others.
pub(crate) fn suite_platforms(provider: &crate::test_discipline::config::Provider) -> Vec<String> {
    provider.platform_classes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_route_never_falls_back_to_shell_or_platform_runner() {
        let dir=tempfile::tempdir().unwrap();let store=TestStore::open_at(&dir.path().join("tests.sqlite3"),false).unwrap();
        let mut queued=request("synthetic-artifact-routing","remote",1.0,true,"small");
        queued.command="zerus-artifact:invalid; echo wrong > wrong.txt".into();queued.platforms=vec!["console-only".into()];store.enqueue(&queued).unwrap();
        let (records,_)=tick(&store,dir.path(),"t",&Classes::new(),&config::Schedule::default(),12,0.0,100.0,||Load{cpu_free:8,ram_free_mb:8000,gpu_free:true,at_night:false},&Default::default(),&Places::here(),None).unwrap();
        assert_eq!(records.len(),1,"explicit artifact route must reach its own admission even on a different platform");
        assert_eq!(records[0].status,"not_run");assert!(records[0].flags.iter().all(|flag|flag!="retried"));assert!(!dir.path().join("wrong.txt").exists());
    }

    fn request(id: &str, suite: &str, seconds: f64, forced: bool, resources: &str) -> Request {
        Request {
            id: id.into(), team: "t".into(), suite: suite.into(), task: None, targets: Vec::new(),
            commit: "c".into(), input_hash: "h".into(), asked_by: "dev".into(), forced, at: 1.0,
            seconds, budget_minutes: 10.0, resources: resources.into(), command: String::new(), platforms: Vec::new(),
            performance: false, compare: String::new(),
        }
    }

    #[test]
    fn the_worker_tick_reads_the_load_only_for_a_queue_and_once_in_thirty_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let store = TestStore::open_at(&dir.path().join("tests.sqlite3"), false).unwrap();
        let classes = Classes::new();
        let night = config::Schedule::default();
        let reads = std::cell::Cell::new(0u32);
        let load = || {
            reads.set(reads.get() + 1);
            Load { cpu_free: 8, ram_free_mb: 8000, gpu_free: true, at_night: false }
        };
        // An empty queue: no read at all.
        let (records, read) = tick(&store, dir.path(), "t", &classes, &night, 12, 0.0, 100.0, load, &Default::default(), &Places::here(), None).unwrap();
        assert!(records.is_empty() && !read && reads.get() == 0, "an empty queue reads no load");
        let mut queued = request("r1", "unit", 10.0, false, "small");
        queued.command = if cfg!(windows) { "cmd /d /s /c exit 0".into() } else { "true".into() };
        store.enqueue(&queued).unwrap();
        // The last read was a moment ago: no second read.
        let (records, read) = tick(&store, dir.path(), "t", &classes, &night, 12, 90.0, 100.0, load, &Default::default(), &Places::here(), None).unwrap();
        assert!(records.is_empty() && !read && reads.get() == 0, "a read waits 30 s");
        // Enough time passed and the queue has work: one read, one run.
        let (records, read) = tick(&store, dir.path(), "t", &classes, &night, 12, 0.0, 100.0, load, &Default::default(), &Places::here(), None).unwrap();
        assert!(read && reads.get() == 1, "the tick read the load once");
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].status, "passed");
        assert!(load_now().cpu_free >= 1, "the real load reader sees the CPUs");
    }

    #[test]
    fn the_night_window_wraps_midnight_and_excludes_the_day() {
        let night = config::Schedule { night_start: 22, night_end: 6 };
        assert!(config::is_night(23, &night) && config::is_night(0, &night) && config::is_night(5, &night));
        assert!(!config::is_night(6, &night) && !config::is_night(12, &night) && !config::is_night(21, &night));
        let day = config::Schedule { night_start: 9, night_end: 17 };
        assert!(config::is_night(10, &day) && !config::is_night(18, &day));
        assert!(!config::is_night(3, &config::Schedule { night_start: 5, night_end: 5 }));
    }

    #[test]
    fn a_result_leaves_one_notice_and_the_inbox_card_links_it() {
        use crate::state::teams::model::{Team, TeamStatus};
        let mut team = Team { status: TeamStatus::Running, ..Default::default() };
        let record = Record {
            id: "r1".into(),
            team: "t".into(),
            suite: "unit".into(),
            commit: "c".into(),
            input_hash: "h".into(),
            status: "passed".into(),
            duration_ms: 1500,
            excerpt: "the log".into(),
            asked_by: "dev".into(),
            at: 10.0,
            affected: Vec::new(), reused_from: String::new(), flags: Vec::new(), due: false, recorded_ms: 0, task: None, runner: String::new(), report: None, performance: false, load: None,
        };
        record_notices(&mut team, std::slice::from_ref(&record), 100.0);
        assert_eq!(team.test_load_at, 100.0);
        // Task #29: a test result clears after 12 h; read the inbox at this
        // test's own clock, just after its records.
        let items = crate::state::with_test_now(1000.0, || super::super::inbox::items(&team));
        let item = items.iter().find(|item| item.kind == "test_result").expect("a test result card");
        assert!(
            item.summary.contains("unit") && item.summary.contains("passed") && item.summary.contains("r1"),
            "{}",
            item.summary
        );
        // Task 374: the card prints a command that reads the record (the
        // action in its JSON), never a usage error.
        assert!(item.command.contains("hgs team test --json"), "{}", item.command);
        assert!(item.command.contains("\"action\":\"results\"") || item.command.contains("\"action\": \"results\""), "{}", item.command);
        assert!(item.command.contains("\"record\":\"r1\"") || item.command.contains("\"record\": \"r1\""), "{}", item.command);
        assert!(item.detail.contains("r1") && item.detail.contains("dev"), "{}", item.detail);
        assert!(!item.detail.contains("the log"), "the card links the record, never the log");
        // The notice list is bounded.
        for n in 0..60 {
            record_notices(&mut team, &[Record { id: format!("x{n}"), ..record.clone() }], 100.0);
        }
        assert_eq!(team.test_notices.len(), 50);
    }

    #[test]
    fn the_scheduler_runs_forced_and_short_suites_first_and_one_heavy_at_a_time() {
        let classes: Classes = [
            ("gpu".to_owned(), Resources { cpu: 8, ram_mb: 16000, gpu: true, exclusive: false }),
            ("small".to_owned(), Resources { cpu: 1, ram_mb: 512, ..Default::default() }),
        ]
        .into_iter()
        .collect();
        let requests = vec![
            request("long", "long", 1000.0, false, "small"),
            request("heavy-a", "a", 10.0, false, "gpu"),
            request("heavy-b", "b", 10.0, false, "gpu"),
            request("forced", "f", 20.0, true, "small"),
            request("short", "s", 5.0, false, "small"),
        ];
        let load = Load { cpu_free: 16, ram_free_mb: 32000, gpu_free: true, at_night: false };
        let picked = schedule(&requests, &classes, &load);
        let chosen: Vec<&str> = picked.iter().map(|request| request.id.as_str()).collect();
        assert_eq!(chosen, ["forced", "short", "heavy-a"], "forced first, short before long, one heavy; a long suite waits for the night");
        let night = Load { at_night: true, ..load };
        let picked = schedule(&requests, &classes, &night);
        let chosen: Vec<&str> = picked.iter().map(|request| request.id.as_str()).collect();
        assert!(chosen.contains(&"long"), "{chosen:?}");
    }

    #[test]
    fn results_page_and_resolve_by_their_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let store = TestStore::open_at(&dir.path().join("tests.sqlite3"), false).unwrap();
        let record = |id: &str, suite: &str, at: f64| Record {
            id: id.into(), team: "t".into(), suite: suite.into(), commit: "c".into(), input_hash: "h".into(),
            status: "passed".into(), duration_ms: 10, excerpt: "ok".into(), asked_by: "dev".into(), at,
            affected: vec!["a".into()], reused_from: String::new(), flags: Vec::new(), due: false, recorded_ms: 0, task: None, runner: String::new(), report: None, performance: false, load: None,
        };
        store.write(&record("1", "cargo", 1.0)).unwrap();
        store.write(&record("2", "cargo", 2.0)).unwrap();
        let found = store.last_green("t", "cargo", "h").unwrap().unwrap();
        assert_eq!(found.id, "2");
        let page = store.results(Some("t"), None, 1, None, None).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, "2");
        let older = store.results(Some("t"), Some(2.0), 10, None, None).unwrap();
        assert_eq!(older.len(), 1);
        assert_eq!(older[0].id, "1");
    }

    /// Task 88: a provider that names its report gets one result per test in
    /// its record, kept in the database and read back with it.
    #[test]
    fn a_suite_with_a_report_keeps_one_result_per_test() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".zerus")).unwrap();
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        std::fs::write(dir.path().join(".zerus").join("tests.toml"),
            "version = 1
[[provider]]
name = \"engine\"
kind = \"command\"
affected = \"all\"
command = \"x\"
results = { format = \"junit\", path = \"out/junit.xml\" }
").unwrap();
        std::fs::write(dir.path().join("fixture.xml"),
            r#"<testsuite><testcase classname="math" name="adds" time="0.1"/><testcase classname="math" name="divides" time="0.2"><failure message="expected 2"/></testcase></testsuite>"#).unwrap();
        // A report from an earlier run is removed before this one.
        std::fs::write(dir.path().join("out").join("junit.xml"), "<testsuite><testcase name=\"stale\"/></testsuite>").unwrap();
        let store = TestStore::open_at(&dir.path().join("tests.sqlite3"), false).unwrap();
        let mut reported = request("rep", "engine", 1.0, false, "small");
        reported.command = if cfg!(windows) { r"copy /y fixture.xml out\junit.xml >nul & exit /b 1".into() } else { "cp fixture.xml out/junit.xml; exit 1".into() };
        store.enqueue(&reported).unwrap();
        let written = run_due(&store, dir.path(), &store.queued("t").unwrap(), 100.0, &Default::default(), None).unwrap();
        assert_eq!(written[0].status, "failed");
        let report = written[0].report.clone().expect("the provider's report");
        assert_eq!((report.passed, report.failed), (1, 1), "{report:?}");
        assert_eq!(report.results[0].name, "divides");
        let read = store.results(Some("t"), None, 10, None, None).unwrap();
        assert_eq!(read[0].report, Some(report));
        // A suite without a report keeps none.
        let mut plain = request("plain", "other", 1.0, false, "small");
        plain.command = if cfg!(windows) { "exit /b 0".into() } else { "true".into() };
        store.enqueue(&plain).unwrap();
        let written = run_due(&store, dir.path(), &store.queued("t").unwrap(), 101.0, &Default::default(), None).unwrap();
        assert!(written[0].report.is_none());
    }

    #[test]
    fn the_runner_writes_the_result_with_its_excerpt_and_empties_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let store = TestStore::open_at(&dir.path().join("tests.sqlite3"), false).unwrap();
        let mut request = request("run", "echo", 5.0, false, "small");
        request.command = if cfg!(windows) { "echo hello & exit /b 0".into() } else { "echo hello".into() };
        store.enqueue(&request).unwrap();
        assert_eq!(store.queued("t").unwrap().len(), 1);
        let written = run_due(&store, dir.path(), &store.queued("t").unwrap(), 100.0, &Default::default(), None).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].status, "passed");
        assert!(written[0].excerpt.contains("hello"), "{}", written[0].excerpt);
        assert!(store.queued("t").unwrap().is_empty());
        let found = store.last_green("t", "echo", "h").unwrap().unwrap();
        assert_eq!(found.id, written[0].id);
    }

    /// Task #18: a suite in a folder with a DeepSeek sandbox's Low label (its
    /// programs would start at Low and be refused in %TEMP%) is one
    /// environment failure naming the owner's step, and nothing runs.
    #[test]
    fn a_suite_in_a_low_labeled_folder_is_one_environment_failure_and_never_runs() {
        let dir = tempfile::tempdir().unwrap();
        let store = TestStore::open_at(&dir.path().join("tests.sqlite3"), false).unwrap();
        let project = dir.path().join("checkout");
        std::fs::create_dir_all(project.join("target")).unwrap();
        super::super::environment::set_test_low(Some(vec![project.clone()]));
        let mut request = request("low", "rust", 5.0, false, "small");
        request.asked_by = "merge queue".into();
        request.command = if cfg!(windows) { "echo ran>ran.txt & exit /b 1".into() } else { "echo ran > ran.txt; exit 1".into() };
        store.enqueue(&request).unwrap();
        let policy = super::super::test_results::Policy { retries: 2, ..Default::default() };
        let written = run_due(&store, &project, &store.queued("t").unwrap(), 100.0, &policy, None).unwrap();
        super::super::environment::set_test_low(None);
        assert_eq!(written.len(), 1);
        let record = &written[0];
        assert_eq!(record.status, "failed");
        assert_eq!(record.flags, vec!["environment".to_owned()], "one environment failure, never retried");
        assert!(record.excerpt.contains(super::super::environment::MARKER), "{}", record.excerpt);
        assert!(record.excerpt.contains(&format!("icacls \"{}\" /setintegritylevel \"(OI)(CI)M\"", project.display())), "{}", record.excerpt);
        assert!(!project.join("ran.txt").exists(), "nothing ran");
        assert!(store.result_of("low").unwrap().is_some());
    }

    /// Task #18: a run that failed because its programs were refused ("Access
    /// is denied", os error 5) while they were built in a Low-labeled target is
    /// reported once as that environment failure and is not retried.
    #[test]
    fn a_run_refused_from_a_low_target_is_one_environment_failure_and_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let store = TestStore::open_at(&dir.path().join("tests.sqlite3"), false).unwrap();
        let project = dir.path().join("line");
        let target = dir.path().join("shared").join("target");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(target.join("debug").join("deps")).unwrap();
        super::super::environment::set_test_low(Some(vec![dir.path().join("shared")]));
        let exe = target.join("debug").join("deps").join("hgs-1.exe");
        let mut request = request("refused", "rust", 5.0, false, "small");
        request.command = if cfg!(windows) {
            format!("echo x>>count.txt & echo Running unittests src\\main.rs ({}) & echo called `Result::unwrap()` on an `Err` value: Access is denied. (os error 5) & exit /b 101", exe.display())
        } else {
            format!("echo x >> count.txt; echo 'Running unittests src/main.rs ({})'; echo 'Access is denied. (os error 5)'; exit 101", exe.display())
        };
        store.enqueue(&request).unwrap();
        let policy = super::super::test_results::Policy { retries: 2, ..Default::default() };
        let written = run_due(&store, &project, &store.queued("t").unwrap(), 100.0, &policy, None).unwrap();
        super::super::environment::set_test_low(None);
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].status, "failed");
        assert!(written[0].flags.contains(&"environment".to_owned()), "{:?}", written[0].flags);
        let shared = dir.path().join("shared");
        assert!(written[0].excerpt.contains(&format!("icacls \"{}\"", shared.display())), "{}", written[0].excerpt);
        let runs = std::fs::read_to_string(project.join("count.txt")).unwrap();
        assert_eq!(runs.lines().count(), 1, "an environment failure is not retried");
    }

    #[test]
    fn a_queued_command_with_a_quoted_folder_runs() {
        // The merge queue names its folder in quotes (`cd /d "tests" && ...`):
        // Windows cmd must receive that line unescaped, or /S sees \" and the
        // run fails in 0 s with "The system cannot find the path specified.".
        let dir = tempfile::tempdir().unwrap();
        let store = TestStore::open_at(&dir.path().join("tests.sqlite3"), false).unwrap();
        let folder = dir.path().join("quoted folder");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("marker.txt"), "quoted-folder-ok\n").unwrap();
        let mut request = request("quoted", "quoted", 5.0, false, "small");
        request.command = if cfg!(windows) {
            "cd /d \"quoted folder\" && type marker.txt".into()
        } else {
            "cd \"quoted folder\" && cat marker.txt".into()
        };
        store.enqueue(&request).unwrap();
        let written = run_due(&store, dir.path(), &store.queued("t").unwrap(), 100.0, &Default::default(), None).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].status, "passed", "{}", written[0].excerpt);
        assert!(written[0].excerpt.contains("quoted-folder-ok"), "{}", written[0].excerpt);
    }

    /// #719: the Tests view reads the queue with what runs now first; a result
    /// names its task, and a task or the profiler's window reads only its own.
    #[test]
    fn the_queue_shows_what_runs_and_a_result_names_its_task() {
        let dir = tempfile::tempdir().unwrap();
        let store = TestStore::open_at(&dir.path().join("tests.sqlite3"), false).unwrap();
        let mut waiting = request("waiting", "qt", 5.0, false, "small");
        waiting.task = Some(7);
        store.enqueue(&waiting).unwrap();
        let mut forced = request("forced", "cargo", 5.0, true, "small");
        forced.at = 2.0;
        store.enqueue(&forced).unwrap();
        let mut running = request("running", "echo", 5.0, false, "small");
        running.task = Some(7);
        running.command = if cfg!(windows) { "exit /b 0".into() } else { "true".into() };
        store.enqueue(&running).unwrap();
        store.start("running", 50.0).unwrap();
        let pending = store.pending("t").unwrap();
        let order: Vec<(&str, &str)> = pending.iter().map(|item| (item.request.id.as_str(), item.state.as_str())).collect();
        assert_eq!(order, [("running", "running"), ("forced", "queued"), ("waiting", "queued")]);
        assert_eq!(pending[0].started, Some(50.0));
        assert_eq!(serde_json::to_value(&pending[0]).unwrap()["suite"], "echo", "the request's fields sit beside its state");
        // The scheduler never picks a running request again.
        assert!(store.queued("t").unwrap().iter().all(|item| item.id != "running"));
        let written = run_due(&store, dir.path(), &[running], 100.0, &Default::default(), None).unwrap();
        assert_eq!(written[0].task, Some(7));
        assert!(store.pending("t").unwrap().iter().all(|item| item.request.id != "running"));
        let other = Record { id: "other".into(), task: Some(8), at: 200.0, ..written[0].clone() };
        store.write(&other).unwrap();
        let mine: Vec<String> = store.results(Some("t"), None, 20, Some(7), None).unwrap().into_iter().map(|record| record.id).collect();
        assert_eq!(mine, [written[0].id.clone()]);
        let window: Vec<String> = store.results(Some("t"), None, 20, None, Some(150.0)).unwrap().into_iter().map(|record| record.id).collect();
        assert_eq!(window, ["other"]);
    }

    /// #719: a database #717 made without the new columns gains them on open
    /// and keeps its rows.
    #[test]
    fn an_older_database_gains_the_start_and_task_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tests.sqlite3");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(
                "CREATE TABLE queue(id TEXT PRIMARY KEY,team TEXT NOT NULL,suite TEXT NOT NULL,task INTEGER,
                    targets TEXT NOT NULL,commit_id TEXT NOT NULL,input_hash TEXT NOT NULL,asked_by TEXT NOT NULL,
                    forced INTEGER NOT NULL,at REAL NOT NULL,seconds REAL NOT NULL,budget_minutes REAL NOT NULL,
                    resources TEXT NOT NULL,command TEXT NOT NULL,state TEXT NOT NULL);
                 CREATE TABLE results(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,team TEXT NOT NULL,
                    suite TEXT NOT NULL,commit_id TEXT NOT NULL,input_hash TEXT NOT NULL,status TEXT NOT NULL,
                    duration_ms INTEGER NOT NULL,excerpt TEXT NOT NULL,asked_by TEXT NOT NULL,at REAL NOT NULL,
                    affected TEXT NOT NULL);
                 INSERT INTO results(id,team,suite,commit_id,input_hash,status,duration_ms,excerpt,asked_by,at,affected)
                    VALUES('old','t','cargo','c','h','passed',5,'ok','dev',1.0,'[]');",
            )
            .unwrap();
        }
        let store = TestStore::open_at(&path, false).unwrap();
        let old = store.results(Some("t"), None, 10, None, None).unwrap();
        assert_eq!((old[0].id.as_str(), old[0].task), ("old", None));
        store.enqueue(&request("new", "cargo", 1.0, false, "small")).unwrap();
        assert_eq!(store.pending("t").unwrap()[0].started, None);
        // Opening again changes nothing.
        drop(store);
        assert_eq!(TestStore::open_at(&path, false).unwrap().pending("t").unwrap().len(), 1);
    }

    /// Task 174: the project's build cache serves the queue's runs too: a
    /// merged commit's cargo suites get the members' sccache wrapper.
    #[test]
    fn a_queued_run_uses_the_projects_build_cache() {
        use super::super::templates::{settings_override, tools_override};
        let dir = tempfile::tempdir().unwrap();
        settings_override(Some(dir.path().join("templates.json")));
        let wrapper = dir.path().join("sccache");
        tools_override(Some(std::collections::HashMap::from([("sccache".to_owned(), wrapper.clone())])));
        std::fs::create_dir_all(dir.path().join(".zerus")).unwrap();
        let shared = dir.path().join("cache");
        std::fs::write(dir.path().join(".zerus/tests.toml"), format!("version = 1\n[build_cache]\nenabled = true\ndir = '{}'\n", shared.display())).unwrap();
        let store = TestStore::open_at(&dir.path().join("tests.sqlite3"), false).unwrap();
        let mut probe = request("cache", "cargo", 1.0, true, "small");
        probe.command = if cfg!(windows) { "echo %RUSTC_WRAPPER%> wrapper.txt".into() } else { "printf '%s' \"$RUSTC_WRAPPER\" > wrapper.txt".into() };
        store.enqueue(&probe).unwrap();
        let written = run_due(&store, dir.path(), &store.queued("t").unwrap(), 100.0, &Default::default(), None).unwrap();
        assert_eq!(written[0].status, "passed", "{}", written[0].excerpt);
        let seen = std::fs::read_to_string(dir.path().join("wrapper.txt")).unwrap();
        assert_eq!(seen.trim(), wrapper.display().to_string());
        // Off: the run gets no wrapper.
        std::fs::write(dir.path().join(".zerus/tests.toml"), "version = 1\n[build_cache]\nenabled = false\n").unwrap();
        let mut off = request("off", "cargo", 1.0, true, "small");
        off.command = probe.command.clone();
        store.enqueue(&off).unwrap();
        run_due(&store, dir.path(), &store.queued("t").unwrap(), 101.0, &Default::default(), None).unwrap();
        let seen = std::fs::read_to_string(dir.path().join("wrapper.txt")).unwrap();
        assert_ne!(seen.trim(), wrapper.display().to_string());
        tools_override(None);
        settings_override(None);
    }

    #[test]
    fn a_suite_reads_its_command_and_class_from_the_project_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".zerus")).unwrap();
        std::fs::write(
            dir.path().join(".zerus/tests.toml"),
            "version = 1\n[budgets]\nsuite_minutes = 10\n[resources.small]\ncpu = 2\n[[provider]]\nname = \"echo\"\nkind = \"command\"\ncommand = \"echo {targets}\"\nresources = \"small\"\n",
        )
        .unwrap();
        let request = suite_from_config(dir.path(), None, "echo", &["a".into(), "b".into()]).unwrap();
        assert_eq!(request.command, "echo a b");
        assert_eq!(request.suite, "echo");
        assert!(request.budget_minutes > 0.0);
    }

    #[test]
    fn the_results_database_is_bounded_and_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let store = TestStore::open_at(&dir.path().join("tests.sqlite3"), false).unwrap();
        for index in 0..(RESULT_LIMIT + 2) {
            let record = Record {
                id: format!("r{index}"), team: "t".into(), suite: "s".into(), commit: "c".into(), input_hash: "h".into(),
                status: "passed".into(), duration_ms: 1, excerpt: String::new(), asked_by: "dev".into(), at: index as f64,
                affected: Vec::new(), reused_from: String::new(), flags: Vec::new(), due: false, recorded_ms: 0, task: None, runner: String::new(), report: None, performance: false, load: None,
            };
            store.write(&record).unwrap();
        }
        let count: i64 = store.db.query_row("SELECT COUNT(*) FROM results", [], |row| row.get(0)).unwrap();
        assert_eq!(count, RESULT_LIMIT);
        let indexed: i64 = store.db.query_row("SELECT COUNT(*) FROM pragma_index_list('results') WHERE name='results_input'", [], |row| row.get(0)).unwrap();
        assert_eq!(indexed, 1);
        let newest = store.results(Some("t"), None, 1, None, None).unwrap();
        assert_eq!(newest[0].id, format!("r{}", RESULT_LIMIT + 1));
    }
    /// Task #281: a performance run looks at the machine first: busy within the
    /// bound it waits, past the bound it runs marked loaded with the numbers,
    /// and a quiet machine runs normally with the load recorded.
    #[test]
    fn perf_runs_wait_for_a_quiet_machine_or_run_loaded_with_the_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = TestStore::open_at(&dir.path().join("tests.sqlite3"), false).unwrap();
        let policy = config::Performance { cpu_percent: 70, disk_percent: 90, builds: 1, wait_seconds: 600.0 };
        let busy = PerfLoad { cpu_percent: 92.0, disk_percent: 95.0, builds: 3, at: 0.0 };
        let quiet = PerfLoad { cpu_percent: 10.0, disk_percent: 20.0, builds: 0, at: 0.0 };
        assert!(matches!(perf_gate(&busy, &policy, 100.0, 200.0), PerfGate::Wait));
        assert!(matches!(perf_gate(&busy, &policy, 100.0, 800.0), PerfGate::Run(true)));
        assert!(matches!(perf_gate(&quiet, &policy, 100.0, 800.0), PerfGate::Run(false)));
        let mut perf = request("perf", "bench", 1.0, false, "small");
        perf.performance = true;
        perf.command = if cfg!(windows) { "exit /b 0".into() } else { "true".into() };
        perf.at = 1000.0;
        store.enqueue(&perf).unwrap();
        perf_load_override(Some(busy.clone()));
        let written = run_due(&store, dir.path(), &store.queued("t").unwrap(), 1000.0, &Default::default(), None).unwrap();
        assert!(written.is_empty(), "a busy machine waits");
        let waiting = store.pending("t").unwrap();
        assert_eq!(waiting.len(), 1);
        assert!(waiting[0].reason.contains("quiet machine"), "{}", waiting[0].reason);
        // Past the bounded wait it runs marked loaded, carrying the numbers.
        let written = run_due(&store, dir.path(), &store.queued("t").unwrap(), 1000.0 + policy.wait_seconds + 1.0, &Default::default(), None).unwrap();
        assert_eq!(written.len(), 1);
        assert!(written[0].performance && written[0].flags.contains(&"loaded".to_owned()), "{:?}", written[0].flags);
        let load = written[0].load.as_ref().expect("the load is recorded");
        assert_eq!(load["loaded"], serde_json::json!(true));
        assert_eq!(load["before"]["cpu_percent"], serde_json::json!(92.0));
        assert_eq!(load["during"]["builds"], serde_json::json!(3));
        // A quiet machine runs normally, still carrying the load.
        store.enqueue(&perf).unwrap();
        perf_load_override(Some(quiet));
        let written = run_due(&store, dir.path(), &store.queued("t").unwrap(), 100.0, &Default::default(), None).unwrap();
        assert_eq!(written.len(), 1);
        assert!(written[0].performance && !written[0].flags.contains(&"loaded".to_owned()), "{:?}", written[0].flags);
        assert_eq!(written[0].load.as_ref().unwrap()["before"]["cpu_percent"], serde_json::json!(10.0));
        perf_load_override(None);
    }

    /// Task #281: a comparison's two binaries alternate in rounds, never one
    /// binary's rounds back to back.
    #[test]
    fn comparisons_alternate_the_binaries_in_rounds() {
        let classes: Classes = [("small".to_owned(), Resources { cpu: 1, ram_mb: 0, gpu: false, exclusive: false })]
            .into_iter()
            .collect();
        let mut requests = Vec::new();
        for round in 0..2 {
            for suite in ["bench-new", "bench-old"] {
                let mut one = request(&format!("{suite}-{round}"), suite, 1.0, false, "small");
                one.compare = "lore".into();
                one.at = 10.0 + f64::from(round);
                requests.push(one);
            }
        }
        let load = Load { cpu_free: 8, ram_free_mb: 0, gpu_free: true, at_night: true };
        let chosen: Vec<String> = schedule(&requests, &classes, &load).into_iter().map(|request| request.id).collect();
        assert_eq!(chosen, ["bench-new-0", "bench-old-0", "bench-new-1", "bench-old-1"], "{chosen:?}");
    }

    /// Task #281: a bench or perf suite is a performance run without any flag.
    #[test]
    fn a_bench_or_perf_suite_is_marked_performance() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".zerus")).unwrap();
        std::fs::write(
            dir.path().join(".zerus/tests.toml"),
            "version = 1\n[[provider]]\nname = \"bench\"\nkind = \"command\"\naffected = \"all\"\ncommand = \"x\"\n",
        )
        .unwrap();
        let request = suite_from_config(dir.path(), None, "bench", &[]).unwrap();
        assert!(request.performance, "a bench suite is a performance run");
    }
}
