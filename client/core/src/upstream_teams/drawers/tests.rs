//! Valid rows are CLI fixtures pinned by the root product-writer contract.
//! Tests named `handwritten_*` deliberately corrupt that evidence.
use super::*;
use serde_json::json;

const TEAM: &str = "00000000-0000-4000-8000-00000000beef";
fn asked(kind: DrawerKind) -> DrawerRead {
    DrawerRead {
        team: TEAM.into(),
        kind,
        after: None,
        recorded_timings: false,
        timing_seqs: None,
        timing_next: None,
    }
}

fn fixture(kind: DrawerKind) -> Value {
    serde_json::from_str(match kind {
        DrawerKind::Feed => {
            include_str!("../../../../../crates/team-plan/fixtures/cli-journal.json")
        }
        DrawerKind::Inbox => {
            include_str!("../../../../../crates/team-plan/fixtures/cli-inbox.json")
        }
        DrawerKind::Cards => {
            include_str!("../../../../../crates/team-plan/fixtures/cli-cards.json")
        }
    })
    .unwrap()
}
fn exit(value: Value) -> Exit {
    Exit::Code {
        code: 0,
        stdout: serde_json::to_vec(&value).unwrap(),
        stderr: String::new(),
    }
}

#[test]
fn product_written_drawer_pages_reach_the_client() {
    let mut pages = serde_json::Map::new();
    for kind in [DrawerKind::Feed, DrawerKind::Inbox, DrawerKind::Cards] {
        let raw = fixture(kind);
        let page = decode(&asked(kind), exit(raw.clone())).unwrap();
        assert_eq!(page.team, TEAM);
        assert_eq!(page.kind, kind);
        assert_eq!(page.rows.len(), raw["rows"].as_array().unwrap().len());
        assert_eq!(page.rows[0].text, raw["rows"][0]["text"]);
        if kind == DrawerKind::Cards {
            assert_eq!(page.rows[0].card.as_ref().unwrap().team, TEAM);
        }
        pages.insert(
            match kind {
                DrawerKind::Feed => "feed",
                DrawerKind::Inbox => "inbox",
                DrawerKind::Cards => "cards",
            }
            .into(),
            serde_json::to_value(page).unwrap(),
        );
    }
    let pinned: Value = serde_json::from_str(include_str!("../fixtures/drawers.json")).unwrap();
    assert_eq!(
        Value::Object(pages),
        pinned,
        "the UI uses the core's product-written projection"
    );
}

fn staffing_inbox() -> Value {
    serde_json::from_str(include_str!("../fixtures/lead-staffing-inbox.json")).unwrap()
}

#[test]
fn product_staffing_inbox_keeps_the_localization_parameters() {
    let page = decode(&asked(DrawerKind::Inbox), exit(staffing_inbox())).unwrap();
    assert_eq!(page.rows.len(), 1);
    let row = &page.rows[0];
    assert_eq!(row.kind, "lead_member_added:m4");
    assert_eq!(row.by, "lead");
    assert_eq!(row.state, "closed");
    assert_eq!(row.notes.len(), 1);
    let note = &row.notes[0];
    assert_eq!(note.code, "lead_member_added");
    assert_eq!(note.parameters, ["m4", "Codex", "reviewer"]);
    assert_eq!(note.text.as_deref(), Some(row.text.as_str()));
}

#[test]
fn handwritten_staffing_parameters_must_be_three_short_ascii_tokens() {
    for parameters in [
        json!(["m4", "Codex"]),
        json!(["m4", "Codex", "reviewer", "extra"]),
        json!(["m4", "Codex", 13]),
        json!(["m4", "Codex", ""]),
        json!(["m4", "Codex", "r".repeat(65)]),
        json!(["m4", "Codex", "reviewer\u{202e}"]),
    ] {
        let mut raw = staffing_inbox();
        raw["rows"][0]["notes"][0]["parameters"] = parameters;
        let page = decode(&asked(DrawerKind::Inbox), exit(raw)).unwrap();
        assert_eq!(page.rows.len(), 1);
        let row = &page.rows[0];
        let note = &row.notes[0];
        assert_eq!(note.code, "lead_member_added");
        assert_eq!(note.text.as_deref(), Some(row.text.as_str()));
        assert!(note.parameters.is_empty());
    }
}

#[test]
fn each_drawer_uses_the_existing_page_and_child_bounds() {
    for kind in [DrawerKind::Feed, DrawerKind::Inbox, DrawerKind::Cards] {
        let mut read = asked(kind);
        read.after = Some("t1.example".into());
        let call = invocation(&read).unwrap();
        assert_eq!(
            call.args,
            [
                "team",
                kind.command(),
                "--team",
                TEAM,
                "--limit",
                "200",
                "--after",
                "t1.example",
                "--json"
            ]
        );
        assert_eq!(call.stdin, None);
        assert_eq!(call.request_id, None);
        assert_eq!(call.timeout, std::time::Duration::from_secs(15));
        read.team = "--other".into();
        assert!(invocation(&read).is_err());
        read.team = TEAM.into();
        read.after = Some("--option".into());
        assert!(invocation(&read).is_err());
    }
}

