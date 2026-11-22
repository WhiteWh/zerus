//! Valid replies are the CLI fixture the root product-writer test pins
//! (`crates/team-plan/fixtures/cli-evidence.json`: m1's report, m3's passing
//! review). Tests named `handwritten_*` corrupt that evidence on purpose.
use super::*;
use serde_json::json;

const TEAM: &str = "00000000-0000-4000-8000-00000000beef";
fn reply() -> Value {
    serde_json::from_str(include_str!(
        "../../../../../crates/team-plan/fixtures/cli-evidence.json"
    ))
    .unwrap()
}

#[test]
fn product_written_evidence_reaches_the_client() {
    let evidence = project(&reply(), TEAM, 1).unwrap();
    let report = evidence.report.as_ref().unwrap();
    assert_eq!(report.member.as_deref(), Some("m1"));
    assert_eq!(report.commit, "0123456789abcdef0123456789abcdef01234567");
    assert_eq!(
        report.text,
        "Trimmed the label and added a case for an empty one."
    );
    assert!(!report.text_truncated);
    assert_eq!(evidence.stages.len(), 1);
    let stage = &evidence.stages[0];
    assert_eq!(
        (stage.node, stage.kind.as_deref(), stage.round),
        (2, Some("review"), 1)
    );
    let verdict = stage.verdict.as_ref().unwrap();
    assert_eq!(verdict.member.as_deref(), Some("m3"));
    assert!(verdict.pass);
    assert!(verdict
        .text
        .starts_with("Reviewed every acceptance criterion."));
    assert!(!evidence.more_stages);
    let pinned: Value = serde_json::from_str(include_str!("../fixtures/evidence.json")).unwrap();
    assert_eq!(
        serde_json::to_value(&evidence).unwrap(),
        pinned,
        "the UI uses the core's projection of the product reply"
    );
}

#[test]
fn the_command_names_one_task_of_one_team() {
    assert_eq!(
        args(TEAM, 7).unwrap(),
        ["team", "evidence", "--team", TEAM, "--node", "7", "--json"]
    );
    assert!(args("--other", 7).is_err());
    assert!(args(TEAM, 0).is_err());
    assert!(args(TEAM, u64::from(u32::MAX) + 1).is_err());
}

#[test]
fn handwritten_foreign_and_malformed_answers_are_refused_whole() {
    let mut cases = Vec::new();
    for (pointer, value) in [
        ("/team", json!("00000000-0000-4000-8000-00000000aaaa")),
        ("/node", json!(2)),
        ("/ok", json!(false)),
        ("/revision", json!("19")),
        ("/report/text", json!(5)),
        ("/report/commit", Value::Null),
        ("/report/notes", json!("none")),
        ("/report/member", json!(7)),
        ("/stages/0/verdict/pass", json!("yes")),
        ("/stages/0/verdict/member", json!("")),
        ("/stages/0/verdict", json!("passed")),
        ("/stages/0/round", Value::Null),
        ("/stages/0/state", Value::Null),
        ("/stages", json!({})),
        ("/more_stages", Value::Null),
    ] {
        let mut broken = reply();
        *broken.pointer_mut(pointer).unwrap() = value;
        cases.push((pointer, broken));
    }
    let mut missing = reply();
    missing.as_object_mut().unwrap().remove("report");
    cases.push(("report removed", missing));
    let mut too_many = reply();
    too_many["stages"] = json!(vec![reply()["stages"][0].clone(); MAX_STAGES + 1]);
    cases.push(("stages over the bound", too_many));
    for (what, broken) in cases {
        assert!(project(&broken, TEAM, 1).is_err(), "{what} was accepted");
    }
}

#[test]
fn handwritten_absent_evidence_stays_absent() {
    let mut empty = reply();
    empty["report"] = Value::Null;
    empty["stages"] = json!([]);
    let evidence = project(&empty, TEAM, 1).unwrap();
    assert_eq!(evidence.report, None);
    assert!(evidence.stages.is_empty());
    let mut unjudged = reply();
    unjudged["stages"][0]["verdict"] = Value::Null;
    unjudged["stages"][0]["kind"] = Value::Null;
    let stage = &project(&unjudged, TEAM, 1).unwrap().stages[0];
    assert_eq!((stage.kind.clone(), stage.verdict.clone()), (None, None));
}

#[test]
fn handwritten_long_texts_are_cut_and_marked() {
    let mut long = reply();
    long["report"]["text"] = json!("r".repeat(TEXT_CHARS + 5));
    long["stages"][0]["verdict"]["text"] = json!("v".repeat(TEXT_CHARS + 1));
    let evidence = project(&long, TEAM, 1).unwrap();
    let report = evidence.report.unwrap();
    assert_eq!(
        (report.text.chars().count(), report.text_truncated),
        (TEXT_CHARS, true)
    );
    let verdict = evidence.stages[0].verdict.clone().unwrap();
    assert_eq!(
        (verdict.text.chars().count(), verdict.text_truncated),
        (TEXT_CHARS, true)
    );
}
