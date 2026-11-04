//! The confirmation snapshot and delay for an end-of-session action. These
//! pure rules do no I/O, reserve no writer and never execute a pending action.
//! The native caller owns cancellation, the existing write lane, fresh reads
//! after the deadline and the one existing dispatcher. Nothing is persisted.
use crate::{actions, creation, details, Action, Exit, Invocation, Outcome, Target};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};

pub const UNDO_WINDOW: Duration = Duration::from_secs(3);
// Match the existing bounded native inspection budget. Provider metadata can
// contain a current question or other opaque details even without history.
pub const METADATA_LIMIT: usize = 2 * 1024 * 1024;
const RECEIPT_LIMIT: usize = 64 * 1024;
pub const STATE_CHANGED: &str = crate::termination_cleanup::STATE_CHANGED;

/// The exact confirmation, not an execution permission. Deserialized values
/// must pass `valid` again at every boundary; a reused name is never a target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Captured {
    pub target: Target,
    pub action: Action,
    pub state: String,
}

fn bounded(value: &str, maximum: usize, required: bool) -> bool {
    (!required || !value.is_empty())
        && value.len() <= maximum
        && !value.chars().any(char::is_control)
}

impl Captured {
    /// Only the owner's selected end actions. Stop, sends, maintenance,
    /// project deletion and Clear archive never acquire a delay here.
    pub fn valid(&self) -> bool {
        let t = &self.target;
        let archived = matches!(self.action, Action::ForgetArchive { .. });
        bounded(&t.computer, 512, false)
            && bounded(&t.name, 512, true)
            && bounded(
                &t.run_id,
                128,
                !matches!(self.action, Action::TerminateShell { .. }),
            )
            && bounded(&t.conversation_id, 256, false)
            && matches!(
                self.state.as_str(),
                "running" | "paused" | "stopped" | "archived"
            )
            && (self.state == "archived") == archived
            && match &self.action {
                Action::Archive | Action::Forget => self.state != "running",
                Action::Terminate | Action::Kill => self.state == "running",
                Action::TerminateShell { ref identity } => {
                    self.state == "running"
                        && t.run_id.is_empty()
                        && t.conversation_id.is_empty()
                        && identity.valid()
                }
                Action::ForgetBinding => !t.conversation_id.is_empty(),
                Action::ClearContext { .. } => {
                    self.state == "running" && !t.conversation_id.is_empty()
                }
                Action::ForgetArchive { archive_id } => creation::canonical_uuid(archive_id),
                _ => false,
            }
    }

    fn archive_id(&self) -> Option<&str> {
        match &self.action {
            Action::ForgetArchive { archive_id } => Some(archive_id),
            _ => None,
        }
    }

    /// Metadata only, through the existing reader. Never ask for historical
    /// events just to check whether a delayed action still names its source.
    pub fn inspection(&self) -> Result<Invocation, String> {
        if !self.valid() {
            return Err(STATE_CHANGED.into());
        }
        let mut inv = details::details_invocation(&self.target, self.archive_id());
        let at = inv.args.iter().position(|s| s == "--after").unwrap();
        inv.args[at + 1] = i64::MAX.to_string();
        Ok(inv)
    }

    /// This is fresh source equality only. The deadline, current native
    /// reservation and CLI's mutation-boundary identity checks still apply.
    pub fn matches(&self, value: &Value) -> bool {
        if let Action::TerminateShell { identity } = &self.action {
            return self.valid()
                && value["name"].as_str() == Some(&self.target.name)
                && value["tracked"] == false
                && ["run_id", "conversation_id"].iter().all(|key| {
                    value
                        .get(key)
                        .is_none_or(|v| v.is_null() || v.as_str() == Some(""))
                })
                && value
                    .get("state")
                    .is_none_or(|v| v.as_str() == Some("running"))
                && value
                    .get("runtime_state")
                    .is_none_or(|v| v.as_str() == Some("live"))
                && serde_json::from_value::<crate::untracked_shell::Identity>(
                    value["untracked_terminal"].clone(),
                )
                .is_ok_and(|current| &current == identity);
        }
        let text = |key: &str| value[key].as_str().unwrap_or("");
        // Native inspections report runtime_state, while catalog rows carry
        // state. A live Codex/Claude/Kimi inspection has no state field at all.
        // Check an explicit state when supplied; otherwise the runtime proves
        // live versus stopped (it does not distinguish paused from stopped).
        let runtime = if self.state == "running" {
            "live"
        } else {
            "stopped"
        };
        let explicit = value.get("state");
        let lifecycle = explicit.is_none_or(|state| state.as_str() == Some(self.state.as_str()))
            && value
                .get("runtime_state")
                .is_none_or(|state| state.as_str() == Some(runtime))
            && (explicit.is_some() || text("runtime_state") == runtime)
            && (self.state != "archived" || text("state") == "archived");
        self.valid()
            && text("name") == self.target.name
            && text("run_id") == self.target.run_id
            && text("conversation_id") == self.target.conversation_id
            && lifecycle
            && text("archive_id") == self.archive_id().unwrap_or("")
    }

