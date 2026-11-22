use super::intents::{Admission, IntentState, Intents};
use super::types::*;
// The helper-script tests run on Unix only.
#[cfg(unix)]
use super::wire::run_action;
use super::wire::{classify, prepare};
use crate::actions::{Exit, Invocation};
use serde_json::{json, Value};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use zerus_team_plan::model::Integration;

fn a_call(team: &str, revision: u64, action: TeamAction) -> TeamCall {
    TeamCall {
        team: team.to_owned(),
        team_name: "Fixes".into(),
        action,
        expected_revision: Some(revision),
        confirmed: false,
        request_id: "11111111-2222-4333-8444-555555555555".into(),
    }
}

/// The bugfix template's digest in `crates/team-plan/fixtures/cli-catalog.json`.
const DIGEST: &str = "6d15fdbdd92ac4d5c5bc53f0bd6da7871c0dbf1842f4d4f25bf94aebc95bbb9d";

/// The permissions a form shows, merge_accepted switched off.
fn shown() -> CreateDelegations {
    CreateDelegations {
        accept_results: true,
        answer_members: true,
        confirm_trust: true,
        resume_exited: true,
        merge_accepted: false,
    }
}

fn create(revision: Option<u64>, integration: Integration) -> TeamCall {
    TeamCall {
        team: "00000000-0000-4000-8000-00000000cafe".into(),
        team_name: String::new(),
        action: TeamAction::Create {
            template: "bugfix".into(),
            project: "p".into(),
            name: "Fixes".into(),
            goal: "Ship it".into(),
            home: "00000000-0000-4000-8000-0000000000aa".into(),
            integration_branch: if integration == Integration::Branch {
                "zerus/team/fixes".into()
            } else {
                String::new()
            },
            base_commit: if integration == Integration::Branch {
                "0123456789abcdef0123456789abcdef01234567".into()
            } else {
                String::new()
            },
            integration,
            expected_template: DIGEST.into(),
            delegations: shown(),
        },
        expected_revision: revision,
        confirmed: false,
        request_id: "11111111-2222-4333-8444-555555555555".into(),
    }
}

fn stdin_of(invocation: &Invocation) -> Value {
    serde_json::from_slice(invocation.stdin.as_deref().unwrap()).unwrap()
}

const TEAM: &str = "00000000-0000-4000-8000-00000000beef";
const ID: &str = "11111111-2222-4333-8444-555555555555";

#[test]
fn draft_actions_keep_the_original_revision_generation_and_node_input() {
    let row: Value = serde_json::from_str(include_str!(
        "../../../../../crates/team-plan/fixtures/cli-phase-draft.json"
    ))
    .unwrap();
    let draft = zerus_team_plan::draft::DraftInput {
        title: row["draft"]["phase"]["title"].as_str().unwrap().into(),
        barrier: true,
        nodes: serde_json::from_value(row["draft"]["nodes"].clone()).unwrap(),
    };
    for (action, op, generation) in [
        (
            TeamAction::SubmitDraftPhase {
                phase: 1,
                draft: draft.clone(),
            },
            "submit-draft-phase",
            None,
        ),
        (
            TeamAction::EditDraftPhase {
                phase: 1,
                generation: 7,
                draft: draft.clone(),
            },
            "edit-draft-phase",
            Some(7),
        ),
    ] {
        let call = a_call(TEAM, 12, action);
        let (invocation, id) = prepare(&call).unwrap();
        assert_eq!(id, ID);
        assert_eq!(invocation.args, ["team", op, "--json"]);
        let input = stdin_of(&invocation);
        assert_eq!(input["request_id"], ID);
        assert_eq!(input["expected_revision"], 12);
        assert_eq!(input["phase"], 1);
        assert_eq!(input.get("generation").and_then(Value::as_u64), generation);
        assert_eq!(input["draft"], serde_json::to_value(&draft).unwrap());
        assert!(input["draft"]["nodes"][0].get("task").is_none());
    }
    let call = a_call(TEAM, 12, TeamAction::SubmitDraftPhase { phase: 0, draft });
    assert!(matches!(prepare(&call), Err(TeamOutcome::Failed { .. })));
}

fn receipt() -> Value {
    json!({"ok": true, "revision": 17, "actor": {"kind": "person"}, "team": TEAM})
}

#[test]
fn every_action_names_its_operation() {
    let cases: Vec<(TeamAction, &str)> = vec![
        (
            TeamAction::AddMember {
                role: "developer".into(),
                agent: "codex".into(),
                model: "".into(),
                effort: "".into(),
                account: Default::default(),
            },
            "add-member",
        ),
        (
            TeamAction::SwapMember {
                member: "m1".into(),
                agent: "codex".into(),
                model: "".into(),
                effort: "".into(),
                account: Default::default(),
            },
            "swap-member",
        ),
        (
            TeamAction::RenameTeam {
                name: "New name".into(),
            },
            "rename-team",
        ),
        (
            TeamAction::SetGoal {
                goal: "New goal".into(),
            },
            "set-goal",
        ),
        (
            TeamAction::Create {
                template: "bugfix".into(),
                project: "p".into(),
                name: "Fixes".into(),
                goal: String::new(),
                home: "00000000-0000-4000-8000-0000000000aa".into(),
                integration_branch: "zerus/team/fixes".into(),
                base_commit: "0123456789abcdef0123456789abcdef01234567".into(),
                integration: Integration::Branch,
                expected_template: DIGEST.into(),
                delegations: shown(),
            },
            "create",
        ),
        (TeamAction::Pause {}, "pause"),
        (TeamAction::Resume {}, "resume"),
        (TeamAction::Stop {}, "stop"),
        (TeamAction::Archive {}, "archive"),
        (
            TeamAction::PauseMember {
                member: "m1".into(),
            },
            "pause-member",
        ),
        (
            TeamAction::ResumeMember {
                member: "m1".into(),
            },
            "resume-member",
        ),
        (
            TeamAction::StopMember {
                member: "m1".into(),
            },
            "stop-member",
        ),
        (
            TeamAction::ArchiveMember {
                member: "m1".into(),
            },
            "archive-member",
        ),
        (
            TeamAction::Settings {
                settings: zerus_team_plan::model::Settings::default(),
            },
            "settings",
        ),
        (
            TeamAction::Decide {
                card: 3,
                decision: "accept".into(),
            },
            "decide",
        ),
        (TeamAction::Admit { node: 2 }, "admit"),
        (
            TeamAction::Route {
                node: 2,
                role: "developer".into(),
                pipeline: "reviewed".into(),
                keep_task_fields: false,
                deps: Vec::new(),
                writes: Vec::new(),
                assignee: None,
            },
            "route",
        ),
        (TeamAction::Accept { node: 2 }, "accept"),
        (
            TeamAction::Return {
                node: 2,
                reason: "Missing a test".into(),
            },
            "return",
        ),
        (
            TeamAction::Reassign {
                node: 2,
                member: "m2".into(),
                reason: "M2 has the context".into(),
            },
            "reassign",
        ),
        (TeamAction::Hold { node: 2 }, "hold"),
        (TeamAction::Release { node: 2 }, "release"),
        (
            TeamAction::Cancel {
                node: 2,
                reason: "No longer needed".into(),
            },
            "cancel",
        ),
        (
            TeamAction::Reply {
                item: 4,
                text: "Go ahead".into(),
            },
            "reply",
        ),
        (
            TeamAction::Ask {
                node: 2,
                generation: 1,
                text: "Which branch?".into(),
            },
            "ask",
        ),
    ];
    // `src/teams/cli/commands.rs` at package 5's fa46dcc08, plus the four
    // member commands its fix round adds.
    const COMMANDS: [&str; 25] = [
        "add-member",
        "swap-member",
        "rename-team",
        "set-goal",
        "settings",
        "pause",
        "resume",
        "stop",
        "archive",
        "decide",
        "admit",
        "route",
        "accept",
        "return",
        "reassign",
        "hold",
        "release",
        "cancel",
        "reply",
        "ask",
        "create",
        "pause-member",
        "resume-member",
        "stop-member",
        "archive-member",
    ];
    for (action, op) in cases {
        assert_eq!(action.op(), op);
        assert!(COMMANDS.contains(&op), "{op} has no hgs team command");
        assert!(action.check().is_ok(), "{op}: {:?}", action.check());
    }
    assert!(
        TeamAction::Pause {}.names_team(),
        "a team action names its team"
    );
    assert!(
        !TeamAction::Create {
            template: "bugfix".into(),
            project: "p".into(),
            name: "Fixes".into(),
            goal: String::new(),
            home: "00000000-0000-4000-8000-0000000000aa".into(),
            integration_branch: String::new(),
            base_commit: String::new(),
            integration: Integration::None,
            expected_template: DIGEST.into(),
            delegations: shown(),
        }
        .names_team(),
        "a create names the team it makes, not one it changes"
    );
}

