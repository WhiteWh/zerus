//! One `hgs team <op> --json` read of package 5's CLI: flags name the team,
//! the page size and the cursor; the reply is one bounded page. The output is
//! size-bounded and the shape the projection relies on is checked before a
//! view sees it.
//!
//! The desktop follows the teams of this computer only, one at a time (M1).
//! The reads are the store's pages and never a whole team. A request that only
//! the person can answer inside a team session is not part of these reads: it
//! arrives in the desktop's session attention data.
use crate::actions::{Exit, Invocation};
use crate::reads::READ_LIMIT;
use crate::run::{run_cancellable, run_limited, Stopped, TooLarge};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

/// Rows one read asks for: the store's own page bound and `--limit` maximum.
pub const PAGE: u64 = 200;
/// The longest team id a read names.
pub const TEAM_ID_BYTES: usize = 64;
/// The longest cursor token the CLI gives back (`t1.` and its page).
pub const CURSOR_BYTES: usize = 2048;
/// How long a read waits for `hgs` (it is this computer's hgs).
const TIMEOUT: Duration = Duration::from_secs(15);
/// How much of a failure or refusal is kept for the view.
const MESSAGE_CHARS: usize = 2000;

/// One read of the team store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeamRead {
    Observed {
        team: String,
        phase: u32,
        node: Option<u32>,
        window_ms: u64,
        after: Option<String>,
    },
    Phases {
        team: String,
        after: Option<String>,
    },
    Draft {
        team: String,
        phase: u32,
    },
    /// `team list`: the teams of this computer, newest first.
    List {
        after: Option<String>,
    },
    /// `team status --team ID`: one team, its members and its counts, without
    /// its bundle.
    Status {
        team: String,
    },
    /// `team queue --team ID`: one page of the team's queue and plan nodes.
    Queue {
        team: String,
        after: Option<String>,
    },
    /// `team waiting`: one page of the open owner cards of every team of this
    /// computer, oldest first.
    Waiting {
        after: Option<String>,
    },
}

impl TeamRead {
    /// The operation's word: `hgs team <op> --json`.
    pub fn op(&self) -> &'static str {
        match self {
            Self::List { .. } => "list",
            Self::Status { .. } => "status",
            Self::Queue { .. } => "queue",
            Self::Waiting { .. } => "waiting",
            Self::Phases { .. } => "phases",
            Self::Draft { .. } => "phase",
            Self::Observed { .. } => "observed",
        }
    }

    /// The team the read is about, if it is about one.
    pub fn team(&self) -> Option<&str> {
        match self {
            Self::Status { team }
            | Self::Queue { team, .. }
            | Self::Phases { team, .. }
            | Self::Draft { team, .. }
            | Self::Observed { team, .. } => Some(team),
            Self::List { .. } | Self::Waiting { .. } => None,
        }
    }

    /// The page cursor the read carries, if any.
    pub fn after(&self) -> Option<&str> {
        match self {
            Self::List { after }
            | Self::Waiting { after }
            | Self::Queue { after, .. }
            | Self::Phases { after, .. }
            | Self::Observed { after, .. } => after.as_deref(),
            Self::Status { .. } | Self::Draft { .. } => None,
        }
    }

    /// The command's words: the operation, its flags, and the output flag.
    /// Every read names this computer's hgs; a page is at most [`PAGE`] rows.
    pub fn args(&self) -> Result<Vec<String>, TeamError> {
        let mut args = vec!["team".to_owned(), self.op().to_owned()];
        match self {
            Self::Observed {
                team,
                phase,
                node,
                window_ms,
                after,
            } => {
                if !valid_team(team)
                    || !(1..=super::observed_timing::MAX_WINDOW_MS).contains(window_ms)
                {
                    return Err(TeamError::Failed {
                        message: "Not an observation scope.".into(),
                    });
                }
                args.extend([
                    "--team".into(),
                    team.clone(),
                    "--phase".into(),
                    phase.to_string(),
                    "--window-ms".into(),
                    window_ms.to_string(),
                    "--limit".into(),
                    PAGE.to_string(),
                ]);
                if let Some(node) = node {
                    args.extend(["--node".into(), node.to_string()]);
                }
                push_after_bounded(
                    &mut args,
                    after.as_deref(),
                    super::observed_timing::MAX_CURSOR_BYTES,
                )?;
            }
            Self::List { after } => {
                args.extend(["--limit".to_owned(), PAGE.to_string()]);
                push_after(&mut args, after.as_deref())?;
            }
            Self::Status { team } | Self::Draft { team, .. } => {
                if !valid_team(team) {
                    return Err(TeamError::Failed {
                        message: "Not a team id.".into(),
                    });
                }
                args.extend(["--team".to_owned(), team.clone()]);
                if let Self::Draft { phase, .. } = self {
                    args.extend(["--phase".to_owned(), phase.to_string()]);
                } else {
                    args.push("--bundle".to_owned());
                }
            }
            Self::Queue { team, after } | Self::Phases { team, after } => {
                if !valid_team(team) {
                    return Err(TeamError::Failed {
                        message: "Not a team id.".into(),
                    });
                }
                args.extend(["--team".to_owned(), team.clone()]);
                args.extend(["--limit".to_owned(), PAGE.to_string()]);
                push_after(&mut args, after.as_deref())?;
            }
            Self::Waiting { after } => {
                args.extend(["--limit".to_owned(), PAGE.to_string()]);
                push_after(&mut args, after.as_deref())?;
            }
        }
        args.push("--json".to_owned());
        Ok(args)
    }
}

