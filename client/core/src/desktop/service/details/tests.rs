use super::*;
use serde_json::json;
use zerus_watch_protocol::SessionRef;

const ID: &str = "00000000-0000-4000-8000-000000000001";
fn route(topic: &str) -> Route {
    let subscription = Subscription {
        topic: topic.into(),
        session: matches!(topic, "activity" | "processes").then(|| SessionRef {
            name: "codex/project/session".into(),
            run_id: Some("run".into()),
            conversation_id: Some("conversation".into()),
        }),
        project: (topic == "tasks").then(|| "project".into()),
        ..Default::default()
    };
    Route::new("configured-alias", ID.into(), subscription).unwrap()
}
fn value(seq: u64, frame: Value) -> Value {
    json!({"id":ID,"computer":"configured-alias","generation":1,"revision":1,"detail_seq":seq,"frame":frame})
}
fn status(state: &str) -> Value {
    let mut value = value(0, Value::Null);
    value["state"] = json!(state);
    value["code"] = json!(state);
    value["complete"] = json!(false);
    value
}
fn native(seq: u64, body: Value) -> Value {
    let mut frame = body;
    frame["v"] = json!(1);
    frame["seq"] = json!(seq);
    frame
}
fn ready(route: &mut Route) {
    route.apply("detail_status", &status("ready")).unwrap();
}

#[test]
fn filtered_native_sequences_and_bodies_keep_their_original_identity() {
    let mut route = route("activity");
    ready(&mut route);
    let body = json!({"name":"codex/project/session","run_id":"run","conversation_id":"conversation",
        "message_events":[{"seq":3,"type":"UserPrompt","detail":"older message"}]});
    let out = route
        .apply(
            "detail",
            &value(
                1,
                native(
                    90,
                    json!({"t":"state","topic":"activity","key":"run:run","rev":9,"value":body}),
                ),
            ),
        )
        .unwrap();
    assert!(
        matches!(&out[0],Change::Activity { computer,key,update:ActivityUpdate::State { value,rev:9 } }
        if computer.0 == "configured-alias" && key == "run:run" && value == &body)
    );
    let event = json!({"seq":101,"type":"ToolCall","detail":"kept"});
    let out = route.apply("detail",&value(2,native(240,json!({"t":"events","topic":"activity","key":"run:run","events":[event],"cursor":101})))).unwrap();
    assert!(
        matches!(&out[0],Change::Activity { update:ActivityUpdate::Events { events,cursor:101 },.. } if events == &vec![event])
    );
}

#[test]
fn native_page_last_establishes_replacement_not_transport_ready() {
    let mut route = route("tasks");
    assert!(route
        .apply("detail_status", &status("ready"))
        .unwrap()
        .is_empty());
    let first = native(
        7,
        json!({"t":"snapshot","topic":"tasks","key":"project","page":0,"last":false,"cursor":0,
        "items":[{"id":"task-1"}]}),
    );
    assert!(route.apply("detail", &value(1, first)).unwrap().is_empty());
    let last = native(
        50,
        json!({"t":"snapshot","topic":"tasks","key":"project","page":1,"last":true,"cursor":0,
        "items":[{"id":"task-2"}],"value":{"project":{"id":"project"},"digest":"digest","damaged":[]}}),
    );
    let out = route.apply("detail", &value(2, last)).unwrap();
    assert!(
        matches!(&out[0],Change::Tasks { key,update:crate::TasksUpdate::List { tasks,digest,.. },.. }
        if key == "project" && tasks.len() == 2 && digest == "digest")
    );
}

#[test]
fn revised_route_drops_staging_and_older_generation_revision_or_consumer() {
    let mut route = route("activity");
    ready(&mut route);
    let first = native(
        1,
        json!({"t":"snapshot","topic":"activity","key":"run:run","page":0,"last":false,"cursor":2,"items":[{"seq":1}]}),
    );
    route.apply("detail", &value(1, first)).unwrap();
    let mut next = status("starting");
    next["revision"] = json!(2);
    route.apply("detail_status", &next).unwrap();
    let old = value(
        2,
        native(
            9,
            json!({"t":"snapshot","topic":"activity","key":"run:run","page":1,"last":true,"cursor":2,"items":[{"seq":2}]}),
        ),
    );
    assert!(route.apply("detail", &old).unwrap().is_empty());
    next["state"] = json!("ready");
    route.apply("detail_status", &next).unwrap();
    let mut page = value(
        1,
        native(
            10,
            json!({"t":"snapshot","topic":"activity","key":"run:run","page":0,"last":true,"cursor":4,"items":[{"seq":4}]}),
        ),
    );
    page["revision"] = json!(2);
    let out = route.apply("detail", &page).unwrap();
    assert!(
        matches!(&out[0],Change::Activity {update:ActivityUpdate::Replace { events,.. },.. } if events == &vec![json!({"seq":4})])
    );
    page["id"] = json!("00000000-0000-4000-8000-000000000002");
    assert!(route.apply("detail", &page).unwrap().is_empty());
}

