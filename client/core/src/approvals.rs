//! The automatic approval journal (`hgs approval-log list|detail`), read as
//! tray/src/AutomaticApprovalSettings.cpp and HgsClient::requestApprovalLog
//! read it: local only, after a `--help` probe for the command (an older
//! `hgs` treats an unknown word as a session launch), bounded in time and
//! size, 50 entries a page. Journal reads never authorize input. [`automatic`] contains
//! the separate dangerous opt-in worker's policy and durable-reservation gate.

pub mod automatic;

use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::actions::{Exit, Invocation};

/// The help line that shows the command exists.
pub const HELP_MARKER: &str = "hgs approval-log begin|finish|list|detail";
/// The most a reply may hold.
const MAX_REPLY: usize = 256 * 1024;
/// The most a `--help` probe may print.
const HELP_BYTES: usize = 256 * 1024;
/// Entries a page holds at most.
pub const PAGE: usize = 50;

/// The probe, as Qt runs it (5 s).
pub fn probe() -> Invocation {
    Invocation {
        args: vec!["--help".into()],
        stdin: None,
        timeout: Duration::from_secs(5),
        request_id: None,
    }
}

/// Qt's failureDetail for a probe that did not finish cleanly.
pub fn probed(hgs: &Path, exit: Option<Exit>) -> Result<(), String> {
    match exit {
        Some(Exit::Code {
            code: 0, stdout, ..
        }) => {
            if String::from_utf8_lossy(&stdout).contains(HELP_MARKER) {
                Ok(())
            } else {
                Err("Update the local hgs CLI to use automatic approvals and its journal.".into())
            }
        }
        Some(Exit::TimedOut) => Err("timed out".into()),
        Some(Exit::StartFailed(error)) => Err(format!("cannot start '{}': {error}", hgs.display())),
        Some(Exit::Code { code, stderr, .. }) => {
            let detail: String = stderr.trim().chars().take(4000).collect();
            Err(if detail.is_empty() {
                format!("exit code {code}")
            } else {
                detail
            })
        }
        Some(Exit::Abnormal { stderr }) if !stderr.trim().is_empty() => {
            Err(stderr.trim().chars().take(4000).collect())
        }
        _ => Err("exit code -1".into()),
    }
}

/// The journal reads and the separate opt-in worker's begin/finish writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Begin,
    Finish,
    List,
    Detail,
}

impl Operation {
    fn word(self) -> &'static str {
        match self {
            Operation::Begin => "begin",
            Operation::Finish => "finish",
            Operation::List => "list",
            Operation::Detail => "detail",
        }
    }
}

/// `hgs approval-log OPERATION` with `data` on stdin (10 s).
pub fn invocation(operation: Operation, data: &Value) -> Invocation {
    Invocation {
        args: vec!["approval-log".into(), operation.word().into()],
        stdin: Some(data.to_string().into_bytes()),
        timeout: Duration::from_secs(10),
        request_id: None,
    }
}

/// A list request: exact computer and session filters (trimmed), and the
/// cursor of a newer (`after`) or older (`before`) page; 0 for none.
pub fn list_request(host: &str, session: &str, before: i64, after: i64) -> Value {
    let mut request = json!({"host": host.trim(), "session": session.trim()});
    if before > 0 {
        request["before"] = json!(before);
    }
    if after > 0 {
        request["after"] = json!(after);
    }
    request
}

/// The reply as HgsClient words its failures: the CLI's own error, or Qt's
/// texts for an empty, oversized or unexplained one. None stands for output
/// past the limit.
pub fn answer(exit: Option<Exit>) -> Result<Map<String, Value>, String> {
    let unavailable = || {
        "The automatic approval journal is unavailable. No new approval was authorized.".to_owned()
    };
    let invalid = || "Invalid automatic approval journal response.".to_owned();
    match exit {
        None => Err(invalid()),
        Some(Exit::Code {
            code: 0, stdout, ..
        }) => match serde_json::from_slice::<Value>(&stdout) {
            Ok(Value::Object(reply)) if !reply.is_empty() && stdout.len() <= MAX_REPLY => Ok(reply),
            _ => Err(invalid()),
        },
        Some(Exit::Code { stderr, .. }) | Some(Exit::Abnormal { stderr }) => {
            let error: String = stderr.trim().chars().take(2000).collect();
            Err(if error.is_empty() {
                unavailable()
            } else {
                error
            })
        }
        Some(_) => Err(unavailable()),
    }
}

