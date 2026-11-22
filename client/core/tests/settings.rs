//! The settings store on a real disk. Task #55 (team ede724c4, 2026-11-10):
//! the store is no longer seeded from Qt, so the Qt store readers and the
//! seed and family-import tests went with them.
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zerus_client_core::settings::{effective, find, Kind, Settings, KEYS};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!(
            "zerus-client-core-settings-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Scratch(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const FLUSH: Duration = Duration::from_secs(5);

#[test]
fn every_key_has_qt_s_default_and_range() {
    let default = |name: &str| effective(find(name).unwrap().kind, None);
    assert_eq!(default("workspace/theme"), "system");
    assert_eq!(default("workspace/language"), "en");
    assert_eq!(default("workspace/usageIndicator"), "rails");
    assert_eq!(default("workspace/contentScale"), 1.0);
    assert_eq!(default("workspace/activityWidth"), 900);
    assert_eq!(default("workspace/compact"), false);
    assert_eq!(default("workspace/expandSessionsOnHover"), true);
    assert_eq!(default("workspace/hideEmptyProjects"), true);
    assert_eq!(default("workspace/animateActivity"), true);
    assert_eq!(default("notifications/optionalQuestions"), false);
    assert_eq!(default("processes/enabled"), false);
    assert_eq!(default("processes/activeSeconds"), 2.5);
    assert_eq!(default("processes/backgroundSeconds"), 30.0);
    assert_eq!(default("processes/unknownSeconds"), 10.0);
    assert_eq!(default("processes/loadingSeconds"), 0.5);
    assert_eq!(default("updates/channel"), "stable");
    assert_eq!(default("updates/automatic"), true);
    assert_eq!(default("shortcuts/queue.sendNow"), json!([]));
    // Qt's historical opt-in never arms the client's worker or imports errors.
    let automatic = find("automaticApprovals/enabled").unwrap();
    assert!(matches!(automatic.kind, Kind::Flag(false)));
    assert_eq!(default(automatic.name), false);
    let error = find("automaticApprovals/error").unwrap();
    assert!(matches!(error.kind, Kind::Text("")));
    assert_eq!(default(error.name), "");
    assert!(find("shortcuts/").is_none() && find("shortcuts/a/b").is_none());
    assert_eq!(default("client/darkSkin"), "zerus-dark");
    assert_eq!(default("client/lightSkin"), "zerus-light");
    assert!(find("client/skin").is_none());
    assert_eq!(default("client/overviewWidth"), 1200);
    assert_eq!(default("client/overviewFullWidth"), false);
    let clamp = |name: &str, value: Value| effective(find(name).unwrap().kind, Some(&value));
    assert_eq!(clamp("workspace/contentScale", json!(5)), 2.0);
    assert_eq!(clamp("workspace/activityWidth", json!(100)), 480);
    assert_eq!(clamp("processes/loadingSeconds", json!(-1)), 0.0);
    assert_eq!(clamp("workspace/theme", json!("sepia")), "system");
    assert_eq!(clamp("workspace/language", json!("de")), "en");
    // Names are unique.
    let mut names: Vec<_> = KEYS.iter().map(|key| key.name).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), KEYS.len());
}

#[test]
fn machine_appearance_editing_is_local_and_color_reset_preserves_style() {
    let scratch = Scratch::new("machine-appearance-edit");
    let path = scratch.0.join("settings.json");
    let settings = Settings::open(path.clone());
    assert_eq!(
        settings.set("machines/colors/peer", json!("#abcdef")),
        Ok(json!("#abcdef"))
    );
    assert_eq!(
        settings.set("machines/vivid/peer", json!(true)),
        Ok(json!(true))
    );
    assert_eq!(
        settings.set("machines/colors/peer", Value::Null),
        Ok(Value::Null)
    );
    assert!(!settings.all().contains_key("machines/colors/peer"));
    assert!(settings.flag("machines/vivid/peer"));
    assert!(settings.flush_within(FLUSH));
    drop(settings);
    let reopened = Settings::open(path);
    assert!(!reopened.all().contains_key("machines/colors/peer"));
    assert!(reopened.flag("machines/vivid/peer"));
}

