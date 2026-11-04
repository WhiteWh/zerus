//! Guarded session actions: the `hgs` calls the Qt client makes
//! (`tray/src/HgsClient.cpp`) and the rules that turn their exits into outcomes.
use crate::attachments::{self, Attachment};
use crate::questions;
use crate::view::ViewRow;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

const CLEANUP_PREFIX: &str = "ZERUS_WORKTREE_CLEANUP ";
const STDERR_CHARS: usize = 2000;

/// The session an action applies to; `computer` is "" for this computer, else
/// the SSH alias.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub computer: String,
    pub name: String,
    pub run_id: String,
    pub conversation_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryControl {
    Now,
    Cancel,
}

impl RecoveryControl {
    fn as_str(self) -> &'static str {
        match self {
            Self::Now => "now",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Qt's guarded `hgs settings NAME --json`, including pending applies.
    Settings {
        model: String,
        effort: String,
        #[serde(default)]
        expected_pending_id: Option<String>,
        #[serde(default)]
        without_terminal: bool,
    },
    Interrupt {
        turn_started: f64,
    },
    MarkRead {
        reply_id: Option<String>,
        attention_signature: Option<String>,
    },
    Pause,
    Resume,
    /// `hgs resume NAME --fresh [--expected-run-id RUN] -d`
    /// (`startFreshSession`): a new conversation in the same folder, with the
    /// same account and model.
    StartFresh,
    Rename {
        new_name: String,
        archive_id: Option<String>,
    },
    Archive,
    Terminate,
    /// `hgs kill NAME`: Terminate for a live session without an HGS archive
    /// (`killSession`; `SessionsWindow::terminateSession` picks it).
    Kill,
    /// A confirmation-bound, untracked shell terminal. Never falls back to Kill.
    TerminateShell {
        identity: crate::untracked_shell::Identity,
    },
    /// `hgs send NAME --json` (`requestSendMessage`): the message and its
    /// files go only to standard input. Built in Rust from the held draft:
    /// webview JSON cannot produce it, so no attachment bytes cross the IPC.
    #[serde(skip_deserializing)]
    Send {
        text: String,
        attachments: Vec<Attachment>,
        /// A subagent's ID; none for the session's own agent.
        agent_id: Option<String>,
        /// The compaction this message waits for (compact-to-send).
        expected_compaction_id: Option<String>,
    },
    /// `hgs processes NAME --stop ID --run RUN --conversation C [--archive A]
    /// [--generation G]` (`requestProcess` with stop): one stop request,
    /// sent one at a time and never retried (`process_view::StopQueue`).
    StopProcess {
        process_id: String,
        #[serde(default)]
        archive_id: Option<String>,
        #[serde(default)]
        generation: Option<String>,
    },
    /// `hgs clear-context NAME --json` (`requestClearContext`): start a new
    /// context in the same session. `dismiss_prompt` first dismisses a
    /// chooser the agent shows; it is sent only when set, since an older hgs
    /// rejects the field.
    ClearContext {
        #[serde(default)]
        dismiss_prompt: bool,
    },
    /// `hgs fork NAME -n TAG -d [--archive A] [--expected-run-id R]
    /// [--expected-conversation-id C]` (`forkSession`): an independent
    /// conversation from this saved context, in the same working folder, named
    /// with the same agent and project and a new tag.
    Fork {
        tag: String,
        #[serde(default)]
        archive_id: Option<String>,
    },
    /// `hgs compact-context NAME --json` (`requestCompactContext`): the
    /// agent's native /compact. Its receipt's `request_id` is the compaction
    /// a waiting message names (compact-to-send).
    CompactContext,
    /// Stage dropped files on the session's computer, as Qt's
    /// `requestStageFiles` (the `attachment` session action). The paths must
    /// be absolute paths of regular files; they are read when the call is
    /// built, so callers build it off the UI thread.
    StageFiles {
        paths: Vec<PathBuf>,
        /// The untracked session's creation time, checked by `hgs` when the
        /// session has neither a run nor a conversation.
        #[serde(default)]
        expected_created: Option<i64>,
    },
    /// `hgs send-now NAME --json` (`requestQueueSendNow`): the agent sends
    /// its native queue now, when it still is the queue `queue_id` names.
    /// Built in Rust from the inspected queue, never from webview JSON.
    #[serde(skip_deserializing)]
    SendNow {
        queue_id: String,
    },
    /// `hgs session-action NAME --json` with `remove_queued`
    /// (`requestRemoveQueued`): removes one OpenCode inbox message that was
    /// not delivered. Built in Rust after the owner confirmed it.
    #[serde(skip_deserializing)]
    RemoveQueued {
        message_id: String,
    },
    /// `hgs answer NAME --json` (`requestAnswerQuestion`): the answers to one
    /// pending question, pinned to its exact id, hash, run and conversation.
    /// Built in Rust from the inspected question and the held answer draft.
    #[serde(skip_deserializing)]
    Answer {
        question: Value,
        answers: Vec<questions::AnswerItem>,
    },
    /// `hgs recovery action --scoped-json`: Retry now or Cancel retry for the
    /// current waiting job of this exact run and conversation.
    Recovery {
        job_id: String,
        action: RecoveryControl,
    },
    /// `hgs kill NAME` as Forget saved session; its own kind so notices and
    /// logs tell it from Terminate's Kill (`killSession`).
    Forget,
    /// `hgs session-action NAME --json` with `"action":"forget"`: the OpenCode
    /// binding forget (`requestForgetBinding`).
    ForgetBinding,
    /// `hgs resume NAME --archive ID -d`: Restore an archived session
    /// (`restoreArchive`). An empty id is refused before any call.
    Restore {
        archive_id: String,
    },
    /// `hgs kill NAME --archive ID`: Forget an archived session
    /// (`forgetArchive`). An empty id is refused before any call.
    ForgetArchive {
        archive_id: String,
    },
    /// `hgs pause --all`: every live session of the computer (`pauseAll`).
    /// `count` is the live sessions the menu counted; it sets the timeout.
    /// Computer-wide: the target's session name is empty and ignored.
    PauseAll {
        count: u32,
    },
    /// `hgs resume --all -d`: every saved session of the computer (`resumeAll`).
    ResumeAll {
        count: u32,
    },
}

/// One `hgs` call: arguments (with a leading `@alias` for a peer), standard
/// input, the time it may take and, for Interrupt, the request identity.
#[derive(Clone, Debug, PartialEq)]
pub struct Invocation {
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Duration,
    pub request_id: Option<String>,
}

/// How a call ended, as the runner observed it.
#[derive(Clone, Debug, PartialEq)]
pub enum Exit {
    Code {
        code: i32,
        stdout: Vec<u8>,
        stderr: String,
    },
    Abnormal {
        stderr: String,
    },
    TimedOut,
    StartFailed(String),
    WriteFailed,
}

/// What `hgs` reported about the worktree of an archived or terminated session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorktreeCleanup {
    pub status: String,
    pub path: String,
    pub common_dir: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<Box<crate::termination_cleanup::ArchiveIdentity>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<Value>,
    #[serde(default)]
    pub details_omitted: bool,
}

