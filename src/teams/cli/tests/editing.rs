//! Person-positive setup uses an explicit admitted-writer model; reads and
//! Session/negative caller checks still use the production CLI unchanged.
//! The editing commands, projected from rows their product writers created.
use super::harness::*;
use crate::teams::cli::actor::Caller;
use serde_json::json;
fn input(h: &Harness, fields: serde_json::Value) -> String {
    let mut fields = fields.as_object().unwrap().clone();
    fields.insert("team".into(), json!(TEAM));
    fields.insert("request_id".into(), json!(uuid::Uuid::new_v4().to_string()));
    fields.insert("expected_revision".into(), json!(h.revision()));
    serde_json::Value::Object(fields).to_string()
}

#[test]
fn lead_add_member_uses_the_pinned_role_and_existing_member_choices() {
    let h = Harness::new();
    h.create();
    h.bind("lead", LEAD_SESSION);
    let request = input(
        &h,
        json!({"role":"developer","agent":"codex","model":"seat-model","effort":"high",
            "account":{"mode":"named","account":"example-account"}}),
    );
    let lead = Caller::Session(key(LEAD_SESSION));
    let (reply, exit) = h.cli(&lead, "lead-add-member", &request);
    assert_eq!(exit, 0, "{reply}");
    let member = h
        .store()
        .member(TEAM, reply["member"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        (member.role.as_str(), member.agent.as_str()),
        ("developer", "codex")
    );
    let launch = member.launch.as_ref().unwrap();
    assert_eq!(
        (launch.model.as_str(), launch.effort.as_str()),
        ("seat-model", "high")
    );
    assert_eq!(
        launch.account,
        zerus_team_plan::catalog::AccountChoice::Named {
            account: "example-account".to_owned(),
        }
    );
    let inbox = h.store().inbox(TEAM, 200, None).unwrap().rows;
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].kind, "lead_member_added:m4");
    assert_eq!(inbox[0].state, "closed");
    assert_eq!(inbox[0].notes[0].parameters, ["m4", "Codex", "developer"]);
    assert_eq!(reply["item"].as_u64(), Some(inbox[0].seq));
    assert!(h.store().cards(TEAM, 200, None).unwrap().rows.is_empty());
    let journal = h.store().journal(TEAM, 200, None).unwrap().rows;
    assert_eq!(
        journal
            .iter()
            .filter(
                |row| row.kind == "lead_add_member" && row.refs.contains(&"member:m4".to_owned())
            )
            .count(),
        1
    );
    assert_eq!(h.cli(&lead, "lead-add-member", &request), (reply, exit));
    assert_eq!(h.store().inbox(TEAM, 200, None).unwrap().rows, inbox);
    assert_eq!(h.store().journal(TEAM, 200, None).unwrap().rows, journal);
}

const LEAD_STAFFING_INBOX: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/client/core/src/upstream_teams/fixtures/lead-staffing-inbox.json"
);

fn lead_staffing_inbox() -> serde_json::Value {
    let h = Harness::new();
    h.create();
    h.bind("lead", LEAD_SESSION);
    h.apply(
        &member("lead", LEAD_SESSION),
        crate::teams::ops::Operation::LeadAddMember {
            role: "reviewer".into(),
            agent: "codex".into(),
            model: String::new(),
            effort: String::new(),
            account: Default::default(),
        },
    );
    let lead = Caller::Session(key(LEAD_SESSION));
    let (inbox, exit) = h.cli(&lead, &format!("inbox --team {TEAM}"), "");
    assert_eq!(exit, 0, "{inbox}");
    inbox
}

#[test]
fn staffing_inbox_fixture_matches_the_product_writer_and_cli_read() {
    let pinned: serde_json::Value =
        serde_json::from_slice(&std::fs::read(LEAD_STAFFING_INBOX).unwrap()).unwrap();
    assert_eq!(lead_staffing_inbox(), pinned);
}

#[test]
#[ignore = "regenerates the staffing inbox from the product writer and CLI"]
fn regenerate_lead_staffing_inbox_fixture() {
    let mut text = serde_json::to_string_pretty(&lead_staffing_inbox()).unwrap();
    text.push('\n');
    std::fs::write(LEAD_STAFFING_INBOX, text).unwrap();
}

#[test]
fn lead_add_member_refuses_other_callers_and_a_stale_revision() {
    let h = Harness::new();
    h.create();
    h.bind("lead", LEAD_SESSION);
    h.bind("m1", M1_SESSION);
    let fields = json!({"role":"reviewer","agent":"codex"});
    let request = input(&h, fields.clone());
    let before = h.revision();
    for (caller, error) in [
        (Caller::Session(key(M1_SESSION)), "not_allowed"),
        (Caller::Person, "person_gesture_required"),
    ] {
        let (reply, exit) = h.cli(&caller, "lead-add-member", &request);
        assert_eq!((exit, code(&reply)), (1, error), "{reply}");
    }
    let lead = Caller::Session(key(LEAD_SESSION));
    let (reply, exit) = h.cli(&lead, "add-member", &request);
    assert_eq!((exit, code(&reply)), (1, "not_allowed"), "{reply}");
    assert_eq!(h.revision(), before);
    h.apply(
        &person(),
        crate::teams::ops::Operation::SetGoal {
            goal: "Updated".into(),
        },
    );
    let (reply, exit) = h.cli(&lead, "lead-add-member", &request);
    assert_eq!((exit, code(&reply)), (1, "stale_revision"), "{reply}");
    assert_eq!(h.revision(), before + 1);
    assert_eq!(h.store().members(TEAM, 200, None).unwrap().rows.len(), 4);
    assert!(h.store().inbox(TEAM, 200, None).unwrap().rows.is_empty());
}