fn push_after(args: &mut Vec<String>, after: Option<&str>) -> Result<(), TeamError> {
    push_after_bounded(args, after, CURSOR_BYTES)
}
fn push_after_bounded(
    args: &mut Vec<String>,
    after: Option<&str>,
    bound: usize,
) -> Result<(), TeamError> {
    let Some(after) = after else {
        return Ok(());
    };
    if after.is_empty()
        || after.len() > bound
        || after.starts_with('-')
        || after.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(TeamError::Failed {
            message: "Not a page cursor.".into(),
        });
    }
    args.extend(["--after".to_owned(), after.to_owned()]);
    Ok(())
}

/// A team id as the CLI takes it: its option takes a lowercase canonical UUID.
pub fn valid_team(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
        && id.len() <= TEAM_ID_BYTES
}

/// Why a read gave no view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeamError {
    /// `hgs` refused it: its stable code (`teams_inactive`, `not_found`, ...)
    /// and one sentence.
    Refused { code: String, message: String },
    /// `hgs` could not run here (it is missing or not executable): Teams are
    /// unavailable on this computer, not merely failing. The desktop hides
    /// the entry point for it.
    Unavailable { message: String },
    /// `hgs` failed, timed out or gave no reply.
    Failed { message: String },
    /// The reply is not the JSON the read expects.
    Invalid { message: String },
    /// The reply passed the read's limit; the command was ended.
    TooLarge { limit: usize },
    /// The shutdown ended the read's child before it answered.
    Cancelled,
}

impl TeamError {
    /// The batch's failure code: a refusal's own code, else a stable one.
    pub fn code(&self) -> &str {
        match self {
            Self::Refused { code, .. } => code,
            Self::Unavailable { .. } => "unavailable",
            Self::Failed { .. } => "failed",
            Self::Invalid { .. } => "invalid_reply",
            Self::TooLarge { .. } => "too_large",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Refused { message, .. }
            | Self::Unavailable { message }
            | Self::Failed { message }
            | Self::Invalid { message } => message,
            Self::TooLarge { .. } => "the reply passed the read's limit",
            Self::Cancelled => "the read was cancelled",
        }
    }
}

impl std::fmt::Display for TeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for TeamError {}

fn text_of(stderr: &str) -> String {
    let text: String = stderr.trim().chars().take(MESSAGE_CHARS).collect();
    if text.is_empty() {
        "hgs gave no reply".to_owned()
    } else {
        text
    }
}