impl WorktreeCleanup {
    /// A complete bounded row for this exact reported folder. It describes
    /// the earlier check; it never establishes current removal authority.
    pub fn review_row(&self) -> Option<&Value> {
        self.review.as_ref().filter(|row| {
            row.is_object()
                && row["path"].as_str() == Some(self.path.as_str())
                && row["kind"] == "linked"
                && serde_json::to_vec(row).is_ok_and(|bytes| bytes.len() <= 60 * 1024)
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Done {
        receipt: Value,
        cleanup: Option<WorktreeCleanup>,
    },
    Failed {
        detail: String,
    },
    /// The request may have taken effect: a send that timed out, ended
    /// abnormally or returned no valid receipt. Never retried automatically.
    Uncertain {
        detail: String,
    },
}

pub(crate) fn failed(detail: impl Into<String>) -> Outcome {
    Outcome::Failed {
        detail: detail.into(),
    }
}

fn uncertain(detail: impl Into<String>) -> Outcome {
    Outcome::Uncertain {
        detail: detail.into(),
    }
}

fn done(receipt: Value, cleanup: Option<WorktreeCleanup>) -> Outcome {
    Outcome::Done { receipt, cleanup }
}

/// `SessionTag::problem` (`tray/src/SessionTag.h`): why a session tag is not
/// accepted, None when it is. `hgs rename` enforces the same rule.
pub fn tag_problem(tag: &str) -> Option<&'static str> {
    if tag.is_empty() {
        return Some(crate::source!("Enter a session name."));
    }
    if tag.trim() != tag {
        return Some(crate::source!("Remove spaces at the beginning or end."));
    }
    if tag
        .chars()
        .any(|c| matches!(c, '/' | ':' | '.') || c.is_control())
    {
        return Some(crate::source!(
            "Names cannot contain /, : or . or control characters."
        ));
    }
    None
}

/// What Fork shows: whether it is available and its tooltip (the agent's
/// reason, or Qt's default). `busy`: a session update, fork, settings change
/// or send runs, or a restored session awaits its inspection.
pub fn fork_available(
    online: bool,
    terminating: bool,
    busy: bool,
    details: &Value,
) -> (bool, String) {
    let tooltip = details["fork_reason"]
        .as_str()
        .unwrap_or("Create an independent conversation with the same context and working folder")
        .to_owned();
    let enabled =
        online && !terminating && !busy && details["fork_supported"].as_bool() == Some(true);
    (enabled, tooltip)
}

/// The fork's suggested tag: `fork`, or the session's tag with `-fork`, then
/// `-2`, `-3`… while a session on the same computer has that name.
pub fn fork_tag(tag: &str, occupied: impl Fn(&str) -> bool) -> String {
    let base = if tag.is_empty() {
        "fork".to_owned()
    } else {
        format!("{tag}-fork")
    };
    let mut candidate = base.clone();
    let mut number = 2;
    while occupied(&candidate) {
        candidate = format!("{base}-{number}");
        number += 1;
    }
    candidate
}

/// Why the fork dialog cannot take this tag, `None` when it can.
pub fn fork_problem(tag: &str, occupied: impl Fn(&str) -> bool) -> Option<String> {
    tag_problem(tag)
        .map(str::to_owned)
        .or_else(|| occupied(tag).then(|| "This session name is already in use.".to_owned()))
}

/// The fork dialog's texts.
pub mod fork_texts {
    pub const TITLE: &str = "Fork session";
    pub const BUTTON: &str = "Fork session";

    pub fn description(folder: &str) -> String {
        format!(
            "Create an independent conversation from this saved context.\nWorking folder: {folder}"
        )
    }

    pub fn forking(machine: &str) -> String {
        format!("Forking session on {machine}…")
    }
}

/// The rename the Qt dialog makes: the agent and project of `old` stay, only
/// the tag after them changes (`src/state/rename.rs` refuses anything else).
fn rename_problem(old: &str, new: &str) -> Option<&'static str> {
    let mut parts = old.splitn(3, '/');
    let (agent, project) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let tag = (!agent.is_empty() && !project.is_empty())
        .then(|| {
            new.strip_prefix(agent)?
                .strip_prefix('/')?
                .strip_prefix(project)?
                .strip_prefix('/')
        })
        .flatten();
    match tag {
        Some(tag) => tag_problem(tag),
        None => Some("rename must keep the same agent/project and use a nonempty tag without '/', ':', '.', control characters or surrounding whitespace"),
    }
}

pub fn invocation(
    target: &Target,
    action: &Action,
    request_id: &str,
) -> Result<Invocation, String> {
    let name = if matches!(action, Action::PauseAll { .. } | Action::ResumeAll { .. }) {
        String::new()
    } else {
        target.name.clone()
    };
    let (mut args, stdin, local, peer, request) = match action {
        Action::Settings {
            model,
            effort,
            expected_pending_id,
            ..
        } => {
            if model.is_empty()
                || model.len() > 256
                || effort.len() > 80
                || model.chars().chain(effort.chars()).any(char::is_control)
                || target.run_id.is_empty()
            {
                return Err("Refresh the session before changing model settings.".into());
            }
            let mut input = json!({"request_id":request_id,"expected_run_id":target.run_id,
                "expected_conversation_id":target.conversation_id,"model":model});
            if !effort.is_empty() {
                input["effort"] = json!(effort);
            }
            if let Some(id) = expected_pending_id {
                input["expected_pending_id"] = json!(id);
            }
            (
                vec!["settings".into(), name, "--json".into()],
                Some(serde_json::to_vec(&input).map_err(|e| e.to_string())?),
                20,
                35,
                Some(request_id.to_owned()),
            )
        }
        Action::Interrupt { turn_started } => {
            let input = json!({
                "request_id": request_id,
                "expected_run_id": target.run_id,
                "expected_conversation_id": target.conversation_id,
                "expected_turn_started": turn_started,
            });
            (
                vec!["interrupt".to_owned(), name, "--json".to_owned()],
                Some(serde_json::to_vec(&input).map_err(|error| error.to_string())?),
                15,
                45,
                Some(request_id.to_owned()),
            )
        }
        Action::MarkRead {
            reply_id,
            attention_signature,
        } => {
            let input = json!({
                "run_id": target.run_id,
                "conversation_id": target.conversation_id,
                "reply_id": reply_id,
                "attention_signature": attention_signature,
            });
            (
                vec!["read".to_owned(), name, "--json".to_owned()],
                Some(serde_json::to_vec(&input).map_err(|error| error.to_string())?),
                15,
                35,
                None,
            )
        }
        Action::Recovery { job_id, action } => {
            if target.name.is_empty()
                || target.run_id.is_empty()
                || target.conversation_id.is_empty()
                || !uuid_shaped(job_id)
                || job_id.to_ascii_lowercase() != *job_id
                || job_id.chars().all(|c| c == '0' || c == '-')
            {
                return Err("Recovery needs its exact session, run, conversation and job.".into());
            }
            let input = json!({
                "name": target.name,
                "id": job_id,
                "action": action.as_str(),
            });
            (
                vec!["recovery".to_owned(), "action".to_owned()],
                Some(serde_json::to_vec(&input).map_err(|error| error.to_string())?),
                10,
                25,
                None,
            )
        }
        Action::Pause => (vec!["pause".to_owned(), name], None, 45, 45, None),
        Action::Resume => (
            vec!["resume".to_owned(), name, "-d".to_owned()],
            None,
            30,
            30,
            None,
        ),
        Action::StartFresh => {
            let mut args = vec!["resume".to_owned(), name, "--fresh".to_owned()];
            if !target.run_id.is_empty() {
                args.extend(["--expected-run-id".to_owned(), target.run_id.clone()]);
            }
            args.push("-d".to_owned());
            (args, None, 30, 30, None)
        }
        Action::Rename {
            new_name,
            archive_id,
        } => {
            if let Some(problem) = rename_problem(&name, new_name) {
                return Err(problem.into());
            }
            let mut args = vec!["rename".to_owned(), name, new_name.clone()];
            if let Some(id) = archive_id.as_ref().filter(|id| !id.is_empty()) {
                args.extend(["--archive".to_owned(), id.clone()]);
            }
            (args, None, 5, 15, None)
        }
        Action::Archive => (vec!["archive".to_owned(), name], None, 30, 45, None),
        Action::Terminate => {
            let mut args = vec!["terminate".to_owned(), name];
            if !target.run_id.is_empty() {
                args.extend(["--expected-run-id".to_owned(), target.run_id.clone()]);
            }
            (args, None, 45, 60, None)
        }
        Action::TerminateShell { identity } => {
            return shell_termination_invocation(target, identity, request_id)
        }
        // A local kill is a tmux kill-session; a peer's goes through SSH.
        Action::Kill if target.run_id.is_empty() => {
            return Err(crate::untracked_shell::UNVERIFIED.into())
        }
        Action::Kill | Action::Forget => (vec!["kill".to_owned(), name], None, 5, 15, None),
        Action::Send {
            text,
            attachments,
            agent_id,
            expected_compaction_id,
        } => {
            let input = send_input(
                target,
                text,
                attachments,
                agent_id.as_deref(),
                expected_compaction_id.as_deref(),
                request_id,
            )?;
            (
                vec!["send".to_owned(), name, "--json".to_owned()],
                Some(input),
                15,
                45,
                Some(request_id.to_owned()),
            )
        }
        Action::StopProcess {
            process_id,
            archive_id,
            generation,
        } => {
            let mut args = vec![
                "processes".to_owned(),
                name,
                "--stop".to_owned(),
                process_id.clone(),
                "--run".to_owned(),
                target.run_id.clone(),
                "--conversation".to_owned(),
                target.conversation_id.clone(),
            ];
            if let Some(archive) = archive_id.as_ref().filter(|id| !id.is_empty()) {
                args.extend(["--archive".to_owned(), archive.clone()]);
            }
            if let Some(generation) = generation.as_ref().filter(|id| !id.is_empty()) {
                args.extend(["--generation".to_owned(), generation.clone()]);
            }
            (args, None, 45, 45, None)
        }
        Action::SendNow { queue_id } => {
            let input = json!({
                "request_id": request_id,
                "expected_run_id": target.run_id,
                "expected_conversation_id": target.conversation_id,
                "queue_id": queue_id,
            });
            (
                vec!["send-now".to_owned(), name, "--json".to_owned()],
                Some(serde_json::to_vec(&input).map_err(|error| error.to_string())?),
                20,
                35,
                Some(request_id.to_owned()),
            )
        }
        Action::Answer { question, answers } => {
            let input = answer_input(&target.name, question, answers, request_id)?;
            (
                vec!["answer".to_owned(), name, "--json".to_owned()],
                Some(input),
                30,
                45,
                Some(request_id.to_owned()),
            )
        }
        Action::RemoveQueued { message_id } => {
            if !native_message_id(message_id) {
                return Err("Choose the exact queued OpenCode message.".to_owned());
            }
            let input = json!({
                "request_id": request_id,
                "action": "remove_queued",
                "expected_run_id": target.run_id,
                "expected_conversation_id": target.conversation_id,
                "message_id": message_id,
            });
            (
                vec!["session-action".to_owned(), name, "--json".to_owned()],
                Some(serde_json::to_vec(&input).map_err(|error| error.to_string())?),
                15,
                45,
                Some(request_id.to_owned()),
            )
        }
        Action::ClearContext { dismiss_prompt } => {
            let mut input = json!({
                "request_id": request_id,
                "expected_run_id": target.run_id,
                "expected_conversation_id": target.conversation_id,
            });
            if *dismiss_prompt {
                input["dismiss_prompt"] = json!(true);
            }
            (
                vec!["clear-context".to_owned(), name, "--json".to_owned()],
                Some(serde_json::to_vec(&input).map_err(|error| error.to_string())?),
                15,
                45,
                Some(request_id.to_owned()),
            )
        }
        Action::Fork { tag, archive_id } => {
            if let Some(problem) = tag_problem(tag) {
                return Err(problem.into());
            }
            let mut args = vec![
                "fork".to_owned(),
                name,
                "-n".to_owned(),
                tag.clone(),
                "-d".to_owned(),
            ];
            if let Some(archive) = archive_id.as_ref().filter(|id| !id.is_empty()) {
                args.extend(["--archive".to_owned(), archive.clone()]);
            }
            if !target.run_id.is_empty() {
                args.extend(["--expected-run-id".to_owned(), target.run_id.clone()]);
            }
            if !target.conversation_id.is_empty() {
                args.extend([
                    "--expected-conversation-id".to_owned(),
                    target.conversation_id.clone(),
                ]);
            }
            (args, None, 60, 90, None)
        }
        Action::CompactContext => {
            let input = json!({
                "request_id": request_id,
                "expected_run_id": target.run_id,
                "expected_conversation_id": target.conversation_id,
            });
            (
                vec!["compact-context".to_owned(), name, "--json".to_owned()],
                Some(serde_json::to_vec(&input).map_err(|error| error.to_string())?),
                15,
                45,
                Some(request_id.to_owned()),
            )
        }
        Action::StageFiles {
            paths,
            expected_created,
        } => {
            let mut input = json!({
                "request_id": request_id,
                "expected_run_id": target.run_id,
                "expected_conversation_id": target.conversation_id,
                "attachments": stage_files(paths)?,
            });
            if let Some(created) = stage_created(target, *expected_created) {
                input["expected_created"] = json!(created);
            }
            (
                vec![
                    "attachment".to_owned(),
                    name,
                    "--stage".to_owned(),
                    "--json".to_owned(),
                ],
                Some(serde_json::to_vec(&input).map_err(|error| error.to_string())?),
                15,
                45,
                Some(request_id.to_owned()),
            )
        }
        Action::Restore { archive_id } | Action::ForgetArchive { archive_id } => {
            // Archive identity must never collapse to a generic resume or
            // kill of a reused name (`restoreArchive`, `forgetArchive`).
            if archive_id.is_empty() {
                return Err("Missing archive identity".into());
            }
            let restore = matches!(action, Action::Restore { .. });
            let mut args = vec![
                if restore { "resume" } else { "kill" }.to_owned(),
                name,
                "--archive".to_owned(),
                archive_id.clone(),
            ];
            if restore {
                args.push("-d".to_owned());
                (args, None, 30, 30, None)
            } else {
                (args, None, 5, 15, None)
            }
        }
        Action::PauseAll { count } => {
            let secs = 15 + 30 * u64::from((*count).clamp(1, 10000));
            (
                vec!["pause".to_owned(), "--all".to_owned()],
                None,
                secs,
                secs,
                None,
            )
        }
        Action::ResumeAll { count } => {
            let secs = 15 + 15 * u64::from((*count).clamp(1, 10000));
            (
                vec!["resume".to_owned(), "--all".to_owned(), "-d".to_owned()],
                None,
                secs,
                secs,
                None,
            )
        }
        Action::ForgetBinding => {
            if target.conversation_id.is_empty() {
                return Err("the session has no run or conversation to forget".into());
            }
            return scoped_lifecycle_invocation(target, "forget", request_id, 15, 45);
        }
    };
    let peer_call = prepend_alias(&mut args, &target.computer);
    Ok(Invocation {
        args,
        stdin,
        timeout: Duration::from_secs(if peer_call { peer } else { local }),
        request_id: request,
    })
}

/// Shared native lifecycle envelope. The operation is chosen in Rust; this is
/// not a webview-selected command or a handoff/execution permission.
pub(crate) fn shell_termination_invocation(
    target: &Target,
    identity: &crate::untracked_shell::Identity,
    request_id: &str,
) -> Result<Invocation, String> {
    if !target.run_id.is_empty()
        || !target.conversation_id.is_empty()
        || target.name.is_empty()
        || !identity.valid()
        || !crate::creation::canonical_uuid(request_id)
    {
        return Err(crate::untracked_shell::UNVERIFIED.into());
    }
    let mut args = vec![
        "session-action".into(),
        target.name.clone(),
        "--json".into(),
    ];
    let peer = prepend_alias(&mut args, &target.computer);
    Ok(Invocation { args, stdin: Some(serde_json::to_vec(&json!({
        "request_id":request_id,"action":"terminate_shell","expected_run_id":"","expected_conversation_id":"","terminal_identity":identity
    })).map_err(|e| e.to_string())?), timeout: Duration::from_secs(if peer { 60 } else { 45 }), request_id: Some(request_id.into()) })
}

pub(crate) fn scoped_lifecycle_invocation(
    target: &Target,
    operation: &str,
    request_id: &str,
    local: u64,
    peer: u64,
) -> Result<Invocation, String> {
    if target.run_id.is_empty() {
        return Err("the session has no identified run".into());
    }
    let input = json!({"request_id":request_id,"action":operation,"expected_run_id":target.run_id,"expected_conversation_id":target.conversation_id});
    let mut args = vec![
        "session-action".into(),
        target.name.clone(),
        "--json".into(),
    ];
    let remote = prepend_alias(&mut args, &target.computer);
    Ok(Invocation {
        args,
        stdin: Some(serde_json::to_vec(&input).map_err(|error| error.to_string())?),
        timeout: Duration::from_secs(if remote { peer } else { local }),
        request_id: Some(request_id.into()),
    })
}

/// At most this much message text, in UTF-8 bytes (`MaximumMessageBytes`).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// While a send to the same session and agent is in flight.
pub const ALREADY_SENDING: &str = "A message to this session is already being sent.";
/// The answer lane of a session is busy.
pub const ALREADY_ANSWERING: &str = "An answer to this session is already being sent.";
/// The largest answer `hgs answer` takes on standard input, and the largest
/// acknowledgement it reads back.
const MAXIMUM_ANSWER_BYTES: usize = 64 * 1024;

/// `requestSendMessage`'s checks and standard input.
fn send_input(
    target: &Target,
    text: &str,
    files: &[Attachment],
    agent_id: Option<&str>,
    compaction_id: Option<&str>,
    request_id: &str,
) -> Result<Vec<u8>, String> {
    if target.name.is_empty() || target.run_id.is_empty() {
        return Err("Refresh the session before sending a message.".into());
    }
    if text.len() > MAX_MESSAGE_BYTES || files.len() > attachments::MAX_ATTACHMENTS {
        return Err("Use at most 64 KiB of text and 8 attachments.".into());
    }
    let mut total = 0;
    for file in files {
        if file.bytes.is_empty() || file.bytes.len() > attachments::MAX_ATTACHMENT_BYTES {
            return Err("Each attachment must be nonempty and at most 10 MiB.".into());
        }
        total += file.bytes.len();
        if total > attachments::MAX_TOTAL_BYTES {
            return Err("Attachments exceed 20 MiB in total.".into());
        }
    }
    if text.trim().is_empty() && files.is_empty() {
        return Err("Write a message or attach a file first.".into());
    }
    let mut input = json!({
        "request_id": request_id,
        "text": text,
        "attachments": files,
        "expected_run_id": target.run_id,
        "expected_conversation_id": target.conversation_id,
    });
    if let Some(agent) = agent_id.filter(|id| !id.is_empty()) {
        input["agent_id"] = json!(agent);
    }
    if let Some(compaction) = compaction_id.filter(|id| !id.is_empty()) {
        input["expected_compaction_id"] = json!(compaction);
    }
    serde_json::to_vec(&input).map_err(|error| error.to_string())
}

/// `requestStageFiles`: 1-8 absolute paths of regular, readable, nonempty
/// files of at most 10 MiB each and 20 MiB together. The type comes from the
/// content (`attachments::sniff`), and the name is the file's own.
fn stage_files(paths: &[PathBuf]) -> Result<Vec<Attachment>, String> {
    if paths.is_empty() || paths.len() > attachments::MAX_ATTACHMENTS {
        return Err("Drop between 1 and 8 files.".into());
    }
    let unreadable = || "Each file must be readable, nonempty, and at most 10 MiB.".to_owned();
    let mut total = 0;
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        if !path.is_absolute() {
            return Err(unreadable());
        }
        let metadata = std::fs::metadata(path).map_err(|_| unreadable())?;
        if !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > attachments::MAX_ATTACHMENT_BYTES as u64
        {
            return Err(unreadable());
        }
        let mut bytes = Vec::new();
        File::open(path)
            .and_then(|file| {
                file.take(attachments::MAX_ATTACHMENT_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
            })
            .map_err(|_| unreadable())?;
        total += bytes.len();
        if bytes.is_empty()
            || bytes.len() > attachments::MAX_ATTACHMENT_BYTES
            || total > attachments::MAX_TOTAL_BYTES
        {
            return Err("Files exceed 20 MiB in total or changed during transfer.".into());
        }
        files.push(Attachment {
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            mime: attachments::sniff(&bytes),
            bytes: bytes.into(),
            reference: String::new(),
        });
    }
    Ok(files)
}

/// The creation time a stage checks: only for an untracked session, which has
/// neither a run nor a conversation.
fn stage_created(target: &Target, expected_created: Option<i64>) -> Option<i64> {
    (target.run_id.is_empty() && target.conversation_id.is_empty())
        .then_some(expected_created)
        .flatten()
        .filter(|created| *created >= 0)
}

/// `hgs [@host] reference NAME --json --run RUN --conversation CONVERSATION
/// [--archive ID]`, 15 s here and 25 s on a peer (`requestSessionReference`,
/// `HgsClient.cpp:592-604`).
pub fn reference_invocation(target: &Target, archive_id: Option<&str>) -> Invocation {
    let mut args = vec![
        "reference".to_owned(),
        target.name.clone(),
        "--json".to_owned(),
        "--run".to_owned(),
        target.run_id.clone(),
        "--conversation".to_owned(),
        target.conversation_id.clone(),
    ];
    if let Some(id) = archive_id.filter(|id| !id.is_empty()) {
        args.extend(["--archive".to_owned(), id.to_owned()]);
    }
    let peer = prepend_alias(&mut args, &target.computer);
    Invocation {
        args,
        stdin: None,
        timeout: Duration::from_secs(if peer { 25 } else { 15 }),
        request_id: None,
    }
}

const REFERENCE_PREFIX: &str = "zerus://session?";
const REFERENCE_CHARS: usize = 8192;
const FAILURE_STDERR_CHARS: usize = 4000;

/// `failureDetail` (`HgsClient.cpp:466`): why a call failed.
pub(crate) fn failure_detail(exit: Exit) -> String {
    let (stderr, code) = match exit {
        Exit::TimedOut => return "timed out".to_owned(),
        Exit::StartFailed(error) => {
            return format!(
                "cannot start '{}': {error}",
                crate::default_hgs().to_string_lossy()
            )
        }
        Exit::Code { code, stderr, .. } => (stderr, code),
        Exit::Abnormal { stderr } => (stderr, -1),
        Exit::WriteFailed => (String::new(), -1),
    };
    let detail: String = stderr.trim().chars().take(FAILURE_STDERR_CHARS).collect();
    if detail.is_empty() {
        format!("exit code {code}")
    } else {
        detail
    }
}

/// The session reference `hgs reference` printed: a JSON object whose `uri`
/// starts with `zerus://session?` and is at most 8192 characters.
pub fn parse_reference(exit: Exit) -> Result<String, String> {
    let Exit::Code {
        code: 0,
        ref stdout,
        ..
    } = exit
    else {
        return Err(failure_detail(exit));
    };
    let uri = serde_json::from_slice::<Value>(stdout)
        .ok()
        .and_then(|value| value.get("uri")?.as_str().map(str::to_owned))
        .filter(|uri| uri.starts_with(REFERENCE_PREFIX) && uri.chars().count() <= REFERENCE_CHARS);
    uri.ok_or_else(|| "Could not read a session reference. Update hgs on this computer.".to_owned())
}

const NOTIFICATIONS_MARKER: &str = "session-notifications-v1";
const NOTIFICATIONS_IDENTITY: &str =
    "The session notification identity is unavailable. Refresh the session.";
/// Shown when the local `hgs` lacks the shared-notification marker.
pub const NOTIFICATIONS_UPDATE: &str =
    "Update the configured local hgs to support shared notification preferences.";
const NOTIFICATIONS_UNCONFIRMED: &str =
    "The shared notification change was not confirmed. Refresh the session before trying again.";

/// The local `hgs --help` probe that runs before the first notification call
/// (`requestNotifications`, `HgsClient.cpp:2401`): an old CLI could read the
/// unknown command as an agent launch. 5 s, no input.
pub fn help_invocation() -> Invocation {
    Invocation {
        args: vec!["--help".to_owned()],
        stdin: None,
        timeout: Duration::from_secs(5),
        request_id: None,
    }
}

/// Whether the help text has a line that is exactly `session-notifications-v1`
/// (a trailing CR is allowed, nothing else).
pub fn has_notifications_marker(stdout: &[u8]) -> bool {
    stdout.split(|&b| b == b'\n').any(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        line == NOTIFICATIONS_MARKER.as_bytes()
    })
}

/// A probe that passes: a normal exit 0 with the marker.
pub fn help_confirms(exit: &Exit) -> bool {
    matches!(exit, Exit::Code { code: 0, stdout, .. } if has_notifications_marker(stdout))
}

/// `SessionNotifications::text` for an identity: non-empty, at most `maximum`
/// bytes, no control character.
fn identity_ok(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(|c| c < ' ' || c == '\u{7f}')
}

/// `hgs [@host] notifications NAME --json` with exactly the four payload keys;
/// 15 s here and 35 s on a peer. The identity is checked first, as Qt does.
pub fn notifications_invocation(
    target: &Target,
    muted: bool,
    expected_revision: Option<i64>,
) -> Result<Invocation, String> {
    let expected = expected_revision.filter(|revision| *revision >= 0);
    let Some(expected) = expected.filter(|_| {
        !target.name.is_empty()
            && identity_ok(&target.run_id, 128)
            && identity_ok(&target.conversation_id, 256)
    }) else {
        return Err(NOTIFICATIONS_IDENTITY.to_owned());
    };
    let input = json!({
        "run_id": target.run_id,
        "conversation_id": target.conversation_id,
        "muted": muted,
        "expected_revision": expected,
    });
    let mut args = vec![
        "notifications".to_owned(),
        target.name.clone(),
        "--json".to_owned(),
    ];
    let peer = prepend_alias(&mut args, &target.computer);
    Ok(Invocation {
        args,
        stdin: Some(serde_json::to_vec(&input).map_err(|error| error.to_string())?),
        timeout: Duration::from_secs(if peer { 35 } else { 15 }),
        request_id: None,
    })
}

