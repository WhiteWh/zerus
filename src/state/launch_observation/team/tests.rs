use super::super::observe_team_session as session;
use super::*;
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}
struct Fixture {
    root: tempfile::TempDir,
    state: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, state }
    }
    fn put(&self, path: &str, value: Value) {
        let p = self.state.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::set_permissions(p.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&p, value.to_string()).unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn start(&self, allocated: bool) {
        let mut v = json!({"launch_id":id(1),"name":"kimi/example/original","started_at":1.0});
        if allocated {
            v["session_id"] = json!(id(2));
        }
        self.put(&format!("launches/{}.json", id(1)), v);
    }
    fn value(&self) -> Value {
        json!({"version":1,"layout":"id","session_id":id(2),"launch_id":id(1),"name":"kimi/example/original","agent":"kimi","account_id":"native-kimi","run_id":id(3),"conversation_id":"","cwd":"/workspace/example"})
    }
    fn read(&self) -> TeamObservation {
        observe(&self.state, &id(1), Instant::now() + Duration::from_secs(3))
    }
    fn legacy(&self, v: Value) {
        self.put(
            &format!("{:x}.json", Sha256::digest(b"kimi/example/original")),
            v,
        );
    }
}
#[test]
fn teams_reads_a_kimi_record_without_a_conversation_and_after_rename() {
    let f = Fixture::new();
    let _ = &f.root;
    f.start(true);
    let mut v = f.value();
    v["name"] = json!("kimi/example/renamed");
    f.put(&format!("sessions/{}.json", id(2)), v);
    let TeamObservation::Record(r) = f.read() else {
        panic!("renamed trust record was hidden")
    };
    assert_eq!(r.name, "kimi/example/renamed");
    assert!(r.conversation_id.is_empty());
    assert_eq!(r.launch_id, id(1));
}

#[test]
fn a_configured_state_alias_resolves_once_but_links_below_it_are_refused() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.start(true);
    let leaf = format!("sessions/{}.json", id(2));
    f.put(&leaf, f.value());
    let alias = f.root.path().join("state-alias");
    symlink(&f.state, &alias).unwrap();
    let read = || {
        session(
            &alias,
            &id(2),
            &id(1),
            "kimi",
            "",
            Instant::now() + Duration::from_secs(3),
        )
    };
    assert!(matches!(read(), TeamSession::Record(r) if r.run_id == id(3)));
    assert!(matches!(
        observe(&alias, &id(1), Instant::now() + Duration::from_secs(3)),
        TeamObservation::Record(r) if r.run_id == id(3)
    ));

    let directory = f.state.join("sessions");
    let saved = f.state.join("saved-sessions");
    fs::rename(&directory, &saved).unwrap();
    symlink(&saved, &directory).unwrap();
    assert_eq!(read(), TeamSession::Unavailable);
    assert_eq!(
        observe(&alias, &id(1), Instant::now() + Duration::from_secs(3)),
        TeamObservation::Unavailable
    );
    fs::remove_file(&directory).unwrap();
    fs::rename(&saved, &directory).unwrap();

    let path = f.state.join(leaf);
    let saved = path.with_extension("saved");
    fs::rename(&path, &saved).unwrap();
    symlink(&saved, &path).unwrap();
    assert_eq!(read(), TeamSession::Unavailable);
    assert_eq!(
        observe(&alias, &id(1), Instant::now() + Duration::from_secs(3)),
        TeamObservation::Unavailable
    );
}

#[test]
fn an_older_start_record_uses_the_legacy_label_route() {
    let f = Fixture::new();
    f.start(false);
    let mut v = f.value();
    v.as_object_mut().unwrap().remove("layout");
    f.legacy(v);
    assert!(matches!(f.read(), TeamObservation::Record(_)));
}
#[test]
fn a_record_from_another_launch_in_the_same_folder_is_positive_mismatch() {
    let f = Fixture::new();
    f.start(true);
    let mut v = f.value();
    v["launch_id"] = json!(id(9));
    f.put(&format!("sessions/{}.json", id(2)), v);
    assert_eq!(
        f.read(),
        TeamObservation::Mismatch {
            field: "launch_id".into()
        }
    );
}
#[test]
fn ended_missing_unreadable_and_expired_observations_remain_distinct() {
    let f = Fixture::new();
    f.start(false);
    assert_eq!(f.read(), TeamObservation::Absent { legacy: true });
    f.put(
        &format!("launches/{}.end.json", id(1)),
        json!({"launch_id":id(1),"name":"kimi/example/original","code":3,"ended_at":2.0}),
    );
    assert_eq!(f.read(), TeamObservation::Ended { code: 3 });
    assert_eq!(
        observe(&f.state, &id(1), Instant::now()),
        TeamObservation::Unavailable
    );
}
#[test]
fn a_legacy_rename_shadow_routes_but_does_not_authorize_a_binding() {
    let f = Fixture::new();
    f.start(false);
    let mut shadow = f.value();
    shadow["rename_shadow"] = json!(true);
    shadow["rename_target"] = json!("kimi/example/new");
    f.legacy(shadow);
    let mut v = f.value();
    v["name"] = json!("kimi/example/new");
    f.put(
        &format!("{:x}.json", Sha256::digest(b"kimi/example/new")),
        v,
    );
    assert!(matches!(f.read(),TeamObservation::Record(r) if r.name=="kimi/example/new"));
}