/// Whether the reply says it refused, whether or not its code is valid.
fn names_a_refusal(stdout: &[u8]) -> bool {
    serde_json::from_slice::<Value>(stdout)
        .is_ok_and(|reply| reply.get("ok").and_then(Value::as_bool) == Some(false))
}

/// The standard output of a read, or why it gave none.
pub(crate) fn output(exit: Exit) -> Result<Vec<u8>, TeamError> {
    match exit {
        Exit::Code {
            code: 0, stdout, ..
        } => Ok(stdout),
        Exit::Code { stdout, stderr, .. } => {
            if let Some((code, message)) = super::refusal::parse(&stdout) {
                return Err(TeamError::Refused { code, message });
            }
            if names_a_refusal(&stdout) {
                // A refusal envelope with no valid code proves nothing: it is
                // an invalid read, never a refusal.
                return Err(invalid("hgs refused the read without a valid code"));
            }
            Err(TeamError::Failed {
                message: text_of(&stderr),
            })
        }
        Exit::Abnormal { stderr } => Err(TeamError::Failed {
            message: text_of(&stderr),
        }),
        Exit::TimedOut => Err(TeamError::Failed {
            message: "hgs did not answer in time".into(),
        }),
        Exit::StartFailed(message) => Err(TeamError::Unavailable {
            message: format!("hgs could not run here: {message}"),
        }),
        Exit::WriteFailed => Err(TeamError::Failed {
            message: "hgs did not take the request".into(),
        }),
    }
}

/// The command of a read: `hgs team <op> --json` with its flags.
pub fn invocation(read: &TeamRead) -> Result<Invocation, TeamError> {
    Ok(Invocation {
        args: read.args()?,
        stdin: None,
        timeout: TIMEOUT,
        request_id: None,
    })
}

fn invalid(message: impl Into<String>) -> TeamError {
    TeamError::Invalid {
        message: message.into(),
    }
}

/// Every read's reply carries the actor it was read for.
fn actor(value: &Value) -> bool {
    value.get("actor").is_some_and(Value::is_object)
}

/// The rows of a page reply, as [`TeamRead`] documents them.
fn rows<'a>(value: &'a Value, read: &TeamRead) -> Result<&'a [Value], TeamError> {
    value
        .get("rows")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| invalid(format!("team {} gives no rows", read.op())))
}

/// Checks a reply: its exit, then the shape the read relies on.
pub fn decode(read: &TeamRead, exit: Exit) -> Result<Value, TeamError> {
    let stdout = output(exit)?;
    if matches!(read, TeamRead::Observed { .. })
        && stdout.len() > super::observed_timing::MAX_PAGE_BYTES
    {
        return Err(invalid("The observation reply exceeds its bound."));
    }
    let value: Value =
        serde_json::from_slice(&stdout).map_err(|error| invalid(error.to_string()))?;
    if !value.is_object() {
        return Err(invalid("not a JSON object"));
    }
    // Every reply says whether the store took the read; only `true` is a
    // success, so a reply without the flag can never replace a shown view.
    match value.get("ok").and_then(Value::as_bool) {
        Some(true) => {}
        Some(false) => {
            // Only a well-formed refusal is a refusal. Anything else is an
            // invalid read, so it can never stand as a refused page.
            let Some((code, message)) = super::refusal::parse(&stdout) else {
                return Err(invalid("hgs refused the read without a valid code"));
            };
            return Err(TeamError::Refused { code, message });
        }
        None => return Err(invalid("team reply gives no success flag")),
    }
    if !actor(&value) {
        return Err(invalid("team reply gives no actor"));
    }
    match read {
        TeamRead::Observed {
            team,
            phase,
            node,
            window_ms,
            ..
        } => {
            super::observed_timing::decode(&value, team, *phase, *node, *window_ms)
                .map_err(invalid)?;
        }
        TeamRead::Phases { team, .. } => {
            if value.get("team").and_then(Value::as_str) != Some(team) {
                return Err(invalid("phase list gives another team"));
            }
            super::view::phases::list(&value).map_err(invalid)?;
        }
        TeamRead::Draft { team, phase } => {
            if value.get("team").and_then(Value::as_str) != Some(team) {
                return Err(invalid("phase draft gives another team"));
            }
            super::view::phases::draft(&value, *phase).map_err(invalid)?;
        }
        TeamRead::Status { team } => {
            let fits = value
                .get("team")
                .filter(|found| found.is_object())
                .is_some_and(|found| {
                    found.get("id").and_then(Value::as_str) == Some(team.as_str())
                });
            if !fits {
                return Err(invalid("team status gives another team"));
            }
            if value.get("members").and_then(Value::as_array).is_none() {
                return Err(invalid("team status gives no members"));
            }
            if value.get("counts").and_then(Value::as_object).is_none() {
                return Err(invalid("team status gives no counts"));
            }
        }
        TeamRead::Queue { team, .. } => {
            if value.get("team").and_then(Value::as_str) != Some(team.as_str()) {
                return Err(invalid("team queue gives another team"));
            }
            rows(&value, read)?;
        }
        TeamRead::List { .. } | TeamRead::Waiting { .. } => {
            let rows = rows(&value, read)?;
            if matches!(read, TeamRead::List { .. })
                && rows
                    .iter()
                    .any(|row| row.get("id").and_then(Value::as_str).is_none())
            {
                return Err(invalid("team list gives a team without an id"));
            }
        }
    }
    Ok(value)
}