#[test]
fn handwritten_foreign_malformed_and_oversized_pages_never_become_empty_success() {
    for kind in [DrawerKind::Feed, DrawerKind::Inbox, DrawerKind::Cards] {
        let raw = fixture(kind);
        let mut foreign = raw.clone();
        foreign["team"] = json!("other");
        let mut missing = raw.clone();
        missing["rows"][0].as_object_mut().unwrap().remove("seq");
        let mut too_many = raw.clone();
        too_many["rows"] = json!(vec![raw["rows"][0].clone(); PAGE as usize + 1]);
        let mut bad_next = raw.clone();
        bad_next["next"] = json!("x".repeat(CURSOR_BYTES + 1));
        for broken in [foreign, missing, too_many, bad_next] {
            assert!(decode(&asked(kind), exit(broken)).is_err());
        }
    }
}

#[test]
fn handwritten_long_text_is_marked_and_the_cursor_is_preserved_exactly() {
    let mut read = asked(DrawerKind::Inbox);
    read.after = Some("t1.original".into());
    let mut raw = fixture(DrawerKind::Inbox);
    raw["next"] = json!("t1.next");
    raw["rows"][0]["text"] = json!("я".repeat(TEXT_CHARS + 1));
    let page = decode(&read, exit(raw)).unwrap();
    assert_eq!(page.next.as_deref(), Some("t1.next"));
    assert_eq!(page.after.as_deref(), Some("t1.original"));
    assert_eq!(page.rows[0].text.chars().count(), TEXT_CHARS);
    assert!(page.rows[0].text_truncated);
}

/// zerus-rzvct G6/G7: a feed row says who acted (its actor's kind, so the
/// window can tell the person, the team service and a member apart) and which
/// task and member it is about, from the product-written journal's refs.
#[test]
fn feed_rows_name_their_actor_kind_task_and_member() {
    let page = decode(&asked(DrawerKind::Feed), exit(fixture(DrawerKind::Feed))).unwrap();
    let rows = serde_json::to_value(&page).unwrap()["rows"].clone();
    let row = |seq: u64| -> Value {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["seq"] == seq)
            .unwrap()
            .clone()
    };
    let facts = |seq: u64| {
        let row = row(seq);
        (
            row["by_kind"].clone(),
            row["subject_node"].clone(),
            row["subject_member"].clone(),
        )
    };
    // create (person), set_member_session member:lead (service),
    // handout_intent node:2 member:m1 (service), ask node:2 (session m1).
    assert_eq!(facts(1), (json!("person"), json!(null), json!(null)));
    assert_eq!(facts(2), (json!("service"), json!(null), json!("lead")));
    assert_eq!(facts(8), (json!("service"), json!(2), json!("m1")));
    assert_eq!(facts(15), (json!("session"), json!(2), json!(null)));
    assert_eq!(row(15)["by"], json!("m1"));
    // add_task names its task by UUID only: no invented node.
    assert_eq!(facts(4), (json!("person"), json!(null), json!(null)));
}

#[test]
fn handwritten_refs_never_invent_a_subject() {
    let mut raw = fixture(DrawerKind::Feed);
    raw["rows"][7]["refs"] = json!(["node:0", "node:x", "member:", 5]);
    raw["rows"][1].as_object_mut().unwrap().remove("refs");
    let page = decode(&asked(DrawerKind::Feed), exit(raw)).unwrap();
    let rows = serde_json::to_value(&page).unwrap()["rows"].clone();
    assert_eq!(rows[7]["subject_node"], json!(null));
    assert_eq!(rows[7]["subject_member"], json!(null));
    assert_eq!(rows[1]["subject_member"], json!(null));
    assert_eq!(rows[1]["by_kind"], json!("service"));
}

#[test]
fn inbox_and_card_rows_carry_their_actor_kind() {
    for (kind, kinds) in [
        (DrawerKind::Inbox, ["service", "session"]),
        (DrawerKind::Cards, ["person", "person"]),
    ] {
        let page = decode(&asked(kind), exit(fixture(kind))).unwrap();
        let rows = serde_json::to_value(&page).unwrap()["rows"].clone();
        for (index, expected) in kinds.iter().enumerate().take(page.rows.len()) {
            assert_eq!(
                rows[index]["by_kind"],
                json!(expected),
                "{kind:?} row {index}"
            );
        }
    }
}
