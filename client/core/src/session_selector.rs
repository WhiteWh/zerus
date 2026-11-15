//! Explicit desktop actions that also select their session by its durable ID
//! (zerus-ucck P1c). The acted-on row's `session_id` follows the name as
//! `--session-id ID` once that computer's `hgs` lists [`MARKER`]; the CLI then
//! acts only while the name still belongs to that session. Reads, polls,
//! history, status, watch, terminal input and read marks keep their name and
//! run form until an O(1) label index exists.
use crate::actions::{invocation, prepend_alias, Action, Exit, Invocation, Target};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The help line of an `hgs` that selects a session by `--session-id`.
pub const MARKER: &str = "session-id-selector-v1";

/// How long one computer's answer to [`probe`] is reused.
pub const ANSWER_TTL: Duration = Duration::from_secs(600);

/// At most this many computers' answers are kept.
const ANSWERS: usize = 64;

/// What the CLI's peer gate prints when that peer's `hgs` cannot select by ID
/// (`gated` in `src/cli.rs`).
const UNSUPPORTED: &str = "update the selected computer to select sessions by ID";

/// The CLI's exit status for an ID several sessions carry.
const AMBIGUOUS: i32 = 5;

/// `hgs [@computer] --help`, which shows whether that computer's `hgs` lists
/// [`MARKER`]: 5 s here, 30 s on a peer. No input.
pub fn probe(computer: &str) -> Invocation {
    let mut args = vec!["--help".to_owned()];
    let peer = prepend_alias(&mut args, computer);
    Invocation {
        args,
        stdin: None,
        timeout: Duration::from_secs(if peer { 30 } else { 5 }),
        request_id: None,
    }
}

/// Whether a probe exited 0 with [`MARKER`] as a whole line once surrounding
/// ASCII whitespace is trimmed, as the CLI's own peer gate reads it.
pub fn confirmed(exit: &Exit) -> bool {
    matches!(exit, Exit::Code { code: 0, stdout, .. } if stdout
        .split(|&b| b == b'\n')
        .any(|line| line.trim_ascii() == MARKER.as_bytes()))
}

/// The explicit actions that select their session by ID as well. Reads, read
/// marks, staged files, process stops and archived conversations never do, nor
/// do end actions: those run only from a capture whose scoped envelope names
/// the run and conversation (`end_action::needs_capture`).
pub fn selects(action: &Action) -> bool {
    match action {
        Action::Send { .. }
        | Action::SendNow { .. }
        | Action::Answer { .. }
        | Action::RemoveQueued { .. }
        | Action::ClearContext { .. }
        | Action::CompactContext
        | Action::Interrupt { .. }
        | Action::Settings { .. }
        | Action::Pause
        | Action::Resume
        | Action::StartFresh => true,
        Action::Rename { archive_id, .. } | Action::Fork { archive_id, .. } => {
            archive_id.as_deref().unwrap_or_default().is_empty()
        }
        _ => false,
    }
}

/// Where `--session-id` goes: right after the session's name, which follows
/// the command and, on a peer, its `@alias`.
fn after_name(target: &Target) -> usize {
    usize::from(!target.computer.is_empty()) + 2
}

/// [`invocation`], selecting the session by its ID as well. `session_id` is
/// the acted-on row's session ID, given only once that computer's `hgs`
/// confirmed [`MARKER`]. An action that [`selects`] then carries
/// `--session-id ID` after the name, which the CLI requires to be that
/// session's current name; any other call keeps its name and run and
/// conversation form.
pub fn selected_invocation(
    target: &Target,
    action: &Action,
    request_id: &str,
    session_id: Option<&str>,
) -> Result<Invocation, String> {
    let mut call = invocation(target, action, request_id)?;
    if let Some(id) = session_id.filter(|id| selects(action) && crate::creation::canonical_uuid(id))
    {
        let at = after_name(target);
        if call.args.get(at - 1) == Some(&target.name) {
            call.args
                .splice(at..at, ["--session-id".to_owned(), id.to_owned()]);
        }
    }
    Ok(call)
}

