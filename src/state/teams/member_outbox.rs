//! WhiteWh fork: a team member's report and question when the lead's computer
//! cannot be reached from the member's.
//!
//! A member on another computer reports with `hgs @LEAD team report --json`,
//! which needs SSH from the member's computer to the lead's. The lead's
//! computer always reaches the member's (it started the member there), but a
//! Windows PC often runs no SSH server. When the call cannot reach the lead's
//! computer, `report` and `ask` are kept here instead, in the member's
//! computer's state (`teams/member-outbox`), and the lead's team worker collects them
//! over its own connection (`team outbox take`), applies each on its computer
//! as if the member had sent it (authorized by the attempt id, as before) and
//! then removes them (`team outbox ack`). A taken item that was never
//! acknowledged is offered again after its lease; the request id keeps a
//! repeated report or question from counting twice.
use super::super::{private_dir, root};
use anyhow::{bail, Context};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The most items one `take` returns.
const TAKE_LIMIT: usize = 50;
/// The most items kept; a member that keeps failing never fills the disk.
const KEEP_LIMIT: usize = 500;
/// How long a taken item waits for its acknowledgement before it is offered again.
const LEASE: Duration = Duration::from_secs(10 * 60);
/// How long an item that was never applied is kept.
const EXPIRE: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the lead's worker asks one member computer.
const COLLECT: Duration = Duration::from_secs(15);

fn dir() -> PathBuf {
    root().join("teams/member-outbox")
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|time| time.as_secs_f64()).unwrap_or_default()
}

/// Keeps a member's `report` or `ask` for the lead's computer `target`; the
/// receipt the member reads instead of the lead's.
pub(crate) fn queue(target: &str, op: &str, input: &Value) -> anyhow::Result<Value> {
    if !matches!(op, "report" | "ask") {
        bail!("only a report or a question waits for the lead's computer");
    }
    let team = input["team"].as_str().unwrap_or_default();
    if team.is_empty() {
        bail!("the call names no team");
    }
    let dir = dir();
    fs::create_dir_all(&dir)?;
    private_dir(&dir).map_err(|error| anyhow::anyhow!("{error}"))?;
    if fs::read_dir(&dir)?.count() >= KEEP_LIMIT {
        bail!("{KEEP_LIMIT} reports and questions already wait for the lead's computer");
    }
    let id = uuid::Uuid::new_v4().to_string();
    let item = json!({"id": id, "target": target, "team": team, "op": op, "input": input, "queued_at": now()});
    let path = dir.join(format!("{id}.json"));
    let stage = dir.join(format!(".{id}.tmp"));
    fs::write(&stage, serde_json::to_vec(&item)?)?;
    fs::rename(&stage, &path)?;
    Ok(json!({
        "queued": true,
        "id": id,
        "op": op,
        "to": target,
        "note": format!("{target} cannot be reached from this computer; its team worker collects this {} from here within a minute", if op == "report" { "report" } else { "question" }),
    }))
}

/// `hgs team outbox take|ack --json`, run by the lead's worker over SSH.
pub(crate) fn command(args: &[String], input: &Value) -> anyhow::Result<Value> {
    if crate::cli::agent_caller() {
        bail!("Only the lead computer's team worker collects a member's reports");
    }
    match args {
        [verb] if verb == "take" => take(input),
        [verb] if verb == "ack" => ack(input),
        _ => bail!("usage: hgs team outbox take|ack --json"),
    }
}

/// The waiting items of the named teams, each leased to this take.
fn take(input: &Value) -> anyhow::Result<Value> {
    let teams: Vec<&str> = input["teams"].as_array().map(|teams| teams.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let dir = dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(json!({"items": []}));
    };
    let now = now();
    let mut items = Vec::new();
    for entry in entries.flatten() {
        if items.len() >= TAKE_LIMIT {
            break;
        }
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(mut item) = fs::read(&path).map_err(anyhow::Error::from).and_then(|bytes| Ok(serde_json::from_slice::<Value>(&bytes)?)) else {
            continue;
        };
        if !teams.contains(&item["team"].as_str().unwrap_or_default()) {
            continue;
        }
        if item["queued_at"].as_f64().is_some_and(|at| now - at > EXPIRE.as_secs_f64()) {
            let _ = fs::remove_file(&path);
            continue;
        }
        let leased = item["taken_at"].as_f64().is_some_and(|at| now - at < LEASE.as_secs_f64());
        if leased {
            continue;
        }
        item["taken_at"] = json!(now);
        fs::write(&path, serde_json::to_vec(&item)?).with_context(|| format!("{}", path.display()))?;
        items.push(json!({"id": item["id"], "team": item["team"], "op": item["op"], "input": item["input"]}));
    }
    Ok(json!({"items": items}))
}

