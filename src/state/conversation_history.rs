//! Bounded public history indexing. Source files remain native-authoritative;
//! derived files contain only public message projections, never raw transcripts.
use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom};

const PUBLIC: &[&str] = &[
    "UserPromptSubmit",
    "UserPromptQueued",
    "UserMessage",
    "TurnStarted",
    "QuestionAnswered",
    "AgentMessage",
    "Stop",
];
const BUDGET: usize = 2 * 1024 * 1024;
// Routine reads stay small, but one native JSONL row can contain an image or
// a large private tool result. Finish that row within an explicit memory cap
// before deciding whether it contributes a public message.
const SOURCE_LINE_BYTES: usize = 16 * 1024 * 1024;
const SOURCE_READER_VERSION: &str = "2";
const ROW_BYTES: usize = 32 * 1024;
const RECONCILE_VERSION: &str = "3";
// Stored provider rows follow the current public projection (zerus-e9qx).
const PROJECTION_VERSION: &str = "1";
const PROJECTION_ROWS: usize = 64;
// A call stops before another row once this much transcript was read. Each row's
// line is read whole, up to SOURCE_LINE_BYTES, so one call reads under 20 MiB.
const PROJECTION_BYTES: usize = 4 * 1024 * 1024;
// The projection change ledger's bound; at the bound the upgrade changes nothing.
const LEDGER_ENTRIES: i64 = 100_000;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    request_id: String,
    expected_run_id: String,
    expected_conversation_id: String,
    #[serde(default)]
    archive_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    before: Option<String>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    around: Option<String>,
    #[serde(default)]
    around_incoming_seq: Option<i64>,
    #[serde(default)]
    history_epoch: Option<String>,
}
/// A `history-projection` request: the history identity, its exact epoch and a
/// generation range. Integers only: a float, string or boolean is refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectionRequest {
    request_id: String,
    expected_run_id: String,
    expected_conversation_id: String,
    #[serde(default)]
    archive_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    history_epoch: String,
    projection_after: i64,
    projection_until: i64,
}
const PROJECTION_ENTRIES: usize = 256;
const INDEX_NOT_READY: &str = "History index is not ready; open its history first";
const PROJECTION_RESPONSE_BYTES: usize = 880 * 1024;
fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn validate(r: &Request) -> Result<()> {
    let id = uuid::Uuid::parse_str(&r.request_id).map_err(|_| "Invalid history request UUID")?;
    if id.is_nil()
        || id.to_string() != r.request_id
        || r.expected_run_id.is_empty()
        || r.expected_run_id.len() > 128
        || r.expected_conversation_id.is_empty()
        || r.expected_conversation_id.len() > 256
        || [&r.expected_run_id, &r.expected_conversation_id]
            .iter()
            .any(|v| v.chars().any(char::is_control))
    {
        return Err("Invalid exact history identity".into());
    }
    if !matches!(r.limit.unwrap_or(100), 1..=100)
        || [
            r.before.is_some(),
            r.after.is_some(),
            r.around.is_some(),
            r.around_incoming_seq.is_some(),
        ]
        .into_iter()
        .filter(|v| *v)
        .count()
            > 1
    {
        return Err("Choose one bounded history selector".into());
    }
    for v in [&r.before, &r.after, &r.around].into_iter().flatten() {
        if v.is_empty() || v.len() > 2048 {
            return Err("Invalid opaque history cursor".into());
        }
    }
    if r.around_incoming_seq.is_some_and(|v| v < 0)
        || r.around_incoming_seq.is_some() != r.history_epoch.is_some()
    {
        return Err("Incoming sequence requires its exact history epoch".into());
    }
    if let Some(v) = &r.agent_id {
        if v.is_empty()
            || v.len() > 160
            || v.contains(['/', '\\'])
            || v.chars().any(char::is_control)
        {
            return Err("Invalid child history identity".into());
        }
    }
    if let Some(v) = &r.archive_id {
        if uuid::Uuid::parse_str(v)
            .map(|id| id.to_string() != *v || id.is_nil())
            .unwrap_or(true)
        {
            return Err("Invalid archive identity".into());
        }
    }
    Ok(())
}
fn record(name: &str, r: &Request) -> Result<Value> {
    // zerus-ucck P3c: the request's identities select among sessions that
    // share the label.
    expect(name, &r.expected_run_id, &r.expected_conversation_id)?;
    let value = if opencode::exists(name) {
        if r.archive_id.is_some() {return Err("OpenCode has no archive history".into());} opencode::binding(name)?
    } else if dsh::exists(name) {
        if r.archive_id.is_some() {
            return Err("DeepSeek has no archive history".into());
        }
        dsh::binding(name)?
    } else {
        let _guard = lock(None)?;
        if let Some(id) = &r.archive_id {
            archive::read_archive(name, id)?
        } else {
            read(name)?
        }
    };
    if string(&value, "name") != name
        || string(&value, "run_id") != r.expected_run_id
        || string(&value, "conversation_id") != r.expected_conversation_id
        || (r.archive_id.is_none() && !string(&value, "archive_id").is_empty())
    {
        return Err("History target changed; refresh its exact identity".into());
    }
    if let Some(id) = &r.agent_id {
        if !value["subagents"].get(id).is_some_and(Value::is_object)
            && !["dsh", "opencode"].contains(&string(&value, "agent"))
        {
            return Err("Child is not in this exact parent conversation".into());
        }
    }
    Ok(value)
}
fn native_record(parent: &Value, r: &Request) -> Value {
    let mut result = parent.clone();
    if let Some(id) = &r.agent_id {
        let child = &parent["subagents"][id];
        if child["external"] == true {
            for (to, from) in [
                ("agent", "provider"),
                ("conversation_id", "conversation_id"),
                ("transcript", "transcript"),
                ("cwd", "cwd"),
                ("agent_home", "agent_home"),
            ] {
                result[to] = child[from].clone();
            }
        }
    }
    result
}
fn key(parent: &Value, r: &Request) -> String {
    hash(
        &json!([
            parent["agent"],
            parent["agent_home"],
            parent["account_id"],
            parent["conversation_id"],
            r.agent_id
        ])
        .to_string(),
    )
}
fn index(parent: &Value, r: &Request) -> Result<(Connection, Guard)> {
    index_in(&absolute_root()?.join("public_history"), parent, r)
}
/// The history index tables and indexes.
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS messages(id TEXT PRIMARY KEY,at REAL NOT NULL,role TEXT NOT NULL,stream TEXT NOT NULL,kind TEXT NOT NULL,ordinal TEXT NOT NULL,body TEXT NOT NULL,incoming INTEGER);
 CREATE INDEX IF NOT EXISTS message_order ON messages(at,ordinal,id); CREATE INDEX IF NOT EXISTS incoming_order ON messages(incoming) WHERE incoming IS NOT NULL;
 CREATE INDEX IF NOT EXISTS message_candidates ON messages(role,at);
 CREATE TABLE IF NOT EXISTS aliases(original TEXT PRIMARY KEY,id TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS receipt_seen(name TEXT PRIMARY KEY,stamp TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS reconcile_work(id TEXT PRIMARY KEY);
 CREATE TABLE IF NOT EXISTS projection_changes(generation INTEGER PRIMARY KEY AUTOINCREMENT,epoch TEXT NOT NULL,id TEXT NOT NULL,change TEXT NOT NULL CHECK(change IN ('retired','refreshed')));
 CREATE INDEX IF NOT EXISTS attachment_candidates ON messages(role,json_extract(body,'$.native_match_hash'));";
/// Opens, creating or migrating, the private history index of `parent` in `directory`.
fn index_in(directory: &Path, parent: &Value, r: &Request) -> Result<(Connection, Guard)> {
    private_dir(directory)?;
    if !crate::platform::verify_private(directory, crate::platform::Private::Directory)
        .map_err(|e| e.to_string())?
    {
        return Err("Public history index directory must be private".into());
    }
    let key = key(parent, r);
    let guard = lock(Some(&directory.join(format!("{key}.lock"))))?;
    let path = directory.join(format!("{key}.sqlite3"));
    let file = crate::platform::refuse_link(crate::platform::private_open(
        OpenOptions::new().create(true).read(true).write(true),
    ))
    .open(&path)
    .map_err(|e| e.to_string())?;
    if !crate::platform::verify_private_file(&file).map_err(|e| e.to_string())? {
        return Err("Public history index file must be private".into());
    }
    drop(file);
    let db = Connection::open(path).map_err(|e| e.to_string())?;
    db.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|e| e.to_string())?;
    db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;")
        .map_err(|e| e.to_string())?;
    db.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
    if get(&db, "schema")?.as_deref() != Some("2") {
        let has = db
            .prepare("PRAGMA table_info(messages)")
            .map_err(|e| e.to_string())?
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(|e| e.to_string())?
            .filter_map(std::result::Result::ok)
            .any(|v| v == "ordinal");
        if !has {
            db.execute_batch("ALTER TABLE messages ADD COLUMN ordinal TEXT NOT NULL DEFAULT ''; ")
                .map_err(|e| e.to_string())?;
        }
        db.execute_batch("DELETE FROM messages;DELETE FROM aliases;DELETE FROM receipt_seen;DELETE FROM reconcile_work;DELETE FROM projection_changes;DELETE FROM meta;").map_err(|e|e.to_string())?;
        set(&db, "schema", "2")?;
    }
    if get(&db, "epoch")?.is_none() {
        // A new index has no old projections to migrate.
        set(&db, "reconcile_version", RECONCILE_VERSION)?;
        set(&db, "projection_version", PROJECTION_VERSION)?;
        set(&db, "epoch", &uuid::Uuid::new_v4().to_string())?;
        set(
            &db,
            "secret",
            &(uuid::Uuid::new_v4().simple().to_string()
                + &uuid::Uuid::new_v4().simple().to_string()),
        )?;
    }
    Ok((db, guard))
}
fn get(db: &Connection, key: &str) -> Result<Option<String>> {
    db.query_row("SELECT value FROM meta WHERE key=?", [key], |row| {
        row.get(0)
    })
    .optional()
    .map_err(|e| e.to_string())
}
fn set(db: &Connection, key: &str, value: &str) -> Result<()> {
    db.execute("INSERT INTO meta(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value]).map_err(|e|e.to_string())?;
    Ok(())
}
fn original(event: &Value, stream: &str) -> Option<String> {
    let kind = string(event, "type");
    let role = if kind == "QuestionAnswered" {
        "You (answer)"
    } else if [
        "UserPromptSubmit",
        "UserPromptQueued",
        "UserMessage",
        "TurnStarted",
    ]
    .contains(&kind)
    {
        "You"
    } else {
        "AgentMessage"
    };
    let source = if string(event, "source").is_empty() {
        stream
    } else {
        string(event, "source")
    };
    let id = event
        .get("message_id")
        .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
        .or_else(|| event.get("seq"))
        .or_else(|| event.get("source_offset"))?;
    let text = if let Some(v) = id.as_str() {
        v.to_owned()
    } else {
        id.to_string()
    };
    Some(format!("{stream}:{source}:{role}:native:{text}"))
}
fn add(db: &Connection, canonical: &str, mut row: Value, stream: &str) -> Result<()> {
    if !PUBLIC.contains(&string(&row, "type"))
        || (string(&row, "detail").trim().is_empty()
            && row["attachments"].as_array().is_none_or(Vec::is_empty))
    {
        return Ok(());
    }
    let detail = string(&row, "detail").to_owned();
    let native_match_hash = if stream != "attachment" && stream != "voice" && stream != "inherited"
        && ["UserPromptSubmit", "UserPromptQueued", "UserMessage", "TurnStarted"].contains(&string(&row, "type"))
    { Some(hash(&detail)) } else { None };
    if detail.len() > ROW_BYTES {
        let mut end = ROW_BYTES;
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        row["detail"] = json!(&detail[..end]);
        row["detail_truncated"] = json!(true);
    }
    let mut public = json!({});
    for field in [
        "type",
        "detail",
        "detail_truncated",
        "source",
        "at",
        "seq",
        "message_id",
        "agent_id",
        "reply_id",
        "message_phase",
        "request_id",
        "question_id",
        "question_hash",
        "answers",
        "attachments",
        "submitted_text",
        "source_offset",
        "native_match_hash",
        "voice",
        "inherited_from",
    ] {
        if let Some(v) = row.get(field) {
            public[field] = v.clone();
        }
    }
    row = public;
    if let Some(digest) = native_match_hash {
        row["native_match_hash"] = json!(digest);
    }
    if string(&row, "message_id").len() > 256 {
        row.as_object_mut().unwrap().remove("message_id");
    }
    let incoming = ["AgentMessage", "Stop"].contains(&string(&row, "type"));
    // A fork's inherited rows are read-only history: never incoming (read
    // marks) and never outgoing (voice and attachment matching).
    let role = if stream == "inherited" {
        "inherited"
    } else if incoming {
        "incoming"
    } else {
        "outgoing"
    };
    let origin = original(&row, stream).ok_or("Public row lacks stable provenance")?;
    let existing: Option<String> = db
        .query_row("SELECT id FROM aliases WHERE original=?", [&origin], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some(id) = existing {
        // Native provenance aliases can point at a permanent media card. A
        // provider refresh must not replace its receipt with a text-only row.
        let owner: String = db.query_row("SELECT stream FROM messages WHERE id=?", [&id], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if (owner == "voice" && stream != "voice") || (owner == "attachment" && stream != "attachment") { return Ok(()); }
        if stream == "provider" || stream == "attachment" || stream == "voice" {
            let old_body: String = db
                .query_row(
                    "SELECT body FROM messages WHERE id=?",
                    [&id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            let encoded = row.to_string();
            let used = get(db, "bytes")?
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            let updated = used.saturating_sub(old_body.len()) + encoded.len();
            if updated > 512 * 1024 * 1024 {
                set(db, "gap", "index_budget_exhausted")?;
                return Ok(());
            }
            set(db, "bytes", &updated.to_string())?;
            db.execute(
                "UPDATE messages SET body=?,kind=? WHERE id=?",
                params![row.to_string(), string(&row, "type"), id],
            )
            .map_err(|e| e.to_string())?;
            if stream != "voice" && role == "outgoing" && old_body != encoded {
                // Revisit old unbound voices when native evidence changes.
                // Repeated unchanged provider windows must not starve a sweep.
                advance_voice_generation(db)?;
            }
            queue(db, &id, at_of(&row))?;
        }
        return Ok(());
    }
    let id = hash(&json!([canonical, origin]).to_string());
    let at = row["at"].as_f64().filter(|v| v.is_finite()).unwrap_or(0.);
    let ordinal = if stream == "journal" || stream == "inherited" {
        format!("0:{:020}", row["seq"].as_i64().unwrap_or(0))
    } else if stream == "provider" {
        let offset = string(&row, "source_offset");
        let (file, byte) = offset.split_once(':').unwrap_or(("0", "0"));
        format!(
            "1:{:020}:{:020}",
            file.parse::<u64>().unwrap_or(0),
            byte.parse::<u64>().unwrap_or(0)
        )
    } else {
        format!("2:{}", string(&row, "message_id"))
    };
    let count: i64 = db
        .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    let used = get(db, "bytes")?
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let cost = row.to_string().len() + origin.len() + 256;
    if count >= 100000 || used + cost > 512 * 1024 * 1024 {
        set(db, "gap", "index_budget_exhausted")?;
        return Ok(());
    }
    db.execute(
        "INSERT OR IGNORE INTO messages(id,at,role,stream,kind,ordinal,body) VALUES(?,?,?,?,?,?,?)",
        params![
            id,
            at,
            role,
            stream,
            string(&row, "type"),
            ordinal,
            row.to_string()
        ],
    )
    .map_err(|e| e.to_string())?;
    db.execute(
        "INSERT OR IGNORE INTO aliases(original,id) VALUES(?,?)",
        params![origin, id],
    )
    .map_err(|e| e.to_string())?;
    set(db, "bytes", &(used + cost).to_string())?;
    if stream != "voice" && role == "outgoing" { advance_voice_generation(db)?; }
    // Reconciliation matches a session's own rows; inherited rows never do.
    if stream != "inherited" {
        queue(db, &id, at)?;
    }
    Ok(())
}
fn advance_voice_generation(db: &Connection) -> Result<()> {
    db.execute("INSERT INTO meta(key,value) VALUES('voice_native_generation','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1",[])
        .map_err(|e|e.to_string())?;
    Ok(())
}
fn at_of(value: &Value) -> f64 {
    value["at"].as_f64().filter(|v| v.is_finite()).unwrap_or(0.)
}
fn queue(db: &Connection, id: &str, at: f64) -> Result<()> {
    db.execute("INSERT OR IGNORE INTO reconcile_work(id) VALUES(?)", [id])
        .map_err(|e| e.to_string())?;
    db.execute("INSERT OR IGNORE INTO reconcile_work(id) SELECT id FROM messages WHERE stream IN ('provider','attachment') AND at BETWEEN ? AND ?",params![at-10.,at+10.]).map_err(|e|e.to_string())?;
    // A queued native acknowledgement may arrive minutes after terminal input.
    // The receipt's exact expanded text includes its immutable staged paths.
    queue_attachment(db, id)?;
    Ok(())
}
fn queue_attachment(db: &Connection, id: &str) -> Result<()> {
    db.execute("INSERT OR IGNORE INTO reconcile_work(id) SELECT candidate.id FROM messages candidate JOIN messages added ON added.id=? WHERE candidate.role='outgoing' AND candidate.stream='attachment' AND candidate.id<>added.id AND json_extract(candidate.body,'$.native_match_hash')=json_extract(added.body,'$.native_match_hash')", [id]).map_err(|e|e.to_string())?;
    Ok(())
}
fn paths(record: &Value) -> Result<Vec<PathBuf>> {
    if string(record, "agent") == "kimi" {
        return questions::wire_paths(record);
    }
    let id = string(record, "conversation_id");
    let path = PathBuf::from(string(record, "transcript"));
    if !["codex", "claude"].contains(&string(record, "agent"))
        || uuid::Uuid::parse_str(id).is_err()
        || !path.is_absolute()
        || !path
            .file_name()
            .is_some_and(|v| v.to_string_lossy().contains(id))
    {
        return Ok(vec![]);
    }
    Ok(vec![path])
}
fn source_chunk(file: &mut File, budget: usize, extend_row: bool) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    (&mut *file).take(budget as u64).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if extend_row && budget > 0 && bytes.len() == budget && !bytes.contains(&b'\n') {
        BufReader::new((&mut *file).take((SOURCE_LINE_BYTES - bytes.len()) as u64))
            .read_until(b'\n', &mut bytes).map_err(|e| e.to_string())?;
    }
    Ok(bytes)
}
fn sources(db: &Connection, parent: &Value, r: &Request, native: &Value) -> Result<(bool, Value)> {
    let paths = paths(native)?;
    if paths.is_empty() {
        return Ok((false, json!({"provider":"unavailable"})));
    }
    let mut descriptors = Vec::new();
    let mut files = Vec::new();
    for path in paths {
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(_) => return Ok((false, json!({"provider":"unavailable"}))),
        };
        let (identity, meta) =
            crate::platform::FileIdentity::with_metadata(&file).map_err(|e| e.to_string())?;
        let mut first = Vec::new();
        BufReader::new((&mut file).take(256 * 1024))
            .read_until(b'\n', &mut first)
            .map_err(|e| e.to_string())?;
        if string(native, "agent") == "codex" {
            let first: Value =
                serde_json::from_slice(&first).map_err(|_| "Provider header cannot be verified")?;
            if first["type"] != "session_meta"
                || first["payload"]["id"] != native["conversation_id"]
            {
                return Err("Provider transcript identity mismatch".into());
            }
        }
        descriptors.push(json!([
            path,
            identity.device,
            identity.inode,
            hash(&String::from_utf8_lossy(&first))
        ]));
        files.push((
            file,
            meta.len(),
            format!("{}:{}", identity.modified, identity.modified_nanos),
        ));
    }
    let descriptor = json!(descriptors).to_string();
    if get(db, "sources")?
        .as_deref()
        .is_some_and(|old| old != descriptor)
    {
        db.execute_batch("DELETE FROM messages;DELETE FROM aliases;DELETE FROM receipt_seen;DELETE FROM reconcile_work;DELETE FROM projection_changes;DELETE FROM meta WHERE key NOT IN ('secret','schema');").map_err(|e|e.to_string())?;
        set(db, "epoch", &uuid::Uuid::new_v4().to_string())?;
    }
    set(db, "sources", &descriptor)?;
    let repair = get(db, "source_reader_version")?.as_deref() != Some(SOURCE_READER_VERSION)
        && get(db, "gap")?.as_deref() == Some("oversized_source_line");
    let canonical = key(parent, r);
    let mut budget = BUDGET;
    let mut complete = true;
    let mut partial_final = false;
    for (number, (mut file, len, modified)) in files.into_iter().enumerate() {
        let offset_key = format!("offset:{number}");
        let mut offset = get(db, &offset_key)?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let prior_len = get(db, &format!("length:{number}"))?.and_then(|v| v.parse::<u64>().ok());
        let changed_same_length = prior_len == Some(len)
            && get(db, &format!("modified:{number}"))?.is_some_and(|v| v != modified);
        let mut checkpoint = Vec::new();
        file.seek(SeekFrom::Start(offset.saturating_sub(256)))
            .map_err(|e| e.to_string())?;
        (&mut file)
            .take(offset.min(256))
            .read_to_end(&mut checkpoint)
            .map_err(|e| e.to_string())?;
        let boundary = format!("{:x}", Sha256::digest(&checkpoint));
        let changed_boundary =
            get(db, &format!("boundary:{number}"))?.is_some_and(|old| old != boundary);
        if len < offset || changed_same_length || changed_boundary {
            db.execute_batch("DELETE FROM messages;DELETE FROM aliases;DELETE FROM receipt_seen;DELETE FROM reconcile_work;DELETE FROM projection_changes;DELETE FROM meta WHERE key NOT IN ('secret','schema');").map_err(|e|e.to_string())?;
            set(db, "epoch", &uuid::Uuid::new_v4().to_string())?;
            return Ok((false, json!({"provider":"source_reset"})));
        }
        // Revalidate the old source checkpoint before replaying its rows. The
        // old reader discarded oversized rows, including private tool noise.
        // Stable provenance makes replay idempotent; retain message IDs,
        // aliases and read sequences, and keep the head incomplete until EOF.
        if repair {
            offset = 0;
            if number == 0 {
                db.execute("DELETE FROM meta WHERE key='gap'", []).map_err(|e| e.to_string())?;
            }
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut skipping = !repair && get(db, &format!("skip:{number}"))?.as_deref() == Some("1");
        let bytes = source_chunk(&mut file, budget, !skipping)?;
        budget = budget.saturating_sub(bytes.len());
        let mut consumed = 0;
        for part in bytes.split_inclusive(|byte| *byte == b'\n').take(512) {
            if !part.ends_with(b"\n") {
                if skipping || (consumed == 0 && part.len() == SOURCE_LINE_BYTES
                    && offset + (bytes.len() as u64) < len) {
                    skipping = true;
                    consumed += part.len();
                    set(db, "gap", "oversized_source_line")?;
                } else if offset + bytes.len() as u64 == len {
                    partial_final = true;
                    // A final unterminated public row is useful, but cannot establish a
                    // complete source. Keep its offset for a later finalized revision.
                    if let Ok(value) = serde_json::from_slice::<Value>(part) {
                        if string(native, "agent") != "claude"
                            || string(&value, "sessionId").is_empty()
                            || value["sessionId"] == native["conversation_id"]
                        {
                            if let Some((mut row, _)) =
                                search::provider_event(string(native, "agent"), &value)
                            {
                                row["source_offset"] =
                                    json!(format!("{number}:{}", offset + consumed as u64));
                                if !fork_copy(parent, &row) {
                                    add(db, &canonical, row, "provider")?;
                                }
                            }
                        }
                    }
                }
                break;
            }
            let position = offset + consumed as u64;
            consumed += part.len();
            if skipping {
                skipping = false;
                continue;
            }
            let value: Value = match serde_json::from_slice(part) {
                Ok(value) => value,
                Err(_) => {
                    set(db, "gap", "malformed_source_line")?;
                    continue;
                }
            };
            if string(native, "agent") == "claude"
                && !string(&value, "sessionId").is_empty()
                && value["sessionId"] != native["conversation_id"]
            {
                set(db, "gap", "source_identity_gap")?;
                continue;
            }
            if let Some((mut row, _)) = search::provider_event(string(native, "agent"), &value) {
                row["source_offset"] = json!(format!("{number}:{position}"));
                if !fork_copy(parent, &row) {
                    add(db, &canonical, row, "provider")?;
                }
            }
        }
        offset += consumed as u64;
        set(db, &offset_key, &offset.to_string())?;
        let mut checkpoint = Vec::new();
        let mut file = File::open(
            descriptors[number][0]
                .as_str()
                .ok_or("Missing source identity")?,
        )
        .map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(offset.saturating_sub(256)))
            .map_err(|e| e.to_string())?;
        file.take(offset.min(256))
            .read_to_end(&mut checkpoint)
            .map_err(|e| e.to_string())?;
        set(
            db,
            &format!("boundary:{number}"),
            &format!("{:x}", Sha256::digest(&checkpoint)),
        )?;
        set(db, &format!("length:{number}"), &len.to_string())?;
        set(db, &format!("modified:{number}"), &modified)?;
        set(
            db,
            &format!("skip:{number}"),
            if skipping { "1" } else { "0" },
        )?;
        complete &= offset == len && !skipping;
    }
    set(db, "source_reader_version", SOURCE_READER_VERSION)?;
    Ok((
        complete,
        json!({"provider":if complete{"complete"}else if partial_final{"partial_final_line"}else{"indexing"}}),
    ))
}
fn journal(db: &Connection, parent: &Value, r: &Request) -> Result<bool> {
    let path = root().join("events.sqlite3");
    if !path.exists() {
        return Ok(true);
    }
    let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let offset = get(db, "journal_seq")?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let mut query=source.prepare("SELECT seq,payload FROM events WHERE name=? AND conversation IS ? AND seq>? ORDER BY seq LIMIT 1000").map_err(|e|e.to_string())?;
    let values = query
        .query_map(
            params![
                journal::journal_name(parent),
                parent["conversation_id"].as_str(),
                offset
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let canonical = key(parent, r);
    for (seq, text) in &values {
        let mut row: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(_) => {
                set(db, "gap", "malformed_journal_row")?;
                continue;
            }
        };
        let agent = string(&row, "agent_id");
        if if let Some(id) = &r.agent_id {
            agent != id
        } else {
            !["", "main"].contains(&agent)
        } {
            continue;
        }
        row["seq"] = json!(seq);
        if r.agent_id.is_some() {
            row["agent_id"] = json!("");
            if row["type"] == "SubagentStop" {
                row["type"] = json!("Stop");
            }
        }
        add(db, &canonical, row, "journal")?;
    }
    if let Some((seq, _)) = values.last() {
        set(db, "journal_seq", &seq.to_string())?;
    }
    Ok(values.len() < 1000)
}
/// A row of a fork's own transcript dated before the fork was created: a
/// copy of the history it began from, which [`inherited`] shows from the
/// source instead (zerus-b4qk.1). Rows without a known time are kept.
fn fork_copy(parent: &Value, row: &Value) -> bool {
    let created = parent["created"].as_f64().unwrap_or(0.0);
    !string(parent, "fork_parent_id").is_empty()
        && created > 0.0
        && row["at"]
            .as_f64()
            .is_some_and(|at| at > 0.0 && at < created)
}

/// A fork's source history (zerus-b4qk.1): each lineage source's main
/// public journal rows up to where the fork began, as rows of their own
/// read-only stream marked `inherited_from`, at most 1000 per call across
/// the whole lineage (read through the main-message index). They sort before
/// the fork's own rows, never count as incoming for read marks, never match
/// voice or attachments and are never queued for reconciliation. An index
/// that had rows before gains them with one epoch reset, dropping the fork's
/// own provider copies of that history. A child view inherits nothing.
fn inherited(db: &Connection, parent: &Value, r: &Request, existed: bool) -> Result<bool> {
    let path = root().join("events.sqlite3");
    // DeepSeek and OpenCode forks carry their copied history natively.
    if r.agent_id.is_some()
        || string(parent, "fork_parent_id").is_empty()
        || ["dsh", "opencode"].contains(&string(parent, "agent"))
        || !path.exists()
    {
        return Ok(true);
    }
    let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let canonical = key(parent, r);
    let mut complete = true;
    let mut budget: i64 = 1000;
    for entry in journal::lineage(&source, parent) {
        let (Some(name), Some(conversation), Some(last)) =
            (entry[0].as_str(), entry[1].as_str(), entry[2].as_i64())
        else {
            continue;
        };
        if budget == 0 {
            complete = false;
            break;
        }
        let offset_key = format!(
            "inherited:{}",
            hash(&json!([name, conversation, last]).to_string())
        );
        let offset = get(db, &offset_key)?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        // The same expressions as the events_main_messages partial index.
        let mut query = source
            .prepare(
                "SELECT seq,payload FROM events WHERE name=? AND conversation=? AND seq>? AND seq<=? \
                 AND COALESCE(json_extract(payload, '$.agent_id'), '') IN ('', 'main') \
                 AND json_extract(payload, '$.type') IN ('UserPromptSubmit','UserPromptQueued','UserMessage','TurnStarted','QuestionAnswered','AgentMessage','Stop') \
                 ORDER BY seq LIMIT ?",
            )
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map(params![name, conversation, offset, last, budget], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        if !rows.is_empty() && get(db, "inherited_added")?.is_none() {
            set(db, "inherited_added", "1")?;
            if existed {
                backfill(db, parent)?;
            }
        }
        for (seq, text) in &rows {
            let Ok(mut row) = serde_json::from_str::<Value>(text) else {
                continue;
            };
            if !["", "main"].contains(&string(&row, "agent_id")) {
                continue;
            }
            row["seq"] = json!(seq);
            row["inherited_from"] = json!(journal::lineage_label(parent, name));
            add(db, &canonical, row, "inherited")?;
        }
        if let Some((seq, _)) = rows.last() {
            set(db, &offset_key, &seq.to_string())?;
        }
        complete &= (rows.len() as i64) < budget;
        budget -= rows.len() as i64;
    }
    Ok(complete)
}

/// An index that already had rows gains a fork's inherited history: a new
/// epoch, so a reader that saw the oldest row reads again, and without the
/// fork's own provider rows dated before its creation (a provider's copy of
/// that history, indexed before copies were filtered). Incoming numbers are
/// assigned again.
fn backfill(db: &Connection, parent: &Value) -> Result<()> {
    let created = parent["created"].as_f64().unwrap_or(0.0);
    if created > 0.0 {
        // The deleted rows no longer count against the index budget: each was
        // charged its body, its original identity and 256 bytes when added.
        let freed: i64 = db
            .query_row(
                "SELECT COALESCE((SELECT sum(length(CAST(body AS BLOB))+256) FROM messages WHERE stream='provider' AND at>0 AND at<?1),0)
                  +COALESCE((SELECT sum(length(CAST(original AS BLOB))) FROM aliases WHERE id IN
                    (SELECT id FROM messages WHERE stream='provider' AND at>0 AND at<?1)),0)",
                [created],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let used = get(db, "bytes")?
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let freed = usize::try_from(freed).unwrap_or(0);
        set(db, "bytes", &used.saturating_sub(freed).to_string())?;
        for table in ["aliases", "reconcile_work"] {
            db.execute(
                &format!("DELETE FROM {table} WHERE id IN (SELECT id FROM messages WHERE stream='provider' AND at>0 AND at<?)"),
                [created],
            )
            .map_err(|e| e.to_string())?;
        }
        db.execute(
            "DELETE FROM messages WHERE stream='provider' AND at>0 AND at<?",
            [created],
        )
        .map_err(|e| e.to_string())?;
    }
    db.execute("UPDATE messages SET incoming=NULL", [])
        .map_err(|e| e.to_string())?;
    set(db, "epoch", &uuid::Uuid::new_v4().to_string())?;
    set(db, "order_reset", "inherited_backfill")
}

fn receipts(db: &Connection, parent: &Value, r: &Request) -> Result<bool> {
    let agent = r.agent_id.as_deref().unwrap_or("");
    let mut cursor = get(db, "receipt_cursor")?.unwrap_or_default();
    let mut retried = false;
    loop {
        let (catalog, page) = match receipt_index::changed(parent, agent, &cursor, 64, true) {
            Ok(value) => value,
            // The locator was rebuilt or its schema changed. Restart the
            // frontier explicitly; receipt_seen still decides which originals
            // need a new import, so nothing is replayed from scratch.
            Err(error) if error == receipt_index::CURSOR_REJECTED && !retried => {
                retried = true;
                cursor.clear();
                set(db, "receipt_cursor", "")?;
                continue;
            }
            Err(_) => {
                // The rebuildable locator is unavailable or not yet migrated:
                // project indexing, never an empty receipt namespace.
                receipt_index::request_migration();
                return Ok(false);
            }
        };
        if page.coverage == receipt_index::Coverage::Gap {
            set(db, "gap", "receipt_source_gap")?;
        }
        for row in page.rows {
            let name = format!("{}.json", row.locator.id);
            let seen: Option<String> = db
                .query_row(
                    "SELECT stamp FROM receipt_seen WHERE name=?",
                    params![name],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| error.to_string())?;
            if seen.as_deref() == Some(row.stamp.as_str()) {
                continue;
            }
            match catalog.original(&row.locator) {
                Ok(value) => {
                    if let Some((mut projected, submitted)) =
                        attachments::history_receipt_value(parent, agent, &value)
                    {
                        projected["native_match_hash"] = json!(hash(&submitted));
                        add(db, &key(parent, r), projected, "attachment")?;
                    }
                    db.execute(
                        "INSERT INTO receipt_seen(name,stamp) VALUES(?,?) ON CONFLICT(name) DO UPDATE SET stamp=excluded.stamp",
                        params![name, row.stamp],
                    )
                    .map_err(|error| error.to_string())?;
                }
                Err(_) => {
                    // A changed, missing or rebound original is an explicit
                    // gap for this receipt; never rewrite it and never infer
                    // that untouched receipts are absent.
                    set(db, "gap", "receipt_source_gap")?;
                }
            }
        }
        set(db, "receipt_cursor", &page.cursor)?;
        return Ok(page.ready && !page.restart_required && page.drained && !page.pending);
    }
}
fn mac(secret: &str, text: &str) -> String {
    let mut key = [0u8; 64];
    let bytes = secret.as_bytes();
    key[..bytes.len()].copy_from_slice(bytes);
    let inner: Vec<u8> = key.iter().map(|v| v ^ 0x36).chain(text.bytes()).collect();
    let outer: Vec<u8> = key
        .iter()
        .map(|v| v ^ 0x5c)
        .chain(Sha256::digest(inner))
        .collect();
    format!("{:x}", Sha256::digest(outer))
}
fn cursor(secret: &str, epoch: &str, at: f64, ordinal: &str, id: &str) -> String {
    let text = json!([epoch, at, ordinal, id]).to_string();
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(text.as_bytes()),
        mac(secret, &text)
    )
}
fn decode(value: &str, secret: &str, epoch: &str) -> Result<(f64, String, String)> {
    let (body, signature) = value.split_once('.').ok_or("Invalid history cursor")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(body)
        .map_err(|_| "Invalid history cursor")?;
    let text = String::from_utf8(bytes).map_err(|_| "Invalid history cursor")?;
    let expected = mac(secret, &text);
    if signature.len() != expected.len()
        || !signature
            .bytes()
            .zip(expected.bytes())
            .fold(true, |ok, (a, b)| ok & (a == b))
    {
        return Err("History cursor authentication failed".into());
    }
    let value: Value = serde_json::from_str(&text).map_err(|_| "Invalid history cursor")?;
    if value[0] != epoch {
        return Err("History epoch changed; reopen its verified head".into());
    }
    Ok((
        value[1]
            .as_f64()
            .filter(|v| v.is_finite())
            .ok_or("Invalid history position")?,
        value[2]
            .as_str()
            .ok_or("Invalid history position")?
            .to_owned(),
        value[3]
            .as_str()
            .ok_or("Invalid history position")?
            .to_owned(),
    ))
}
fn incoming(db: &Connection, complete: bool) -> Result<Option<i64>> {
    if !complete {
        return Ok(None);
    }
    // An unnumbered incoming row before the newest numbered row means history
    // arrived late. One uncorrelated comparison: a correlated search for any
    // later numbered row is quadratic, about 20 s for 10k unnumbered rows (R3).
    let historical: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM messages m WHERE m.role='incoming' AND m.incoming IS NULL
              AND (m.at,m.ordinal,m.id)<(SELECT at,ordinal,id FROM messages WHERE incoming IS NOT NULL
                ORDER BY at DESC,ordinal DESC,id DESC LIMIT 1))",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if historical {
        db.execute("UPDATE messages SET incoming=NULL", [])
            .map_err(|e| e.to_string())?;
        set(db, "epoch", &uuid::Uuid::new_v4().to_string())?;
        set(db, "order_reset", "historical_backfill")?;
    }
    let mut next: i64 = db
        .query_row(
            "SELECT COALESCE(max(incoming),0) FROM messages",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut query=db.prepare("SELECT id FROM messages WHERE role='incoming' AND incoming IS NULL ORDER BY at,ordinal,id LIMIT 1000").map_err(|e|e.to_string())?;
    let ids = query
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let more = ids.len() == 1000;
    for id in ids {
        next += 1;
        db.execute(
            "UPDATE messages SET incoming=? WHERE id=?",
            params![next, id],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(if more { None } else { Some(next) })
}
fn merge_row(db: &Connection, first: &str, second: &str, body: &str, kind: &str) -> Result<()> {
    let a: Option<i64> = db
        .query_row("SELECT incoming FROM messages WHERE id=?", [first], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    let b: Option<i64> = db
        .query_row("SELECT incoming FROM messages WHERE id=?", [second], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    let (keep, remove) = if b.is_some() && a.is_none() {
        (second, first)
    } else {
        (first, second)
    };
    if a.is_some() && b.is_some() && a != b {
        db.execute("UPDATE messages SET incoming=NULL", [])
            .map_err(|e| e.to_string())?;
        set(db, "epoch", &uuid::Uuid::new_v4().to_string())?;
        set(db, "order_reset", "canonical_merge")?;
    }
    let attachment = serde_json::from_str::<Value>(body).ok().is_some_and(|row| row["source"] == "hgs_delivery");
    db.execute(
        "UPDATE messages SET body=?,kind=?,stream=CASE WHEN ? THEN 'attachment' ELSE stream END WHERE id=?",
        params![body, kind, attachment, keep],
    )
    .map_err(|e| e.to_string())?;
    db.execute("UPDATE aliases SET id=? WHERE id=?", params![keep, remove])
        .map_err(|e| e.to_string())?;
    db.execute(
        "INSERT OR REPLACE INTO aliases(original,id) VALUES(?,?)",
        params![format!("history:{remove}"), keep],
    )
    .map_err(|e| e.to_string())?;
    db.execute("DELETE FROM messages WHERE id=?", [remove])
        .map_err(|e| e.to_string())?;
    db.execute("DELETE FROM reconcile_work WHERE id=?", [remove])
        .map_err(|e| e.to_string())?;
    queue_attachment(db, keep)?;
    Ok(())
}
/// Hook detail() uses journal::clipped(...,journal::REPLY_LIMIT), and 1200 in
/// older journals: Unicode scalars, plus exactly one synthetic U+2026 when more
/// text exists. Other ellipses are literal.
fn stop_excerpt_matches(provider: &str, hook: &str) -> bool {
    let trimmed = hook.trim();
    if trimmed.is_empty() || trimmed.chars().all(|c| c == '…') {
        return false;
    }
    let excerpt = hook
        .strip_suffix('…')
        .filter(|prefix| [1200, journal::REPLY_LIMIT].contains(&prefix.chars().take(journal::REPLY_LIMIT + 1).count()))
        .unwrap_or(hook);
    !excerpt.trim().is_empty() && provider.contains(excerpt)
}

/// Retry old cached pairs without rebuilding sources or changing stable IDs.
/// Primary-key scans survive merges/deletes; a version is committed only once
/// both the scan and the ordinary bounded work queue have finished.
fn advance_reconcile_upgrade(db: &Connection) -> Result<bool> {
    if get(db, "reconcile_version")?.as_deref() == Some(RECONCILE_VERSION) {
        return Ok(true);
    }
    let offset = get(db, "reconcile_upgrade_after")?.unwrap_or_default();
    let mut query = db
        .prepare("SELECT id,role,stream,body FROM messages WHERE id>? ORDER BY id LIMIT 256")
        .map_err(|e| e.to_string())?;
    let rows = query
        .query_map([offset], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (id, role, stream, body) in &rows {
        let mut row: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
        if role == "outgoing" && !["attachment", "voice"].contains(&stream.as_str()) {
            if row["source"] == "hgs_delivery" {
                db.execute("UPDATE messages SET stream='attachment' WHERE id=?", [id]).map_err(|e|e.to_string())?;
            } else if string(&row, "native_match_hash").is_empty() {
                row["native_match_hash"] = json!(hash(string(&row, "detail")));
                db.execute("UPDATE messages SET body=? WHERE id=?", params![row.to_string(),id]).map_err(|e|e.to_string())?;
            }
        }
        if role == "outgoing" { queue_attachment(db, id)?; }
        if (role == "incoming" && stream == "provider") || (role == "outgoing" && row["source"] == "hgs_delivery") {
            db.execute("INSERT OR IGNORE INTO reconcile_work(id) VALUES(?)", [id])
                .map_err(|e| e.to_string())?;
        }
    }
    if let Some((id, _, _, _)) = rows.last() {
        set(db, "reconcile_upgrade_after", id)?;
    }
    Ok(rows.len() < 256)
}
/// Applies the current public projection to outgoing Claude rows indexed
/// before it: each row's own transcript line, found by its source offset and
/// verified by its uuid, is projected again. That includes a hook row that a
/// provider row merged into, which keeps the provider's body and aliases. A row
/// that line still publishes is refreshed in place through its alias; a row it
/// no longer publishes, such as a compaction summary or Claude's record of a
/// local command, is removed when nothing else was merged into it. Incoming
/// rows, their sequences and the epoch never change. Bounded per call and
/// resumable by row ID.
fn advance_projection_upgrade(db: &Connection, canonical: &str, native: &Value) -> Result<Value> {
    let epoch = get(db, "epoch")?.ok_or("History epoch unavailable")?;
    // Only this epoch's changes can be served; an earlier epoch's are dropped.
    db.execute("DELETE FROM projection_changes WHERE epoch<>?", [&epoch]).map_err(|e| e.to_string())?;
    let generation: i64 = db
        .query_row("SELECT COALESCE(MAX(generation),0) FROM projection_changes", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if get(db, "projection_version")?.as_deref() == Some(PROJECTION_VERSION) {
        return Ok(json!({"state":"complete","generation":generation}));
    }
    if string(native, "agent") != "claude" {
        set(db, "projection_version", PROJECTION_VERSION)?;
        return Ok(json!({"state":"complete","generation":generation}));
    }
    let paths = paths(native)?;
    // Without the transcript nothing can be proven; try again on a later call.
    if paths.is_empty() {
        return Ok(json!({"state":"indexing","generation":generation}));
    }
    let after = get(db, "projection_upgrade_after")?.unwrap_or_default();
    let mut query = db
        .prepare("SELECT id,body FROM messages WHERE id>? AND role='outgoing' AND stream IN ('provider','journal') ORDER BY id LIMIT ?")
        .map_err(|e| e.to_string())?;
    let rows = query
        .query_map(params![after, PROJECTION_ROWS as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let (mut read, mut last, mut full) = (0, None, false);
    for (id, body) in &rows {
        if read >= PROJECTION_BYTES {
            break;
        }
        let previous = last.replace(id.clone());
        let row: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
        if row["source"] != "claude_transcript" {
            continue;
        }
        // The row must own its provider origin; another owner proves nothing here.
        let origin = original(&row, "provider").unwrap_or_default();
        let owner: Option<String> = db
            .query_row("SELECT id FROM aliases WHERE original=?", [&origin], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        if owner.as_ref() != Some(id) {
            tally(db, "projection_unverified")?;
            continue;
        }
        let Some(line) = native_line(&paths, &row, native, &mut read) else {
            tally(db, "projection_unverified")?;
            continue;
        };
        let change = match search::provider_event("claude", &line) {
            Some((fresh, _)) => {
                let stored = string(&row, "native_match_hash");
                let changed = (!stored.is_empty() && stored != hash(string(&fresh, "detail")))
                    || (stored.is_empty() && row["detail"] != fresh["detail"]);
                changed.then_some(("refreshed", Some(fresh)))
            }
            None => {
                let mut aliases = db
                    .prepare("SELECT original FROM aliases WHERE id=? LIMIT 2")
                    .map_err(|e| e.to_string())?;
                let aliases = aliases
                    .query_map([id], |r| r.get::<_, String>(0))
                    .map_err(|e| e.to_string())?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?;
                if aliases.len() == 1 && aliases[0] == origin {
                    Some(("retired", None))
                } else {
                    tally(db, "projection_retained")?;
                    None
                }
            }
        };
        let Some((change, fresh)) = change else { continue };
        // Every row change is recorded with it; without room, nothing changes.
        if !ledger_room(db)? {
            last = previous;
            full = true;
            break;
        }
        if let Some(mut fresh) = fresh {
            fresh["source_offset"] = row["source_offset"].clone();
            add(db, canonical, fresh, "provider")?;
            let rewritten: Option<String> = db
                .query_row("SELECT body FROM messages WHERE id=?", [id], |r| r.get(0))
                .optional()
                .map_err(|e| e.to_string())?;
            // add declines a rewrite past the index budget; that is no change.
            if rewritten.as_deref() == Some(body.as_str()) {
                tally(db, "projection_declined")?;
                continue;
            }
        } else {
            db.execute("DELETE FROM messages WHERE id=?", [id]).map_err(|e| e.to_string())?;
            db.execute("DELETE FROM aliases WHERE id=?", [id]).map_err(|e| e.to_string())?;
            db.execute("DELETE FROM reconcile_work WHERE id=?", [id]).map_err(|e| e.to_string())?;
            advance_voice_generation(db)?;
        }
        db.execute("INSERT INTO projection_changes(epoch,id,change) VALUES(?,?,?)", params![epoch, id, change])
            .map_err(|e| e.to_string())?;
        tally(db, &format!("projection_{change}"))?;
    }
    let processed = !full && last.as_ref() == rows.last().map(|(id, _)| id);
    if processed && rows.len() < PROJECTION_ROWS {
        set(db, "projection_version", PROJECTION_VERSION)?;
        db.execute("DELETE FROM meta WHERE key='projection_upgrade_after'", []).map_err(|e| e.to_string())?;
    } else if let Some(id) = last {
        set(db, "projection_upgrade_after", &id)?;
    }
    let state = if full {
        "ledger_full"
    } else if get(db, "projection_version")?.as_deref() == Some(PROJECTION_VERSION) {
        "complete"
    } else {
        "indexing"
    };
    let generation: i64 = db
        .query_row("SELECT COALESCE(MAX(generation),0) FROM projection_changes", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let mut status = json!({"state":state,"generation":generation});
    for name in ["refreshed", "retired", "retained", "declined", "unverified"] {
        status[name] = json!(get(db, &format!("projection_{name}"))?.and_then(|v| v.parse::<i64>().ok()).unwrap_or(0));
    }
    Ok(status)
}
/// The newest ledger generation of the index's current epoch, after deleting
/// entries of any other epoch; 0 when it has none.
fn projection_generation(db: &Connection) -> Result<i64> {
    let epoch = get(db, "epoch")?.ok_or("History epoch unavailable")?;
    db.execute("DELETE FROM projection_changes WHERE epoch<>?", [&epoch]).map_err(|e| e.to_string())?;
    db.query_row("SELECT COALESCE(MAX(generation),0) FROM projection_changes", [], |r| r.get(0))
        .map_err(|e| e.to_string())
}
/// Whether one more projection change can be recorded: fewer than
/// LEDGER_ENTRIES entries and a next generation within SQLite's integer range.
fn ledger_room(db: &Connection) -> Result<bool> {
    let (entries, last): (i64, i64) = db
        .query_row(
            "SELECT (SELECT count(*) FROM projection_changes),
                COALESCE((SELECT seq FROM sqlite_sequence WHERE name='projection_changes'),0)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    Ok(entries < LEDGER_ENTRIES && last < i64::MAX)
}
/// The complete transcript line a provider row was projected from, verified by
/// its uuid and conversation, or None. A present `sessionId` must be exactly the
/// conversation's ID. Adds the bytes read to `read`.
fn native_line(paths: &[PathBuf], row: &Value, native: &Value, read: &mut usize) -> Option<Value> {
    let (file, byte) = string(row, "source_offset").split_once(':')?;
    let mut source = File::open(paths.get(file.parse::<usize>().ok()?)?).ok()?;
    source.seek(SeekFrom::Start(byte.parse::<u64>().ok()?)).ok()?;
    let mut line = Vec::new();
    BufReader::new(source.take(SOURCE_LINE_BYTES as u64)).read_until(b'\n', &mut line).ok()?;
    *read += line.len();
    if !line.ends_with(b"\n") {
        return None;
    }
    let value: Value = serde_json::from_slice(&line).ok()?;
    let session = match value.get("sessionId") {
        None => true,
        Some(Value::String(session)) => !session.is_empty() && native["conversation_id"].as_str() == Some(session.as_str()),
        Some(_) => false,
    };
    let uuid = string(row, "message_id");
    (session && !uuid.is_empty() && value.get("uuid").and_then(Value::as_str) == Some(uuid)).then_some(value)
}
fn tally(db: &Connection, key: &str) -> Result<()> {
    db.execute("INSERT INTO meta(key,value) VALUES(?,'1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1", [key])
        .map_err(|e| e.to_string())?;
    Ok(())
}
fn finish_reconcile_upgrade(db: &Connection, scan_done: bool) -> Result<bool> {
    if !scan_done {
        return Ok(false);
    }
    let pending: bool = db
        .query_row("SELECT EXISTS(SELECT 1 FROM reconcile_work)", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    if !pending {
        set(db, "reconcile_version", RECONCILE_VERSION)?;
        db.execute("DELETE FROM meta WHERE key='reconcile_upgrade_after'", [])
            .map_err(|e| e.to_string())?;
    }
    Ok(!pending)
}
/// Expanded attachment text is receipt-specific, so delayed acknowledgements
/// use its exact digest rather than a timestamp or a user-text prefix. Ambiguous
/// native copies/receipts stay separate until their provenance is resolved.
fn reconcile_attachment(db: &Connection, id: &str, value: &Value, body: &str) -> Result<()> {
    let digest = string(value, "native_match_hash");
    if digest.is_empty() { return Ok(()); }
    let mut query = db.prepare("SELECT id,stream,kind FROM messages WHERE role='outgoing' AND json_extract(body,'$.native_match_hash')=? LIMIT 33").map_err(|e|e.to_string())?;
    let candidates = query.query_map([digest], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))
        .map_err(|e|e.to_string())?.collect::<std::result::Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
    if candidates.len() > 32 { return Ok(()); }
    let receipts = candidates.iter().filter(|(_,stream,_)| stream == "attachment").count();
    let native: Vec<_> = candidates.iter().filter(|(other,stream,kind)| other != id && !["attachment","voice"].contains(&stream.as_str())
        && ["UserPromptSubmit","UserPromptQueued","UserMessage","TurnStarted"].contains(&kind.as_str())).collect();
    if receipts == 1 {
        if let [native] = native.as_slice() {
            merge_row(db, &native.0, id, body, string(value,"type"))?;
        }
    }
    Ok(())
}

fn reconcile(db: &Connection) -> Result<()> {
    // A work queue handles either arrival order, body revisions and deleted-ID
    // reuse. Candidate uniqueness is checked across the full canonical index.
    let mut query=db.prepare("SELECT w.id,m.at,m.role,m.stream,m.body FROM reconcile_work w LEFT JOIN messages m ON m.id=w.id ORDER BY w.id LIMIT 64").map_err(|e|e.to_string())?;
    let rows = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (id, at, role, stream, body) in &rows {
        if let (Some(at), Some(role), Some(stream), Some(body)) = (at, role, stream, body) {
            if ["provider", "attachment"].contains(&stream.as_str()) {
                let value: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
                if stream == "attachment" {
                    reconcile_attachment(db, id, &value, body)?;
                    db.execute("DELETE FROM reconcile_work WHERE id=?", [id]).map_err(|e|e.to_string())?;
                    continue;
                }
                let radius = 3.;
                let count:i64=db.query_row("SELECT count(*) FROM (SELECT 1 FROM messages WHERE role=? AND at BETWEEN ? AND ? LIMIT 33)",params![role,at-radius,at+radius],|r|r.get(0)).map_err(|e|e.to_string())?;
                if count <= 32 {
                    let mut near=db.prepare("SELECT id,stream,kind,body FROM messages WHERE role=? AND at BETWEEN ? AND ?").map_err(|e|e.to_string())?;
                    let candidates = near
                        .query_map(params![role, at - radius, at + radius], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                                r.get::<_, String>(3)?,
                            ))
                        })
                        .map_err(|e| e.to_string())?
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .map_err(|e| e.to_string())?;
                    let matches = |text: &str| {
                        if role == "incoming" {
                            stop_excerpt_matches(string(&value, "detail"), text)
                        } else {
                            text == string(&value, "detail")
                        }
                    };
                    let hooks: Vec<_> = candidates
                        .iter()
                        .filter(|(other, source, kind, text)| {
                            other != id
                                && source == "journal"
                                && if role == "incoming" {
                                    kind == "Stop"
                                } else {
                                    ["UserPromptSubmit", "UserPromptQueued", "UserMessage", "TurnStarted"].contains(&kind.as_str())
                                }
                                && serde_json::from_str::<Value>(text)
                                    .ok()
                                    .is_some_and(|row| matches(string(&row, "detail")))
                        })
                        .collect();
                    if let [hook] = hooks.as_slice() {
                        let hook_value: Value =
                            serde_json::from_str(&hook.3).map_err(|e| e.to_string())?;
                        let competing = candidates
                            .iter()
                            .filter(|(_, source, _, text)| {
                                source == stream
                                    && serde_json::from_str::<Value>(text).ok().is_some_and(|row| {
                                        if role == "incoming" {
                                            stop_excerpt_matches(
                                                string(&row, "detail"),
                                                string(&hook_value, "detail"),
                                            )
                                        } else {
                                            string(&row, "detail") == string(&hook_value, "detail")
                                        }
                                    })
                            })
                            .count();
                        if competing == 1 {
                            merge_row(db, &hook.0, id, body, string(&value, "type"))?;
                        }
                    }
                }
            }
        }
        db.execute("DELETE FROM reconcile_work WHERE id=?", [id])
            .map_err(|e| e.to_string())?;
    }
    let pending: bool = db
        .query_row("SELECT EXISTS(SELECT 1 FROM reconcile_work)", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    set(db, "reconciled", if pending { "0" } else { "1" })?;
    Ok(())
}
fn projection_number(number: &serde_json::Number) -> String {
    let raw = number.to_string();
    if !raw.contains(['.', 'e', 'E']) {
        return if raw == "-0" { "0".into() } else { raw };
    }
    // serde_json writes the shortest digits that round-trip, as Python does.
    let (sign, unsigned) = match raw.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", raw.as_str()),
    };
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().unwrap_or(0)),
        None => (unsigned, 0),
    };
    let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let all = mantissa.replace('.', "");
    let first = all.find(|c| c != '0').unwrap_or(all.len());
    let digits = all[first..].trim_end_matches('0');
    if digits.is_empty() {
        return format!("{sign}0.0");
    }
    let power = exponent + point - first as i32 - 1;
    if !(-4..16).contains(&power) {
        let (lead, rest) = digits.split_at(1);
        let rest = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let mark = if power < 0 { '-' } else { '+' };
        return format!("{sign}{lead}{rest}e{mark}{:02}", power.abs());
    }
    let position = power + 1;
    if position <= 0 {
        format!(
            "{sign}0.{}{digits}",
            "0".repeat(position.unsigned_abs() as usize)
        )
    } else if position as usize >= digits.len() {
        let zeros = "0".repeat(position as usize - digits.len());
        format!("{sign}{digits}{zeros}.0")
    } else {
        let (whole, fraction) = digits.split_at(position as usize);
        format!("{sign}{whole}.{fraction}")
    }
}

fn ascii_wire_size(value: &Value) -> usize {
    fn text(value: &str) -> usize {
        2 + value.chars().map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            ' '..='~' => 1,
            c => 6 * c.len_utf16(),
        }).sum::<usize>()
    }
    match value {
        Value::Null | Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(n) => projection_number(n).len(),
        Value::String(s) => text(s),
        Value::Array(items) => 2 + items.iter().map(ascii_wire_size).sum::<usize>() + items.len().saturating_sub(1),
        Value::Object(items) => 2 + items.iter().map(|(k,v)| text(k) + 1 + ascii_wire_size(v)).sum::<usize>() + items.len().saturating_sub(1),
    }
}

/// A native row is eligible only within the source boundary persisted before
/// delivery. Text is checked separately, and both sides must have one candidate.
fn voice_after_boundary(row: &Value, correlation: &Value, sources: &Value) -> bool {
    if correlation["native_receipt_status"] != "submitted" { return false; }
    let boundary = &correlation["boundary"];
    let Some(at) = correlation["native_submitted_at"].as_f64() else { return false; };
    if at_of(row) < at - 1. || at_of(row) > at + 30. { return false; }
    if let Some((file, byte)) = string(row, "source_offset").split_once(':') {
        let (Ok(file), Ok(byte)) = (file.parse::<usize>(), byte.parse::<u64>()) else { return false; };
        let source = &sources[file];
        return source[0] == boundary["transcript"] && !source[0].is_null()
            && source[1] == boundary["source_device"] && !source[1].is_null()
            && source[2] == boundary["source_inode"] && !source[2].is_null()
            && boundary["source_bytes"].as_u64().is_some_and(|start| byte >= start);
    }
    row["seq"].as_i64().zip(boundary["journal_seq"].as_i64())
        .is_some_and(|(seq, start)| seq > start)
}

fn voice_binding_matches(row: &Value, binding: &Value, correlation: &Value, sources: &Value) -> bool {
    (!string(binding,"provider_message_id").is_empty()
        && row["message_id"] == binding["provider_message_id"]
        && row["source"] == binding["provider_source"])
    // Offsets are identities only inside the original source file. Never
    // replace an explicit provider ID with another message at its old offset.
    || (string(binding,"provider_message_id").is_empty()
        && !string(binding,"provider_source_offset").is_empty()
        && row["source_offset"] == binding["provider_source_offset"]
        && row["source"] == binding["provider_source"]
        && voice_after_boundary(row,correlation,sources))
    || binding["journal_seq"].as_i64().is_some_and(|seq| row["seq"].as_i64() == Some(seq)
        || row["original_ids"].as_array().into_iter().flatten().filter_map(Value::as_str)
            .any(|id| id.starts_with("journal:") && id.ends_with(&format!(":native:{seq}"))))
}

/// Desktop inspection has a bounded journal window even before the paged
/// history index is opened. Resolve its exact post-delivery journal sequence
/// so the same voice card works when no phone has requested history yet.
/// The journal rows a voice message may bind to: this session's own main
/// prompts. A fork's inherited rows belong to its source and never bind
/// (zerus-b4qk); the submission boundary keeps them out as well, since they
/// all precede the fork.
fn voice_candidates(detail: &Value) -> Vec<&Value> {
    detail["message_events"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(detail["events"].as_array().into_iter().flatten())
        .filter(|event| {
            [
                "UserPromptSubmit",
                "UserPromptQueued",
                "UserMessage",
                "TurnStarted",
            ]
            .contains(&string(event, "type"))
                && ["", "main"].contains(&string(event, "agent_id"))
                && event.get("inherited_from").is_none()
        })
        .collect()
}

pub(super) fn inspected_voice_messages(record: &Value, detail: &Value, agent: Option<&str>) -> Value {
    let mut voices = voice::messages(record, agent);
    let events = voice_candidates(detail);
    let mut candidates = Vec::new();
    for (index,row) in voices.iter().enumerate() {
        let Some(correlation) = voice::correlation(record,string(row,"message_id")) else { continue; };
        if correlation["state"] != "submitted" || correlation["native_binding"].is_object() { continue; }
        let mut found: Vec<_> = events.iter().filter(|event|
            !string(row,"detail").trim().is_empty() && string(event,"detail").trim() == string(row,"detail").trim()
                && voice_after_boundary(event,&correlation,&json!([])))
            .filter_map(|event| event["seq"].as_i64()).collect();
        found.sort_unstable(); found.dedup();
        if let [seq] = found.as_slice() { candidates.push((index,*seq)); }
    }
    for (index,seq) in &candidates {
        if candidates.iter().filter(|(_,other)| other == seq).count() != 1 { continue; }
        let row = &mut voices[*index];
        let binding = json!({"journal_seq":seq});
        if voice::bind_native(record,string(row,"message_id"),&binding).unwrap_or(false) {
            row["voice"]["native_binding"] = binding;
        }
    }
    json!(voices)
}

/// The OpenCode rows that are durable history. Queued inbox items are not
/// native history until OpenCode admits them, and a reply OpenCode still
/// streams changes later; journal rows are never replaced once indexed.
fn opencode_history(detail: &Value) -> impl Iterator<Item = &Value> {
    detail["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row.get("delivery_state").is_none() && row["streaming"] != true)
}

/// OpenCode admits a queued message after the delivery window; a voice
/// binding to its exact message ID still finds that native row.
fn bound_opencode_row(db: &Connection, binding: &Value) -> Result<Option<(String, Value)>> {
    let Some(id) = binding["provider_message_id"]
        .as_str()
        .filter(|id| !id.is_empty() && binding["provider_source"] == "opencode_native")
    else {
        return Ok(None);
    };
    let origin = original(
        &json!({"type":"UserPromptSubmit","source":"opencode_native","message_id":id}),
        "journal",
    )
    .ok_or("Invalid OpenCode voice binding")?;
    let found: Option<(String, String)> = db
        .query_row(
            "SELECT m.id,m.body FROM aliases a JOIN messages m ON m.id=a.id WHERE a.original=? AND m.role='outgoing' AND m.stream!='voice'",
            [&origin],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(found.and_then(|(id, body)| serde_json::from_str::<Value>(&body).ok().map(|v| (id, v))))
}

/// Cross-page competition must use the same unique provider/journal eligibility
/// as candidate collection. A source boundary alone also covers later repeated
/// prompts; it does not make an already-bound old voice their owner.
fn competing_voice_candidate(db: &Connection, voice_row: &Value, correlation: &Value, native_id: &str, native: &Value, sources: &Value) -> Result<bool> {
    let binding=&correlation["native_binding"];
    if voice_binding_matches(native,binding,correlation,sources) { return Ok(true); }
    if !voice_after_boundary(native,correlation,sources) { return Ok(false); }
    let Some(at)=correlation["native_submitted_at"].as_f64() else { return Ok(false); };
    let mut query=db.prepare("SELECT id,body FROM messages WHERE role='outgoing' AND stream!='voice' AND at BETWEEN ? AND ? ORDER BY at,ordinal,id LIMIT 65")
        .map_err(|e|e.to_string())?;
    let rows=query.query_map(params![at-1.,at+30.],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))
        .map_err(|e|e.to_string())?.collect::<std::result::Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
    // An over-budget competing window cannot establish uniqueness.
    if rows.len()>64 || rows.iter().map(|(_,body)|body.len()).sum::<usize>()>512*1024 { return Ok(true); }
    let rows:Vec<_>=rows.into_iter().filter_map(|(id,body)|serde_json::from_str::<Value>(&body).ok().map(|row|(id,row)))
        .filter(|(_,row)| ["UserPromptSubmit","UserPromptQueued","UserMessage","TurnStarted"].contains(&string(row,"type"))
            && string(row,"detail").trim()==string(voice_row,"detail").trim()
            && voice_after_boundary(row,correlation,sources)).collect();
    let providers:Vec<_>=rows.iter().filter(|(_,row)|!string(row,"source_offset").is_empty()).collect();
    let journals:Vec<_>=rows.iter().filter(|(_,row)|row["seq"].as_i64().is_some()).collect();
    Ok(matches!(providers.as_slice(),[provider] if provider.0==native_id
        && (string(binding,"provider_source_offset").is_empty() || voice_binding_matches(&provider.1,binding,correlation,sources)))
        || matches!(journals.as_slice(),[journal] if journal.0==native_id
            && (binding["journal_seq"].is_null() || journal.1["seq"]==binding["journal_seq"])))
}

fn reconcile_voice(db: &Connection, parent: &Value, request: &Request) -> Result<bool> {
    if request.agent_id.as_deref().is_some_and(|id| !id.is_empty()) { return Ok(true); }
    let sources: Value = get(db,"sources")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!([]));
    let generation = hash(&json!([get(db,"epoch")?,sources,get(db,"voice_native_generation")?]).to_string());
    // Finish the bounded sweep already in progress before adopting a newer
    // generation. Constant native traffic must not reset progress to row zero.
    if get(db,"voice_reconcile_generation")?.is_none() {
        set(db,"voice_reconcile_after","0")?;
        set(db,"voice_reconcile_generation",&generation)?;
    }
    let after = get(db,"voice_reconcile_after")?.and_then(|value|value.parse::<i64>().ok()).unwrap_or(0);
    let (voices,next,complete) = voice::reconciliation_page(parent,after)?;
    let page_ids:std::collections::BTreeSet<_>=voices.iter().map(|(row,_)|string(row,"message_id").to_owned()).collect();
    let mut possible = Vec::new();
    for (row,correlation) in voices {
        if correlation["state"] != "submitted" { continue; }
        let binding = &correlation["native_binding"];
        let Some(at) = correlation["native_submitted_at"].as_f64() else { continue; };
        let mut query = db.prepare("SELECT id,body FROM messages WHERE role='outgoing' AND stream!='voice' AND at BETWEEN ? AND ? ORDER BY at,ordinal,id LIMIT 65")
            .map_err(|e|e.to_string())?;
        let rows = query.query_map(params![at-1.,at+30.], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))
            .map_err(|e|e.to_string())?.collect::<std::result::Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
        if rows.len() > 64 { continue; }
        let mut rows: Vec<_> = rows.into_iter().filter_map(|(id,body)| serde_json::from_str::<Value>(&body).ok().map(|v|(id,v)))
            .filter(|(_,row)| ["UserPromptSubmit","UserPromptQueued","UserMessage","TurnStarted"].contains(&string(row,"type"))).collect();
        if let Some(found) = bound_opencode_row(db,binding)? {
            if !rows.iter().any(|(id,_)| *id == found.0) { rows.push(found); }
        }
        for (id,row) in &mut rows {
            let mut aliases = db.prepare("SELECT original FROM aliases WHERE id=? ORDER BY original LIMIT 8").map_err(|e|e.to_string())?;
            let ids = aliases.query_map([id.as_str()],|r|r.get::<_,String>(0)).map_err(|e|e.to_string())?
                .collect::<std::result::Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
            row["original_ids"] = json!(ids);
        }
        let mut native_rows: Vec<(String,Value)> = rows.iter().filter(|(_,native)|
            !string(&row,"detail").trim().is_empty()
            && string(native,"detail").trim() == string(&row,"detail").trim()
            && voice_binding_matches(native,binding,&correlation,&sources)).cloned().collect();
        let eligible: Vec<_> = rows.iter().filter(|(_,native)| !string(&row,"detail").trim().is_empty()
            && string(native,"detail").trim() == string(&row,"detail").trim()
            && voice_after_boundary(native,&correlation,&sources)).collect();
        let providers: Vec<_> = eligible.iter().filter(|(_,native)| !string(native,"source_offset").is_empty()).collect();
        let journals: Vec<_> = eligible.iter().filter(|(_,native)| native["seq"].as_i64().is_some()).collect();
        let mut proposed = if binding.is_object() {binding.clone()} else {json!({})};
        if let [provider] = providers.as_slice() {
            let compatible = string(binding,"provider_source_offset").is_empty()
                || voice_binding_matches(&provider.1,binding,&correlation,&sources);
            if compatible {
                for (from,to) in [("message_id","provider_message_id"),("source_offset","provider_source_offset"),("source","provider_source")] {
                    if let Some(value) = provider.1.get(from).filter(|value| value.as_str().is_some_and(|text| !text.is_empty())) {
                        proposed[to] = value.clone();
                    }
                }
                native_rows.push((provider.0.clone(),provider.1.clone()));
            }
        }
        if let [journal] = journals.as_slice() {
            if binding["journal_seq"].is_null() || journal.1["seq"] == binding["journal_seq"] {
                proposed["journal_seq"] = journal.1["seq"].clone();
                native_rows.push((journal.0.clone(),journal.1.clone()));
            }
        }
        native_rows.sort_by(|a,b|a.0.cmp(&b.0));native_rows.dedup_by(|a,b|a.0==b.0);
        if !native_rows.is_empty() {possible.push((row,native_rows,proposed));}
    }
    for (voice_row, native_rows, binding) in &possible {
        if native_rows.iter().any(|(id,_)| possible.iter().filter(|(_,others,_)|others.iter().any(|(other,_)|other==id)).count()!=1) {continue;}
        let mut unique = true;
        for (native_id,native) in native_rows {
            let Some(candidates) = voice::reconciliation_candidates(parent,native)? else { unique=false;break; };
            let mut competitors=0;
            for (other,correlation) in candidates {
                if string(&other,"detail").trim()!=string(voice_row,"detail").trim() {continue;}
                let id=string(&other,"message_id");
                let competes=if page_ids.contains(id) {
                    possible.iter().any(|(row,rows,_)|string(row,"message_id")==id && rows.iter().any(|(id,_)|id==native_id))
                } else { competing_voice_candidate(db,&other,&correlation,native_id,native,&sources)? };
                if competes {competitors+=1;}
                if competitors>1 {break;}
            }
            if competitors!=1 {unique=false;break;}
        }
        if !unique {continue;}
        let mut row = voice_row.clone();
        let message_id = string(&row,"message_id").to_owned();
        if !voice::bind_native(parent,&message_id,binding)? {continue;}
        row["voice"]["native_binding"] = binding.clone();
        let origin = original(&row,"voice").ok_or("Voice row lacks provenance")?;
        let id: String = db.query_row("SELECT id FROM aliases WHERE original=?",[&origin],|r|r.get(0))
            .map_err(|e|e.to_string())?;
        for (native_id,native) in native_rows {
            merge_row(db,&id,native_id,&row.to_string(),"UserMessage")?;
            queue(db,&id,at_of(native))?;
        }
    }
    if complete && get(db,"voice_reconcile_generation")?.as_deref()!=Some(&generation) {
        set(db,"voice_reconcile_after","0")?;
        set(db,"voice_reconcile_generation",&generation)?;
        return Ok(false);
    }
    set(db,"voice_reconcile_after",&next.to_string())?;
    Ok(complete)
}
/// One stored row as history pages and projection changes return it: at most 8
/// aliases as `original_ids`, its ID, cursor and stream, its incoming number when
/// the history is complete, and never its native match digest.
fn public_row(db: &Connection, secret: &str, epoch: &str, complete: bool,
    (id, at, ordinal, body, incoming): (&str, f64, &str, &str, Option<i64>)) -> Result<Value> {
    let mut value: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let mut aliases = db
        .prepare("SELECT original FROM aliases WHERE id=? ORDER BY original LIMIT 8")
        .map_err(|e| e.to_string())?;
    let originals = aliases
        .query_map([id], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    value["original_ids"] = json!(originals);
    value["history_id"] = json!(id);
    value["history_cursor"] = json!(cursor(secret, epoch, at, ordinal, id));
    value["history_stream"] = json!(if string(&value, "source") == "hgs_delivery" {
        "attachment"
    } else if string(&value, "source").is_empty() {
        "journal"
    } else {
        "provider"
    });
    value.as_object_mut().unwrap().remove("native_match_hash");
    // A fork's inherited row has its own type, keeping the original as
    // `inherited_type`: an app that does not know it drops the row (as it
    // drops any unknown type) instead of counting a source reply as unread.
    if value.get("inherited_from").is_some() {
        value["inherited_type"] = value["type"].take();
        value["type"] = json!("Inherited");
    }
    value["incoming_seq"] = if complete {
        json!(incoming)
    } else {
        Value::Null
    };
    Ok(value)
}
fn page(
    db: &Connection,
    r: &Request,
    complete: bool,
    total: Option<i64>,
    status: Value,
) -> Result<Value> {
    let epoch = get(db, "epoch")?.ok_or("History epoch unavailable")?;
    let secret = get(db, "secret")?.ok_or("History cursor key unavailable")?;
    let anchor = r
        .before
        .as_ref()
        .or(r.after.as_ref())
        .or(r.around.as_ref())
        .map(|v| decode(v, &secret, &epoch))
        .transpose()?;
    let mut around = if r.around.is_some() {
        anchor.clone()
    } else {
        None
    };
    if let Some(seq) = r.around_incoming_seq {
        if !complete || r.history_epoch.as_deref() != Some(&epoch) {
            return Err("Exact complete history epoch required for incoming anchor".into());
        }
        around = db
            .query_row(
                "SELECT at,ordinal,id FROM messages WHERE incoming>? ORDER BY incoming LIMIT 1",
                [seq],
                |row| {
                    Ok((
                        row.get::<_, f64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
    }
    let limit = r.limit.unwrap_or(100) as i64;
    let (sql, position) = if let Some(anchor) = around {
        ("SELECT id,at,ordinal,body,incoming FROM messages WHERE (at,ordinal,id)>=(?,?,?) ORDER BY at,ordinal,id LIMIT ?",anchor)
    } else if r.after.is_some() {
        ("SELECT id,at,ordinal,body,incoming FROM messages WHERE (at,ordinal,id)>(?,?,?) ORDER BY at,ordinal,id LIMIT ?",anchor.unwrap())
    } else {
        ("SELECT id,at,ordinal,body,incoming FROM messages WHERE (at,ordinal,id)<(?,?,?) ORDER BY at DESC,ordinal DESC,id DESC LIMIT ?",anchor.unwrap_or((f64::MAX,"~".into(),"~".into())))
    };
    let mut query = db.prepare(sql).map_err(|e| e.to_string())?;
    let mut rows = query
        .query_map(params![position.0, position.1, position.2, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    rows.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0)));
    let mut events = Vec::new();
    let mut kept = Vec::new();
    let mut bytes = 0;
    let mut detail_truncated = false;
    // Consume in query direction so a byte-limited older page remains adjacent
    // to its anchor; reverse only the rows actually returned.
    if sql.contains(" DESC") {
        rows.reverse();
    }
    for (id, at, ordinal, body, incoming) in &rows {
        let mut value = public_row(db, &secret, &epoch, complete, (id.as_str(), *at, ordinal.as_str(), body.as_str(), *incoming))?;
        let mut cost = ascii_wire_size(&value) + 1;
        if bytes + cost > 880 * 1024 {
            if !events.is_empty() {
                break;
            }
            value["detail"] = json!(journal::clipped(&value["detail"], 500));
            value["detail_truncated"] = json!(true);
            detail_truncated = true;
            cost = ascii_wire_size(&value) + 1;
            if cost > 880 * 1024 {
                return Err("Public message metadata exceeds the bounded history page".into());
            }
        }
        bytes += cost;
        detail_truncated |= value["detail_truncated"] == true;
        events.push(value);
        kept.push((id.clone(), *at, ordinal.clone(), body.clone(), *incoming));
    }
    rows = kept;
    if sql.contains(" DESC") {
        rows.reverse();
        events.reverse();
    }
    let has_before = if let Some((id, at, ordinal, _, _)) = rows.first() {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE (at,ordinal,id)<(?,?,?))",
            params![at, ordinal, id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|e| e.to_string())?
    } else {
        false
    };
    let has_after = if let Some((id, at, ordinal, _, _)) = rows.last() {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE (at,ordinal,id)>(?,?,?))",
            params![at, ordinal, id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|e| e.to_string())?
    } else {
        false
    };
    Ok(
        json!({"history_epoch":epoch,"events":events,"next_before":if has_before{rows.first().map(|(id,at,ordinal,_,_)|cursor(&secret,&epoch,*at,ordinal,id))}else{None},"next_after":if has_after{rows.last().map(|(id,at,ordinal,_,_)|cursor(&secret,&epoch,*at,ordinal,id))}else{None},"has_more_before":has_before,"has_more_after":has_after,
  "head":{"incoming_seq":total,"total_incoming":total,"complete":complete},"indexing":!complete&&get(db,"gap")?.is_none()&&(status["provider"]=="indexing"||status["provider"]=="source_reset"||status["journal"]=="indexing"||status["attachments"]=="indexing"||status["canonical"]=="indexing"||status["inherited"]=="indexing"),"truncated":detail_truncated||!complete,"source_status":status}),
    )
}
/// Opens the history index of `parent` in `directory` for a projection read,
/// under the lock `index_in` uses, only when the directory, lock file and
/// database already exist privately and hold a schema-2 index with its epoch,
/// cursor key and ledger. It creates, migrates, resets and repairs nothing:
/// only ordinary history calls build an index, so a missing, uninitialized or
/// other-schema one is refused unchanged.
fn open_projection_index(directory: &Path, parent: &Value, r: &Request) -> Result<(Connection, Guard)> {
    if !crate::platform::verify_private(directory, crate::platform::Private::Directory).map_err(|_| INDEX_NOT_READY.to_owned())? {
        return Err("Public history index directory must be private".into());
    }
    let key = key(parent, r);
    // Never follows a link or waits on a FIFO; only a private regular file is locked.
    let lock = crate::platform::no_follow(OpenOptions::new().write(true))
        .open(directory.join(format!("{key}.lock")))
        .map_err(|_| INDEX_NOT_READY.to_owned())?;
    if !crate::platform::verify_private_file(&lock).map_err(|e| e.to_string())? {
        return Err("Public history index lock must be private".into());
    }
    let guard = Guard::exclusive(lock).map_err(|e| e.to_string())?;
    let path = directory.join(format!("{key}.sqlite3"));
    let file = crate::platform::refuse_link(OpenOptions::new().read(true).write(true))
        .open(&path)
        .map_err(|_| INDEX_NOT_READY.to_owned())?;
    if !crate::platform::verify_private_file(&file).map_err(|e| e.to_string())? {
        return Err("Public history index file must be private".into());
    }
    drop(file);
    let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| e.to_string())?;
    db.busy_timeout(std::time::Duration::from_secs(2)).map_err(|e| e.to_string())?;
    // Connection settings only; neither writes the database.
    db.execute_batch("PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;").map_err(|e| e.to_string())?;
    let tables = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .map_err(|e| e.to_string())?
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let ready = ["meta", "messages", "aliases", "reconcile_work", "receipt_seen", "projection_changes"]
        .iter()
        .all(|table| tables.iter().any(|name| name == table))
        && get(&db, "schema")?.as_deref() == Some("2")
        && get(&db, "epoch")?.is_some()
        && get(&db, "secret")?.is_some();
    if !ready {
        return Err(INDEX_NOT_READY.into());
    }
    Ok((db, guard))
}
/// The identity of a projection request, checked as `history` checks its own,
/// with its epoch and generation range checked separately.
fn validate_projection(p: &ProjectionRequest) -> Result<Request> {
    let request = Request {
        request_id: p.request_id.clone(),
        expected_run_id: p.expected_run_id.clone(),
        expected_conversation_id: p.expected_conversation_id.clone(),
        archive_id: p.archive_id.clone(),
        agent_id: p.agent_id.clone(),
        limit: None,
        before: None,
        after: None,
        around: None,
        around_incoming_seq: None,
        history_epoch: None,
    };
    validate(&request)?;
    if p.history_epoch.is_empty() || p.history_epoch.len() > 128 || p.history_epoch.chars().any(char::is_control) {
        return Err("Invalid exact history epoch".into());
    }
    if p.projection_after < 0 || p.projection_until < p.projection_after {
        return Err("Invalid projection range".into());
    }
    Ok(request)
}
/// Admits a projection read against the open index. The read never indexes
/// sources or reconciles: it resolves against the index that ordinary history
/// calls settled, so it refuses while reconciliation work is pending or the last
/// reconciliation did not finish. Its epoch must be the current one, checked
/// before anything else, and `until` no later than that epoch's newest change.
/// Returns that newest generation.
fn admit_projection(db: &Connection, p: &ProjectionRequest) -> Result<i64> {
    if get(db, "epoch")?.as_deref() != Some(p.history_epoch.as_str()) {
        return Err("History epoch changed; reopen its history".into());
    }
    let pending: bool = db
        .query_row("SELECT EXISTS(SELECT 1 FROM reconcile_work)", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if pending || get(db, "reconciled")?.as_deref() != Some("1") {
        return Err("History is still indexing; try again".into());
    }
    let generation = projection_generation(db)?;
    if p.projection_until > generation {
        return Err("Projection position is past the newest change".into());
    }
    Ok(generation)
}
/// One projection read as a single transaction: admission, the generation,
/// successor resolution and the encoded response come from one snapshot. Any
/// refusal rolls back, so it changes nothing and its response is never sent.
fn serve_projection(db: &Connection, p: &ProjectionRequest, mut envelope: Value) -> Result<Value> {
    db.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;
    let served = admit_projection(db, p).and_then(|generation| {
        envelope["history_epoch"] = json!(p.history_epoch);
        envelope["projection_generation"] = json!(generation);
        projection_page(db, envelope, p.projection_after, p.projection_until)
    });
    match served {
        Ok(response) => {
            db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            Ok(response)
        }
        Err(error) => {
            db.execute_batch("ROLLBACK").map_err(|e| e.to_string())?;
            Err(error)
        }
    }
}
/// The row that now stands for history row `id`: the row itself, else the owner
/// of its `history:<id>` alias, which every merge repoints to its survivor.
fn successor(db: &Connection, id: &str) -> Result<Option<String>> {
    let own: Option<String> = db
        .query_row("SELECT id FROM messages WHERE id=?", [id], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    if own.is_some() {
        return Ok(own);
    }
    db.query_row("SELECT m.id FROM aliases a JOIN messages m ON m.id=a.id WHERE a.original=?", [format!("history:{id}")], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())
}
/// The changes `after < generation <= until` as one response of at most
/// PROJECTION_ENTRIES entries and PROJECTION_RESPONSE_BYTES on the wire, the
/// `envelope` included. Each entry is resolved retired (no successor), resolved
/// refreshed (its successor as a page row) or unresolved (no successor, with a
/// reason). The first entry is always returned: an oversize successor first has
/// its detail clipped as a page clips it, then becomes unresolved `oversize`.
/// `complete` means every change up to `until` is in the response.
fn projection_page(db: &Connection, mut envelope: Value, after: i64, until: i64) -> Result<Value> {
    let secret = get(db, "secret")?.ok_or("History cursor key unavailable")?;
    let epoch = get(db, "epoch")?.ok_or("History epoch unavailable")?;
    let mut query = db
        .prepare("SELECT generation,id,change FROM projection_changes WHERE generation>? AND generation<=? ORDER BY generation LIMIT ?")
        .map_err(|e| e.to_string())?;
    let changes = query
        .query_map(params![after, until, PROJECTION_ENTRIES as i64 + 1], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let unresolved = |generation: i64, id: &str, change: &str, reason: &str| {
        json!({"generation":generation,"history_id":id,"change":change,"resolution":"unresolved","successor":null,"reason":reason})
    };
    // The largest values the final fields can take, so the result cannot outgrow its charge.
    envelope["projection_changes"] = json!([]);
    envelope["next_projection_after"] = json!(i64::MAX);
    envelope["complete"] = json!(false);
    let mut bytes = ascii_wire_size(&envelope);
    let (mut entries, mut stopped) = (Vec::new(), false);
    for (generation, id, change) in changes.iter().take(PROJECTION_ENTRIES) {
        let mut entry = match (change.as_str(), successor(db, id)?) {
            ("retired", None) => {
                json!({"generation":generation,"history_id":id,"change":"retired","resolution":"resolved","successor":null})
            }
            ("refreshed", Some(row)) => {
                let (at, ordinal, body, incoming): (f64, String, String, Option<i64>) = db
                    .query_row("SELECT at,ordinal,body,incoming FROM messages WHERE id=?", [&row], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })
                    .map_err(|e| e.to_string())?;
                let successor = public_row(db, &secret, &epoch, true, (row.as_str(), at, ordinal.as_str(), body.as_str(), incoming))?;
                json!({"generation":generation,"history_id":id,"change":"refreshed","resolution":"resolved","successor":successor})
            }
            ("retired", Some(_)) => unresolved(*generation, id.as_str(), change.as_str(), "unexpected_successor"),
            _ => unresolved(*generation, id.as_str(), change.as_str(), "no_successor"),
        };
        let mut cost = ascii_wire_size(&entry) + 1;
        if bytes + cost > PROJECTION_RESPONSE_BYTES {
            if !entries.is_empty() {
                stopped = true;
                break;
            }
            if entry["successor"].is_object() {
                entry["successor"]["detail"] = json!(journal::clipped(&entry["successor"]["detail"], 500));
                entry["successor"]["detail_truncated"] = json!(true);
                cost = ascii_wire_size(&entry) + 1;
            }
            if bytes + cost > PROJECTION_RESPONSE_BYTES {
                entry = unresolved(*generation, id.as_str(), change.as_str(), "oversize");
                cost = ascii_wire_size(&entry) + 1;
            }
            if bytes + cost > PROJECTION_RESPONSE_BYTES {
                return Err("Projection response envelope exceeds its bound".into());
            }
        }
        bytes += cost;
        entries.push(entry);
    }
    let complete = !stopped && changes.len() <= PROJECTION_ENTRIES;
    let next = if complete { until } else { entries.last().and_then(|entry| entry["generation"].as_i64()).unwrap_or(after) };
    envelope["projection_changes"] = json!(entries);
    envelope["next_projection_after"] = json!(next);
    envelope["complete"] = json!(complete);
    Ok(envelope)
}
/// `hgs history-projection <session> --json`: the retired and refreshed history
/// rows of the current epoch after `projection_after`, up to `projection_until`,
/// for a phone to apply to its cached window. Read-only apart from deleting
/// another epoch's ledger entries.
pub(super) fn projection_dispatch(args: &[String]) -> Result<i32> {
    if args.len() != 2 || args[1] != "--json" {
        return Err("usage: hgs history-projection <session> --json".into());
    }
    let mut bytes = Vec::new();
    io::stdin().take(16 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > 16 * 1024 {
        return Err("History projection request exceeds bounds".into());
    }
    let projection: ProjectionRequest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let request = validate_projection(&projection)?;
    let name = &args[0];
    let parent = record(name, &request)?;
    let directory = crate::platform::canonical(root()).map_err(|_| INDEX_NOT_READY.to_owned())?.join("public_history");
    let (db, _guard) = open_projection_index(&directory, &parent, &request)?;
    let mut envelope = json!({"request_id":request.request_id,"name":name,"run_id":request.expected_run_id,
        "conversation_id":if let Some(id) = &request.agent_id {
            format!("{}/{}", request.expected_conversation_id, id)
        } else {
            request.expected_conversation_id.clone()
        }});
    if let Some(id) = &request.agent_id {
        envelope["agent_id"] = json!(id);
        envelope["parent_conversation_id"] = json!(request.expected_conversation_id);
    }
    if let Some(id) = &request.archive_id {
        envelope["archive_id"] = json!(id);
    }
    let result = serve_projection(&db, &projection, envelope)?;
    // A fresh read result cannot quietly switch to a replacement run during I/O.
    let _current = record(name, &request)?;
    println!("{result}");
    Ok(0)
}
/// Ends a history call's transaction and reads its page: incoming numbers are
/// assigned, then the reported projection generation is that of the epoch the
/// page returns, since reconciliation and incoming numbering can replace it.
fn respond(db: &Connection, r: &Request, mut complete: bool, mut status: Value) -> Result<Value> {
    let total = incoming(db, complete)?;
    if complete && total.is_none() {
        complete = false;
        status["canonical"] = json!("indexing");
    }
    status["projection_upgrade"]["generation"] = json!(projection_generation(db)?);
    db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
    page(db, r, complete, total, status)
}
pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    if args.len() != 2 || args[1] != "--json" {
        return Err("usage: hgs history <session> --json".into());
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 16 * 1024 {
        return Err("History request exceeds bounds".into());
    }
    let request: Request = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate(&request)?;
    let name = &args[0];
    let mut parent = record(name, &request)?;
    // A fork recorded before its lineage finds its source once and saves it,
    // as `inspect` does, before this index's lock is taken (zerus-b4qk.1).
    if request.archive_id.is_none()
        && !string(&parent, "fork_parent_id").is_empty()
        && !parent["fork_lineage"].is_array()
    {
        let events = root().join("events.sqlite3");
        if let Ok(source) = Connection::open_with_flags(events, OpenFlags::SQLITE_OPEN_READ_ONLY)
        {
            journal::save_derived_lineage(&source, &mut parent);
        }
    }
    let native = native_record(&parent, &request);
    let (db, _guard) = index(&parent, &request)?;
    db.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| e.to_string())?;
    // Whether this index had rows before this call: only such an index gets
    // the one-time inherited backfill reset.
    let existed: bool = db
        .query_row("SELECT EXISTS(SELECT 1 FROM messages)", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let (provider_complete, mut status) = if string(&parent,"agent")=="opencode" {
        let detail=opencode::inspect(name,request.agent_id.as_deref())?;
        // An unreadable service or an OpenCode 1 binding has no history to index.
        if let Some(reason)=detail["inspection_reason"].as_str() {return Err(reason.into());}
        // A child view carries its exact parent separately from its own ID.
        if detail["run_id"]!=parent["run_id"] || detail.get("parent_conversation_id").unwrap_or(&detail["conversation_id"])!=&parent["conversation_id"]
            || detail.get("agent_id").map(|id| Some(id.as_str().unwrap_or_default())!=request.agent_id.as_deref()).unwrap_or(request.agent_id.is_some()) {return Err("OpenCode history identity changed".into());}
        for row in opencode_history(&detail) { add(&db,&key(&parent,&request),row.clone(),"journal")?; }
        let done=detail["history_truncated"]==false;
        (done,json!({"provider":if done{"complete"}else{"partial_native_window"}}))
    } else if string(&parent, "agent") == "dsh" {
        let detail = dsh::inspect(name, request.agent_id.as_deref())?;
        if detail["run_id"] != parent["run_id"]
            || detail
                .get("parent_conversation_id")
                .unwrap_or(&detail["conversation_id"])
                != &parent["conversation_id"]
        {
            return Err("DeepSeek history identity changed".into());
        }
        for row in detail["events"].as_array().into_iter().flatten() {
            add(&db, &key(&parent, &request), row.clone(), "journal")?;
        }
        let done = detail["history_truncated"] == false;
        (
            done,
            json!({"provider":if done{"complete"}else{"partial_resident_adapter"}}),
        )
    } else {
        sources(&db, &parent, &request, &native)?
    };
    let journal_complete = if ["dsh","opencode"].contains(&string(&parent,"agent")) {
        true
    } else {
        journal(&db, &parent, &request)?
    };
    let inherited_complete = inherited(&db, &parent, &request, existed)?;
    status["inherited"] = json!(if inherited_complete {"complete"} else {"indexing"});
    let receipts_complete = receipts(&db, &parent, &request)?;
    let voice_after = get(&db,"voice_seq")?.and_then(|seq|seq.parse::<i64>().ok()).unwrap_or(0);
    let (voice_rows,voice_seq,voice_complete) = voice::history_page(&parent,request.agent_id.as_deref(),voice_after)?;
    for row in voice_rows {
        add(&db, &key(&parent, &request), row, "voice")?;
    }
    set(&db,"voice_seq",&voice_seq.to_string())?;
    status["voice"] = json!(if voice_complete {"complete"} else {"indexing"});
    let source_complete = provider_complete && journal_complete && inherited_complete && receipts_complete && voice_complete;
    status["projection_upgrade"] = advance_projection_upgrade(&db, &key(&parent, &request), &native)?;
    let upgrade_scan_done = advance_reconcile_upgrade(&db)?;
    let mut voice_reconcile_complete = false;
    if source_complete {
        reconcile(&db)?;
        voice_reconcile_complete = reconcile_voice(&db, &parent, &request)?;
        reconcile(&db)?;
    }
    status["voice_reconciliation"] = json!(if voice_reconcile_complete {"complete"} else {"indexing"});
    let upgrade_done = finish_reconcile_upgrade(&db, upgrade_scan_done)?;
    if let Some(reset) = get(&db, "order_reset")? {
        status["canonical_order_reset"] = json!(reset);
    }
    let gap = get(&db, "gap")?;
    if let Some(gap) = &gap {
        status["gap"] = json!(gap);
    }
    status["journal"] = json!(if journal_complete {
        "complete"
    } else {
        "indexing"
    });
    status["attachments"] = json!(if receipts_complete {
        "complete"
    } else {
        "indexing"
    });
    let mut source_bytes = 0u64;
    let mut known_bytes = 0u64;
    let mut progress = db
        .prepare("SELECT key,value FROM meta WHERE key LIKE 'offset:%' OR key LIKE 'length:%'")
        .map_err(|e| e.to_string())?;
    for entry in progress
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
    {
        let (name, value) = entry.map_err(|e| e.to_string())?;
        let value = value.parse::<u64>().unwrap_or(0);
        if name.starts_with("offset:") {
            source_bytes += value;
        } else {
            known_bytes += value;
        }
    }
    status["progress"] = json!({"source_bytes":source_bytes,"known_source_bytes":known_bytes,"journal_seq":get(&db,"journal_seq")?,"receipt_files":db.query_row("SELECT count(*) FROM receipt_seen",[],|r|r.get::<_,i64>(0)).map_err(|e|e.to_string())?,"canonical_pending":db.query_row("SELECT count(*) FROM reconcile_work",[],|r|r.get::<_,i64>(0)).map_err(|e|e.to_string())?,"assigned_incoming":db.query_row("SELECT count(*) FROM messages WHERE incoming IS NOT NULL",[],|r|r.get::<_,i64>(0)).map_err(|e|e.to_string())?,"reconcile_upgrade_after":get(&db,"reconcile_upgrade_after")?,"reconcile_version":get(&db,"reconcile_version")?});
    let complete = source_complete
        && voice_reconcile_complete
        && upgrade_done
        && get(&db, "reconciled")?.as_deref() == Some("1")
        && gap.is_none();
    status["canonical"] = json!(if !upgrade_scan_done
        || (source_complete && (!voice_reconcile_complete || !upgrade_done || get(&db, "reconciled")?.as_deref() != Some("1")))
    {
        "indexing"
    } else {
        "complete"
    });
    let mut result = respond(&db, &request, complete, status)?;
    // A fresh read result cannot quietly switch to a replacement run during I/O.
    let _current = record(name, &request)?;
    result["request_id"] = json!(request.request_id);
    result["name"] = json!(name);
    result["run_id"] = json!(request.expected_run_id);
    result["conversation_id"] = json!(if let Some(id) = &request.agent_id {
        format!("{}/{}", request.expected_conversation_id, id)
    } else {
        request.expected_conversation_id.clone()
    });
    if let Some(id) = &request.agent_id {
        result["agent_id"] = json!(id);
        result["parent_conversation_id"] = json!(request.expected_conversation_id);
    }
    if let Some(id) = &request.archive_id {
        result["archive_id"] = json!(id);
    }
    if ascii_wire_size(&result) > 1024 * 1024 - 4096 {
        return Err("History response exceeds its transport envelope".into());
    }
    println!("{result}");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_ascii_budget_matches_the_shared_wire_literals() {
        let fixture: Value = serde_json::from_str(include_str!("../../mobile/android/app/src/test/resources/history-projection-wire.json")).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            assert_eq!(ascii_wire_size(&case["value"]), case["bytes"].as_u64().unwrap() as usize, "{}", case["ascii"]);
        }
    }

    #[test]
    fn voice_matching_requires_source_boundary_and_keeps_exact_journal_alias() {
        let sources = json!([["/synthetic/transcript.jsonl",7,8,"header"]]);
        let correlation = json!({"native_receipt_status":"submitted","native_submitted_at":100.,
            "boundary":{"transcript":"/synthetic/transcript.jsonl","source_device":7,
                "source_inode":8,"source_bytes":200,"journal_seq":40}});
        let mut native = json!({"source_offset":"0:200","at":101.});
        assert!(voice_after_boundary(&native,&correlation,&sources));
        native["source_offset"] = json!("0:199");
        assert!(!voice_after_boundary(&native,&correlation,&sources));
        native["source_offset"] = json!("0:200");
        assert!(!voice_after_boundary(&native,&correlation,&json!([["/synthetic/transcript.jsonl",7,9,"header"]])));
        assert!(!voice_after_boundary(&json!({"seq":40,"at":101.}),&correlation,&sources));
        assert!(voice_after_boundary(&json!({"seq":41,"at":101.}),&correlation,&sources));
        assert!(!voice_after_boundary(&json!({"seq":41,"at":500.}),&correlation,&sources));
        assert!(voice_binding_matches(&json!({"original_ids":["journal:codex:You:native:41"]}),&json!({"journal_seq":41}),&correlation,&sources));
        assert!(!voice_binding_matches(&json!({"original_ids":["journal:codex:You:native:141"]}),&json!({"journal_seq":41}),&correlation,&sources));
    }

    /// #1235 review R3: a fork's voice message never binds to a row it
    /// inherited, even one with the same words.
    #[test]
    fn a_forks_inherited_rows_are_never_voice_candidates() {
        let detail = json!({
            "message_events": [
                {"seq":1,"type":"UserPromptSubmit","agent_id":"","detail":"Same words","inherited_from":"codex/p/source"},
                {"seq":9,"type":"UserPromptSubmit","agent_id":"","detail":"Same words"},
            ],
            "events": [
                {"seq":2,"type":"TurnStarted","agent_id":"main","detail":"Same words","inherited_from":"codex/p/source"},
                {"seq":10,"type":"UserPromptSubmit","agent_id":"child","detail":"Same words"},
                {"seq":11,"type":"Stop","agent_id":"","detail":"Same words"},
            ],
        });
        let seqs: Vec<_> = voice_candidates(&detail)
            .iter()
            .filter_map(|event| event["seq"].as_i64())
            .collect();
        assert_eq!(seqs, [9]);
    }

    #[test]
    fn voice_offset_binding_rejects_replaced_sources_and_conflicting_native_ids() {
        let sources = json!([["/synthetic/transcript.jsonl",7,8,"header"]]);
        let replacement = json!([["/synthetic/transcript.jsonl",7,9,"header"]]);
        let correlation = json!({"native_receipt_status":"submitted","native_submitted_at":100.,
            "boundary":{"transcript":"/synthetic/transcript.jsonl","source_device":7,
                "source_inode":8,"source_bytes":200,"journal_seq":40}});
        let mut native = json!({"source":"codex_transcript","source_offset":"0:200","at":101.});
        let mut binding = json!({"provider_source":"codex_transcript","provider_source_offset":"0:200"});
        assert!(voice_binding_matches(&native,&binding,&correlation,&sources));
        assert!(!voice_binding_matches(&native,&binding,&correlation,&replacement));
        binding["provider_message_id"] = json!("original-id");
        native["message_id"] = json!("another-id");
        assert!(!voice_binding_matches(&native,&binding,&correlation,&sources));
        native["message_id"] = json!("original-id");
        assert!(voice_binding_matches(&native,&binding,&correlation,&replacement));
    }

    #[test]
    fn voice_card_survives_native_refresh_and_state_revisions() {
        let db = database();
        let mut voice = row("UserMessage","voice-id","",1.);
        voice["source"] = json!("hgs_voice");
        voice["attachments"] = json!([{"request_id":"voice-id","index":0,"mime":"audio/wav"}]);
        voice["voice"] = json!({"id":"voice-id","state":"transcribing","revision":2});
        add(&db,"conversation",voice.clone(),"voice").unwrap();
        let native = row("UserMessage","native-id","Synthetic voice",2.);
        add(&db,"conversation",native.clone(),"provider").unwrap();
        let id = |origin:&str| db.query_row("SELECT id FROM aliases WHERE original=?",[origin],|r|r.get::<_,String>(0)).unwrap();
        let voice_id = id(&original(&voice,"voice").unwrap());
        let native_id = id(&original(&native,"provider").unwrap());
        merge_row(&db,&voice_id,&native_id,&voice.to_string(),"UserMessage").unwrap();
        add(&db,"conversation",native,"provider").unwrap();
        voice["detail"] = json!("Synthetic voice");
        voice["voice"]["state"] = json!("submitted");
        voice["voice"]["revision"] = json!(3);
        add(&db,"conversation",voice,"voice").unwrap();
        let (count,body):(i64,String) = db.query_row("SELECT count(*),body FROM messages",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        let body:Value = serde_json::from_str(&body).unwrap();
        assert_eq!(count,1);
        assert_eq!(body["voice"]["state"],"submitted");
        assert_eq!(body["source"],"hgs_voice");
    }
    #[test]
    fn an_opencode_voice_binding_finds_its_exact_native_row() {
        let db = database();
        let binding = json!({"provider_message_id":"msg_V","provider_source":"opencode_native"});
        let native = json!({"type":"UserPromptSubmit","detail":"Spoken text","source":"opencode_native","message_id":"msg_V","at":5000.0});
        assert!(voice_binding_matches(
            &native,
            &binding,
            &json!({}),
            &json!([])
        ));
        let mut other = native.clone();
        other["message_id"] = json!("msg_W");
        assert!(!voice_binding_matches(
            &other,
            &binding,
            &json!({}),
            &json!([])
        ));
        assert!(bound_opencode_row(&db, &binding).unwrap().is_none());
        add(&db, "conversation", other, "journal").unwrap();
        add(&db, "conversation", native, "journal").unwrap();
        // Admitted from the queue long after delivery, outside the time window.
        let (_, row) = bound_opencode_row(&db, &binding).unwrap().unwrap();
        assert_eq!(row["message_id"], "msg_V");
        assert_eq!(row["detail"], "Spoken text");
        assert!(bound_opencode_row(&db, &json!({"journal_seq":3}))
            .unwrap()
            .is_none());
    }
    #[test]
    fn opencode_rows_are_indexed_once_they_are_final() {
        let db = database();
        let user = json!({"type":"UserPromptSubmit","detail":"hello","source":"opencode_native","message_id":"msg_U","at":1.0});
        let reply = |text: &str, streaming: bool| {
            let mut row = json!({"type":"AgentMessage","detail":text,"source":"opencode_native","message_id":"msg_A","reply_id":"msg_A","at":2.0});
            if streaming {
                row["streaming"] = json!(true);
            }
            row
        };
        let queued = json!({"type":"UserPromptSubmit","detail":"later","source":"opencode_native","message_id":"msg_Q","at":3.0,"delivery_state":"queued"});
        let index = |events: Value| {
            for row in opencode_history(&json!({ "events": events })) {
                add(&db, "conversation", row.clone(), "journal").unwrap();
            }
        };
        index(json!([user, reply("Partial", true), queued]));
        let bodies = |db: &Connection| -> Vec<Value> {
            let mut query = db.prepare("SELECT body FROM messages ORDER BY at").unwrap();
            query
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|body| serde_json::from_str(&body.unwrap()).unwrap())
                .collect()
        };
        assert_eq!(bodies(&db).len(), 1);
        for _ in 0..2 {
            index(json!([user, reply("Partial and final", false)]));
        }
        let stored = bodies(&db);
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[1]["detail"], "Partial and final");
    }
    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE messages(id TEXT PRIMARY KEY,at REAL NOT NULL,role TEXT NOT NULL,stream TEXT NOT NULL,kind TEXT NOT NULL,ordinal TEXT NOT NULL,body TEXT NOT NULL,incoming INTEGER); CREATE TABLE aliases(original TEXT PRIMARY KEY,id TEXT NOT NULL); CREATE TABLE reconcile_work(id TEXT PRIMARY KEY); CREATE TABLE receipt_seen(name TEXT PRIMARY KEY,stamp TEXT NOT NULL); CREATE TABLE projection_changes(generation INTEGER PRIMARY KEY AUTOINCREMENT,epoch TEXT NOT NULL,id TEXT NOT NULL,change TEXT NOT NULL CHECK(change IN ('retired','refreshed')));").unwrap();
        set(&db, "epoch", "test-epoch").unwrap();
        set(&db, "secret", &"a".repeat(64)).unwrap();
        db
    }
    fn request() -> Request {
        serde_json::from_value(json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":"run","expected_conversation_id":"conversation"})).unwrap()
    }
    fn row(kind: &str, id: &str, text: &str, at: f64) -> Value {
        json!({"type":kind,"message_id":id,"detail":text,"at":at,"source":if kind=="Stop"{""}else{"codex_transcript"},"source_offset":"0:12"})
    }
    #[test]
    fn revisions_and_equal_time_provenance_are_stable() {
        let db = database();
        add(
            &db,
            "scope",
            row("AgentMessage", "one", "Partial", 1.),
            "provider",
        )
        .unwrap();
        let id: String = db
            .query_row("SELECT id FROM messages", [], |r| r.get(0))
            .unwrap();
        add(
            &db,
            "scope",
            row("AgentMessage", "one", "Complete final", 1.),
            "provider",
        )
        .unwrap();
        let value = page(&db, &request(), false, None, json!({})).unwrap();
        assert_eq!(value["events"].as_array().unwrap().len(), 1);
        assert_eq!(value["events"][0]["history_id"], id);
        assert_eq!(value["events"][0]["detail"], "Complete final");
    }
    #[test]
    fn either_arrival_order_reconciles_and_future_work_is_not_skipped() {
        for provider_first in [false, true] {
            let db = database();
            let hook = row("Stop", "hook", "answer", 10.);
            let full = row("AgentMessage", "provider", "Full answer", 10.);
            if provider_first {
                add(&db, "scope", full.clone(), "provider").unwrap();
                reconcile(&db).unwrap();
            }
            add(&db, "scope", hook, "journal").unwrap();
            if !provider_first {
                add(&db, "scope", full, "provider").unwrap();
            }
            reconcile(&db).unwrap();
            assert_eq!(
                db.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                1
            );
            add(
                &db,
                "scope",
                row("Stop", "next-hook", "next", 20.),
                "journal",
            )
            .unwrap();
            add(
                &db,
                "scope",
                row("AgentMessage", "next-provider", "Full next", 20.),
                "provider",
            )
            .unwrap();
            reconcile(&db).unwrap();
            assert_eq!(
                db.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                2
            );
        }
    }
    #[test]
    fn ambiguous_repeated_messages_remain_distinct() {
        let db = database();
        for id in ["hook-one", "hook-two"] {
            add(&db, "scope", row("Stop", id, "answer", 10.), "journal").unwrap();
        }
        add(
            &db,
            "scope",
            row("AgentMessage", "provider", "Full answer", 10.),
            "provider",
        )
        .unwrap();
        reconcile(&db).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
    }
    #[test]
    fn historical_backfill_resets_epoch_instead_of_inflating_old_watermark() {
        let db = database();
        add(
            &db,
            "scope",
            row("AgentMessage", "later", "Later", 20.),
            "provider",
        )
        .unwrap();
        assert_eq!(incoming(&db, true).unwrap(), Some(1));
        add(
            &db,
            "scope",
            row("AgentMessage", "older", "Older", 10.),
            "provider",
        )
        .unwrap();
        assert_eq!(incoming(&db, true).unwrap(), Some(2));
        assert_ne!(get(&db, "epoch").unwrap().unwrap(), "test-epoch");
    }
    /// #1248 R3: numbering 10k unnumbered incoming rows, as after a fork's
    /// backfill or a long session's first build, stays linear in every call.
    /// The correlated check took about 20 s per call, its SQLite steps growing
    /// with the square of the rows, and the connector stops a call at 2 s.
    #[test]
    fn numbering_many_unnumbered_rows_stays_within_a_linear_step_budget() {
        use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
        // About 0.7M steps per call measured; the quadratic check needs hundreds of millions.
        const BUDGET: usize = 2_000_000;
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        set(&db, "epoch", "test-epoch").unwrap();
        let insert = |id: &str, at: f64, role: &str| {
            db.execute(
                "INSERT INTO messages(id,at,role,stream,kind,ordinal,body) VALUES(?,?,?,'provider','Stop',?,'{}')",
                params![id, at, role, format!("1:{id}")],
            )
            .unwrap();
        };
        db.execute_batch("BEGIN").unwrap();
        for n in 0..10_000 {
            insert(&format!("out{n:05}"), n as f64 * 5., "outgoing");
            insert(&format!("in{n:05}"), n as f64 * 5. + 1., "incoming");
        }
        db.execute_batch("COMMIT").unwrap();
        let steps = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = steps.clone();
        db.progress_handler(1, Some(move || counted.fetch_add(1, Relaxed) >= BUDGET));
        let number = |db: &Connection| {
            for calls in 1..=20 {
                steps.store(0, Relaxed);
                let total = incoming(db, true).expect("numbering stays within its step budget");
                if let Some(total) = total {
                    return (total, calls);
                }
            }
            panic!("numbering never completed");
        };
        assert_eq!(number(&db), (10_000, 11));
        assert_eq!(get(&db, "order_reset").unwrap(), None);
        // A row dated before the numbered ones still resets the numbering.
        insert("early", 0.5, "incoming");
        assert_eq!(number(&db), (10_001, 11));
        assert_eq!(get(&db, "order_reset").unwrap().as_deref(), Some("historical_backfill"));
        db.progress_handler(0, None::<fn() -> bool>);
    }
    /// #1248 N8/N9: an inherited backfill drops only the fork's provider rows
    /// dated before its creation, returns their bytes to the budget and numbers
    /// incoming rows again.
    #[test]
    fn inherited_backfill_drops_only_earlier_provider_copies_and_renumbers() {
        let db = database();
        let bytes = |db: &Connection| get(db, "bytes").unwrap().unwrap().parse::<usize>().unwrap();
        let mut journal = row("UserPromptSubmit", "asked", "Asked", 50.);
        journal["seq"] = json!(1);
        add(&db, "scope", journal, "journal").unwrap();
        add(&db, "scope", row("AgentMessage", "own", "Own reply", 150.), "provider").unwrap();
        let kept = bytes(&db);
        add(&db, "scope", row("AgentMessage", "copy", "Copied reply", 10.), "provider").unwrap();
        assert!(bytes(&db) > kept);
        assert_eq!(incoming(&db, true).unwrap(), Some(2));
        backfill(&db, &json!({"created": 100.})).unwrap();
        let details = db
            .prepare("SELECT json_extract(body,'$.detail') FROM messages ORDER BY at,ordinal,id")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(details, ["Asked", "Own reply"]);
        assert_eq!(bytes(&db), kept);
        assert_eq!(incoming(&db, true).unwrap(), Some(1));
        assert_eq!(get(&db, "order_reset").unwrap().as_deref(), Some("inherited_backfill"));
    }
    #[test]
    fn unicode_pages_fit_and_cursors_do_not_skip_rows() {
        let db = database();
        for n in 0..100 {
            add(
                &db,
                "scope",
                row(
                    "AgentMessage",
                    &format!("id-{n}"),
                    &"😀".repeat(8000),
                    n as f64,
                ),
                "provider",
            )
            .unwrap();
        }
        let mut r = request();
        let first = page(&db, &r, false, None, json!({})).unwrap();
        assert!(ascii_wire_size(&first) < 1024 * 1024);
        let first_rows = first["events"].as_array().unwrap();
        assert!(first_rows.len() < 100);
        r.before = first["next_before"].as_str().map(str::to_owned);
        let previous = page(&db, &r, false, None, json!({})).unwrap();
        let last = previous["events"].as_array().unwrap().last().unwrap();
        assert_eq!(
            last["at"].as_f64().unwrap() + 1.,
            first_rows[0]["at"].as_f64().unwrap()
        );
    }
    fn attachment_row(id: &str, submitted: &str, at: f64) -> Value {
        json!({"type":"UserPromptSubmit","source":"hgs_delivery","message_id":id,"at":at,
            "detail":"Compare images","submitted_text":"Compare images","native_match_hash":hash(submitted),
            "attachments":[{"request_id":id,"index":0,"name":"image.png","mime":"image/png","bytes":120}]})
    }
    #[test]
    fn delayed_attachment_native_acknowledgements_merge_in_either_order_and_survive_refresh() {
        let submitted = "Compare images\n\nInspect the attached image at \"/synthetic/request-one/image.png\"";
        for native_first in [false, true] {
            let db = database();
            if native_first {
                add(&db,"scope",row("UserMessage","native",submitted,202.),"provider").unwrap();
                reconcile(&db).unwrap();
            }
            add(&db,"scope",attachment_row("receipt",submitted,100.),"attachment").unwrap();
            reconcile(&db).unwrap();
            if !native_first {
                add(&db,"scope",row("UserMessage","native",submitted,202.),"provider").unwrap();
                reconcile(&db).unwrap();
            }
            let result = page(&db,&request(),true,Some(0),json!({})).unwrap();
            assert_eq!(result["events"].as_array().unwrap().len(),1);
            assert_eq!(result["events"][0]["detail"],"Compare images");
            assert!(result["events"][0]["original_ids"].as_array().unwrap().len()>=2);
            add(&db,"scope",row("UserMessage","native",submitted,202.),"provider").unwrap();
            reconcile(&db).unwrap();
            let refreshed = page(&db,&request(),true,Some(0),json!({})).unwrap();
            assert_eq!(refreshed["events"][0]["detail"],"Compare images");
            assert!(!refreshed.to_string().contains("/synthetic/"));
        }
    }
    #[test]
    fn claude_compaction_rows_stay_out_and_a_framed_paste_merges_with_its_receipt() {
        // The shape Claude 2.1.296 writes for /compact followed by a phone message with an image.
        let temp = tempfile::tempdir().unwrap();
        let conversation = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("{conversation}.jsonl"));
        let submitted = "Compare images\n\nInspect the attached image at \"/synthetic/request-one/image.png\"";
        let user = |uuid: &str, second: u32, content: &str, flags: Value| {
            let mut row = json!({"type":"user","uuid":uuid,"sessionId":conversation,
                "timestamp":format!("2026-10-19T00:07:{second:02}Z"),"message":{"role":"user","content":content}});
            for (key, value) in flags.as_object().unwrap() { row[key] = value.clone(); }
            row.to_string() + "\n"
        };
        std::fs::write(&path, [
            user("typed", 1, "/compact", json!({})),
            user("command", 2, "<command-name>/compact</command-name>\n            <command-message>compact</command-message>\n            <command-args></command-args>", json!({})),
            user("summary", 3, "This session is being continued from a previous conversation that ran out of context.",
                json!({"isVisibleInTranscriptOnly":true,"isCompactSummary":true})),
            user("output", 4, "<local-command-stdout>\u{1b}[2mCompacted (ctrl+o to see full summary)\u{1b}[22m</local-command-stdout>", json!({})),
            user("pasted", 5, &format!("\n\n<pasted_content id=\"c9a0\">\n{submitted}\n</pasted_content id=\"c9a0\">\n"), json!({"origin":{"kind":"human"}})),
        ].concat()).unwrap();
        let native = json!({"agent":"claude","conversation_id":conversation,"transcript":path});
        let db = database();
        let (complete, _) = sources(&db, &native, &request(), &native).unwrap();
        assert!(complete);
        add(&db,"scope",attachment_row("receipt",submitted,100.),"attachment").unwrap();
        for _ in 0..4 { reconcile(&db).unwrap(); }
        let result = page(&db,&request(),true,Some(0),json!({})).unwrap();
        let details: Vec<_> = result["events"].as_array().unwrap().iter().map(|row| row["detail"].clone()).collect();
        assert_eq!(details, [json!("/compact"), json!("Compare images")]);
        assert!(!result.to_string().contains("/synthetic/"));
    }
    #[test]
    fn old_claude_projections_are_retired_or_refreshed_in_place_without_an_epoch_reset() {
        let temp = tempfile::tempdir().unwrap();
        let conversation = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("{conversation}.jsonl"));
        let submitted = "Compare images\n\nInspect the attached image at \"/synthetic/request-one/image.png\"";
        let command = "<command-name>/compact</command-name>\n            <command-message>compact</command-message>\n            <command-args></command-args>";
        let output = "<local-command-stdout>\u{1b}[2mCompacted (ctrl+o to see full summary)\u{1b}[22m</local-command-stdout>";
        let summary = "This session is being continued from a previous conversation that ran out of context.";
        let framed = format!("\n\n<pasted_content id=\"c9a0\">\n{submitted}\n</pasted_content id=\"c9a0\">\n");
        // Each Claude 2.1.296 line with the public row the projection before zerus-e9qx stored for it.
        let lines = [
            ("typed", 1, json!({"message":{"role":"user","content":"/compact"}}), "/compact".to_owned()),
            ("command", 2, json!({"message":{"role":"user","content":command}}), command.to_owned()),
            ("summary", 3, json!({"isCompactSummary":true,"isVisibleInTranscriptOnly":true,"message":{"role":"user","content":summary}}), summary.to_owned()),
            ("output", 4, json!({"message":{"role":"user","content":output}}),
                "<local-command-stdout>Compacted (ctrl+o to see full summary)</local-command-stdout>".to_owned()),
            ("pasted", 5, json!({"origin":{"kind":"human"},"message":{"role":"user","content":framed}}), format!("\n\n{submitted}\n")),
        ];
        let db = database();
        let mut transcript = String::new();
        for (uuid, second, mut line, old) in lines {
            line["type"] = json!("user");
            line["uuid"] = json!(uuid);
            line["sessionId"] = json!(conversation);
            line["timestamp"] = json!(format!("2026-10-19T00:07:{second:02}Z"));
            let offset = transcript.len();
            transcript += &(line.to_string() + "\n");
            add(&db, "scope", json!({"type":"UserPromptSubmit","source":"claude_transcript","at":1_792_368_000.0 + second as f64,
                "message_id":uuid,"agent_id":"","detail":old,"source_offset":format!("0:{offset}")}), "provider").unwrap();
        }
        std::fs::write(&path, transcript).unwrap();
        add(&db, "scope", json!({"type":"AgentMessage","source":"claude_transcript","at":1_792_368_010.0,"message_id":"reply",
            "agent_id":"","detail":"Done.","source_offset":"0:999999"}), "provider").unwrap();
        db.execute("UPDATE messages SET incoming=7 WHERE role='incoming'", []).unwrap();
        add(&db,"scope",attachment_row("receipt",submitted,1_792_368_005.5),"attachment").unwrap();
        for _ in 0..4 { reconcile(&db).unwrap(); }
        // Before the upgrade the old rows show, and the framed copy stays beside its receipt.
        let old = page(&db,&request(),true,Some(7),json!({})).unwrap();
        assert_eq!(old["events"].as_array().unwrap().len(), 7);
        let owner = |uuid: &str| -> String {
            db.query_row("SELECT id FROM aliases WHERE original=?", [format!("provider:claude_transcript:You:native:{uuid}")], |r| r.get(0)).unwrap()
        };
        let mut expected: Vec<(String, String)> = [("command","retired"),("summary","retired"),("output","retired"),("pasted","refreshed")]
            .iter().map(|(uuid, change)| (owner(uuid), change.to_string())).collect();
        let native = json!({"agent":"claude","conversation_id":conversation,"transcript":path});
        let status = advance_projection_upgrade(&db, "scope", &native).unwrap();
        assert_eq!(status, json!({"state":"complete","generation":4,"refreshed":1,"retired":3,"retained":0,"declined":0,"unverified":0}));
        // Each change has its ledger entry, in this epoch, in the order made.
        let ledger: Vec<(i64, String, String, String)> = db.prepare("SELECT generation,epoch,id,change FROM projection_changes ORDER BY generation")
            .unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(ledger.iter().map(|entry| entry.0).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert!(ledger.iter().all(|entry| entry.1 == "test-epoch"));
        let mut changes: Vec<(String, String)> = ledger.into_iter().map(|entry| (entry.2, entry.3)).collect();
        changes.sort();
        expected.sort();
        assert_eq!(changes, expected);
        for _ in 0..4 { reconcile(&db).unwrap(); }
        let result = page(&db,&request(),true,Some(7),json!({})).unwrap();
        let details: Vec<_> = result["events"].as_array().unwrap().iter().map(|row| row["detail"].clone()).collect();
        assert_eq!(details, [json!("/compact"), json!("Compare images"), json!("Done.")]);
        assert!(!result.to_string().contains("/synthetic/"));
        // The epoch and every incoming sequence are unchanged.
        assert_eq!(result["history_epoch"], "test-epoch");
        assert_eq!(db.query_row("SELECT incoming FROM messages WHERE role='incoming'",[],|r|r.get::<_,i64>(0)).unwrap(), 7);
        assert_eq!(get(&db, "projection_version").unwrap().as_deref(), Some(PROJECTION_VERSION));
        assert!(get(&db, "projection_upgrade_after").unwrap().is_none());
        std::fs::remove_file(&path).unwrap();
        assert_eq!(advance_projection_upgrade(&db, "scope", &native).unwrap(), json!({"state":"complete","generation":4}));
    }
    #[test]
    fn projection_upgrade_keeps_what_it_cannot_prove_and_resumes_in_bounded_steps() {
        let temp = tempfile::tempdir().unwrap();
        let conversation = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("{conversation}.jsonl"));
        let command = "<command-name>/clear</command-name>\n            <command-message>clear</command-message>\n            <command-args></command-args>";
        let db = database();
        let mut transcript = String::new();
        let mut line = |uuid: &str, session: &str| {
            let offset = transcript.len();
            transcript += &(json!({"type":"user","uuid":uuid,"sessionId":session,"timestamp":"2026-10-19T00:00:00Z",
                "message":{"role":"user","content":command}}).to_string() + "\n");
            offset
        };
        let mut rows = Vec::new();
        for n in 0..PROJECTION_ROWS + 6 {
            let uuid = format!("command-{n:03}");
            rows.push((uuid.clone(), format!("0:{}", line(&uuid, &conversation))));
        }
        // A line of another uuid, another conversation, a position inside a line or past
        // the end proves nothing; a row merged with other evidence is kept.
        let other = line("someone-else", &conversation);
        rows.push(("moved".into(), format!("0:{other}")));
        rows.push(("mid-line".into(), format!("0:{}", other + 1)));
        rows.push(("missing".into(), "0:99999999".into()));
        rows.push(("other-session".into(), format!("0:{}", line("other-session", "another-conversation"))));
        rows.push(("merged".into(), format!("0:{}", line("merged", &conversation))));
        for (n, (uuid, offset)) in rows.iter().enumerate() {
            add(&db, "scope", json!({"type":"UserPromptSubmit","source":"claude_transcript","at":n as f64,"message_id":uuid,
                "agent_id":"","detail":command,"source_offset":offset}), "provider").unwrap();
        }
        let merged: String = db.query_row("SELECT id FROM messages WHERE json_extract(body,'$.message_id')='merged'",[],|r|r.get(0)).unwrap();
        db.execute("INSERT INTO aliases(original,id) VALUES('history:earlier',?)", [&merged]).unwrap();
        std::fs::write(&path, transcript).unwrap();
        // A conversation without its transcript waits rather than marking rows unverified.
        assert_eq!(advance_projection_upgrade(&db, "scope", &json!({"agent":"claude","conversation_id":conversation})).unwrap(),
            json!({"state":"indexing","generation":0}));
        assert!(get(&db, "projection_upgrade_after").unwrap().is_none());
        let native = json!({"agent":"claude","conversation_id":conversation,"transcript":path});
        let first = advance_projection_upgrade(&db, "scope", &native).unwrap();
        assert_eq!(first["state"], "indexing");
        assert!(get(&db, "projection_upgrade_after").unwrap().is_some());
        assert!(get(&db, "projection_version").unwrap().is_none());
        let second = advance_projection_upgrade(&db, "scope", &native).unwrap();
        assert_eq!(second, json!({"state":"complete","generation":PROJECTION_ROWS + 6,"refreshed":0,"retired":PROJECTION_ROWS + 6,
            "retained":1,"declined":0,"unverified":4}));
        let mut left: Vec<String> = db.prepare("SELECT json_extract(body,'$.message_id') FROM messages").unwrap()
            .query_map([],|r|r.get(0)).unwrap().map(|r|r.unwrap()).collect();
        left.sort();
        assert_eq!(left, ["merged","mid-line","missing","moved","other-session"]);
        // Other agents have no old Claude projection: the version is set and nothing is read.
        let codex = database();
        add(&codex, "scope", json!({"type":"UserPromptSubmit","source":"codex_transcript","at":1.0,"message_id":"codex",
            "agent_id":"","detail":command,"source_offset":"0:0"}), "provider").unwrap();
        assert_eq!(advance_projection_upgrade(&codex, "scope", &json!({"agent":"codex"})).unwrap(), json!({"state":"complete","generation":0}));
        assert_eq!(codex.query_row("SELECT count(*) FROM messages",[],|r|r.get::<_,i64>(0)).unwrap(), 1);
    }
    #[test]
    fn a_hook_row_holding_its_provider_merge_is_refreshed_and_meets_its_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let conversation = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("{conversation}.jsonl"));
        let submitted = "Compare images\n\nInspect the attached image at \"/synthetic/request-one/image.png\"";
        let command = "<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>";
        let framed = format!("\n\n<pasted_content id=\"c9a0\">\n{submitted}\n</pasted_content id=\"c9a0\">\n");
        let db = database();
        let mut transcript = String::new();
        // Each line's old provider row, and a hook row with the same old text: the
        // existing reconciliation merges them as it does for every Claude prompt.
        for (uuid, seq, at, content, human, old) in [
            ("pasted", 7001, 1_792_368_005.0, framed.clone(), true, format!("\n\n{submitted}\n")),
            ("command", 7002, 1_792_368_020.0, command.to_owned(), false, command.to_owned()),
        ] {
            let offset = transcript.len();
            let mut line = json!({"type":"user","uuid":uuid,"sessionId":conversation,"timestamp":"2026-10-19T00:07:05Z",
                "message":{"role":"user","content":content}});
            if human {
                line["origin"] = json!({"kind":"human"});
            }
            transcript += &(line.to_string() + "\n");
            add(&db, "scope", json!({"type":"UserPromptSubmit","source":"claude_transcript","at":at,"message_id":uuid,"agent_id":"",
                "detail":old,"source_offset":format!("0:{offset}")}), "provider").unwrap();
            add(&db, "scope", json!({"type":"UserPromptSubmit","seq":seq,"at":at + 0.5,"agent_id":"","detail":old}), "journal").unwrap();
        }
        std::fs::write(&path, transcript).unwrap();
        add(&db, "scope", json!({"type":"AgentMessage","source":"claude_transcript","at":1_792_368_030.0,"message_id":"reply",
            "agent_id":"","detail":"Done.","source_offset":"0:999999"}), "provider").unwrap();
        db.execute("UPDATE messages SET incoming=7 WHERE role='incoming'", []).unwrap();
        for _ in 0..4 { reconcile(&db).unwrap(); }
        let row = |id: &str| -> (String, Value, Vec<String>) {
            let (stream, body): (String, String) = db.query_row("SELECT stream,body FROM messages WHERE id=?", [id],
                |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            let mut aliases: Vec<String> = db.prepare("SELECT original FROM aliases WHERE id=?").unwrap()
                .query_map([id], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
            aliases.sort();
            (stream, serde_json::from_str(&body).unwrap(), aliases)
        };
        let owner = |original: &str| -> String {
            db.query_row("SELECT id FROM aliases WHERE original=?", [original], |r| r.get(0)).unwrap()
        };
        let pasted = owner("provider:claude_transcript:You:native:pasted");
        let (stream, body, before) = row(&pasted);
        assert_eq!(stream, "journal");
        assert_eq!((body["message_id"].clone(), body["source_offset"].clone()), (json!("pasted"), json!("0:0")));
        assert_eq!(before.len(), 3);
        assert!(before.contains(&"journal:journal:You:native:7001".to_owned()));
        assert_eq!(owner("journal:journal:You:native:7002"), owner("provider:claude_transcript:You:native:command"));
        // A clean receipt cannot meet the framed digest yet.
        add(&db,"scope",attachment_row("receipt",submitted,1_792_368_006.0),"attachment").unwrap();
        for _ in 0..4 { reconcile(&db).unwrap(); }
        assert_eq!(db.query_row("SELECT count(*) FROM messages WHERE role='outgoing'",[],|r|r.get::<_,i64>(0)).unwrap(), 3);
        let native = json!({"agent":"claude","conversation_id":conversation,"transcript":path});
        let status = advance_projection_upgrade(&db, "scope", &native).unwrap();
        // The merged private command has hook evidence too, so it stays.
        assert_eq!(status, json!({"state":"complete","generation":1,"refreshed":1,"retired":0,"retained":1,"declined":0,"unverified":0}));
        for _ in 0..4 { reconcile(&db).unwrap(); }
        let (stream, body, after) = row(&pasted);
        assert_eq!((stream.as_str(), body["detail"].clone()), ("attachment", json!("Compare images")));
        assert!(before.iter().all(|alias| after.contains(alias)), "{after:?}");
        assert!(after.contains(&"attachment:hgs_delivery:You:native:receipt".to_owned()));
        assert_eq!(after.len(), 5);
        let result = page(&db,&request(),true,Some(7),json!({})).unwrap();
        let details: Vec<_> = result["events"].as_array().unwrap().iter().map(|row| row["detail"].clone()).collect();
        assert_eq!(details, [json!("Compare images"), json!(command), json!("Done.")]);
        assert_eq!(result["history_epoch"], "test-epoch");
        assert_eq!(db.query_row("SELECT incoming FROM messages WHERE role='incoming'",[],|r|r.get::<_,i64>(0)).unwrap(), 7);
    }
    #[test]
    fn only_an_absent_or_exact_session_id_proves_a_transcript_line() {
        let temp = tempfile::tempdir().unwrap();
        let conversation = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("{conversation}.jsonl"));
        let command = "<command-name>/clear</command-name>\n            <command-message>clear</command-message>\n            <command-args></command-args>";
        let db = database();
        let mut transcript = String::new();
        let sessions = [("absent", None), ("exact", Some(json!(conversation))), ("null", Some(Value::Null)), ("bool", Some(json!(true))),
            ("number", Some(json!(7))), ("array", Some(json!([conversation]))), ("object", Some(json!({"id":conversation}))),
            ("empty", Some(json!(""))), ("other", Some(json!("another-conversation")))];
        for (n, (uuid, session)) in sessions.iter().enumerate() {
            let mut line = json!({"type":"user","uuid":uuid,"timestamp":"2026-10-19T00:00:00Z","message":{"role":"user","content":command}});
            if let Some(session) = session {
                line["sessionId"] = session.clone();
            }
            let offset = transcript.len();
            transcript += &(line.to_string() + "\n");
            add(&db, "scope", json!({"type":"UserPromptSubmit","source":"claude_transcript","at":n as f64,"message_id":uuid,
                "agent_id":"","detail":command,"source_offset":format!("0:{offset}")}), "provider").unwrap();
        }
        std::fs::write(&path, transcript).unwrap();
        let native = json!({"agent":"claude","conversation_id":conversation,"transcript":path});
        let status = advance_projection_upgrade(&db, "scope", &native).unwrap();
        assert_eq!(status, json!({"state":"complete","generation":2,"refreshed":0,"retired":2,"retained":0,"declined":0,"unverified":7}));
        let mut left: Vec<String> = db.prepare("SELECT json_extract(body,'$.message_id') FROM messages").unwrap()
            .query_map([],|r|r.get(0)).unwrap().map(|r|r.unwrap()).collect();
        left.sort();
        assert_eq!(left, ["array","bool","empty","null","number","object","other"]);
    }
    #[test]
    fn projection_upgrade_reads_whole_lines_and_stops_after_its_soft_byte_budget() {
        let temp = tempfile::tempdir().unwrap();
        let conversation = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("{conversation}.jsonl"));
        // Three 2.5 MiB command outputs: the first call reads two whole lines (5 MiB,
        // past its 4 MiB budget), defers the third untouched, and the next call ends.
        let output = format!("<local-command-stdout>{}</local-command-stdout>", "x".repeat(PROJECTION_BYTES * 5 / 8));
        let db = database();
        let mut transcript = String::new();
        for n in 0..3 {
            let uuid = format!("output-{n}");
            let offset = transcript.len();
            transcript += &(json!({"type":"user","uuid":uuid,"sessionId":conversation,"timestamp":"2026-10-19T00:00:00Z",
                "message":{"role":"user","content":output}}).to_string() + "\n");
            add(&db, "scope", json!({"type":"UserPromptSubmit","source":"claude_transcript","at":n as f64,"message_id":uuid,
                "agent_id":"","detail":output,"source_offset":format!("0:{offset}")}), "provider").unwrap();
        }
        std::fs::write(&path, transcript).unwrap();
        let native = json!({"agent":"claude","conversation_id":conversation,"transcript":path});
        let first = advance_projection_upgrade(&db, "scope", &native).unwrap();
        assert_eq!(first, json!({"state":"indexing","generation":2,"refreshed":0,"retired":2,"retained":0,"declined":0,"unverified":0}));
        assert_eq!(db.query_row("SELECT count(*) FROM messages",[],|r|r.get::<_,i64>(0)).unwrap(), 1);
        let second = advance_projection_upgrade(&db, "scope", &native).unwrap();
        assert_eq!(second, json!({"state":"complete","generation":3,"refreshed":0,"retired":3,"retained":0,"declined":0,"unverified":0}));
    }
    #[test]
    fn no_row_changes_without_its_ledger_entry() {
        let temp = tempfile::tempdir().unwrap();
        let conversation = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("{conversation}.jsonl"));
        let command = "<command-name>/clear</command-name>\n            <command-message>clear</command-message>\n            <command-args></command-args>";
        let framed = "\n\n<pasted_content id=\"c9a0\">\nFrom the phone\n</pasted_content id=\"c9a0\">\n";
        // A command record to retire and a framed paste to refresh.
        let setup = || -> (Connection, Value) {
            let db = database();
            let mut transcript = String::new();
            for (uuid, content, human, old) in [("command", command, false, command), ("pasted", framed, true, "\n\nFrom the phone\n")] {
                let offset = transcript.len();
                let mut line = json!({"type":"user","uuid":uuid,"sessionId":conversation,"timestamp":"2026-10-19T00:00:00Z",
                    "message":{"role":"user","content":content}});
                if human {
                    line["origin"] = json!({"kind":"human"});
                }
                transcript += &(line.to_string() + "\n");
                add(&db, "scope", json!({"type":"UserPromptSubmit","source":"claude_transcript","at":1.0,"message_id":uuid,"agent_id":"",
                    "detail":old,"source_offset":format!("0:{offset}")}), "provider").unwrap();
            }
            std::fs::write(&path, transcript).unwrap();
            (db, json!({"agent":"claude","conversation_id":conversation,"transcript":path}))
        };
        let rows = |db: &Connection| -> Vec<(String, String)> {
            db.prepare("SELECT id,body FROM messages ORDER BY id").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap().map(|r| r.unwrap()).collect()
        };
        let ledger = |db: &Connection| -> i64 { db.query_row("SELECT count(*) FROM projection_changes", [], |r| r.get(0)).unwrap() };
        // A full ledger, or a generation at the end of SQLite's integer range, changes nothing.
        for full in ["WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<100000)
                INSERT INTO projection_changes(epoch,id,change) SELECT 'test-epoch','earlier-'||i,'retired' FROM n;",
            "INSERT INTO projection_changes(generation,epoch,id,change) VALUES(9223372036854775807,'test-epoch','last','retired');"] {
            let (db, native) = setup();
            db.execute_batch(full).unwrap();
            let (before, entries) = (rows(&db), ledger(&db));
            for _ in 0..2 {
                assert_eq!(advance_projection_upgrade(&db, "scope", &native).unwrap()["state"], "ledger_full");
            }
            assert_eq!((rows(&db), ledger(&db)), (before, entries));
            assert!(get(&db, "projection_upgrade_after").unwrap().is_none());
            assert!(get(&db, "projection_version").unwrap().is_none());
        }
        // A rewrite add declines past the index budget is no change and has no entry;
        // an earlier epoch's entries are dropped, not served.
        let (db, native) = setup();
        let pasted: String = db.query_row("SELECT id FROM aliases WHERE original='provider:claude_transcript:You:native:pasted'", [], |r| r.get(0)).unwrap();
        let body: String = db.query_row("SELECT body FROM messages WHERE id=?", [&pasted], |r| r.get(0)).unwrap();
        set(&db, "bytes", "600000000").unwrap();
        db.execute("INSERT INTO projection_changes(epoch,id,change) VALUES('earlier-epoch','old','retired')", []).unwrap();
        let status = advance_projection_upgrade(&db, "scope", &native).unwrap();
        assert_eq!(status, json!({"state":"complete","generation":2,"refreshed":0,"retired":1,"retained":0,"declined":1,"unverified":0}));
        assert_eq!(db.query_row("SELECT body FROM messages WHERE id=?", [&pasted], |r| r.get::<_, String>(0)).unwrap(), body);
        let entries: Vec<(String, String)> = db.prepare("SELECT epoch,change FROM projection_changes").unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(entries, [("test-epoch".to_owned(), "retired".to_owned())]);
        // With room for exactly one entry, of two changes only the first is made and
        // recorded; the second waits at ledger_full with the cursor before it.
        for (seed, entries) in [
            ("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<99999)
                INSERT INTO projection_changes(epoch,id,change) SELECT 'test-epoch','earlier-'||i,'retired' FROM n;", 100_000),
            ("INSERT INTO projection_changes(generation,epoch,id,change) VALUES(9223372036854775806,'test-epoch','last','retired');", 2),
        ] {
            let (db, native) = setup();
            db.execute_batch(seed).unwrap();
            let before = rows(&db);
            let status = advance_projection_upgrade(&db, "scope", &native).unwrap();
            assert_eq!(status["state"], "ledger_full");
            assert_eq!(status["refreshed"].as_i64().unwrap() + status["retired"].as_i64().unwrap(), 1);
            assert_eq!(ledger(&db), entries);
            let (first, second) = (&before[0], &before[1]);
            assert_eq!(get(&db, "projection_upgrade_after").unwrap().as_ref(), Some(&first.0));
            let after = rows(&db);
            assert!(!after.contains(first));
            assert!(after.contains(second));
        }
        // An earlier epoch's entries go, but the spent sequence does not come back.
        let (db, native) = setup();
        db.execute_batch("INSERT INTO projection_changes(generation,epoch,id,change) VALUES(9223372036854775807,'earlier-epoch','last','retired');").unwrap();
        let before = rows(&db);
        let status = advance_projection_upgrade(&db, "scope", &native).unwrap();
        assert_eq!((status["state"].clone(), status["generation"].clone()), (json!("ledger_full"), json!(0)));
        assert_eq!((rows(&db), ledger(&db)), (before, 0));
        // A failed append undoes the change it records, for a retirement and a refresh.
        for keep in ["command", "pasted"] {
            let (db, native) = setup();
            let other = if keep == "command" { "pasted" } else { "command" };
            let id: String = db.query_row("SELECT id FROM aliases WHERE original=?",
                [format!("provider:claude_transcript:You:native:{other}")], |r| r.get(0)).unwrap();
            db.execute("DELETE FROM messages WHERE id=?", [&id]).unwrap();
            db.execute("DELETE FROM aliases WHERE id=?", [&id]).unwrap();
            db.execute("DELETE FROM reconcile_work", []).unwrap();
            db.execute_batch("CREATE TEMP TRIGGER refuse BEFORE INSERT ON projection_changes BEGIN SELECT RAISE(ABORT, 'ledger append failed'); END;").unwrap();
            let snapshot = |db: &Connection| -> Vec<String> {
                ["SELECT id||'|'||body||'|'||ifnull(incoming,'') FROM messages ORDER BY id", "SELECT original||'|'||id FROM aliases ORDER BY original",
                    "SELECT id FROM reconcile_work ORDER BY id", "SELECT key||'='||value FROM meta ORDER BY key",
                    "SELECT generation||'|'||epoch||'|'||id||'|'||change FROM projection_changes ORDER BY generation"]
                    .iter().flat_map(|sql| db.prepare(sql).unwrap().query_map([], |r| r.get::<_, String>(0)).unwrap().map(|r| r.unwrap()).collect::<Vec<_>>())
                    .collect()
            };
            let before = snapshot(&db);
            db.execute_batch("BEGIN IMMEDIATE").unwrap();
            let error = advance_projection_upgrade(&db, "scope", &native).unwrap_err().to_string();
            assert!(error.contains("ledger append failed"), "{keep}: {error}");
            db.execute_batch("ROLLBACK").unwrap();
            assert_eq!(snapshot(&db), before, "{keep}");
        }
    }
    #[test]
    fn source_resets_clear_the_projection_ledger() {
        let temp = tempfile::tempdir().unwrap();
        let conversation = uuid::Uuid::new_v4().to_string();
        let first = temp.path().join(format!("{conversation}.jsonl"));
        let line = json!({"type":"user","uuid":"one","sessionId":conversation,"timestamp":"2026-10-19T00:00:00Z",
            "message":{"role":"user","content":"Hello"}}).to_string() + "\n";
        std::fs::write(&first, line.repeat(2)).unwrap();
        let db = database();
        let native = json!({"agent":"claude","conversation_id":conversation,"transcript":first});
        let ledger = |db: &Connection| -> i64 { db.query_row("SELECT count(*) FROM projection_changes", [], |r| r.get(0)).unwrap() };
        let record = |db: &Connection| {
            db.execute("INSERT INTO projection_changes(epoch,id,change) VALUES('test-epoch','old','retired')", []).unwrap();
        };
        sources(&db, &native, &request(), &native).unwrap();
        record(&db);
        // A truncated source starts a new epoch without the old entries.
        std::fs::write(&first, &line).unwrap();
        assert_eq!(sources(&db, &native, &request(), &native).unwrap().1["provider"], "source_reset");
        assert_eq!(ledger(&db), 0);
        assert_ne!(get(&db, "epoch").unwrap().as_deref(), Some("test-epoch"));
        // So does another source file.
        sources(&db, &native, &request(), &native).unwrap();
        record(&db);
        let second = temp.path().join(format!("{conversation}-moved.jsonl"));
        std::fs::write(&second, &line).unwrap();
        let moved = json!({"agent":"claude","conversation_id":conversation,"transcript":second});
        sources(&db, &moved, &request(), &moved).unwrap();
        assert_eq!(ledger(&db), 0);
    }
    #[test]
    fn the_reported_generation_belongs_to_the_epoch_the_response_returns() {
        let ledger = |db: &Connection| -> i64 { db.query_row("SELECT count(*) FROM projection_changes", [], |r| r.get(0)).unwrap() };
        let seed = |db: &Connection, entries: i64| {
            for n in 0..entries {
                db.execute("INSERT INTO projection_changes(epoch,id,change) VALUES('test-epoch',?,'retired')", [format!("old-{n}")]).unwrap();
            }
        };
        // One history call after its sources, in dispatch's order: the projection
        // upgrade, reconciliation, then respond.
        let call = |db: &Connection| -> Value {
            db.execute_batch("BEGIN IMMEDIATE").unwrap();
            let mut status = json!({"provider":"complete"});
            status["projection_upgrade"] = advance_projection_upgrade(db, "scope", &json!({"agent":"codex"})).unwrap();
            let scan = advance_reconcile_upgrade(db).unwrap();
            reconcile(db).unwrap();
            finish_reconcile_upgrade(db, scan).unwrap();
            respond(db, &request(), true, status).unwrap()
        };
        let check = |db: &Connection, response: &Value, generation: i64| {
            assert_eq!(response["history_epoch"].as_str(), get(db, "epoch").unwrap().as_deref());
            assert_eq!(response["source_status"]["projection_upgrade"]["generation"], json!(generation));
        };
        // Reconciliation merges two differently numbered rows and replaces the epoch.
        let db = database();
        let prefix = "🙂".repeat(1200);
        add(&db, "scope", row("Stop", "hook", &format!("{prefix}…"), 10.), "journal").unwrap();
        add(&db, "scope", row("AgentMessage", "provider", &format!("{prefix} full reply"), 11.), "provider").unwrap();
        db.execute("DELETE FROM reconcile_work", []).unwrap();
        set(&db, "reconciled", "1").unwrap();
        assert_eq!(incoming(&db, true).unwrap(), Some(2));
        seed(&db, 3);
        let response = call(&db);
        assert_ne!(response["history_epoch"], "test-epoch");
        check(&db, &response, 0);
        assert_eq!(ledger(&db), 0);
        let next = call(&db);
        assert_eq!(next["history_epoch"], response["history_epoch"]);
        check(&db, &next, 0);
        // Incoming numbering meets an older backfilled reply and replaces the epoch.
        let db = database();
        add(&db, "scope", row("AgentMessage", "later", "Later", 20.), "provider").unwrap();
        assert_eq!(incoming(&db, true).unwrap(), Some(1));
        seed(&db, 2);
        add(&db, "scope", row("AgentMessage", "older", "Older", 10.), "provider").unwrap();
        let response = call(&db);
        assert_ne!(response["history_epoch"], "test-epoch");
        check(&db, &response, 0);
        assert_eq!(ledger(&db), 0);
        let next = call(&db);
        assert_eq!(next["history_epoch"], response["history_epoch"]);
        check(&db, &next, 0);
        // Without a new epoch the generation stays.
        let db = database();
        add(&db, "scope", row("AgentMessage", "reply", "Reply", 20.), "provider").unwrap();
        seed(&db, 2);
        let response = call(&db);
        assert_eq!(response["history_epoch"], "test-epoch");
        check(&db, &response, 2);
        assert_eq!(ledger(&db), 2);
        check(&db, &call(&db), 2);
    }
    #[cfg(unix)]
    #[test]
    fn an_existing_index_gains_the_ledger_and_a_legacy_schema_resets_it() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("public_history");
        let parent = json!({"agent":"claude","agent_home":"/synthetic/home","account_id":"account","conversation_id":"conversation"});
        let r = request();
        let ledger = |db: &Connection| -> i64 { db.query_row("SELECT count(*) FROM projection_changes", [], |r| r.get(0)).unwrap() };
        // A schema-2 index from before the ledger: no table and no projection marker.
        let (db, guard) = index_in(&directory, &parent, &r).unwrap();
        add(&db, "scope", row("AgentMessage", "kept", "Kept", 1.), "provider").unwrap();
        db.execute_batch("DROP TABLE projection_changes; DELETE FROM meta WHERE key='projection_version'; UPDATE messages SET incoming=3;").unwrap();
        let epoch = get(&db, "epoch").unwrap();
        drop(db);
        drop(guard);
        let (db, guard) = index_in(&directory, &parent, &r).unwrap();
        assert_eq!(get(&db, "epoch").unwrap(), epoch);
        assert_eq!(get(&db, "schema").unwrap().as_deref(), Some("2"));
        assert!(get(&db, "projection_version").unwrap().is_none());
        assert_eq!(ledger(&db), 0);
        assert_eq!(db.query_row("SELECT incoming FROM messages", [], |r| r.get::<_, i64>(0)).unwrap(), 3);
        // A legacy schema is reset whole, ledger included, under a new epoch.
        db.execute_batch("INSERT INTO projection_changes(epoch,id,change) VALUES('x','old','retired'); UPDATE meta SET value='1' WHERE key='schema';").unwrap();
        drop(db);
        drop(guard);
        let (db, _guard) = index_in(&directory, &parent, &r).unwrap();
        assert_eq!((ledger(&db), db.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0)).unwrap()), (0, 0));
        assert_ne!(get(&db, "epoch").unwrap(), epoch);
        assert_eq!(get(&db, "projection_version").unwrap().as_deref(), Some(PROJECTION_VERSION));
        assert_eq!(get(&db, "schema").unwrap().as_deref(), Some("2"));
    }
    fn projection(epoch: &str, after: i64, until: i64) -> ProjectionRequest {
        serde_json::from_value(json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":"run",
            "expected_conversation_id":"conversation","history_epoch":epoch,"projection_after":after,"projection_until":until})).unwrap()
    }
    fn serve(db: &Connection, after: i64, until: i64) -> Result<Value> {
        serve_projection(db, &projection("test-epoch", after, until), json!({}))
    }
    fn record_change(db: &Connection, id: &str, change: &str) {
        db.execute("INSERT INTO projection_changes(epoch,id,change) VALUES('test-epoch',?,?)", [id, change]).unwrap();
    }
    fn stored(db: &Connection, id: &str, body: Value, at: f64) {
        db.execute("INSERT INTO messages(id,at,role,stream,kind,ordinal,body) VALUES(?,?,'outgoing','provider','UserPromptSubmit',?,?)",
            params![id, at, format!("1:{id}"), body.to_string()]).unwrap();
    }
    #[test]
    fn projection_requests_are_strict_and_refusals_change_nothing() {
        let base = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":"run","expected_conversation_id":"conversation",
            "history_epoch":"test-epoch","projection_after":0,"projection_until":1});
        assert!(serde_json::from_value::<ProjectionRequest>(base.clone()).is_ok());
        for (field, value) in [("projection_after", json!(1.5)), ("projection_after", json!("1")), ("projection_until", json!(true)),
            ("projection_until", json!(9223372036854775808u64)), ("projection_after", Value::Null), ("extra", json!(1))] {
            let mut raw = base.clone();
            raw[field] = value;
            assert!(serde_json::from_value::<ProjectionRequest>(raw).is_err(), "{field}");
        }
        let mut raw = base.clone();
        raw.as_object_mut().unwrap().remove("projection_until");
        assert!(serde_json::from_value::<ProjectionRequest>(raw).is_err());
        assert!(validate_projection(&projection("test-epoch", 0, 1)).is_ok());
        for bad in [projection("test-epoch", -1, 1), projection("test-epoch", 3, 2), projection("", 0, 0),
            projection(&"e".repeat(129), 0, 0), projection("a\u{7}b", 0, 0)] {
            assert!(validate_projection(&bad).is_err());
        }
        let mut wrong = projection("test-epoch", 0, 0);
        wrong.request_id = "not-a-uuid".into();
        assert!(validate_projection(&wrong).is_err());
        // Refusals roll back: no ledger, meta or row change, and no response.
        let db = database();
        db.execute("INSERT INTO projection_changes(epoch,id,change) VALUES('earlier-epoch','old','retired')", []).unwrap();
        record_change(&db, "gone", "retired");
        let snapshot = |db: &Connection| -> Vec<String> {
            ["SELECT generation||'|'||epoch||'|'||id||'|'||change FROM projection_changes ORDER BY generation",
                "SELECT key||'='||value FROM meta ORDER BY key", "SELECT id FROM reconcile_work ORDER BY id"]
                .iter().flat_map(|sql| db.prepare(sql).unwrap().query_map([], |r| r.get::<_, String>(0)).unwrap().map(|r| r.unwrap()).collect::<Vec<_>>())
                .collect()
        };
        let before = snapshot(&db);
        assert_eq!(serve(&db, 0, 2).unwrap_err(), "History is still indexing; try again");
        assert_eq!(snapshot(&db), before);
        set(&db, "reconciled", "1").unwrap();
        db.execute("INSERT INTO reconcile_work(id) VALUES('pending')", []).unwrap();
        let pending = snapshot(&db);
        assert_eq!(serve(&db, 0, 2).unwrap_err(), "History is still indexing; try again");
        assert_eq!(snapshot(&db), pending);
        db.execute("DELETE FROM reconcile_work", []).unwrap();
        let settled = snapshot(&db);
        assert_eq!(serve_projection(&db, &projection("other-epoch", 0, 2), json!({})).unwrap_err(), "History epoch changed; reopen its history");
        assert_eq!(serve(&db, 0, 3).unwrap_err(), "Projection position is past the newest change");
        assert_eq!(snapshot(&db), settled);
        // Settled and exact: served in one transaction, the earlier epoch's entry gone.
        let response = serve(&db, 0, 2).unwrap();
        assert_eq!(response["history_epoch"], "test-epoch");
        assert_eq!(response["projection_generation"], 2);
        assert_eq!(response["projection_changes"], json!([{"generation":2,"history_id":"gone","change":"retired","resolution":"resolved","successor":null}]));
        assert_eq!((response["next_projection_after"].clone(), response["complete"].clone()), (json!(2), json!(true)));
        assert_eq!(db.query_row("SELECT count(*) FROM projection_changes", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }
    #[test]
    fn projection_entries_resolve_through_aliases_and_return_page_rows() {
        let db = database();
        set(&db, "reconciled", "1").unwrap();
        let id = |message: &str| -> String {
            db.query_row("SELECT id FROM messages WHERE json_extract(body,'$.message_id')=?", [message], |r| r.get(0)).unwrap()
        };
        let body = |id: &str| -> String { db.query_row("SELECT body FROM messages WHERE id=?", [id], |r| r.get(0)).unwrap() };
        for (message, at) in [("present", 1.), ("a", 2.), ("b", 3.), ("c", 4.), ("p", 6.)] {
            add(&db, "scope", row("UserPromptSubmit", message, &format!("Prompt {message}"), at), "provider").unwrap();
        }
        add(&db, "scope", json!({"type":"UserPromptSubmit","seq":9,"at":6.0,"agent_id":"","detail":"Prompt p"}), "journal").unwrap();
        let (present, a, b, c, p) = (id("present"), id("a"), id("b"), id("c"), id("p"));
        let j: String = db.query_row("SELECT id FROM aliases WHERE original='journal:journal:You:native:9'", [], |r| r.get(0)).unwrap();
        // Two merges: a into b, then b into c, so history:a ends at c.
        merge_row(&db, &b, &a, &body(&b), "UserPromptSubmit").unwrap();
        merge_row(&db, &c, &b, &body(&c), "UserPromptSubmit").unwrap();
        // At equal time the hook row keeps the provider row and takes a receipt's body.
        let receipt = attachment_row("receipt", "Prompt p", 6.0).to_string();
        merge_row(&db, &j, &p, &receipt, "UserPromptSubmit").unwrap();
        db.execute("DELETE FROM reconcile_work", []).unwrap();
        for (changed, change) in [("gone", "retired"), (present.as_str(), "refreshed"), (a.as_str(), "refreshed"), (p.as_str(), "refreshed"),
            ("missing", "refreshed"), (present.as_str(), "retired")] {
            record_change(&db, changed, change);
        }
        let response = serve(&db, 0, 6).unwrap();
        let entries = response["projection_changes"].as_array().unwrap();
        assert_eq!(entries.len(), 6);
        assert_eq!(entries[0], json!({"generation":1,"history_id":"gone","change":"retired","resolution":"resolved","successor":null}));
        for (entry, owner) in [(&entries[1], &present), (&entries[2], &c), (&entries[3], &j)] {
            assert_eq!((entry["resolution"].clone(), entry["successor"]["history_id"].clone()), (json!("resolved"), json!(owner)));
            assert!(entry["successor"].get("native_match_hash").is_none());
            assert!(entry.get("reason").is_none());
        }
        assert_eq!(entries[3]["successor"]["history_stream"], "attachment");
        assert_eq!(entries[3]["successor"]["attachments"], attachment_row("receipt", "Prompt p", 6.0)["attachments"]);
        assert_eq!(entries[4], json!({"generation":5,"history_id":"missing","change":"refreshed","resolution":"unresolved","successor":null,"reason":"no_successor"}));
        assert_eq!(entries[5], json!({"generation":6,"history_id":present,"change":"retired","resolution":"unresolved","successor":null,"reason":"unexpected_successor"}));
        assert_eq!((response["next_projection_after"].clone(), response["complete"].clone()), (json!(6), json!(true)));
        // A successor is the page's own row, byte for byte, including page's limit of 8 aliases.
        for n in 0..12 {
            db.execute("INSERT INTO aliases(original,id) VALUES(?,?)", params![format!("extra:{n:02}"), c]).unwrap();
        }
        let successor = serve(&db, 2, 3).unwrap()["projection_changes"][0]["successor"].clone();
        let page_row = page(&db, &request(), true, Some(0), json!({})).unwrap()["events"].as_array().unwrap().iter()
            .find(|row| row["history_id"] == json!(c)).unwrap().clone();
        assert_eq!(successor["original_ids"].as_array().unwrap().len(), 8);
        assert_eq!(serde_json::to_string(&successor).unwrap(), serde_json::to_string(&page_row).unwrap());
    }
    #[test]
    fn ordinary_reconciliation_changes_the_served_successor() {
        let db = database();
        add(&db, "scope", row("UserPromptSubmit", "p", "Same prompt", 10.), "provider").unwrap();
        reconcile(&db).unwrap();
        let p: String = db.query_row("SELECT id FROM messages", [], |r| r.get(0)).unwrap();
        record_change(&db, &p, "refreshed");
        assert_eq!(serve(&db, 0, 1).unwrap()["projection_changes"][0]["successor"]["history_id"], json!(p));
        // A hook row for the same prompt arrives; until it is reconciled nothing is served.
        add(&db, "scope", json!({"type":"UserPromptSubmit","seq":12,"at":10.5,"agent_id":"","detail":"Same prompt"}), "journal").unwrap();
        assert_eq!(serve(&db, 0, 1).unwrap_err(), "History is still indexing; try again");
        reconcile(&db).unwrap();
        let j: String = db.query_row("SELECT id FROM aliases WHERE original='journal:journal:You:native:12'", [], |r| r.get(0)).unwrap();
        assert_ne!(j, p);
        let entry = serve(&db, 0, 1).unwrap()["projection_changes"][0].clone();
        assert_eq!((entry["resolution"].clone(), entry["successor"]["history_id"].clone()), (json!("resolved"), json!(j)));
        assert_eq!(entry["successor"]["detail"], "Same prompt");
    }
    #[test]
    fn projection_responses_are_bounded_and_never_drop_an_entry() {
        let db = database();
        set(&db, "reconciled", "1").unwrap();
        for n in 0..300 {
            record_change(&db, &format!("gone-{n}"), "retired");
        }
        let first = serve(&db, 0, 300).unwrap();
        assert_eq!(first["projection_changes"].as_array().unwrap().len(), 256);
        assert_eq!((first["next_projection_after"].clone(), first["complete"].clone()), (json!(256), json!(false)));
        let second = serve(&db, 256, 300).unwrap();
        assert_eq!(second["projection_changes"].as_array().unwrap().len(), 44);
        assert_eq!((second["next_projection_after"].clone(), second["complete"].clone()), (json!(300), json!(true)));
        let tail = serve(&db, 300, 300).unwrap();
        assert_eq!((tail["projection_changes"].clone(), tail["next_projection_after"].clone(), tail["complete"].clone()), (json!([]), json!(300), json!(true)));
        // The whole encoded response is charged: a detail past the cap is clipped as a
        // page clips it, metadata past it is an explicit oversize statement, and an entry
        // that fits only alone waits for the next response.
        let db = database();
        set(&db, "reconciled", "1").unwrap();
        let prompt = |id: &str, detail: String, attachments: Value| {
            json!({"type":"UserPromptSubmit","source":"claude_transcript","message_id":id,"detail":detail,"attachments":attachments})
        };
        stored(&db, "long", prompt("long", "x".repeat(900 * 1024), json!([])), 1.);
        stored(&db, "wide", prompt("wide", "short".into(), json!([{"name":"y".repeat(900 * 1024),"mime":"text/plain","bytes":1}])), 2.);
        stored(&db, "half-1", prompt("half-1", "z".repeat(500 * 1024), json!([])), 3.);
        stored(&db, "half-2", prompt("half-2", "z".repeat(500 * 1024), json!([])), 4.);
        for id in ["long", "wide", "half-1", "half-2"] {
            record_change(&db, id, "refreshed");
        }
        let responses = [serve(&db, 0, 4).unwrap(), serve(&db, 1, 4).unwrap(), serve(&db, 3, 4).unwrap()];
        for response in &responses {
            assert!(ascii_wire_size(response) <= PROJECTION_RESPONSE_BYTES);
        }
        let first = responses[0]["projection_changes"].as_array().unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!((first[0]["resolution"].clone(), first[0]["successor"]["detail_truncated"].clone()), (json!("resolved"), json!(true)));
        assert_eq!(first[0]["successor"]["detail"].as_str().unwrap().chars().count(), 501);
        assert_eq!((responses[0]["next_projection_after"].clone(), responses[0]["complete"].clone()), (json!(1), json!(false)));
        let second = responses[1]["projection_changes"].as_array().unwrap();
        assert_eq!(second.len(), 2);
        assert_eq!(second[0], json!({"generation":2,"history_id":"wide","change":"refreshed","resolution":"unresolved","successor":null,"reason":"oversize"}));
        assert_eq!(second[1]["successor"]["history_id"], "half-1");
        assert_eq!((responses[1]["next_projection_after"].clone(), responses[1]["complete"].clone()), (json!(3), json!(false)));
        let third = responses[2]["projection_changes"].as_array().unwrap();
        assert_eq!((third.len(), third[0]["successor"]["history_id"].clone()), (1, json!("half-2")));
        assert_eq!((responses[2]["next_projection_after"].clone(), responses[2]["complete"].clone()), (json!(4), json!(true)));
    }
    #[cfg(unix)]
    #[test]
    fn a_projection_read_opens_only_an_existing_ready_index_and_changes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("public_history");
        let parent = json!({"agent":"claude","agent_home":"/synthetic/home","account_id":"account","conversation_id":"conversation"});
        let r = request();
        let key = key(&parent, &r);
        let (db_path, lock_path) = (directory.join(format!("{key}.sqlite3")), directory.join(format!("{key}.lock")));
        // Every file under the temporary root with its exact bytes.
        let files = |root: &Path| -> Vec<(String, String)> {
            let mut files = Vec::new();
            let mut pending = vec![root.to_path_buf()];
            while let Some(path) = pending.pop() {
                for entry in std::fs::read_dir(&path).unwrap() {
                    let entry = entry.unwrap().path();
                    if entry.is_dir() {
                        files.push((entry.display().to_string(), "directory".into()));
                        pending.push(entry);
                    } else {
                        files.push((entry.display().to_string(), format!("{:x}", Sha256::digest(std::fs::read(&entry).unwrap()))));
                    }
                }
            }
            files.sort();
            files
        };
        let refused = |reason: &str| {
            let before = files(temp.path());
            assert_eq!(open_projection_index(&directory, &parent, &r).err().as_deref(), Some(reason));
            assert_eq!(files(temp.path()), before);
        };
        // Missing directory, then an empty private directory: nothing is created.
        refused(INDEX_NOT_READY);
        assert!(!directory.exists());
        private_dir(&directory).unwrap();
        refused(INDEX_NOT_READY);
        // An uninitialized index: an empty private database beside its lock file.
        drop(lock_file(&lock_path).unwrap());
        drop(crate::platform::private_open(OpenOptions::new().create(true).write(true)).open(&db_path).unwrap());
        refused(INDEX_NOT_READY);
        assert_eq!(std::fs::metadata(&db_path).unwrap().len(), 0);
        std::fs::remove_file(&db_path).unwrap();
        std::fs::remove_file(&lock_path).unwrap();
        // A real index of another schema, then one from before the ledger, is not migrated.
        let (db, guard) = index_in(&directory, &parent, &r).unwrap();
        let epoch = get(&db, "epoch").unwrap().unwrap();
        set(&db, "schema", "1").unwrap();
        drop(db);
        drop(guard);
        refused(INDEX_NOT_READY);
        let other = Connection::open(&db_path).unwrap();
        set(&other, "schema", "2").unwrap();
        other.execute_batch("DROP TABLE projection_changes;").unwrap();
        drop(other);
        refused(INDEX_NOT_READY);
        let other = Connection::open(&db_path).unwrap();
        other.execute_batch("CREATE TABLE projection_changes(generation INTEGER PRIMARY KEY AUTOINCREMENT,epoch TEXT NOT NULL,id TEXT NOT NULL,change TEXT NOT NULL CHECK(change IN ('retired','refreshed')));").unwrap();
        set(&other, "reconciled", "1").unwrap();
        other.execute("INSERT INTO projection_changes(epoch,id,change) VALUES(?,'gone','retired')", [&epoch]).unwrap();
        drop(other);
        // A ready schema-2 index: a wrong epoch is refused with every byte unchanged.
        let before = files(temp.path());
        let (db, guard) = open_projection_index(&directory, &parent, &r).unwrap();
        assert_eq!(serve_projection(&db, &projection("other-epoch", 0, 1), json!({})).unwrap_err(), "History epoch changed; reopen its history");
        drop(db);
        drop(guard);
        assert_eq!(files(temp.path()), before);
        let (db, _guard) = open_projection_index(&directory, &parent, &r).unwrap();
        let response = serve_projection(&db, &projection(&epoch, 0, 1), json!({})).unwrap();
        assert_eq!(response["projection_changes"], json!([{"generation":1,"history_id":"gone","change":"retired","resolution":"resolved","successor":null}]));
    }
    /// Every entry under `root` by its kind, never opening a FIFO: a file by its
    /// bytes, a link by its target.
    #[cfg(unix)]
    fn entries(root: &Path) -> Vec<(String, String)> {
        use std::os::unix::fs::FileTypeExt;
        let mut entries = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            for entry in std::fs::read_dir(&path).unwrap() {
                let entry = entry.unwrap().path();
                let kind = std::fs::symlink_metadata(&entry).unwrap().file_type();
                let state = if kind.is_symlink() {
                    format!("link {}", std::fs::read_link(&entry).unwrap().display())
                } else if kind.is_dir() {
                    pending.push(entry.clone());
                    "directory".into()
                } else if kind.is_fifo() {
                    "fifo".into()
                } else if kind.is_file() {
                    format!("{:x}", Sha256::digest(std::fs::read(&entry).unwrap()))
                } else {
                    "other".into()
                };
                entries.push((entry.display().to_string(), state));
            }
        }
        entries.sort();
        entries
    }
    #[cfg(unix)]
    #[test]
    fn the_projection_lock_and_metadata_must_be_ready_before_anything_is_locked() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("public_history");
        let parent = json!({"agent":"claude","agent_home":"/synthetic/home","account_id":"account","conversation_id":"conversation"});
        let r = request();
        let key = key(&parent, &r);
        let (db_path, lock_path, aside) = (directory.join(format!("{key}.sqlite3")), directory.join(format!("{key}.lock")), temp.path().join("aside"));
        let (db, guard) = index_in(&directory, &parent, &r).unwrap();
        add(&db, "scope", row("UserPromptSubmit", "kept", "Kept", 1.), "provider").unwrap();
        set(&db, "reconciled", "1").unwrap();
        let epoch = get(&db, "epoch").unwrap().unwrap();
        db.execute("INSERT INTO projection_changes(epoch,id,change) VALUES(?,'gone','retired')", [&epoch]).unwrap();
        db.execute("DELETE FROM reconcile_work", []).unwrap();
        drop(db);
        drop(guard);
        let refused = |reason: &str| {
            let before = entries(temp.path());
            assert_eq!(open_projection_index(&directory, &parent, &r).err().as_deref(), Some(reason));
            assert_eq!(entries(temp.path()), before);
        };
        let private = "Public history index lock must be private";
        // A lock that is a link, a directory, a FIFO or not private is refused before locking.
        std::fs::rename(&lock_path, &aside).unwrap();
        std::os::unix::fs::symlink(&aside, &lock_path).unwrap();
        refused(INDEX_NOT_READY);
        std::fs::remove_file(&lock_path).unwrap();
        std::fs::create_dir(&lock_path).unwrap();
        refused(INDEX_NOT_READY);
        std::fs::remove_dir(&lock_path).unwrap();
        let name = std::ffi::CString::new(lock_path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        // A held non-blocking reader lets the write open return at once, so the
        // regular-file check, not a wait, decides; the control stays bounded.
        let reader = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&lock_path).unwrap();
        refused(private);
        drop(reader);
        std::fs::remove_file(&lock_path).unwrap();
        std::fs::rename(&aside, &lock_path).unwrap();
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        refused(private);
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&lock_path, &aside).unwrap();
        refused(private);
        std::fs::remove_file(&aside).unwrap();
        // The lock without its database, and a database without its epoch or cursor key.
        std::fs::rename(&db_path, &aside).unwrap();
        refused(INDEX_NOT_READY);
        std::fs::rename(&aside, &db_path).unwrap();
        for key in ["epoch", "secret"] {
            let other = Connection::open(&db_path).unwrap();
            let value = get(&other, key).unwrap().unwrap();
            other.execute("DELETE FROM meta WHERE key=?", [key]).unwrap();
            drop(other);
            refused(INDEX_NOT_READY);
            let other = Connection::open(&db_path).unwrap();
            set(&other, key, &value).unwrap();
        }
        // Ready again, with its row, alias and ledger entry intact.
        let (db, _guard) = open_projection_index(&directory, &parent, &r).unwrap();
        let count = |sql: &str| -> i64 { db.query_row(sql, [], |row| row.get(0)).unwrap() };
        assert_eq!((count("SELECT count(*) FROM messages"), count("SELECT count(*) FROM aliases"), count("SELECT count(*) FROM projection_changes")), (1, 1, 1));
        let response = serve_projection(&db, &projection(&epoch, 0, 1), json!({})).unwrap();
        assert_eq!(response["projection_changes"][0]["history_id"], "gone");
    }
    #[test]
    fn delayed_attachment_retries_after_provider_and_journal_acknowledgements_reconcile() {
        let db = database();
        let submitted = "Compare images\n\nInspect the attached image at \"/synthetic/request-one/image.png\"";
        add(&db,"scope",row("UserMessage","native",submitted,202.),"provider").unwrap();
        add(&db,"scope",row("UserPromptSubmit","hook",submitted,203.),"journal").unwrap();
        add(&db,"scope",attachment_row("receipt",submitted,100.),"attachment").unwrap();
        for _ in 0..4 { reconcile(&db).unwrap(); }
        let result = page(&db,&request(),true,Some(0),json!({})).unwrap();
        assert_eq!(result["events"].as_array().unwrap().len(),1);
        assert_eq!(result["events"][0]["detail"],"Compare images");
        assert!(!result.to_string().contains("/synthetic/"));
        assert_eq!(result["events"][0]["original_ids"].as_array().unwrap().iter()
            .filter(|value| value.as_str().is_some_and(|id| id.starts_with("provider:") || id.starts_with("journal:") || id.starts_with("attachment:"))).count(),3);
    }

    #[test]
    fn delayed_attachment_hash_uses_full_native_text_before_public_row_clipping() {
        let db = database();
        let submitted = "Ж".repeat(ROW_BYTES) + "\n\nInspect the attached image at \"/synthetic/request-one/image.png\"";
        add(&db,"scope",row("UserMessage","native",&submitted,202.),"provider").unwrap();
        let body: String = db.query_row("SELECT body FROM messages",[],|r|r.get(0)).unwrap();
        let native: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(native["detail_truncated"],true);
        assert_eq!(native["native_match_hash"],hash(&submitted));
        // An upgrade in progress must not replace an already-complete hash
        // with the digest of the clipped public display text.
        advance_reconcile_upgrade(&db).unwrap();
        add(&db,"scope",attachment_row("receipt",&submitted,100.),"attachment").unwrap();
        reconcile(&db).unwrap();
        let result = page(&db,&request(),true,Some(0),json!({})).unwrap();
        assert_eq!(result["events"].as_array().unwrap().len(),1);
        assert_eq!(result["events"][0]["detail"],"Compare images");
        assert!(!result.to_string().contains("/synthetic/"));
    }

    #[test]
    fn delayed_attachment_matching_preserves_ambiguous_and_independent_sends() {
        let submitted = "Compare images\n\nInspect the attached image at \"/synthetic/request-one/image.png\"";
        for duplicate in ["native", "receipt", "independent"] {
            let db = database();
            add(&db,"scope",row("UserMessage","native",submitted,202.),"provider").unwrap();
            add(&db,"scope",attachment_row("receipt",submitted,100.),"attachment").unwrap();
            match duplicate {
                "native" => add(&db,"scope",row("UserMessage","other",submitted,203.),"provider").unwrap(),
                "receipt" => add(&db,"scope",attachment_row("other",submitted,99.),"attachment").unwrap(),
                _ => add(&db,"scope",row("UserMessage","other",&submitted.replace("request-one","request-two"),203.),"provider").unwrap(),
            }
            reconcile(&db).unwrap();
            let count: i64 = db.query_row("SELECT count(*) FROM messages",[],|r|r.get(0)).unwrap();
            assert_eq!(count,if duplicate=="independent" {2} else {3},"{duplicate}");
        }
    }
    #[test]
    fn cached_delayed_attachment_pairs_upgrade_without_resetting_read_epoch() {
        let submitted = "Compare images\n\nInspect the attached image at \"/synthetic/request-one/image.png\"";
        let db = database();
        add(&db,"scope",row("UserMessage","native",submitted,202.),"provider").unwrap();
        add(&db,"scope",attachment_row("receipt",submitted,100.),"attachment").unwrap();
        // Previous schema had no exact hash on native outgoing rows.
        db.execute("UPDATE messages SET body=json_remove(body,'$.native_match_hash') WHERE stream='provider'",[]).unwrap();
        db.execute("DELETE FROM reconcile_work",[]).unwrap();
        set(&db,"reconcile_version","2").unwrap();
        let epoch = get(&db,"epoch").unwrap();
        let scan = advance_reconcile_upgrade(&db).unwrap();
        reconcile(&db).unwrap();
        assert!(finish_reconcile_upgrade(&db,scan).unwrap());
        assert_eq!(get(&db,"epoch").unwrap(),epoch);
        assert_eq!(db.query_row("SELECT count(*) FROM messages",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    }

    #[test]
    fn hook_clipping_recognizes_unicode_scalar_limit_only() {
        for prefix in ["x".repeat(1200), "🧭Ж🙂α".repeat(300), "Ж".repeat(journal::REPLY_LIMIT)] {
            let full = format!("{prefix} continued public reply");
            let hook = format!("{prefix}…");
            assert!(stop_excerpt_matches(&full, &hook));
            assert!(!stop_excerpt_matches(
                &format!("different{}", prefix.chars().skip(1).collect::<String>()),
                &hook
            ));
        }
        assert!(!stop_excerpt_matches(
            "Short ending continues",
            "Short ending…"
        ));
        assert!(stop_excerpt_matches(
            "Short ending… literally",
            "Short ending…"
        ));
        assert!(stop_excerpt_matches("Literal ... retained", "..."));
        for blank in ["", " ", "\n", "…", "……"] {
            assert!(!stop_excerpt_matches("public text … ...", blank));
        }
    }
    #[test]
    fn clipped_hooks_merge_in_either_arrival_order_without_read_inflation() {
        for text in ["A".repeat(1200), "🧭Ж🙂α".repeat(300)] {
            for provider_first in [false, true] {
                let db = database();
                let full = format!("{text} final continuation");
                let hook = format!("{text}…");
                if provider_first {
                    add(
                        &db,
                        "scope",
                        row("AgentMessage", "provider", &full, 10.),
                        "provider",
                    )
                    .unwrap();
                    reconcile(&db).unwrap();
                }
                add(&db, "scope", row("Stop", "hook", &hook, 11.), "journal").unwrap();
                if !provider_first {
                    add(
                        &db,
                        "scope",
                        row("AgentMessage", "provider", &full, 10.),
                        "provider",
                    )
                    .unwrap();
                }
                reconcile(&db).unwrap();
                assert_eq!(incoming(&db, true).unwrap(), Some(1));
                let page = page(&db, &request(), true, Some(1), json!({})).unwrap();
                assert_eq!(page["events"].as_array().unwrap().len(), 1);
                assert_eq!(page["events"][0]["detail"], full);
            }
        }
    }
    #[test]
    fn clipped_pairs_respect_radius_and_mutual_uniqueness() {
        let prefix = "Ж".repeat(1200);
        let hook = format!("{prefix}…");
        let full = format!("{prefix} continuation");
        for case in ["wrong_text", "outside_radius", "two_hooks", "two_providers"] {
            let db = database();
            add(&db, "scope", row("Stop", "hook", &hook, 10.), "journal").unwrap();
            add(
                &db,
                "scope",
                row(
                    "AgentMessage",
                    "provider",
                    if case == "wrong_text" {
                        "unrelated"
                    } else {
                        &full
                    },
                    if case == "outside_radius" { 14. } else { 10. },
                ),
                "provider",
            )
            .unwrap();
            if case == "two_hooks" {
                add(
                    &db,
                    "scope",
                    row("Stop", "other_hook", &hook, 10.),
                    "journal",
                )
                .unwrap();
            }
            if case == "two_providers" {
                add(
                    &db,
                    "scope",
                    row("AgentMessage", "other_provider", &full, 10.),
                    "provider",
                )
                .unwrap();
            }
            reconcile(&db).unwrap();
            let expected = if case.starts_with("two_") { 3 } else { 2 };
            assert_eq!(
                db.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                expected,
                "{case}"
            );
        }
    }
    #[test]
    fn cached_numbered_duplicates_upgrade_with_aliases_and_explicit_epoch_reset() {
        let db = database();
        let prefix = "🙂".repeat(1200);
        let full = format!("{prefix} full reply");
        add(
            &db,
            "scope",
            row("Stop", "hook", &format!("{prefix}…"), 10.),
            "journal",
        )
        .unwrap();
        add(
            &db,
            "scope",
            row("AgentMessage", "provider", &full, 11.),
            "provider",
        )
        .unwrap();
        // Model an index finalized by the old literal-ellipsis matcher.
        db.execute("DELETE FROM reconcile_work", []).unwrap();
        set(&db, "reconciled", "1").unwrap();
        assert_eq!(incoming(&db, true).unwrap(), Some(2));
        let before = page(&db, &request(), true, Some(2), json!({})).unwrap();
        let old_cursor = before["events"][0]["history_cursor"]
            .as_str()
            .unwrap()
            .to_owned();
        let old_epoch = get(&db, "epoch").unwrap().unwrap();
        let scan = advance_reconcile_upgrade(&db).unwrap();
        reconcile(&db).unwrap();
        assert!(finish_reconcile_upgrade(&db, scan).unwrap());
        assert_eq!(incoming(&db, true).unwrap(), Some(1));
        let epoch = get(&db, "epoch").unwrap().unwrap();
        assert_ne!(epoch, old_epoch);
        assert!(decode(&old_cursor, &get(&db, "secret").unwrap().unwrap(), &epoch).is_err());
        let after = page(&db, &request(), true, Some(1), json!({})).unwrap();
        let row = &after["events"][0];
        assert_eq!(after["events"].as_array().unwrap().len(), 1);
        assert_eq!(row["detail"], full);
        assert!(row["original_ids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().starts_with("history:")));
        assert_eq!(
            get(&db, "reconcile_version").unwrap().as_deref(),
            Some(RECONCILE_VERSION)
        );
    }
    #[test]
    fn upgrade_scan_is_bounded_resumable_and_does_not_reset_clean_epoch() {
        let db = database();
        for n in 0..300 {
            add(
                &db,
                "scope",
                row(
                    "AgentMessage",
                    &format!("provider-{n}"),
                    "Unique",
                    n as f64 * 10.,
                ),
                "provider",
            )
            .unwrap();
        }
        db.execute("DELETE FROM reconcile_work", []).unwrap();
        assert_eq!(incoming(&db, true).unwrap(), Some(300));
        let old_epoch = get(&db, "epoch").unwrap();
        let scan = advance_reconcile_upgrade(&db).unwrap();
        assert!(!scan);
        reconcile(&db).unwrap();
        assert!(!finish_reconcile_upgrade(&db, scan).unwrap());
        let cursor = get(&db, "reconcile_upgrade_after").unwrap().unwrap();
        assert!(get(&db, "reconcile_version").unwrap().is_none());
        for _ in 0..8 {
            let scan = advance_reconcile_upgrade(&db).unwrap();
            reconcile(&db).unwrap();
            if finish_reconcile_upgrade(&db, scan).unwrap() {
                break;
            }
        }
        assert_eq!(
            get(&db, "reconcile_version").unwrap().as_deref(),
            Some(RECONCILE_VERSION)
        );
        assert_eq!(get(&db, "epoch").unwrap(), old_epoch);
        assert!(!cursor.is_empty());
        assert!(get(&db, "reconcile_upgrade_after").unwrap().is_none());
        assert_eq!(incoming(&db, true).unwrap(), Some(300));
    }
    #[test]
    fn partial_and_oversized_sources_make_bounded_progress() {
        let temp = tempfile::tempdir().unwrap();
        let conversation = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("rollout-{conversation}.jsonl"));
        let header =
            json!({"type":"session_meta","payload":{"id":conversation}}).to_string() + "\n";
        let public=json!({"type":"event_msg","timestamp":"2026-10-09T00:00:00Z","payload":{"type":"agent_message","message":"Final without newline"}}).to_string();
        std::fs::write(&path, header.clone() + &public).unwrap();
        let native = json!({"agent":"codex","conversation_id":conversation,"transcript":path});
        let db = database();
        let r = request();
        let (_, status) = sources(&db, &native, &r, &native).unwrap();
        assert_eq!(status["provider"], "partial_final_line");
        assert_eq!(
            db.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        std::fs::write(
            &path,
            header.clone() + &"x".repeat(BUDGET * 2 + 123) + "\n" + &public + "\n",
        )
        .unwrap();
        for _ in 0..8 {
            let _ = sources(&db, &native, &r, &native).unwrap();
        }
        assert_eq!(
            get(&db, "offset:0")
                .unwrap()
                .unwrap()
                .parse::<u64>()
                .unwrap(),
            std::fs::metadata(&path).unwrap().len()
        );
    }
}
