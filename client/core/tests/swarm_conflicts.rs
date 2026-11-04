use serde_json::{json, Value};
use zerus_client_core::swarm::conflicts::{self, Catalog, CHANGED};

const LOCAL: &str = "11111111-1111-4111-8111-111111111111";
const PEER: &str = "22222222-2222-4222-8222-222222222222";
fn conflict(field: Value, values: Vec<Value>) -> Value {
    let key = field.to_string();
    json!({"key":key,"field":field,"selected":"v0","variants":values.into_iter().enumerate().map(|(i,value)|json!({"id":format!("v{i}"),"actor":if i%2==0 {LOCAL} else {PEER},"value":value,"clock":i+1,"parents":[]})).collect::<Vec<_>>()})
}
fn snapshot(rows: Vec<Value>) -> Value {
    json!({"schema":1,"initialized":true,"node_id":LOCAL,"swarm_id":"fixture-swarm","digest":"first",
    "organization":{"projects":[
        {"id":"ungrouped","name":"Inbox","color":null,"folders":[]},
        {"id":"project-a","name":"Development","color":"#123456","folders":[{"id":"folder","path":"/synthetic/checkout"}]},
        {"id":"project-b","name":"Development","color":"#abcdef","folders":[]}
    ]},"machines":[{"id":LOCAL,"name":"Desktop","connection":"local-fixture","local":true},{"id":PEER,"name":"Build computer","connection":"fixture-peer","local":false}],"conflicts":rows})
}
fn session(values: Vec<Value>) -> Value {
    conflict(
        json!({"kind":"session","machine":LOCAL,"session":"codex/fixture/session"}),
        values,
    )
}
fn review_fixture() -> Value {
    snapshot(vec![session(vec![
        json!("project-a"),
        Value::Null,
        json!("project-a"),
    ])])
}
fn id(catalog: &Catalog<'_>) -> String {
    catalog.page(0).unwrap().rows[0].id.clone()
}

#[test]
fn exact_values_are_grouped_and_shown_now_never_selects_them() {
    let source = review_fixture();
    let original = source.clone();
    let catalog = Catalog::checked(&source).unwrap();
    let page = catalog.page(0).unwrap();
    assert_eq!(page.rows[0].setting, "Session project");
    assert_eq!(page.rows[0].subject, "codex/fixture/session on Desktop");
    let review = catalog.review(&id(&catalog)).unwrap();
    let page = review.choices(&catalog, 0).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(
        page.rows[0].project.as_ref().unwrap().name,
        "Development (project-)"
    );
    assert_eq!(page.rows[0].origins.len(), 1);
    assert_eq!(page.rows[0].origins[0].text, "Desktop (this computer)");
    assert!(page.rows[0].origins[0].local && page.rows[0].shown);
    assert!(!page.rows[1].shown);
    assert_eq!(page.rows[1].text, "Use the local default project");
    assert!(review
        .explanation(false)
        .contains("not a resolved conflict"));
    assert!(review.explanation(true).starts_with(CHANGED));
    assert_eq!(source, original);
    assert_eq!(format!("{review:?}"), "Review { .. }");
}

#[test]
fn resolution_uses_all_original_parents_and_the_original_json_value() {
    let source = review_fixture();
    let catalog = Catalog::checked(&source).unwrap();
    let review = catalog.review(&id(&catalog)).unwrap();
    let call = review.resolve(&catalog, 1, false).unwrap();
    assert_eq!(call.args, ["swarm", "resolve"]);
    assert!(call.request_id.is_none());
    assert_eq!(
        serde_json::from_slice::<Value>(&call.stdin.unwrap()).unwrap(),
        json!({"key":source["conflicts"][0]["key"],"versions":["v0","v1","v2"],"value":null})
    );
    assert!(review.resolve(&catalog, usize::MAX, true).is_err());
}

