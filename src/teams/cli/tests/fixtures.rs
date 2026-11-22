//! Read replies, a template refusal and an unknown write result, pinned as shared
//! fixtures from exactly the JSON a person's `hgs team` prints.
use super::harness::*;
use crate::teams::cli::actor::Caller;
use crate::teams::ops::{HandoutOutcome, Operation, ReportNote, TaskStage};
use serde_json::Value;
use zerus_team_plan::model::Acceptance;

const STATUS: &str = include_str!("../../../../crates/team-plan/fixtures/cli-status.json");
const STATUS_STOPPED: &str =
    include_str!("../../../../crates/team-plan/fixtures/cli-status-stopped.json");
const QUEUE: &str = include_str!("../../../../crates/team-plan/fixtures/cli-queue.json");
const INBOX: &str = include_str!("../../../../crates/team-plan/fixtures/cli-inbox.json");
const CARDS: &str = include_str!("../../../../crates/team-plan/fixtures/cli-cards.json");
const JOURNAL: &str = include_str!("../../../../crates/team-plan/fixtures/cli-journal.json");
const LIST: &str = include_str!("../../../../crates/team-plan/fixtures/cli-list.json");
const WAITING: &str = include_str!("../../../../crates/team-plan/fixtures/cli-waiting.json");
const STORAGE_FAILED: &str =
    include_str!("../../../../crates/team-plan/fixtures/cli-storage-failed.json");

const CATALOG: &str = include_str!("../../../../crates/team-plan/fixtures/cli-catalog.json");
const CHANGED: &str =
    include_str!("../../../../crates/team-plan/fixtures/cli-template-changed.json");
const CURRENT: &str = include_str!("../../../../crates/team-plan/fixtures/cli-current.json");
const EVIDENCE: &str = include_str!("../../../../crates/team-plan/fixtures/cli-evidence.json");

fn lead() -> Caller {
    Caller::Session(key(LEAD_SESSION))
}

/// Each fixture's reply. The stopped team's status carries the items the
/// service's own passes wrote, which it runs only on Linux and macOS.
fn replies() -> Vec<(&'static str, Value)> {
    let harness = seeded();
    let (board, _) = super::verdict::configured_stage(
        zerus_team_plan::catalog::StageKind::Review,
        3,
        Some("example-model"),
    );
    let board_read = |caller: &Caller, line: &str| {
        let (reply, exit) = board.cli(caller, line, "");
        assert_eq!(exit, 0, "{line}: {reply}");
        reply
    };
    let read = |caller: &Caller, line: &str| {
        let (reply, exit) = harness.cli(caller, line, "");
        assert_eq!(exit, 0, "{line}: {reply}");
        reply
    };
    // Only Linux and macOS add the stopped team's reply below.
    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(unused_mut))]
    let mut replies = vec![
        (
            "cli-status.json",
            board_read(&Caller::Person, &format!("status --team {TEAM}")),
        ),
        (
            "cli-queue.json",
            board_read(&lead(), &format!("queue --team {TEAM}")),
        ),
        (
            "cli-inbox.json",
            read(&lead(), &format!("inbox --team {TEAM}")),
        ),
        (
            "cli-cards.json",
            read(&lead(), &format!("cards --team {TEAM}")),
        ),
        (
            "cli-journal.json",
            read(&lead(), &format!("journal --team {TEAM} --limit 200")),
        ),
        ("cli-list.json", read(&Caller::Person, "list")),
        ("cli-waiting.json", read(&Caller::Person, "waiting")),
        (
            "cli-storage-failed.json",
            super::storage_failure::failed_write(),
        ),
        ("cli-catalog.json", read(&Caller::Person, "catalog show")),
        (
            "cli-template-changed.json",
            super::create_binding::changed_refusal_fixture(),
        ),
        (
            "cli-current.json",
            read(&Caller::Session(key(M1_SESSION)), "current"),
        ),
        // zerus-5gcbh: the evidence beside a decision once the review passed.
        ("cli-evidence.json", super::evidence::passed_review_reply()),
    ];
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    replies.insert(
        1,
        ("cli-status-stopped.json", {
            let stopped = super::status_items::stopped_with_conditions();
            let (reply, exit) = stopped.cli(&Caller::Person, &format!("status --team {TEAM}"), "");
            assert_eq!(exit, 0, "{reply}");
            reply
        }),
    );
    replies
}

