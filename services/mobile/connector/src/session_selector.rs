//! Explicit phone mutations that also select their session by its durable ID
//! (zerus-ucck P1c-2b). The connector reads the ID from its own listing row
//! for the request's exact run and adds `--session-id ID` after the name, once
//! the CLIs on the call's path list the markers; phones never send it. The CLI
//! then acts only while the name still belongs to that session. P3d also
//! routes fresh auxiliary reads through one captured ID when every CLI on
//! the path advertises complete label support. Original actor status/recovery
//! calls and archived conversations keep their original form.
use serde_json::{Map, Value};

/// The help line of an `hgs` that selects a session by `--session-id`.
pub const LABELS_MARKER: &str = "session-labels-v1";

pub const SELECTOR_MARKER: &str = "session-id-selector-v1";

/// The help line of an `hgs` whose mobile peer entry points accept
/// `--session-id` after the session of the operations below.
pub const PEER_MARKER: &str = "mobile-peer-session-id-v1";

/// Why a selected call failed: the CLI refused the ID before any effect.
pub const CHANGED: &str = "native session changed; refresh before action";

/// The fixed native commands of [`COMMANDS`] by phone operation.
pub const COMMANDS: &[&str] = &[
    "answer",
    "clear_context",
    "compact_context",
    "interrupt",
    "send",
    "send_now",
    "settings",
];

/// The scoped lifecycle actions (`session-action`) that select by ID.
/// `restore` and every archived conversation never do.
pub const LIFECYCLE: &[&str] = &[
    "archive",
    "forget",
    "fork",
    "pause",
    "remove_queued",
    "rename",
    "resume",
    "terminate",
];

/// The CLI exit status for an ID several sessions carry.
const AMBIGUOUS: i32 = 5;

/// What one call selects: the session's name as the call passes it, and its
/// ID after `--session-id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub name: String,
    pub id: String,
}

/// Whether `id` is a session ID as the CLI accepts it: a lowercase,
/// hyphenated, non-nil UUID.
pub fn canonical(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|parsed| !parsed.is_nil() && parsed.to_string() == id)
}

/// Whether a phone operation with this payload selects by ID. Payloads that
/// name an archived conversation never do.
pub fn selects(operation: &str, payload: &Value) -> bool {
    (COMMANDS.contains(&operation) || LIFECYCLE.contains(&operation))
        && payload.get("archive_id").is_none()
}

/// The listed session ID for each live run, from listing rows. Archived rows,
/// rows without a run and IDs that are not canonical are left out.
pub fn index<'a>(
    rows: impl IntoIterator<Item = &'a Map<String, Value>>,
) -> std::collections::HashMap<String, String> {
    let mut runs = std::collections::HashSet::new();
    let mut ambiguous = std::collections::HashSet::new();
    let mut ids = std::collections::HashMap::<String, String>::new();
    let mut result = std::collections::HashMap::new();
    for row in rows
        .into_iter()
        .filter(|row| row.get("state").and_then(Value::as_str) != Some("archived"))
        .filter(|row| row.get("archive_id").is_none())
    {
        let Some(run) = row
            .get("run_id")
            .and_then(Value::as_str)
            .filter(|run| !run.is_empty())
        else {
            continue;
        };
        // Even a row whose ID is unavailable makes a second candidate for this
        // run. It must not disappear and let the other row win by iteration.
        if !runs.insert(run.to_owned()) {
            ambiguous.insert(run.to_owned());
        }
        let Some(id) = row
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|id| canonical(id))
        else {
            continue;
        };
        if let Some(previous) = ids.insert(id.to_owned(), run.to_owned()) {
            ambiguous.insert(previous);
            ambiguous.insert(run.to_owned());
        }
        result.insert(run.to_owned(), id.to_owned());
    }
    result.retain(|run, _| !ambiguous.contains(run));
    result
}

/// `argv` with `--session-id ID` right after the session's name, which
/// follows the native command: `[command, name, --json]`.
pub fn selected_argv(argv: &[String], selection: &Selection) -> Option<Vec<String>> {
    match argv {
        [command, name, json] if *name == selection.name && json == "--json" => Some(vec![
            command.clone(),
            name.clone(),
            "--session-id".into(),
            selection.id.clone(),
            json.clone(),
        ]),
        _ => None,
    }
}

