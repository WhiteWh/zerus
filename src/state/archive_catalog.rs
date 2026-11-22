//! Rebuildable metadata index. Archive JSON files remain authoritative; no journal
//! is opened until its row belongs to a requested page. Callers hold the state lock.
use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;

const MAX_DATE: u64 = 253_402_300_800;
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Query {
    #[serde(default)]
    pub from: u64,
    #[serde(default = "end_of_time")]
    pub until: u64,
    #[serde(default = "page_size")]
    pub limit: u64,
    #[serde(default)]
    pub cursor: String,
    #[serde(default)]
    pub query: String,
}
fn query_hash(query: &str) -> String {
    format!("{:x}", Sha256::digest(query.as_bytes()))
}
fn end_of_time() -> u64 {
    MAX_DATE
}
fn page_size() -> u64 {
    50
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    catalog: String,
    from: u64,
    until: u64,
    query: String,
    at: f64,
    id: String,
    #[serde(default)]
    newer: bool,
}

pub(crate) fn validate(value: &Value) -> Result<Query> {
    let q: Query = serde_json::from_value(value.clone()).map_err(|_| "invalid archive query")?;
    if q.from >= q.until
        || q.until > MAX_DATE
        || !(1..=50).contains(&q.limit)
        || q.cursor.len() > 1024
        || q.query.chars().count() > 256
        || q.query.chars().any(char::is_control)
    {
        return Err("invalid archive range, page size or search".into());
    }
    if !q.cursor.is_empty() {
        decode(&q)?;
    }
    Ok(q)
}
fn decode(q: &Query) -> Result<Cursor> {
    let bytes = URL_SAFE_NO_PAD
        .decode(&q.cursor)
        .map_err(|_| "invalid archive cursor")?;
    let c: Cursor = serde_json::from_slice(&bytes).map_err(|_| "invalid archive cursor")?;
    if c.version != 1
        || c.from != q.from
        || c.until != q.until
        || c.query != query_hash(&q.query)
        || !c.at.is_finite()
        || c.at < q.from as f64
        || c.at >= q.until as f64
        || uuid::Uuid::parse_str(&c.id).is_err()
    {
        return Err("archive cursor belongs to another query; refresh the archive".into());
    }
    Ok(c)
}
pub(super) fn stamp(path: &Path) -> Result<String> {
    let m = fs::metadata(path).map_err(|e| e.to_string())?;
    Ok(format!(
        "{}:{:?}",
        m.len(),
        m.modified().map_err(|e| e.to_string())?
    ))
}
pub(super) fn search_text(record: &Value) -> String {
    [
        "name",
        "archive_id",
        "agent",
        "conversation_id",
        "run_id",
        "cwd",
        "launch_dir",
        "title",
    ]
    .iter()
    .map(|key| {
        if *key == "title" {
            string(record, key).chars().take(1024).collect::<String>()
        } else {
            string(record, key).to_owned()
        }
    })
    .collect::<Vec<_>>()
    .join(" ")
    .to_lowercase()
}
fn sql<T>(r: rusqlite::Result<T>) -> Result<T> {
    r.map_err(|e| format!("archive metadata index: {e}"))
}

/// Task 193: one read-only catalog for a listing that must not write (owner
/// decision, 2026-11-07), pinned once for the whole listing. None when the
/// index is absent or not current: the reader then lists the record instead of
/// rebuilding the index.
pub(super) fn existing() -> Option<Catalog> {
    if refresh::directory().ok()? == "absent" {
        return None;
    }
    pin(refresh::existing().ok()?).ok()
}

#[cfg(all(test, unix))]
std::thread_local! {
    pub(super) static BEFORE_METADATA_READ: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
    static AFTER_PAGE_COUNT: std::cell::Cell<Option<fn()>> = const { std::cell::Cell::new(None) };
}

mod refresh;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use refresh::team_worker;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use refresh::team_worker_with_budget;
pub(crate) const INCOMPLETE: &str = refresh::INCOMPLETE;
pub(super) fn incomplete(error: &str) -> bool {
    error.starts_with(INCOMPLETE) || error.starts_with("archive metadata index:")
}

