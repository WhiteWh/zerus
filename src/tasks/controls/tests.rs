use super::*;
use serde_json::json;

const ID: &str = "00000000-0000-4000-8000-000000000001";
fn executor() -> Value {
    json!({"computer":ID,"session_id":ID,"run":"original-run","birth":"original-birth","conversation":"original-conversation","child":null})
}
fn plan() -> Value {
    json!({"schema":1,"request_id":ID,"action":"pause","target":{"scope":"attempt","project":"project-one","task":ID,"attempt":ID},"observed_pauses":[],"observed_stops":[]})
}
fn execute() -> Value {
    let value = json!({"schema":1,"request":{"id":ID,"admission":"0".repeat(64),"action":"pause","executor":executor(),"attempt":{"project":"project-one","task":ID,"attempt":ID,"generation":1,"fingerprint":"a".repeat(64)},"slot":{"id":ID,"revision":1},"task_version":"original-version","turn":"original-turn","observed_pauses":[],"observed_stops":[]},
        "caller":{"kind":"person"},"configuration":"b".repeat(64),"store_identity":"c".repeat(64),"cli_image":"d".repeat(64),"loaded_adapter":"e".repeat(64)});
    serde_json::to_value(
        serde_json::from_value::<wire::Plan>(value)
            .unwrap()
            .seal()
            .unwrap(),
    )
    .unwrap()
}
fn read(action: &str, value: &Value) -> std::result::Result<Command, String> {
    wire::read(action, &serde_json::to_vec(value).unwrap())
}

