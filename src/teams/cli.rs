//! `hgs team`: the command line of Agent Teams.
//!
//! One command, one JSON reply. A command that has input reads one JSON
//! object on stdin; a request never names its own actor. Writes go through the
//! store's operations, which own every decision about authority; the CLI only
//! proves who is calling and which command that actor may run.
mod actor;
mod board;
mod catalog_cmd;
mod commands;
mod create;
mod input;
mod observed_timing;
mod phases;
mod provider_settings;
mod reads;
#[cfg(test)]
mod tests;

use crate::config::Config;
use serde_json::{json, Value};
use zerus_team_plan::problem::code;
use zerus_team_plan::Problem;

pub(super) const USAGE: &str = "usage: hgs team preview on|off|status | catalog show|snapshot|receipt|import|export | template list|show|save|import|export|remove | create|start-planning --template ID | list|waiting|status|base|inbox|queue|cards|journal|phases [--team ID] [--limit 1..200] [--after CURSOR] [--bundle] | phase --team ID --phase N | observed --team ID --phase N [--node N] [--window-ms N] [--limit 1..200] [--after CURSOR] | journal --recorded-timings [same page flags] [--timing-seqs IDS] | evidence --team ID --node N | current | validation-config (JSON read) | advance-base|validation-settings|settings|pause|resume|stop|archive|decide|admit|route|set-deps|set-phase-barrier|pause-phase|resume-phase|move-phase|set-phase-acceptance|submit-draft-phase|edit-draft-phase|approve-phase|reject-phase|undo-reject-phase|accept-phase|send-back-phase|submit-phase-corrections|edit-phase-corrections|approve-phase-corrections|reject-phase-corrections|undo-phase-correction-rejection|accept|return|reassign|hold|release|cancel|reply|ask|verdict|pause-member|resume-member|stop-member|archive-member|add-member|lead-add-member|swap-member|rename-team|set-goal (JSON on stdin)";

/// The running command. `args` are the words after `team`; `provided` is a
/// test's stdin, and `None` reads this command's own stdin when it needs one.
pub(super) fn dispatch(config: &Config, args: &[String]) -> (Value, i32) {
    let words = input::words(args);
    // None of these commands has an independent Session alternative. Refuse
    // before caller inspection; neither detachment nor a different state root
    // can supply a window gesture. The writer repeats this boundary too.
    if words.first().is_some_and(|word| {
        matches!(
            word.as_str(),
            "create"
                | "start-planning"
                | "advance-base"
                | "validation-settings"
                | "decide"
                | "settings"
                | "add-member"
                | "swap-member"
                | "rename-team"
                | "set-goal"
                | "set-deps"
                | "set-phase-acceptance"
                | "stop"
                | "archive"
                | "stop-member"
                | "archive-member"
        )
    }) {
        return refusal(crate::desktop::teams::person_actions::required());
    }
    let caller = crate::teams::caller::caller(config);
    let mutation = words
        .first()
        .is_some_and(|word| commands::variant(word).is_some());
    if mutation && matches!(caller, actor::Caller::Person) {
        return refusal(crate::desktop::teams::person_actions::required());
    }
    run(config, &caller, &words, None)
}

fn run(
    config: &Config,
    caller: &actor::Caller,
    args: &[String],
    provided: Option<&str>,
) -> (Value, i32) {
    match execute(config, caller, args, provided) {
        Ok(reply) => (reply, 0),
        Err(problem) if is_read(args) => problem_reply(problem, true),
        Err(problem) => refusal(problem),
    }
}

fn execute(
    config: &Config,
    caller: &actor::Caller,
    args: &[String],
    provided: Option<&str>,
) -> Result<Value, Problem> {
    let (command, rest) = args
        .split_first()
        .ok_or_else(|| input::usage("name a team command"))?;
    match command.as_str() {
        "catalog" | "template" => catalog_cmd::run(config, caller, command, rest, provided),
        "create" => commands::create(config, caller, rest, provided),
        "start-planning" => create::run_kind(config, caller, rest, provided, true),
        "validation-config" => provider_settings::read(config, caller, rest, provided),
        "status" => reads::status(config, caller, rest),
        "observed" => observed_timing::read(config, caller, rest),
        "base" => reads::base(config, caller, rest),
        "phases" | "phase" => phases::read(config, caller, command, rest),
        "list" => reads::list_teams(config, caller, rest),
        "waiting" => reads::waiting(config, caller, rest),
        "inbox" | "queue" | "cards" | "journal" => reads::list(config, caller, command, rest),
        "current" => reads::current(config, caller, rest),
        "evidence" => reads::evidence(config, caller, rest),
        other => commands::run(config, caller, other, rest, provided),
    }
}

/// One request's largest accepted size: the same bound the operation envelope
/// validates against.
const MAX_REQUEST: u64 = 262_144;

/// A command that reads one JSON request never waits on a terminal's stdin
/// twice: the text is read once, here, only when the command needs it, and
/// never past the request bound. A request one byte over the bound is refused
/// before anything parses it.
fn request_input(provided: Option<&str>) -> Result<String, Problem> {
    if let Some(text) = provided {
        if text.len() as u64 > MAX_REQUEST {
            return Err(too_large());
        }
        return Ok(text.to_owned());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_REQUEST + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| input::invalid(format!("Cannot read the request on stdin: {error}")))?;
    if bytes.len() as u64 > MAX_REQUEST {
        return Err(too_large());
    }
    String::from_utf8(bytes).map_err(|_| input::invalid("The request on stdin is not UTF-8."))
}

fn too_large() -> Problem {
    Problem::new(
        code::OVER_CAP,
        format!("The request is larger than {MAX_REQUEST} bytes."),
    )
}

pub(super) fn refusal(problem: Problem) -> (Value, i32) {
    problem_reply(problem, false)
}

fn problem_reply(mut problem: Problem, read: bool) -> (Value, i32) {
    if problem.code == code::STORAGE_FAILED {
        let detail = problem
            .detail
            .take()
            .unwrap_or_else(|| problem.message.clone());
        problem = problem.with_detail(detail);
        problem.message = if read {
            "The read failed and may be repeated."
        } else {
            "The result is not known; repeat the same request with the same request ID."
        }
        .into();
    }
    (json!({"ok": false, "error": problem}), 1)
}

fn is_read(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some(
            "validation-config" | "status" | "base" | "list" | "waiting" | "inbox" | "queue"
            | "cards" | "journal" | "current" | "phases" | "phase" | "observed" | "evidence",
        ) => true,
        Some("catalog" | "template") => matches!(
            args.get(1).map(String::as_str),
            Some("show" | "list" | "export" | "snapshot" | "receipt")
        ),
        _ => false,
    }
}

pub(super) fn unknown(command: &str) -> Problem {
    Problem::new(
        code::NOT_FOUND,
        format!("Unknown team command: {command}; {USAGE}"),
    )
    .with_refs([command])
}

pub(super) fn storage(error: impl std::fmt::Display) -> Problem {
    crate::teams::store::storage(error)
}

pub(super) fn not_found_team(team: &str) -> Problem {
    Problem::new(code::NOT_FOUND, "Team does not exist.").with_refs([team])
}