#[test]
fn changed_catalog_identity_variants_field_or_removed_conflict_refuses_old_value() {
    let source = review_fixture();
    let catalog = Catalog::checked(&source).unwrap();
    let review = catalog.review(&id(&catalog)).unwrap();
    let mut changed = source.clone();
    changed["digest"] = json!("unrelated-head");
    changed["organization"]["projects"][1]["name"] = json!("Renamed elsewhere");
    let current = Catalog::checked(&changed).unwrap();
    assert!(review.matches(&current));
    assert!(review.resolve(&current, 0, false).is_ok());
    for key in ["node_id", "swarm_id"] {
        let mut changed = source.clone();
        changed[key] = json!("different");
        let current = Catalog::checked(&changed).unwrap();
        assert!(!review.matches(&current));
        assert_eq!(review.resolve(&current, 0, false).unwrap_err(), CHANGED);
    }
    for change in 0..3 {
        let mut changed = source.clone();
        match change {
            0 => changed["conflicts"][0]["variants"][0]["value"] = json!("ungrouped"),
            1 => changed["conflicts"][0]["variants"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id":"new-concurrent","actor":PEER,"value":"project-b"})),
            _ => changed["conflicts"] = json!([]),
        }
        let current = Catalog::checked(&changed).unwrap();
        assert_eq!(review.choices(&current, 0).unwrap_err(), CHANGED);
        assert!(review.resolve(&current, 0, true).is_err());
    }
}

#[test]
fn confirmations_cover_exact_removal_and_identity_merge_values() {
    for (field, values, confirmed_index, action) in [
        (
            json!({"kind":"project","id":"project-a","field":"alive"}),
            vec![json!(true), json!(false)],
            1,
            "Apply project choice",
        ),
        (
            json!({"kind":"folder","id":"folder"}),
            vec![
                json!({"name":"Checkout","project":"project-a","machine":LOCAL,"path":"/synthetic/new"}),
                Value::Null,
            ],
            1,
            "Keep this folder link",
        ),
        (
            json!({"kind":"machine","id":LOCAL,"field":"redirect"}),
            vec![json!(PEER), json!("33333333-3333-4333-8333-333333333333")],
            0,
            "Keep this value",
        ),
    ] {
        let source = snapshot(vec![conflict(field, values)]);
        let catalog = Catalog::checked(&source).unwrap();
        let review = catalog.review(&id(&catalog)).unwrap();
        let selection = review.selection(&catalog, confirmed_index).unwrap();
        assert!(selection.confirmation);
        assert!(review.choices(&catalog, 0).unwrap().rows[confirmed_index].removal);
        assert_eq!(selection.action, action);
        assert!(selection
            .outcome
            .contains("This choice will synchronize to connected computers."));
        assert!(review.resolve(&catalog, confirmed_index, false).is_err());
        assert!(review.resolve(&catalog, confirmed_index, true).is_ok());
    }
}

#[test]
fn folder_json_and_no_file_removal_consequence_remain_exact() {
    let folder = json!({"project":"project-a","machine":PEER,"name":"Literal \"name\"","path":"/synthetic/é ; $literal"});
    let source = snapshot(vec![conflict(
        json!({"kind":"folder","id":"folder"}),
        vec![folder.clone(), Value::Null],
    )]);
    let catalog = Catalog::checked(&source).unwrap();
    assert_eq!(
        catalog.page(0).unwrap().rows[0].subject,
        "/synthetic/checkout"
    );
    let review = catalog.review(&id(&catalog)).unwrap();
    let choice = review.choices(&catalog, 0).unwrap();
    assert_eq!(choice.rows[0].text,"Name: Literal \"name\"\nProject: Development (project-)\nComputer: Build computer\n/synthetic/é ; $literal");
    assert!(review
        .selection(&catalog, 1)
        .unwrap()
        .outcome
        .contains("Files on disk stay intact."));
    let call = review.resolve(&catalog, 0, false).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&call.stdin.unwrap()).unwrap()["value"],
        folder
    );
}