/// The reply's `preferences` when Qt's rule confirms the change: exit 0, exactly
/// `ok, applied, run_id, conversation_id, preferences`, `ok` true, a boolean
/// `applied`, this session's identities, valid preferences with the requested
/// `muted`, and revision `expected + 1` when applied, else `expected`. Anything
/// else is the trimmed stderr or Qt's fallback text.
pub fn classify_notifications(
    target: &Target,
    muted: bool,
    expected_revision: i64,
    exit: Exit,
) -> Result<Value, String> {
    let stderr = match &exit {
        Exit::Code { stderr, .. } | Exit::Abnormal { stderr } => stderr.as_str(),
        _ => "",
    };
    let fail = || trimmed_stderr(stderr).unwrap_or_else(|| NOTIFICATIONS_UNCONFIRMED.to_owned());
    let Exit::Code {
        code: 0, stdout, ..
    } = &exit
    else {
        return Err(fail());
    };
    let Ok(Value::Object(mut reply)) = serde_json::from_slice::<Value>(stdout) else {
        return Err(fail());
    };
    let exact = reply.len() == 5
        && ["ok", "applied", "run_id", "conversation_id", "preferences"]
            .iter()
            .all(|key| reply.contains_key(*key));
    let applied = reply.get("applied").and_then(Value::as_bool);
    let preferences = reply.get("preferences").and_then(Value::as_object);
    let revision = preferences.and_then(|p| crate::view::preferences_revision(p.get("revision")));
    let confirmed = exact
        && reply.get("ok") == Some(&Value::Bool(true))
        && reply.get("run_id").and_then(Value::as_str) == Some(target.run_id.as_str())
        && reply.get("conversation_id").and_then(Value::as_str)
            == Some(target.conversation_id.as_str())
        && preferences.is_some_and(|p| {
            crate::view::preferences_valid(p, &target.conversation_id)
                && p.get("muted") == Some(&Value::Bool(muted))
        })
        && match (applied, revision) {
            (Some(true), Some(revision)) => {
                expected_revision < i64::MAX && revision == expected_revision + 1
            }
            (Some(false), Some(revision)) => revision == expected_revision,
            _ => false,
        };
    if !confirmed {
        return Err(fail());
    }
    Ok(reply.remove("preferences").unwrap_or(Value::Null))
}

const HELP_CAP: usize = 256 * 1024;
const NOTIFICATIONS_CAP: usize = 512 * 1024;

/// The whole shared-notification change: the local help probe (refused without
/// the marker), then the call, then [`classify_notifications`]. Blocks.
pub fn change_notifications(
    hgs: &std::path::Path,
    target: &Target,
    muted: bool,
    expected_revision: Option<i64>,
) -> Result<Value, String> {
    let call = notifications_invocation(target, muted, expected_revision)?;
    let expected = expected_revision.unwrap_or_default();
    let probe = crate::run::run_limited(hgs, &help_invocation(), &[], HELP_CAP);
    if !probe.as_ref().is_ok_and(help_confirms) {
        return Err(NOTIFICATIONS_UPDATE.to_owned());
    }
    // A reply of more than the cap is not a reply (Qt's maximumOutputBytes).
    let exit =
        crate::run::run_limited(hgs, &call, &[], NOTIFICATIONS_CAP).unwrap_or_else(|too_large| {
            Exit::Abnormal {
                stderr: too_large.stderr,
            }
        });
    classify_notifications(target, muted, expected, exit)
}

/// Puts `@alias` first for a peer; true when `computer` is a peer.
pub(crate) fn prepend_alias(args: &mut Vec<String>, computer: &str) -> bool {
    let peer = !computer.is_empty();
    if peer {
        args.insert(0, format!("@{computer}"));
    }
    peer
}

fn trimmed_stderr(stderr: &str) -> Option<String> {
    let text = stderr.trim();
    (!text.is_empty()).then(|| text.chars().take(STDERR_CHARS).collect())
}

/// Qt's recovery action is admitted only after a fresh full inspection still
/// names the same session, run, conversation and waiting job.
pub fn recovery_preflight(target: &Target, action: &Action, exit: Exit) -> Result<(), String> {
    let Action::Recovery { job_id, .. } = action else {
        return Ok(());
    };
    let Exit::Code {
        code: 0, stdout, ..
    } = exit
    else {
        return Err(
            "Recovery state could not be confirmed. Reload its state before trying again.".into(),
        );
    };
    let details: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
    let job = &details["recovery"];
    if details["run_id"] == target.run_id
        && details["conversation_id"] == target.conversation_id
        && job["name"] == target.name
        && job["id"] == *job_id
        && job["state"] == "waiting"
    {
        Ok(())
    } else {
        Err("This scheduled recovery is no longer current; nothing was changed".into())
    }
}

pub fn classify(target: &Target, action: &Action, invocation: &Invocation, exit: Exit) -> Outcome {
    match action {
        Action::Settings {
            model,
            effort,
            without_terminal,
            ..
        } => classify_settings(target, invocation, model, effort, *without_terminal, exit),
        Action::Interrupt { .. } | Action::CompactContext => {
            classify_session_action(target, invocation, exit, &["submitted"], None)
        }
        // A clear may be confirmed at once.
        Action::ClearContext { .. } => {
            classify_session_action(target, invocation, exit, &["submitted", "confirmed"], None)
        }
        Action::StageFiles {
            expected_created, ..
        } => classify_session_action(
            target,
            invocation,
            exit,
            &["staged"],
            stage_created(target, *expected_created),
        ),
        Action::Send { agent_id, .. } => {
            classify_send(target, invocation, agent_id.as_deref(), exit)
        }
        Action::StopProcess { process_id, .. } => classify_stop(target, process_id, exit),
        Action::SendNow { queue_id } => classify_send_now(target, invocation, queue_id, exit),
        Action::RemoveQueued { .. } => classify_remove_queued(target, invocation, exit),
        Action::Answer { question, answers } => {
            classify_answer(target, invocation, question, answers, exit)
        }
        Action::Recovery { job_id, action } => {
            classify_recovery(target, invocation, job_id, *action, exit)
        }
        Action::ForgetBinding => {
            classify_scoped_lifecycle(target, invocation, exit, "The session was not forgotten.")
        }
        Action::MarkRead { .. } => classify_read(target, exit),
        Action::Archive | Action::Terminate => classify_write(exit, true),
        Action::TerminateShell { .. } => crate::end_action::Captured {
            target: target.clone(),
            action: action.clone(),
            state: "running".into(),
        }
        .completed(invocation, exit),
        _ => classify_write(exit, false),
    }
}

fn classify_settings(
    target: &Target,
    invocation: &Invocation,
    model: &str,
    effort: &str,
    without_terminal: bool,
    exit: Exit,
) -> Outcome {
    let check = if without_terminal {
        "the session"
    } else {
        "Terminal"
    };
    let reason = match exit {
        Exit::StartFailed(error) => return failed(error),
        Exit::TimedOut => format!("Settings change timed out. Check {check} before retrying."),
        Exit::WriteFailed => format!("Settings transfer failed. Check {check} before retrying."),
        Exit::Code {
            code: 0, stdout, ..
        } => {
            let receipt: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            let text = |key| receipt.get(key).and_then(Value::as_str);
            if matches!(text("status"), Some("applied" | "scheduled"))
                && invocation.request_id.is_some()
                && text("request_id") == invocation.request_id.as_deref()
                && text("name") == Some(target.name.as_str())
                && text("run_id") == Some(target.run_id.as_str())
                && text("conversation_id") == Some(target.conversation_id.as_str())
                && text("model") == Some(model)
                && (effort.is_empty() || text("effort") == Some(effort))
            {
                return done(receipt, None);
            }
            format!("Invalid settings acknowledgement. Check {check} before retrying.")
        }
        Exit::Code { stderr, .. } | Exit::Abnormal { stderr } => trimmed_stderr(&stderr)
            .unwrap_or_else(|| {
                format!("Could not confirm session settings. Check {check} before retrying.")
            }),
    };
    failed(reason)
}

