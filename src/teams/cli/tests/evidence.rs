//! zerus-5gcbh: the evidence beside a task's decision. `evidence --team ID
//! --node N` reads the task's latest report and each of its stages with the
//! stage's latest verdict (who gave it, pass or return, its findings), from
//! the rows the report and verdict writers stored; the person and the lead
//! read it, a member does not.
use super::harness::*;
use super::verdict::{configured_stage, REVIEWER_SESSION};
use crate::teams::cli::actor::Caller;
use serde_json::{json, Value};

fn lead() -> Caller {
    Caller::Session(key(LEAD_SESSION))
}
fn reviewer() -> Caller {
    Caller::Session(key(REVIEWER_SESSION))
}
/// The reviewer's verdict through the product writer (`hgs team verdict`).
fn verdict(h: &Harness, node: u32, pass: bool, text: &str, id: u32) {
    let input = json!({"request_id":uid(id),"team":TEAM,"expected_revision":h.revision(),"node":node,"generation":1,"pass":pass,"text":text});
    let (reply, exit) = h.cli(&reviewer(), "verdict", &input.to_string());
    assert_eq!(exit, 0, "{reply}");
}
fn evidence(h: &Harness, caller: &Caller, node: u32) -> (Value, i32) {
    h.cli(caller, &format!("evidence --team {TEAM} --node {node}"), "")
}

/// The reply the desktop reads once the review passed and the task waits for
/// its decision; also pinned as `cli-evidence.json`.
pub(super) fn passed_review_reply() -> Value {
    let (h, stage) = configured_stage(
        zerus_team_plan::catalog::StageKind::Review,
        3,
        Some("example-model"),
    );
    verdict(
        &h,
        stage,
        true,
        "Reviewed every acceptance criterion. One nit: rename the helper.",
        0x8100,
    );
    let (reply, exit) = evidence(&h, &lead(), 1);
    assert_eq!(exit, 0, "{reply}");
    reply
}

#[test]
fn evidence_names_the_report_and_the_reviewers_verdict_beside_the_decision() {
    let (h, stage) = configured_stage(
        zerus_team_plan::catalog::StageKind::Review,
        3,
        Some("example-model"),
    );
    verdict(
        &h,
        stage,
        true,
        "Reviewed every acceptance criterion. One nit: rename the helper.",
        0x8100,
    );
    for caller in [lead(), Caller::Person] {
        let (reply, exit) = evidence(&h, &caller, 1);
        assert_eq!(exit, 0, "{reply}");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["team"], TEAM);
        assert_eq!(reply["node"], 1);
        assert_eq!(reply["revision"], h.revision());
        let report = &reply["report"];
        assert_eq!(report["generation"], 1);
        assert_eq!(report["member"], "m1");
        assert_eq!(report["commit"], COMMIT);
        assert_eq!(
            report["text"],
            "Trimmed the label and added a case for an empty one."
        );
        assert_eq!(report["notes"], json!([]));
        assert_eq!(reply["more_stages"], false);
        let stages = reply["stages"].as_array().unwrap();
        assert_eq!(stages.len(), 1, "{reply}");
        assert_eq!(stages[0]["node"], stage);
        assert_eq!(stages[0]["kind"], "review");
        assert_eq!(stages[0]["round"], 1);
        // The verdict is recorded; the stage node keeps its stored state until
        // the service observes it, and the evidence never invents another.
        assert_eq!(stages[0]["state"], "in_progress");
        assert_eq!(
            stages[0]["verdict"],
            json!({"generation":1,"member":"m3","pass":true,"text":"Reviewed every acceptance criterion. One nit: rename the helper."})
        );
    }
}

#[test]
fn a_returned_review_keeps_its_findings_and_who_gave_them() {
    let (h, stage) = configured_stage(
        zerus_team_plan::catalog::StageKind::Review,
        3,
        Some("example-model"),
    );
    verdict(
        &h,
        stage,
        false,
        "The empty label still overflows; add a test.",
        0x8101,
    );
    let (reply, exit) = evidence(&h, &Caller::Person, 1);
    assert_eq!(exit, 0, "{reply}");
    let verdict = &reply["stages"][0]["verdict"];
    assert_eq!(verdict["member"], "m3");
    assert_eq!(verdict["pass"], false);
    assert_eq!(
        verdict["text"],
        "The empty label still overflows; add a test."
    );
}

#[test]
fn a_task_without_report_or_stages_has_no_invented_evidence() {
    let h = seeded();
    let node = h.add(7);
    let (reply, exit) = evidence(&h, &Caller::Person, node);
    assert_eq!(exit, 0, "{reply}");
    assert_eq!(reply["report"], Value::Null);
    assert_eq!(reply["stages"], json!([]));
    assert_eq!(reply["more_stages"], false);
}