#[test]
fn unavailable_projects_archives_and_computer_origins_follow_qt_text() {
    let mut row = session(vec![json!("missing-project"), Value::Null]);
    row["field"]["session"] = json!("archive\n123456789");
    row["key"] = json!(row["field"].to_string());
    row["variants"][1]["actor"] = json!("unknown-node");
    let source = snapshot(vec![row]);
    let catalog = Catalog::checked(&source).unwrap();
    assert!(catalog.page(0).unwrap().rows[0]
        .subject
        .starts_with("Archived session (12345678)"));
    let review = catalog.review(&id(&catalog)).unwrap();
    let choices = review.choices(&catalog, 0).unwrap();
    assert_eq!(choices.rows[0].text, "Unavailable project (missing-)");
    assert_eq!(
        choices.rows[1].origins[0].text,
        "Unknown computer (unknown-)"
    );
    assert!(review
        .selection(&catalog, 0)
        .unwrap()
        .outcome
        .contains("Until it is restored"));
    assert!(!review.selection(&catalog, 0).unwrap().confirmation);
}

#[test]
fn pages_keep_review_and_value_ordinals_stable_without_selecting_anything() {
    let mut rows = Vec::new();
    for i in 0..130 {
        rows.push(conflict(
            json!({"kind":"project","id":format!("p{i}"),"field":"name"}),
            (0..75).map(|n| json!(format!("Value {n}"))).collect(),
        ));
    }
    let source = snapshot(rows);
    let catalog = Catalog::checked(&source).unwrap();
    let review = catalog.review(&id(&catalog)).unwrap();
    assert_eq!(
        (
            catalog.page(0).unwrap().offset,
            catalog.page(0).unwrap().rows.len()
        ),
        (0, 64)
    );
    assert_eq!(
        (
            catalog.page(1).unwrap().offset,
            catalog.page(1).unwrap().rows.len()
        ),
        (64, 64)
    );
    assert_eq!(
        (
            catalog.page(usize::MAX).unwrap().offset,
            catalog.page(2).unwrap().rows.len()
        ),
        (128, 2)
    );
    let page = review.choices(&catalog, 1).unwrap();
    assert_eq!((page.offset, page.total, page.rows.len()), (32, 75, 32));
    assert_eq!(page.rows[0].index, 32);
    assert_eq!(review.choices(&catalog, usize::MAX).unwrap().offset, 64);
    assert!(review.matches(&catalog));
    assert_eq!(review.choices(&catalog, 0).unwrap().rows[0].index, 0);
    let empty = snapshot(vec![]);
    assert_eq!(
        Catalog::checked(&empty)
            .unwrap()
            .page(usize::MAX)
            .unwrap()
            .total,
        0
    );
}

#[test]
fn malformed_or_ambiguous_evidence_refuses_the_whole_catalog() {
    let source = review_fixture();
    for case in 0..10 {
        let mut invalid = source.clone();
        match case {
            0 => invalid["conflicts"][0]["variants"][1]["id"] = json!("v0"),
            1 => invalid["conflicts"][0]["key"] = json!("{}"),
            2 => invalid["conflicts"][0]["selected"] = json!("unknown"),
            3 => invalid["conflicts"][0]["variants"][1]["value"] = json!(true),
            4 => invalid["conflicts"][0]["variants"][0]
                .as_object_mut()
                .unwrap()
                .remove("value")
                .map(|_| ())
                .unwrap(),
            5 => invalid["conflicts"] = json!([source["conflicts"][0], source["conflicts"][0]]),
            6 => invalid["schema"] = json!(2),
            7 => invalid["machines"][1]["id"] = json!(LOCAL),
            8 => invalid["organization"]["projects"][1]["id"] = json!("ungrouped"),
            _ => {
                invalid["conflicts"][0]["variants"] =
                    json!([{"id":"single","actor":LOCAL,"value":null}])
            }
        }
        assert!(Catalog::checked(&invalid).is_err(), "case {case}");
    }
    let unknown = snapshot(vec![conflict(
        json!({"kind":"preference","field":"new"}),
        vec![json!("a"), json!("b")],
    )]);
    assert!(Catalog::checked(&unknown).is_err());
}