/// Removes the items the lead's computer has applied.
fn ack(input: &Value) -> anyhow::Result<Value> {
    let mut removed = 0;
    for id in input["ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if uuid::Uuid::parse_str(id).is_err() {
            continue;
        }
        if fs::remove_file(dir().join(format!("{id}.json"))).is_ok() {
            removed += 1;
        }
    }
    Ok(json!({"removed": removed}))
}

/// The lead's side, from its worker's tick: each computer that runs a member
/// of `teams` is asked at most every [`COLLECT`] for what its members kept;
/// each item is applied here as the member's own call, and removed there once
/// applied. A computer whose hgs predates this, or that cannot be reached,
/// is asked again later.
/// When the lead's worker last asked each member computer.
static ASKED: std::sync::Mutex<Option<std::collections::HashMap<String, std::time::Instant>>> = std::sync::Mutex::new(None);
/// Task #18 of team c536f0e5 (P2.6b): the least time between two asks of one
/// computer, also when its catalog keeps changing.
const HURRIED: Duration = Duration::from_secs(1);

/// Task #18: the computer's catalog showed a member's turn change, which a
/// kept report usually comes with: the next collection asks it at once (at
/// most every `HURRIED`).
pub(crate) fn hurry(computer: &str) {
    let mut asked = ASKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(at) = asked.as_mut().and_then(|asked| asked.get_mut(computer)) {
        if let Some(earlier) = std::time::Instant::now().checked_sub(COLLECT) {
            if at.elapsed() >= HURRIED {
                *at = earlier;
            }
        }
    }
}

pub(crate) fn collect(teams: &[super::model::Team]) {
    use std::collections::{BTreeMap, HashMap};
    use std::time::Instant;
    let mut computers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for team in teams {
        for member in team.members.iter().filter(|member| !member.computer.is_empty()) {
            let ids = computers.entry(member.computer.clone()).or_default();
            if !ids.contains(&team.id) {
                ids.push(team.id.clone());
            }
        }
    }
    for (computer, ids) in computers {
        {
            let mut asked = ASKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let asked = asked.get_or_insert_with(HashMap::new);
            if asked.get(&computer).is_some_and(|at| at.elapsed() < COLLECT) {
                continue;
            }
            asked.insert(computer.clone(), Instant::now());
        }
        let hgs = super::dispatcher::hgs;
        let Ok(output) = hgs(&computer, &["team", "outbox", "take", "--json"], Some(&json!({"teams": ids})), Duration::from_secs(60)) else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let Ok(answer) = serde_json::from_slice::<Value>(&output.stdout) else {
            continue;
        };
        let mut applied = Vec::new();
        for item in answer["items"].as_array().into_iter().flatten() {
            let op = item["op"].as_str().unwrap_or_default();
            if !matches!(op, "report" | "ask") {
                applied.push(item["id"].clone());
                continue;
            }
            match hgs("", &["team", op, "--json"], Some(&item["input"]), Duration::from_secs(120)) {
                Ok(done) if done.status.success() => applied.push(item["id"].clone()),
                Ok(done) => eprintln!(
                    "hgs team worker: a {op} kept on {computer} was not applied: {}",
                    String::from_utf8_lossy(&done.stderr).trim()
                ),
                Err(error) => eprintln!("hgs team worker: a {op} kept on {computer}: {error:#}"),
            }
        }
        if !applied.is_empty() {
            if let Err(error) = hgs(&computer, &["team", "outbox", "ack", "--json"], Some(&json!({"ids": applied})), Duration::from_secs(60)) {
                eprintln!("hgs team worker: {computer}: acknowledging kept reports: {error:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_report_is_taken_once_per_lease_and_gone_after_its_ack() {
        let temp = tempfile::tempdir().unwrap();
        super::super::super::set_test_root(Some(temp.path().to_path_buf()));
        let input = json!({"request_id": "r1", "team": "t1", "task": 1, "attempt": "a1", "text": "done"});
        let receipt = queue("lead-pc", "report", &input).unwrap();
        assert_eq!(receipt["queued"], true);
        assert!(queue("lead-pc", "current", &input).is_err(), "only a report or a question waits");
        assert_eq!(take(&json!({"teams": ["other"]})).unwrap()["items"], json!([]), "another team's take finds nothing");
        let taken = take(&json!({"teams": ["t1"]})).unwrap();
        assert_eq!(taken["items"][0]["input"], input);
        assert_eq!(taken["items"][0]["op"], "report");
        assert_eq!(take(&json!({"teams": ["t1"]})).unwrap()["items"], json!([]), "leased until acknowledged");
        let id = taken["items"][0]["id"].clone();
        assert_eq!(ack(&json!({"ids": [id, "../escape"]})).unwrap()["removed"], 1);
        assert_eq!(take(&json!({"teams": ["t1"]})).unwrap()["items"], json!([]));
        super::super::super::set_test_root(None);
    }
}