/// One pinned SQLite read view for an identity lookup batch. Exact positives
/// require native bodies; the first negative verifies complete compact coverage.
/// Callers hold the state writer lock and reuse that proof for later lookups.
pub(super) struct Catalog {
    db: Connection,
    directory: String,
    complete: std::cell::Cell<bool>,
}
impl std::ops::Deref for Catalog {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        &self.db
    }
}
impl Drop for Catalog {
    fn drop(&mut self) {
        let _ = self.db.execute_batch("ROLLBACK");
    }
}
fn pin(db: Connection) -> Result<Catalog> {
    pin_transaction(db, false)
}
fn pin_transaction(db: Connection, writing: bool) -> Result<Catalog> {
    sql(db.execute_batch(if writing {
        "BEGIN IMMEDIATE"
    } else {
        "BEGIN DEFERRED"
    }))?;
    if !refresh::ready(&db)? {
        return Err(refresh::INCOMPLETE.into());
    }
    Ok(Catalog {
        db,
        directory: refresh::directory()?,
        complete: std::cell::Cell::new(false),
    })
}
pub(super) fn catalog() -> Result<Catalog> {
    let result = refresh::catalog().and_then(pin);
    if result.is_err() {
        refresh::request();
    }
    result
}
pub(super) fn writer_catalog() -> Result<Catalog> {
    // Acquire SQLite's writer exclusion before publishing the native file,
    // so a concurrent worker cannot invalidate this transaction's snapshot.
    refresh::writer_catalog().and_then(|db| pin_transaction(db, true))
}
pub(super) fn published(db: Catalog, path: &Path) {
    if refresh::published(&db, path, db.complete.get()).is_err() {
        refresh::request();
    }
}
pub(super) fn before_removal(path: &Path) -> Option<Catalog> {
    if path.parent() != Some(root().join("archives").as_path())
        || !fs::symlink_metadata(path).ok()?.is_file()
    {
        return None;
    }
    let db = writer_catalog().ok()?;
    if refresh::proof(&db).ok()? != db.directory {
        return None;
    }
    db.complete.set(true);
    Some(db)
}
pub(super) fn removed(db: Catalog, path: &Path) {
    if refresh::removed(&db, path).is_err() {
        refresh::request();
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod teams;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use teams::{find_run, member_archive_pending, with_member_archive_probe};

fn current_proof(db: &Catalog) -> Result<()> {
    if refresh::directory()? != db.directory {
        return Err(refresh::INCOMPLETE.into());
    }
    Ok(())
}
pub(super) fn worker() -> Result<i32> {
    refresh::worker()
}
pub(super) fn contains(db: &Catalog, record: &Value) -> Result<bool> {
    find(db, record).map(|id| id.is_some())
}
/// Notification admission neither rebuilds nor schedules archive maintenance.
/// A missing or incomplete compact proof remains unknown.
pub(super) fn contains_existing(record: &Value) -> Result<bool> {
    if refresh::directory()? == "absent" {
        return Ok(false);
    }
    contains(&pin(refresh::existing()?)?, record)
}
pub(super) fn find(db: &Catalog, record: &Value) -> Result<Option<String>> {
    current_proof(db)?;
    use rusqlite::OptionalExtension;
    let id:Option<String>=sql(db.query_row("SELECT substr(id,1,65) FROM archive_identities WHERE valid=1 AND name=? AND run=? AND conversation=? ORDER BY id LIMIT 1",params![string(record,"name"),string(record,"run_id"),string(record,"conversation_id")],|r|r.get(0)).optional())?;
    if let Some(id) = id {
        // A selected positive has an exact native witness even when unrelated
        // archive metadata is unavailable. The cache alone grants nothing.
        let archived = archive::read_archive(string(record, "name"), &id)?;
        if !archive::equivalent(record, std::slice::from_ref(&archived)) {
            return Err("Archive identity changed; retain the saved binding for review".into());
        }
        return Ok(Some(id));
    }
    if !db.complete.get() {
        let directory = refresh::proof(db)?;
        if directory != db.directory {
            return Err(refresh::INCOMPLETE.into());
        }
        db.complete.set(true);
    }
    Ok(None)
}
/// Cheap invalidation for a bounded legacy preview; no archive file is opened.
pub(crate) fn revision() -> Result<String> {
    let directory = refresh::directory()?;
    let publication = root().join("archive-catalog-v1.sqlite3");
    let published = match fs::symlink_metadata(&publication) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => stamp(&publication)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        _ => return Err(INCOMPLETE.into()),
    };
    let legacy = format!("{directory}\n{published}");
    Ok(query_hash(&match dsh::native_archives::revision()? {
        Some(native) => format!("{legacy}\n{native}"),
        None => legacy,
    }))
}

pub(crate) fn read_query() -> Result<Value> {
    use std::io::Read;
    let mut bytes = Vec::new();
    io::stdin()
        .take(8193)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 8192 {
        return Err("archive query exceeds 8 KiB".into());
    }
    serde_json::from_slice(crate::state::without_bom(&bytes)).map_err(|_| "invalid archive query JSON".into())
}

pub(crate) fn validate_lookup(value: &Value) -> Result<(&str, &str)> {
    let fields = value.as_object().ok_or("invalid archive lookup")?;
    let name = string(value, "name");
    let id = string(value, "archive_id");
    if fields.len() != 2
        || name.is_empty()
        || name.len() > 2048
        || name.starts_with(['-', '@'])
        || name.contains('@')
        || name.chars().any(char::is_control)
        || uuid::Uuid::parse_str(id).is_err()
    {
        return Err("archive lookup requires an exact session name and archive ID".into());
    }
    Ok((name, id))
}

/// Recover a phone's exact target after a connector restart without listing
/// other archives, reading transcript journals or rebuilding the catalog.
pub(crate) fn lookup(value: &Value) -> Result<Value> { lookup_scoped(value,false) }
pub(crate) fn lookup_scoped(value: &Value, windows_mobile: bool) -> Result<Value> {
    let (name, id) = validate_lookup(value)?;
    let _guard = lock(None)?;
    if !name.starts_with("dsh/") {
        let path = archive::path(id)?;
        if !fs::symlink_metadata(&path).map_err(|e| e.to_string())?.file_type().is_file() {
            return Err("archive record is not a regular file".into());
        }
    }
    let record = archive::read_archive(name, id)?;
    let mut result=json!({"version":1,"name":name,"archive_id":id,
        "run_id":string(&record,"run_id"),"conversation_id":string(&record,"conversation_id")});
    if windows_mobile {
        if !supported_provider(&record["agent"]) { return Err("provider is unavailable on a Windows local gateway".into()); }
        result["cmd"]=record["agent"].clone();
    }
    Ok(result)
}
pub(crate) fn page(value: &Value) -> Result<Value> {
    page_with(value, false)
}

/// zerus-080n8: a bounded page (`hgs archives --json --bounded`) counts at
/// most this many matches; a larger total or position is sent as the cap with
/// `total_capped` or `position_capped` ("10000+").
pub(crate) const COUNT_CAP: u64 = 10_000;
/// zerus-080n8: each scan of a bounded page looks at most at this many catalog
/// rows in date order, so a rare search costs the same on any catalog size. A
/// page that reaches it before its limit of matches is `partial` and continues
/// from the last row it looked at.
pub(crate) const EXAMINE: u64 = 25_000;
/// The marker a CLI lists in its help once it serves bounded pages.
pub(crate) const BOUNDED_MARKER: &str = "archive-bounded-pages-v1";

#[derive(Clone, Copy, Debug)]
struct Bounds {
    count: u64,
    examine: u64,
}
const BOUNDS: Bounds = Bounds { count: COUNT_CAP, examine: EXAMINE };

/// A keyset point of the catalog: `(archived_at, archive_id)`.
type Point = (f64, String);
/// A listed row: `(archive_id, name, archived_at)`.
type Listed = (String, String, f64);

/// One bounded page before its summaries are read.
#[derive(Debug, Default, PartialEq)]
struct Window {
    /// Newest first.
    rows: Vec<Listed>,
    total: u64,
    total_capped: bool,
    position: u64,
    position_capped: bool,
    /// The scan reached its budget before `limit` matches.
    partial: bool,
    /// Rows the page's scan looked at.
    searched: u64,
    /// Where Older continues, exclusive.
    next: Option<Point>,
    /// Where Newer continues, exclusive.
    previous: Option<Point>,
}

/// Matches on one side of `point` (none: the whole range), counting at most
/// `b.count` of them and looking at most at `b.examine` rows: the count and
/// whether either bound stopped it, so the count is a lower bound.
fn bounded_count(db: &Connection, q: &Query, needle: &str, side: Option<(&Point, bool, bool)>, b: Bounds) -> Result<(u64, bool)> {
    // (newer side, inclusive of the point itself)
    let (clause, order) = match side {
        None => ("1", "at DESC,id DESC"),
        Some((_, true, false)) => ("(at>?3 OR (at=?3 AND id>?4))", "at ASC,id ASC"),
        Some((_, true, true)) => ("(at>?3 OR (at=?3 AND id>=?4))", "at ASC,id ASC"),
        Some((_, false, _)) => ("(at<?3 OR (at=?3 AND id<?4))", "at DESC,id DESC"),
    };
    let (at, id) = side.map_or((0.0, String::new()), |(point, ..)| (point.0, point.1.clone()));
    if needle.is_empty() {
        let n: u64 = sql(db.query_row(
            &format!("SELECT count(*) FROM (SELECT 1 FROM archive_rows WHERE at>=?1 AND at<?2 AND {clause} LIMIT ?5)"),
            params![q.from, q.until, at, id, b.count + 1],
            |r| r.get(0),
        ))?;
        return Ok((n.min(b.count), n > b.count));
    }
    let (n, looked): (u64, u64) = sql(db.query_row(
        &format!("SELECT (SELECT count(*) FROM (SELECT 1 FROM (SELECT search FROM archive_rows WHERE at>=?1 AND at<?2 AND {clause} ORDER BY {order} LIMIT ?6) WHERE instr(search,?7)>0 LIMIT ?5)),
                         (SELECT count(*) FROM (SELECT 1 FROM archive_rows WHERE at>=?1 AND at<?2 AND {clause} LIMIT ?6+1))"),
        params![q.from, q.until, at, id, b.count + 1, b.examine, needle],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ))?;
    Ok((n.min(b.count), n > b.count || looked > b.examine))
}

/// The bounded window of a page: one scan from the cursor in its direction,
/// at most `b.examine` rows, keeping the first `q.limit` matches.
fn bounded_window(db: &Connection, q: &Query, cursor: Option<&Cursor>, b: Bounds) -> Result<Window> {
    let needle = q.query.trim().to_lowercase();
    let newer = cursor.is_some_and(|c| c.newer);
    let point = cursor.map(|c| (c.at, c.id.clone()));
    let (total, total_capped) = bounded_count(db, q, &needle, None, b)?;
    #[cfg(all(test, unix))]
    AFTER_PAGE_COUNT.with(|slot| {
        if let Some(hook) = slot.take() {
            hook();
        }
    });
    let (clause, order) = match (point.is_some(), newer) {
        (false, _) => ("1", "at DESC,id DESC"),
        (true, true) => ("(at>?3 OR (at=?3 AND id>?4))", "at ASC,id ASC"),
        (true, false) => ("(at<?3 OR (at=?3 AND id<?4))", "at DESC,id DESC"),
    };
    let (at, id) = point.clone().unwrap_or((0.0, String::new()));
    let mut stmt = sql(db.prepare(&format!(
        "SELECT id,name,at,instr(search,?5)>0 FROM archive_rows WHERE at>=?1 AND at<?2 AND {clause} ORDER BY {order} LIMIT ?6"
    )))?;
    // One row past the budget tells whether rows remain after it.
    let mut scan = sql(stmt.query(params![q.from, q.until, at, id, needle, b.examine + 1]))?;
    let (mut rows, mut searched, mut last, mut more, mut stopped) = (Vec::new(), 0u64, None, false, false);
    // SQLite steps lazily: a page of ordinary matches stops after limit + 1.
    while let Some(row) = sql(scan.next())? {
        if searched == b.examine {
            stopped = true;
            break;
        }
        let listed: Listed = (sql(row.get(0))?, sql(row.get(1))?, sql(row.get(2))?);
        searched += 1;
        last = Some((listed.2, listed.0.clone()));
        if sql(row.get::<_, bool>(3))? {
            if rows.len() as u64 == q.limit {
                more = true;
                break;
            }
            rows.push(listed);
        }
    }
    let full = rows.len() as u64 == q.limit;
    // A full page stopped at the budget may have more: it continues from its last row.
    more |= stopped && full;
    let partial = stopped && !full;
    if newer {
        rows.reverse();
    }
    let edge = |row: &Listed| (row.2, row.0.clone());
    // F: the first page starts at the newest match; no position count.
    let (position, position_capped) = match (&point, rows.first()) {
        (None, _) => (0, false),
        (Some(_), Some(first)) => bounded_count(db, q, &needle, Some((&edge(first), true, false)), b)?,
        (Some(p), None) if !newer => bounded_count(db, q, &needle, Some((p, true, true)), b)?,
        (Some(_), None) => match &last {
            Some(looked) if partial => bounded_count(db, q, &needle, Some((looked, true, false)), b)?,
            _ => (0, false),
        },
    };
    let (next, previous) = if !newer {
        let next = if more { rows.last().map(edge) } else if partial { last.clone() } else { None };
        let previous = (position > 0 || position_capped).then(|| rows.first().map(edge).or_else(|| point.clone())).flatten();
        (next, previous)
    } else {
        let previous = if more { rows.first().map(edge) } else if partial { last.clone() } else { None };
        (rows.last().map(edge).or_else(|| last.clone()), previous)
    };
    // zerus-0g4rm: a capped total is a lower bound over the newest rows of the
    // range; a deeper page knows at least its position and rows.
    let total = if total_capped { total.max(position + rows.len() as u64) } else { total };
    Ok(Window { rows, total, total_capped, position, position_capped, partial, searched, next, previous })
}

/// A page cursor: the keyset point with the query's range and hash and the
/// catalog it belongs to.
fn encode_cursor(q: &Query, catalog: &str, at: f64, id: &str, newer: bool) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&Cursor {
            version: 1,
            catalog: catalog.to_owned(),
            from: q.from,
            until: q.until,
            query: query_hash(&q.query),
            at,
            id: id.to_owned(),
            newer,
        })
        .map_err(|e| e.to_string())?,
    ))
}