#[test]
fn a_call_carries_its_identity_and_revision() {
    let call = a_call(TEAM, 42, TeamAction::Accept { node: 7 });
    let (invocation, request_id) = prepare(&call).unwrap();
    assert_eq!(request_id, ID);
    assert_eq!(invocation.args, ["team", "accept", "--json"]);
    assert_eq!(invocation.request_id.as_deref(), Some(request_id.as_str()));
    let stdin = stdin_of(&invocation);
    assert_eq!(stdin["request_id"], request_id);
    assert_eq!(stdin["team"], TEAM);
    assert_eq!(stdin["expected_revision"], 42);
    assert_eq!(stdin["node"], 7);
    assert!(
        stdin.get("by").is_none(),
        "the request never names its actor"
    );
}

#[test]
fn one_intent_keeps_its_request_id() {
    // The id is the call's, made once by the person's action: preparing the
    // same call again never changes it.
    let call = a_call(TEAM, 42, TeamAction::Hold { node: 1 });
    let (_, first) = prepare(&call).unwrap();
    let (_, second) = prepare(&call).unwrap();
    assert_eq!(first, ID);
    assert_eq!(second, first, "the core never replaces a request id");
    // A call without one cannot run; the wire shape requires it.
    let mut missing = call.clone();
    missing.request_id = String::new();
    assert!(matches!(prepare(&missing), Err(TeamOutcome::Failed { .. })));
    let json = serde_json::to_value(&call).unwrap();
    let mut without = json.clone();
    without.as_object_mut().unwrap().remove("request_id");
    assert!(
        serde_json::from_value::<TeamCall>(without).is_err(),
        "a call must name its intent"
    );
    missing.request_id = "not-a-uuid".into();
    assert!(matches!(prepare(&missing), Err(TeamOutcome::Failed { .. })));
}

#[test]
fn a_person_confirms_a_destructive_action_first() {
    for action in [
        TeamAction::Stop {},
        TeamAction::Archive {},
        TeamAction::StopMember {
            member: "m1".into(),
        },
        TeamAction::ArchiveMember {
            member: "m1".into(),
        },
        TeamAction::Cancel {
            node: 3,
            reason: "Not needed".into(),
        },
    ] {
        let asked = prepare(&a_call(TEAM, 42, action.clone())).unwrap_err();
        let TeamOutcome::Confirm { confirmation } = asked else {
            panic!("{action:?} asks first");
        };
        assert!(!confirmation.text.is_empty());
        assert!(!confirmation.action.is_empty());
        assert!(confirmation.text.contains("Fixes"));
        let mut confirmed = a_call(TEAM, 42, action.clone());
        confirmed.confirmed = true;
        assert!(prepare(&confirmed).is_ok(), "{action:?} runs confirmed");
    }
    assert!(prepare(&a_call(TEAM, 42, TeamAction::Pause {})).is_ok());
}

