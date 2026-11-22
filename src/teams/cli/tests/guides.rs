//! The team guide pages are executed. Every `hgs team` example in them runs
//! against a live temporary team with the caller the page is written for, so a
//! stale command or an incomplete envelope fails the suite.
use super::harness::*;
use crate::teams::cli::actor::Caller;
use serde_json::{json, Value};
use std::cell::Cell;

const MEMBER_PAGE: &str = include_str!("../../../agent_guide/team.md");
const LEAD_PAGE: &str = include_str!("../../../agent_guide/team-lead.md");
const REWORK_PAGE: &str = include_str!("../../../agent_guide/team-rework.md");
const PLANNING_PAGE: &str = include_str!("../../../agent_guide/team-planning.md");
const M2_SESSION: &str = "launch_id:00000000-0000-4000-8000-000000000032";
mod planning;

#[derive(Debug)]
struct Example {
    args: Vec<String>,
    input: Option<String>,
}

fn line_start(page: &str, at: usize) -> usize {
    page[..at].rfind('\n').map_or(0, |index| index + 1)
}

/// A line that begins an example: nothing before `hgs team` but indentation,
/// or a numbered list marker such as `1. `.
fn begins_example(page: &str, at: usize) -> bool {
    let prefix = page[line_start(page, at)..at].trim();
    if prefix.is_empty() {
        return true;
    }
    let Some(number) = prefix.strip_suffix('.') else {
        return false;
    };
    !number.is_empty() && number.chars().all(|c| c.is_ascii_digit())
}

/// The JSON object an example feeds on stdin, when the next non-empty line
/// starts one. Mere prose after a command is not an example's input.
fn input_after(page: &str, from: usize) -> Option<String> {
    let rest = &page[from..];
    let mut offset = 0;
    for line in rest.split_inclusive('\n').take(4) {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            offset += line.len();
            continue;
        }
        let start = offset + (line.len() - trimmed.len());
        if !trimmed.starts_with('{') {
            return None;
        }
        let text = &rest[start..];
        let mut depth = 0i32;
        for (index, character) in text.char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(text[..=index].to_owned());
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    None
}

fn examples(page: &str) -> Vec<Example> {
    let marker = "hgs team ";
    let mut found = Vec::new();
    let mut search = 0;
    while let Some(offset) = page[search..].find(marker) {
        let at = search + offset;
        search = at + marker.len();
        if !begins_example(page, at) {
            continue;
        }
        let after = at + marker.len();
        let end = page[after..]
            .find('\n')
            .map_or(page.len(), |line| after + line);
        let text = &page[after..end];
        // The executable words stop at the two-space gap before the prose.
        let words = text.split("  ").next().unwrap_or("").trim();
        let args: Vec<String> = words.split_whitespace().map(str::to_owned).collect();
        assert!(!args.is_empty(), "an example names no command: {text}");
        found.push(Example {
            args,
            input: input_after(page, end),
        });
    }
    found
}