/// The archive records of listed rows: read under the catalog lock.
fn records_of(rows: &[Listed], limit: u64) -> Result<Vec<Value>> {
    rows.iter()
        .take(limit as usize)
        .map(|(id, name, _)| archive::read_archive(name, id))
        .collect::<Result<Vec<_>>>()
}

const SUMMARY_PAGE_BYTES: usize = 250_000;
/// One valid preference can exceed the ordinary page budget. Return that row
/// alone, leaving room below 1 MiB for cursors and query metadata.
const SINGLE_SUMMARY_BYTES: usize = 768 * 1024;

/// The page's bounded session summaries: read after the catalog lock is released.
fn sessions_of(
    rows: &[Listed],
    records: &[Value],
    marks: Option<&Value>,
    name_taken: Option<&dyn Fn(&str) -> bool>,
) -> Result<Vec<Value>> {
    let preferences = notifications::load();
    let mut summaries = Vec::new();
    for ((id, name, at), record) in rows.iter().zip(records) {
        let parts: Vec<_> = name.splitn(3, '/').collect();
        let mut row = match name_taken {
            Some(taken) => journal::archive_summary(record, taken(name)),
            None => journal::summary(record, false),
        };
        row.as_object_mut().ok_or("invalid archive summary")?.extend(json!({"name":name,"cmd":record["agent"],"project":parts.get(1),"tag":parts.get(2),"attached":0,"clients":[],"created":record["created"],"state":"archived","resumable":!string(record,"conversation_id").is_empty(),"archive_id":id,"archived_at":at}).as_object().unwrap().clone());
        if let Some(title) = record["title"].as_str() {
            row["title"] = json!(title.chars().take(1024).collect::<String>());
        }
        notifications::enrich(&mut row, preferences.as_ref(), journal::journal_name(record));
        if let Some(marks) = marks {
            read_marks::enrich_listing(&mut row, marks, journal::journal_name(record), Ok(Some(Vec::new())));
        }
        summaries.push(row);
    }
    workspace::enrich(&mut summaries, &BTreeMap::new());
    let mut sessions = Vec::new();
    let mut bytes = 0;
    for row in summaries {
        let size = serde_json::to_vec(&row).map_err(|e| e.to_string())?.len();
        if bytes + size > SUMMARY_PAGE_BYTES {
            if sessions.is_empty() {
                if size <= SINGLE_SUMMARY_BYTES {
                    sessions.push(row);
                    break;
                }
                return Err("archived session summary exceeds page limit".into());
            }
            break;
        }
        bytes += size;
        sessions.push(row);
    }
    Ok(sessions)
}