/// The session ID a call for `target` selects by, as [`selected_invocation`]
/// placed it.
pub fn selected_id<'a>(target: &Target, call: &'a Invocation) -> Option<&'a str> {
    let at = after_name(target);
    (call.args.get(at - 1) == Some(&target.name)
        && call.args.get(at).is_some_and(|flag| flag == "--session-id"))
    .then(|| call.args.get(at + 1).map(String::as_str))
    .flatten()
}

/// Whether the CLI refused the call's `--session-id` before any effect, while
/// it resolved the ID to a name and before the command ran
/// (`select_session`): the ID now names another session, no session or
/// several. The caller treats it as a stale target; nothing retries it.
///
/// Only the whole standard error `hgs: MESSAGE` with one line end, naming
/// this ID and the target's name, with this exit status, counts:
/// - 1: `Session ID ID is OTHER, not NAME`;
/// - 1: `No live or saved session has ID ID`;
/// - 5: `Several sessions carry ID ID: NAMES`.
///
/// `Session ID ID no longer names NAME; refresh and retry` keeps the action's
/// own classification: a command's later read can report it after its effect
/// (`kill` forgets after it ends the terminal). So does a refusal a command
/// wrapped, such as Settings' "settings change not confirmed: ...".
pub fn refused(target: &Target, call: &Invocation, exit: &Exit) -> bool {
    let Some(id) = selected_id(target, call) else {
        return false;
    };
    let Exit::Code { code, stderr, .. } = exit else {
        return false;
    };
    let Some(message) = stderr
        .strip_suffix('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .and_then(|line| line.strip_prefix("hgs: "))
    else {
        return false;
    };
    if message.contains(['\n', '\r']) || message.to_lowercase().contains("uncertain") {
        return false;
    }
    let name = target.name.as_str();
    match *code {
        1 => {
            message == format!("No live or saved session has ID {id}")
                || message
                    .strip_prefix(&format!("Session ID {id} is "))
                    .and_then(|rest| rest.strip_suffix(&format!(", not {name}")))
                    .is_some_and(|other| !other.is_empty() && other != name)
        }
        AMBIGUOUS => message
            .strip_prefix(&format!("Several sessions carry ID {id}: "))
            .is_some_and(|names| !names.trim().is_empty()),
        _ => false,
    }
}

/// Whether a call that selected by ID met a peer whose `hgs` no longer
/// selects by ID, so the kept answer for that computer is out of date.
pub fn unsupported(target: &Target, call: &Invocation, exit: &Exit) -> bool {
    selected_id(target, call).is_some()
        && matches!(exit, Exit::Code { code: 1, stderr, .. } if stderr.contains(UNSUPPORTED))
}

/// Each computer's answer to [`probe`] through one `hgs`, reused for
/// [`ANSWER_TTL`]. A probe that did not exit 0 is not kept, so the next call
/// asks again.
#[derive(Debug, Default)]
pub struct Answers(HashMap<(PathBuf, String), (Instant, bool)>);

impl Answers {
    /// The kept answer for `computer` through `hgs`, while it is fresh.
    pub fn get(&self, hgs: &Path, computer: &str, now: Instant) -> Option<bool> {
        self.0
            .get(&(hgs.to_path_buf(), computer.to_owned()))
            .filter(|(at, _)| now.saturating_duration_since(*at) < ANSWER_TTL)
            .map(|(_, answer)| *answer)
    }

    /// Keeps a finished probe's answer and returns whether it confirmed
    /// [`MARKER`]. Beyond [`ANSWERS`] computers the stale and then the oldest
    /// answers go.
    pub fn record(&mut self, hgs: &Path, computer: &str, exit: &Exit, now: Instant) -> bool {
        let answer = confirmed(exit);
        if !matches!(exit, Exit::Code { code: 0, .. }) {
            return answer;
        }
        let key = (hgs.to_path_buf(), computer.to_owned());
        if !self.0.contains_key(&key) && self.0.len() >= ANSWERS {
            self.0
                .retain(|_, (at, _)| now.saturating_duration_since(*at) < ANSWER_TTL);
            while self.0.len() >= ANSWERS {
                let Some(oldest) = self
                    .0
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                self.0.remove(&oldest);
            }
        }
        self.0.insert(key, (now, answer));
        answer
    }

    /// Drops the answer for `computer`, so the next call reads its help again.
    pub fn forget(&mut self, hgs: &Path, computer: &str) {
        self.0.remove(&(hgs.to_path_buf(), computer.to_owned()));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::classify;
    use crate::Outcome;
    use serde_json::Value;

    const ID: &str = "3f2a9c1d-0b4e-4d6a-9c1d-2e3f4a5b6c7d";

    fn target(computer: &str) -> Target {
        Target {
            computer: computer.into(),
            name: "claude/sample-project/review".into(),
            run_id: "run-1".into(),
            conversation_id: "conv-1".into(),
        }
    }

    fn exit(code: i32, stdout: &[u8], stderr: &str) -> Exit {
        Exit::Code {
            code,
            stdout: stdout.to_vec(),
            stderr: stderr.into(),
        }
    }

    fn send() -> Action {
        Action::Send {
            text: "hi".into(),
            attachments: vec![],
            agent_id: None,
            expected_compaction_id: None,
            queue_when_unconfirmed: false,
        }
    }

    fn selecting() -> Vec<Action> {
        vec![
            send(),
            Action::SendNow {
                queue_id: "queue-1".into(),
            },
            Action::ClearContext {
                dismiss_prompt: false,
            },
            Action::CompactContext,
            Action::Interrupt { turn_started: 1.0 },
            Action::Settings {
                model: "model-a".into(),
                effort: String::new(),
                expected_pending_id: None,
                without_terminal: false,
            },
            Action::Pause,
            Action::Resume,
            Action::StartFresh,
            Action::Rename {
                new_name: "claude/sample-project/renamed".into(),
                archive_id: None,
            },
            Action::Fork {
                tag: "fork".into(),
                archive_id: None,
            },
        ]
    }

    /// An explicit action carries the confirmed session ID right after the
    /// name, here and on a peer; nothing else about the call changes.
    #[test]
    fn a_confirmed_session_id_follows_the_name_on_explicit_actions_only() {
        for computer in ["", "mac"] {
            let t = target(computer);
            for action in &selecting() {
                assert!(selects(action), "{action:?}");
                let plain = invocation(&t, action, "req-1").unwrap();
                let call = selected_invocation(&t, action, "req-1", Some(ID)).unwrap();
                let at = usize::from(!computer.is_empty()) + 2;
                let mut expected = plain.args.clone();
                expected.splice(at..at, ["--session-id".to_owned(), ID.to_owned()]);
                assert_eq!(call.args, expected, "{action:?}");
                assert_eq!(call.args[at - 1], t.name, "{action:?}");
                assert_eq!(selected_id(&t, &call), Some(ID), "{action:?}");
                assert_eq!(selected_id(&t, &plain), None, "{action:?}");
                assert_eq!(
                    (call.stdin, call.timeout, call.request_id),
                    (plain.stdin, plain.timeout, plain.request_id),
                    "{action:?}"
                );
                // Without a confirmed ID, or with a malformed one, the call keeps its old form.
                for absent in [
                    None,
                    Some(""),
                    Some("3F2A9C1D-0B4E-4D6A-9C1D-2E3F4A5B6C7D"),
                    Some("not-an-id"),
                ] {
                    let kept = selected_invocation(&t, action, "req-1", absent).unwrap();
                    assert_eq!(kept.args, plain.args, "{action:?} {absent:?}");
                }
            }
        }
        assert!(selects(&Action::RemoveQueued {
            message_id: "m".into()
        }));
        assert!(selects(&Action::Answer {
            question: Value::Null,
            answers: vec![]
        }));
    }

    /// Reads, read marks, archived conversations and other calls never select
    /// by ID, even when a session ID is offered.
    #[test]
    fn reads_marks_and_archived_conversations_keep_their_form() {
        for action in [
            Action::MarkRead {
                reply_id: None,
                attention_signature: None,
            },
            Action::StopProcess {
                process_id: "p".into(),
                archive_id: None,
                generation: None,
            },
            Action::Rename {
                new_name: "claude/sample-project/renamed".into(),
                archive_id: Some("a".into()),
            },
            Action::Fork {
                tag: "fork".into(),
                archive_id: Some("a".into()),
            },
            Action::Restore {
                archive_id: "a".into(),
            },
            Action::ForgetArchive {
                archive_id: "a".into(),
            },
            Action::ForgetBinding,
            Action::StageFiles {
                paths: vec![],
                expected_created: None,
            },
            Action::PauseAll { count: 2 },
            Action::ResumeAll { count: 2 },
            // End actions run only from a capture (zerus-sa9qn.25).
            Action::Archive,
            Action::Terminate,
            Action::Kill,
            Action::Forget,
        ] {
            assert!(!selects(&action), "{action:?}");
            for computer in ["", "mac"] {
                if let Ok(plain) = invocation(&target(computer), &action, "req-1") {
                    let call =
                        selected_invocation(&target(computer), &action, "req-1", Some(ID)).unwrap();
                    assert_eq!(call.args, plain.args, "{action:?}");
                }
            }
        }
    }

    #[test]
    fn the_probe_reads_the_computer_s_own_help() {
        assert_eq!(probe("").args, ["--help"]);
        assert_eq!(probe("").timeout, Duration::from_secs(5));
        assert_eq!(probe("mac").args, ["@mac", "--help"]);
        assert_eq!(probe("mac").timeout, Duration::from_secs(30));
        assert!(probe("mac").stdin.is_none() && probe("mac").request_id.is_none());
        assert!(confirmed(&exit(
            0,
            b"usage\n  session-id-selector-v1\r\nmore\n",
            ""
        )));
        for refused in [
            exit(0, b"usage\n  session-id-selector-v10\n", ""),
            exit(0, b"usage: session-id-selector-v1 soon\n", ""),
            exit(1, b"session-id-selector-v1\n", ""),
            Exit::Abnormal {
                stderr: String::new(),
            },
            Exit::TimedOut,
        ] {
            assert!(!confirmed(&refused), "{refused:?}");
        }
    }

    /// The CLI's refusals of the selected ID are a stale target: the call
    /// failed before any effect, whatever the action's own classification.
    #[test]
    fn a_refused_session_id_is_a_stale_target() {
        let changed = Outcome::Failed {
            detail: crate::termination_cleanup::STATE_CHANGED.into(),
        };
        for computer in ["", "mac"] {
            let t = target(computer);
            for action in selecting() {
                let call = selected_invocation(&t, &action, "req-1", Some(ID)).unwrap();
                for (code, stderr) in [
                    (
                        1,
                        format!(
                            "hgs: Session ID {ID} is claude/sample-project/renamed, not {}\n",
                            t.name
                        ),
                    ),
                    (1, format!("hgs: No live or saved session has ID {ID}\n")),
                    (5, format!("hgs: Several sessions carry ID {ID}: a, b\n")),
                ] {
                    let refusal = exit(code, b"", &stderr);
                    assert!(refused(&t, &call, &refusal), "{action:?} {stderr}");
                    assert_eq!(classify(&t, &action, &call, refusal), changed, "{action:?}");
                }
                // Another ID, another status, or a possible delivery is not this refusal.
                let other = "0f2a9c1d-0b4e-4d6a-9c1d-2e3f4a5b6c7d";
                for (code, stderr) in [
                    (1, format!("hgs: No live or saved session has ID {other}\n")),
                    (2, format!("hgs: No live or saved session has ID {ID}\n")),
                    (5, format!("hgs: No live or saved session has ID {ID}\n")),
                    (1, format!("hgs: Several sessions carry ID {ID}: a, b\n")),
                    (2, format!("hgs: Several sessions carry ID {ID}: a, b\n")),
                    (3, format!("hgs: Several sessions carry ID {ID}: a, b\n")),
                    (5, "hgs: Ambiguous session name\n".to_owned()),
                    (
                        1,
                        format!("hgs: delivery uncertain; Session ID {ID} is x, not y\n"),
                    ),
                    (
                        1,
                        format!("hgs: Session ID {ID} is x, not claude/sample-project/other\n"),
                    ),
                    // The read-time refusal, which a command's later read can
                    // report after its effect (kill, then forget).
                    (
                        1,
                        format!(
                            "hgs: Session ID {ID} no longer names {}; refresh and retry\n",
                            t.name
                        ),
                    ),
                    // A refusal a command wrapped, such as Settings.
                    (
                        1,
                        format!(
                            "hgs: settings change not confirmed: No live or saved session has ID {ID}\n"
                        ),
                    ),
                    // More than the one refusal line, or no line end.
                    (
                        1,
                        format!("note: x\nhgs: No live or saved session has ID {ID}\n"),
                    ),
                    (1, format!("hgs: No live or saved session has ID {ID}")),
                    (1, format!("No live or saved session has ID {ID}\n")),
                    (1, "hgs: the agent is busy\n".to_owned()),
                ] {
                    let near = exit(code, b"", &stderr);
                    assert!(!refused(&t, &call, &near), "{action:?} {stderr}");
                    // The action keeps its own classification.
                    let plain = invocation(&t, &action, "req-1").unwrap();
                    assert_eq!(
                        classify(&t, &action, &call, near.clone()),
                        classify(&t, &action, &plain, near),
                        "{action:?} {stderr}"
                    );
                }
                assert!(!refused(&t, &call, &Exit::TimedOut));
                // Without the selector, the same text is the action's own failure.
                let plain = invocation(&t, &action, "req-1").unwrap();
                let text = format!("hgs: No live or saved session has ID {ID}\n");
                assert!(!refused(&t, &plain, &exit(1, b"", &text)));
            }
        }
    }

    #[test]
    fn a_peer_that_lost_the_selector_is_recognised() {
        let t = target("mac");
        let call = selected_invocation(&t, &Action::Pause, "req-1", Some(ID)).unwrap();
        let gate = exit(
            1,
            b"",
            "hgs: update the selected computer to select sessions by ID\n",
        );
        assert!(unsupported(&t, &call, &gate));
        assert!(!unsupported(&t, &call, &exit(1, b"", "hgs: offline\n")));
        let plain = invocation(&t, &Action::Pause, "req-1").unwrap();
        assert!(!unsupported(&t, &plain, &gate));
    }

    #[test]
    fn answers_are_kept_per_computer_and_cli_for_a_bounded_time() {
        let (hgs, other) = (
            Path::new("/opt/example/hgs"),
            Path::new("/opt/example/hgs-2"),
        );
        let start = Instant::now();
        let mut answers = Answers::default();
        assert_eq!(answers.get(hgs, "mac", start), None);
        let yes = exit(0, b"session-id-selector-v1\n", "");
        let no = exit(0, b"usage\n", "");
        assert!(answers.record(hgs, "mac", &yes, start));
        assert!(!answers.record(hgs, "", &no, start));
        assert_eq!(answers.get(hgs, "mac", start), Some(true));
        assert_eq!(answers.get(hgs, "", start), Some(false));
        assert_eq!(answers.get(other, "mac", start), None);
        let later = start + ANSWER_TTL - Duration::from_secs(1);
        assert_eq!(answers.get(hgs, "mac", later), Some(true));
        assert_eq!(answers.get(hgs, "mac", start + ANSWER_TTL), None);
        // An unfinished probe answers no and is not kept; a kept answer stays.
        for unfinished in [exit(1, b"session-id-selector-v1\n", ""), Exit::TimedOut] {
            assert!(!answers.record(hgs, "studio", &unfinished, start));
            assert_eq!(answers.get(hgs, "studio", start), None);
        }
        answers.forget(hgs, "mac");
        assert_eq!(answers.get(hgs, "mac", start), None);
        // Bounded: beyond the limit the oldest answers go.
        for n in 0..ANSWERS + 10 {
            let at = start + Duration::from_millis(n as u64);
            answers.record(hgs, &format!("peer-{n}"), &yes, at);
        }
        assert_eq!(answers.len(), ANSWERS);
        let last = start + Duration::from_millis((ANSWERS + 9) as u64);
        assert_eq!(answers.get(hgs, "peer-0", last), None);
        assert_eq!(
            answers.get(hgs, &format!("peer-{}", ANSWERS + 9), last),
            Some(true)
        );
    }
}