fn line(example: &Example) -> String {
    example
        .args
        .iter()
        .map(|word| {
            if word == "TEAM" {
                TEAM.to_owned()
            } else {
                word.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The node an example acts on: a fresh reported task for each decision.
fn node_for(command: &str) -> u32 {
    match command {
        "verdict" => 2,
        "return" => 2,
        "reassign" => 3,
        _ => 1,
    }
}

fn prepare(example: &Example, harness: &Harness, item: u64, steps: &Cell<u32>) -> String {
    let command = example.args[0].as_str();
    let text = example
        .input
        .as_deref()
        .expect("a write example carries a request");
    let mut value: Value = serde_json::from_str(text).expect("a guide example is one JSON object");
    let object = value.as_object_mut().expect("a request is an object");
    // The envelope is mandatory on every write.
    for field in ["request_id", "team", "expected_revision"] {
        assert!(
            object.contains_key(field),
            "`{command}` omits {field}: {text}"
        );
    }
    let step = steps.get() + 1;
    steps.set(step);
    object.insert("request_id".into(), json!(uid(0x5000 + step)));
    object.insert("team".into(), json!(TEAM));
    object.insert("expected_revision".into(), json!(harness.revision()));
    let node = node_for(command);
    if object.contains_key("node") {
        object.insert("node".into(), json!(node));
    }
    if object.contains_key("generation") {
        let generation = harness
            .store()
            .generation(TEAM, node)
            .unwrap()
            .expect("the node's generation");
        object.insert("generation".into(), json!(generation));
    }
    if object.contains_key("item") {
        object.insert("item".into(), json!(item));
    }
    value.to_string()
}

#[test]
fn every_example_of_both_pages_runs() {
    let harness = Harness::new();
    harness.create();
    harness.bind("lead", LEAD_SESSION);
    harness.bind("m1", M1_SESSION);
    harness.bind("m2", M2_SESSION);
    for node in 1..=3 {
        harness.reported(node);
    }
    let known = [
        "current",
        "ask",
        "verdict",
        "inbox",
        "queue",
        "status",
        "accept",
        "return",
        "reassign",
        "reply",
        "lead-add-member",
    ];
    let steps = Cell::new(0);
    let mut item = 0u64;
    let mut verdicts = 0;
    for example in examples(MEMBER_PAGE) {
        let command = example.args[0].as_str();
        assert!(known.contains(&command), "the member page names {command}");
        if command == "verdict" {
            let (stage_harness, stage) = super::verdict::stage_harness();
            assert_eq!(node_for(command), stage);
            let input = prepare(&example, &stage_harness, 0, &steps);
            let (reply, exit) = stage_harness.cli(
                &Caller::Session(key(super::verdict::REVIEWER_SESSION)),
                &line(&example),
                &input,
            );
            assert_eq!(exit, 0, "member page verdict: {reply}");
            verdicts += 1;
            continue;
        }
        let input = example
            .input
            .as_ref()
            .map(|_| prepare(&example, &harness, 0, &steps));
        let (reply, exit) = harness.cli(
            &Caller::Session(key(M1_SESSION)),
            &line(&example),
            input.as_deref().unwrap_or(""),
        );
        assert_eq!(exit, 0, "member page `{command}`: {reply}");
        if command == "ask" {
            item = reply["item"].as_u64().expect("the question's item");
            assert!(item > 0);
        }
    }
    for example in examples(LEAD_PAGE) {
        let command = example.args[0].as_str();
        assert!(known.contains(&command), "the lead page names {command}");
        let input = example
            .input
            .as_ref()
            .map(|_| prepare(&example, &harness, item, &steps));
        let (reply, exit) = harness.cli(
            &Caller::Session(key(LEAD_SESSION)),
            &line(&example),
            input.as_deref().unwrap_or(""),
        );
        assert_eq!(exit, 0, "lead page `{command}`: {reply}");
    }
    assert!(item > 0, "the member page's ask was never answered");
    assert_eq!(verdicts, 1, "the member page's verdict was never executed");
}

#[test]
fn the_pages_keep_their_bounds_and_name_the_new_commands() {
    for (name, page) in [
        ("team", MEMBER_PAGE),
        ("team-lead", LEAD_PAGE),
        ("team-planning", PLANNING_PAGE),
        ("team-rework", REWORK_PAGE),
    ] {
        assert!(page.len() <= 2400, "{name} has {} bytes", page.len());
        assert!(page.is_ascii(), "{name}");
        assert!(page.ends_with('\n'));
        assert!(page.lines().all(|line| line.len() <= 100), "{name}");
    }
    assert!(examples(MEMBER_PAGE)
        .iter()
        .any(|example| example.args[0] == "verdict"));
    let lead = examples(LEAD_PAGE)
        .into_iter()
        .map(|example| example.args[0].clone())
        .collect::<Vec<_>>();
    assert!(lead.contains(&"accept".to_owned()));
    assert!(lead.contains(&"return".to_owned()));
    assert!(lead.contains(&"reassign".to_owned()));
    assert!(lead.contains(&"reply".to_owned()));
    assert!(lead.contains(&"lead-add-member".to_owned()));
}

/// This guide example runs inside the real operation-written rework fixture,
/// in the exact bound lead's scope, before its correction is approved.
#[cfg(unix)]
pub(super) fn correction_edit(h: &Harness, base: &str, rework: &str, generation: u64) {
    let read = h.store().phase_draft(TEAM, 1).unwrap();
    let proposal = read.rework.unwrap().draft.unwrap();
    let examples = examples(REWORK_PAGE);
    assert_eq!(examples.len(), 1);
    let example = &examples[0];
    assert_eq!(example.args[0], "edit-phase-corrections");
    let mut body: Value = serde_json::from_str(example.input.as_deref().unwrap()).unwrap();
    body["request_id"] = json!(uid(11_000 + h.revision() as u32));
    body["team"] = json!(TEAM);
    body["expected_revision"] = json!(h.revision());
    body["publication"] = json!(base);
    body["rework"] = json!(rework);
    body["generation"] = json!(generation);
    body["draft"]["barrier"] = json!(read.phase.barrier);
    for (i, node) in body["draft"]["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        node["id"] = json!(proposal.nodes[i].id);
    }
    body["draft"]["nodes"][1]["deps"][0]["node"] = json!(proposal.nodes[0].id);
    let (result, exit) = h.cli(
        &Caller::Session(key(LEAD_SESSION)),
        &line(example),
        &body.to_string(),
    );
    assert_eq!(exit, 0, "lead correction guide: {result}");
}
