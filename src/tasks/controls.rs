//! Closed CLI surface for inactive task controls (cg5h.4.7, slice 2).
//! Parsing or a valid plan supplies no native authority. The following wiring
//! leaf must prove the original actor, owning store and actually loaded native
//! adapter before admitting any effect. There is no fallback to `interrupt`.
use super::{fail, Config, Result};
use serde_json::{json, Value};
use std::{borrow::Cow, io::Read};
use zerus_desktop_ledger::task_controls::{REQUEST_BYTES, TASK_CONTROLS};

#[cfg(test)]
mod tests;
pub(crate) mod wire;
use wire::Command;

const INACTIVE: &str =
    "Task controls are inactive until the executor queue is activated. Nothing was attempted.";
pub(crate) const HELP_MARKER: &str = "task-control-v1";

pub(crate) fn help_marker() -> Option<&'static str> {
    TASK_CONTROLS.then_some(HELP_MARKER)
}

pub(super) fn dispatch(config: &Config, args: &[String]) -> Result<i32> {
    let result = command(config, args);
    match result {
        Ok(value) if value["ok"] == false && !args.iter().any(|s| s == "--json") => {
            Err(fail(value["error"].as_str().unwrap_or(INACTIVE)))
        }
        Ok(value) => {
            let code = if value["ok"] == false { 1 } else { 0 };
            println!("{value}");
            Ok(code)
        }
        Err((code, error)) if args.iter().any(|s| s == "--json") => {
            println!(
                "{}",
                json!({"schema":1,"ok":false,"code":code,"error":error,"attempted":false})
            );
            Ok(1)
        }
        Err((_, error)) => Err(fail(error)),
    }
}

fn command(config: &Config, args: &[String]) -> std::result::Result<Value, (&'static str, String)> {
    let command = if args.first().is_some_and(|s| s == "begin-attempt") {
        wire::begin(&args[1..]).map(Command::Begin)
    } else {
        let action = wire::action(&args[1..]).map_err(|e| ("invalid_task_control", e))?;
        let mut input = Vec::new();
        std::io::stdin()
            .take(REQUEST_BYTES as u64 + 1)
            .read_to_end(&mut input)
            .map_err(|e| ("invalid_task_control", e.to_string()))?;
        wire::read(action, &input)
    }
    .map_err(|e| ("invalid_task_control", e))?;
    let (operation, request_id, reference) = command.identity();
    if !TASK_CONTROLS {
        return Ok(
            json!({"schema":1,"ok":false,"code":"task_controls_inactive","error":INACTIVE,
            "attempted":false,"operation":operation,"request_id":request_id,"reference":reference}),
        );
    }
    crate::desktop::task_controls::request(config, command)
        .map_err(|e| ("task_control_service_unavailable", e.to_string()))
}

/// The plain start sentence of the task instructions, which managed work
/// replaces; it names the start command each dispatched prompt pins.
pub(super) const PLAIN_START: &str = "Acknowledge a dispatched task with the `hgs task start` command its prompt pins before working; this does not send it again.";

pub(super) fn session_instructions(original: &str) -> Cow<'_, str> {
    if !TASK_CONTROLS {
        return Cow::Borrowed(original);
    }
    original.replace(
        PLAIN_START,
        "Acknowledge managed work exactly once with its original pinned `hgs task begin-attempt <ref> --attempt UUID` command. It replaces start; do not run both. Plain start is only the legacy acknowledgement and never binds a managed native turn."
    ).into()
}

/// The caller gates this before adding it to a managed native request and
/// before hashing that request. This never scans or rewrites task/user text.
pub(super) fn pin_dispatch(prompt: &mut String, project: &str, task: &str, attempt: &str) {
    if TASK_CONTROLS {
        pin_begin(prompt, project, task, attempt);
    }
    super::reports::pin(prompt, project, task, attempt);
}

/// Only fresh send/handoff/reopen uses this instruction. Ordinary feedback
/// keeps its report origin and never requests a second acknowledgement.
pub(super) fn pin_begin(prompt: &mut String, project: &str, task: &str, attempt: &str) {
    let reference = zerus_replica::command_reference(project, task);
    prompt.push_str(&format!("\n\n[Zerus acknowledgement origin: {attempt}]\nAcknowledge this execution exactly once with `hgs task begin-attempt {reference} --attempt {attempt}`. This replaces any plain task start instruction above; do not run both. Keep this original attempt UUID. A start acknowledgement cannot substitute a newer attempt."));
}