#[test]
fn head_and_encoded_snapshot_pressure_refuse_without_a_prefix() {
    let many = snapshot(vec![session(
        (0..=conflicts::VARIANTS)
            .map(|i| json!(format!("p{i}")))
            .collect(),
    )]);
    assert!(Catalog::checked(&many).err().unwrap().contains("too large"));
    let mut huge = review_fixture();
    huge["opaque"] = json!("x".repeat(conflicts::SNAPSHOT_BYTES));
    assert!(Catalog::checked(&huge).err().unwrap().contains("too large"));
    let mut uninitialized = review_fixture();
    uninitialized["initialized"] = json!(false);
    let catalog = Catalog::checked(&uninitialized).unwrap();
    let review = catalog.review(&id(&catalog)).unwrap();
    assert!(review.resolve(&catalog, 0, false).is_err());
}

#[test]
fn existing_session_projection_supplies_titles_without_reading_history() {
    let source = review_fixture();
    let lookup = |machine: &str, session: &str| {
        assert_eq!((machine, session), (LOCAL, "codex/fixture/session"));
        Some("A saved title".into())
    };
    let catalog = Catalog::checked(&source)
        .unwrap()
        .with_session_titles("fixture-local", &lookup)
        .unwrap();
    let row = catalog.page(0).unwrap().rows.remove(0);
    assert_eq!(row.subject, "A saved title on Desktop");
    assert_eq!(row.computer.as_deref(), Some("fixture-local"));
    let oversized = |_: &str, _: &str| Some("x".repeat(2049));
    assert!(Catalog::checked(&source)
        .unwrap()
        .with_session_titles("local", &oversized)
        .unwrap()
        .page(0)
        .is_err());
}

#[test]
fn fifty_thousand_summaries_are_paged_and_the_next_row_refuses_whole() {
    let rows = (0..conflicts::CONFLICTS).map(|i| {
        json!({"key":format!("{{\"kind\":\"project\",\"id\":\"p{i}\",\"field\":\"name\"}}"),
            "field":{"kind":"project","id":format!("p{i}"),"field":"name"},"selected":null,
            "variants":[{"id":"a","actor":"local","value":"one"},{"id":"b","actor":"peer","value":"two"}]})
    }).collect();
    let mut source = snapshot(rows);
    let started = std::time::Instant::now();
    let catalog = Catalog::checked(&source).unwrap();
    let page = catalog.page(usize::MAX).unwrap();
    assert_eq!(
        (page.offset, page.total, page.rows.len()),
        (49_984, 50_000, 16)
    );
    println!("50000 summaries: checked+last page {:?}; encoded snapshot {} bytes; serialized last page {} bytes",started.elapsed(),serde_json::to_vec(&source).unwrap().len(),serde_json::to_vec(&page).unwrap().len());
    drop(catalog);
    source["conflicts"].as_array_mut().unwrap().push(conflict(
        json!({"kind":"project","id":"extra","field":"name"}),
        vec![json!("a"), json!("b")],
    ));
    assert!(Catalog::checked(&source)
        .err()
        .unwrap()
        .contains("too large"));
}

#[test]
fn encoded_snapshot_byte_bound_includes_escaping_and_refuses_one_extra_byte() {
    let mut source = review_fixture();
    source["opaque"] = json!("");
    let overhead = serde_json::to_vec(&source).unwrap().len();
    source["opaque"] = json!("x".repeat(conflicts::SNAPSHOT_BYTES - overhead));
    assert_eq!(
        serde_json::to_vec(&source).unwrap().len(),
        conflicts::SNAPSHOT_BYTES
    );
    assert!(Catalog::checked(&source).is_ok());
    source["opaque"] = json!(format!("{}x", source["opaque"].as_str().unwrap()));
    assert!(Catalog::checked(&source)
        .err()
        .unwrap()
        .contains("too large"));
    source["opaque"] = json!("\n".repeat(conflicts::SNAPSHOT_BYTES / 2));
    assert!(Catalog::checked(&source).is_err());
}