#[test]
fn a_change_needs_the_revision_it_was_read_at() {
    let mut call = a_call(TEAM, 42, TeamAction::Accept { node: 7 });
    call.expected_revision = None;
    let refused = prepare(&call).unwrap_err();
    assert!(matches!(refused, TeamOutcome::Failed { .. }));
    // A new team names its own id and compares revision zero.
    let call = create(None, Integration::Branch);
    let (invocation, _) = prepare(&call).unwrap();
    assert_eq!(
        invocation.args,
        ["team", "create", "--template", "bugfix", "--json"]
    );
    let stdin = stdin_of(&invocation);
    assert_eq!(stdin["team"], "00000000-0000-4000-8000-00000000cafe");
    assert_eq!(stdin["expected_revision"], 0);
    assert_eq!(stdin["project"], "p");
    assert_eq!(stdin["name"], "Fixes");
    assert_eq!(stdin["goal"], "Ship it");
    assert_eq!(stdin["home"], "00000000-0000-4000-8000-0000000000aa");
    assert_eq!(stdin["integration_branch"], "zerus/team/fixes");
    assert_eq!(
        stdin["base_commit"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert!(
        stdin.get("integration").is_none(),
        "the template's mode is the client's check, not the store's field"
    );
}

#[test]
fn create_checks_the_branch_only_for_a_branch_template() {
    // A no-integration template needs neither a branch nor a base commit.
    assert!(prepare(&create(None, Integration::None)).is_ok());
    let mut none = create(None, Integration::None);
    if let TeamAction::Create {
        integration_branch, ..
    } = &mut none.action
    {
        *integration_branch = "still/a/branch".into();
    }
    assert!(prepare(&none).is_ok(), "a named branch is still checked");
    // A branch template needs both.
    let mut branch = create(None, Integration::Branch);
    if let TeamAction::Create {
        integration_branch, ..
    } = &mut branch.action
    {
        integration_branch.clear();
    }
    assert!(matches!(prepare(&branch), Err(TeamOutcome::Failed { .. })));
    let mut commit = create(None, Integration::Branch);
    if let TeamAction::Create { base_commit, .. } = &mut commit.action {
        base_commit.clear();
    }
    assert!(matches!(prepare(&commit), Err(TeamOutcome::Failed { .. })));
}

#[test]
fn a_reason_or_a_reply_must_say_something() {
    for action in [
        TeamAction::Return {
            node: 1,
            reason: "   ".into(),
        },
        TeamAction::Cancel {
            node: 1,
            reason: String::new(),
        },
        TeamAction::Reassign {
            node: 1,
            member: "m1".into(),
            reason: String::new(),
        },
        TeamAction::Reply {
            item: 1,
            text: "\n".into(),
        },
        TeamAction::Ask {
            node: 1,
            generation: 1,
            text: "  ".into(),
        },
        TeamAction::Decide {
            card: 1,
            decision: String::new(),
        },
    ] {
        assert!(action.check().is_err(), "{action:?} must say something");
    }
    let long = "x".repeat(TEXT_CHARS + 1);
    assert!(TeamAction::Reply {
        item: 1,
        text: long
    }
    .check()
    .is_err());
    assert!(TeamAction::Reply {
        item: 1,
        text: "x".repeat(TEXT_CHARS)
    }
    .check()
    .is_ok());
}

#[test]
fn a_request_that_cannot_be_made_never_runs() {
    let mut call = a_call(TEAM, 42, TeamAction::Accept { node: 0 });
    assert!(matches!(prepare(&call), Err(TeamOutcome::Failed { .. })));
    call.action = TeamAction::Accept { node: 1 };
    call.team = "has space".into();
    assert!(matches!(prepare(&call), Err(TeamOutcome::Failed { .. })));
    call = a_call(
        TEAM,
        42,
        TeamAction::Return {
            node: 1,
            reason: "   ".into(),
        },
    );
    assert!(matches!(prepare(&call), Err(TeamOutcome::Failed { .. })));
    call = a_call(
        TEAM,
        42,
        TeamAction::Route {
            node: 1,
            role: "developer".into(),
            pipeline: "reviewed".into(),
            keep_task_fields: false,
            deps: Vec::new(),
            writes: vec!["/absolute/path".into()],
            assignee: None,
        },
    );
    assert!(
        matches!(prepare(&call), Err(TeamOutcome::Failed { .. })),
        "a write set is relative to the repository"
    );
}

#[test]
fn an_outcome_of_each_kind() {
    let call = a_call(TEAM, 42, TeamAction::Accept { node: 7 });
    let done = classify(
        &call,
        Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(&receipt()).unwrap(),
            stderr: String::new(),
        },
    );
    assert_eq!(
        done,
        TeamOutcome::Done {
            receipt: receipt(),
            request_id: ID.into(),
        }
    );
    assert_eq!(done.request_id(), Some(ID));

    let failed = classify(
        &call,
        Exit::Code {
            code: 1,
            stdout:
                br#"{"ok":false,"error":{"code":"stale_revision","message":"The team moved."}}"#
                    .to_vec(),
            stderr: String::new(),
        },
    );
    assert_eq!(
        failed,
        TeamOutcome::Failed {
            detail: "The team moved.".into(),
            code: "stale_revision".into(),
            field: None,
            request_id: ID.into(),
        }
    );
    assert!(failed.repeatable_by_desktop());

    let uncertain = classify(&call, Exit::TimedOut);
    assert!(matches!(uncertain, TeamOutcome::Uncertain { .. }));
    assert!(!uncertain.repeatable_by_desktop());
    assert!(matches!(
        classify(
            &call,
            Exit::Abnormal {
                stderr: "boom".into()
            }
        ),
        TeamOutcome::Uncertain { .. }
    ));
    assert!(matches!(
        classify(
            &call,
            Exit::Code {
                code: 0,
                stdout: b"{}".to_vec(),
                stderr: String::new()
            }
        ),
        TeamOutcome::Uncertain { .. }
    ));
    let start_failed = classify(&call, Exit::StartFailed("no hgs".into()));
    assert!(matches!(start_failed, TeamOutcome::Failed { .. }));
    assert!(start_failed.repeatable_by_desktop());

    let confirmed = prepare(&a_call(TEAM, 42, TeamAction::Stop {})).unwrap_err();
    assert!(matches!(confirmed, TeamOutcome::Confirm { .. }));
    assert_eq!(confirmed.request_id(), None);
    assert!(confirmed.repeatable_by_desktop());
}

#[test]
fn a_success_needs_a_complete_receipt() {
    let call = a_call(TEAM, 42, TeamAction::Accept { node: 7 });
    // `{"ok": true}` alone proves nothing about what was written.
    for incomplete in [
        json!({"ok": true}),
        json!({"ok": true, "revision": 17, "actor": {"kind": "person"}}),
        json!({"ok": true, "revision": 17, "actor": {"kind": "person"}, "team": "other"}),
        json!({"ok": true, "revision": "17", "actor": {"kind": "person"}, "team": TEAM}),
    ] {
        let outcome = classify(
            &call,
            Exit::Code {
                code: 0,
                stdout: serde_json::to_vec(&incomplete).unwrap(),
                stderr: String::new(),
            },
        );
        assert!(
            matches!(outcome, TeamOutcome::Uncertain { .. }),
            "{incomplete} is not a complete receipt"
        );
        assert!(!outcome.repeatable_by_desktop());
    }
}

#[test]
fn an_unreadable_nonzero_exit_is_uncertain() {
    // A started command can write and then fail to deliver its receipt; the
    // exit code alone never proves that nothing was applied.
    let call = a_call(
        TEAM,
        42,
        TeamAction::Reply {
            item: 1,
            text: "Go ahead".into(),
        },
    );
    for exit in [
        Exit::Code {
            code: 1,
            stdout: b"truncated receipt".to_vec(),
            stderr: "write failed after commit".into(),
        },
        Exit::Code {
            code: 1,
            stdout: Vec::new(),
            stderr: String::new(),
        },
        Exit::Code {
            code: 2,
            stdout: b"{}".to_vec(),
            stderr: "hgs is not installed".into(),
        },
    ] {
        let outcome = classify(&call, exit);
        assert!(matches!(outcome, TeamOutcome::Uncertain { .. }));
        assert!(
            !outcome.repeatable_by_desktop(),
            "an unreadable failure may have applied the change"
        );
    }
}

#[test]
fn an_uncertain_call_is_never_repeated_by_the_desktop() {
    let call = a_call(TEAM, 42, TeamAction::Hold { node: 1 });
    for exit in [
        Exit::TimedOut,
        Exit::WriteFailed,
        Exit::Abnormal {
            stderr: String::new(),
        },
        Exit::Code {
            code: 0,
            stdout: b"not json".to_vec(),
            stderr: String::new(),
        },
    ] {
        let outcome = classify(&call, exit);
        assert!(matches!(outcome, TeamOutcome::Uncertain { .. }));
        assert!(!outcome.repeatable_by_desktop());
        assert_eq!(outcome.request_id(), Some(ID));
    }
}

#[test]
fn a_pending_intent_reports_pending() {
    let mut intents = Intents::default();
    assert_eq!(
        intents.admit(&a_call(TEAM, 42, TeamAction::Pause {})),
        Admission::Send
    );
    let again = intents.attempt(
        Path::new("/nonexistent"),
        &a_call(TEAM, 42, TeamAction::Pause {}),
    );
    assert_eq!(
        again,
        TeamOutcome::Pending {
            request_id: ID.into()
        }
    );
    assert!(!again.repeatable_by_desktop());
    assert_eq!(intents.state(ID), Some(IntentState::Pending));
}

#[test]
fn a_call_is_deserialized_without_naming_its_actor() {
    let parsed: TeamCall = serde_json::from_value(json!({
        "team": TEAM,
        "team_name": "Fixes",
        "expected_revision": 42,
        "confirmed": false,
        "request_id": ID,
        "action": {"kind": "return", "node": 2, "reason": "Add a test"},
    }))
    .unwrap();
    assert_eq!(
        parsed.action,
        TeamAction::Return {
            node: 2,
            reason: "Add a test".into()
        }
    );
    assert_eq!(parsed.expected_revision, Some(42));
    let mut other = serde_json::to_value(&parsed).unwrap();
    other["actor"] = json!({"kind": "service"});
    assert!(serde_json::from_value::<TeamCall>(other).is_err());
}

/// A helper `hgs` script that records every request it is given.
#[cfg(unix)]
struct Helper {
    dir: PathBuf,
    script: PathBuf,
    effects: PathBuf,
}

#[cfg(unix)]
impl Helper {
    /// `receipt` prints a complete success receipt; otherwise the script ends
    /// nonzero without one, after recording the request.
    fn new(name: &str, success: bool) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("zerus-teams-action-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let effects = dir.join("effects.jsonl");
        let script = dir.join("hgs");
        let body = if success {
            format!(
                "printf '%s' '{}'\n",
                serde_json::to_string(&receipt()).unwrap()
            )
        } else {
            "printf '%s' 'receipt transport failed after commit' >&2\nexit 1\n".to_owned()
        };
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n{{ cat; echo; }} >> '{}'\n{body}",
                effects.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            dir,
            script,
            effects,
        }
    }

    fn effects(&self) -> usize {
        std::fs::read_to_string(&self.effects)
            .map(|text| text.lines().filter(|line| !line.is_empty()).count())
            .unwrap_or(0)
    }
}