#[test]
fn account_marks_are_local_per_profile_and_removable() {
    let scratch = Scratch::new("account-marks");
    let path = scratch.0.join("settings.json");
    let settings = Settings::open(path.clone());
    assert_eq!(settings.get("sessions/accountMarks"), "letter");
    assert_eq!(
        settings.set("sessions/accountMarks", json!("emoji")),
        Ok(json!("emoji"))
    );
    let color = "accounts/color/studio%1Fclaude-a1";
    let mark = "accounts/mark/studio%1Fclaude-a1";
    assert_eq!(settings.set(color, json!("#2b97ff")), Ok(json!("#2b97ff")));
    assert_eq!(settings.set(mark, json!("x")), Ok(json!("x")));
    for invalid in [
        "accounts/color/",
        "accounts/color/a/b",
        "accounts/mark/a\\b",
    ] {
        assert!(
            settings.set(invalid, json!("#2b97ff")).is_err(),
            "{invalid}"
        );
    }
    assert!(settings.all().contains_key(color));
    assert_eq!(settings.set(mark, Value::Null), Ok(Value::Null));
    assert!(!settings.all().contains_key(mark));
    assert!(settings.flush_within(FLUSH));
    drop(settings);
    let reopened = Settings::open(path);
    assert_eq!(reopened.get(color), "#2b97ff");
    assert!(!reopened.all().contains_key(mark));
    assert_eq!(reopened.get("sessions/accountMarks"), "emoji");
}

#[test]
fn account_marks_can_be_hidden_per_account_and_everywhere() {
    // zerus-sa9qn.13: presentation only; labels and colors stay.
    let scratch = Scratch::new("account-marks-hidden");
    let settings = Settings::open(scratch.0.join("settings.json"));
    assert_eq!(settings.get("sessions/showAccountMarks"), true);
    assert_eq!(
        settings.set("sessions/showAccountMarks", json!(false)),
        Ok(json!(false))
    );
    let hidden = "accounts/hide/studio%1Fclaude-a1";
    assert_eq!(settings.set(hidden, json!(true)), Ok(json!(true)));
    assert!(settings.set(hidden, json!("yes")).is_err());
    assert!(settings.set("accounts/hide/a/b", json!(true)).is_err());
    assert_eq!(settings.set(hidden, Value::Null), Ok(Value::Null));
    assert!(!settings.all().contains_key(hidden));
}

#[test]
fn new_child_rows_start_compact_unless_turned_off() {
    // zerus-sa9qn.14: a client flag, on by default, and the device's record.
    let scratch = Scratch::new("compact-new-children");
    let settings = Settings::open(scratch.0.join("settings.json"));
    assert_eq!(settings.get("sessions/compactNewChildren"), true);
    assert_eq!(
        settings.set("sessions/compactNewChildren", json!(false)),
        Ok(json!(false))
    );
    assert_eq!(settings.get("sessions/childRowsSeen"), "");
    let seen = r#"{"11111111-1111-4111-8111-111111111111":[1,["a"]]}"#;
    assert_eq!(
        settings.set("sessions/childRowsSeen", json!(seen)),
        Ok(json!(seen))
    );
}

