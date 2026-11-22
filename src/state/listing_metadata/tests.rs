use super::*;
use std::fs;
mod bench;
mod child_detail;
mod mobile;
mod parity;
mod readers;
mod writers;

fn enable(root: &Path) {
    let root = checked(Directory::open(&crate::platform::canonical(root).unwrap())).unwrap();
    checked(root.ensure_child("listing-metadata"))
        .unwrap()
        .replace_file("enabled", b"1\n")
        .unwrap();
}
fn fixture() -> tempfile::TempDir {
    let root = crate::host::staging::private_tempdir();
    enable(root.path());
    root
}

#[test]
fn publication_removal_and_repair_require_explicit_opt_in() {
    for operation in ["write", "remove", "repair"] {
        let root = crate::host::staging::private_tempdir();
        let path = path(root.path());
        let value = record();
        storage::atomic(&path, &value.to_string()).unwrap();
        let mut calls = 0;
        match operation {
            "write" => {
                let result: Result<()> = write_at(root.path(), &path, &value, |publication| {
                    calls += 1;
                    storage::atomic_observed(&path, &value.to_string(), publication)?;
                    Err("original unknown outcome".into())
                });
                assert_eq!(result.unwrap_err(), "original unknown outcome");
                assert_eq!(calls, 1);
            }
            "remove" => {
                remove_at(root.path(), &path, || {
                    calls += 1;
                    fs::remove_file(&path).map_err(|e| e.to_string())
                })
                .unwrap();
                assert_eq!(calls, 1);
                assert!(!path.exists());
            }
            _ => {
                let mut work = None;
                let step = background_at(root.path(), &mut work).unwrap();
                assert_eq!((step.entries, step.rows, step.bytes), (0, 0, 0));
                assert!(!step.complete, "disabled display evidence is unavailable");
                assert!(work.is_none());
            }
        }
        assert!(
            !root.path().join("listing-metadata").exists(),
            "default-off {operation} must not create advisory state"
        );
    }
}