#[cfg(unix)]
impl Drop for Helper {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(unix)]
#[test]
fn an_action_runs_once_through_the_real_runner() {
    let helper = Helper::new("once", true);
    let call = a_call(TEAM, 42, TeamAction::Accept { node: 7 });
    let outcome = run_action(&helper.script, &call);
    assert!(matches!(outcome, TeamOutcome::Done { .. }), "{outcome:?}");
    assert_eq!(helper.effects(), 1, "one call is one request");
}

#[cfg(unix)]
#[test]
fn an_effect_then_an_unreadable_exit_is_uncertain_and_sent_once() {
    let helper = Helper::new("effect", false);
    let call = a_call(TEAM, 42, TeamAction::Accept { node: 7 });
    let mut intents = Intents::default();
    let first = intents.attempt(&helper.script, &call);
    assert!(matches!(first, TeamOutcome::Uncertain { .. }), "{first:?}");
    let second = intents.attempt(&helper.script, &call);
    assert_eq!(second, first, "an uncertain intent returns its record");
    assert_eq!(helper.effects(), 1, "the desktop never resends by itself");
    assert_eq!(intents.state(ID), Some(IntentState::Uncertain));
    // The person asks to check it: the same id and content go again.
    let checked = intents.check_again(&helper.script, &call);
    assert!(matches!(checked, TeamOutcome::Uncertain { .. }));
    assert_eq!(helper.effects(), 2, "only the person's check resends it");
    let stdin: Value = serde_json::from_str(
        std::fs::read_to_string(&helper.effects)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(stdin["request_id"], ID, "the same intent keeps its id");
}

/// The store's storage failure, as `hgs team` prints it (with the problem's
/// optional detail), and the problem itself: `crates/team-plan/fixtures`.
const CLI_STORAGE_FAILED: &str =
    include_str!("../../../../../crates/team-plan/fixtures/cli-storage-failed.json");
const PROBLEM_STORAGE_FAILED: &str =
    include_str!("../../../../../crates/team-plan/fixtures/problem-storage-failed.json");
const PROBLEM_STORE_BUSY: &str =
    include_str!("../../../../../crates/team-plan/fixtures/problem-store-busy.json");
const STORAGE_SENTENCE: &str =
    "The result is not known; repeat the same request with the same request ID.";

#[test]
fn the_storage_fixtures_are_one_problem() {
    let cli: Value = serde_json::from_str(CLI_STORAGE_FAILED).unwrap();
    let problem: Value = serde_json::from_str(PROBLEM_STORAGE_FAILED).unwrap();
    assert_eq!(cli["ok"], false);
    // The printed problem carries its optional detail; the rest is the problem.
    let mut error = cli["error"].clone();
    assert!(error.as_object_mut().unwrap().remove("detail").is_some());
    assert_eq!(error, problem);
    assert_eq!(problem["code"], super::wire::STORAGE_FAILED);
    assert_eq!(problem["message"], STORAGE_SENTENCE);
}

/// Owner decision of 2026-11-12 (zerus-63rru.19): a storage failure is not a
/// refusal. Its result is not known, so the call is uncertain with its own
/// request id and the store's sentence, and only Check (the same id) or
/// Dismiss leave it; a real refusal still fails definitely.
#[test]
fn a_storage_failure_is_uncertain_with_the_same_request_id() {
    for action in [
        TeamAction::Accept { node: 7 },
        TeamAction::Decide {
            card: 3,
            decision: "approve".into(),
        },
    ] {
        let call = a_call(TEAM, 42, action);
        for code in [1, 0] {
            let outcome = classify(
                &call,
                Exit::Code {
                    code,
                    stdout: CLI_STORAGE_FAILED.as_bytes().to_vec(),
                    stderr: String::new(),
                },
            );
            assert_eq!(
                outcome,
                TeamOutcome::Uncertain {
                    detail: STORAGE_SENTENCE.into(),
                    request_id: ID.into(),
                },
                "exit {code}"
            );
            assert!(!outcome.repeatable_by_desktop());
        }
    }
    let create = create(None, Integration::None);
    let outcome = classify(
        &create,
        Exit::Code {
            code: 1,
            stdout: CLI_STORAGE_FAILED.as_bytes().to_vec(),
            stderr: String::new(),
        },
    );
    assert_eq!(outcome.request_id(), Some(ID));
    assert!(
        matches!(outcome, TeamOutcome::Uncertain { .. }),
        "a New team create too"
    );
    // A storage failure without a sentence still says what to do.
    let bare = classify(
        &a_call(TEAM, 42, TeamAction::Pause {}),
        Exit::Code {
            code: 1,
            stdout: br#"{"ok":false,"error":{"code":"storage_failed"}}"#.to_vec(),
            stderr: String::new(),
        },
    );
    assert!(matches!(&bare, TeamOutcome::Uncertain { detail, .. } if !detail.is_empty()));
    // A rule's refusal is still a definite one.
    let refused = classify(
        &a_call(TEAM, 42, TeamAction::Accept { node: 7 }),
        Exit::Code {
            code: 1,
            stdout: br#"{"ok":false,"error":{"code":"invalid_state","message":"The node is not reported."}}"#.to_vec(),
            stderr: String::new(),
        },
    );
    assert_eq!(
        refused,
        TeamOutcome::Failed {
            detail: "The node is not reported.".into(),
            code: "invalid_state".into(),
            field: None,
            request_id: ID.into(),
        }
    );
    assert!(refused.repeatable_by_desktop());
}

#[test]
fn check_after_a_storage_failure_sends_the_same_request_id() {
    let call = a_call(TEAM, 42, TeamAction::Accept { node: 7 });
    let mut intents = Intents::default();
    assert_eq!(intents.admit(&call), Admission::Send);
    let outcome = intents.settle(
        &call,
        classify(
            &call,
            Exit::Code {
                code: 1,
                stdout: CLI_STORAGE_FAILED.as_bytes().to_vec(),
                stderr: String::new(),
            },
        ),
    );
    assert_eq!(outcome.request_id(), Some(ID));
    assert_eq!(intents.state(ID), Some(IntentState::Uncertain));
    // The same call again is not a fresh send: it answers its recorded outcome.
    assert!(matches!(
        intents.admit(&call),
        Admission::Recorded(TeamOutcome::Uncertain { .. })
    ));
    // Check sends the same request id and content once more.
    assert_eq!(intents.admit_again(&call), Admission::Send);
    assert_eq!(intents.state(ID), Some(IntentState::Pending));
    let (invocation, request_id) = prepare(&call).unwrap();
    assert_eq!(request_id, ID);
    assert_eq!(invocation.request_id.as_deref(), Some(ID));
    assert_eq!(stdin_of(&invocation)["request_id"], ID);
    // A refusal settles and is never checked again.
    let refused_call = TeamCall {
        request_id: "22222222-2222-4333-8444-555555555555".into(),
        ..a_call(TEAM, 42, TeamAction::Accept { node: 8 })
    };
    assert_eq!(intents.admit(&refused_call), Admission::Send);
    intents.settle(
        &refused_call,
        classify(
            &refused_call,
            Exit::Code {
                code: 1,
                stdout: br#"{"ok":false,"error":{"code":"invalid_state","message":"No."}}"#
                    .to_vec(),
                stderr: String::new(),
            },
        ),
    );
    assert_ne!(
        intents.state("22222222-2222-4333-8444-555555555555"),
        Some(IntentState::Uncertain)
    );
}

// The create binding (zerus-63rru.30): what the person confirmed is bound at
// the create. The call carries the digest of the template the form showed and
// the five permissions as the form shows them; the command line refuses a
// changed template with template_changed before anything is written.

/// `crates/team-plan/fixtures/cli-template-changed.json` and its problem.
const CLI_TEMPLATE_CHANGED: &str =
    include_str!("../../../../../crates/team-plan/fixtures/cli-template-changed.json");
const PROBLEM_TEMPLATE_CHANGED: &str =
    include_str!("../../../../../crates/team-plan/fixtures/problem-template-changed.json");

#[test]
fn a_create_carries_the_shown_templates_digest_and_its_five_permissions() {
    let (invocation, _) = prepare(&create(None, Integration::Branch)).unwrap();
    assert_eq!(
        invocation.args,
        ["team", "create", "--template", "bugfix", "--json"]
    );
    let input = stdin_of(&invocation);
    assert_eq!(input["expected_template"], DIGEST);
    assert_eq!(
        input["delegations"],
        json!({"accept_results": true, "answer_members": true, "confirm_trust": true, "resume_exited": true, "merge_accepted": false})
    );
}

#[test]
fn a_create_names_all_five_permissions_and_a_digest_or_it_is_not_read() {
    let call = |action: Value| {
        serde_json::from_value::<TeamCall>(json!({
            "team": "00000000-0000-4000-8000-00000000cafe", "team_name": "", "request_id": ID,
            "action": action,
        }))
    };
    let mut action = serde_json::to_value(&create(None, Integration::Branch).action).unwrap();
    assert!(call(action.clone()).is_ok());
    // The contract's own Delegations fills a missing field with its default
    // (true); the create's never does.
    let mut fewer = action.clone();
    fewer["delegations"]
        .as_object_mut()
        .unwrap()
        .remove("merge_accepted");
    assert!(call(fewer).is_err(), "four permissions are not read");
    let mut unknown = action.clone();
    unknown["delegations"]["future_power"] = json!(true);
    assert!(call(unknown).is_err(), "an unknown permission is not read");
    let mut text = action.clone();
    text["delegations"]["merge_accepted"] = json!("false");
    assert!(call(text).is_err(), "a permission is a boolean");
    let mut unbound = action.clone();
    unbound.as_object_mut().unwrap().remove("expected_template");
    assert!(
        call(unbound).is_err(),
        "a create without a digest is not read"
    );
    action["expected_template"] = json!("");
    let blank = call(action).unwrap();
    assert_eq!(blank.action.check(), Err("Not a template digest.".into()));
    assert!(matches!(
        prepare(&TeamCall {
            expected_revision: None,
            ..blank
        }),
        Err(TeamOutcome::Failed { .. })
    ));
    // The five are the contract's five.
    let contract: Vec<String> =
        serde_json::to_value(zerus_team_plan::model::Delegations::default())
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
    let ours: Vec<String> = serde_json::to_value(shown())
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        ours.iter().collect::<std::collections::BTreeSet<_>>(),
        contract.iter().collect()
    );
}