#[test]
fn set_normalizes_persists_and_tells_listeners() {
    let scratch = Scratch::new("set");
    let path = scratch.0.join("data/settings.json");
    let settings = Settings::open(path.clone());
    let heard = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&heard);
    let revisions = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&revisions);
    settings.subscribe(Box::new(move |name, value, revision| {
        sink.lock().unwrap().push((name.to_owned(), value.clone()));
        seen.lock().unwrap().push(revision);
    }));
    assert_eq!(
        settings.set("workspace/theme", json!("dark")),
        Ok(json!("dark"))
    );
    assert_eq!(
        settings.set("processes/activeSeconds", json!(0.2)),
        Ok(json!(1.0))
    );
    assert_eq!(
        settings.set("workspace/activityWidth", json!(1300)),
        Ok(json!(1300))
    );
    assert!(settings
        .set("workspace/activityWidth", json!(13.5))
        .is_err());
    assert!(settings.set("workspace/compact", json!("yes")).is_err());
    assert!(settings.set("no/such", json!(true)).is_err());
    assert_eq!(
        settings.set("shortcuts/queue.sendNow", json!(["Ctrl+K"])),
        Ok(json!(["Ctrl+K"]))
    );
    assert_eq!(
        settings.set("shortcuts/message.push", json!([])),
        Ok(json!([]))
    );
    // Null removes an override: the defaults apply again, told as null.
    assert_eq!(
        settings.set("shortcuts/message.push", Value::Null),
        Ok(Value::Null)
    );
    assert!(!settings.all().contains_key("shortcuts/message.push"));
    assert_eq!(heard.lock().unwrap().len(), 6);
    // The load reads the revision under the same lock.
    assert_eq!(settings.all_tracked().1, 6);
    // Each change takes the next revision; the answer carries it too.
    assert_eq!(*revisions.lock().unwrap(), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(
        settings.set_tracked("workspace/language", json!("ru")),
        Ok((json!("ru"), 7))
    );
    assert_eq!(
        heard.lock().unwrap()[1],
        ("processes/activeSeconds".to_owned(), json!(1.0))
    );
    assert!(settings.flush_within(FLUSH));
    drop(settings);
    let stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored["version"], 1);
    assert_eq!(stored["values"]["workspace/theme"], "dark");
    assert!(stored["values"].get("shortcuts/message.push").is_none());
    let names: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    assert_eq!(names, vec!["settings.json"], "no temporary files left");
    let again = Settings::open(path);
    assert_eq!(again.get("workspace/theme"), "dark");
    assert_eq!(again.get("processes/activeSeconds"), 1.0);
    assert_eq!(again.get("shortcuts/queue.sendNow"), json!(["Ctrl+K"]));
}

#[test]
fn an_unreadable_store_keeps_its_file_and_runs_at_defaults() {
    let scratch = Scratch::new("damaged");
    let path = scratch.0.join("settings.json");
    fs::write(&path, b"{not json").unwrap();
    let settings = Settings::open(path.clone());
    assert_eq!(settings.get("workspace/theme"), "system");
    assert!(settings.error().is_some());
    assert!(settings.set("workspace/theme", json!("dark")).is_err());
    assert!(settings.flush_within(FLUSH));
    drop(settings);
    assert_eq!(fs::read(&path).unwrap(), b"{not json");
}

/// A write the disk refuses is not flushed: flush_within promises the data
/// is on disk (speech providers send nothing on a false answer).
#[cfg(unix)]
#[test]
fn flush_within_is_false_after_a_refused_write() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("refused");
    let dir = scratch.0.join("data");
    fs::create_dir_all(&dir).unwrap();
    let settings = Settings::open(dir.join("settings.json"));
    assert!(settings.set("workspace/theme", json!("dark")).is_ok());
    assert!(settings.flush_within(FLUSH));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores the mode; the refusal cannot be shown then.
    if fs::write(dir.join("probe"), b"").is_ok() {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    assert!(settings.set("workspace/theme", json!("light")).is_ok());
    assert!(!settings.flush_within(FLUSH));
    assert!(settings
        .error()
        .is_some_and(|error| error.starts_with("Settings not saved")));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    // A later write that succeeds clears the failure.
    assert!(settings.set("workspace/theme", json!("system")).is_ok());
    assert!(settings.flush_within(FLUSH));
}

/// The web client imports every setting's default from
/// `client/src/store/settings-defaults.json`; this keeps that file equal to
/// the table. `ZERUS_UPDATE_SETTINGS_DEFAULTS=1` rewrites it.
#[test]
fn the_web_client_defaults_equal_the_table() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/store/settings-defaults.json");
    let table: serde_json::Map<String, Value> = KEYS
        .iter()
        .map(|key| (key.name.to_owned(), effective(key.kind, None)))
        .collect();
    let expected = serde_json::to_string_pretty(&table).unwrap() + "\n";
    if std::env::var_os("ZERUS_UPDATE_SETTINGS_DEFAULTS").is_some() {
        fs::write(&path, &expected).unwrap();
    }
    // Task #33: compared as text. A Windows checkout with Git's autocrlf
    // holds the fixture with CRLF, and the bytes then differed although the
    // contents are equal.
    assert_eq!(
        fs::read_to_string(&path)
            .unwrap_or_default()
            .replace("\r\n", "\n"),
        expected,
        "regenerate with ZERUS_UPDATE_SETTINGS_DEFAULTS=1 cargo test --test settings"
    );
}

