//! Agent Teams: a lead session, member sessions with roles, and work that
//! runs through a pipeline. See
//! `docs/superpowers/specs/2026-11-11-agent-teams-design.md`.
//!
//! Teams are a preview and are off unless the person turned them on for this
//! computer. With them off nothing here creates a file, and every `hgs team`
//! command except `preview` answers `teams_inactive`.
// WhiteWh fork: upstream's Agent Teams are compiled but not wired to `hgs team`
// until the fork's pluggable team systems choose them (ZERUS/T-l2n9qt).
#![allow(dead_code)]
use crate::cli::Result;
use crate::config::Config;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use zerus_team_plan::problem::code;
use zerus_team_plan::Problem;

pub(crate) mod caller;
pub(crate) mod catalog;
pub(crate) mod clean;
mod cli;
// The member launch's host check (zerus-63rru.28) uses this once a DeepSeek
// seat is accepted; until then only its tests do.
#[cfg(unix)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod hosted;
pub(crate) const OLDER_DEEPSEEK_HOST: &str = "older_deepseek_host";
pub(crate) mod integrate;
pub(crate) mod ops;
pub(crate) mod providers;
pub(crate) mod store;
pub(crate) mod tasks;

/// The folder of everything Teams keep under the `hgs` state directory.
const DIR: &str = "teams";
/// Its presence turns the preview on for this computer.
const PREVIEW: &str = "preview";

const INACTIVE_MESSAGE: &str =
    "Agent Teams are off on this computer. Nothing was attempted. A person turns them on with: hgs team preview on";
/// The stable code every `hgs team` command answers while the preview is off.
pub(crate) const INACTIVE: &str = code::TEAMS_INACTIVE;

fn preview_file(state: &Path) -> PathBuf {
    state.join(DIR).join(PREVIEW)
}

/// Whether the person turned Teams on for this computer.
pub(crate) fn enabled(state: &Path) -> bool {
    preview_file(state).is_file()
}

pub(crate) fn dispatch(config: &Config, args: &[String], _dry: bool) -> Result<i32> {
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|word| *word != "--json")
        .collect();
    let (reply, exit) = match words.first().copied() {
        Some("preview") => preview(
            &config.state,
            words.get(1).copied().unwrap_or("status"),
            || caller::caller(config),
        ),
        _ if !enabled(&config.state) => inactive(),
        _ => cli::dispatch(config, args),
    };
    println!("{reply}");
    Ok(exit)
}

/// The answer of every command but `preview` while the preview is off.
pub(crate) fn inactive() -> (Value, i32) {
    refusal(Problem::new(INACTIVE, INACTIVE_MESSAGE))
}

fn refusal(problem: Problem) -> (Value, i32) {
    (json!({"ok": false, "error": problem}), 1)
}

/// The preview switch's own entry, and the off answer for every command. The
/// commands themselves live in [`cli`]; kept here for the preview switch's
/// tests, which never reach a command.
#[cfg(test)]
fn answer(state: &Path, args: &[String], from_session: bool) -> (Value, i32) {
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|word| *word != "--json")
        .collect();
    if words.first() == Some(&"preview") {
        return preview(state, words.get(1).copied().unwrap_or("status"), || {
            if from_session {
                caller::Caller::Refused(Problem::new(
                    code::NOT_ALLOWED,
                    "A session cannot change the preview.",
                ))
            } else {
                caller::Caller::Person
            }
        });
    }
    if !enabled(state) {
        return refusal(Problem::new(INACTIVE, INACTIVE_MESSAGE));
    }
    let command = words.first().copied().unwrap_or("");
    refusal(
        Problem::new(code::NOT_FOUND, format!("Unknown team command: {command}"))
            .with_refs([command]),
    )
}

fn preview(state: &Path, action: &str, proof: impl FnOnce() -> caller::Caller) -> (Value, i32) {
    let shown = |state: &Path| {
        json!({
            "ok": true, "preview": if enabled(state) { "on" } else { "off" },
            "note": caller::limits(),
        })
    };
    // Status is public and does not read caller state. Mutating either way
    // uses the same caller proof as every person-only Teams operation.
    if matches!(action, "on" | "off") {
        match proof() {
            caller::Caller::Person => (),
            caller::Caller::Refused(problem) => return refusal(problem),
            caller::Caller::Session(_) => return refusal(Problem::new(code::NOT_ALLOWED,
                "Only a person turns the Teams preview on or off, from a terminal outside any session.")),
        }
    }
    match action {
        "status" => (shown(state), 0),
        "on" => match turn_on(state) {
            Ok(()) => (shown(state), 0),
            Err(_) => refusal(Problem::new(
                code::INVALID_STATE,
                "Cannot turn the preview on.",
            )),
        },
        "off" => match turn_off(state) {
            Ok(()) => (shown(state), 0),
            Err(_) => refusal(Problem::new(
                code::INVALID_STATE,
                "Cannot turn the preview off.",
            )),
        },
        other => refusal(
            Problem::new(code::NOT_FOUND, "Usage: hgs team preview on|off|status")
                .with_refs([other]),
        ),
    }
}