#[test]
fn check_after_a_storage_failure_sends_the_same_create_byte_for_byte() {
    let call = create(None, Integration::Branch);
    let (first, _) = prepare(&call).unwrap();
    let mut intents = Intents::default();
    assert_eq!(intents.admit(&call), Admission::Send);
    intents.settle(
        &call,
        classify(
            &call,
            Exit::Code {
                code: 1,
                stdout: CLI_STORAGE_FAILED.as_bytes().to_vec(),
                stderr: String::new(),
            },
        ),
    );
    assert_eq!(intents.state(ID), Some(IntentState::Uncertain));
    // The same id with other permissions or another digest is not this call.
    for other in [
        TeamCall {
            action: match call.action.clone() {
                TeamAction::Create {
                    template,
                    project,
                    name,
                    goal,
                    home,
                    integration_branch,
                    base_commit,
                    integration,
                    expected_template,
                    ..
                } => TeamAction::Create {
                    template,
                    project,
                    name,
                    goal,
                    home,
                    integration_branch,
                    base_commit,
                    integration,
                    expected_template,
                    delegations: CreateDelegations {
                        merge_accepted: true,
                        ..shown()
                    },
                },
                _ => unreachable!(),
            },
            ..call.clone()
        },
        TeamCall {
            action: match call.action.clone() {
                TeamAction::Create {
                    template,
                    project,
                    name,
                    goal,
                    home,
                    integration_branch,
                    base_commit,
                    integration,
                    delegations,
                    ..
                } => TeamAction::Create {
                    template,
                    project,
                    name,
                    goal,
                    home,
                    integration_branch,
                    base_commit,
                    integration,
                    expected_template:
                        "a13f8ce303eeec0f3a1eeae443bc1a1594696e48c365e18d5099afec353e722a".into(),
                    delegations,
                },
                _ => unreachable!(),
            },
            ..call.clone()
        },
    ] {
        assert!(
            matches!(intents.admit_again(&other), Admission::Refused(TeamOutcome::Failed { ref code, .. }) if code == "duplicate"),
            "another binding under the same id is refused"
        );
        assert_eq!(intents.state(ID), Some(IntentState::Uncertain));
    }
    assert_eq!(intents.admit_again(&call), Admission::Send);
    let (again, request_id) = prepare(&call).unwrap();
    assert_eq!(request_id, ID);
    assert_eq!(again.args, first.args);
    assert_eq!(
        again.stdin, first.stdin,
        "Check sends the create's bytes as they were"
    );
    let input = stdin_of(&again);
    assert_eq!(input["request_id"], ID);
    assert_eq!(input["expected_template"], DIGEST);
    assert_eq!(input["delegations"]["merge_accepted"], false);
}