#[test]
fn explicit_child_expansion_persists_without_replacing_collapsed_parents() {
    let scratch = Scratch::new("child-expansion");
    let path = scratch.0.join("settings.json");
    let collapsed =
        "[\"11111111-1111-4111-8111-111111111111\\n22222222-2222-4222-8222-222222222222\"]";
    let expanded =
        "[\"11111111-1111-4111-8111-111111111111\\n33333333-3333-4333-8333-333333333333\"]";
    let settings = Settings::open(path.clone());
    settings
        .set("sessions/collapsedTeams", json!(collapsed))
        .unwrap();
    settings
        .set("sessions/expandedTeams", json!(expanded))
        .unwrap();
    assert!(settings.flush_within(FLUSH));
    drop(settings);
    let reopened = Settings::open(path);
    assert_eq!(reopened.get("sessions/collapsedTeams"), json!(collapsed));
    assert_eq!(reopened.get("sessions/expandedTeams"), json!(expanded));
}

#[test]
fn oversized_preferences_cannot_make_the_existing_store_unreadable() {
    let scratch = Scratch::new("g3-store-limit");
    let path = scratch.0.join("settings.json");
    let settings = Settings::open(path.clone());
    settings.set("workspace/theme", json!("dark")).unwrap();
    assert!(settings.flush_within(FLUSH));
    let before = fs::read(&path).unwrap();
    assert!(settings
        .set("workspace/launchAccounts", json!("x".repeat(4 << 20)))
        .is_err());
    assert_eq!(settings.text("workspace/theme"), "dark");
    assert!(settings.flush_within(FLUSH));
    drop(settings);
    assert_eq!(fs::read(&path).unwrap(), before);
    let reopened = Settings::open(path);
    assert!(reopened.error().is_none());
    assert_eq!(reopened.text("workspace/theme"), "dark");
}

#[test]
fn inspector_preferences_are_device_local_and_explicit_false_wins() {
    use zerus_client_core::settings::{default_value, effective, find};
    let open = find("workspace/inspectorOpen").unwrap();
    assert_eq!(default_value(open.kind), json!("auto"));
    assert_eq!(effective(open.kind, Some(&json!(false))), json!(false));
    assert_eq!(effective(open.kind, Some(&json!(true))), json!(true));
    assert_eq!(effective(open.kind, Some(&json!("bad"))), json!("auto"));
    let width = find("workspace/inspectorWidth").unwrap();
    assert_eq!(effective(width.kind, Some(&json!(100))), json!(240));
    assert_eq!(effective(width.kind, Some(&json!(2000))), json!(900));
    assert!(find("workspace/inspectorTab").is_some());
    assert!(find("workspace/inspectorClosed").is_some());
}

#[test]
fn project_vivid_is_exact_local_boolean_state_and_never_an_old_switch_seed() {
    let scratch = Scratch::new("project-vivid");
    let path = scratch.0.join("settings.json");
    let store = Settings::open(path.clone());
    for id in ["alpha", "nested/project", "project\\id"] {
        let key = format!("projects/vivid/{id}");
        assert!(find(&key).is_some());
        assert!(!store.contains(&key));
        assert_eq!(store.set(&key, json!(true)).unwrap(), json!(true));
        assert_eq!(store.set(&key, json!(false)).unwrap(), json!(false));
    }
    for key in [
        "projects/vivid/".into(),
        "projects/vivid/bad\nidentity".into(),
        format!("projects/vivid/{}", "a".repeat(257)),
    ] {
        assert!(find(&key).is_none());
    }
    assert!(store.flush_within(FLUSH));
    drop(store);
    let store = Settings::open(path);
    assert!(store.contains("projects/vivid/alpha"));
    assert!(!store.flag("projects/vivid/alpha"));
    assert!(!store.contains("projects/vivid/other"));
}
