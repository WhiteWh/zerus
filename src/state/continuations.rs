//! Claude's background service runs jobs in processes that inherited the HGS
//! run token of whichever session started the service. A job's hooks never
//! belong to that session by the token: the job belongs to the session whose
//! agent window moved it to the background, as Claude's session registry
//! records (`jobId` of the job's process, `parkedJobId` of the window's).
use super::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::time::Duration;

/// How long a job that no live session holds is not looked for again.
const UNHELD: Duration = Duration::from_secs(60);

/// The Claude process that runs this hook, when Claude named it for this event's
/// own conversation rather than an outer Claude passing its variables on.
fn claude_process(sid: &str) -> Option<u32> {
    if nonempty_env("CLAUDE_CODE_SESSION_ID").as_deref() != Some(sid) {
        return None;
    }
    nonempty_env("CLAUDE_PID")?.parse().ok()
}

/// Whether a Claude process that is neither the session's agent nor runs under it
/// sent the event: the run token it carries is only inherited.
pub(super) fn foreign(record: Option<&Value>, sid: &str) -> bool {
    let Some(claude) = claude_process(sid) else {
        return false;
    };
    let agent = record
        .and_then(|record| record["pid"].as_u64())
        .and_then(|pid| u32::try_from(pid).ok());
    agent != Some(claude) && !agent.is_some_and(runs_under)
}

fn runs_under(agent: u32) -> bool {
    let mut pid = std::process::id();
    for _ in 0..64 {
        if pid == agent {
            return true;
        }
        let Some(process) = crate::platform::process_identity(pid) else {
            return false;
        };
        if process.parent == 0 || process.parent == pid {
            return false;
        }
        pid = process.parent;
    }
    false
}

/// A Claude process's entry in Claude's session registry.
fn registry(pid: u32) -> Option<Value> {
    let dir = nonempty_env("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"));
    let path = dir.join("sessions").join(format!("{pid}.json"));
    let entry: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    (entry["pid"] == pid).then_some(entry)
}

/// The background job the session's live agent window holds.
fn parked(record: &Value) -> Option<String> {
    if !process_alive(record) {
        return None;
    }
    let entry = registry(u32::try_from(record["pid"].as_u64()?).ok()?)?;
    Some(string(&entry, "parkedJobId").to_owned()).filter(|job| !job.is_empty())
}

/// The live session whose agent window holds the Claude background job that this
/// process runs under, while the job is on that session's conversation. A
/// command such a job runs proves its session this way, as a hook's event does.
pub(super) fn background_session() -> Option<Value> {
    let sid = nonempty_env("CLAUDE_CODE_SESSION_ID")?;
    let claude: u32 = nonempty_env("CLAUDE_PID")?.parse().ok()?;
    if !runs_under(claude) {
        return None;
    }
    // Nor through a recorded DeepSeek host (zerus-ppd26.15).
    if super::dsh::host_check() != super::dsh::HostCheck::Outside {
        return None;
    }
    let job = registry(claude)
        .filter(|entry| entry["kind"] == "bg")
        .map(|entry| string(&entry, "jobId").to_owned())
        .filter(|job| !job.is_empty())?;
    let mut holding = records().ok()?.into_iter().filter(|record| {
        string(record, "agent") == "claude"
            && record["state"] != "archived"
            && string(record, "conversation_id") == sid
            && parked(record).as_deref() == Some(job.as_str())
    });
    match (holding.next(), holding.next()) {
        (Some(record), None) => Some(record),
        _ => None,
    }
}

fn links() -> PathBuf {
    root().join("background_jobs")
}

fn read_link(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The session a link names, while its window still holds the job.
fn held(link: &Value) -> Option<Value> {
    read_run(string(link, "name"), string(link, "run_id"))
        .ok()?
        .filter(|record| parked(record).as_deref() == Some(string(link, "job")))
}

/// A link that no live session held when it was written a moment ago.
fn unheld(link: &Value, path: &Path) -> bool {
    link.is_object()
        && string(link, "run_id").is_empty()
        && fs::metadata(path)
            .and_then(|meta| meta.modified())
            .is_ok_and(|at| at.elapsed().is_ok_and(|age| age < UNHELD))
}

/// The live session whose agent window moved the background job that runs this
/// hook. A job that continues the session's conversation takes it on unchanged.
pub(super) fn owner(event: &Value, sid: &str, panes: &Snapshot) -> Result<Option<Value>> {
    let Some(job) = claude_process(sid)
        .and_then(registry)
        .filter(|entry| entry["kind"] == "bg")
        .map(|entry| string(&entry, "jobId").to_owned())
        .filter(|job| !job.is_empty())
    else {
        return Ok(None);
    };
    let path = links().join(format!("{:x}.json", Sha256::digest(job.as_bytes())));
    let link = read_link(&path);
    let mut record = match held(&link) {
        Some(record) => record,
        None if unheld(&link, &path) => return Ok(None),
        None => {
            let mut holding = records()?.into_iter().filter(|record| {
                string(record, "agent") == "claude"
                    && matches(record, panes_of(panes, record))
                    && parked(record).as_deref() == Some(job.as_str())
            });
            let found = match (holding.next(), holding.next()) {
                (Some(record), None) => Some(record),
                _ => None,
            };
            // A link outlives neither its window's hold on the job nor, unheld, a minute.
            for entry in fs::read_dir(links()).into_iter().flatten().flatten() {
                let old = read_link(&entry.path());
                if held(&old).is_none() && !unheld(&old, &entry.path()) {
                    let _ = fs::remove_file(entry.path());
                }
            }
            let link = found.as_ref().map_or_else(
                || json!({"job": job}),
                |record| json!({"job": job, "name": record["name"], "run_id": record["run_id"]}),
            );
            atomic(&path, &format!("{link}\n"))?;
            let Some(record) = found else {
                return Ok(None);
            };
            record
        }
    };
    // Owner routing precedes hook admission. A pending Terminate still needs
    // completion observations for its original conversation, never adoption of
    // a continued conversation before that admission gate can inspect it.
    if !terminating::pending(&record)
        && string(&record, "conversation_id") != sid
        && continued_in(&record).as_deref() == Some(sid)
    {
        record["conversation_id"] = json!(sid);
        if !string(event, "transcript_path").is_empty() {
            record["transcript"] = event["transcript_path"].clone();
        }
        write(&mut record)?;
    }
    Ok(Some(record))
}

/// The conversation that the mark ending the session's transcript names. Claude
/// may add its own bookkeeping after the mark; a later transcript entry means the
/// conversation went on here.
fn continued_in(record: &Value) -> Option<String> {
    const TAIL: u64 = 16 * 1024;
    let mut file = fs::File::open(string(record, "transcript")).ok()?;
    let length = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(TAIL))).ok()?;
    let mut tail = Vec::new();
    file.take(TAIL).read_to_end(&mut tail).ok()?;
    for line in tail.trim_ascii_end().rsplit(|byte| *byte == b'\n') {
        let entry: Value = serde_json::from_slice(line).ok()?;
        if entry["type"] == "continued-in" {
            return (entry["sessionId"] == record["conversation_id"])
                .then(|| string(&entry, "continuedInSessionId").to_owned())
                .filter(|sid| !sid.is_empty());
        }
        if entry.get("uuid").is_some() {
            return None;
        }
    }
    None
}