#[test]
fn native_providers_read_their_own_legacy_bindings_and_id_layout() {
    for agent in ["dsh", "opencode"] {
        for allocated in [false, true] {
            for by_id in [false, true] {
                let f = Fixture::new();
                let label = format!("{agent}/example/original");
                let mut start = json!({"launch_id":id(1),"name":label,"started_at":1.0});
                if allocated {
                    start["session_id"] = json!(id(2));
                }
                f.put(&format!("launches/{}.json", id(1)), start);
                let mut value = f.value();
                value["name"] = json!(label);
                value["agent"] = json!(agent);
                value["backend"] = json!(agent);
                value.as_object_mut().unwrap().remove("account_id");
                if by_id {
                    value["name"] = json!(format!("{agent}/example/renamed"));
                    f.put(&format!("{agent}/sessions/{}.json", id(2)), value);
                    let leaf = format!("{:x}.json", Sha256::digest(label.as_bytes()));
                    f.put(
                        &format!("labels/{leaf}"),
                        json!({"label":label,"ids":[id(2)]}),
                    );
                } else {
                    value.as_object_mut().unwrap().remove("layout");
                    let leaf = format!("{:x}.json", Sha256::digest(label.as_bytes()));
                    f.put(&format!("{agent}/bindings/{leaf}"), value);
                    f.put(&format!("ids/{}.json", id(2)), json!({"name":label}));
                }
                let TeamObservation::Record(record) = f.read() else {
                    panic!("{agent}: allocated={allocated}, by_id={by_id}")
                };
                assert_eq!(record.agent, agent);
                assert_eq!(record.account, format!("native-{agent}"));
                assert!(record.conversation_id.is_empty());
            }
        }
    }
}