#[test]
fn invalid_opt_in_or_the_kill_switch_preserves_an_existing_cache_unchanged() {
    for mode in ["absent", "invalid", "disabled", "directory"] {
        let root = fixture();
        let store = Store::open(root.path()).unwrap();
        drop(store);
        let marker = root.path().join("listing-metadata/enabled");
        match mode {
            "absent" => fs::remove_file(&marker).unwrap(),
            "invalid" => fs::write(&marker, b"0\n").unwrap(),
            "disabled" => fs::write(root.path().join("listing-metadata/disabled"), b"").unwrap(),
            _ => {
                fs::remove_file(&marker).unwrap();
                fs::create_dir(&marker).unwrap();
            }
        }
        let path = path(root.path());
        let value = record();
        write_at(root.path(), &path, &value, |publication| {
            storage::atomic_observed(&path, &value.to_string(), publication)
        })
        .unwrap();
        remove_at(root.path(), &path, || {
            fs::remove_file(&path).map_err(|e| e.to_string())
        })
        .unwrap();
        let step = background_at(root.path(), &mut None).unwrap();
        assert_eq!((step.entries, step.rows, step.bytes), (0, 0, 0));
        assert!(!step.complete);
        assert!(!root
            .path()
            .join("listing-metadata/coverage-lost.json")
            .exists());
        let db = rusqlite::Connection::open_with_flags(
            root.path().join("listing-metadata/index.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let changes: (i64, i64, i64) = db
            .query_row(
                "SELECT writes,(SELECT count(*) FROM intents),(SELECT count(*) FROM sources) FROM meta",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(changes, (0, 0, 0), "{mode}");
    }
}

#[test]
#[cfg(unix)]
fn linked_shared_or_unreadable_markers_cannot_enable_advisory_io() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    for mode in ["symlink", "shared", "unreadable"] {
        let root = fixture();
        let marker = root.path().join("listing-metadata/enabled");
        let other = root.path().join("listing-metadata/other");
        match mode {
            "symlink" => {
                fs::rename(&marker, &other).unwrap();
                symlink(&other, &marker).unwrap();
            }
            "shared" => fs::hard_link(&marker, &other).unwrap(),
            _ => fs::set_permissions(&marker, fs::Permissions::from_mode(0o000)).unwrap(),
        }
        assert!(!enabled(root.path()), "{mode}");
        let value = record();
        let path = path(root.path());
        write_at(root.path(), &path, &value, |publication| {
            storage::atomic_observed(&path, &value.to_string(), publication)
        })
        .unwrap();
        assert!(
            !root.path().join("listing-metadata/index.sqlite3").exists(),
            "{mode}"
        );
    }
}

#[test]
fn opting_out_discards_in_progress_repair_without_more_background_io() {
    let root = fixture();
    let mut value = record();
    value.as_object_mut().unwrap().remove("shell_jobs");
    for n in 0..300 {
        value["name"] = json!(format!("codex/example/opt-out-{n}"));
        let path = root.path().join(format!(
            "{}.json",
            storage::label_key(string(&value, "name"))
        ));
        storage::atomic(&path, &value.to_string()).unwrap();
    }
    let mut work = None;
    let first = background_at(root.path(), &mut work).unwrap();
    assert!(!first.done && work.is_some());
    fs::remove_file(root.path().join("listing-metadata/enabled")).unwrap();
    let step = background_at(root.path(), &mut work).unwrap();
    assert_eq!((step.entries, step.rows, step.bytes), (0, 0, 0));
    assert!(!step.complete && work.is_none());
}

fn record() -> Value {
    json!({"version":1,"name":"codex/example/one","agent":"codex",
        "session_id":"11111111-1111-4111-8111-111111111111",
        "run_id":"22222222-2222-4222-8222-222222222222",
        "conversation_id":"33333333-3333-4333-8333-333333333333",
        "journal_name":"session:11111111-1111-4111-8111-111111111111",
        "activity":"idle","phase":"interrupted","pid":null,
        "process_start":null,"prompt":"Public prompt","last_message":"Public reply",
        "shell_jobs":{"private":{"output":"x".repeat(260_000)}},
        "active_tools":{},"subagents":{}})
}

#[test]
fn overlapping_writes_never_pair_an_old_seed_with_a_later_file_stamp() {
    for writer in ["instrumented", "legacy-atomic", "legacy-in-place"] {
        let root = fixture();
        let selected = path(root.path());
        let first = record();
        let mut second = first.clone();
        second["phase"] = json!("working");
        second["last_message"] = json!("Newer canonical record");
        let (mut first_calls, mut second_calls) = (0, 0);
        write_at(root.path(), &selected, &first, |publication| {
            first_calls += 1;
            storage::atomic_observed(&selected, &first.to_string(), publication)?;
            if writer == "instrumented" {
                write_at(root.path(), &selected, &second, |publication| {
                    second_calls += 1;
                    storage::atomic_observed(&selected, &second.to_string(), publication)
                })
            } else {
                second_calls += 1;
                if writer == "legacy-in-place" {
                    fs::write(&selected, second.to_string()).map_err(|e| e.to_string())
                } else {
                    storage::atomic(&selected, &second.to_string())
                }
            }
        })
        .unwrap();
        assert_eq!((first_calls, second_calls), (1, 1), "{writer}");
        let actual: Value = serde_json::from_slice(&fs::read(&selected).unwrap()).unwrap();
        assert_eq!(actual["phase"], "working");
        assert!(repair_to_end(root.path()).complete, "{writer}");
        let page = Store::open(root.path()).unwrap().page(None).unwrap();
        assert!(page.fresh);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(
            page.rows[0].1["record"]["phase"], actual["phase"],
            "{writer}: seed provenance cannot be borrowed from another publication"
        );
        assert_eq!(
            page.rows[0].1["record"]["last_message"],
            actual["last_message"]
        );
    }
}

#[test]
fn missing_wrong_target_or_reused_publication_evidence_is_never_certified() {
    for mode in ["missing", "wrong-target", "reused"] {
        let root = fixture();
        let selected = path(root.path());
        let first = record();
        let mut second = first.clone();
        second["phase"] = json!("working");
        storage::atomic(&selected, &second.to_string()).unwrap();
        let mut calls = 0;
        write_at(root.path(), &selected, &first, |mut publication| {
            calls += 1;
            match mode {
                "missing" => Ok(()),
                "wrong-target" => storage::atomic_observed(
                    &root.path().join("other.json"),
                    &first.to_string(),
                    publication,
                ),
                _ => {
                    storage::atomic_observed(
                        &selected,
                        &first.to_string(),
                        publication.as_deref_mut(),
                    )?;
                    storage::atomic_observed(&selected, &second.to_string(), publication)
                }
            }
        })
        .unwrap();
        assert_eq!(
            calls, 1,
            "{mode}: advisory evidence cannot change or replay the callback"
        );
        let store = Store::open(root.path()).unwrap();
        let (rows, pending): (i64, i64) = store
            .db
            .query_row(
                "SELECT (SELECT count(*) FROM sources),(SELECT count(*) FROM intents)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (rows, pending),
            (0, 1),
            "{mode}: unproved data cannot settle"
        );
        drop(store);
        assert!(repair_to_end(root.path()).complete);
        let page = Store::open(root.path()).unwrap().page(None).unwrap();
        assert!(page.fresh);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].1["record"]["phase"], "working", "{mode}");
        assert_eq!(calls, 1);
    }
}