fn turn_on(state: &Path) -> std::io::Result<()> {
    crate::platform::private_dir(&state.join(DIR))?;
    std::fs::write(preview_file(state), b"on\n")
}

/// Removes the marker, and the folder too when nothing else is in it.
fn turn_off(state: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(preview_file(state)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let _ = std::fs::remove_dir(state.join(DIR));
    Ok(())
}

#[cfg(all(test, unix))]
mod preview_client_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh empty folder, removed when the value is dropped.
    struct State(PathBuf);

    impl State {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("hgs-teams-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn entries(&self) -> usize {
            std::fs::read_dir(&self.0).unwrap().count()
        }
    }

    impl Drop for State {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    fn code_of(reply: &Value) -> &str {
        reply["error"]["code"].as_str().unwrap_or("")
    }

    #[test]
    fn off_by_default_and_nothing_is_created() {
        let state = State::new("default");
        assert!(!enabled(&state.0));
        for command in [
            "status --json",
            "create --json",
            "",
            "inbox",
            "preview status",
        ] {
            let (reply, exit) = answer(&state.0, &words(command), false);
            if command == "preview status" {
                assert_eq!(
                    (reply["ok"].as_bool(), reply["preview"].as_str(), exit),
                    (Some(true), Some("off"), 0)
                );
            } else {
                assert_eq!((code_of(&reply), exit), ("teams_inactive", 1), "{command}");
            }
        }
        assert_eq!(state.entries(), 0);
    }

    #[test]
    fn a_person_turns_the_preview_on_and_off() {
        let state = State::new("toggle");
        let (reply, exit) = answer(&state.0, &words("preview on"), false);
        assert_eq!((reply["preview"].as_str(), exit), (Some("on"), 0));
        assert!(enabled(&state.0));
        let (reply, _) = answer(&state.0, &words("preview status --json"), false);
        assert_eq!(reply["preview"], "on");
        let (reply, exit) = answer(&state.0, &words("preview off"), false);
        assert_eq!((reply["preview"].as_str(), exit), (Some("off"), 0));
        assert!(!enabled(&state.0));
        assert_eq!(state.entries(), 0, "the empty folder goes with the marker");
        let (reply, exit) = answer(&state.0, &words("preview off"), false);
        assert_eq!(
            (reply["preview"].as_str(), exit),
            (Some("off"), 0),
            "off twice is still off"
        );
    }

    /// One text states every limit of the caller proofs: the interim
    /// ancestry rule, the hosted DeepSeek proof with its child agents and
    /// forks, and that proof's own limit (zerus-63rru.28).
    #[test]
    fn the_preview_states_every_limit_of_the_caller_proofs() {
        let state = State::new("limits");
        let (reply, exit) = answer(&state.0, &words("preview status"), false);
        assert_eq!(exit, 0);
        let note = reply["note"].as_str().unwrap();
        assert_eq!(note, caller::limits());
        for part in [
            caller::INTERIM_NOTE,
            "A DeepSeek child agent's command is proven as its top-level session",
            "a DeepSeek fork is its own session",
            crate::state::session_graph::HOSTED_LIMIT,
        ] {
            assert!(note.contains(part), "{part}");
        }
    }

    #[test]
    fn a_session_cannot_change_the_preview() {
        let state = State::new("session");
        let (reply, exit) = answer(&state.0, &words("preview on"), true);
        assert_eq!((code_of(&reply), exit), ("not_allowed", 1));
        assert!(!enabled(&state.0));
        assert_eq!(state.entries(), 0);
        answer(&state.0, &words("preview on"), false);
        let (reply, exit) = answer(&state.0, &words("preview off"), true);
        assert_eq!((code_of(&reply), exit), ("not_allowed", 1));
        assert!(enabled(&state.0));
        let (reply, exit) = answer(&state.0, &words("preview status"), true);
        assert_eq!(
            (reply["preview"].as_str(), exit),
            (Some("on"), 0),
            "a session may read it"
        );
    }

    #[test]
    fn turning_off_keeps_a_folder_that_holds_something() {
        let state = State::new("keep");
        answer(&state.0, &words("preview on"), false);
        std::fs::write(state.0.join(DIR).join("other"), b"x").unwrap();
        answer(&state.0, &words("preview off"), false);
        assert!(!enabled(&state.0));
        assert!(state.0.join(DIR).join("other").is_file());
    }

    #[test]
    fn an_unknown_command_is_named_once_the_preview_is_on() {
        let state = State::new("unknown");
        answer(&state.0, &words("preview on"), false);
        let (reply, exit) = answer(&state.0, &words("frobnicate --json"), false);
        assert_eq!((code_of(&reply), exit), ("not_found", 1));
        assert_eq!(reply["error"]["refs"][0], "frobnicate");
        let (reply, exit) = answer(&state.0, &words("preview maybe"), false);
        assert_eq!((code_of(&reply), exit), ("not_found", 1));
    }
}