#[test]
fn the_read_fixtures_are_the_replies_the_cli_prints() {
    let pinned = [
        ("cli-status.json", STATUS),
        ("cli-status-stopped.json", STATUS_STOPPED),
        ("cli-queue.json", QUEUE),
        ("cli-inbox.json", INBOX),
        ("cli-cards.json", CARDS),
        ("cli-journal.json", JOURNAL),
        ("cli-list.json", LIST),
        ("cli-waiting.json", WAITING),
        ("cli-storage-failed.json", STORAGE_FAILED),
        ("cli-catalog.json", CATALOG),
        ("cli-template-changed.json", CHANGED),
        ("cli-current.json", CURRENT),
        ("cli-evidence.json", EVIDENCE),
    ];
    let replies = replies();
    let produced: Vec<&str> = replies.iter().map(|(name, _)| *name).collect();
    let expected_names: Vec<&str> = pinned
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| {
            cfg!(any(target_os = "linux", target_os = "macos"))
                || *name != "cli-status-stopped.json"
        })
        .collect();
    assert_eq!(produced, expected_names);
    for (name, actual) in &replies {
        let text = pinned
            .iter()
            .find(|(pinned_name, _)| pinned_name == name)
            .map(|(_, text)| *text)
            .unwrap();
        let expected: Value =
            serde_json::from_str(text).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(actual, &expected, "{name} is not the reply this hgs prints");
        if *name == "cli-storage-failed.json" {
            assert_eq!(actual["ok"], false, "{name}");
            assert_eq!(actual["error"]["code"], "storage_failed", "{name}");
        } else if *name == "cli-template-changed.json" {
            assert_eq!(actual["ok"], false, "{name}");
            assert_eq!(actual["error"]["code"], "template_changed", "{name}");
        } else {
            assert_eq!(actual["ok"], true, "{name}");
            assert!(actual.get("error").is_none(), "{name}");
        }
    }
}

/// Rewrites the fixtures from the seed. Run with:
/// `cargo +1.99.1 test --bin hgs teams:: -- --ignored regenerate_fixtures`
#[test]
#[ignore = "writes the shared fixtures; run only when a reply shape changes on purpose"]
fn regenerate_fixtures() {
    for (name, reply) in replies() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/team-plan/fixtures")
            .join(name);
        let mut text = serde_json::to_string_pretty(&reply).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
    }
}

/// The core's and the UI's own copy of one production card: the row of a
/// report's pipeline acceptance card, with the service's notes beside the
/// member's text, read through the person's `waiting` reply
/// (zerus-63rru.105). Its producer is the real store, `talk::card_notes` and
/// the CLI read, so the clients see the shape the service writes, not a
/// hand-written card.
const REPORT_NOTES_CARD: &str =
    include_str!("../../../../client/core/src/upstream_teams/fixtures/report-notes-card.json");

fn report_notes_card() -> Value {
    let h = Harness::new();
    let mut team = fixture().team;
    team.bundle.pipelines[0].stages.clear();
    team.bundle.pipelines[0].accept = Acceptance::Person;
    h.apply(
        &person(),
        Operation::Create {
            expected_template: None,
            delegations: None,
            project: team.project,
            name: team.name,
            goal: team.goal,
            home: team.home,
            bundle: Box::new(team.bundle),
            integration_branch: team.integration_branch,
            base_commit: team.base_commit,
        },
    );
    h.bind("m1", M1_SESSION);
    let node = h.add(1);
    let effect = uid(0x8100);
    h.apply(
        &service(),
        Operation::HandoutIntent {
            node,
            member: "m1".into(),
            effect_request_id: effect.clone(),
        },
    );
    h.apply(
        &service(),
        Operation::HandoutOutcome {
            effect_request_id: effect,
            outcome: HandoutOutcome::Submitted {
                receipt: "receipt-example".into(),
            },
        },
    );
    for stage in [TaskStage::InProgress, TaskStage::InReview, TaskStage::Done] {
        h.observe(node, stage);
    }
    h.apply(
        &service(),
        Operation::RecordReport {
            node,
            generation: 1,
            member: "m1".into(),
            text: "Trimmed the label and added a case for an empty one.".into(),
            commit: COMMIT.into(),
            notes: vec![
                ReportNote::new("text_shortened"),
                ReportNote::new("worktree_dirty"),
            ],
        },
    );
    let (reply, exit) = h.cli(&Caller::Person, "waiting", "");
    assert_eq!(exit, 0, "{reply}");
    let rows = reply["rows"].as_array().expect("the waiting rows");
    assert_eq!(rows.len(), 1, "the report's one acceptance card: {reply}");
    assert_eq!(rows[0]["kind"], "pipeline_acceptance", "{reply}");
    assert_eq!(reply["next"], Value::Null, "{reply}");
    rows[0].clone()
}

#[test]
fn the_report_notes_card_fixture_is_the_waiting_row_the_cli_prints() {
    let expected: Value = serde_json::from_str(REPORT_NOTES_CARD)
        .unwrap_or_else(|error| panic!("report-notes-card.json: {error}"));
    assert_eq!(
        report_notes_card(),
        expected,
        "client/core/src/upstream_teams/fixtures/report-notes-card.json is not the waiting row hgs prints; regenerate it"
    );
}

/// The core's and the UI's fixture, from the production writer. Run with:
/// `cargo +1.99.1 test --bin hgs teams::cli::tests::fixtures::regenerate_report_notes_card -- --ignored`
#[test]
#[ignore = "writes client/core/src/upstream_teams/fixtures/report-notes-card.json"]
fn regenerate_report_notes_card() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("client/core/src/upstream_teams/fixtures/report-notes-card.json");
    let mut text = serde_json::to_string_pretty(&report_notes_card()).unwrap();
    text.push('\n');
    std::fs::write(&path, text).unwrap();
}