#[test]
fn a_changed_template_is_a_refusal_the_intent_settles_never_uncertain() {
    let problem: Value = serde_json::from_str(PROBLEM_TEMPLATE_CHANGED).unwrap();
    let reply: Value = serde_json::from_str(CLI_TEMPLATE_CHANGED).unwrap();
    assert_eq!(reply["error"], problem, "the two fixtures are one problem");
    for code in [1, 0] {
        let call = create(None, Integration::Branch);
        let mut intents = Intents::default();
        assert_eq!(intents.admit(&call), Admission::Send);
        let outcome = intents.settle(
            &call,
            classify(
                &call,
                Exit::Code {
                    code,
                    stdout: CLI_TEMPLATE_CHANGED.as_bytes().to_vec(),
                    stderr: String::new(),
                },
            ),
        );
        assert_eq!(
            outcome,
            TeamOutcome::Failed {
                detail: "The template changed since it was shown. Review it again.".into(),
                code: "template_changed".into(),
                field: None,
                request_id: ID.into(),
            },
            "exit {code}"
        );
        assert_eq!(intents.state(ID), Some(IntentState::Failed));
        // Check cannot send a settled refusal again; it answers the refusal.
        assert!(matches!(
            intents.admit_again(&call),
            Admission::Recorded(TeamOutcome::Failed { .. })
        ));
    }
}

/// The store's busy refusal (zerus-63rru.30's store codes): nothing was
/// written, so it is a refusal like any other code, never an unknown result;
/// the action may simply be sent again (a new intent), and Check has nothing to
/// check. Its sentence is the store's, without the code.
#[test]
fn a_busy_store_is_a_refusal_nothing_was_written() {
    let problem: Value = serde_json::from_str(PROBLEM_STORE_BUSY).unwrap();
    assert_eq!(problem["code"], zerus_team_plan::problem::code::STORE_BUSY);
    let stdout = json!({"ok": false, "error": problem})
        .to_string()
        .into_bytes();
    let call = a_call(TEAM, 42, TeamAction::Accept { node: 7 });
    let mut intents = Intents::default();
    assert_eq!(intents.admit(&call), Admission::Send);
    let outcome = intents.settle(
        &call,
        classify(
            &call,
            Exit::Code {
                code: 1,
                stdout,
                stderr: String::new(),
            },
        ),
    );
    assert_eq!(
        outcome,
        TeamOutcome::Failed {
            detail: "The team store is busy. Nothing was written; try again.".into(),
            code: "store_busy".into(),
            field: None,
            request_id: ID.into(),
        }
    );
    assert_eq!(intents.state(ID), Some(IntentState::Failed));
    let again = TeamCall {
        request_id: "22222222-2222-4333-8444-555555555555".into(),
        ..call
    };
    assert_eq!(
        intents.admit(&again),
        Admission::Send,
        "the same action is sent again"
    );
}

/// A problem may carry a detail (`cli-storage-failed.json` does). A person does
/// not need it in the sentence: the outcome keeps the store's message only,
/// whether the code is uncertain or a refusal.
#[test]
fn a_problems_detail_stays_out_of_the_sentence() {
    let cli: Value = serde_json::from_str(CLI_STORAGE_FAILED).unwrap();
    let detail = cli["error"]["detail"].as_str().unwrap().to_owned();
    let call = a_call(TEAM, 42, TeamAction::Accept { node: 7 });
    let exit = |stdout: Vec<u8>| Exit::Code {
        code: 1,
        stdout,
        stderr: String::new(),
    };
    match classify(&call, exit(CLI_STORAGE_FAILED.as_bytes().to_vec())) {
        TeamOutcome::Uncertain { detail: shown, .. } => {
            assert_eq!(shown, STORAGE_SENTENCE);
            assert!(!shown.contains(&detail));
        }
        other => panic!("{other:?}"),
    }
    let refused = json!({"ok": false, "error": {"code": "invalid_state", "message": "No.", "detail": detail}});
    match classify(&call, exit(refused.to_string().into_bytes())) {
        TeamOutcome::Failed {
            detail: shown,
            code,
            ..
        } => {
            assert_eq!((shown.as_str(), code.as_str()), ("No.", "invalid_state"));
        }
        other => panic!("{other:?}"),
    }
}