#[test]
fn evidence_refuses_a_stage_an_unknown_node_a_member_and_bad_flags() {
    let (h, stage) = configured_stage(
        zerus_team_plan::catalog::StageKind::Review,
        3,
        Some("example-model"),
    );
    let (reply, exit) = evidence(&h, &Caller::Person, stage);
    assert_eq!((code(&reply), exit), ("invalid_state", 1), "{reply}");
    let (reply, exit) = evidence(&h, &Caller::Person, 999);
    assert_eq!((code(&reply), exit), ("not_found", 1), "{reply}");
    let (reply, exit) = evidence(&h, &Caller::Session(key(M1_SESSION)), 1);
    assert_eq!((code(&reply), exit), ("not_allowed", 1), "{reply}");
    for line in [
        format!("evidence --team {TEAM}"),
        format!("evidence --team {TEAM} --node x"),
        format!("evidence --team {TEAM} --node 1 --limit 5"),
    ] {
        let (reply, exit) = h.cli(&Caller::Person, &line, "");
        assert_eq!(
            (code(&reply), exit),
            ("invalid_state", 1),
            "{line}: {reply}"
        );
    }
}

#[test]
fn evidence_is_a_listed_read() {
    assert!(crate::teams::cli::USAGE.contains("evidence --team ID --node N"));
}

/// Review of #2531 (codex/zerus-review2531/review2531-cx): the report's
/// author is the recipient of the report's own generation, never the node's
/// current implementer; and 25 real review rounds through the product
/// writers show the newest 24 stages with `more_stages`. Adopted from the
/// reviewer's probes (review2531 evidence-review.rs).
mod rounds {
    use super::*;
    use crate::teams::ops::{HandoutOutcome, Operation, StageTaskOutcome, TaskStage};
    use rusqlite::params;

    const M2: &str = "launch_id:00000000-0000-4000-8000-000000000032";

    fn submit_reopen(h: &Harness, node: u32, member: &str, round: u32) {
        let id = uid(0xa100 + round);
        h.apply(
            &service(),
            Operation::ReopenIntent {
                node,
                member: member.into(),
                effect_request_id: id.clone(),
            },
        );
        h.apply(
            &service(),
            Operation::ReopenOutcome {
                effect_request_id: id,
                outcome: HandoutOutcome::Submitted {
                    receipt: "reopened fixture".into(),
                },
            },
        );
        h.observe(node, TaskStage::InProgress);
    }

    fn report_round(h: &Harness, generation: u32, member: &str, body: String) {
        h.observe(1, TaskStage::InReview);
        h.apply(
            &service(),
            Operation::RecordReport {
                node: 1,
                generation,
                member: member.into(),
                text: body,
                commit: COMMIT.into(),
                notes: vec![],
            },
        );
    }

    fn deliver_review(h: &Harness, stage: u32, round: u32) {
        let id = uid(0xa200 + round);
        h.apply(
            &service(),
            Operation::StageTaskIntent {
                node: stage,
                effect_request_id: id.clone(),
            },
        );
        h.apply(
            &service(),
            Operation::StageTaskOutcome {
                effect_request_id: id,
                outcome: StageTaskOutcome::Created {
                    task: task(500 + round),
                },
            },
        );
        h.observe(stage, TaskStage::Todo);
        let id = uid(0xa300 + round);
        h.apply(
            &service(),
            Operation::HandoutIntent {
                node: stage,
                member: "m3".into(),
                effect_request_id: id.clone(),
            },
        );
        h.apply(
            &service(),
            Operation::HandoutOutcome {
                effect_request_id: id,
                outcome: HandoutOutcome::Submitted {
                    receipt: "stage delivered fixture".into(),
                },
            },
        );
        h.observe(stage, TaskStage::InProgress);
    }

    fn rounds_fixture() -> (Harness, u32) {
        let h = Harness::new();
        let mut team = fixture().team;
        team.bundle.pipelines[0].max_rounds = 25;
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
        h.bind("lead", LEAD_SESSION);
        h.bind("m1", M1_SESSION);
        h.bind("m3", REVIEWER_SESSION);
        assert_eq!(h.add(1), 1);
        let id = uid(0xa000);
        h.apply(
            &service(),
            Operation::HandoutIntent {
                node: 1,
                member: "m1".into(),
                effect_request_id: id.clone(),
            },
        );
        h.apply(
            &service(),
            Operation::HandoutOutcome {
                effect_request_id: id,
                outcome: HandoutOutcome::Submitted {
                    receipt: "initial handout".into(),
                },
            },
        );
        h.observe(1, TaskStage::InProgress);
        report_round(
            &h,
            1,
            "m1",
            "Trimmed the label and added a case for an empty one.".into(),
        );
        let stage = h.store().stage_tasks(TEAM, 4, None).unwrap().rows[0].node;
        deliver_review(&h, stage, 1);
        (h, stage)
    }

