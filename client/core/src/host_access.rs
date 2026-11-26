//! Multi-person host access (epic zerus-un7n, phase 5/6) for the desktop:
//! the caller's status on a host, the provider support matrix and the shared
//! project history and memory. hgs owns every decision (`hgs [@host]
//! host-access ... --json`, `multi-person-access-v1`); this module builds the
//! calls and turns the replies into typed outcomes, so the webview never
//! parses hgs text.
//!
//! A host whose installed hgs lacks the capability marker gets
//! [`Outcome::Unsupported`]; nothing is sent and nothing falls back to another
//! route. A refusal by the host is [`Outcome::Refused`] with its code, so the
//! UI states exactly why (not a member, revoked, binding required ...). The UI
//! hides nothing as a security measure: hosts enforce every rule.
use crate::actions::{Exit, Invocation};
use crate::run::run_limited;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

pub const MARKER: &str = "Multi-person access ABI: multi-person-access-v1";
const READ_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    History,
    Memory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Status,
    Support,
    List {
        project: String,
        kind: Kind,
        #[serde(default)]
        cursor: Option<String>,
        #[serde(default)]
        limit: Option<u32>,
    },
    Read {
        project: String,
        kind: Kind,
        person: String,
        id: String,
    },
    /// Publish approved messages of one of the caller's own sessions.
    Publish {
        project: String,
        title: String,
        session: String,
        entries: Vec<Entry>,
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        expected_generation: Option<u64>,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Continue with my account: a new session of the caller from a
    /// publication, never a resume of the author's session.
    Continue {
        project: String,
        person: String,
        id: String,
        generation: u64,
        #[serde(default)]
        request_id: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    /// The configured host alias; empty for this computer.
    #[serde(default)]
    pub host: String,
    pub op: Op,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Done {
        value: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    /// The host's hgs has no multi-person access. Nothing was sent.
    Unsupported { detail: String },
    /// The host refused, with its contract code. Nothing was changed.
    Refused { code: String, detail: String },
    /// Never sent, or failed before any effect.
    Failed { detail: String },
    /// A write may have been applied: read again; the same request ID may be
    /// sent again only with the identical body.
    Uncertain { detail: String, request_id: String },
}

fn host_ok(host: &str) -> bool {
    host.is_empty()
        || (host.len() <= 128 && !host.starts_with('-') && host.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
}

fn route(host: &str) -> Vec<String> {
    if host.is_empty() {
        Vec::new()
    } else {
        vec![format!("@{host}")]
    }
}

/// A host refusal from stderr: `host-access/<code>: <detail> (zerus-host-access-v1)`.
pub fn refusal(stderr: &str) -> Option<(String, String)> {
    let line = stderr.lines().find_map(|line| {
        let line = line.trim();
        line.find("host-access/").map(|at| &line[at..])
    })?;
    let rest = line.strip_prefix("host-access/")?;
    let (code, detail) = rest.split_once(": ")?;
    if code.is_empty() || code.len() > 64 || !code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return None;
    }
    let detail = detail.strip_suffix(" (zerus-host-access-v1)").unwrap_or(detail);
    Some((code.to_owned(), detail.chars().take(500).collect()))
}

impl Call {
    fn request_id(&self) -> Option<String> {
        match &self.op {
            Op::Publish { request_id, .. } | Op::Continue { request_id, .. } => {
                Some(request_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().hyphenated().to_string()))
            }
            _ => None,
        }
    }

    pub fn probe(&self) -> Invocation {
        let mut args = route(&self.host);
        args.push("--help".into());
        Invocation { args, stdin: None, timeout: Duration::from_secs(20), request_id: None }
    }

    /// The call, or the outcome when it cannot be made.
    #[allow(clippy::result_large_err)]
    pub fn prepare(&self) -> Result<Invocation, Outcome> {
        if !host_ok(&self.host) {
            return Err(Outcome::Failed { detail: "Choose a configured computer.".into() });
        }
        let request_id = self.request_id();
        let mut args = route(&self.host);
        args.push("host-access".into());
        let (words, stdin): (&[&str], Option<Value>) = match &self.op {
            Op::Status => (&["status", "--json"], None),
            Op::Support => (&["support", "--json"], None),
            Op::List { project, kind, cursor, limit } => (
                &["share", "list", "--json"],
                Some(json!({"project":project,"kind":kind,"cursor":cursor,"limit":limit.map(|l| l.min(100))})),
            ),
            Op::Read { project, kind, person, id } => {
                (&["share", "read", "--json"], Some(json!({"project":project,"kind":kind,"person":person,"id":id})))
            }
            Op::Publish { project, title, session, entries, id, expected_generation, .. } => {
                if entries.is_empty() {
                    return Err(Outcome::Failed { detail: "Choose the messages to publish.".into() });
                }
                let mut body = json!({"request_id":request_id,"project":project,"title":title,"session":session,"entries":entries});
                if let Some(id) = id {
                    body["id"] = json!(id);
                    body["expected_generation"] = json!(expected_generation);
                }
                (&["share", "publish", "--json"], Some(body))
            }
            Op::Continue { project, person, id, generation, .. } => (
                &["share", "continue", "--json"],
                Some(json!({"request_id":request_id,"project":project,"person":person,"id":id,"generation":generation})),
            ),
        };
        args.extend(words.iter().map(|w| w.to_string()));
        Ok(Invocation {
            args,
            stdin: stdin.map(|value| value.to_string().into_bytes()),
            timeout: Duration::from_secs(60),
            request_id,
        })
    }
}

/// The outcome of a finished call.
pub fn outcome(call: &Call, invocation: &Invocation, exit: Exit) -> Outcome {
    let write = invocation.request_id.clone();
    match exit {
        Exit::Code { code: 0, stdout, .. } => match serde_json::from_slice::<Value>(&stdout) {
            Ok(value) => Outcome::Done { value, request_id: write },
            Err(_) => match write {
                Some(request_id) => Outcome::Uncertain { detail: "The host's reply was unreadable.".into(), request_id },
                None => Outcome::Failed { detail: "The host's reply was unreadable.".into() },
            },
        },
        Exit::Code { stderr, code, .. } => {
            if let Some((code, detail)) = refusal(&stderr) {
                return Outcome::Refused { code, detail };
            }
            // Exit 3: the host was not reached, so nothing ran there.
            if code == 3 || write.is_none() {
                return Outcome::Failed { detail: stderr.lines().next().unwrap_or("The call failed.").chars().take(500).collect() };
            }
            Outcome::Uncertain { detail: "The host may have applied the change.".into(), request_id: write.unwrap_or_default() }
        }
        Exit::StartFailed(_) | Exit::WriteFailed => Outcome::Failed { detail: "hgs could not be started.".into() },
        Exit::TimedOut | Exit::Abnormal { .. } => match write {
            Some(request_id) => Outcome::Uncertain { detail: "The call did not finish.".into(), request_id },
            None => Outcome::Failed { detail: format!("{:?} did not finish.", call.op).chars().take(200).collect() },
        },
    }
}

/// Probes the host's marker, then runs the call.
pub fn run(hgs: &Path, call: &Call) -> Outcome {
    let invocation = match call.prepare() {
        Ok(invocation) => invocation,
        Err(outcome) => return outcome,
    };
    match run_limited(hgs, &call.probe(), &[], 256 * 1024) {
        Ok(Exit::Code { code: 0, stdout, .. }) if String::from_utf8_lossy(&stdout).lines().any(|line| line == MARKER) => {}
        Ok(Exit::Code { code: 0, .. }) => {
            return Outcome::Unsupported { detail: "This computer's hgs has no multi-person host access; update it there.".into() }
        }
        _ => return Outcome::Failed { detail: "Could not check the computer's hgs.".into() },
    }
    match run_limited(hgs, &invocation, &[], READ_LIMIT) {
        Ok(exit) => outcome(call, &invocation, exit),
        Err(_) => Outcome::Failed { detail: "The host's reply exceeded its bound.".into() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(op: Op) -> Call {
        Call { host: "build".into(), op }
    }

    #[test]
    fn calls_route_to_the_host_with_json_on_stdin() {
        let list = call(Op::List { project: "p".into(), kind: Kind::History, cursor: None, limit: Some(5000) }).prepare().unwrap();
        assert_eq!(list.args, ["@build", "host-access", "share", "list", "--json"]);
        let body: Value = serde_json::from_slice(list.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(body["limit"], 100);
        assert!(list.request_id.is_none());
        let status = Call { host: String::new(), op: Op::Status }.prepare().unwrap();
        assert_eq!(status.args, ["host-access", "status", "--json"]);
        assert!(call(Op::Status).probe().args.ends_with(&["--help".to_owned()]));
    }

    #[test]
    fn writes_carry_one_request_id_and_never_an_author_session() {
        let invocation = call(Op::Continue { project: "p".into(), person: "a".repeat(64), id: "i".into(), generation: 2, request_id: None })
            .prepare()
            .unwrap();
        let body: Value = serde_json::from_slice(invocation.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(body["request_id"], invocation.request_id.clone().unwrap().as_str());
        assert_eq!(body.as_object().unwrap().len(), 5);
        assert!(body.get("session").is_none());
        let empty = call(Op::Publish { project: "p".into(), title: "t".into(), session: "s".into(), entries: vec![], id: None, expected_generation: None, request_id: None });
        assert!(matches!(empty.prepare(), Err(Outcome::Failed { .. })));
        assert!(matches!(Call { host: "-oProxy".into(), op: Op::Status }.prepare(), Err(Outcome::Failed { .. })));
    }

    #[test]
    fn host_refusals_become_typed_outcomes() {
        let stderr = "hgs: host-access/not-a-member: you are not a member of this project (zerus-host-access-v1)\n";
        assert_eq!(refusal(stderr), Some(("not-a-member".into(), "you are not a member of this project".into())));
        assert_eq!(refusal("Some other failure"), None);
        assert_eq!(refusal("host-access/BAD CODE: x"), None);
        let c = call(Op::Status);
        let inv = c.prepare().unwrap();
        let refused = outcome(&c, &inv, Exit::Code { code: 1, stdout: vec![], stderr: stderr.into() });
        assert!(matches!(refused, Outcome::Refused { ref code, .. } if code == "not-a-member"));
    }

    #[test]
    fn an_uncertain_write_keeps_its_request_id_and_reads_are_never_uncertain() {
        let write = call(Op::Publish { project: "p".into(), title: "t".into(), session: "s".into(),
            entries: vec![Entry { role: "person".into(), text: "x".into() }], id: None, expected_generation: None, request_id: None });
        let inv = write.prepare().unwrap();
        let id = inv.request_id.clone().unwrap();
        assert_eq!(outcome(&write, &inv, Exit::TimedOut), Outcome::Uncertain { detail: "The call did not finish.".into(), request_id: id });
        let read = call(Op::Support);
        let inv = read.prepare().unwrap();
        assert!(matches!(outcome(&read, &inv, Exit::TimedOut), Outcome::Failed { .. }));
        // Exit 3: unreachable, nothing ran.
        let inv = write.prepare().unwrap();
        assert!(matches!(outcome(&write, &inv, Exit::Code { code: 3, stdout: vec![], stderr: "unreachable".into() }), Outcome::Failed { .. }));
    }

    #[test]
    fn the_marker_matches_the_cli() {
        assert_eq!(MARKER, "Multi-person access ABI: multi-person-access-v1");
    }
}