#[test]
fn a_changed_or_substituted_staging_object_cannot_certify_a_publication() {
    for mode in ["in-place-change", "different-object", "unobserved"] {
        let root = fixture();
        let selected = path(root.path());
        let first = record();
        let mut second = first.clone();
        second["phase"] = json!("working");
        storage::atomic(&selected, &first.to_string()).unwrap();
        let file = File::open(&selected).unwrap();
        let mut publication = Publication::default();
        if mode != "unobserved" {
            publication.staged(&file);
        }
        let file = if mode == "in-place-change" {
            fs::write(&selected, second.to_string()).unwrap();
            file
        } else if mode == "different-object" {
            storage::atomic(&selected, &second.to_string()).unwrap();
            File::open(&selected).unwrap()
        } else {
            file
        };
        publication.published(file);
        let store = Store::open(root.path()).unwrap();
        let key = Key::at(root.path(), &selected).unwrap();
        assert!(publication.prove(&store, &key).is_err(), "{mode}");
    }
}

#[test]
#[cfg(unix)]
fn directory_publications_keep_their_own_objects_and_preserve_later_writers() {
    let root = fixture();
    let selected = path(root.path());
    let name = selected.file_name().unwrap().to_str().unwrap();
    let directory = checked(Directory::open(
        &crate::platform::canonical(root.path()).unwrap(),
    ))
    .unwrap();
    let mut value = record();
    write_at(root.path(), &selected, &value, |publication| {
        Publication::write_new(
            publication,
            &directory,
            name,
            format!("{value}\n").as_bytes(),
        )
    })
    .unwrap();
    assert_eq!(cached(root.path())["record"]["phase"], "interrupted");
    value["phase"] = json!("input");
    write_at(root.path(), &selected, &value, |publication| {
        Publication::replace(
            publication,
            &directory,
            name,
            format!("{value}\n").as_bytes(),
        )
    })
    .unwrap();
    assert_eq!(cached(root.path())["record"]["phase"], "input");
    value["phase"] = json!("approval");
    let mut later = value.clone();
    later["phase"] = json!("working");
    let mut calls = 0;
    write_at(root.path(), &selected, &value, |publication| {
        calls += 1;
        Publication::replace(
            publication,
            &directory,
            name,
            format!("{value}\n").as_bytes(),
        )?;
        storage::atomic(&selected, &later.to_string())
    })
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(
        cached(root.path())["record"]["phase"],
        "input",
        "unconfirmed publication keeps the prior certified projection"
    );
    assert!(repair_to_end(root.path()).complete);
    assert_eq!(cached(root.path())["record"]["phase"], "working");
    assert_eq!(calls, 1);
}

#[test]
fn legacy_seed_rows_are_unavailable_until_bounded_canonical_revalidation() {
    let root = fixture();
    let selected = path(root.path());
    let first = record();
    write_at(root.path(), &selected, &first, |publication| {
        storage::atomic_observed(&selected, &first.to_string(), publication)
    })
    .unwrap();
    assert!(repair_to_end(root.path()).complete);
    let mut second = first.clone();
    second["phase"] = json!("working");
    storage::atomic(&selected, &second.to_string()).unwrap();
    let key = Key::at(root.path(), &selected).unwrap();
    let mut legacy: Value = serde_json::from_str(&Seed::encode(&key, &first).unwrap()).unwrap();
    legacy["version"] = json!(1);
    let legacy = legacy.to_string();
    assert!(
        Seed::decode(&key, &legacy).is_err(),
        "v1 provenance must never be grandfathered"
    );
    let store = Store::open(root.path()).unwrap();
    let later_stamp = store.source_stamp(&key).unwrap().unwrap();
    store
        .db
        .execute(
            "UPDATE sources SET seed=?1,stamp=?2 WHERE source=?3",
            params![legacy, later_stamp, key.0],
        )
        .unwrap();
    store
        .db
        .execute(
            "UPDATE snapshots SET seed=?1 WHERE source=?2",
            params![legacy, key.0],
        )
        .unwrap();
    assert!(
        store.page(None).is_err(),
        "legacy generations are unavailable, never fresh"
    );
    drop(store);
    let mut work = None;
    let mut read = 0;
    let mut complete = false;
    for _ in 0..128 {
        let step = background_at(root.path(), &mut work).unwrap();
        read += step.bytes;
        assert!(step.entries <= 256 && step.rows <= 128 && step.bytes <= 16 << 20);
        if step.done {
            complete = step.complete;
            break;
        }
    }
    assert!(complete);
    assert_eq!(
        read as u64,
        fs::metadata(&selected).unwrap().len(),
        "legacy rows require exactly one bounded canonical read"
    );
    let page = Store::open(root.path()).unwrap().page(None).unwrap();
    assert!(page.fresh);
    assert_eq!(page.rows[0].1["record"]["phase"], "working");
}