/// The review's B2: what is sent is strict at every level, the action inside
/// the call included. Its probe: a Create with a field this version does not
/// know is refused, never prepared without it; a variant without fields
/// refuses one too.
#[test]
fn an_unknown_field_in_an_action_is_refused_never_left_out() {
    let probe = json!({
        "team": "00000000-0000-4000-8000-00000000c0de", "team_name": "Review",
        "request_id": "11111111-1111-4111-8111-111111111111",
        "action": {
            "kind": "create", "template": "bugfix", "expected_template": DIGEST,
            "delegations": {"accept_results": true, "answer_members": true, "confirm_trust": true, "resume_exited": true, "merge_accepted": false},
            "project": "example-project", "name": "Review", "goal": "", "home": "00000000-0000-4000-8000-0000000000aa",
            "integration": "branch", "integration_branch": "zerus/team/review",
            "base_commit": "0123456789abcdef0123456789abcdef01234567",
            "future_precondition": "must not be silently lost",
        },
    });
    assert!(serde_json::from_value::<TeamCall>(probe.clone()).is_err());
    let mut known = probe.clone();
    known["action"]
        .as_object_mut()
        .unwrap()
        .remove("future_precondition");
    let call: TeamCall = serde_json::from_value(known).expect("the same create without it is read");
    assert!(prepare(&call).is_ok());
    for action in [
        json!({"kind": "pause", "added_later": true}),
        json!({"kind": "decide", "card": 3, "decision": "approve", "added_later": true}),
        json!({"kind": "cancel", "node": 2, "reason": "Not needed.", "added_later": true}),
    ] {
        assert!(
            serde_json::from_value::<TeamAction>(action.clone()).is_err(),
            "{action}"
        );
    }
}

#[test]
fn every_action_the_view_sends_is_still_read() {
    assert_eq!(
        serde_json::from_value::<TeamAction>(json!({"kind": "pause"})).unwrap(),
        TeamAction::Pause {}
    );
    for action in [
        create(None, Integration::Branch).action,
        TeamAction::Pause {},
        TeamAction::StopMember {
            member: "m1".into(),
        },
        TeamAction::Decide {
            card: 3,
            decision: "approve".into(),
        },
        TeamAction::Return {
            node: 2,
            reason: "Again.".into(),
        },
        TeamAction::Route {
            node: 2,
            role: "developer".into(),
            pipeline: "reviewed".into(),
            keep_task_fields: false,
            deps: Vec::new(),
            writes: Vec::new(),
            assignee: None,
        },
    ] {
        let sent = serde_json::to_value(&action).unwrap();
        assert_eq!(serde_json::from_value::<TeamAction>(sent).unwrap(), action);
    }
}

/// Every kind the core reads, as the webview must write it
/// (`fixtures/action-kinds.json`, which the Team tab's test also reads). The
/// match below names every variant, so a new one cannot be left out.
fn action_kinds() -> Vec<String> {
    fn sample(action: &TeamAction) -> &'static str {
        match action {
            TeamAction::SetPhaseBarrier { .. }
            | TeamAction::PausePhase { .. }
            | TeamAction::ResumePhase { .. }
            | TeamAction::MovePhase { .. }
            | TeamAction::SetPhaseAcceptance { .. }
            | TeamAction::SendBackPhase { .. }
            | TeamAction::SubmitPhaseCorrections { .. }
            | TeamAction::EditPhaseCorrections { .. }
            | TeamAction::ApprovePhaseCorrections { .. }
            | TeamAction::RejectPhaseCorrections { .. }
            | TeamAction::UndoPhaseCorrectionRejection { .. }
            | TeamAction::ApprovePhase { .. }
            | TeamAction::RejectPhase { .. }
            | TeamAction::UndoRejectPhase { .. }
            | TeamAction::AcceptPhase { .. }
            | TeamAction::Create { .. }
            | TeamAction::StartPlanning { .. }
            | TeamAction::SubmitDraftPhase { .. }
            | TeamAction::EditDraftPhase { .. }
            | TeamAction::AddMember { .. }
            | TeamAction::SwapMember { .. }
            | TeamAction::RenameTeam { .. }
            | TeamAction::SetGoal { .. }
            | TeamAction::Pause {}
            | TeamAction::Resume {}
            | TeamAction::Stop {}
            | TeamAction::Archive {}
            | TeamAction::AdvanceBase { .. }
            | TeamAction::PauseMember { .. }
            | TeamAction::ResumeMember { .. }
            | TeamAction::StopMember { .. }
            | TeamAction::ArchiveMember { .. }
            | TeamAction::ValidationConfig { .. }
            | TeamAction::Settings { .. }
            | TeamAction::Decide { .. }
            | TeamAction::Admit { .. }
            | TeamAction::SetDeps { .. }
            | TeamAction::Route { .. }
            | TeamAction::Accept { .. }
            | TeamAction::Return { .. }
            | TeamAction::Reassign { .. }
            | TeamAction::Hold { .. }
            | TeamAction::Release { .. }
            | TeamAction::Cancel { .. }
            | TeamAction::Reply { .. }
            | TeamAction::Ask { .. } => "",
        }
    }
    let member = || "m1".to_owned();
    let scope = || zerus_team_plan::phase_control::PhaseControlScope::Draft {
        phase: 1,
        generation: 1,
    };
    let samples = vec![
        TeamAction::SetPhaseBarrier {
            scope: scope(),
            barrier: true,
        },
        TeamAction::PausePhase {
            scope: scope(),
            pause: None,
            reason: "Review".into(),
        },
        TeamAction::ResumePhase {
            scope: scope(),
            pause: None,
        },
        TeamAction::MovePhase {
            scope: scope(),
            relative_to: zerus_team_plan::phase_control::PhaseControlScope::Draft {
                phase: 3,
                generation: 1,
            },
            placement: zerus_team_plan::phase_control::PhasePlacement::Before,
        },
        TeamAction::SetPhaseAcceptance {
            scope: scope(),
            acceptance: zerus_team_plan::Acceptance::Person,
        },
        TeamAction::SendBackPhase {
            phase: 1,
            publication: TEAM.into(),
            comment: "Revise".into(),
        },
        TeamAction::SubmitPhaseCorrections {
            phase: 1,
            publication: TEAM.into(),
            rework: TEAM.into(),
            draft: zerus_team_plan::draft::DraftInput {
                title: String::new(),
                barrier: false,
                nodes: vec![],
            },
        },
        TeamAction::EditPhaseCorrections {
            phase: 1,
            publication: TEAM.into(),
            rework: TEAM.into(),
            generation: 1,
            draft: zerus_team_plan::draft::DraftInput {
                title: String::new(),
                barrier: false,
                nodes: vec![],
            },
        },
        TeamAction::ApprovePhaseCorrections {
            phase: 1,
            publication: TEAM.into(),
            rework: TEAM.into(),
            generation: 1,
        },
        TeamAction::RejectPhaseCorrections {
            phase: 1,
            publication: TEAM.into(),
            rework: TEAM.into(),
            generation: 1,
            comment: "Revise".into(),
        },
        TeamAction::UndoPhaseCorrectionRejection {
            phase: 1,
            publication: TEAM.into(),
            rework: TEAM.into(),
            generation: 1,
            rejection: TEAM.into(),
        },
        TeamAction::ApprovePhase {
            phase: 1,
            generation: 1,
            confirm_initial: true,
        },
        TeamAction::RejectPhase {
            phase: 1,
            generation: 1,
            comment: "Revise".into(),
        },
        TeamAction::UndoRejectPhase {
            phase: 1,
            generation: 1,
            rejection: TEAM.into(),
        },
        TeamAction::AcceptPhase {
            phase: 1,
            publication: TEAM.into(),
        },
        serde_json::from_value::<TeamAction>({
            let mut value = serde_json::to_value(create(None, Integration::Branch).action).unwrap();
            value["kind"] = "start_planning".into();
            value
        })
        .unwrap(),
        TeamAction::SubmitDraftPhase {
            phase: 1,
            draft: zerus_team_plan::draft::DraftInput {
                title: String::new(),
                barrier: false,
                nodes: Vec::new(),
            },
        },
        TeamAction::EditDraftPhase {
            phase: 1,
            generation: 1,
            draft: zerus_team_plan::draft::DraftInput {
                title: String::new(),
                barrier: false,
                nodes: Vec::new(),
            },
        },
        TeamAction::AddMember {
            role: "developer".into(),
            agent: "claude".into(),
            model: String::new(),
            effort: String::new(),
            account: Default::default(),
        },
        TeamAction::SwapMember {
            member: member(),
            agent: "codex".into(),
            model: String::new(),
            effort: String::new(),
            account: Default::default(),
        },
        TeamAction::RenameTeam {
            name: "Team".into(),
        },
        TeamAction::SetGoal {
            goal: String::new(),
        },
        create(None, Integration::Branch).action,
        TeamAction::Pause {},
        TeamAction::Resume {},
        TeamAction::Stop {},
        TeamAction::Archive {},
        TeamAction::AdvanceBase {
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
        },
        TeamAction::PauseMember { member: member() },
        TeamAction::ResumeMember { member: member() },
        TeamAction::StopMember { member: member() },
        TeamAction::ArchiveMember { member: member() },
        TeamAction::ValidationConfig {
            selection: zerus_team_plan::validation::Selection {
                source_digest: "a".repeat(64),
                digest: "b".repeat(64),
                override_config: None,
            },
        },
        TeamAction::Settings {
            settings: Default::default(),
        },
        TeamAction::Decide {
            card: 1,
            decision: "approve".into(),
        },
        TeamAction::Admit { node: 1 },
        TeamAction::SetDeps {
            node: 1,
            deps: Vec::new(),
        },
        TeamAction::Route {
            node: 1,
            role: "r".into(),
            pipeline: "p".into(),
            keep_task_fields: false,
            deps: Vec::new(),
            writes: Vec::new(),
            assignee: None,
        },
        TeamAction::Accept { node: 1 },
        TeamAction::Return {
            node: 1,
            reason: "r".into(),
        },
        TeamAction::Reassign {
            node: 1,
            member: member(),
            reason: String::new(),
        },
        TeamAction::Hold { node: 1 },
        TeamAction::Release { node: 1 },
        TeamAction::Cancel {
            node: 1,
            reason: String::new(),
        },
        TeamAction::Reply {
            item: 1,
            text: "t".into(),
        },
        TeamAction::Ask {
            node: 1,
            generation: 1,
            text: "t".into(),
        },
    ];
    let mut kinds: Vec<String> = samples
        .iter()
        .map(|action| {
            sample(action);
            serde_json::to_value(action).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    kinds.sort();
    kinds
}

#[test]
#[ignore = "writes the shared action-kind fixture using the typed serializer"]
fn regenerate_action_kinds() {
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/upstream_teams/fixtures/action-kinds.json"),
        serde_json::to_string_pretty(&action_kinds()).unwrap() + "\n",
    )
    .unwrap();
}