/// One archive page; `bounded` (zerus-080n8) caps its counts and scans, and
/// without it the page is exactly what older clients read.
pub(crate) fn page_with(value: &Value, bounded: bool) -> Result<Value> {
    page_with_bounds(value, bounded.then_some(BOUNDS), None, None, true)
}

/// Plain `ls` retains a recent archive window. Even its count reads only one
/// row past that window; older rows are reached through the ordinary cursor.
pub(super) fn preview(marks: &Value, name_taken: Option<&dyn Fn(&str) -> bool>) -> Result<Value> {
    let mut page = match page_with_bounds(
        &json!({}),
        Some(Bounds {
            count: 50,
            examine: 51,
        }),
        Some(marks),
        name_taken,
        true,
    ) {
        Ok(page) => page,
        Err(error) => {
            json!({"version":1,"sessions":[],"next_cursor":"","previous_cursor":"","position":0,"total":0,
            "total_capped":true,"position_capped":true,"partial":true,"searched":0,"catalog_complete":false,"error":error,
            "from":0,"until":MAX_DATE,"query":""})
        }
    };
    page["command"] = json!(["archives", "--json", "--bounded"]);
    Ok(page)
}

fn page_with_bounds(
    value: &Value,
    bounds: Option<Bounds>,
    marks: Option<&Value>,
    name_taken: Option<&dyn Fn(&str) -> bool>,
    include_native: bool,
) -> Result<Value> {
    let q = validate(value)?;
    let _guard = lock(None)?;
    let db = catalog()?;
    let search_complete = refresh::search_complete(&db)?;
    let catalog: String = sql(db.query_row(
        "SELECT value FROM catalog_meta WHERE key='id'",
        [],
        |r| r.get(0),
    ))?;
    let catalog = if include_native {
        match dsh::native_archives::attach(&db)? {
            Some(id) if db.path().is_none_or(str::is_empty) => id,
            Some(id) => query_hash(&format!("{catalog}\n{id}")),
            None => catalog,
        }
    } else { catalog };
    let cursor = if q.cursor.is_empty() {
        None
    } else {
        Some(decode(&q)?)
    };
    if cursor.as_ref().is_some_and(|c| c.catalog != catalog) {
        return Err(
            "archive cursor belongs to another computer or rebuilt catalog; refresh".into(),
        );
    }
    if let Some(bounds) = bounds {
        let w = bounded_window(&db, &q, cursor.as_ref(), bounds)?;
        let newer = cursor.as_ref().is_some_and(|c| c.newer);
        // zerus-m0boz: the byte limit keeps the rows nearest the cursor, the
        // newest going Older and the oldest going Newer, so the next page in
        // the same direction goes on from the last one sent.
        let nearest: Vec<Listed> = if newer {
            w.rows.iter().rev().cloned().collect()
        } else {
            w.rows.clone()
        };
        let records = records_of(&nearest, q.limit)?;
        drop(db);
        drop(_guard); // Journal summaries never block live hooks or lifecycle writers.
        let mut sessions = sessions_of(&nearest, &records, marks, name_taken)?;
        let encode = |at: f64, id: &str, newer: bool| encode_cursor(&q, &catalog, at, id, newer);
        let (mut next, mut previous, mut position) =
            (w.next.clone(), w.previous.clone(), w.position);
        if !sessions.is_empty() && sessions.len() < nearest.len() {
            let (id, _, at) = &nearest[sessions.len() - 1];
            if newer {
                // The newer rows left out are matches newer than every row sent.
                previous = Some((*at, id.clone()));
                position += (nearest.len() - sessions.len()) as u64;
            } else {
                next = Some((*at, id.clone()));
            }
        }
        if newer {
            sessions.reverse();
        }
        let next = next
            .map(|(at, id)| encode(at, &id, false))
            .transpose()?
            .unwrap_or_default();
        let previous = previous
            .as_ref()
            .map(|(at, id)| encode(*at, id, true))
            .transpose()?
            .unwrap_or_default();
        return Ok(
            json!({"version":1,"sessions":sessions,"next_cursor":next,"previous_cursor":previous,"position":position,"total":w.total,
            "total_capped":w.total_capped || !search_complete,"position_capped":w.position_capped || !search_complete,"partial":w.partial || !search_complete,"searched":w.searched,
            "from":q.from,"until":q.until,"query":q.query}),
        );
    }
    let at = cursor.as_ref().map_or(MAX_DATE as f64, |c| c.at);
    let id = cursor.as_ref().map_or("", |c| c.id.as_str());
    let needle = q.query.trim().to_lowercase();
    let total: u64 = sql(db.query_row(
        "SELECT count(*) FROM archive_rows WHERE at>=? AND at<? AND instr(search,?)>0",
        params![q.from, q.until, needle],
        |r| r.get(0),
    ))?;
    #[cfg(all(test, unix))]
    AFTER_PAGE_COUNT.with(|slot| {
        if let Some(hook) = slot.take() {
            hook();
        }
    });
    let newer = cursor.as_ref().is_some_and(|c| c.newer);
    let mut stmt=sql(db.prepare(if newer {
        "SELECT id,name,at FROM archive_rows WHERE at>=? AND at<? AND instr(search,?)>0 AND (at>? OR (at=? AND id>?)) ORDER BY at ASC,id ASC LIMIT ?"
    } else {
        "SELECT id,name,at FROM archive_rows WHERE at>=? AND at<? AND instr(search,?)>0 AND (at<? OR (at=? AND id<?)) ORDER BY at DESC,id DESC LIMIT ?"
    }))?;
    let rows = sql(
        stmt.query_map(params![q.from, q.until, needle, at, at, id, q.limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
            ))
        }),
    )?;
    let mut rows = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);
    if newer {
        rows.reverse();
    }
    let position: u64 = if let Some((first_id, _, first_at)) = rows.first() {
        sql(db.query_row("SELECT count(*) FROM archive_rows WHERE at>=? AND at<? AND instr(search,?)>0 AND (at>? OR (at=? AND id>?))",
            params![q.from,q.until,needle,first_at,first_at,first_id],|r|r.get(0)))?
    } else {
        total
    };
    let records = records_of(&rows, q.limit)?;
    drop(db);
    drop(_guard); // Journal summaries never block live hooks or lifecycle writers.
    let sessions = sessions_of(&rows, &records, marks, name_taken)?;
    let encode = |at: f64, id: &str, newer: bool| -> Result<String> {
        Ok(URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&Cursor {
                version: 1,
                catalog: catalog.clone(),
                from: q.from,
                until: q.until,
                query: query_hash(&q.query),
                at,
                id: id.to_owned(),
                newer,
            })
            .map_err(|e| e.to_string())?,
        ))
    };
    let next = if !sessions.is_empty() && position + (sessions.len() as u64) < total {
        let (id, _, at) = &rows[sessions.len() - 1];
        encode(*at, id, false)?
    } else {
        String::new()
    };
    let previous = if position > 0 {
        if let Some((id, _, at)) = rows.first() {
            encode(*at, id, true)?
        } else if let Some(c) = cursor {
            encode(c.at, &c.id, true)?
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    let mut result = json!({"version":1,"sessions":sessions,"next_cursor":next,"previous_cursor":previous,"position":position,"total":total,"from":q.from,"until":q.until,"query":q.query});
    if !search_complete {
        result["total_capped"] = json!(true);
        result["position_capped"] = json!(true);
        result["partial"] = json!(true);
        result["catalog_complete"] = json!(false);
    }
    Ok(result)
}

fn supported_provider(provider: &Value) -> bool {
    matches!(provider.as_str(),Some("codex"|"claude"|"kimi"|"qwen"))
}
/// The authoritative archive reader already rejects providers outside AGENTS
/// before indexing/counting. Keep its bounded totals and cursors unchanged.
/// Refuse an inconsistent page instead of guessing a filtered total.
pub(crate) fn page_scoped(value: &Value, windows_mobile: bool) -> Result<Value> {
    let result = if windows_mobile {
        page_with_bounds(value, None, None, None, false)?
    } else {
        page(value)?
    };
    if windows_mobile && !result["sessions"].as_array().is_some_and(|rows|
        rows.iter().all(|row|supported_provider(&row["cmd"]))) {
        return Err("archive provider scope is unavailable".into());
    }
    Ok(result)
}

#[cfg(all(test, unix))]
mod refresh_tests;

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: Bounds = Bounds { count: 3, examine: 5 };

    fn id(n: u32) -> String {
        uuid::Uuid::from_u128(u128::from(n) + 1).to_string()
    }
    /// A private in-memory catalog: row n is archived at 1000 + n; `hits` hold "needle".
    fn catalog_of(rows: u32, hits: &[u32]) -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE archive_rows(id TEXT PRIMARY KEY,name TEXT NOT NULL,run TEXT NOT NULL,conversation TEXT NOT NULL,at REAL NOT NULL,search TEXT NOT NULL,stamp TEXT NOT NULL,seen TEXT NOT NULL);
            CREATE INDEX archive_by_date ON archive_rows(at DESC,id DESC);").unwrap();
        for n in 0..rows {
            let search = if hits.contains(&n) { format!("codex/p/s{n} needle") } else { format!("codex/p/s{n} other") };
            db.execute("INSERT INTO archive_rows VALUES(?1,?2,'r','c',?3,?4,'s','g')", params![id(n), format!("codex/p/s{n}"), 1000.0 + f64::from(n), search]).unwrap();
        }
        db
    }
    fn query(needle: &str, limit: u64) -> Query {
        Query { from: 0, until: MAX_DATE, limit, cursor: String::new(), query: needle.into() }
    }
    fn at(n: u32) -> Point {
        (1000.0 + f64::from(n), id(n))
    }
    fn cursor(point: &Point, newer: bool) -> Cursor {
        Cursor { version: 1, catalog: "c".into(), from: 0, until: MAX_DATE, query: String::new(), at: point.0, id: point.1.clone(), newer }
    }
    fn names(w: &Window) -> Vec<String> {
        w.rows.iter().map(|(_, name, _)| name.clone()).collect()
    }

    #[test]
    fn counts_stop_at_the_cap_for_every_query_and_report_it() {
        let db = catalog_of(4, &[0, 1, 2, 3]);
        // B: the empty query is capped too.
        let w = bounded_window(&db, &query("", 2), None, SMALL).unwrap();
        assert_eq!((w.total, w.total_capped), (3, true));
        let w = bounded_window(&db, &query("needle", 2), None, SMALL).unwrap();
        assert_eq!((w.total, w.total_capped), (3, true));
        let exact = bounded_window(&db, &query("needle", 2), None, Bounds { count: 10, examine: 10 }).unwrap();
        assert_eq!((exact.total, exact.total_capped), (4, false));
        assert_eq!(names(&exact), ["codex/p/s3", "codex/p/s2"]);
        assert!(exact.next.is_some() && !exact.partial);
        // F: the first page starts at position 0 without counting newer rows.
        assert_eq!((exact.position, exact.position_capped, exact.previous.clone()), (0, false, None));
    }

    #[test]
    fn a_rare_match_past_the_budget_is_reached_by_partial_continuations() {
        let db = catalog_of(12, &[1]);
        let first = bounded_window(&db, &query("needle", 2), None, SMALL).unwrap();
        assert!(first.rows.is_empty() && first.partial);
        assert_eq!(first.searched, 5);
        // Five newest rows looked at, none matched: a lower bound, never an exact "0".
        assert_eq!((first.total, first.total_capped), (0, true));
        assert_eq!(first.next, Some(at(7)));
        let second = bounded_window(&db, &query("needle", 2), Some(&cursor(&first.next.clone().unwrap(), false)), SMALL).unwrap();
        assert!(second.rows.is_empty() && second.partial);
        assert_eq!(second.next, Some(at(2)));
        let third = bounded_window(&db, &query("needle", 2), Some(&cursor(&second.next.clone().unwrap(), false)), SMALL).unwrap();
        assert_eq!(names(&third), ["codex/p/s1"]);
        assert!(!third.partial && third.next.is_none());
        assert_eq!(third.searched, 2);
        // zerus-0g4rm: a capped total is a lower bound, never below the rows this page found.
        assert!(third.total_capped && third.total >= third.position + third.rows.len() as u64);
        // Nothing newer matches, within the budget looked at.
        assert_eq!((third.position, third.previous.is_some()), (0, third.position_capped));
    }

    #[test]
    fn newer_continues_partially_too() {
        let db = catalog_of(12, &[10, 0]);
        // From an Older cursor at row 1, Newer looks at rows 2..6, then 7..11.
        let first = bounded_window(&db, &query("needle", 2), Some(&cursor(&at(1), true)), SMALL).unwrap();
        assert!(first.rows.is_empty() && first.partial);
        assert_eq!(first.previous, Some(at(6)));
        // Older from an empty Newer page continues below what it looked at.
        assert_eq!(first.next, Some(at(6)));
        let second = bounded_window(&db, &query("needle", 2), Some(&cursor(&at(6), true)), SMALL).unwrap();
        assert_eq!(names(&second), ["codex/p/s10"]);
        assert!(!second.partial && second.previous.is_none());
        assert_eq!(second.next, Some(at(10)));
    }

    #[test]
    fn a_cursor_at_a_row_looked_at_but_not_sent_validates_and_continues() {
        // G: the continuation point is an ordinary (at, id) keyset cursor.
        let db = catalog_of(12, &[1]);
        let q = query("needle", 2);
        let first = bounded_window(&db, &q, None, SMALL).unwrap();
        let (point_at, point_id) = first.next.clone().unwrap();
        let text = encode_cursor(&q, "catalog-1", point_at, &point_id, false).unwrap();
        let resumed = Query { cursor: text, ..q.clone() };
        let decoded = decode(&resumed).unwrap();
        assert_eq!((decoded.at, decoded.id.as_str(), decoded.newer, decoded.catalog.as_str()), (point_at, point_id.as_str(), false, "catalog-1"));
        assert!(bounded_window(&db, &q, Some(&decoded), SMALL).unwrap().partial);
        // Another query's hash still refuses it.
        assert!(decode(&Query { query: "other".into(), ..resumed }).is_err());
    }

    #[test]
    fn a_full_page_stopped_by_the_budget_continues_from_its_last_row() {
        let db = catalog_of(12, &[11, 10, 9, 8, 7]);
        let w = bounded_window(&db, &query("needle", 5), None, SMALL).unwrap();
        assert_eq!(w.rows.len(), 5);
        assert!(!w.partial);
        assert_eq!(w.next, Some(at(7)));
    }
}
