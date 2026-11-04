//! Canonical desktop rows cross the same serialized projection the webview reads.
use serde_json::{json, Value};
use std::path::PathBuf;
use zerus_client_core::{view, CatalogState, ComputerId, ViewRow};
use zerus_watch_protocol::Frame;

const FILTERS: &str =
    include_str!("../../../mobile/android/app/src/test/resources/session-filter-contract.json");
const QUESTIONS: &str =
    include_str!("../../../mobile/android/app/src/test/resources/optional-question-contract.json");

fn input(raw: &Value, name: &str) -> Value {
    let mut raw = raw.clone();
    raw["name"] = json!(format!("codex/contract/{name}"));
    raw["cmd"] = json!("codex");
    raw
}

fn projected(row: &ViewRow) -> Value {
    let wire = serde_json::to_value(row).unwrap();
    // The frontend's catalog, marks, filters and chip read these fields. No
    // status classification or optional-count interpretation is copied here.
    let fields = [
        "name",
        "agent",
        "status",
        "caption",
        "phase",
        "attention",
        "unread",
        "needs_action",
        "needs_attention",
        "child_attention",
        "acknowledged_status",
        "acknowledged_caption",
        "attention_fingerprint",
        "created",
        "state",
        "tracked",
        "activity",
        "process_state",
        "background",
        "run_id",
        "conversation_id",
        "reply_id",
        "attention_signature",
        "shared_read",
        "read_reply_id",
        "read_attention_signature",
        "unread_candidate",
        "optional_question_count",
    ];
    Value::Object(
        fields
            .into_iter()
            .map(|field| (field.into(), wire[field].clone()))
            .collect(),
    )
}

fn fixture() -> Value {
    let filters: Value = serde_json::from_str(FILTERS).unwrap();
    let questions: Value = serde_json::from_str(QUESTIONS).unwrap();
    let cases = |contract: &Value| -> Value {
        Value::Object(
            contract["cases"]
                .as_array()
                .unwrap()
                .iter()
                .map(|case| {
                    let name = case["name"].as_str().unwrap();
                    (
                        name.into(),
                        projected(&view::row(&input(&case["raw"], name))),
                    )
                })
                .collect(),
        )
    };
    let sequences = questions["desktop_sequences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|sequence| {
            let name = sequence["name"].as_str().unwrap();
            let mut state = CatalogState::new();
            let steps = sequence["steps"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(n, step)| {
                    let raw = input(&step["raw"], name);
                    let value = if n == 0 {
                        json!({"v":1,"seq":1,"t":"snapshot","topic":"catalog","page":0,"last":true,
                    "cursor":1,"items":[{"key":"run:contract","row":raw}]})
                    } else {
                        json!({"v":1,"seq":n+1,"t":"delta","topic":"catalog","cursor":n+1,
                    "upsert":[{"key":"run:contract","row":raw}]})
                    };
                    let frame: Frame = serde_json::from_value(value).unwrap();
                    state.apply(&ComputerId::local(), &frame);
                    projected(&state.rows()["run:contract"])
                })
                .collect();
            (name.into(), Value::Array(steps))
        })
        .collect();
    json!({"filters":cases(&filters),"questions":cases(&questions),"sequences":Value::Object(sequences)})
}

fn fixture_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/session-contract-view.json")
}

#[test]
fn the_serialized_shared_contract_projection_is_current() {
    // One compact row per case, like the existing marks-overlay fixture.
    let sections = fixture()
        .as_object()
        .unwrap()
        .iter()
        .map(|(section, cases)| {
            let rows = cases
                .as_object()
                .unwrap()
                .iter()
                .map(|(name, row)| format!("    {}: {}", serde_json::to_string(name).unwrap(), row))
                .collect::<Vec<_>>()
                .join(",\n");
            format!("  \"{section}\": {{\n{rows}\n  }}")
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let current = format!("{{\n{sections}\n}}\n");
    if std::env::var_os("ZERUS_UPDATE_FIXTURES").is_some() {
        std::fs::write(fixture_file(), &current).unwrap();
    }
    assert_eq!(std::fs::read_to_string(fixture_file()).unwrap(), current);
}

#[test]
fn working_one_optional_count_crosses_the_frontend_wire() {
    let contract: Value = serde_json::from_str(QUESTIONS).unwrap();
    let case = contract["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "working-one")
        .unwrap();
    let wire = serde_json::to_value(view::row(&input(&case["raw"], "working-one"))).unwrap();
    assert_eq!(wire["optional_question_count"], case["count"]);
    assert!(
        wire.get("notify").is_none(),
        "native notification internals stay private"
    );
}