/// [`read`] that ends its child within one poll once `cancel` is set: the
/// shutdown of the reader uses it, so closing the application never waits for
/// a slow read.
pub fn read_cancellable(
    hgs: &Path,
    read: &TeamRead,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Value, TeamError> {
    let invocation = invocation(read)?;
    let exit =
        run_cancellable(hgs, &invocation, &[], READ_LIMIT, cancel).map_err(
            |stopped| match stopped {
                Stopped::TooLarge(TooLarge { cap, .. }) => TeamError::TooLarge { limit: cap },
                Stopped::Cancelled => TeamError::Cancelled,
            },
        )?;
    decode(read, exit)
}

/// Runs one read through `hgs` and checks its reply. Blocks for at most the
/// read's timeout plus a few seconds; callers run it off the UI thread.
pub fn read(hgs: &Path, read: &TeamRead) -> Result<Value, TeamError> {
    let invocation = invocation(read)?;
    let exit = run_limited(hgs, &invocation, &[], READ_LIMIT)
        .map_err(|TooLarge { cap, .. }| TeamError::TooLarge { limit: cap })?;
    decode(read, exit)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pinned CLI replies of package 5, copied beside this module so the
    /// branch's tests run before package 5 merges; they are the same bytes as
    /// `crates/team-plan/fixtures/cli-*.json` at its `fa46dcc08`.
    fn fixture(name: &str) -> Value {
        let text = match name {
            "status" => include_str!("fixtures/cli-status.json"),
            "queue" => include_str!("fixtures/cli-queue.json"),
            "cards" => include_str!("fixtures/cli-cards.json"),
            other => panic!("no fixture {other}"),
        };
        serde_json::from_str(text).expect("a pinned CLI fixture parses")
    }

    const TEAM: &str = "00000000-0000-4000-8000-00000000beef";

    fn ok(stdout: Value) -> Exit {
        Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(&stdout).unwrap(),
            stderr: String::new(),
        }
    }

    #[test]
    fn each_read_names_its_flags_and_one_page() {
        assert_eq!(
            TeamRead::Status { team: TEAM.into() }.args().unwrap(),
            ["team", "status", "--team", TEAM, "--bundle", "--json"]
        );
        assert_eq!(
            TeamRead::Queue {
                team: TEAM.into(),
                after: None
            }
            .args()
            .unwrap(),
            ["team", "queue", "--team", TEAM, "--limit", "200", "--json"]
        );
        assert_eq!(
            TeamRead::Queue {
                team: TEAM.into(),
                after: Some("t1.abc".into())
            }
            .args()
            .unwrap(),
            ["team", "queue", "--team", TEAM, "--limit", "200", "--after", "t1.abc", "--json"]
        );
        assert_eq!(
            TeamRead::List { after: None }.args().unwrap(),
            ["team", "list", "--limit", "200", "--json"]
        );
        assert_eq!(
            TeamRead::Waiting { after: None }.args().unwrap(),
            ["team", "waiting", "--limit", "200", "--json"]
        );
    }

    #[test]
    fn a_read_sends_no_request_and_waits_fifteen_seconds() {
        let invocation = invocation(&TeamRead::Status { team: TEAM.into() }).unwrap();
        assert_eq!(invocation.stdin, None);
        assert_eq!(invocation.timeout, TIMEOUT);
        assert_eq!(invocation.request_id, None);
    }

    #[test]
    fn an_unknown_team_id_or_cursor_is_refused() {
        for team in [
            "",
            "example",
            "00000000-0000-4000-8000-00000000BEEF",
            "00000000-0000-4000-8000-00000000beefX",
        ] {
            assert!(!valid_team(team), "{team:?}");
            assert!(
                TeamRead::Status {
                    team: team.to_owned()
                }
                .args()
                .is_err(),
                "{team:?}"
            );
        }
        assert!(valid_team(TEAM));
        for cursor in ["", "has space", "-option", "with\nnewline"] {
            assert!(
                TeamRead::List {
                    after: Some(cursor.into())
                }
                .args()
                .is_err(),
                "{cursor:?}"
            );
        }
    }

    #[test]
    fn the_pinned_status_reply_is_the_expected_shape() {
        let reply = decode(
            &TeamRead::Status { team: TEAM.into() },
            ok(fixture("status")),
        );
        assert!(reply.is_ok(), "{:?}", reply.err());
        assert!(matches!(
            decode(
                &TeamRead::Status { team: TEAM.into() },
                ok(
                    serde_json::json!({"ok": true, "team": {"id": "other"}, "members": [], "counts": {}})
                )
            ),
            Err(TeamError::Invalid { .. })
        ));
        for reply in [
            serde_json::json!({"ok": true, "team": {"id": TEAM}, "counts": {}}),
            serde_json::json!({"ok": true, "team": {"id": TEAM}, "members": []}),
            serde_json::json!({"ok": true, "team": {"id": TEAM}, "members": [], "counts": {}, "actor": 3}),
        ] {
            assert!(
                matches!(
                    decode(&TeamRead::Status { team: TEAM.into() }, ok(reply)),
                    Err(TeamError::Invalid { .. })
                ),
                "a status reply missing a part is refused"
            );
        }
    }

    #[test]
    fn the_pinned_page_replies_are_the_expected_shape() {
        for read in [
            TeamRead::Queue {
                team: TEAM.into(),
                after: None,
            },
            TeamRead::List { after: None },
            TeamRead::Waiting { after: None },
        ] {
            let reply = if matches!(read, TeamRead::Queue { .. }) {
                fixture("queue")
            } else if matches!(read, TeamRead::List { .. }) {
                serde_json::json!({"ok": true, "actor": {"kind": "person"}, "rows": [], "next": null})
            } else {
                fixture("cards")
            };
            assert!(decode(&read, ok(reply)).is_ok(), "{read:?}");
        }
        // A queue page of another team, a page without rows, and a list row
        // without an id are refused.
        assert!(matches!(
            decode(
                &TeamRead::Queue {
                    team: "00000000-0000-4000-8000-00000000ffff".into(),
                    after: None
                },
                ok(fixture("queue"))
            ),
            Err(TeamError::Invalid { .. })
        ));
        assert!(matches!(
            decode(
                &TeamRead::Queue {
                    team: TEAM.into(),
                    after: None
                },
                ok(serde_json::json!({"ok": true, "actor": {"kind": "person"}, "team": TEAM}))
            ),
            Err(TeamError::Invalid { .. })
        ));
        assert!(matches!(
            decode(
                &TeamRead::List { after: None },
                ok(
                    serde_json::json!({"ok": true, "actor": {"kind": "person"}, "rows": [{"name": "x"}]})
                )
            ),
            Err(TeamError::Invalid { .. })
        ));
        assert!(matches!(
            decode(
                &TeamRead::Waiting { after: None },
                ok(serde_json::json!({"ok": true, "rows": []}))
            ),
            Err(TeamError::Invalid { .. })
        ));
    }

    #[test]
    fn a_refusal_keeps_its_code() {
        let refused = serde_json::json!({
            "ok": false,
            "error": {"code": "teams_inactive", "message": "Teams are off."},
        });
        let error = decode(
            &TeamRead::List { after: None },
            Exit::Code {
                code: 1,
                stdout: serde_json::to_vec(&refused).unwrap(),
                stderr: String::new(),
            },
        )
        .unwrap_err();
        assert_eq!(error.code(), "teams_inactive");
        assert_eq!(error.message(), "Teams are off.");
    }

    #[test]
    fn a_missing_success_flag_is_invalid() {
        let error = decode(
            &TeamRead::List { after: None },
            ok(serde_json::json!({"actor": {}, "rows": [], "next": null})),
        )
        .unwrap_err();
        assert_eq!(error.code(), "invalid_reply");
    }

    #[test]
    fn a_missing_hgs_is_unavailable_not_failed() {
        let error = read(
            std::path::Path::new("/nonexistent/example-review-hgs"),
            &TeamRead::List { after: None },
        )
        .unwrap_err();
        assert_eq!(error.code(), "unavailable");
    }

    #[test]
    fn a_refusal_without_a_valid_code_is_an_invalid_read() {
        for error in [
            serde_json::json!({"ok": false, "error": {"code": ""}}),
            serde_json::json!({"ok": false, "error": {}}),
            serde_json::json!({"ok": false, "error": {"code": "has space"}}),
            serde_json::json!({"ok": false, "error": {"message": "no code"}}),
        ] {
            let reply = serde_json::json!({"ok": true, "actor": {}, "rows": [], "next": null});
            let mut reply = reply;
            reply["ok"] = serde_json::json!(false);
            reply["error"] = error["error"].clone();
            // Both a started command that ends nonzero and one that ends zero
            // with the malformed envelope are invalid reads.
            for code in [1, 0] {
                let decode_error = decode(
                    &TeamRead::List { after: None },
                    Exit::Code {
                        code,
                        stdout: serde_json::to_vec(&reply).unwrap(),
                        stderr: String::new(),
                    },
                )
                .unwrap_err();
                assert!(
                    matches!(decode_error, TeamError::Invalid { .. }),
                    "{reply} is not a refusal: {decode_error:?}"
                );
            }
        }
        // A well-formed refusal is still a refusal with its code.
        let refused = serde_json::json!({
            "ok": false,
            "error": {"code": "teams_inactive", "message": "Teams are off."},
        });
        let error = decode(
            &TeamRead::List { after: None },
            Exit::Code {
                code: 1,
                stdout: serde_json::to_vec(&refused).unwrap(),
                stderr: String::new(),
            },
        )
        .unwrap_err();
        assert_eq!(error.code(), "teams_inactive");
    }

    #[test]
    fn a_failure_or_a_bad_reply_is_worded() {
        let failed = decode(
            &TeamRead::List { after: None },
            Exit::Code {
                code: 2,
                stdout: Vec::new(),
                stderr: " hgs is not installed\n".into(),
            },
        )
        .unwrap_err();
        assert_eq!(failed.code(), "failed");
        assert_eq!(failed.message(), "hgs is not installed");
        let timed_out = decode(&TeamRead::List { after: None }, Exit::TimedOut).unwrap_err();
        assert_eq!(timed_out.code(), "failed");
        assert!(timed_out.message().contains("in time"));
        let invalid = decode(
            &TeamRead::List { after: None },
            Exit::Code {
                code: 0,
                stdout: b"not json".to_vec(),
                stderr: String::new(),
            },
        )
        .unwrap_err();
        assert_eq!(invalid.code(), "invalid_reply");
    }
}