/// Probes, then runs the operation. Blocks; never on the main thread.
/// `supported` remembers a passed probe, as HgsClient does.
pub fn call(
    hgs: &Path,
    operation: Operation,
    data: &Value,
    supported: &mut bool,
) -> Result<Map<String, Value>, String> {
    if !*supported {
        match crate::run::run_limited(hgs, &probe(), &[], HELP_BYTES) {
            Ok(help) => probed(hgs, Some(help))?,
            // Qt reads all of it; a help this long is no hgs we know.
            Err(_) => return Err(format!("help exceeds {HELP_BYTES} bytes")),
        }
        *supported = true;
    }
    match crate::run::run_limited(hgs, &invocation(operation, data), &[], MAX_REPLY) {
        Ok(exit) => answer(Some(exit)),
        Err(too_large) => past_limit(&too_large.stderr),
    }
}

/// A reply past the limit was cut off before its exit was known: the
/// CLI's own error when it wrote one, as Qt shows a failed call's, else
/// Qt's text for an oversized reply.
fn past_limit(stderr: &str) -> Result<Map<String, Value>, String> {
    let error: String = stderr.trim().chars().take(2000).collect();
    Err(if error.is_empty() {
        "Invalid automatic approval journal response.".into()
    } else {
        error
    })
}

/// What a journal entry's status reads as.
pub fn status_text(status: &str) -> &'static str {
    match status {
        "approved" => "Approved once",
        "failed" => "Failed",
        "cancelled" => "Cancelled",
        _ => "Unconfirmed",
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    pub id: i64,
    /// Milliseconds since the epoch.
    pub at: i64,
    pub host: String,
    pub session: String,
    pub prompt: String,
    /// The status as the page shows it.
    pub result: String,
}

/// One page and its cursors (0: none that way).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Page {
    pub entries: Vec<Entry>,
    pub next: i64,
    pub previous: i64,
}

/// QJsonValue::toInteger: a whole number, otherwise 0.
fn integer(value: &Value) -> i64 {
    value.as_i64().unwrap_or_else(|| {
        value
            .as_f64()
            .filter(|n| n.fract() == 0.0 && n.abs() < 9.2e18)
            .map_or(0, |n| n as i64)
    })
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// A list reply as the page reads it; more than 50 entries is refused.
pub fn page(reply: &Map<String, Value>) -> Result<Page, String> {
    let empty = Vec::new();
    let entries = reply
        .get("entries")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if entries.len() > PAGE {
        return Err("Invalid approval journal page.".into());
    }
    Ok(Page {
        entries: entries
            .iter()
            .map(|entry| Entry {
                id: integer(&entry["id"]),
                at: integer(&entry["at"]),
                host: text(&entry["host"]),
                session: text(&entry["session"]),
                prompt: text(&entry["prompt"]),
                result: status_text(entry["status"].as_str().unwrap_or_default()).into(),
            })
            .collect(),
        next: reply.get("next").map_or(0, integer),
        previous: reply.get("previous").map_or(0, integer),
    })
}

/// A detail reply as Qt shows it: the outcome, each question with its body
/// and the answers chosen for it, then the outcome's detail.
pub fn detail(reply: &Map<String, Value>) -> String {
    let mut parts = vec![status_text(
        reply
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .to_owned()];
    let empty = Vec::new();
    let answers = reply
        .get("answers")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let questions = reply
        .get("question")
        .and_then(|question| question["questions"].as_array())
        .unwrap_or(&empty);
    for item in questions {
        parts.push(text(&item["question"]));
        parts.push(text(&item["body"]));
        for option in item["options"].as_array().unwrap_or(&empty) {
            for answer in answers {
                let chosen = answer["selected_option_ids"].as_array();
                if chosen.is_some_and(|ids| ids.contains(&option["id"])) {
                    parts.push(format!("Selected answer: {}", text(&option["label"])));
                }
            }
        }
    }
    let outcome = reply.get("detail").map(text).unwrap_or_default();
    if !outcome.is_empty() {
        parts.push(outcome);
    }
    parts.join("\n\n")
}

#[cfg(test)]
mod tests;