    pub fn checked_inspection(&self, exit: Exit) -> Result<(), String> {
        let refusal = if matches!(self.action, Action::TerminateShell { .. }) {
            crate::untracked_shell::UNVERIFIED
        } else {
            STATE_CHANGED
        };
        let Exit::Code {
            code: 0, stdout, ..
        } = exit
        else {
            return Err(refusal.into());
        };
        if stdout.len() > METADATA_LIMIT
            || !serde_json::from_slice::<Value>(&stdout).is_ok_and(|v| self.matches(&v))
        {
            return Err(refusal.into());
        }
        Ok(())
    }

    /// The existing CLI's scoped lifecycle envelope, also supported by stable
    /// v0.47.0. Never fall back to a name-only delayed Archive or deletion.
    /// A shell may have an identified run with an empty native conversation.
    pub fn invocation(&self, request_id: &str) -> Result<Invocation, String> {
        if !self.valid() || !creation::canonical_uuid(request_id) {
            return Err(STATE_CHANGED.into());
        }
        if matches!(self.action, Action::ClearContext { .. }) {
            return actions::invocation(&self.target, &self.action, request_id);
        }
        if let Action::TerminateShell { identity } = &self.action {
            return actions::shell_termination_invocation(&self.target, identity, request_id);
        }
        let (op, local, remote) = match self.action {
            Action::Archive => ("archive", 30, 45),
            Action::Terminate => ("terminate", 45, 60),
            _ => ("forget", 15, 45),
        };
        let mut inv =
            actions::scoped_lifecycle_invocation(&self.target, op, request_id, local, remote)?;
        if let Some(id) = self.archive_id() {
            let mut body: Value = serde_json::from_slice(inv.stdin.as_deref().unwrap())
                .map_err(|error| error.to_string())?;
            body["archive_id"] = Value::String(id.into());
            inv.stdin = Some(serde_json::to_vec(&body).map_err(|error| error.to_string())?);
        }
        Ok(inv)
    }

    /// Reuses the existing exact receipt validator and cleanup-line decoder.
    /// An exit code alone never confirms the delayed action or offers replay.
    pub fn completed(&self, inv: &Invocation, exit: Exit) -> Outcome {
        if !self.valid() {
            return actions::failed(STATE_CHANGED);
        }
        if matches!(&exit, Exit::Code { stdout, .. } if stdout.len() > RECEIPT_LIMIT) {
            return actions::failed("Invalid acknowledgement. Check the session before retrying.");
        }
        if matches!(self.action, Action::ClearContext { .. }) {
            return actions::classify(&self.target, &self.action, inv, exit);
        }
        let cleanup = match &exit {
            Exit::Code {
                code: 0, stderr, ..
            } if matches!(self.action, Action::Archive | Action::Terminate) => {
                actions::cleanup_line(stderr)
            }
            _ => None,
        };
        let outcome = actions::classify_scoped_lifecycle(
            &self.target,
            inv,
            exit,
            "The session was not changed.",
        );
        match outcome {
            Outcome::Done { receipt, .. } => {
                if let Action::TerminateShell { identity } = &self.action {
                    if receipt["terminal_closed"] != true
                        || serde_json::to_value(identity).ok().as_ref()
                            != Some(&receipt["terminal_identity"])
                    {
                        return actions::failed(
                            "Invalid acknowledgement. Check the session before retrying.",
                        );
                    }
                }
                if self
                    .archive_id()
                    .is_some_and(|id| receipt["archive_id"].as_str() != Some(id))
                {
                    actions::failed("Invalid acknowledgement. Check the session before retrying.")
                } else {
                    Outcome::Done { receipt, cleanup }
                }
            }
            other => other,
        }
    }
}

/// Monotonic, one-use timing only; not an action, source proof or writer lease.
/// A native owner waits independently of webview timers, drops it on close or
/// quit, and checks the captured source again before the existing dispatcher.
pub struct Window {
    deadline: Instant,
    ended: bool,
}
impl Window {
    pub fn new(confirmed: Instant) -> Self {
        Self {
            deadline: confirmed + UNDO_WINDOW,
            ended: false,
        }
    }
    pub fn remaining(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }
    pub fn cancel(&mut self) {
        self.ended = true;
    }
    /// No early release, no replay, and no rescheduling on visibility changes.
    pub fn take_elapsed(&mut self, now: Instant) -> bool {
        if self.ended || now < self.deadline {
            return false;
        }
        self.ended = true;
        true
    }
}

#[cfg(test)]
mod tests;