#[test]
fn scoped_acknowledgement_refuses_missing_duplicate_or_extra_origins() {
    let args = |values: &[&str]| values.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let b = wire::begin(&args(&["orchard/T-234567", "--attempt", ID, "--json"])).unwrap();
    assert_eq!((&*b.reference, &*b.attempt), ("orchard/T-234567", ID));
    for bad in [
        vec![],
        args(&["orchard/T-234567"]),
        args(&["orchard/T-234567", "--attempt", "latest"]),
        args(&["orchard/T-234567", "--attempt", ID, "--attempt", ID]),
        args(&["orchard/T-234567", "--attempt", ID, "--json", "--json"]),
        args(&["orchard/T-234567", "--attempt", ID, "--project", "p"]),
    ] {
        assert!(wire::begin(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn closed_wire_requires_exact_schema_targets_and_bounded_pages() {
    assert!(read("plan", &plan()).is_ok());
    assert!(read("execute", &execute()).is_ok());
    let status = json!({"schema":1,"target":{"scope":"executor","executor":executor(),"after":null,"limit":128}});
    assert!(read("status", &status).is_ok());
    for action in ["plan", "execute", "status"] {
        let original = match action {
            "plan" => plan(),
            "execute" => execute(),
            _ => status.clone(),
        };
        for bad in ["unknown", "wrong_schema"] {
            let mut value = original.clone();
            if bad == "unknown" {
                value["actor"] = "person".into();
            } else {
                value["schema"] = 2.into();
            }
            assert!(read(action, &value).is_err());
        }
        assert!(wire::read(action, &vec![b' '; REQUEST_BYTES + 1]).is_err());
    }
    let mut value = status;
    for limit in [0, 129] {
        value["target"]["limit"] = limit.into();
        assert!(read("status", &value).is_err());
    }
    value["target"]["limit"] = 1.into();
    value["target"]["after"] = 0.into();
    assert!(read("status", &value).is_err());
    let mut value = plan();
    value["target"]["attempt"] = "latest".into();
    assert!(read("plan", &value).is_err());
    let mut value = execute();
    value["request"]["slot"]["revision"] = 0.into();
    assert!(read("execute", &value).is_err());
    value = execute();
    value["loaded_adapter"] = "only a newer file on disk".into();
    assert!(read("execute", &value).is_err());
    value = execute();
    value["request"]["action"] = "send".into();
    assert!(read("execute", &value).is_err());
    value = execute();
    value["caller"] = json!({"kind":"executor","executor":executor()});
    assert!(read("execute", &value).is_err());
    for key in ["slot", "turn"] {
        value = execute();
        value["request"][key] = Value::Null;
        assert!(read("execute", &value).is_err());
    }
}

#[test]
fn a_person_caller_has_exactly_its_kind_and_preserves_the_existing_wire() {
    let original = execute();
    let Command::Execute(parsed) = read("execute", &original).unwrap() else {
        panic!("expected execute");
    };
    assert_eq!(serde_json::to_value(parsed).unwrap(), original);
    for key in ["surprise", "executor", "observed_stops"] {
        let mut value = original.clone();
        value["caller"][key] = 1.into();
        assert!(read("execute", &value).is_err(), "accepted {key}");
    }
}

#[test]
fn stop_release_names_an_executor_and_explicit_observed_stops() {
    let mut value = json!({"schema":1,"request_id":ID,"action":"release_stop","target":{"scope":"executor","executor":executor()},"observed_pauses":[],"observed_stops":[ID]});
    assert!(read("plan", &value).is_ok());
    value["observed_stops"] = json!([]);
    assert!(read("plan", &value).is_err());
    value["observed_stops"] = json!([ID, ID]);
    assert!(read("plan", &value).is_err());
    value["observed_stops"] = json!([ID]);
    value["observed_pauses"] = json!([ID]);
    assert!(read("plan", &value).is_err());
}

#[test]
fn a_pinned_begin_is_one_acknowledgement_and_does_not_rewrite_user_text() {
    let original = "Task text quotes hgs task start orchard/T-234567 as an example.";
    let mut prompt = original.to_owned();
    pin_begin(&mut prompt, "orchard", "T-234567", ID);
    assert!(prompt.starts_with(original));
    assert_eq!(
        prompt
            .matches("`hgs task begin-attempt orchard/T-234567 --attempt ")
            .count(),
        1
    );
    assert!(prompt.contains("replaces any plain task start"));
    assert!(prompt.contains("do not run both"));
}

/// zerus-sa9qn.42: the instructions name the start command each dispatched
/// prompt pins instead of asking the agent to compose it, and managed work
/// still finds that sentence to replace.
#[test]
fn the_instructions_point_to_the_pinned_start_command() {
    let instructions = super::super::AGENT_INSTRUCTIONS;
    assert_eq!(instructions.matches(PLAIN_START).count(), 1);
    assert!(!instructions.contains("Acknowledge a dispatched task with `hgs task start <ref>`"));
    assert!(PLAIN_START.contains("the `hgs task start` command its prompt pins"));
}

#[test]
fn dormant_helpers_keep_instructions_and_existing_report_prompt_bytes() {
    assert!(help_marker().is_none());
    assert_eq!(
        session_instructions(super::super::AGENT_INSTRUCTIONS),
        super::super::AGENT_INSTRUCTIONS
    );
    let mut prompt = "Original task".to_owned();
    super::super::reports::pin(&mut prompt, "orchard", "T-234567", ID);
    assert!(!prompt.contains("acknowledgement origin"));
    assert!(!prompt.contains("begin-attempt"));
    assert!(prompt.contains("hgs task report-attempt orchard/T-234567 --attempt "));
    let mut wrapped = "Original task".to_owned();
    pin_dispatch(&mut wrapped, "orchard", "T-234567", ID);
    assert_eq!(wrapped, prompt); // Entire native prompt, not a text-match receipt.
    let args = vec![
        "begin-attempt".into(),
        "orchard/T-234567".into(),
        "--attempt".into(),
        ID.into(),
        "--json".into(),
    ];
    let config = Config {
        home: "/nonexistent/task-control-home".into(),
        dir: "/nonexistent/task-control-config".into(),
        state: "/nonexistent/task-control-state".into(),
        selves: vec![],
        peers: vec![],
        base_peers: vec![],
        peers_from_environment: false,
        machines: Default::default(),
        tab: false,
        tab_colors: String::new(),
    };
    let refusal = command(&config, &args).unwrap();
    assert_eq!(refusal["code"], "task_controls_inactive");
    assert_eq!(refusal["attempted"], false);
    assert_eq!(refusal["request_id"], ID);
}

#[test]
fn admission_binds_every_outer_guard_and_only_the_identical_plan_repeats() {
    let mut value = execute();
    value["request"]["action"] = "yield".into();
    let plan = serde_json::from_value::<wire::Plan>(value)
        .unwrap()
        .seal()
        .unwrap();
    let original = serde_json::to_value(&plan).unwrap();
    let bytes = serde_json::to_vec(&original).unwrap();
    let Command::Execute(repeated) = wire::read("execute", &bytes).unwrap() else {
        panic!("execute");
    };
    assert_eq!(
        serde_json::to_vec(&repeated).unwrap(),
        serde_json::to_vec(&plan).unwrap()
    );
    assert_eq!(repeated.request, plan.request);
    for key in [
        "caller",
        "configuration",
        "store_identity",
        "cli_image",
        "loaded_adapter",
    ] {
        let mut changed = original.clone();
        if key == "caller" {
            changed[key] = json!({"kind":"executor","executor":executor()});
        } else {
            changed[key] = "f".repeat(64).into();
        }
        assert!(
            read("execute", &changed).is_err(),
            "{key} retained the old digest"
        );
        // Even with a newly computed valid binding, its immutable Request now
        // differs under the original UUID. Ledger reserve must refuse it.
        let new = serde_json::from_value::<wire::Plan>(changed)
            .unwrap()
            .seal()
            .unwrap();
        assert_eq!(new.request.id, plan.request.id);
        assert_ne!(new.request.admission, plan.request.admission, "{key}");
        assert_ne!(new.request, plan.request, "{key}");
    }
    let mut omitted = original.clone();
    omitted["request"]
        .as_object_mut()
        .unwrap()
        .remove("admission");
    assert!(read("execute", &omitted).is_err());
    let mut missing_selector = original;
    missing_selector["request"]["executor"]
        .as_object_mut()
        .unwrap()
        .remove("session_id");
    assert!(read("execute", &missing_selector).is_err());
}