// zerus-63rru.53: the member's durable handle, its session's own ID.
impl Fixture {
    fn handle(&self, hint: &str) -> TeamSession {
        session(
            &self.state,
            &id(2),
            &id(1),
            "kimi",
            hint,
            Instant::now() + Duration::from_secs(3),
        )
    }
    fn at_label(&self, label: &str, v: Value) {
        self.put(&format!("{:x}.json", Sha256::digest(label.as_bytes())), v);
    }
    fn legacy_value(&self, label: &str) -> Value {
        let mut v = self.value();
        v.as_object_mut().unwrap().remove("layout");
        v["name"] = json!(label);
        v
    }
}
#[test]
fn a_handle_needs_no_launch_evidence_and_follows_its_session_in_either_layout() {
    // No start or end file at all: the evidence is gone.
    let f = Fixture::new();
    let mut v = f.value();
    v["name"] = json!("kimi/example/renamed");
    f.put(&format!("sessions/{}.json", id(2)), v);
    let TeamSession::Record(r) = f.handle("") else {
        panic!("the ID layout was not read")
    };
    assert_eq!(
        (r.name.as_str(), r.session_id.as_str()),
        ("kimi/example/renamed", id(2).as_str())
    );

    // A legacy record through the ID index under its current label.
    let f = Fixture::new();
    f.at_label("kimi/example/now", f.legacy_value("kimi/example/now"));
    f.put(
        &format!("ids/{}.json", id(2)),
        json!({"name":"kimi/example/now"}),
    );
    assert!(matches!(f.handle(""), TeamSession::Record(r) if r.name == "kimi/example/now"));
}
#[test]
fn a_stale_or_missing_index_entry_falls_back_to_the_label_at_binding() {
    let f = Fixture::new();
    // The index names a label another session holds now: a miss, not a fault.
    let mut other = f.legacy_value("kimi/example/taken");
    other["session_id"] = json!(id(7));
    f.at_label("kimi/example/taken", other);
    f.put(
        &format!("ids/{}.json", id(2)),
        json!({"name":"kimi/example/taken"}),
    );
    assert_eq!(f.handle(""), TeamSession::Gone);
    // The label at binding, through two legacy renames, reaches the record.
    for (from, to) in [
        ("kimi/example/original", "kimi/example/second"),
        ("kimi/example/second", "kimi/example/third"),
    ] {
        let mut shadow = f.legacy_value(from);
        shadow["rename_shadow"] = json!(true);
        shadow["rename_target"] = json!(to);
        f.at_label(from, shadow);
    }
    f.at_label("kimi/example/third", f.legacy_value("kimi/example/third"));
    assert!(
        matches!(f.handle("kimi/example/original"), TeamSession::Record(r) if r.name == "kimi/example/third")
    );
}
#[test]
fn the_label_at_binding_is_followed_through_at_most_three_renames() {
    let names = [
        "kimi/example/r0",
        "kimi/example/r1",
        "kimi/example/r2",
        "kimi/example/r3",
        "kimi/example/r4",
    ];
    for renames in [3, 4] {
        let f = Fixture::new();
        for hop in 0..renames {
            let mut shadow = f.legacy_value(names[hop]);
            shadow["rename_shadow"] = json!(true);
            shadow["rename_target"] = json!(names[hop + 1]);
            f.at_label(names[hop], shadow);
        }
        f.at_label(names[renames], f.legacy_value(names[renames]));
        let found = matches!(f.handle(names[0]), TeamSession::Record(_));
        assert_eq!(found, renames == 3, "{renames} renames");
    }
}
#[test]
fn another_session_at_the_label_an_archive_or_nothing_is_gone() {
    let f = Fixture::new();
    assert_eq!(f.handle("kimi/example/original"), TeamSession::Gone);
    // The label now holds another session (its ID differs).
    let mut other = f.legacy_value("kimi/example/original");
    other["session_id"] = json!(id(7));
    f.at_label("kimi/example/original", other);
    assert_eq!(f.handle("kimi/example/original"), TeamSession::Gone);
    // A record that is an archive is not the live or saved session.
    let f = Fixture::new();
    let mut archived = f.value();
    archived["archive_id"] = json!(id(8));
    f.put(&format!("sessions/{}.json", id(2)), archived);
    assert_eq!(f.handle(""), TeamSession::Gone);
}
#[test]
fn the_handle_s_session_must_carry_the_bound_launch() {
    for launch in [json!(id(9)), Value::Null, json!("")] {
        let f = Fixture::new();
        let mut v = f.value();
        v["launch_id"] = launch.clone();
        f.put(&format!("sessions/{}.json", id(2)), v);
        assert_eq!(f.handle(""), TeamSession::LaunchChanged, "{launch}");
    }
}
#[test]
fn a_corrupt_record_or_an_expired_deadline_is_unavailable() {
    let f = Fixture::new();
    let mut v = f.value();
    v["session_id"] = json!(id(7));
    f.put(&format!("sessions/{}.json", id(2)), v);
    assert_eq!(f.handle(""), TeamSession::Unavailable);
    let f = Fixture::new();
    f.put(&format!("sessions/{}.json", id(2)), f.value());
    assert_eq!(
        session(&f.state, &id(2), &id(1), "kimi", "", Instant::now()),
        TeamSession::Unavailable
    );
    assert_eq!(
        session(
            &f.state,
            "not-an-id",
            &id(1),
            "kimi",
            "",
            Instant::now() + Duration::from_secs(3)
        ),
        TeamSession::Unavailable
    );
}
#[test]
fn a_session_started_fresh_is_named_apart_from_another_launch() {
    // Before its new conversation starts: no launch ID and `fresh_from`.
    let f = Fixture::new();
    let mut v = f.value();
    v["launch_id"] = Value::Null;
    v["fresh_from"] = json!({"conversation_id":"earlier","at":1.0});
    f.put(&format!("sessions/{}.json", id(2)), v.clone());
    assert_eq!(f.handle(""), TeamSession::StartedFresh);
    // After it: the fresh start's boundary.
    v.as_object_mut().unwrap().remove("fresh_from");
    v["session_clear"] = json!({"source":"fresh_start","conversation_id":"next","previous_conversation_id":"earlier"});
    f.put(&format!("sessions/{}.json", id(2)), v.clone());
    assert_eq!(f.handle(""), TeamSession::StartedFresh);
    // Another launch's ID is a changed launch even with those marks.
    v["launch_id"] = json!(id(9));
    f.put(&format!("sessions/{}.json", id(2)), v);
    assert_eq!(f.handle(""), TeamSession::LaunchChanged);
}