#[test]
fn the_action_kinds_are_the_shared_list() {
    let kinds = action_kinds();
    let shared: Vec<String> =
        serde_json::from_str(include_str!("../fixtures/action-kinds.json")).unwrap();
    assert_eq!(kinds, shared);
}

#[test]
fn advance_base_requires_confirmation_and_keeps_revision_and_identity() {
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let mut call = a_call(
        TEAM,
        7,
        TeamAction::AdvanceBase {
            commit: commit.into(),
        },
    );
    let TeamOutcome::Confirm { confirmation } = prepare(&call).unwrap_err() else {
        panic!("confirmation required")
    };
    assert_eq!(confirmation.name, "teamAdvanceBaseConfirm");
    assert!(confirmation.text.contains(commit));
    call.confirmed = true;
    let (invocation, _) = prepare(&call).unwrap();
    assert_eq!(invocation.args, ["team", "advance-base", "--json"]);
    let input = stdin_of(&invocation);
    assert_eq!(input["commit"], commit);
    assert_eq!(input["request_id"], call.request_id);
    assert_eq!(input["expected_revision"], 7);
    assert!(input.get("base_revision").is_none());
    for invalid in ["HEAD", "main", "0123456", "--all"] {
        call.action = TeamAction::AdvanceBase {
            commit: invalid.into(),
        };
        assert!(prepare(&call).is_err());
    }
}

/// zerus-3u5yu (6, mutant rs5 of the #2527 review): a window Route that keeps
/// the task's fields names none of them itself; the service binds them.
#[test]
fn a_route_that_keeps_the_task_fields_names_none_of_them() {
    let route = |deps: Vec<zerus_team_plan::model::Dep>,
                 writes: Vec<String>,
                 assignee: Option<String>| TeamAction::Route {
        node: 2,
        role: "developer".into(),
        pipeline: "reviewed".into(),
        keep_task_fields: true,
        deps,
        writes,
        assignee,
    };
    assert_eq!(route(Vec::new(), Vec::new(), None).check(), Ok(()));
    let refusal = Err(
        "A placement edit cannot also change the task's dependencies, write paths or assignee."
            .to_owned(),
    );
    assert_eq!(
        route(
            vec![zerus_team_plan::model::Dep {
                node: 1,
                kind: zerus_team_plan::model::DepKind::Soft
            }],
            Vec::new(),
            None
        )
        .check(),
        refusal
    );
    assert_eq!(
        route(Vec::new(), vec!["src/example.rs".into()], None).check(),
        refusal
    );
    assert_eq!(
        route(Vec::new(), Vec::new(), Some("m1".into())).check(),
        refusal
    );
}