#[test]
fn lead_add_member_refuses_a_missing_role_an_unsupported_agent_and_the_cap() {
    let h = Harness::new();
    h.create();
    h.bind("lead", LEAD_SESSION);
    let lead = Caller::Session(key(LEAD_SESSION));
    let mut settings = h.store().team(TEAM).unwrap().unwrap().settings;
    settings.caps.max_members = 3;
    h.apply(
        &person(),
        crate::teams::ops::Operation::ChangeSettings { settings },
    );
    let before = h.revision();
    for (role, agent, error) in [
        ("missing", "codex", "unknown_role"),
        ("reviewer", "opencode", "invalid_state"),
        ("reviewer", "codex", "over_cap"),
    ] {
        let (reply, exit) = h.cli(
            &lead,
            "lead-add-member",
            &input(&h, json!({"role":role,"agent":agent})),
        );
        assert_eq!((exit, code(&reply)), (1, error), "{reply}");
    }
    assert_eq!(h.revision(), before);
    assert_eq!(h.store().members(TEAM, 200, None).unwrap().rows.len(), 4);
    assert!(h.store().inbox(TEAM, 200, None).unwrap().rows.is_empty());
}

#[test]
fn editing_commands_return_member_identity_and_status_keeps_pinned_choices() {
    let h = Harness::new();
    h.create();
    let bundle = h.store().team(TEAM).unwrap().unwrap().bundle;
    let add = input(
        &h,
        json!({"role":"developer","agent":"codex","model":"seat-model","effort":"high","account":{"mode":"named","account":"example-account"}}),
    );
    let (reply, code) = h.native_command("add-member", &add);
    assert_eq!(code, 0, "{reply}");
    let member = reply["member"].as_str().unwrap();
    assert_eq!(
        h.native_command("add-member", &add).0,
        reply,
        "saved request was not replayed"
    );
    let (status, exit) = h.cli(&Caller::Person, &format!("status --team {TEAM}"), "");
    assert_eq!(exit, 0, "{status}");
    let row = status["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == member)
        .unwrap();
    assert_eq!(row["model"], "seat-model");
    assert_eq!(row["effort"], "high");
    assert_eq!(
        row["account_choice"],
        json!({"mode":"named","account":"example-account"})
    );
    assert_eq!(row["open_work"], 0);
    assert!(status["member_roles"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["id"] == "developer"));
    assert_eq!(h.store().team(TEAM).unwrap().unwrap().bundle, bundle);
    for (command, body) in [
        ("rename-team", json!({"name":"Readable team"})),
        ("set-goal", json!({"goal":"New goal"})),
    ] {
        let (reply, exit) = h.native_command(command, &input(&h, body));
        assert_eq!(exit, 0, "{reply}");
    }
    let team = h.store().team(TEAM).unwrap().unwrap();
    assert_eq!(
        (team.name.as_str(), team.goal.as_str()),
        ("Readable team", "New goal")
    );
    let (reply, exit) = h.native_command(
        "swap-member",
        &input(&h, json!({"member":member,"agent":"claude"})),
    );
    assert_eq!(exit, 1);
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Stop the member first"));
}
#[test]
fn status_counts_current_work_outside_the_displayed_queue_page() {
    let h = super::harness::seeded();
    let first = h.store().member_edit_work(TEAM).unwrap();
    let (reply, exit) = h.cli(&Caller::Person, &format!("status --team {TEAM}"), "");
    assert_eq!(exit, 0, "{reply}");
    for m in reply["members"].as_array().unwrap() {
        let id = m["id"].as_str().unwrap();
        assert_eq!(m["open_work"], json!(first.get(id).copied().unwrap_or(0)));
    }
    assert_eq!(
        first.get("m1"),
        Some(&1),
        "the reported task still belongs to its holder"
    );
    assert_eq!(first.values().sum::<usize>(), 1);
    // An agent's edit request cannot gain authority from its request fields.
    let (reply, exit) = h.cli(
        &Caller::Session(key(M1_SESSION)),
        "rename-team",
        &input(&h, json!({"name":"Forged"})),
    );
    assert_eq!(exit, 1);
    assert_eq!(reply["error"]["code"], "not_allowed");
}

#[test]
fn a_long_goal_is_projected_completely_for_editing_without_widening_the_bar() {
    let h = Harness::new();
    h.create();
    let goal = "x".repeat(9000);
    let (reply, exit) = h.native_command("set-goal", &input(&h, json!({"goal":goal})));
    assert_eq!(exit, 0, "{reply}");
    let (status, exit) = h.cli(&Caller::Person, &format!("status --team {TEAM}"), "");
    assert_eq!(exit, 0, "{status}");
    let view = zerus_client_core::upstream_teams::view::team(&status, &json!({"rows":[]}));
    assert_eq!(view.edit_goal.as_deref(), Some(goal.as_str()));
    assert_eq!(
        view.bar.goal.chars().count(),
        zerus_client_core::upstream_teams::view::TEXT_CHARS
    );
}