/// `runSessionAction`: the receipt must echo this exact request and session,
/// in the state the action reaches, and the creation time it checked.
fn classify_session_action(
    target: &Target,
    invocation: &Invocation,
    exit: Exit,
    states: &[&str],
    expected_created: Option<i64>,
) -> Outcome {
    match exit {
        Exit::TimedOut => failed("Request timed out. Check Terminal before retrying."),
        Exit::WriteFailed => failed("Transfer failed. Check Terminal before retrying."),
        Exit::StartFailed(error) => failed(error),
        Exit::Abnormal { stderr } => failed(
            trimmed_stderr(&stderr)
                .unwrap_or_else(|| "Could not complete the request. Check Terminal.".to_owned()),
        ),
        Exit::Code { code, stderr, .. } if code != 0 => failed(
            trimmed_stderr(&stderr)
                .unwrap_or_else(|| "Could not complete the request. Check Terminal.".to_owned()),
        ),
        Exit::Code { stdout, .. } => {
            let receipt: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            let field = |key: &str| receipt.get(key).and_then(Value::as_str);
            let matches = field("status").is_some_and(|status| states.contains(&status))
                && invocation.request_id.is_some()
                && field("request_id") == invocation.request_id.as_deref()
                && field("name") == Some(target.name.as_str())
                && field("run_id") == Some(target.run_id.as_str())
                && field("conversation_id") == Some(target.conversation_id.as_str())
                && expected_created.is_none_or(|created| {
                    receipt.get("created").and_then(Value::as_i64) == Some(created)
                });
            if matches {
                done(receipt, None)
            } else {
                failed("Invalid acknowledgement. Check Terminal before retrying.")
            }
        }
    }
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A native startup request that may come before a conversation exists:
/// folder trust, Claude's permission mode, Codex's hooks review and update.
fn startup_request(question: &Value) -> bool {
    let (source, transport, id, hash) = (
        field(question, "source"),
        field(question, "answer_transport"),
        field(question, "question_id"),
        field(question, "question_hash"),
    );
    if hash.len() != 64 {
        return false;
    }
    let trust = ["claude", "codex", "kimi"].into_iter().any(|provider| {
        source == format!("{provider}_folder_trust")
            && transport == format!("{provider}_tui")
            && id == format!("{provider}-trust:{hash}")
    });
    trust
        || (source == "claude_permission_mode"
            && transport == "claude_tui"
            && id == format!("claude-permissions:{hash}"))
        || (source == "codex_hooks_trust"
            && transport == "codex_tui"
            && id == format!("codex-hooks-trust:{hash}"))
        || (source == "codex_update"
            && transport == "codex_tui"
            && id == format!("codex-update:{hash}"))
}

/// A Codex optional question answered asynchronously.
fn async_question(question: &Value) -> bool {
    field(question, "source") == "codex_async"
        && question.get("optional").and_then(Value::as_bool) == Some(true)
}

/// Every answer is a Skip of a Codex optional question that allows it.
fn skipping(question: &Value, answers: &[questions::AnswerItem]) -> bool {
    async_question(question)
        && question.get("can_skip").and_then(Value::as_bool) == Some(true)
        && questions::all_skips(answers)
}

/// A request identity as `QUuid` reads one: 8-4-4-4-12 hexadecimal digits.
fn uuid_shaped(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    parts.len() == 5
        && parts.iter().zip([8, 4, 4, 4, 12]).all(|(part, length)| {
            part.len() == length && part.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// `requestAnswerQuestion`'s refusals and standard input.
fn answer_input(
    name: &str,
    question: &Value,
    answers: &[questions::AnswerItem],
    request_id: &str,
) -> Result<Vec<u8>, String> {
    const NOT_READY: &str =
        "This request is not ready to answer. Wait for an updated request or open Terminal.";
    let can_answer = question.get("can_answer").and_then(Value::as_bool) == Some(true);
    if name.is_empty()
        || field(question, "question_id").is_empty()
        || field(question, "question_hash").is_empty()
        || field(question, "run_id").is_empty()
        || (field(question, "conversation_id").is_empty() && !startup_request(question))
        || answers.is_empty()
        || (!can_answer && !skipping(question, answers))
    {
        return Err(NOT_READY.to_owned());
    }
    if !uuid_shaped(request_id) {
        return Err("The approval journal request identity is invalid.".to_owned());
    }
    let input = json!({
        "request_id": request_id,
        "question_id": field(question, "question_id"),
        "expected_question_hash": field(question, "question_hash"),
        "expected_run_id": field(question, "run_id"),
        "expected_conversation_id": field(question, "conversation_id"),
        "answers": answers,
    });
    let bytes = serde_json::to_vec(&input).map_err(|error| error.to_string())?;
    if bytes.len() > MAXIMUM_ANSWER_BYTES {
        return Err("The answers exceed 64 KiB.".to_owned());
    }
    Ok(bytes)
}

/// `requestAnswerQuestion`'s outcome. Only a failed start, or a plain exit 1
/// without "delivery uncertain", is a definite failure; anything else may
/// have answered. Success needs the receipt to echo the request, session,
/// question, hash, run and conversation.
fn classify_answer(
    target: &Target,
    invocation: &Invocation,
    question: &Value,
    answers: &[questions::AnswerItem],
    exit: Exit,
) -> Outcome {
    const TIMED_OUT: &str = "Answer delivery timed out. Check Terminal before retrying.";
    let detail = |stderr: &str, code: i32| {
        let text: String = stderr.trim().chars().take(FAILURE_STDERR_CHARS).collect();
        if text.is_empty() {
            format!("Answer transfer failed (exit {code}).")
        } else {
            text
        }
    };
    match exit {
        Exit::StartFailed(error) => failed(format!("Cannot start hgs: {error}")),
        // Qt's runner keeps waiting for its timeout after a broken transfer.
        Exit::TimedOut | Exit::WriteFailed => uncertain(TIMED_OUT),
        Exit::Abnormal { stderr } => uncertain(detail(&stderr, -1)),
        Exit::Code { code, stderr, .. } if code != 0 => {
            if code == 1 && !stderr.to_lowercase().contains("delivery uncertain") {
                failed(detail(&stderr, code))
            } else {
                uncertain(detail(&stderr, code))
            }
        }
        Exit::Code { stdout, .. } => {
            if stdout.len() > MAXIMUM_ANSWER_BYTES {
                return uncertain(
                    "The answer acknowledgement is too large. Check Terminal before retrying.",
                );
            }
            let receipt: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            let status = field(&receipt, "status");
            let submitted = async_question(question)
                && !skipping(question, answers)
                && status == "submitted"
                && receipt.get("skipped").and_then(Value::as_bool) != Some(true);
            let echoed = (status == "answered" || submitted)
                && invocation.request_id.is_some()
                && receipt.get("request_id").and_then(Value::as_str)
                    == invocation.request_id.as_deref()
                && field(&receipt, "name") == target.name
                && field(&receipt, "question_id") == field(question, "question_id")
                && field(&receipt, "question_hash") == field(question, "question_hash")
                && field(&receipt, "run_id") == field(question, "run_id")
                && receipt
                    .get("conversation_id")
                    .and_then(Value::as_str)
                    .is_some_and(|conversation| conversation == field(question, "conversation_id"));
            if echoed {
                done(receipt, None)
            } else {
                uncertain("The answer acknowledgement is invalid. Check Terminal before retrying.")
            }
        }
    }
}

/// OpenCode's message ID as `hgs` checks it: `msg_` and at most 128 ASCII
/// letters, digits, `_` and `-`.
fn native_message_id(id: &str) -> bool {
    id.strip_prefix("msg_").is_some_and(|rest| {
        (1..=124).contains(&rest.len())
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    })
}

/// `requestQueueSendNow`'s outcome. A start that failed sent nothing; a
/// timeout, a broken transfer, an abnormal end or "delivery uncertain" may
/// have sent the queue. Success needs the receipt to echo the request,
/// session, run, conversation and queue.
fn classify_send_now(
    target: &Target,
    invocation: &Invocation,
    queue_id: &str,
    exit: Exit,
) -> Outcome {
    const UNCONFIRMED: &str =
        "Could not confirm sending the queue. Check Terminal before retrying.";
    match exit {
        Exit::StartFailed(error) => failed(error),
        Exit::TimedOut => uncertain("Queue action timed out. Check Terminal before retrying."),
        Exit::WriteFailed => uncertain("Queue action failed. Check Terminal before retrying."),
        Exit::Abnormal { stderr } => {
            uncertain(trimmed_stderr(&stderr).unwrap_or_else(|| UNCONFIRMED.to_owned()))
        }
        Exit::Code { code, stderr, .. } if code != 0 => {
            let detail = trimmed_stderr(&stderr).unwrap_or_else(|| UNCONFIRMED.to_owned());
            if stderr.to_lowercase().contains("delivery uncertain") {
                uncertain(detail)
            } else {
                failed(detail)
            }
        }
        Exit::Code { stdout, .. } => {
            let receipt: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            let field = |key: &str| receipt.get(key).and_then(Value::as_str);
            let echoed = field("status") == Some("submitted")
                && invocation.request_id.is_some()
                && field("request_id") == invocation.request_id.as_deref()
                && field("name") == Some(target.name.as_str())
                && field("run_id") == Some(target.run_id.as_str())
                && field("conversation_id") == Some(target.conversation_id.as_str())
                && field("queue_id") == Some(queue_id);
            if echoed {
                done(receipt, None)
            } else {
                uncertain("Invalid queue acknowledgement. Check Terminal before retrying.")
            }
        }
    }
}

/// Qt's recovery edit returns the updated job. Its identity and resulting
/// state must match; an unknown outcome is never replayed.
fn classify_recovery(
    target: &Target,
    _invocation: &Invocation,
    job_id: &str,
    action: RecoveryControl,
    exit: Exit,
) -> Outcome {
    match exit {
        Exit::StartFailed(error) => failed(error),
        Exit::WriteFailed => {
            uncertain("Recovery transfer failed. Reload its state before trying again.")
        }
        Exit::TimedOut => {
            uncertain("Recovery request timed out. Reload its state before trying again.")
        }
        Exit::Abnormal { stderr } => uncertain(trimmed_stderr(&stderr).unwrap_or_else(|| {
            "Recovery ended abnormally. Reload its state before trying again.".into()
        })),
        Exit::Code { code, stderr, .. } if code != 0 => failed(
            trimmed_stderr(&stderr)
                .unwrap_or_else(|| "Could not update recovery. Reload its state.".into()),
        ),
        Exit::Code { stdout, .. } => {
            let job: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            let identity = job["id"] == job_id && job["name"] == target.name;
            let settled = match action {
                RecoveryControl::Now => {
                    job["state"] == "waiting" && job["due_at"].as_f64().is_some_and(f64::is_finite)
                }
                RecoveryControl::Cancel => job["state"] == "cancelled",
            };
            if identity && settled {
                done(job, None)
            } else {
                uncertain("Invalid recovery response. Reload its state before trying again.")
            }
        }
    }
}

/// The scoped `runSessionAction` for `remove_queued`. A durable receipt that
/// echoes the request reports failed, uncertain or completed; nothing is
/// retried, and what may have removed the message is uncertain.
fn classify_remove_queued(target: &Target, invocation: &Invocation, exit: Exit) -> Outcome {
    const INCOMPLETE: &str = "Could not complete the request. Check the session before retrying.";
    match exit {
        Exit::StartFailed(error) => failed(error),
        Exit::WriteFailed => uncertain("Transfer failed. Check the session before retrying."),
        Exit::TimedOut => uncertain("Request timed out. Check the session before retrying."),
        Exit::Abnormal { stderr } => {
            uncertain(trimmed_stderr(&stderr).unwrap_or_else(|| INCOMPLETE.to_owned()))
        }
        Exit::Code { code, stderr, .. } if code != 0 => {
            failed(trimmed_stderr(&stderr).unwrap_or_else(|| INCOMPLETE.to_owned()))
        }
        Exit::Code { stdout, .. } => {
            let receipt: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            let field = |key: &str| receipt.get(key).and_then(Value::as_str);
            let identity = invocation.request_id.is_some()
                && field("request_id") == invocation.request_id.as_deref()
                && field("name") == Some(target.name.as_str())
                && field("run_id") == Some(target.run_id.as_str())
                && field("conversation_id") == Some(target.conversation_id.as_str());
            let reason: String = field("error")
                .unwrap_or("")
                .trim()
                .chars()
                .take(STDERR_CHARS)
                .collect();
            match field("status") {
                Some("failed") if identity => failed(if reason.is_empty() {
                    "The queued message was not removed.".to_owned()
                } else {
                    reason
                }),
                Some("uncertain") if identity => uncertain(if reason.is_empty() {
                    "The result was not confirmed. Refresh before trying again.".to_owned()
                } else {
                    format!("The result was not confirmed ({reason}). Refresh before trying again.")
                }),
                Some("completed") if identity => done(receipt, None),
                _ => uncertain("Invalid acknowledgement. Check the session before retrying."),
            }
        }
    }
}

/// `requestSendMessage`'s outcome. Only a timeout before the start, a failed
/// start or a plain exit 1 without "delivery uncertain" is a definite
/// failure; anything else may have reached the agent. Success needs the
/// receipt to echo the request, agent, session, run and conversation, and a
/// first message (no conversation yet) to say so.
fn classify_send(
    target: &Target,
    invocation: &Invocation,
    agent_id: Option<&str>,
    exit: Exit,
) -> Outcome {
    let detail = |stderr: &str, code: i32| {
        let text: String = stderr.trim().chars().take(FAILURE_STDERR_CHARS).collect();
        if text.is_empty() {
            format!("Message transfer failed (exit {code}).")
        } else {
            text
        }
    };
    match exit {
        Exit::TimedOut => uncertain(
            "Delivery timed out. The message may have reached the agent; inspect Terminal before retrying.",
        ),
        Exit::WriteFailed => uncertain("Message transfer failed. Inspect Terminal before retrying."),
        Exit::StartFailed(error) => failed(format!("Cannot start hgs: {error}")),
        Exit::Abnormal { stderr } => uncertain(detail(&stderr, -1)),
        Exit::Code { code, stderr, .. } if code != 0 => {
            let text = detail(&stderr, code);
            if code != 1 || stderr.to_lowercase().contains("delivery uncertain") {
                uncertain(text)
            } else {
                failed(text)
            }
        }
        Exit::Code { stdout, .. } => {
            let receipt: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            let field = |key: &str| receipt.get(key).and_then(Value::as_str);
            let valid = receipt.is_object()
                && field("status") == Some("submitted")
                && invocation.request_id.is_some()
                && field("request_id") == invocation.request_id.as_deref()
                && field("agent_id").unwrap_or("") == agent_id.unwrap_or("")
                && field("name") == Some(target.name.as_str())
                && field("run_id") == Some(target.run_id.as_str())
                && field("conversation_id") == Some(target.conversation_id.as_str())
                && (!target.conversation_id.is_empty()
                    || receipt.get("first_message") == Some(&Value::Bool(true)));
            if valid {
                done(receipt, None)
            } else {
                uncertain("Delivery acknowledgement is invalid. Inspect Terminal before retrying.")
            }
        }
    }
}

/// A stop receipt names the same process, run and conversation, and says
/// `requested` or `already-finished`.
fn classify_stop(target: &Target, process_id: &str, exit: Exit) -> Outcome {
    match exit {
        Exit::Code {
            code: 0, stdout, ..
        } => {
            let receipt: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            let field = |key: &str| receipt.get(key).and_then(Value::as_str);
            let confirmed = field("id") == Some(process_id)
                && field("run_id") == Some(target.run_id.as_str())
                && field("conversation_id") == Some(target.conversation_id.as_str())
                && matches!(field("status"), Some("requested" | "already-finished"));
            if confirmed {
                done(receipt, None)
            } else {
                failed("Stop was not confirmed")
            }
        }
        exit => failed(failure_detail(exit)),
    }
}

/// Whether the inspected conversation can be cleared now, or after
/// dismissing a chooser the agent shows (`clearableContext`).
pub fn clearable(details: &Value) -> bool {
    let text = |key: &str| details[key].as_str().unwrap_or("");
    let flag = |key: &str| details[key].as_bool().unwrap_or(false);
    !text("run_id").is_empty()
        && !text("conversation_id").is_empty()
        && (flag("clear_context_supported") || flag("clear_context_dismiss"))
}

/// The scoped `runSessionAction` (`HgsClient.cpp:2169-2245`) for the OpenCode
/// forget: only a `completed` receipt naming this request, session, run and
/// conversation is done; a durable `failed` or `uncertain` one is never retried.
pub(crate) fn classify_scoped_lifecycle(
    target: &Target,
    invocation: &Invocation,
    exit: Exit,
    not_completed: &str,
) -> Outcome {
    const FALLBACK: &str = "Could not complete the request. Check the session before retrying.";
    match exit {
        Exit::TimedOut => failed("Request timed out. Check the session before retrying."),
        Exit::WriteFailed => failed("Transfer failed. Check the session before retrying."),
        Exit::StartFailed(error) => failed(error),
        Exit::Abnormal { stderr } => {
            failed(trimmed_stderr(&stderr).unwrap_or_else(|| FALLBACK.to_owned()))
        }
        Exit::Code { code, stderr, .. } if code != 0 => {
            failed(trimmed_stderr(&stderr).unwrap_or_else(|| FALLBACK.to_owned()))
        }
        Exit::Code { stdout, .. } => {
            let receipt: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            let field = |key: &str| receipt.get(key).and_then(Value::as_str);
            let identity = invocation.request_id.is_some()
                && field("request_id") == invocation.request_id.as_deref()
                && field("name") == Some(target.name.as_str())
                && field("run_id") == Some(target.run_id.as_str())
                && field("conversation_id") == Some(target.conversation_id.as_str());
            let reason = field("error")
                .map(|text| text.trim().chars().take(STDERR_CHARS).collect::<String>())
                .filter(|text| !text.is_empty());
            match (identity, field("status")) {
                (true, Some("failed")) => {
                    failed(reason.unwrap_or_else(|| not_completed.to_owned()))
                }
                (true, Some("uncertain")) => failed(match reason {
                    Some(reason) => format!(
                        "The result was not confirmed ({reason}). Refresh before trying again."
                    ),
                    None => "The result was not confirmed. Refresh before trying again.".to_owned(),
                }),
                (true, Some("completed")) => done(receipt, None),
                _ => failed("Invalid acknowledgement. Check the session before retrying."),
            }
        }
    }
}

/// `requestRead`: applied marks, or a session that is gone (nothing to mark).
fn classify_read(target: &Target, exit: Exit) -> Outcome {
    const FALLBACK: &str = "Could not save read state on that machine.";
    match exit {
        Exit::TimedOut => failed("Saving read state timed out."),
        Exit::Code {
            code: 0, stdout, ..
        } => {
            let reply: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            let applied = |key: &str| {
                reply
                    .get("applied")
                    .and_then(|applied| applied.get(key))
                    .is_some_and(Value::is_boolean)
            };
            let read = reply.get("read");
            let read_ok = match read {
                None | Some(Value::Null) => true,
                Some(read) => {
                    read.get("conversation_id").and_then(Value::as_str)
                        == Some(target.conversation_id.as_str())
                }
            };
            if reply.get("ok") == Some(&Value::Bool(true))
                && applied("reply_id")
                && applied("attention_signature")
                && read_ok
            {
                done(reply, None)
            } else {
                failed(FALLBACK)
            }
        }
        Exit::Code {
            code: 1,
            stdout,
            stderr,
        } => {
            let reply: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
            if reply == json!({"ok": false, "error": "not_found"}) {
                done(reply, None)
            } else {
                failed(trimmed_stderr(&stderr).unwrap_or_else(|| FALLBACK.to_owned()))
            }
        }
        Exit::Code { stderr, .. } | Exit::Abnormal { stderr } => {
            failed(trimmed_stderr(&stderr).unwrap_or_else(|| FALLBACK.to_owned()))
        }
        Exit::StartFailed(error) => failed(error),
        Exit::WriteFailed => failed(FALLBACK),
    }
}

/// `runWrite`: exit 0 is done; Archive and Terminate may report their worktree.
fn classify_write(exit: Exit, may_clean_up: bool) -> Outcome {
    match exit {
        Exit::Code {
            code: 0, stderr, ..
        } => done(
            Value::Null,
            if may_clean_up {
                cleanup_line(&stderr)
            } else {
                None
            },
        ),
        Exit::TimedOut => failed("timed out"),
        Exit::StartFailed(error) => failed(error),
        Exit::WriteFailed => failed("could not send the request"),
        Exit::Abnormal { stderr } => {
            failed(trimmed_stderr(&stderr).unwrap_or_else(|| "hgs ended abnormally".to_owned()))
        }
        Exit::Code { code, stderr, .. } => failed(
            trimmed_stderr(&stderr).unwrap_or_else(|| format!("hgs exited with code {code}")),
        ),
    }
}

/// The single worktree report line; anything else (none, several, a partial
/// object) is not trusted to name a worktree.
pub(crate) fn cleanup_line(stderr: &str) -> Option<WorktreeCleanup> {
    let mut lines = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(CLEANUP_PREFIX));
    let line = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    let cleanup: WorktreeCleanup = serde_json::from_str(line).ok()?;
    (!cleanup.path.is_empty() && !cleanup.common_dir.is_empty()).then_some(cleanup)
}

/// `NativeSessionCapabilities::action` without details: reported allowed
/// actions win; without them every operation is allowed except that an
/// "opencode" session only renames and forgets.
pub fn allowed(row: &ViewRow, operation: &str) -> bool {
    match &row.allowed_actions {
        Some(list) => list.iter().any(|action| action == operation),
        None => row.agent != "opencode" || matches!(operation, "rename" | "forget"),
    }
}

/// `NativeSessionCapabilities::action` with the inspection: its
/// `allowed_actions` list wins over the row's.
pub fn allowed_with(row: &ViewRow, details: &Value, operation: &str) -> bool {
    match details.get("allowed_actions").and_then(Value::as_array) {
        Some(list) => list.iter().any(|action| action.as_str() == Some(operation)),
        None => allowed(row, operation),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(computer: &str) -> Target {
        Target {
            computer: computer.into(),
            name: "claude/sample-project/review".into(),
            run_id: "run-1".into(),
            conversation_id: "conv-1".into(),
        }
    }
    fn exit0(stdout: &[u8], stderr: &str) -> Exit {
        Exit::Code {
            code: 0,
            stdout: stdout.to_vec(),
            stderr: stderr.into(),
        }
    }

    fn preferences(muted: bool, revision: i64) -> Value {
        json!({"conversation_id": "conv-1", "muted": muted, "revision": revision, "suppressed": {
            "reply_id": null, "attention_signature": null, "optional_question_ids": [],
            "main_attention": null, "child_attention": {}}})
    }
    fn reply(applied: bool, muted: bool, revision: i64) -> Value {
        json!({"ok": true, "applied": applied, "run_id": "run-1", "conversation_id": "conv-1",
            "preferences": preferences(muted, revision)})
    }
    fn reply_exit(value: &Value) -> Exit {
        exit0(value.to_string().as_bytes(), "")
    }
    const UNCONFIRMED: &str =
        "The shared notification change was not confirmed. Refresh the session before trying again.";

    #[test]
    fn settings_pin_native_identity_pending_change_and_timeout() {
        let action = Action::Settings {
            model: "model-a".into(),
            effort: "high".into(),
            expected_pending_id: Some("pending-1".into()),
            without_terminal: false,
        };
        for (computer, seconds) in [("", 20), ("mac", 35)] {
            let t = target(computer);
            let call = invocation(&t, &action, "request-1").unwrap();
            assert_eq!(call.timeout.as_secs(), seconds);
            let input: Value = serde_json::from_slice(call.stdin.as_ref().unwrap()).unwrap();
            assert_eq!(
                input,
                json!({"request_id":"request-1","expected_run_id":"run-1","expected_conversation_id":"conv-1","model":"model-a","effort":"high","expected_pending_id":"pending-1"})
            );
            assert!(call
                .args
                .ends_with(&["settings".into(), t.name, "--json".into()]));
        }
    }

    #[test]
    fn settings_receipts_must_echo_every_requested_field() {
        let t = target("");
        let action = Action::Settings {
            model: "model-a".into(),
            effort: "high".into(),
            expected_pending_id: None,
            without_terminal: false,
        };
        let call = invocation(&t, &action, "request-1").unwrap();
        let receipt = json!({"status":"scheduled","request_id":"request-1","name":t.name,"run_id":t.run_id,"conversation_id":t.conversation_id,"model":"model-a","effort":"high","pending_settings_id":"pending-1"});
        assert!(matches!(
            classify(
                &t,
                &action,
                &call,
                exit0(&serde_json::to_vec(&receipt).unwrap(), "")
            ),
            Outcome::Done { .. }
        ));
        for key in [
            "status",
            "request_id",
            "name",
            "run_id",
            "conversation_id",
            "model",
            "effort",
        ] {
            let mut wrong = receipt.clone();
            wrong[key] = json!("different");
            assert!(
                matches!(
                    classify(
                        &t,
                        &action,
                        &call,
                        exit0(&serde_json::to_vec(&wrong).unwrap(), "")
                    ),
                    Outcome::Failed { .. }
                ),
                "{key}"
            );
        }
        let native = Action::Settings {
            model: "provider/model".into(),
            effort: String::new(),
            expected_pending_id: None,
            without_terminal: true,
        };
        let call = invocation(&t, &native, "request-2").unwrap();
        let input: Value = serde_json::from_slice(call.stdin.as_ref().unwrap()).unwrap();
        assert!(
            input.get("effort").is_none(),
            "OpenCode default follows Qt's absent effort field"
        );
        assert_eq!(
            classify(&t, &native, &call, Exit::TimedOut),
            failed("Settings change timed out. Check the session before retrying.")
        );
    }

    #[test]
    fn the_help_probe_is_local_short_and_needs_the_exact_marker() {
        let probe = help_invocation();
        assert_eq!(probe.args, ["--help"]);
        assert_eq!(probe.stdin, None);
        assert_eq!(probe.timeout, std::time::Duration::from_secs(5));
        assert!(has_notifications_marker(
            b"usage\nsession-notifications-v1\nmore"
        ));
        assert!(has_notifications_marker(
            b"usage\r\nsession-notifications-v1\r\n"
        ));
        assert!(has_notifications_marker(b"session-notifications-v1"));
        for text in [
            &b"session-notifications-v10\n"[..],
            b" session-notifications-v1\n",
            b"session-notifications-v1 \n",
            b"x session-notifications-v1\n",
            b"session-notifications-v1\r\r\n",
            b"",
        ] {
            assert!(!has_notifications_marker(text), "{text:?}");
        }
        assert!(help_confirms(&exit0(b"session-notifications-v1\n", "")));
        assert!(!help_confirms(&Exit::Code {
            code: 1,
            stdout: b"session-notifications-v1\n".to_vec(),
            stderr: String::new()
        }));
        assert!(!help_confirms(&Exit::TimedOut));
    }

    #[test]
    fn the_notification_call_matches_the_qt_client() {
        let local = notifications_invocation(&target(""), true, Some(4)).unwrap();
        assert_eq!(
            local.args,
            ["notifications", "claude/sample-project/review", "--json"]
        );
        assert_eq!(local.timeout, std::time::Duration::from_secs(15));
        assert_eq!(
            String::from_utf8(local.stdin.unwrap()).unwrap(),
            r#"{"conversation_id":"conv-1","expected_revision":4,"muted":true,"run_id":"run-1"}"#
        );
        let peer = notifications_invocation(&target("mac"), false, Some(0)).unwrap();
        assert_eq!(
            peer.args,
            [
                "@mac",
                "notifications",
                "claude/sample-project/review",
                "--json"
            ]
        );
        assert_eq!(peer.timeout, std::time::Duration::from_secs(35));
    }

    #[test]
    fn a_bad_identity_fails_before_any_call() {
        let text = "The session notification identity is unavailable. Refresh the session.";
        let long = |n: usize| "x".repeat(n);
        let mut cases = Vec::new();
        for (run, conv) in [
            ("", "c"),
            ("r", ""),
            (long(129).leak() as &str, "c"),
            ("r", long(257).leak() as &str),
            ("r\n", "c"),
            ("r", "c\u{7f}"),
        ] {
            let mut t = target("");
            t.run_id = run.into();
            t.conversation_id = conv.into();
            cases.push(t);
        }
        let mut nameless = target("");
        nameless.name.clear();
        cases.push(nameless);
        for t in cases {
            assert_eq!(
                notifications_invocation(&t, true, Some(1)).unwrap_err(),
                text
            );
        }
        for revision in [None, Some(-1)] {
            assert_eq!(
                notifications_invocation(&target(""), true, revision).unwrap_err(),
                text
            );
        }
        let mut edge = target("");
        edge.run_id = long(128);
        edge.conversation_id = long(256);
        assert!(notifications_invocation(&edge, true, Some(1)).is_ok());
    }

    #[test]
    fn a_confirmed_change_returns_the_preferences() {
        let t = target("");
        assert_eq!(
            classify_notifications(&t, true, 4, reply_exit(&reply(true, true, 5))).unwrap(),
            preferences(true, 5)
        );
        assert_eq!(
            classify_notifications(&t, false, 4, reply_exit(&reply(false, false, 4))).unwrap(),
            preferences(false, 4)
        );
    }

    #[test]
    fn every_confirmation_field_is_required() {
        let t = target("");
        let check = |value: Value, muted: bool, expected: i64| {
            classify_notifications(&t, muted, expected, reply_exit(&value)).unwrap_err()
        };
        let mutate = |f: &dyn Fn(&mut Value)| {
            let mut value = reply(true, true, 5);
            f(&mut value);
            value
        };
        let cases: Vec<(&str, Value)> = vec![
            ("ok false", mutate(&|v| v["ok"] = json!(false))),
            ("ok text", mutate(&|v| v["ok"] = json!("true"))),
            ("applied text", mutate(&|v| v["applied"] = json!("yes"))),
            (
                "applied missing",
                mutate(&|v| {
                    v.as_object_mut().unwrap().remove("applied");
                }),
            ),
            ("extra key", mutate(&|v| v["extra"] = json!(1))),
            ("run id", mutate(&|v| v["run_id"] = json!("run-2"))),
            (
                "conversation",
                mutate(&|v| v["conversation_id"] = json!("conv-2")),
            ),
            (
                "preferences conversation",
                mutate(&|v| v["preferences"]["conversation_id"] = json!("conv-2")),
            ),
            (
                "preferences invalid",
                mutate(&|v| {
                    v["preferences"]
                        .as_object_mut()
                        .unwrap()
                        .remove("suppressed");
                }),
            ),
            (
                "muted other",
                mutate(&|v| v["preferences"]["muted"] = json!(false)),
            ),
            (
                "muted text",
                mutate(&|v| v["preferences"]["muted"] = json!("true")),
            ),
            (
                "revision skipped",
                mutate(&|v| v["preferences"]["revision"] = json!(6)),
            ),
            (
                "revision same when applied",
                mutate(&|v| v["preferences"]["revision"] = json!(4)),
            ),
            (
                "fraction",
                mutate(&|v| v["preferences"]["revision"] = json!(5.5)),
            ),
            ("not an object", json!([1])),
        ];
        for (name, value) in cases {
            assert_eq!(check(value, true, 4), UNCONFIRMED, "{name}");
        }
        // Not applied: the revision must stay.
        assert_eq!(check(reply(false, true, 5), true, 4), UNCONFIRMED);
        // The muted value must be the requested one.
        assert_eq!(check(reply(true, true, 5), false, 4), UNCONFIRMED);
        // Text that is not JSON.
        assert_eq!(
            classify_notifications(&t, true, 4, exit0(b"nope", "")).unwrap_err(),
            UNCONFIRMED
        );
        assert_eq!(
            classify_notifications(&t, true, i64::MAX, reply_exit(&reply(true, true, i64::MAX)))
                .unwrap_err(),
            UNCONFIRMED
        );
    }

    #[test]
    fn a_failed_call_reports_the_trimmed_stderr_or_the_fallback() {
        let t = target("");
        let nonzero = |stderr: &str| Exit::Code {
            code: 2,
            stdout: reply(true, true, 5).to_string().into_bytes(),
            stderr: stderr.into(),
        };
        assert_eq!(
            classify_notifications(&t, true, 4, nonzero("  stale revision \n")).unwrap_err(),
            "stale revision"
        );
        assert_eq!(
            classify_notifications(&t, true, 4, nonzero(" ")).unwrap_err(),
            UNCONFIRMED
        );
        assert_eq!(
            classify_notifications(&t, true, 4, nonzero(&"e".repeat(2500)))
                .unwrap_err()
                .chars()
                .count(),
            2000
        );
        for exit in [
            Exit::TimedOut,
            Exit::StartFailed("x".into()),
            Exit::WriteFailed,
        ] {
            assert_eq!(
                classify_notifications(&t, true, 4, exit).unwrap_err(),
                UNCONFIRMED
            );
        }
        assert_eq!(
            classify_notifications(
                &t,
                true,
                4,
                Exit::Abnormal {
                    stderr: "killed".into()
                }
            )
            .unwrap_err(),
            "killed"
        );
    }

    fn send(text: &str, attachments: Vec<Attachment>) -> Action {
        Action::Send {
            text: text.into(),
            attachments,
            agent_id: None,
            expected_compaction_id: None,
        }
    }

    fn file(name: &str, bytes: Vec<u8>) -> Attachment {
        Attachment {
            name: name.into(),
            mime: "text/plain".into(),
            bytes: bytes.into(),
            reference: String::new(),
        }
    }

    fn exit(code: i32, stdout: &[u8], stderr: &str) -> Exit {
        Exit::Code {
            code,
            stdout: stdout.to_vec(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn a_send_cannot_come_from_webview_json() {
        let refused = serde_json::from_value::<Action>(json!({
            "kind": "send", "text": "hi", "attachments": [],
            "agent_id": null, "expected_compaction_id": null
        }));
        assert!(refused.is_err());
        let stage: Action = serde_json::from_value(json!({
            "kind": "stage_files", "paths": ["/home/example/a.txt"]
        }))
        .unwrap();
        assert_eq!(
            stage,
            Action::StageFiles {
                paths: vec!["/home/example/a.txt".into()],
                expected_created: None
            }
        );
        assert_eq!(
            serde_json::to_value(uncertain("x")).unwrap(),
            json!({"outcome": "uncertain", "detail": "x"})
        );
    }

    #[test]
    fn a_send_checks_like_qt_before_any_process() {
        let t = target("");
        let refusal =
            |target: &Target, action: &Action| invocation(target, action, "u").unwrap_err();
        let mut no_run = t.clone();
        no_run.run_id.clear();
        assert_eq!(
            refusal(&no_run, &send("hi", vec![])),
            "Refresh the session before sending a message."
        );
        let limits = "Use at most 64 KiB of text and 8 attachments.";
        assert_eq!(
            refusal(&t, &send(&"x".repeat(MAX_MESSAGE_BYTES + 1), vec![])),
            limits
        );
        assert!(invocation(&t, &send(&"x".repeat(MAX_MESSAGE_BYTES), vec![]), "u").is_ok());
        let nine = (0..9).map(|_| file("a", vec![1])).collect();
        assert_eq!(refusal(&t, &send("hi", nine)), limits);
        assert_eq!(
            refusal(&t, &send("hi", vec![file("e", vec![])])),
            "Each attachment must be nonempty and at most 10 MiB."
        );
        let ten = attachments::MAX_ATTACHMENT_BYTES;
        assert_eq!(
            refusal(
                &t,
                &send(
                    "hi",
                    vec![
                        file("a", vec![0; ten]),
                        file("b", vec![0; ten]),
                        file("c", vec![0])
                    ]
                )
            ),
            "Attachments exceed 20 MiB in total."
        );
        assert_eq!(
            refusal(&t, &send(" \n\t", vec![])),
            "Write a message or attach a file first."
        );
    }

    #[test]
    fn a_send_puts_the_message_only_on_stdin() {
        let action = Action::Send {
            text: "secret words".into(),
            attachments: vec![Attachment {
                reference: "[File #1]".into(),
                ..file("a.txt", b"hi".to_vec())
            }],
            agent_id: Some("agent-7".into()),
            expected_compaction_id: Some("compaction-1".into()),
        };
        let inv = invocation(&target("mac"), &action, "req-1").unwrap();
        assert_eq!(
            inv.args,
            ["@mac", "send", "claude/sample-project/review", "--json"]
        );
        assert_eq!(inv.timeout, Duration::from_secs(45));
        assert_eq!(inv.request_id.as_deref(), Some("req-1"));
        let input: Value = serde_json::from_slice(inv.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({
                "request_id": "req-1", "text": "secret words",
                "attachments": [{"name": "a.txt", "mime": "text/plain", "data_base64": "aGk=", "reference": "[File #1]"}],
                "expected_run_id": "run-1", "expected_conversation_id": "conv-1",
                "agent_id": "agent-7", "expected_compaction_id": "compaction-1"
            })
        );
        let local = invocation(&target(""), &send("hi", vec![]), "req-2").unwrap();
        assert_eq!(local.timeout, Duration::from_secs(15));
        let input: Value = serde_json::from_slice(local.stdin.as_ref().unwrap()).unwrap();
        assert!(input.get("agent_id").is_none() && input.get("expected_compaction_id").is_none());
    }

    #[test]
    fn send_outcomes_separate_certain_from_uncertain_failures() {
        let t = target("");
        let action = send("hi", vec![]);
        let inv = invocation(&t, &action, "req-1").unwrap();
        let outcome = |exit| classify(&t, &action, &inv, exit);
        assert_eq!(
            outcome(Exit::TimedOut),
            uncertain("Delivery timed out. The message may have reached the agent; inspect Terminal before retrying.")
        );
        assert_eq!(
            outcome(Exit::WriteFailed),
            uncertain("Message transfer failed. Inspect Terminal before retrying.")
        );
        assert_eq!(
            outcome(Exit::StartFailed("No such file or directory".into())),
            failed("Cannot start hgs: No such file or directory")
        );
        assert_eq!(
            outcome(Exit::Abnormal {
                stderr: String::new()
            }),
            uncertain("Message transfer failed (exit -1).")
        );
        assert_eq!(
            outcome(exit(1, b"", " no such session \n")),
            failed("no such session")
        );
        assert_eq!(
            outcome(exit(1, b"", "Delivery Uncertain: pane closed")),
            uncertain("Delivery Uncertain: pane closed")
        );
        assert_eq!(
            outcome(exit(2, b"", "")),
            uncertain("Message transfer failed (exit 2).")
        );
        let receipt = json!({"status": "submitted", "request_id": "req-1", "name": "claude/sample-project/review",
                             "run_id": "run-1", "conversation_id": "conv-1"});
        assert!(matches!(
            outcome(exit(0, receipt.to_string().as_bytes(), "")),
            Outcome::Done { .. }
        ));
        let invalid =
            uncertain("Delivery acknowledgement is invalid. Inspect Terminal before retrying.");
        for (key, value) in [
            ("status", json!("queued")),
            ("request_id", json!("req-2")),
            ("agent_id", json!("agent-7")),
            ("name", json!("claude/sample-project/other")),
            ("run_id", json!("run-2")),
            ("conversation_id", json!(null)),
        ] {
            let mut changed = receipt.clone();
            changed[key] = value;
            assert_eq!(
                outcome(exit(0, changed.to_string().as_bytes(), "")),
                invalid,
                "{key}"
            );
        }
        assert_eq!(outcome(exit(0, b"not json", "")), invalid);
        // A first message has no conversation yet and must say so.
        let mut first = t.clone();
        first.conversation_id.clear();
        let inv = invocation(&first, &action, "req-1").unwrap();
        let mut receipt = receipt.clone();
        receipt["conversation_id"] = json!("");
        let outcome = |receipt: &Value| {
            classify(
                &first,
                &action,
                &inv,
                exit(0, receipt.to_string().as_bytes(), ""),
            )
        };
        assert_eq!(outcome(&receipt), invalid);
        receipt["first_message"] = json!(true);
        assert!(matches!(outcome(&receipt), Outcome::Done { .. }));
    }

    #[test]
    fn staged_files_are_checked_like_qt() {
        let dir = std::env::temp_dir().join(format!("zerus-stage-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        std::fs::write(&a, b"hello").unwrap();
        let stage = |paths: Vec<PathBuf>| Action::StageFiles {
            paths,
            expected_created: Some(1700),
        };
        let t = target("");
        let refusal = |action: &Action| invocation(&t, action, "u").unwrap_err();
        assert_eq!(refusal(&stage(vec![])), "Drop between 1 and 8 files.");
        assert_eq!(
            refusal(&stage(vec![a.clone(); 9])),
            "Drop between 1 and 8 files."
        );
        let unreadable = "Each file must be readable, nonempty, and at most 10 MiB.";
        assert_eq!(
            refusal(&stage(vec!["a.txt".into()])),
            unreadable,
            "relative"
        );
        assert_eq!(refusal(&stage(vec![dir.clone()])), unreadable, "folder");
        assert_eq!(refusal(&stage(vec![dir.join("missing")])), unreadable);
        let empty = dir.join("empty");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(refusal(&stage(vec![empty])), unreadable);
        #[cfg(unix)]
        assert_eq!(
            refusal(&stage(vec!["/dev/null".into()])),
            unreadable,
            "device"
        );
        let big = dir.join("big");
        std::fs::write(&big, vec![0; attachments::MAX_ATTACHMENT_BYTES]).unwrap();
        assert_eq!(
            refusal(&stage(vec![big.clone(), big.clone(), a.clone()])),
            "Files exceed 20 MiB in total or changed during transfer."
        );
        let inv = invocation(&t, &stage(vec![a.clone()]), "req-1").unwrap();
        assert_eq!(
            inv.args,
            [
                "attachment",
                "claude/sample-project/review",
                "--stage",
                "--json"
            ]
        );
        let input: Value = serde_json::from_slice(inv.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({"request_id": "req-1", "expected_run_id": "run-1", "expected_conversation_id": "conv-1",
                   "attachments": [{"name": "a.txt", "mime": "text/plain", "data_base64": "aGVsbG8="}]})
        );
        // An untracked session checks its creation time.
        let mut untracked = t.clone();
        untracked.run_id.clear();
        untracked.conversation_id.clear();
        let action = stage(vec![a]);
        let inv = invocation(&untracked, &action, "req-1").unwrap();
        let input: Value = serde_json::from_slice(inv.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(input["expected_created"], json!(1700));
        let receipt = json!({"status": "staged", "request_id": "req-1", "name": "claude/sample-project/review",
                             "run_id": "", "conversation_id": "", "created": 1700});
        let outcome = |receipt: &Value| {
            classify(
                &untracked,
                &action,
                &inv,
                exit(0, receipt.to_string().as_bytes(), ""),
            )
        };
        assert!(matches!(outcome(&receipt), Outcome::Done { .. }));
        let mut other = receipt.clone();
        other["created"] = json!(1701);
        assert_eq!(
            outcome(&other),
            failed("Invalid acknowledgement. Check Terminal before retrying.")
        );
        other = receipt.clone();
        other["status"] = json!("submitted");
        assert_eq!(
            outcome(&other),
            failed("Invalid acknowledgement. Check Terminal before retrying.")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_process_stop_names_the_run_and_checks_its_receipt() {
        let action = Action::StopProcess {
            process_id: "job-1".into(),
            archive_id: Some("a-7".into()),
            generation: Some("g-2".into()),
        };
        let t = target("studio");
        let inv = invocation(&t, &action, "u").unwrap();
        assert_eq!(
            inv.args,
            [
                "@studio",
                "processes",
                "claude/sample-project/review",
                "--stop",
                "job-1",
                "--run",
                "run-1",
                "--conversation",
                "conv-1",
                "--archive",
                "a-7",
                "--generation",
                "g-2"
            ]
        );
        assert_eq!(inv.timeout, Duration::from_secs(45));
        let stopped = |status: &str, id: &str| {
            let receipt =
                json!({"id": id, "run_id": "run-1", "conversation_id": "conv-1", "status": status});
            classify(
                &t,
                &action,
                &inv,
                exit(0, receipt.to_string().as_bytes(), ""),
            )
        };
        assert!(matches!(
            stopped("requested", "job-1"),
            Outcome::Done { .. }
        ));
        assert!(matches!(
            stopped("already-finished", "job-1"),
            Outcome::Done { .. }
        ));
        assert_eq!(
            stopped("refused", "job-1"),
            failed("Stop was not confirmed")
        );
        assert_eq!(
            stopped("requested", "job-2"),
            failed("Stop was not confirmed")
        );
        assert_eq!(
            classify(&t, &action, &inv, exit(1, b"", "no such process\n")),
            failed("no such process")
        );
        assert_eq!(
            classify(&t, &action, &inv, Exit::TimedOut),
            failed("timed out")
        );
        let from_webview: Action =
            serde_json::from_value(json!({"kind": "stop_process", "process_id": "job-1"})).unwrap();
        assert_eq!(
            from_webview,
            Action::StopProcess {
                process_id: "job-1".into(),
                archive_id: None,
                generation: None
            }
        );
    }

    #[test]
    fn clear_and_compact_are_session_actions_with_receipts() {
        let t = target("mac");
        let clear = Action::ClearContext {
            dismiss_prompt: false,
        };
        let inv = invocation(&t, &clear, "req-1").unwrap();
        assert_eq!(
            inv.args,
            [
                "@mac",
                "clear-context",
                "claude/sample-project/review",
                "--json"
            ]
        );
        assert_eq!(inv.timeout, Duration::from_secs(45));
        let input: Value = serde_json::from_slice(inv.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({"request_id": "req-1", "expected_run_id": "run-1", "expected_conversation_id": "conv-1"})
        );
        let dismiss = invocation(
            &target(""),
            &Action::ClearContext {
                dismiss_prompt: true,
            },
            "req-2",
        )
        .unwrap();
        assert_eq!(dismiss.timeout, Duration::from_secs(15));
        let input: Value = serde_json::from_slice(dismiss.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(input["dismiss_prompt"], json!(true));
        let receipt = |status: &str| {
            json!({"status": status, "request_id": "req-1", "name": "claude/sample-project/review",
                   "run_id": "run-1", "conversation_id": "conv-1"})
            .to_string()
        };
        let outcome = |action: &Action, status: &str| {
            let inv = invocation(&t, action, "req-1").unwrap();
            classify(&t, action, &inv, exit(0, receipt(status).as_bytes(), ""))
        };
        assert!(matches!(outcome(&clear, "confirmed"), Outcome::Done { .. }));
        assert!(matches!(outcome(&clear, "submitted"), Outcome::Done { .. }));
        assert!(matches!(
            outcome(&Action::CompactContext, "submitted"),
            Outcome::Done { .. }
        ));
        assert_eq!(
            outcome(&Action::CompactContext, "confirmed"),
            failed("Invalid acknowledgement. Check Terminal before retrying.")
        );
        let compact = invocation(&t, &Action::CompactContext, "req-9").unwrap();
        assert_eq!(compact.args[1], "compact-context");
        assert_eq!(compact.request_id.as_deref(), Some("req-9"));
        let from_webview: Action =
            serde_json::from_value(json!({"kind": "clear_context"})).unwrap();
        assert_eq!(
            from_webview,
            Action::ClearContext {
                dismiss_prompt: false
            }
        );
        assert!(clearable(
            &json!({"run_id": "r", "conversation_id": "c", "clear_context_dismiss": true})
        ));
        assert!(!clearable(
            &json!({"run_id": "r", "conversation_id": "", "clear_context_supported": true})
        ));
        assert!(!clearable(&json!({"run_id": "r", "conversation_id": "c"})));
    }

    #[test]
    fn a_fresh_start_names_the_run_it_replaces() {
        let inv = invocation(&target("mac"), &Action::StartFresh, "u").unwrap();
        assert_eq!(
            inv.args,
            [
                "@mac",
                "resume",
                "claude/sample-project/review",
                "--fresh",
                "--expected-run-id",
                "run-1",
                "-d"
            ]
        );
        assert_eq!(inv.timeout, Duration::from_secs(30));
        let mut untracked = target("");
        untracked.run_id.clear();
        assert_eq!(
            invocation(&untracked, &Action::StartFresh, "u")
                .unwrap()
                .args,
            ["resume", "claude/sample-project/review", "--fresh", "-d"]
        );
    }

    #[test]
    fn a_fork_names_its_source_and_a_free_tag() {
        let action = Action::Fork {
            tag: "review-fork".into(),
            archive_id: Some("a-7".into()),
        };
        let inv = invocation(&target("mac"), &action, "u").unwrap();
        assert_eq!(
            inv.args,
            [
                "@mac",
                "fork",
                "claude/sample-project/review",
                "-n",
                "review-fork",
                "-d",
                "--archive",
                "a-7",
                "--expected-run-id",
                "run-1",
                "--expected-conversation-id",
                "conv-1"
            ]
        );
        assert_eq!(inv.timeout, Duration::from_secs(90));
        assert_eq!(
            invocation(&target(""), &action, "u").unwrap().timeout,
            Duration::from_secs(60)
        );
        assert_eq!(
            invocation(
                &target(""),
                &Action::Fork {
                    tag: "a/b".into(),
                    archive_id: None
                },
                "u"
            )
            .unwrap_err(),
            "Names cannot contain /, : or . or control characters."
        );
        assert!(matches!(
            classify(&target(""), &action, &inv, exit(0, b"", "")),
            Outcome::Done { .. }
        ));
        let taken = ["review-fork", "review-fork-2"];
        assert_eq!(
            fork_tag("review", |tag| taken.contains(&tag)),
            "review-fork-3"
        );
        assert_eq!(fork_tag("", |_| false), "fork");
        assert_eq!(
            fork_problem("review-fork", |tag| taken.contains(&tag)).as_deref(),
            Some("This session name is already in use.")
        );
        assert_eq!(
            fork_problem("", |_| false).as_deref(),
            Some("Enter a session name.")
        );
        assert_eq!(fork_problem("next", |_| false), None);
        let (enabled, tooltip) =
            fork_available(true, false, false, &json!({"fork_supported": true}));
        assert!(enabled);
        assert_eq!(
            tooltip,
            "Create an independent conversation with the same context and working folder"
        );
        let (enabled, tooltip) = fork_available(
            true,
            false,
            false,
            &json!({"fork_supported": false, "fork_reason": "No saved context yet."}),
        );
        assert!(!enabled);
        assert_eq!(tooltip, "No saved context yet.");
        assert!(!fork_available(false, false, false, &json!({"fork_supported": true})).0);
        assert_eq!(
            fork_texts::description("/home/example/project"),
            "Create an independent conversation from this saved context.\nWorking folder: /home/example/project"
        );
        assert_eq!(fork_texts::forking("studio"), "Forking session on studio…");
    }

    #[test]
    fn calls_match_the_qt_client() {
        let t = target("");
        assert_eq!(
            invocation(&t, &Action::Pause, "u").unwrap().args,
            ["pause", "claude/sample-project/review"]
        );
        assert_eq!(
            invocation(&t, &Action::Resume, "u").unwrap().args,
            ["resume", "claude/sample-project/review", "-d"]
        );
        assert_eq!(
            invocation(&t, &Action::Archive, "u").unwrap().args,
            ["archive", "claude/sample-project/review"]
        );
        let rename = invocation(
            &t,
            &Action::Rename {
                new_name: "claude/sample-project/review-2".into(),
                archive_id: Some("a1".into()),
            },
            "u",
        )
        .unwrap();
        assert_eq!(
            rename.args,
            [
                "rename",
                "claude/sample-project/review",
                "claude/sample-project/review-2",
                "--archive",
                "a1"
            ]
        );
        let term = invocation(&target("mac"), &Action::Terminate, "u").unwrap();
        assert_eq!(
            term.args,
            [
                "@mac",
                "terminate",
                "claude/sample-project/review",
                "--expected-run-id",
                "run-1"
            ]
        );
        assert_eq!(term.timeout, std::time::Duration::from_secs(60));
        let mut no_run = target("");
        no_run.run_id.clear();
        assert_eq!(
            invocation(&no_run, &Action::Terminate, "u").unwrap().args,
            ["terminate", "claude/sample-project/review"]
        );
    }

    #[test]
    fn pause_all_and_resume_all_are_computer_wide_with_scaled_timeouts() {
        let secs = |c: &str, a: Action| invocation(&target(c), &a, "u").unwrap().timeout.as_secs();
        assert_eq!(
            invocation(&target(""), &Action::PauseAll { count: 3 }, "u")
                .unwrap()
                .args,
            ["pause", "--all"]
        );
        assert_eq!(
            invocation(&target("mac"), &Action::PauseAll { count: 3 }, "u")
                .unwrap()
                .args,
            ["@mac", "pause", "--all"]
        );
        assert_eq!(
            invocation(&target(""), &Action::ResumeAll { count: 3 }, "u")
                .unwrap()
                .args,
            ["resume", "--all", "-d"]
        );
        assert_eq!(
            invocation(&target("mac"), &Action::ResumeAll { count: 3 }, "u")
                .unwrap()
                .args,
            ["@mac", "resume", "--all", "-d"]
        );
        for c in ["", "mac"] {
            assert_eq!(secs(c, Action::PauseAll { count: 3 }), 15 + 90);
            assert_eq!(secs(c, Action::PauseAll { count: 0 }), 45);
            assert_eq!(secs(c, Action::PauseAll { count: 99999 }), 15 + 30 * 10000);
            assert_eq!(secs(c, Action::ResumeAll { count: 3 }), 15 + 45);
            assert_eq!(secs(c, Action::ResumeAll { count: 0 }), 30);
            assert_eq!(secs(c, Action::ResumeAll { count: 99999 }), 15 + 15 * 10000);
        }
        assert_eq!(
            serde_json::to_value(Action::PauseAll { count: 2 }).unwrap(),
            json!({"kind": "pause_all", "count": 2})
        );
        let inv = invocation(&target(""), &Action::PauseAll { count: 1 }, "u").unwrap();
        assert!(matches!(
            classify(
                &target(""),
                &Action::PauseAll { count: 1 },
                &inv,
                exit0(b"", "")
            ),
            Outcome::Done { cleanup: None, .. }
        ));
    }

    #[test]
    fn timeouts_differ_for_this_computer_and_a_peer() {
        let secs = |computer: &str, action: Action| {
            invocation(&target(computer), &action, "u")
                .unwrap()
                .timeout
                .as_secs()
        };
        let interrupt = Action::Interrupt { turn_started: 1.0 };
        let read = Action::MarkRead {
            reply_id: None,
            attention_signature: None,
        };
        let rename = || Action::Rename {
            new_name: "claude/sample-project/b".into(),
            archive_id: None,
        };
        assert_eq!(
            (secs("", interrupt.clone()), secs("p", interrupt)),
            (15, 45)
        );
        assert_eq!((secs("", read.clone()), secs("p", read)), (15, 35));
        assert_eq!(
            (secs("", Action::Pause), secs("p", Action::Pause)),
            (45, 45)
        );
        assert_eq!(
            (secs("", Action::Resume), secs("p", Action::Resume)),
            (30, 30)
        );
        assert_eq!((secs("", rename()), secs("p", rename())), (5, 15));
        assert_eq!(
            (secs("", Action::Archive), secs("p", Action::Archive)),
            (30, 45)
        );
        assert_eq!(
            (secs("", Action::Terminate), secs("p", Action::Terminate)),
            (45, 60)
        );
        assert_eq!((secs("", Action::Kill), secs("p", Action::Kill)), (5, 15));
    }

    #[test]
    fn kill_names_the_session_and_reports_no_worktree() {
        let inv = invocation(&target("mac"), &Action::Kill, "u").unwrap();
        assert_eq!(inv.args, ["@mac", "kill", "claude/sample-project/review"]);
        assert_eq!(inv.stdin, None);
        let line = "ZERUS_WORKTREE_CLEANUP {\"status\":\"removed\",\"path\":\"/home/example/wt\",\"common_dir\":\"/home/example/.git\"}\n";
        assert!(matches!(
            classify(&target(""), &Action::Kill, &inv, exit0(b"", line)),
            Outcome::Done { cleanup: None, .. }
        ));
        assert_eq!(
            classify(&target(""), &Action::Kill, &inv, Exit::TimedOut),
            Outcome::Failed {
                detail: "timed out".into()
            }
        );
    }

    #[test]
    fn start_fresh_resumes_with_fresh_and_the_run_guard() {
        let t = target("");
        let inv = invocation(&t, &Action::StartFresh, "u").unwrap();
        assert_eq!(
            inv.args,
            [
                "resume",
                "claude/sample-project/review",
                "--fresh",
                "--expected-run-id",
                "run-1",
                "-d"
            ]
        );
        assert_eq!((inv.stdin.clone(), inv.request_id.clone()), (None, None));
        assert_eq!(inv.timeout.as_secs(), 30);
        let mut no_run = target("mac");
        no_run.run_id.clear();
        let inv = invocation(&no_run, &Action::StartFresh, "u").unwrap();
        assert_eq!(
            inv.args,
            [
                "@mac",
                "resume",
                "claude/sample-project/review",
                "--fresh",
                "-d"
            ]
        );
        assert_eq!(inv.timeout.as_secs(), 30);
        let line = "ZERUS_WORKTREE_CLEANUP {\"status\":\"removed\",\"path\":\"/home/example/wt\",\"common_dir\":\"/home/example/.git\"}\n";
        assert!(matches!(
            classify(&t, &Action::StartFresh, &inv, exit0(b"", line)),
            Outcome::Done { cleanup: None, .. }
        ));
        assert_eq!(
            classify(&t, &Action::StartFresh, &inv, Exit::TimedOut),
            Outcome::Failed {
                detail: "timed out".into()
            }
        );
    }

    #[test]
    fn restore_and_forget_archive_name_the_archive() {
        let restore = Action::Restore {
            archive_id: "a1".into(),
        };
        let forget = Action::ForgetArchive {
            archive_id: "a1".into(),
        };
        let inv = invocation(&target(""), &restore, "u").unwrap();
        assert_eq!(
            inv.args,
            [
                "resume",
                "claude/sample-project/review",
                "--archive",
                "a1",
                "-d"
            ]
        );
        assert_eq!(inv.stdin, None);
        assert_eq!(inv.timeout.as_secs(), 30);
        let inv = invocation(&target("mac"), &restore, "u").unwrap();
        assert_eq!(inv.args[..2], ["@mac", "resume"]);
        assert_eq!(inv.args.last().unwrap(), "-d");
        assert_eq!(inv.timeout.as_secs(), 30);
        let inv = invocation(&target(""), &forget, "u").unwrap();
        assert_eq!(
            inv.args,
            ["kill", "claude/sample-project/review", "--archive", "a1"]
        );
        assert_eq!(inv.timeout.as_secs(), 5);
        let inv = invocation(&target("mac"), &forget, "u").unwrap();
        assert_eq!(
            inv.args,
            [
                "@mac",
                "kill",
                "claude/sample-project/review",
                "--archive",
                "a1"
            ]
        );
        assert_eq!(inv.timeout.as_secs(), 15);
        assert_eq!(
            serde_json::to_value(&restore).unwrap(),
            json!({"kind": "restore", "archive_id": "a1"})
        );
        assert_eq!(
            serde_json::to_value(&forget).unwrap(),
            json!({"kind": "forget_archive", "archive_id": "a1"})
        );
        for action in [
            Action::Restore {
                archive_id: String::new(),
            },
            Action::ForgetArchive {
                archive_id: String::new(),
            },
        ] {
            assert_eq!(
                invocation(&target("mac"), &action, "u").unwrap_err(),
                "Missing archive identity"
            );
        }
    }

    #[test]
    fn forget_kills_like_kill_but_is_its_own_kind() {
        let inv = invocation(&target("mac"), &Action::Forget, "u").unwrap();
        assert_eq!(inv.args, ["@mac", "kill", "claude/sample-project/review"]);
        assert_eq!(inv.stdin, None);
        assert_eq!(inv.timeout.as_secs(), 15);
        assert_eq!(
            invocation(&target(""), &Action::Forget, "u")
                .unwrap()
                .timeout
                .as_secs(),
            5
        );
        assert_ne!(Action::Forget, Action::Kill);
        assert_eq!(
            serde_json::to_value(Action::Forget).unwrap(),
            json!({"kind": "forget"})
        );
        assert_eq!(
            serde_json::to_value(Action::StartFresh).unwrap(),
            json!({"kind": "start_fresh"})
        );
        assert_eq!(
            serde_json::to_value(Action::ForgetBinding).unwrap(),
            json!({"kind": "forget_binding"})
        );
        assert!(matches!(
            classify(&target(""), &Action::Forget, &inv, exit0(b"", "")),
            Outcome::Done { cleanup: None, .. }
        ));
        assert_eq!(
            classify(&target(""), &Action::Forget, &inv, Exit::WriteFailed),
            Outcome::Failed {
                detail: "could not send the request".into()
            }
        );
    }

    #[test]
    fn forget_binding_sends_its_identity_through_session_action() {
        let inv = invocation(&target("mac"), &Action::ForgetBinding, "req-9").unwrap();
        assert_eq!(
            inv.args,
            [
                "@mac",
                "session-action",
                "claude/sample-project/review",
                "--json"
            ]
        );
        assert_eq!(inv.timeout.as_secs(), 45);
        assert_eq!(inv.request_id.as_deref(), Some("req-9"));
        let input: Value = serde_json::from_slice(inv.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({"request_id":"req-9","action":"forget","expected_run_id":"run-1","expected_conversation_id":"conv-1"})
        );
        assert!(!String::from_utf8(inv.stdin.unwrap()).unwrap().contains(' '));
        let local = invocation(&target(""), &Action::ForgetBinding, "r").unwrap();
        assert_eq!(local.timeout.as_secs(), 15);
        for clear in [0, 1] {
            let mut t = target("");
            if clear == 0 {
                t.run_id.clear()
            } else {
                t.conversation_id.clear()
            }
            assert!(invocation(&t, &Action::ForgetBinding, "r").is_err());
        }
    }

    #[test]
    fn forget_binding_follows_the_scoped_receipt_rules() {
        let t = target("");
        let a = Action::ForgetBinding;
        let inv = invocation(&t, &a, "req-3").unwrap();
        let receipt = |status: &str, error: Option<&str>, request: &str, run: &str| {
            let mut value = json!({"status": status, "request_id": request,
                "name": "claude/sample-project/review", "run_id": run, "conversation_id": "conv-1"});
            if let Some(error) = error {
                value["error"] = json!(error);
            }
            serde_json::to_vec(&value).unwrap()
        };
        let failed = |detail: &str| Outcome::Failed {
            detail: detail.into(),
        };
        let run = |bytes: Vec<u8>| classify(&t, &a, &inv, exit0(&bytes, ""));
        assert!(matches!(
            run(receipt("completed", None, "req-3", "run-1")),
            Outcome::Done { .. }
        ));
        assert_eq!(
            run(receipt("failed", Some(" nope "), "req-3", "run-1")),
            failed("nope")
        );
        assert_eq!(
            run(receipt("failed", None, "req-3", "run-1")),
            failed("The session was not forgotten.")
        );
        assert_eq!(
            run(receipt("uncertain", Some("lost"), "req-3", "run-1")),
            failed("The result was not confirmed (lost). Refresh before trying again.")
        );
        assert_eq!(
            run(receipt("uncertain", None, "req-3", "run-1")),
            failed("The result was not confirmed. Refresh before trying again.")
        );
        let invalid = failed("Invalid acknowledgement. Check the session before retrying.");
        assert_eq!(run(receipt("completed", None, "other", "run-1")), invalid);
        assert_eq!(run(receipt("completed", None, "req-3", "run-2")), invalid);
        assert_eq!(run(receipt("failed", Some("x"), "other", "run-1")), invalid);
        assert_eq!(run(receipt("submitted", None, "req-3", "run-1")), invalid);
        assert_eq!(run(b"{}".to_vec()), invalid);
        assert_eq!(run(b"not json".to_vec()), invalid);
        assert_eq!(
            classify(&t, &a, &inv, Exit::TimedOut),
            failed("Request timed out. Check the session before retrying.")
        );
        assert_eq!(
            classify(&t, &a, &inv, Exit::WriteFailed),
            failed("Transfer failed. Check the session before retrying.")
        );
        assert_eq!(
            classify(
                &t,
                &a,
                &inv,
                Exit::Code {
                    code: 1,
                    stdout: vec![],
                    stderr: " boom \n".into()
                }
            ),
            failed("boom")
        );
        assert_eq!(
            classify(
                &t,
                &a,
                &inv,
                Exit::Code {
                    code: 2,
                    stdout: vec![],
                    stderr: String::new()
                }
            ),
            failed("Could not complete the request. Check the session before retrying.")
        );
        assert_eq!(
            classify(
                &t,
                &a,
                &inv,
                Exit::Abnormal {
                    stderr: String::new()
                }
            ),
            failed("Could not complete the request. Check the session before retrying.")
        );
        assert_eq!(
            classify(&t, &a, &inv, Exit::StartFailed("no hgs".into())),
            failed("no hgs")
        );
    }

    #[test]
    fn interrupt_sends_its_identity_and_checks_the_receipt() {
        let t = target("");
        let a = Action::Interrupt {
            turn_started: 1793000000.5,
        };
        let inv = invocation(&t, &a, "req-2").unwrap();
        assert_eq!(
            inv.args,
            ["interrupt", "claude/sample-project/review", "--json"]
        );
        let input: serde_json::Value = serde_json::from_slice(inv.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            input,
            serde_json::json!({"request_id":"req-2","expected_run_id":"run-1",
            "expected_conversation_id":"conv-1","expected_turn_started":1793000000.5})
        );
        let ok = br#"{"status":"submitted","request_id":"req-2","name":"claude/sample-project/review","run_id":"run-1","conversation_id":"conv-1"}"#;
        assert!(matches!(
            classify(&t, &a, &inv, exit0(ok, "")),
            Outcome::Done { .. }
        ));
        assert_eq!(
            classify(&t, &a, &inv, exit0(b"{}", "")),
            Outcome::Failed {
                detail: "Invalid acknowledgement. Check Terminal before retrying.".into()
            }
        );
        assert_eq!(
            classify(&t, &a, &inv, Exit::TimedOut),
            Outcome::Failed {
                detail: "Request timed out. Check Terminal before retrying.".into()
            }
        );
        assert_eq!(
            classify(&t, &a, &inv, Exit::WriteFailed),
            Outcome::Failed {
                detail: "Transfer failed. Check Terminal before retrying.".into()
            }
        );
        assert_eq!(
            classify(
                &t,
                &a,
                &inv,
                Exit::Code {
                    code: 1,
                    stdout: vec![],
                    stderr: String::new()
                }
            ),
            Outcome::Failed {
                detail: "Could not complete the request. Check Terminal.".into()
            }
        );
    }

    #[test]
    fn mark_read_accepts_applied_or_not_found() {
        let t = target("");
        let a = Action::MarkRead {
            reply_id: Some("rep-1".into()),
            attention_signature: None,
        };
        let inv = invocation(&t, &a, "u").unwrap();
        assert_eq!(inv.args, ["read", "claude/sample-project/review", "--json"]);
        let input: serde_json::Value = serde_json::from_slice(inv.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            input,
            serde_json::json!({"run_id":"run-1","conversation_id":"conv-1","reply_id":"rep-1","attention_signature":null})
        );
        let applied = br#"{"ok":true,"applied":{"reply_id":true,"attention_signature":false},"read":{"conversation_id":"conv-1"}}"#;
        assert!(matches!(
            classify(&t, &a, &inv, exit0(applied, "")),
            Outcome::Done { .. }
        ));
        let gone = br#"{"ok":false,"error":"not_found"}"#;
        assert!(matches!(
            classify(
                &t,
                &a,
                &inv,
                Exit::Code {
                    code: 1,
                    stdout: gone.to_vec(),
                    stderr: String::new()
                }
            ),
            Outcome::Done { .. }
        ));
        assert_eq!(
            classify(&t, &a, &inv, Exit::TimedOut),
            Outcome::Failed {
                detail: "Saving read state timed out.".into()
            }
        );
        assert_eq!(
            classify(&t, &a, &inv, exit0(b"{\"ok\":true}", "")),
            Outcome::Failed {
                detail: "Could not save read state on that machine.".into()
            }
        );
    }

    #[test]
    fn writes_succeed_on_exit_zero_and_report_stderr_otherwise() {
        let t = target("");
        let inv = invocation(&t, &Action::Pause, "u").unwrap();
        assert!(matches!(
            classify(&t, &Action::Pause, &inv, exit0(b"", "")),
            Outcome::Done { cleanup: None, .. }
        ));
        assert_eq!(
            classify(
                &t,
                &Action::Pause,
                &inv,
                Exit::Code {
                    code: 1,
                    stdout: vec![],
                    stderr: " hgs: no such session \n".into()
                }
            ),
            Outcome::Failed {
                detail: "hgs: no such session".into()
            }
        );
        assert_eq!(
            classify(&t, &Action::Pause, &inv, Exit::TimedOut),
            Outcome::Failed {
                detail: "timed out".into()
            }
        );
        assert_eq!(
            classify(
                &t,
                &Action::Pause,
                &inv,
                Exit::StartFailed("No such file or directory".into())
            ),
            Outcome::Failed {
                detail: "No such file or directory".into()
            }
        );
    }

    #[test]
    fn archive_and_terminate_carry_exactly_one_cleanup_line() {
        let t = target("");
        let inv = invocation(&t, &Action::Archive, "u").unwrap();
        let line = "ZERUS_WORKTREE_CLEANUP {\"status\":\"kept\",\"path\":\"/home/example/sample-project-wt\",\"common_dir\":\"/home/example/sample-project/.git\",\"reason\":\"unsaved work\"}\n";
        match classify(&t, &Action::Archive, &inv, exit0(b"", line)) {
            Outcome::Done {
                cleanup: Some(c), ..
            } => {
                assert_eq!(c.status, "kept");
                assert_eq!(c.reason.as_deref(), Some("unsaved work"));
            }
            other => panic!("{other:?}"),
        }
        let two = format!("{line}{line}");
        assert!(matches!(
            classify(&t, &Action::Archive, &inv, exit0(b"", &two)),
            Outcome::Done { cleanup: None, .. }
        ));
        let no_dir = "ZERUS_WORKTREE_CLEANUP {\"status\":\"removed\",\"path\":\"/home/example/wt\",\"common_dir\":\"\"}\n";
        assert!(matches!(
            classify(&t, &Action::Archive, &inv, exit0(b"", no_dir)),
            Outcome::Done { cleanup: None, .. }
        ));
        assert!(matches!(
            classify(
                &t,
                &Action::Pause,
                &invocation(&t, &Action::Pause, "u").unwrap(),
                exit0(b"", line)
            ),
            Outcome::Done { cleanup: None, .. }
        ));
    }

    #[test]
    fn rename_changes_only_the_tag_by_the_session_tag_rule() {
        let rename = |new_name: &str| {
            invocation(
                &target(""),
                &Action::Rename {
                    new_name: new_name.into(),
                    archive_id: None,
                },
                "u",
            )
            .map(|inv| inv.args)
        };
        assert_eq!(
            rename("claude/sample-project/release planning").unwrap()[2],
            "claude/sample-project/release planning"
        );
        for (bad, problem) in [
            ("claude/sample-project/", "Enter a session name."),
            (
                "claude/sample-project/ review",
                "Remove spaces at the beginning or end.",
            ),
            (
                "claude/sample-project/review ",
                "Remove spaces at the beginning or end.",
            ),
            (
                "claude/sample-project/a/b",
                "Names cannot contain /, : or . or control characters.",
            ),
            (
                "claude/sample-project/a:b",
                "Names cannot contain /, : or . or control characters.",
            ),
            (
                "claude/sample-project/v1.2",
                "Names cannot contain /, : or . or control characters.",
            ),
            (
                "claude/sample-project/a\u{7}b",
                "Names cannot contain /, : or . or control characters.",
            ),
        ] {
            assert_eq!(rename(bad), Err(problem.to_owned()), "{bad:?}");
        }
        for other in [
            "codex/sample-project/review-2",
            "claude/other-project/review-2",
            "claude/sample-project",
            "",
        ] {
            assert!(
                rename(other).unwrap_err().starts_with("rename must keep"),
                "{other:?}"
            );
        }
        assert_eq!(tag_problem("release planning"), None);
    }

    #[test]
    fn allowed_follows_reports_then_the_agent() {
        let listed = ViewRow {
            allowed_actions: Some(vec!["rename".into()]),
            agent: "claude".into(),
            ..ViewRow::default()
        };
        assert!(allowed(&listed, "rename"));
        assert!(!allowed(&listed, "pause"));
        let claude = ViewRow {
            agent: "claude".into(),
            ..ViewRow::default()
        };
        assert!(allowed(&claude, "pause"));
        let opencode = ViewRow {
            agent: "opencode".into(),
            ..ViewRow::default()
        };
        assert!(allowed(&opencode, "rename") && allowed(&opencode, "forget"));
        assert!(!allowed(&opencode, "pause"));
    }

    const UNREADABLE: &str = "Could not read a session reference. Update hgs on this computer.";

    #[test]
    fn the_reference_call_matches_the_qt_client() {
        let local = reference_invocation(&target(""), None);
        assert_eq!(
            local.args,
            [
                "reference",
                "claude/sample-project/review",
                "--json",
                "--run",
                "run-1",
                "--conversation",
                "conv-1"
            ]
        );
        assert_eq!(local.timeout, Duration::from_secs(15));
        assert!(local.stdin.is_none() && local.request_id.is_none());
        let peer = reference_invocation(&target("mac"), Some("a1"));
        assert_eq!(peer.args[0], "@mac");
        assert_eq!(peer.args[1], "reference");
        assert_eq!(peer.args[peer.args.len() - 2..], ["--archive", "a1"]);
        assert_eq!(peer.timeout, Duration::from_secs(25));
        let empty = reference_invocation(&target(""), Some(""));
        assert!(!empty.args.contains(&"--archive".to_owned()));
    }

    #[test]
    fn a_reference_is_a_session_uri_of_bounded_size() {
        let uri = "zerus://session?machine=m&name=n";
        let ok = exit0(format!(r#"{{"uri":"{uri}"}}"#).as_bytes(), "");
        assert_eq!(parse_reference(ok), Ok(uri.to_owned()));
        for bad in [
            exit0(br#"{"uri":"https://example.com/x"}"#, ""),
            exit0(b"not json", ""),
            exit0(br#"{"other":1}"#, ""),
            exit0(br#"{"uri":5}"#, ""),
            exit0(
                format!(r#"{{"uri":"zerus://session?{}"}}"#, "x".repeat(8193 - 16)).as_bytes(),
                "",
            ),
        ] {
            assert_eq!(parse_reference(bad), Err(UNREADABLE.to_owned()));
        }
        let longest = format!("zerus://session?{}", "x".repeat(8192 - 16));
        let max = exit0(format!(r#"{{"uri":"{longest}"}}"#).as_bytes(), "");
        assert_eq!(parse_reference(max), Ok(longest));
    }

    #[test]
    fn a_failed_reference_call_gives_the_failure_detail() {
        let code = |code, stderr: &str| Exit::Code {
            code,
            stdout: Vec::new(),
            stderr: stderr.into(),
        };
        assert_eq!(parse_reference(code(1, " boom \n")), Err("boom".to_owned()));
        assert_eq!(
            parse_reference(code(2, "  ")),
            Err("exit code 2".to_owned())
        );
        assert_eq!(
            parse_reference(code(1, &"e".repeat(5000)))
                .unwrap_err()
                .chars()
                .count(),
            4000
        );
        assert_eq!(parse_reference(Exit::TimedOut), Err("timed out".to_owned()));
        assert_eq!(
            parse_reference(Exit::StartFailed("No such file".into())),
            Err(format!(
                "cannot start '{}': No such file",
                crate::default_hgs().to_string_lossy()
            ))
        );
        assert_eq!(
            parse_reference(Exit::Abnormal { stderr: "x".into() }),
            Err("x".to_owned())
        );
        assert_eq!(
            parse_reference(Exit::WriteFailed),
            Err("exit code -1".to_owned())
        );
    }

    #[test]
    fn send_now_input_and_outcomes() {
        let t = target("studio");
        let action = Action::SendNow {
            queue_id: "q-1".into(),
        };
        let inv = invocation(&t, &action, "req-9").unwrap();
        assert_eq!(
            inv.args,
            [
                "@studio",
                "send-now",
                "claude/sample-project/review",
                "--json"
            ]
        );
        assert_eq!(inv.timeout, Duration::from_secs(35));
        assert_eq!(
            invocation(&target(""), &action, "r").unwrap().timeout,
            Duration::from_secs(20)
        );
        let input: Value = serde_json::from_slice(inv.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({"request_id": "req-9", "expected_run_id": "run-1",
                   "expected_conversation_id": "conv-1", "queue_id": "q-1"})
        );
        let outcome = |exit: Exit| classify(&t, &action, &inv, exit);
        let receipt = json!({"status": "submitted", "request_id": "req-9", "name": "claude/sample-project/review",
                             "run_id": "run-1", "conversation_id": "conv-1", "queue_id": "q-1"});
        assert!(matches!(
            outcome(exit(0, receipt.to_string().as_bytes(), "")),
            Outcome::Done { .. }
        ));
        for (key, value) in [
            ("status", "requested"),
            ("request_id", "other"),
            ("name", "other"),
            ("run_id", "other"),
            ("conversation_id", "other"),
            ("queue_id", "q-2"),
        ] {
            let mut changed = receipt.clone();
            changed[key] = json!(value);
            assert_eq!(
                outcome(exit(0, changed.to_string().as_bytes(), "")),
                uncertain("Invalid queue acknowledgement. Check Terminal before retrying."),
                "{key}"
            );
        }
        assert_eq!(
            outcome(Exit::StartFailed("no hgs".into())),
            failed("no hgs")
        );
        assert_eq!(
            outcome(Exit::TimedOut),
            uncertain("Queue action timed out. Check Terminal before retrying.")
        );
        assert_eq!(
            outcome(Exit::WriteFailed),
            uncertain("Queue action failed. Check Terminal before retrying.")
        );
        assert_eq!(
            outcome(Exit::Abnormal { stderr: "".into() }),
            uncertain("Could not confirm sending the queue. Check Terminal before retrying.")
        );
        assert_eq!(
            outcome(exit(
                1,
                b"",
                "hgs: The native queue is no longer visible; nothing was sent\n"
            )),
            failed("hgs: The native queue is no longer visible; nothing was sent")
        );
        assert_eq!(
            outcome(exit(1, b"", "Delivery uncertain: check Terminal")),
            uncertain("Delivery uncertain: check Terminal")
        );
    }

    #[test]
    fn remove_queued_input_and_outcomes() {
        let t = target("");
        let action = Action::RemoveQueued {
            message_id: "msg_01H-abc_9".into(),
        };
        let inv = invocation(&t, &action, "req-3").unwrap();
        assert_eq!(
            inv.args,
            ["session-action", "claude/sample-project/review", "--json"]
        );
        assert_eq!(inv.timeout, Duration::from_secs(15));
        assert_eq!(
            invocation(&target("mac"), &action, "r").unwrap().timeout,
            Duration::from_secs(45)
        );
        let input: Value = serde_json::from_slice(inv.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({"request_id": "req-3", "action": "remove_queued", "expected_run_id": "run-1",
                   "expected_conversation_id": "conv-1", "message_id": "msg_01H-abc_9"})
        );
        let outcome = |exit: Exit| classify(&t, &action, &inv, exit);
        let receipt = |status: &str, error: &str| {
            json!({"status": status, "request_id": "req-3", "name": "claude/sample-project/review",
                   "run_id": "run-1", "conversation_id": "conv-1", "error": error})
            .to_string()
        };
        assert!(matches!(
            outcome(exit(0, receipt("completed", "").as_bytes(), "")),
            Outcome::Done { .. }
        ));
        assert_eq!(
            outcome(exit(0, receipt("failed", "").as_bytes(), "")),
            failed("The queued message was not removed.")
        );
        assert_eq!(
            outcome(exit(0, receipt("failed", " already sent ").as_bytes(), "")),
            failed("already sent")
        );
        assert_eq!(
            outcome(exit(0, receipt("uncertain", "").as_bytes(), "")),
            uncertain("The result was not confirmed. Refresh before trying again.")
        );
        assert_eq!(
            outcome(exit(
                0,
                receipt("uncertain", "socket closed").as_bytes(),
                ""
            )),
            uncertain("The result was not confirmed (socket closed). Refresh before trying again.")
        );
        let invalid = uncertain("Invalid acknowledgement. Check the session before retrying.");
        assert_eq!(
            outcome(exit(0, receipt("submitted", "").as_bytes(), "")),
            invalid
        );
        let mut other: Value = serde_json::from_str(&receipt("completed", "")).unwrap();
        other["run_id"] = json!("run-2");
        assert_eq!(outcome(exit(0, other.to_string().as_bytes(), "")), invalid);
        assert_eq!(outcome(Exit::StartFailed("gone".into())), failed("gone"));
        assert_eq!(
            outcome(Exit::WriteFailed),
            uncertain("Transfer failed. Check the session before retrying.")
        );
        assert_eq!(
            outcome(Exit::TimedOut),
            uncertain("Request timed out. Check the session before retrying.")
        );
        assert_eq!(
            outcome(exit(2, b"", "")),
            failed("Could not complete the request. Check the session before retrying.")
        );
    }

    #[test]
    fn a_bad_message_id_is_refused() {
        let refused = Err("Choose the exact queued OpenCode message.".to_owned());
        for id in [
            "",
            "msg_",
            "message_1",
            "msg_a b",
            "msg_a/b",
            &format!("msg_{}", "x".repeat(125)),
        ] {
            let action = Action::RemoveQueued {
                message_id: id.to_owned(),
            };
            assert_eq!(invocation(&target(""), &action, "r"), refused, "{id}");
        }
        let longest = Action::RemoveQueued {
            message_id: format!("msg_{}", "x".repeat(124)),
        };
        assert!(invocation(&target(""), &longest, "r").is_ok());
    }

    #[test]
    fn queue_actions_never_come_from_webview_json() {
        assert!(
            serde_json::from_value::<Action>(json!({"kind": "send_now", "queue_id": "q"})).is_err()
        );
        assert!(serde_json::from_value::<Action>(
            json!({"kind": "remove_queued", "message_id": "msg_a"})
        )
        .is_err());
        // The same shape still reads an ordinary action.
        assert!(serde_json::from_value::<Action>(json!({"kind": "pause"})).is_ok());
    }

    #[test]
    fn allowed_with_prefers_details() {
        let opencode = ViewRow {
            agent: "opencode".into(),
            ..ViewRow::default()
        };
        assert!(!allowed_with(&opencode, &json!({}), "remove_queued"));
        assert!(allowed_with(
            &opencode,
            &json!({"allowed_actions": ["remove_queued"]}),
            "remove_queued"
        ));
        let reported = ViewRow {
            allowed_actions: Some(vec!["remove_queued".into()]),
            ..opencode.clone()
        };
        assert!(allowed_with(&reported, &json!({}), "remove_queued"));
        assert!(!allowed_with(
            &reported,
            &json!({"allowed_actions": ["rename"]}),
            "remove_queued"
        ));
    }

    const REQUEST: &str = "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";

    fn asked() -> Value {
        json!({"question_id": "q-1", "question_hash": "h-1", "run_id": "run-1",
               "conversation_id": "conv-1", "source": "hook", "can_answer": true,
               "questions": [{"id": "q_0"}]})
    }

    fn chosen() -> Vec<questions::AnswerItem> {
        vec![questions::AnswerItem::Choice {
            question_id: "q_0".into(),
            selected_option_ids: vec!["pg".into()],
            text: String::new(),
        }]
    }

    #[test]
    fn answer_input_is_qts() {
        let t = target("studio");
        let action = Action::Answer {
            question: asked(),
            answers: chosen(),
        };
        let inv = invocation(&t, &action, REQUEST).unwrap();
        // The answers never go into the arguments.
        assert_eq!(
            inv.args,
            [
                "@studio",
                "answer",
                "claude/sample-project/review",
                "--json"
            ]
        );
        assert_eq!(inv.timeout, Duration::from_secs(45));
        assert_eq!(
            invocation(&target(""), &action, REQUEST).unwrap().timeout,
            Duration::from_secs(30)
        );
        let input: Value = serde_json::from_slice(inv.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({"request_id": REQUEST, "question_id": "q-1", "expected_question_hash": "h-1",
                   "expected_run_id": "run-1", "expected_conversation_id": "conv-1",
                   "answers": [{"question_id": "q_0", "selected_option_ids": ["pg"], "text": ""}]})
        );
        // A startup request may come before the conversation.
        let hash = "f".repeat(64);
        let trust = json!({"question_id": format!("claude-trust:{hash}"), "question_hash": hash,
            "run_id": "run-1", "conversation_id": "", "source": "claude_folder_trust",
            "answer_transport": "claude_tui", "can_answer": true});
        let action = Action::Answer {
            question: trust,
            answers: chosen(),
        };
        assert!(invocation(&t, &action, REQUEST).is_ok());
        // A Skip of a Codex optional question that allows it.
        let skip = json!({"question_id": "cx", "question_hash": "h", "run_id": "run-1",
            "conversation_id": "conv-1", "source": "codex_async", "optional": true,
            "can_skip": true, "can_answer": false});
        let action = Action::Answer {
            question: skip,
            answers: vec![questions::AnswerItem::Skip {
                question_id: "q_0".into(),
                skip: true,
            }],
        };
        let input: Value =
            serde_json::from_slice(&invocation(&t, &action, REQUEST).unwrap().stdin.unwrap())
                .unwrap();
        assert_eq!(
            input["answers"],
            json!([{"question_id": "q_0", "skip": true}])
        );
    }

    #[test]
    fn answer_refusals() {
        let t = target("");
        let not_ready = Err(
            "This request is not ready to answer. Wait for an updated request or open Terminal."
                .to_owned(),
        );
        let refused = |question: Value, answers: Vec<questions::AnswerItem>| {
            invocation(&t, &Action::Answer { question, answers }, REQUEST)
        };
        for key in ["question_id", "question_hash", "run_id", "conversation_id"] {
            let mut question = asked();
            question[key] = json!("");
            assert_eq!(refused(question, chosen()), not_ready, "{key}");
        }
        assert_eq!(refused(asked(), Vec::new()), not_ready);
        let mut closed = asked();
        closed["can_answer"] = json!(false);
        assert_eq!(refused(closed, chosen()), not_ready);
        // A startup form needs its exact id and a 64-character hash.
        let near = json!({"question_id": "claude-trust:abc", "question_hash": "abc", "run_id": "r",
            "conversation_id": "", "source": "claude_folder_trust", "answer_transport": "claude_tui", "can_answer": true});
        assert_eq!(refused(near, chosen()), not_ready);
        assert_eq!(
            invocation(
                &t,
                &Action::Answer {
                    question: asked(),
                    answers: chosen()
                },
                "not-a-uuid"
            ),
            Err("The approval journal request identity is invalid.".to_owned())
        );
        let huge = vec![questions::AnswerItem::Choice {
            question_id: "q_0".into(),
            selected_option_ids: Vec::new(),
            text: "x".repeat(70_000),
        }];
        assert_eq!(
            refused(asked(), huge),
            Err("The answers exceed 64 KiB.".to_owned())
        );
        assert!(serde_json::from_value::<Action>(
            json!({"kind": "answer", "question": {}, "answers": []})
        )
        .is_err());
    }

    #[test]
    fn answer_outcomes() {
        let t = target("");
        let outcome = |question: Value, answers: Vec<questions::AnswerItem>, exit: Exit| {
            let action = Action::Answer { question, answers };
            let inv = invocation(&t, &action, REQUEST).unwrap();
            classify(&t, &action, &inv, exit)
        };
        let receipt = json!({"status": "answered", "request_id": REQUEST, "name": "claude/sample-project/review",
            "question_id": "q-1", "question_hash": "h-1", "run_id": "run-1", "conversation_id": "conv-1"});
        let ok = |receipt: &Value| exit(0, receipt.to_string().as_bytes(), "");
        assert!(matches!(
            outcome(asked(), chosen(), ok(&receipt)),
            Outcome::Done { .. }
        ));
        let invalid =
            uncertain("The answer acknowledgement is invalid. Check Terminal before retrying.");
        for key in [
            "request_id",
            "name",
            "question_id",
            "question_hash",
            "run_id",
            "conversation_id",
        ] {
            let mut changed = receipt.clone();
            changed[key] = json!("other");
            assert_eq!(outcome(asked(), chosen(), ok(&changed)), invalid, "{key}");
        }
        let mut missing = receipt.clone();
        missing.as_object_mut().unwrap().remove("conversation_id");
        assert_eq!(outcome(asked(), chosen(), ok(&missing)), invalid);
        // Submitted only for a Codex optional question's real answer.
        let mut submitted = receipt.clone();
        submitted["status"] = json!("submitted");
        assert_eq!(outcome(asked(), chosen(), ok(&submitted)), invalid);
        let mut codex = asked();
        codex["source"] = json!("codex_async");
        codex["optional"] = json!(true);
        codex["can_skip"] = json!(true);
        assert!(matches!(
            outcome(codex.clone(), chosen(), ok(&submitted)),
            Outcome::Done { .. }
        ));
        let mut skipped = submitted.clone();
        skipped["skipped"] = json!(true);
        assert_eq!(outcome(codex.clone(), chosen(), ok(&skipped)), invalid);
        let skip = vec![questions::AnswerItem::Skip {
            question_id: "q_0".into(),
            skip: true,
        }];
        assert_eq!(outcome(codex, skip, ok(&submitted)), invalid);
        // Failures: only a failed start or a plain exit 1 is definite.
        assert_eq!(
            outcome(asked(), chosen(), Exit::StartFailed("no such file".into())),
            failed("Cannot start hgs: no such file")
        );
        assert_eq!(
            outcome(
                asked(),
                chosen(),
                exit(1, b"", "hgs: the question changed\n")
            ),
            failed("hgs: the question changed")
        );
        assert_eq!(
            outcome(asked(), chosen(), exit(1, b"", "")),
            failed("Answer transfer failed (exit 1).")
        );
        assert_eq!(
            outcome(
                asked(),
                chosen(),
                exit(1, b"", "Delivery Uncertain: check Terminal")
            ),
            uncertain("Delivery Uncertain: check Terminal")
        );
        assert_eq!(
            outcome(asked(), chosen(), exit(2, b"", "")),
            uncertain("Answer transfer failed (exit 2).")
        );
        let timed_out = uncertain("Answer delivery timed out. Check Terminal before retrying.");
        assert_eq!(outcome(asked(), chosen(), Exit::TimedOut), timed_out);
        assert_eq!(outcome(asked(), chosen(), Exit::WriteFailed), timed_out);
        assert_eq!(
            outcome(
                asked(),
                chosen(),
                Exit::Abnormal {
                    stderr: String::new()
                }
            ),
            uncertain("Answer transfer failed (exit -1).")
        );
        let big = vec![b' '; 70_000];
        assert_eq!(
            outcome(asked(), chosen(), exit(0, &big, "")),
            uncertain("The answer acknowledgement is too large. Check Terminal before retrying.")
        );
    }

    #[test]
    fn an_uncertain_answer_is_never_retried() {
        // An uncertain outcome is reported once; the card then locks Submit
        // until Allow retry, and a new attempt is a new request identity.
        let shown = questions::Question::from_value(&asked());
        let mut draft = questions::AnswerDraft::default();
        draft.begin_sending();
        draft.set_error(
            "Answer delivery timed out. Check Terminal before retrying.",
            true,
        );
        let ctx = questions::CardContext {
            available: true,
            ..questions::CardContext::default()
        };
        assert!(!questions::card(&shown, &draft, &ctx).submit.enabled);
        assert!(!draft.sending && draft.uncertain);
        draft.allow_retry();
        assert!(!draft.uncertain);
    }

    #[test]
    fn recovery_actions_use_qt_payload_preflight_and_updated_job() {
        const JOB: &str = "11111111-1111-4111-8111-111111111111";
        const REQUEST: &str = "22222222-2222-4222-8222-222222222222";
        let target = target("studio");
        let action = Action::Recovery {
            job_id: JOB.into(),
            action: RecoveryControl::Now,
        };
        let inv = invocation(&target, &action, REQUEST).unwrap();
        assert_eq!(inv.args, ["@studio", "recovery", "action"]);
        assert_eq!(inv.timeout, Duration::from_secs(25));
        assert_eq!(inv.request_id, None);
        let input: Value = serde_json::from_slice(inv.stdin.as_deref().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({"name": target.name, "id": JOB, "action": "now"})
        );

        let fresh = json!({
            "run_id": "run-1", "conversation_id": "conv-1",
            "recovery": {"id": JOB, "name": target.name, "state": "waiting"},
        });
        assert_eq!(
            recovery_preflight(&target, &action, reply_exit(&fresh)),
            Ok(())
        );
        let stale = json!({
            "run_id": "run-2", "conversation_id": "conv-1",
            "recovery": {"id": JOB, "name": target.name, "state": "waiting"},
        });
        assert!(recovery_preflight(&target, &action, reply_exit(&stale)).is_err());
        let already_cancelled = json!({
            "run_id": "run-1", "conversation_id": "conv-1",
            "recovery": {"id": JOB, "name": target.name, "state": "cancelled"},
        });
        assert!(recovery_preflight(&target, &action, reply_exit(&already_cancelled)).is_err());
        assert!(recovery_preflight(&target, &action, Exit::TimedOut).is_err());

        let updated = json!({"id": JOB, "name": target.name, "state": "waiting", "due_at": 123});
        assert!(matches!(
            classify(&target, &action, &inv, reply_exit(&updated)),
            Outcome::Done { .. }
        ));
        let wrong = json!({"id": JOB, "name": target.name, "state": "waiting", "due_at": "later"});
        assert!(matches!(
            classify(&target, &action, &inv, reply_exit(&wrong)),
            Outcome::Uncertain { .. }
        ));
        assert!(matches!(
            classify(&target, &action, &inv, Exit::TimedOut),
            Outcome::Uncertain { .. }
        ));
        let bad = Action::Recovery {
            job_id: "not-a-job".into(),
            action: RecoveryControl::Cancel,
        };
        assert!(invocation(&target, &bad, REQUEST).is_err());
    }
}
