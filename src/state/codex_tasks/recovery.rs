//! A real new main prompt may repair the old same-process task-binding bug.
//! Neither a read, a completion nor a replayed/fabricated hook repairs input.
use super::*;
use sha2::Digest;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::time::{Duration, Instant};

fn header(file: &mut File) -> Option<Value> {
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    BufReader::new(file.take((64 << 10) + 1))
        .read_until(b'\n', &mut bytes)
        .ok()?;
    if bytes.len() > 64 << 10 || !bytes.ends_with(b"\n") {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

pub(super) fn first_start(record: &Value, parent: &str) -> bool {
    let query = || -> Option<bool> {
        #[cfg(unix)]
        let identity = crate::sqlite_file::existing(&root().join("events.sqlite3")).ok()?;
        let db = rusqlite::Connection::open_with_flags(
            root().join("events.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .ok()?;
        #[cfg(unix)]
        crate::sqlite_file::bind(&db, identity).ok()?;
        db.busy_timeout(Duration::ZERO).ok()?;
        let until = Instant::now() + Duration::from_millis(50);
        let mut steps = 0;
        db.progress_handler(
            100,
            Some(move || {
                steps += 100;
                steps > 10_000 || Instant::now() >= until
            }),
        );
        let name = if string(record, "journal_name").is_empty() {
            string(record, "name")
        } else {
            string(record, "journal_name")
        };
        let mut statement = db.prepare("SELECT json_extract(payload,'$.type'),json_extract(payload,'$.agent_id'),json_extract(payload,'$.run_id') FROM events INDEXED BY events_session WHERE name=?1 AND conversation=?2 ORDER BY seq LIMIT 64").ok()?;
        let rows = statement
            .query_map(rusqlite::params![name, parent], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .ok()?;
        for row in rows {
            let (kind, agent, run) = row.ok()?;
            if kind.as_deref() == Some("SessionStart")
                && agent.as_deref().unwrap_or("").is_empty()
                && run.as_deref() == Some(string(record, "run_id"))
            {
                return Some(true);
            }
        }
        Some(false)
    };
    query() == Some(true)
}

fn creation(file: &mut File, child: &str, created: f64) -> Option<(u64, Vec<u8>)> {
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(16 << 20);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut reader = BufReader::new(file.take((16 << 20) + 1));
    let mut offset = start;
    let mut bytes = Vec::new();
    for index in 0..16_384 {
        bytes.clear();
        let size = reader.read_until(b'\n', &mut bytes).ok()?;
        if size == 0 {
            return None;
        }
        let at = offset;
        offset += size as u64;
        if !bytes.ends_with(b"\n") {
            return None;
        }
        if (start > 0 && index == 0) || bytes.len() > 16 << 10 {
            continue;
        }
        let row: Value = serde_json::from_slice(&bytes).ok()?;
        let item = &row["payload"]["item"];
        if row["type"] != "event_msg"
            || row["payload"]["type"] != "item_completed"
            || item["type"] != "McpToolCall"
            || item["server"] != "codex_tui"
            || item["tool"] != "create_thread"
            || item["status"] != "completed"
        {
            continue;
        }
        let stamp = chrono::DateTime::parse_from_rfc3339(string(&row, "timestamp"))
            .ok()?
            .timestamp_millis() as f64
            / 1000.0;
        if stamp < created || stamp > now() + 1.0 {
            continue;
        }
        let event = json!({"tool_response":item["result"]});
        if super::created(&event).as_deref() == Some(child) {
            return Some((at, bytes));
        }
    }
    None
}

/// An ordinary native transition may already have restored the main binding.
/// This only admits a historical receipt candidate to exact-input checking;
/// it repairs no binding, mutates no receipt and grants no input authority.
pub(in crate::state) fn original_main_receipt(
    record: &Value,
    path: &Path,
    file: &File,
    meta: &Value,
    rows: &[Value],
    child: &str,
) -> bool {
    let prove = || -> Option<()> {
        let parent = string(record, "conversation_id");
        if record["agent"] != "codex"
            || child == parent
            || !uuid::Uuid::parse_str(parent)
                .is_ok_and(|id| !id.is_nil() && id.to_string() == parent)
            || !uuid::Uuid::parse_str(child).is_ok_and(|id| !id.is_nil() && id.to_string() == child)
            || !first_start(record, parent)
        {
            return None;
        }
        let account = crate::platform::canonical(string(record, "agent_home")).ok()?;
        let payload = &meta["payload"];
        let created = chrono::DateTime::parse_from_rfc3339(string(payload, "timestamp"))
            .ok()?
            .timestamp_millis() as f64
            / 1000.0;
        let launched = record["created"].as_f64()?;
        let pending = record["input_pending_at"].as_f64()?;
        if !path.is_absolute()
            || !path.starts_with(account.join("sessions"))
            || meta["type"] != "session_meta"
            || payload["id"] != parent
            || payload["source"] != "cli"
            || payload.get("forked_from_id").is_some_and(|v| !v.is_null())
            || payload
                .get("parent_thread_id")
                .is_some_and(|v| !v.is_null())
            || !(launched - 1.0..=launched + 5.0).contains(&created)
            || !pending.is_finite()
            || pending < created
            || pending > now()
        {
            return None;
        }
        let pid = u32::try_from(record["pid"].as_u64()?).ok()?;
        let process = crate::platform::process_identity(pid)?;
        let identity = crate::platform::FileIdentity::with_metadata(file).ok()?.0;
        if !matches!(crate::platform::holds_open(pid, path).ok()?, crate::platform::OpenFiles::Held)
            || !process_alive(record)
            || !rows.iter().any(|row| {
                let item = &row["payload"]["item"];
                if row["type"] != "event_msg"
                    || row["payload"]["type"] != "item_completed"
                    || item["type"] != "McpToolCall"
                    || item["server"] != "codex_tui"
                    || item["tool"] != "create_thread"
                    || item["status"] != "completed"
                {
                    return false;
                }
                let at = chrono::DateTime::parse_from_rfc3339(string(row, "timestamp"))
                    .ok()
                    .map(|at| at.timestamp_millis() as f64 / 1000.0);
                at.is_some_and(|at| (created..=pending).contains(&at))
                    && super::created(&json!({"tool_response":item["result"]})).as_deref()
                        == Some(child)
            })
            || crate::platform::process_identity(pid)? != process
            || crate::platform::file_identity(path).map(|id| (id.device, id.inode))
                != Some((identity.device, identity.inode))
        {
            return None;
        }
        // The caller additionally rechecks the complete held source stamp
        // after the exact user/turn proof, before writing acceptance only.
        Some(())
    };
    prove().is_some()
}

pub(in crate::state) fn recover_main(record: &mut Value, event: &Value) -> Result<bool> {
    if record["agent"] != "codex"
        || event["hook_event_name"] != "UserPromptSubmit"
        || !string(event, "agent_id").is_empty()
        || string(event, "prompt").is_empty()
        || event["session_id"] == record["conversation_id"]
        || !string(record, "expected_id").is_empty()
        || pause_active(record)
        || !string(record, "error").is_empty()
        || record["pending_settings"].is_object()
        || clear_context::awaiting_start(record)
        || terminating::pending(record)
        || !external_agents::main_codex_hook(record)
    {
        return Ok(false);
    }
    let prove = || -> Option<(PathBuf, String, String)> {
        let parent = string(event, "session_id");
        let child = string(record, "conversation_id");
        if !uuid::Uuid::parse_str(parent).is_ok_and(|id| !id.is_nil() && id.to_string() == parent)
            || !uuid::Uuid::parse_str(child).is_ok_and(|id| !id.is_nil() && id.to_string() == child)
            || !first_start(record, parent)
        {
            return None;
        }
        let account = crate::platform::canonical(string(record, "agent_home")).ok()?;
        let path = PathBuf::from(string(event, "transcript_path"));
        if !path.is_absolute() || !path.starts_with(account.join("sessions")) {
            return None;
        }
        let mut file = crate::platform::no_follow(std::fs::OpenOptions::new().read(true))
            .open(&path)
            .ok()?;
        let identity = crate::platform::FileIdentity::with_metadata(&file).ok()?.0;
        let meta = header(&mut file)?;
        let payload = &meta["payload"];
        let created = chrono::DateTime::parse_from_rfc3339(string(payload, "timestamp"))
            .ok()?
            .timestamp_millis() as f64
            / 1000.0;
        // This narrow recovery is only for the original run's first native
        // conversation. Later legitimate /new/resume histories stay untouched.
        let launched = record["created"].as_f64()?;
        if meta["type"] != "session_meta"
            || payload["id"] != parent
            || payload["source"] != "cli"
            || payload.get("forked_from_id").is_some_and(|v| !v.is_null())
            || payload
                .get("parent_thread_id")
                .is_some_and(|v| !v.is_null())
            || !(launched - 1.0..=launched + 5.0).contains(&created)
        {
            return None;
        }
        let pid = u32::try_from(record["pid"].as_u64()?).ok()?;
        let process = crate::platform::process_identity(pid)?;
        if !matches!(crate::platform::holds_open(pid, &path).ok()?, crate::platform::OpenFiles::Held) {
            return None;
        }
        let (offset, bytes) = creation(&mut file, child, created)?;
        // Positive producer evidence survives unrelated appends, never a
        // replacement, truncation or edit of the selected creation record.
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut again = vec![0; bytes.len()];
        file.read_exact(&mut again).ok()?;
        if again != bytes
            || header(&mut file)? != meta
            || crate::platform::file_identity(&path).map(|id| (id.device, id.inode))
                != Some((identity.device, identity.inode))
            || crate::platform::process_identity(pid)? != process
            || !process_alive(record)
        {
            return None;
        }
        Some((path, parent.into(), child.into()))
    };
    let Some((path, parent, child)) = prove() else {
        return Ok(false);
    };
    let old = serde_json::to_vec(record).map_err(|e| e.to_string())?;
    if old.len() > 4 << 20 {
        return Ok(false);
    }
    let directory = root().join("codex-binding-repairs");
    private_dir(&directory)?;
    let audit = directory.join(format!(
        "{:x}.json",
        sha2::Sha256::digest(format!("{}\0{child}\0{parent}", string(record, "run_id")).as_bytes())
    ));
    match crate::platform::private_open(std::fs::OpenOptions::new().create_new(true).write(true))
        .open(&audit)
    {
        Ok(mut file) => {
            file.write_all(&old).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            crate::platform::sync_dir(&directory).map_err(|e| e.to_string())?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let retained: Value = serde_json::from_slice(
                &labels::read_file(&audit, 4 << 20)
                    .map_err(|e| e.to_string())?
                    .ok_or("Native binding repair audit vanished")?,
            )
            .map_err(|e| e.to_string())?;
            if retained["run_id"] != record["run_id"] || retained["conversation_id"] != child {
                return Err("Native binding repair audit conflicts".into());
            }
        }
        Err(error) => return Err(error.to_string()),
    }
    let child_record = json!({"external":true,"provider":"codex","conversation_id":child,
        "transcript":record["transcript"],"agent_home":record["agent_home"],"name":"Native Codex task",
        "state":"unknown","started":record["created"],"updated":now()});
    record["conversation_id"] = json!(parent);
    record["transcript"] = json!(path);
    record["codex_native_tasks"] =
        json!({"parent":parent,"run":record["run_id"],"pending":{},"threads":{child.clone():true}});
    record["subagents"] = json!({format!("external:codex:{child}"):child_record});
    for key in ["prompt", "last_message", "active_tools", "shell_jobs"] {
        record.as_object_mut().unwrap().remove(key);
    }
    record["codex_binding_recovery"] = json!({"from":child,"to":parent,"run_id":record["run_id"],
        "pending_request":record["input_pending_request"],"pending_at":record["input_pending_at"],
        "at":now(),"source":"original_native_prompt","replayed":false});
    // The real prompt proceeds through the ordinary hook/acceptance path.
    // Pending markers and every original input/unknown receipt are unchanged.
    Ok(true)
}