/// Auxiliary reads/metadata for one fresh phone request. This is routing,
/// never an actor-plan rewrite: original status/recovery/effect command forms
/// are deliberately absent. Archive argv and existing selectors stay intact.
pub fn auxiliary_argv(argv: &[String], selection: &Selection) -> Option<Vec<String>> {
    const AUXILIARY: &[&str] = &[
        "inspect",
        "processes",
        "history",
        "history-projection",
        "terminal",
        "hooks",
        "read",
        "notifications",
        "question-routing",
        "file-read",
        "attachment",
        "voice",
    ];
    let [command, name, rest @ ..] = argv else {
        return None;
    };
    if !AUXILIARY.contains(&command.as_str())
        || name != &selection.name
        || !canonical(&selection.id)
        || rest
            .iter()
            .any(|word| matches!(word.as_str(), "--session-id" | "--archive"))
    {
        return None;
    }
    Some(
        [
            vec![
                command.clone(),
                name.clone(),
                "--session-id".into(),
                selection.id.clone(),
            ],
            rest.to_vec(),
        ]
        .concat(),
    )
}

/// Whether a failed selected call is the CLI's refusal of that exact ID before
/// any effect: one the CLI makes while it resolves the ID to a name, before the
/// command runs (`select_session`). Only the whole standard error
/// `hgs: MESSAGE` with one line end, with this exit status, counts:
/// - 1: `Session ID ID is OTHER, not NAME`;
/// - 1: `No live or saved session has ID ID`;
/// - 5: `Several sessions carry ID ID: NAMES`.
///
/// `Session ID ID no longer names NAME; refresh and retry` is not one: a
/// command's later read can also report it after its effect (`kill` forgets
/// after it ends the terminal). Anything else, a mention of uncertain delivery
/// or another ID, name or status included, is not this refusal either.
pub fn refused(selection: &Selection, code: Option<i32>, stderr: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(stderr) else {
        return false;
    };
    let Some(message) = text
        .strip_suffix('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .and_then(|line| line.strip_prefix("hgs: "))
    else {
        return false;
    };
    if message.contains(['\n', '\r']) || message.to_lowercase().contains("uncertain") {
        return false;
    }
    let (name, id) = (selection.name.as_str(), selection.id.as_str());
    match code {
        Some(1) => {
            if message == format!("No live or saved session has ID {id}") {
                return true;
            }
            message
                .strip_prefix(&format!("Session ID {id} is "))
                .and_then(|rest| rest.strip_suffix(&format!(", not {name}")))
                .is_some_and(|other| !other.is_empty() && other != name)
        }
        Some(AMBIGUOUS) => message
            .strip_prefix(&format!("Several sessions carry ID {id}: "))
            .is_some_and(|names| !names.trim().is_empty()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "3f2a9c1d-0b4e-4d6a-9c1d-2e3f4a5b6c7d";
    const OTHER: &str = "0f2a9c1d-0b4e-4d6a-9c1d-2e3f4a5b6c7d";
    const NAME: &str = "codex/example/session";

    fn selection() -> Selection {
        Selection {
            name: NAME.into(),
            id: ID.into(),
        }
    }

    #[test]
    fn only_the_cli_s_pre_effect_refusals_of_this_id_count() {
        let s = selection();
        for (code, stderr) in [
            (
                1,
                format!("hgs: Session ID {ID} is codex/example/renamed, not {NAME}\n"),
            ),
            (1, format!("hgs: No live or saved session has ID {ID}\n")),
            (1, format!("hgs: No live or saved session has ID {ID}\r\n")),
            (5, format!("hgs: Several sessions carry ID {ID}: a, b\n")),
        ] {
            assert!(
                refused(&s, Some(code), stderr.as_bytes()),
                "{code} {stderr}"
            );
        }
    }

    /// Each near miss stays what it was: after a write began, uncertain.
    #[test]
    fn every_near_miss_is_not_a_refusal() {
        let s = selection();
        let near = [
            // Another exit status for each refusal.
            (5, format!("hgs: No live or saved session has ID {ID}\n")),
            (2, format!("hgs: No live or saved session has ID {ID}\n")),
            (1, format!("hgs: Several sessions carry ID {ID}: a, b\n")),
            (2, format!("hgs: Several sessions carry ID {ID}: a, b\n")),
            (3, format!("hgs: Several sessions carry ID {ID}: a, b\n")),
            (5, format!("hgs: Session ID {ID} is x, not {NAME}\n")),
            // Exit 5 with other text.
            (5, "hgs: Ambiguous session name\n".to_owned()),
            (5, format!("hgs: Several sessions carry ID {ID}: \n")),
            // Another ID or name.
            (1, format!("hgs: No live or saved session has ID {OTHER}\n")),
            (1, format!("hgs: Session ID {OTHER} is x, not {NAME}\n")),
            (
                1,
                format!("hgs: Session ID {ID} is x, not codex/example/other\n"),
            ),
            (1, format!("hgs: Session ID {ID} is {NAME}, not {NAME}\n")),
            (
                1,
                format!(
                    "hgs: Session ID {ID} no longer names codex/example/other; refresh and retry\n"
                ),
            ),
            // Uncertain delivery, anywhere in the text.
            (
                1,
                format!("hgs: delivery uncertain: No live or saved session has ID {ID}\n"),
            ),
            (
                1,
                format!("hgs: Session ID {ID} is delivery uncertain, not {NAME}\n"),
            ),
            // More output than the one refusal line.
            (
                1,
                format!("warning: x\nhgs: No live or saved session has ID {ID}\n"),
            ),
            (
                1,
                format!("hgs: No live or saved session has ID {ID}\nmore\n"),
            ),
            (1, format!("hgs: No live or saved session has ID {ID}")),
            (1, format!("No live or saved session has ID {ID}\n")),
            (1, format!("hgs: No live or saved session has ID {ID} \n")),
            // The read-time refusal, which a command can report after its
            // effect, and a refusal its command wrapped.
            (
                1,
                format!("hgs: Session ID {ID} no longer names {NAME}; refresh and retry\n"),
            ),
            (
                1,
                format!(
                    "hgs: settings change not confirmed: No live or saved session has ID {ID}\n"
                ),
            ),
            // Other failures.
            (1, "hgs: native command failed\n".to_owned()),
            (1, String::new()),
        ];
        for (code, stderr) in near {
            assert!(
                !refused(&s, Some(code), stderr.as_bytes()),
                "{code} {stderr:?}"
            );
        }
        // A signal, or output that is not text.
        let text = format!("hgs: No live or saved session has ID {ID}\n");
        assert!(!refused(&s, None, text.as_bytes()));
        assert!(!refused(&s, Some(1), b"hgs: \xff\n"));
    }

    #[test]
    fn the_flag_follows_the_name_of_a_fixed_scoped_command_only() {
        let s = selection();
        let argv = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(
            selected_argv(&argv(&["send", NAME, "--json"]), &s).unwrap(),
            argv(&["send", NAME, "--session-id", ID, "--json"])
        );
        assert_eq!(
            selected_argv(&argv(&["session-action", NAME, "--json"]), &s).unwrap(),
            argv(&["session-action", NAME, "--session-id", ID, "--json"])
        );
        for other in [
            argv(&["send", "codex/example/other", "--json"]),
            argv(&["send", NAME]),
            argv(&["processes", NAME, "--stop", "p"]),
        ] {
            assert_eq!(selected_argv(&other, &s), None, "{other:?}");
        }
    }

    #[test]
    fn only_listed_explicit_mutations_select() {
        for operation in COMMANDS.iter().chain(LIFECYCLE) {
            assert!(selects(operation, &json!({})), "{operation}");
            assert!(
                !selects(operation, &json!({"archive_id": "a"})),
                "{operation}"
            );
        }
        for operation in [
            "restore",
            "process_stop",
            "recovery_action",
            "set_mobile_delivery",
            "mark_read",
            "history",
            "inspect",
            "terminal_input",
            "session_notifications",
        ] {
            assert!(!selects(operation, &json!({})), "{operation}");
        }
    }

    #[test]
    fn the_index_keeps_live_runs_with_canonical_ids() {
        let rows = [
            json!({"name": "a", "run_id": "run-a", "session_id": ID}),
            json!({"name": "b", "run_id": "run-b", "session_id": ID.to_uppercase()}),
            json!({"name": "c", "run_id": "", "session_id": OTHER}),
            json!({"name": "d", "session_id": OTHER}),
            json!({"name": "e", "run_id": "run-e", "session_id": OTHER, "state": "archived"}),
            json!({"name": "f", "run_id": "run-f", "session_id": OTHER, "archive_id": "x"}),
            json!({"name": "g", "run_id": "run-g"}),
            json!({"name": "h", "run_id": "run-h", "session_id": "00000000-0000-0000-0000-000000000000"}),
        ];
        let index = index(rows.iter().filter_map(Value::as_object));
        assert_eq!(index.len(), 1);
        assert_eq!(index.get("run-a").map(String::as_str), Some(ID));
    }

    #[test]
    fn shared_labels_keep_distinct_ids_but_duplicate_run_or_id_evidence_never_wins() {
        let rows = [
            json!({"name":NAME,"run_id":"one","session_id":ID}),
            json!({"name":NAME,"run_id":"two","session_id":OTHER}),
        ];
        assert_eq!(index(rows.iter().filter_map(Value::as_object)).len(), 2);
        for second in [
            json!({"name":NAME,"run_id":"one","session_id":OTHER}),
            json!({"name":NAME,"run_id":"two","session_id":ID}),
            json!({"name":NAME,"run_id":"one","session_id":null}),
            rows[0].clone(),
        ] {
            for reverse in [false, true] {
                let mut candidates = [rows[0].clone(), second.clone()];
                if reverse {
                    candidates.reverse();
                }
                assert!(index(candidates.iter().filter_map(Value::as_object)).is_empty());
            }
        }
    }
}