    #[test]
    fn reassigned_task_keeps_the_old_report_author_until_new_report() {
        let (h, stage) = rounds_fixture();
        h.bind("m2", M2);
        verdict(&h, stage, true, "Review of the first report", 0x9100);
        h.observe(stage, TaskStage::Done);
        h.apply(
            &member("lead", LEAD_SESSION),
            Operation::Reassign {
                node: 1,
                member: "m2".into(),
                reason: "The second developer will continue".into(),
            },
        );
        submit_reopen(&h, 1, "m2", 2);
        let (older, exit) = evidence(&h, &lead(), 1);
        assert_eq!(exit, 0, "{older}");
        report_round(&h, 2, "m2", "The new report from m2.".into());
        let (newer, exit) = evidence(&h, &lead(), 1);
        assert_eq!(exit, 0, "{newer}");
        assert_eq!(older["report"]["generation"], 1);
        assert_eq!(
            older["report"]["text"],
            "Trimmed the label and added a case for an empty one."
        );
        assert_eq!(
            older["report"]["member"], "m1",
            "generation 1 must retain its original author"
        );
        assert_eq!(newer["report"]["generation"], 2);
        assert_eq!(newer["report"]["member"], "m2");
    }

    #[test]
    fn twenty_five_real_review_rounds_show_the_newest_twenty_four() {
        let (h, mut stage) = rounds_fixture();
        let mut expected = vec![];
        for round in 1..=25 {
            if round > 1 {
                submit_reopen(&h, 1, "m1", round);
                report_round(&h, round, "m1", format!("Report from round {round}"));
                let pending = h.store().stage_tasks(TEAM, 4, None).unwrap().rows;
                stage = pending.iter().find(|row| row.target == 1).unwrap().node;
                let (pre, exit) = evidence(&h, &lead(), 1);
                assert_eq!(exit, 0, "{pre}");
                assert_eq!(pre["stages"][0]["node"], stage);
                assert!(pre["stages"][0]["verdict"].is_null());
                deliver_review(&h, stage, round);
            }
            verdict(
                &h,
                stage,
                round == 25,
                &format!("Findings from round {round}"),
                0xa400 + round,
            );
            h.observe(stage, TaskStage::Done);
            expected.push(stage);
            if round == 24 {
                let (reply, exit) = evidence(&h, &lead(), 1);
                assert_eq!(exit, 0, "{reply}");
                assert_eq!(reply["stages"].as_array().unwrap().len(), 24);
                assert_eq!(reply["more_stages"], false);
            }
        }
        let (reply, exit) = evidence(&h, &lead(), 1);
        assert_eq!(exit, 0, "{reply}");
        assert_eq!(reply["revision"], h.revision());
        assert_eq!(reply["report"]["generation"], 25);
        assert_eq!(reply["report"]["member"], "m1");
        assert_eq!(reply["report"]["text"], "Report from round 25");
        assert_eq!(reply["more_stages"], true);
        let rows = reply["stages"].as_array().unwrap();
        assert_eq!(rows.len(), 24);
        for ((row, id), round) in rows.iter().zip(expected.iter().rev()).zip((2..=25).rev()) {
            assert_eq!(row["node"], *id);
            assert_eq!(row["kind"], "review");
            assert_eq!(row["round"], round);
            assert_eq!(row["verdict"]["member"], "m3");
            assert_eq!(row["verdict"]["pass"], round == 25);
            assert_eq!(
                row["verdict"]["text"],
                format!("Findings from round {round}")
            );
        }
        // Each of the read's lookups is an indexed search without a sort.
        let store = h.store();
        let db = store.db.as_ref().unwrap();
        for (query, node) in [
            ("SELECT node FROM stage_tasks WHERE team=?1 AND target=?2 ORDER BY node DESC LIMIT 25", 1),
            ("SELECT generation,text,commit_id,notes FROM reports WHERE team=?1 AND node=?2 ORDER BY generation DESC LIMIT 1", 1),
            ("SELECT generation,actor,pass,text FROM verdicts WHERE team=?1 AND node=?2 ORDER BY generation DESC LIMIT 1", stage),
            ("SELECT member,state FROM effects WHERE team=?1 AND node=?2 AND generation=25 ORDER BY seq DESC LIMIT 1", 1),
        ] {
            let mut statement = db.prepare(&format!("EXPLAIN QUERY PLAN {query}")).unwrap();
            let plan = statement
                .query_map(params![TEAM, node], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .join(" ");
            assert!(plan.contains("SEARCH") && plan.contains("INDEX") && !plan.contains("TEMP B-TREE"), "{query}: {plan}");
        }
    }
}