#[test]
fn canonical_publication_has_a_durable_pre_file_display_intent() {
    let root = fixture();
    let path = root
        .path()
        .join(format!("{}.json", storage::label_key("codex/example/one")));
    let value = record();
    let mut called = 0;
    write_at(root.path(), &path, &value, |publication| {
        called += 1;
        let db = rusqlite::Connection::open_with_flags(
            root.path().join("listing-metadata/index.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("durable advisory intent must precede canonical publication");
        let pending: i64 = db
            .query_row("SELECT count(*) FROM intents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pending, 1);
        storage::atomic_observed(&path, &value.to_string(), publication)
    })
    .unwrap();
    assert_eq!(called, 1, "canonical publication is never replayed");
}

#[test]
fn canonical_publication_settles_only_small_public_display_inputs() {
    let root = fixture();
    let path = root
        .path()
        .join(format!("{}.json", storage::label_key("codex/example/one")));
    let mut value = record();
    value["keep_warm_quiet"] =
        json!({"prompt":"Original public prompt","last_message":"Original public reply"});
    let original = value.clone();
    write_at(root.path(), &path, &value, |publication| {
        storage::atomic_observed(&path, &value.to_string(), publication)
    })
    .unwrap();
    assert_eq!(
        value, original,
        "display evidence must not mutate canonical content"
    );
    let db = rusqlite::Connection::open_with_flags(
        root.path().join("listing-metadata/index.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("display publication must exist after a successful canonical write");
    let (seed, pending): (String, i64) = db
        .query_row(
            "SELECT seed,(SELECT count(*) FROM intents) FROM sources WHERE tombstone=0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(pending, 0);
    assert!(
        seed.len() < 4096,
        "large shell output must never enter the display projection"
    );
    let seed: Value = serde_json::from_str(&seed).unwrap();
    assert!(seed["record"].get("shell_jobs").is_none());
    assert_eq!(seed["record"]["name"], value["name"]);
    assert_eq!(seed["record"]["keep_warm_quiet"], value["keep_warm_quiet"]);
}

fn path(root: &Path) -> PathBuf {
    root.join(format!("{}.json", storage::label_key("codex/example/one")))
}
fn cached(root: &Path) -> Value {
    let store = Store::open(root).unwrap();
    let seed: String = store
        .db
        .query_row("SELECT seed FROM sources WHERE tombstone=0", [], |r| {
            r.get(0)
        })
        .unwrap();
    serde_json::from_str(&seed).unwrap()
}

#[test]
fn public_seed_keeps_identity_and_display_status_parity_without_private_payloads() {
    let root = fixture();
    let key = Key::at(root.path(), &path(root.path())).unwrap();
    let mut value = record();
    value["account_id"] = json!("fixture-account");
    value["agent_home"] = json!("/srv/example/agent");
    value["supervisor"] = json!({"pid":123,"start":"original supervisor birth"});
    value["parent"] = json!({"schema":1,"machine":"44444444-4444-4444-8444-444444444444","host":"example","name":"claude/example/parent","session_id":value["session_id"],"source":"launch","at":1});
    value["last_event_at"] = json!(100);
    value["subagents"] = json!({"child":{"name":"Child","state":"working","display_state":"unknown","attention_id":"q1","detail":"Visible child detail","updated":10,"completion_source":"hook","tracking_evidence":"native","process_state":"unknown","private_body":"secret"}});
    value["active_tools"] = json!({"question-1":{"name":"AskUserQuestion","detail":"Choose","started":20,"arguments":{"private":"secret"}}});
    for phase in [
        "working",
        "input",
        "approval",
        "interrupted",
        "compacting",
        "error",
    ] {
        value["phase"] = json!(phase);
        let seed: Value = serde_json::from_str(&Seed::encode(&key, &value).unwrap()).unwrap();
        let reduced = &seed["record"];
        assert_eq!(reduced["supervisor"], value["supervisor"]);
        assert_eq!(account_binding(reduced), account_binding(&value));
        assert_eq!(parent_link::listed(reduced), parent_link::listed(&value));
        assert_eq!(
            journal::journal_name(reduced),
            journal::journal_name(&value)
        );
        for live in [true, false] {
            let activity = journal::normalized_activity(&value, live);
            assert_eq!(journal::normalized_activity(reduced, live), activity);
            assert_eq!(
                telemetry::summary(reduced, &activity, "AskUserQuestion", "Choose"),
                telemetry::summary(&value, &activity, "AskUserQuestion", "Choose")
            );
        }
        assert!(reduced["active_tools"]["question-1"]
            .get("arguments")
            .is_none());
        assert!(reduced["subagents"]["child"].get("private_body").is_none());
    }
}

#[test]
fn an_unknown_canonical_outcome_keeps_the_last_projection_and_pending_intent() {
    let root = fixture();
    let path = path(root.path());
    let mut value = record();
    write_at(root.path(), &path, &value, |publication| {
        storage::atomic_observed(&path, &value.to_string(), publication)
    })
    .unwrap();
    value["phase"] = json!("input");
    let mut calls = 0;
    let result: Result<()> = write_at(root.path(), &path, &value, |publication| {
        calls += 1;
        storage::atomic_observed(&path, &value.to_string(), publication)?;
        Err("unknown source outcome".into())
    });
    assert_eq!(calls, 1);
    assert_eq!(result.unwrap_err(), "unknown source outcome");
    assert_eq!(cached(root.path())["record"]["phase"], "interrupted");
    let store = Store::open(root.path()).unwrap();
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM intents", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn corrupt_disabled_busy_or_oversized_advisory_state_never_fails_or_replays_canonical_io() {
    for failure in ["corrupt", "disabled", "busy", "oversized"] {
        let root = fixture();
        let path = path(root.path());
        let mut value = record();
        let store = Store::open(root.path()).unwrap();
        let held = if failure == "busy" {
            Some(store)
        } else {
            drop(store);
            None
        };
        match failure {
            "corrupt" => fs::write(
                root.path().join("listing-metadata/index.sqlite3"),
                b"not a database",
            )
            .unwrap(),
            "disabled" => fs::write(root.path().join("listing-metadata/disabled"), b"").unwrap(),
            "oversized" => {
                value["active_tools"] = json!({"q":{"name":"Question","detail":"x".repeat(20<<10)}})
            }
            _ => (),
        }
        let original = value.clone();
        let mut calls = 0;
        write_at(root.path(), &path, &value, |publication| {
            calls += 1;
            storage::atomic_observed(&path, &value.to_string(), publication)
        })
        .unwrap();
        assert_eq!(calls, 1, "{failure}");
        assert_eq!(value, original);
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
            value
        );
        assert_eq!(
            root.path()
                .join("listing-metadata/coverage-lost.json")
                .is_file(),
            failure != "disabled",
            "{failure}: explicit disablement must not write coverage markers"
        );
        drop(held);
    }
}

#[test]
fn every_binding_layout_is_eligible_but_archives_routes_and_wrong_identities_are_not() {
    let root = fixture();
    let mut value = record();
    for directory in storage::BINDING_DIRS {
        let agent = if directory.starts_with("dsh/") {
            "dsh"
        } else if directory.starts_with("opencode/") {
            "opencode"
        } else {
            "codex"
        };
        value["name"] = json!(format!("{agent}/example/one"));
        value["agent"] = json!(agent);
        if agent != "codex" {
            value["backend"] = json!(agent);
            value["conversation_id"] = json!(if agent == "dsh" {
                "session-native"
            } else {
                "ses_native"
            });
        }
        let id = directory.ends_with("sessions");
        value["layout"] = if id { json!("id") } else { Value::Null };
        let file = if id {
            string(&value, "session_id").to_owned()
        } else {
            storage::label_key(string(&value, "name"))
        };
        let key = Key::at(
            root.path(),
            &root.path().join(directory).join(format!("{file}.json")),
        )
        .unwrap();
        Seed::encode(&key, &value).unwrap();
        let mut changed = value.clone();
        changed[if id { "session_id" } else { "name" }] = json!("different");
        assert!(Seed::encode(&key, &changed).is_err());
    }
    for directory in ["archives", "name_routes", "renames", "../outside", "dsh"] {
        assert!(Key::at(
            root.path(),
            &root
                .path()
                .join(directory)
                .join(format!("{}.json", storage::label_key("codex/example/one")))
        )
        .is_none());
    }
}

fn repair_to_end(root: &Path) -> repair::Stats {
    let mut work = None;
    for _ in 0..2048 {
        let s = background_at(root, &mut work).unwrap();
        assert!(s.entries <= 256 && s.bytes <= 16 << 20 && s.rows <= 128);
        if s.done {
            return s;
        }
    }
    panic!("bounded background pass did not finish");
}

#[test]
fn background_repair_reads_legacy_sources_once_and_never_rewrites_them() {
    let root = fixture();
    let mut originals = Vec::new();
    for n in 0..70 {
        let mut value = record();
        value["name"] = json!(format!("codex/example/session-{n}"));
        let path = root.path().join(format!(
            "{}.json",
            storage::label_key(string(&value, "name"))
        ));
        let bytes = serde_json::to_vec(&value).unwrap();
        storage::atomic(&path, std::str::from_utf8(&bytes).unwrap()).unwrap();
        originals.push((path, bytes));
    }
    assert!(repair_to_end(root.path()).complete);
    let store = Store::open(root.path()).unwrap();
    let rows: i64 = store
        .db
        .query_row("SELECT count(*) FROM sources WHERE tombstone=0", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 70);
    drop(store);
    let mut work = None;
    let mut read = 0;
    loop {
        let s = background_at(root.path(), &mut work).unwrap();
        read += s.bytes;
        if s.done {
            assert!(s.complete);
            break;
        }
    }
    assert_eq!(
        read, 0,
        "unchanged canonical bodies must not be parsed again"
    );
    for (path, bytes) in originals {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn repair_observes_an_unknown_write_and_a_lost_deletion_without_replaying_either() {
    let root = fixture();
    let path = path(root.path());
    let mut value = record();
    write_at(root.path(), &path, &value, |publication| {
        storage::atomic_observed(&path, &value.to_string(), publication)
    })
    .unwrap();
    value["phase"] = json!("approval");
    let mut calls = 0;
    let _: Result<()> = write_at(root.path(), &path, &value, |publication| {
        calls += 1;
        storage::atomic_observed(&path, &value.to_string(), publication)?;
        Err("unknown".into())
    });
    assert!(repair_to_end(root.path()).complete);
    assert_eq!(calls, 1);
    assert_eq!(cached(root.path())["record"]["phase"], "approval");
    // Legacy binary/dead publisher: no index or gap marker writes at all.
    fs::remove_file(&path).unwrap();
    assert!(repair_to_end(root.path()).complete);
    let store = Store::open(root.path()).unwrap();
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM sources WHERE tombstone=0", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert!(
        !path.exists(),
        "repair never recreates a removed canonical record"
    );
}

#[test]
fn incomplete_or_unreadable_coverage_retains_last_known_rows_and_never_publishes_absence() {
    let root = fixture();
    let path = path(root.path());
    let value = record();
    write_at(root.path(), &path, &value, |publication| {
        storage::atomic_observed(&path, &value.to_string(), publication)
    })
    .unwrap();
    assert!(repair_to_end(root.path()).complete);
    let before = Store::open(root.path()).unwrap().page(None).unwrap();
    assert!(before.fresh);
    assert_eq!(before.rows.len(), 1);
    storage::atomic(&path, "invalid current record").unwrap();
    assert!(!repair_to_end(root.path()).complete);
    let store = Store::open(root.path()).unwrap();
    let last = store.page(None).unwrap();
    assert!(!last.fresh);
    assert_eq!(last.rows, before.rows);
    assert_eq!(
        cached_after_drop(store, root.path())["record"]["name"],
        value["name"]
    );
}
fn cached_after_drop(store: Store, root: &Path) -> Value {
    drop(store);
    cached(root)
}

#[test]
fn paged_generation_is_stable_while_new_canonical_records_arrive() {
    let root = fixture();
    for n in 0..150 {
        let mut value = record();
        value["name"] = json!(format!("codex/example/s-{n}"));
        let path = root.path().join(format!(
            "{}.json",
            storage::label_key(string(&value, "name"))
        ));
        storage::atomic(&path, &value.to_string()).unwrap();
    }
    assert!(repair_to_end(root.path()).complete);
    let first = Store::open(root.path()).unwrap().page(None).unwrap();
    assert_eq!(first.rows.len(), 128);
    let cursor = first.next.unwrap();
    let mut value = record();
    value["name"] = json!("codex/example/new-arrival");
    let path = root.path().join(format!(
        "{}.json",
        storage::label_key(string(&value, "name"))
    ));
    write_at(root.path(), &path, &value, |publication| {
        storage::atomic_observed(&path, &value.to_string(), publication)
    })
    .unwrap();
    assert!(repair_to_end(root.path()).complete);
    let old = Store::open(root.path())
        .unwrap()
        .page(Some(&cursor))
        .unwrap();
    assert_eq!(old.rows.len(), 22);
    assert!(old.next.is_none());
    assert!(!old.fresh);
    let mut all = first.rows;
    all.extend(old.rows);
    assert!(all
        .iter()
        .all(|r| r.1["record"]["name"] != "codex/example/new-arrival"));
    let mut got = 0;
    let mut next = None;
    loop {
        let page = Store::open(root.path())
            .unwrap()
            .page(next.as_ref())
            .unwrap();
        assert!(page.fresh);
        got += page.rows.len();
        next = page.next;
        if next.is_none() {
            break;
        }
    }
    assert_eq!(got, 151);
}

#[test]
fn failed_json_reads_are_charged_to_the_same_raw_byte_budget() {
    let root = fixture();
    for n in 0..6 {
        let path = root.path().join(format!(
            "{}.json",
            storage::label_key(&format!("codex/example/bad-{n}"))
        ));
        storage::atomic(&path, &"x".repeat(4 << 20)).unwrap();
    }
    let mut work = None;
    let mut total = 0;
    loop {
        let step = background_at(root.path(), &mut work).unwrap();
        assert!(step.bytes <= 16 << 20);
        total += step.bytes;
        if step.done {
            assert!(!step.complete);
            break;
        }
    }
    assert_eq!(total, 24 << 20);
    assert!(Store::open(root.path()).unwrap().page(None).is_err());
}

#[test]
fn enumerating_non_record_files_is_bounded_and_restarts_after_a_crash() {
    let root = fixture();
    for n in 0..600 {
        fs::write(root.path().join(format!("unrelated-{n}")), b"").unwrap();
    }
    let value = record();
    let path = path(root.path());
    storage::atomic(&path, &value.to_string()).unwrap();
    let mut work = None;
    let first = background_at(root.path(), &mut work).unwrap();
    assert!(first.entries <= 256);
    assert!(!first.done);
    drop(work); // the retained ReadDir is gone; durable queue is not.
    assert!(repair_to_end(root.path()).complete);
    assert_eq!(
        Store::open(root.path())
            .unwrap()
            .page(None)
            .unwrap()
            .rows
            .len(),
        1
    );
}

#[test]
fn a_corrupt_or_partially_created_display_store_rebuilds_in_background_only() {
    for contents in [b"corrupt sqlite header".as_slice(), b"".as_slice()] {
        let root = fixture();
        let path = path(root.path());
        let value = record();
        write_at(root.path(), &path, &value, |publication| {
            storage::atomic_observed(&path, &value.to_string(), publication)
        })
        .unwrap();
        let original = fs::read(&path).unwrap();
        fs::write(root.path().join("listing-metadata/index.sqlite3"), contents).unwrap();
        assert!(repair_to_end(root.path()).complete);
        let page = Store::open(root.path()).unwrap().page(None).unwrap();
        assert!(page.fresh);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}

#[test]
fn failed_canonical_publication_is_observed_as_absent_without_recreating_it() {
    let root = fixture();
    let path = path(root.path());
    let value = record();
    let mut calls = 0;
    let result: Result<()> = write_at(root.path(), &path, &value, |_| {
        calls += 1;
        Err("original error".into())
    });
    assert_eq!(result.unwrap_err(), "original error");
    assert!(!path.exists());
    assert!(repair_to_end(root.path()).complete);
    assert_eq!(calls, 1);
    assert!(!path.exists());
    assert!(Store::open(root.path())
        .unwrap()
        .page(None)
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn repair_never_waits_for_or_acquires_the_canonical_writer() {
    use fs2::FileExt;
    let root = fixture();
    let path = path(root.path());
    let value = record();
    storage::atomic(&path, &value.to_string()).unwrap();
    storage::atomic(&root.path().join(".lock"), "").unwrap();
    let floor = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join(".lock"))
        .unwrap();
    floor.lock_exclusive().unwrap();
    // Stable display inputs remain observable without taking this floor.
    assert!(repair_to_end(root.path()).complete);
    assert!(Store::open(root.path()).is_ok());
    drop(floor);
}

#[test]
fn an_unfinished_rename_journal_keeps_display_coverage_incomplete_after_a_crash() {
    let root = fixture();
    let path = path(root.path());
    let value = record();
    storage::atomic(&path, &value.to_string()).unwrap();
    let pending = root
        .path()
        .join("renames/11111111-1111-4111-8111-111111111111.json");
    storage::atomic(&pending, "{}").unwrap();
    assert!(!repair_to_end(root.path()).complete);
    assert!(Store::open(root.path()).unwrap().page(None).is_err());
    fs::remove_file(pending).unwrap();
    assert!(repair_to_end(root.path()).complete);
}

#[test]
#[cfg(unix)]
fn a_changed_root_alias_cannot_attach_new_seed_data_to_an_old_canonical_file() {
    let parent = crate::host::staging::private_tempdir();
    let a = parent.path().join("a");
    let b = parent.path().join("b");
    private_dir(&a).unwrap();
    private_dir(&b).unwrap();
    enable(&a);
    let alias = parent.path().join("selected");
    std::os::unix::fs::symlink(&a, &alias).unwrap();
    let at = path(&alias);
    let mut value = record();
    write_at(&alias, &at, &value, |publication| {
        storage::atomic_observed(&at, &value.to_string(), publication)
    })
    .unwrap();
    value["phase"] = json!("approval");
    write_at(&alias, &at, &value, |publication| {
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&b, &alias).unwrap();
        storage::atomic_observed(&at, &value.to_string(), publication)
    })
    .unwrap();
    assert_eq!(cached(&a)["record"]["phase"], "interrupted");
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(path(&b)).unwrap()).unwrap()["phase"],
        "approval"
    );
}

#[test]
fn an_unknown_future_cache_schema_is_unavailable_without_failing_canonical_io() {
    let root = fixture();
    let path = path(root.path());
    let value = record();
    let store = Store::open(root.path()).unwrap();
    store.db.execute_batch("PRAGMA user_version=999;").unwrap();
    drop(store);
    write_at(root.path(), &path, &value, |publication| {
        storage::atomic_observed(&path, &value.to_string(), publication)
    })
    .unwrap();
    let db =
        rusqlite::Connection::open(root.path().join("listing-metadata/index.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM sources", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let mut work = None;
    assert!(background_at(root.path(), &mut work).is_err());
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        999
    );
    assert!(path.is_file());
}

#[test]
fn repeated_repairs_keep_only_bounded_generations_instead_of_growing_history() {
    let root = fixture();
    for n in 0..150 {
        let mut value = record();
        value["name"] = json!(format!("codex/example/bounded-{n}"));
        let path = root.path().join(format!(
            "{}.json",
            storage::label_key(string(&value, "name"))
        ));
        storage::atomic(&path, &value.to_string()).unwrap();
    }
    for _ in 0..12 {
        assert!(repair_to_end(root.path()).complete);
    }
    let store = Store::open(root.path()).unwrap();
    let rows: i64 = store
        .db
        .query_row("SELECT count(*) FROM snapshots", [], |r| r.get(0))
        .unwrap();
    assert!(
        rows <= 3 * 150,
        "old observed generations grew without a bound: {rows}"
    );
}

#[test]
fn replacing_the_cache_epoch_mid_enumeration_restarts_the_directory_cursor() {
    let root = fixture();
    for n in 0..600 {
        let mut value = record();
        value.as_object_mut().unwrap().remove("shell_jobs");
        value["name"] = json!(format!("codex/example/epoch-{n}"));
        let path = root.path().join(format!(
            "{}.json",
            storage::label_key(string(&value, "name"))
        ));
        storage::atomic(&path, &value.to_string()).unwrap();
    }
    let mut work = None;
    let first = background_at(root.path(), &mut work).unwrap();
    assert!(!first.done);
    fs::remove_dir_all(root.path().join("listing-metadata")).unwrap();
    enable(root.path());
    let mut done = false;
    for _ in 0..128 {
        let step = background_at(root.path(), &mut work).unwrap();
        if step.done {
            assert!(step.complete);
            done = true;
            break;
        }
    }
    assert!(done);
    let store = Store::open(root.path()).unwrap();
    let rows:i64=store.db.query_row("SELECT count(*) FROM snapshots WHERE generation=(SELECT generation FROM meta WHERE id=1)",[],|r|r.get(0)).unwrap();
    assert_eq!(
        rows, 600,
        "a reset must not skip the old cursor's already-enumerated prefix"
    );
}

#[test]
fn valid_json_corruption_cannot_be_returned_as_fresh_display_data() {
    let root = fixture();
    let path = path(root.path());
    let value = record();
    write_at(root.path(), &path, &value, |publication| {
        storage::atomic_observed(&path, &value.to_string(), publication)
    })
    .unwrap();
    assert!(repair_to_end(root.path()).complete);
    let store = Store::open(root.path()).unwrap();
    store.db.execute_batch("UPDATE sources SET seed=json_set(seed,'$.record.phase','approval'); UPDATE snapshots SET seed=json_set(seed,'$.record.phase','approval');").unwrap();
    assert!(
        store.page(None).is_err(),
        "a checksum-mismatched cache row is unavailable, not fresh"
    );
    drop(store);
    assert!(repair_to_end(root.path()).complete);
    assert_eq!(cached(root.path())["record"]["phase"], "interrupted");
    let page = Store::open(root.path()).unwrap().page(None).unwrap();
    assert!(page.fresh);
    assert_eq!(page.rows[0].1["record"]["phase"], "interrupted");
}

#[test]
fn a_full_advisory_intent_queue_still_allows_the_original_canonical_write() {
    let root = fixture();
    let path = path(root.path());
    let value = record();
    let store = Store::open(root.path()).unwrap();
    store.db.execute_batch("WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<4096) INSERT INTO intents SELECT CAST(x AS TEXT),'fixture',NULL,0 FROM n;").unwrap();
    drop(store);
    let mut calls = 0;
    write_at(root.path(), &path, &value, |publication| {
        calls += 1;
        storage::atomic_observed(&path, &value.to_string(), publication)
    })
    .unwrap();
    assert_eq!(calls, 1);
    assert!(path.is_file());
    assert!(root
        .path()
        .join("listing-metadata/coverage-lost.json")
        .is_file());
}

#[test]
fn canonical_deletion_is_reserved_before_removal_and_settles_one_tombstone() {
    let root = fixture();
    let path = path(root.path());
    let value = record();
    write_at(root.path(), &path, &value, |publication| {
        storage::atomic_observed(&path, &value.to_string(), publication)
    })
    .unwrap();
    let mut calls = 0;
    remove_at(root.path(), &path, || {
        calls += 1;
        let db = rusqlite::Connection::open_with_flags(
            root.path().join("listing-metadata/index.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM intents WHERE seed IS NULL", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        fs::remove_file(&path).map_err(|e| e.to_string())?;
        crate::platform::sync_dir(root.path()).map_err(|e| e.to_string())
    })
    .unwrap();
    assert_eq!(calls, 1);
    assert!(!path.exists());
    let store = Store::open(root.path()).unwrap();
    assert_eq!(
        store
            .db
            .query_row("SELECT tombstone FROM sources", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM intents", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn unsupported_display_inputs_still_reserve_an_unknown_change_before_canonical_io() {
    let root = fixture();
    let path = path(root.path());
    let mut value = record();
    value["layout_transition"] = json!({"version":1});
    let mut calls = 0;
    write_at(root.path(), &path, &value, |publication| {
        calls += 1;
        let db = rusqlite::Connection::open_with_flags(
            root.path().join("listing-metadata/index.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM intents", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        storage::atomic_observed(&path, &value.to_string(), publication)
    })
    .unwrap();
    assert_eq!(calls, 1);
    assert!(path.is_file());
    let store = Store::open(root.path()).unwrap();
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM intents", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(store);
    assert!(!repair_to_end(root.path()).complete);
}