#[test]
fn detail_gap_wrong_target_and_unannounced_revision_refuse_without_completion() {
    for (field, changed, error) in [
        ("detail_seq", json!(2), "detail_sequence_gap"),
        ("revision", json!(2), "detail_not_ready"),
        (
            "frame",
            native(
                4,
                json!({"t":"state","topic":"processes","key":"other","value":{},"rev":1}),
            ),
            "detail_target_changed",
        ),
    ] {
        let mut route = route("processes");
        ready(&mut route);
        let mut input = value(
            1,
            native(
                4,
                json!({"t":"state","topic":"processes","key":"run:run","value":{},"rev":1}),
            ),
        );
        input[field] = changed;
        assert_eq!(route.apply("detail", &input), Err(error));
    }
}

#[test]
fn unsupported_and_local_ended_status_never_invent_native_end() {
    for state in ["unsupported", "incomplete", "ended"] {
        let mut route = route("activity");
        let out = route.apply("detail_status", &status(state)).unwrap();
        assert!(
            matches!(&out[0],Change::Activity {update:ActivityUpdate::Connection {ready:false,code,..},..} if code == state)
        );
    }
    let mut route = route("activity");
    ready(&mut route);
    let out = route.apply("detail",&value(1,native(8,json!({"t":"error","topic":"activity","key":"run:run","code":"subscription_ended","message":"ended","fatal":false})))).unwrap();
    assert!(matches!(
        &out[0],
        Change::Activity {
            update: ActivityUpdate::Ended,
            ..
        }
    ));
}

#[test]
fn organization_keeps_one_original_consistent_snapshot_for_edits() {
    let mut route = route("organization");
    ready(&mut route);
    assert!(route.organization_snapshot().is_none());
    let full = json!({"node_id":"native-node","swarm_id":"swarm","digest":"digest","organization":{"projects":[]}});
    let out = route
        .apply(
            "detail",
            &value(
                1,
                native(
                    8,
                    json!({"t":"snapshot","topic":"organization","page":0,"last":true,"cursor":7,
        "items":[{"key":"organization","row":full}]}),
                ),
            ),
        )
        .unwrap();
    assert!(
        matches!(&out[0],Change::Organization {update:crate::OrganizationUpdate::State {digest,..},..} if digest == "digest")
    );
    let snapshot = route.organization_snapshot().unwrap();
    assert_eq!(snapshot.node_id, "native-node");
    assert_eq!(snapshot.base, full["organization"]);
    let mut next = status("incomplete");
    next["detail_seq"] = json!(1);
    route.apply("detail_status", &next).unwrap();
    assert!(route.organization_snapshot().is_none());
}

#[test]
fn closed_selectors_reject_unbounded_or_ambiguous_demands_and_bad_aliases() {
    let base = route("activity").subscription;
    for spec in [
        Subscription {
            path: Some("/unexpected".into()),
            ..base.clone()
        },
        Subscription {
            tail: Some(101),
            ..base.clone()
        },
        Subscription {
            after: Some(-1),
            ..base.clone()
        },
        Subscription {
            after: Some(0),
            tail: Some(5),
            ..base.clone()
        },
    ] {
        assert!(Key::new("", &spec).is_err());
    }
    for alias in ["@peer", "-flag", "two words"] {
        assert!(Key::new(alias, &base).is_err());
    }
}

#[test]
fn native_gap_discards_only_staging_and_the_next_snapshot_replaces_the_tail() {
    let mut route = route("activity");
    ready(&mut route);
    let first = native(
        8,
        json!({"t":"snapshot","topic":"activity","key":"run:run","page":0,"last":false,"cursor":4,"items":[{"seq":3}]}),
    );
    assert!(route.apply("detail", &value(1, first)).unwrap().is_empty());
    let gap = native(
        20,
        json!({"t":"gap","topic":"activity","key":"run:run","reason":"overflow"}),
    );
    assert!(route.apply("detail", &value(2, gap)).unwrap().is_empty());
    let last = native(
        50,
        json!({"t":"snapshot","topic":"activity","key":"run:run","page":0,"last":true,"cursor":9,"items":[{"seq":9}]}),
    );
    let out = route.apply("detail", &value(3, last)).unwrap();
    assert!(
        matches!(&out[0],Change::Activity {update:ActivityUpdate::Replace {events,cursor:9},..} if events==&vec![json!({"seq":9})])
    );
}
