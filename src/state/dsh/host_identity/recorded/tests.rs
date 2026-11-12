//! The reader's record rules and bounds, carried over from Agent Teams'
//! `caller_hosts` tests, and the walk's rules (zerus-ppd26.15).
use super::*;
use serde_json::json;
use std::collections::HashMap;

const THIS: &str = "11111111-1111-4111-8111-111111111111";
const OTHER: &str = "22222222-2222-4222-8222-222222222222";

fn folder(record: &Value, host_pid: Option<u32>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("host.identity.json"), record.to_string()).unwrap();
    if let Some(pid) = host_pid {
        std::fs::write(dir.path().join("host.pid"), pid.to_string()).unwrap();
    }
    dir
}

fn running(boot: &str) -> Value {
    json!({"state":"running","version":2,"pid":4242,"birth":"77","pid_file":[1,2,3,4],"boot":boot})
}

fn named(pid: u32, birth: Option<&str>) -> Named {
    Named {
        pid,
        birth: birth.map(Into::into),
    }
}

/// The writer's boot read is the reader's: a record stamped with it is this
/// boot's, here as in every real-process test.
#[test]
fn a_record_stamped_with_the_writers_boot_is_this_boots() {
    let Some(boot) = written_boot() else {
        eprintln!("skipped: this computer's boot cannot be read");
        return;
    };
    let dir = folder(&running(&boot), Some(4242));
    let got = named_in(dir.path(), Some(&boot)).unwrap();
    assert_eq!(got, vec![named(4242, Some("77"))]);
}

/// When this boot cannot be read, no record is set aside for its boot.
#[test]
fn an_unread_boot_sets_nothing_aside() {
    let dir = folder(&running(OTHER), Some(4242));
    assert_eq!(
        named_in(dir.path(), None).unwrap(),
        vec![named(4242, Some("77"))]
    );
    let got = named_in(dir.path(), Some(THIS)).unwrap();
    assert_eq!(got, Vec::new(), "another boot, with its own host.pid");
}

/// Only the form the writer stamps can set a record aside; a boot in any
/// other form is unknown, and the record stays named.
#[test]
fn only_the_writers_boot_form_sets_a_record_aside() {
    let lettered = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let dir = folder(&running(lettered), None);
    let got = named_in(dir.path(), Some(THIS)).unwrap();
    assert_eq!(got, Vec::new(), "the written form of another boot");
    for form in [
        lettered.to_uppercase(),
        format!("{{{lettered}}}"),
        lettered.replace('-', ""),
        "garbage".to_owned(),
    ] {
        let dir = folder(&running(&form), None);
        let got = named_in(dir.path(), Some(THIS)).unwrap();
        assert_eq!(got, vec![named(4242, Some("77"))], "{form}");
    }
}

/// A starting record from another boot sets aside the `host.pid` it
/// publishes as its PID file; a different `host.pid` stays named.
#[test]
fn an_old_boot_starting_record_covers_its_own_host_pid() {
    let starting = |pid_file: u32| {
        json!({"state":"starting","version":2,"attempt":"33333333-3333-4333-8333-333333333333",
            "boot":OTHER,"launcher":{"pid":1u32 << 30,"birth":"1"},"phase":"spawning",
            "pid_file":[pid_file,[1,2,3,4]],"previous":null})
    };
    let dir = folder(&starting(5151), Some(5151));
    assert_eq!(named_in(dir.path(), Some(THIS)).unwrap(), Vec::new());
    let dir = folder(&starting(5151), Some(6161));
    assert_eq!(
        named_in(dir.path(), Some(THIS)).unwrap(),
        vec![named(6161, None)]
    );
    let dir = folder(&starting(5151), Some(5151));
    assert_eq!(
        named_in(dir.path(), Some(OTHER)).unwrap(),
        vec![named(5151, None)],
        "this boot's own starting record names its PID file once"
    );
}

/// A private state folder with `dsh/hosts`.
fn state() -> tempfile::TempDir {
    let state = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(state.path().join("dsh/hosts")).unwrap();
    state
}

fn hosts(state: &tempfile::TempDir) -> PathBuf {
    std::fs::canonicalize(state.path())
        .unwrap()
        .join("dsh/hosts")
}

/// A running record and its `host.pid` in host folder `name`.
fn record(state: &tempfile::TempDir, name: &str, pid: u32, birth: &str) {
    let folder = state.path().join("dsh/hosts").join(name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("host.identity.json"),
        json!({"state":"running","version":1,"pid":pid,"birth":birth,"pid_file":[1,2,3,4]})
            .to_string(),
    )
    .unwrap();
    std::fs::write(folder.join("host.pid"), pid.to_string()).unwrap();
}

fn identity(state: &tempfile::TempDir, name: &str, value: &Value) {
    let folder = state.path().join("dsh/hosts").join(name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("host.identity.json"), value.to_string()).unwrap();
}

fn read(state: &tempfile::TempDir, boot: Option<&str>) -> Result<Vec<Named>, Refusal> {
    recorded_hosts_at(state.path(), boot).map(|found| found.into_iter().map(|r| r.named).collect())
}

/// At most 64 host folders are read, two small files each (the identity
/// record and `host.pid`); stray files beside them are not counted. A 65th
/// folder refuses before any file is read, naming the hosts folder.
#[test]
fn recorded_host_folders_are_read_within_their_bound() {
    for (folders, refused) in [(HOST_FOLDERS, false), (HOST_FOLDERS + 1, true)] {
        let state = state();
        for n in 0..folders {
            record(&state, &format!("{n:016x}"), 1 << 30, "1");
        }
        for stray in [".DS_Store", "notes.txt", "host.pid"] {
            std::fs::write(state.path().join("dsh/hosts").join(stray), b"x").unwrap();
        }
        let before = reads();
        let got = read(&state, None);
        let count = reads() - before;
        if refused {
            assert_eq!(
                got,
                Err(Refusal::TooManyFolders(hosts(&state))),
                "the folder bound is its own cause"
            );
            assert_eq!(count, 0);
        } else {
            assert_eq!(got.unwrap().len(), HOST_FOLDERS);
            assert_eq!(count, 2 * HOST_FOLDERS);
        }
    }
}

/// More entries of any kind than the bound refuse with their own cause.
#[test]
fn more_entries_than_the_bound_refuse() {
    for (entries, refused) in [(HOST_ENTRIES, false), (HOST_ENTRIES + 1, true)] {
        let state = state();
        for n in 0..entries {
            std::fs::write(
                state.path().join("dsh/hosts").join(format!("stray-{n}")),
                b"",
            )
            .unwrap();
        }
        let got = read(&state, None);
        if refused {
            assert_eq!(
                got,
                Err(Refusal::TooManyEntries(hosts(&state))),
                "the entry bound is its own cause"
            );
        } else {
            assert_eq!(got, Ok(Vec::new()));
        }
    }
}

/// A state folder, or its `dsh` or `hosts` folder, that does not exist records
/// no host. One that exists but cannot be read as a folder refuses, naming it.
#[test]
fn a_missing_state_or_host_folder_records_no_host() {
    let gone = tempfile::tempdir().unwrap();
    let missing = gone.path().join("state");
    assert_eq!(recorded_hosts_at(&missing, None), Ok(Vec::new()));
    let state = tempfile::tempdir().unwrap();
    assert_eq!(read(&state, None), Ok(Vec::new()));
    std::fs::create_dir(state.path().join("dsh")).unwrap();
    assert_eq!(read(&state, None), Ok(Vec::new()));
    std::fs::write(state.path().join("dsh/hosts"), b"not a folder").unwrap();
    assert_eq!(
        read(&state, None),
        Err(Refusal::UnreadableRecord(
            std::fs::canonicalize(state.path())
                .unwrap()
                .join("dsh/hosts")
        ))
    );
}

/// Only directories are host folders: a stray `.DS_Store` beside them is not
/// one, alone or next to a recorded host.
#[test]
fn a_stray_file_in_the_host_records_is_not_a_host_folder() {
    let state = state();
    std::fs::write(state.path().join("dsh/hosts/.DS_Store"), b"\0").unwrap();
    assert_eq!(read(&state, None), Ok(Vec::new()));
    record(&state, "0000000000000001", 4242, "77");
    assert_eq!(read(&state, None), Ok(vec![named(4242, Some("77"))]));
}

/// A link in place of a host folder may lead to a live host: it refuses like
/// an unreadable record and names the link.
#[test]
fn a_link_in_place_of_a_host_folder_refuses() {
    let state = state();
    record(&state, "0000000000000001", 4242, "77");
    let hosts_dir = state.path().join("dsh/hosts");
    let moved = state.path().join("moved-host");
    std::fs::rename(hosts_dir.join("0000000000000001"), &moved).unwrap();
    std::os::unix::fs::symlink(&moved, hosts_dir.join("0000000000000001")).unwrap();
    assert_eq!(
        read(&state, None),
        Err(Refusal::UnreadableRecord(
            hosts(&state).join("0000000000000001")
        ))
    );
}

/// A malformed record names its own file: the identity record or `host.pid`.
#[test]
fn a_malformed_record_names_its_file() {
    let state = state();
    let folder = state.path().join("dsh/hosts/0000000000000001");
    std::fs::create_dir_all(&folder).unwrap();
    for (file, bytes) in [
        ("host.identity.json", b"{".as_slice()),
        (
            "host.identity.json",
            br#"{"state":"running","pid":0,"birth":"1"}"#,
        ),
        ("host.identity.json", br#"{"state":"stopped"}"#),
        ("host.pid", b"not a pid"),
    ] {
        for name in ["host.identity.json", "host.pid"] {
            let _ = std::fs::remove_file(folder.join(name));
        }
        std::fs::write(folder.join(file), bytes).unwrap();
        assert_eq!(
            read(&state, None),
            Err(Refusal::UnreadableRecord(
                hosts(&state).join("0000000000000001").join(file)
            )),
            "{file}"
        );
    }
}

/// A starting record names the previous host with its birth and a PID file
/// without one; its launcher is not a host, and a start that ended before
/// its PID file is named nowhere (the stated limit).
#[test]
fn a_starting_record_names_its_previous_host_and_its_pid_file() {
    let boot = written_boot();
    let starting = |previous: Value, pid_file: Value, launcher: u32| {
        json!({"state":"starting","version":2,
            "attempt":"11111111-1111-4111-8111-111111111111","boot":boot,
            "launcher":{"pid":launcher,"birth":"9"},"phase":"spawning",
            "pid_file":pid_file,"previous":previous})
    };
    let state = state();
    identity(
        &state,
        "0000000000000001",
        &starting(
            json!({"version":2,"pid":4242,"birth":"77","pid_file":[1,2,3,4],"boot":boot}),
            Value::Null,
            99,
        ),
    );
    assert_eq!(
        read(&state, boot.as_deref()),
        Ok(vec![named(4242, Some("77"))])
    );
    identity(
        &state,
        "0000000000000001",
        &starting(Value::Null, json!([5151, [1, 2, 3, 4]]), 99),
    );
    assert_eq!(read(&state, boot.as_deref()), Ok(vec![named(5151, None)]));
    identity(
        &state,
        "0000000000000001",
        &starting(Value::Null, Value::Null, 99),
    );
    assert_eq!(
        read(&state, boot.as_deref()),
        Ok(Vec::new()),
        "the launcher is not a host"
    );
}

/// A record that carries another boot's identity names no process of this
/// boot, running or starting; one without a boot identity still names it.
#[test]
fn a_host_record_from_another_boot_is_not_this_boots_host() {
    let state = state();
    identity(
        &state,
        "0000000000000001",
        &json!({"state":"running","version":2,"pid":4242,"birth":"77","pid_file":[1,2,3,4],"boot":OTHER}),
    );
    std::fs::write(
        state.path().join("dsh/hosts/0000000000000001/host.pid"),
        "4242",
    )
    .unwrap();
    assert_eq!(read(&state, Some(THIS)), Ok(Vec::new()));
    identity(
        &state,
        "0000000000000001",
        &json!({"state":"running","version":1,"pid":4242,"birth":"77","pid_file":[1,2,3,4]}),
    );
    assert_eq!(read(&state, Some(THIS)), Ok(vec![named(4242, Some("77"))]));
}

/// Each record keeps the file that names it.
#[test]
fn each_named_process_keeps_its_record_file() {
    let state = state();
    identity(
        &state,
        "0000000000000001",
        &json!({"state":"running","version":1,"pid":4242,"birth":"77","pid_file":[1,2,3,4]}),
    );
    std::fs::write(
        state.path().join("dsh/hosts/0000000000000001/host.pid"),
        "6161",
    )
    .unwrap();
    let folder = hosts(&state).join("0000000000000001");
    assert_eq!(
        recorded_hosts_at(state.path(), None).unwrap(),
        vec![
            Recorded {
                named: named(4242, Some("77")),
                record: folder.join("host.identity.json")
            },
            Recorded {
                named: named(6161, None),
                record: folder.join("host.pid")
            },
        ]
    );
}

fn own(parent: u32, birth: Option<&str>) -> Inspection {
    Inspection::Own(Process {
        parent,
        birth: birth.map(Into::into),
    })
}

/// A fake process table: pid -> inspection.
fn table(rows: &[(u32, Inspection)]) -> impl FnMut(u32) -> Result<Inspection, ()> + Clone {
    let rows: HashMap<u32, Inspection> = rows.iter().cloned().collect();
    move |pid| rows.get(&pid).cloned().ok_or(())
}

/// The walk ends at the first process of another user, which it does not
/// inspect further; that last same-user process is the top.
#[test]
fn the_walk_ends_at_the_first_foreign_parent() {
    let rows = [
        (10, own(20, Some("b10"))),
        (20, own(30, Some("b20"))),
        (30, own(1, Some("b30"))),
        (1, Inspection::Foreign),
    ];
    let chain = walk(10, table(&rows)).unwrap();
    assert_eq!(
        chain.iter().map(|a| a.pid).collect::<Vec<_>>(),
        vec![10, 20, 30]
    );
    assert_eq!(
        chain.iter().map(|a| a.top).collect::<Vec<_>>(),
        vec![false, false, true]
    );
    assert_eq!(unchanged(&chain, table(&rows)), Ok(()));
}

/// A parent of zero, a loop or an unreadable step refuses as an unreadable
/// ancestry.
#[test]
fn an_unreadable_ancestry_refuses() {
    let unreadable = Err(Refusal::UnreadableAncestry);
    assert_eq!(
        walk(10, table(&[(10, own(0, Some("b")))])),
        unreadable,
        "parent zero"
    );
    assert_eq!(
        walk(10, table(&[(10, own(10, Some("b")))])),
        unreadable,
        "own parent"
    );
    assert_eq!(
        walk(
            10,
            table(&[(10, own(20, Some("b"))), (20, own(10, Some("c")))])
        ),
        unreadable,
        "a loop"
    );
    assert_eq!(
        walk(10, table(&[(10, own(20, Some("b")))])),
        unreadable,
        "an unreadable parent"
    );
    assert_eq!(walk(10, table(&[])), unreadable, "an unreadable command");
}

/// Required by Agent Teams: a chain that reaches the bound without the top is
/// a refusal of its own, never a top and never "no host".
#[test]
fn the_walk_fails_closed_at_its_bound() {
    let steps = ANCESTRY_STEPS as u32;
    let mut deep: Vec<(u32, Inspection)> = (0..steps)
        .map(|n| (100 + n, own(101 + n, Some("b"))))
        .collect();
    deep.push((100 + steps, own(1, Some("b"))));
    deep.push((1, Inspection::Foreign));
    assert_eq!(
        walk(100, table(&deep)),
        Err(Refusal::AncestryBound),
        "one past the bound"
    );
    let chain = walk(101, table(&deep)).unwrap();
    assert_eq!(
        chain.len(),
        ANCESTRY_STEPS,
        "exactly the bound reaches the top"
    );
    assert!(chain.last().unwrap().top);
}

/// Required by Agent Teams: an unreadable birth anywhere on the chain, the
/// top included, is a refusal naming that process, never a mismatch that
/// reads as "no host".
#[test]
fn an_unreadable_birth_on_the_chain_refuses() {
    let foreign = (1, Inspection::Foreign);
    assert_eq!(
        walk(
            10,
            table(&[
                (10, own(20, None)),
                (20, own(1, Some("c"))),
                foreign.clone()
            ])
        ),
        Err(Refusal::UnreadableBirth(10)),
        "below the top"
    );
    assert_eq!(
        walk(
            10,
            table(&[(10, own(20, Some("b"))), (20, own(1, None)), foreign])
        ),
        Err(Refusal::UnreadableBirth(20)),
        "the top"
    );
    // The matcher, given such a chain, refuses whether or not a record names it.
    let chain = [Ancestor {
        pid: 10,
        identity: Process {
            parent: 1,
            birth: None,
        },
        top: true,
    }];
    let record = Recorded {
        named: named(10, Some("b10")),
        record: PathBuf::from("/state/dsh/hosts/10/host.identity.json"),
    };
    assert_eq!(hosted(&chain, &[record]), Err(Refusal::UnreadableBirth(10)));
    assert_eq!(hosted(&chain, &[]), Err(Refusal::UnreadableBirth(10)));
}

/// A process that changes while it is read, or after the walk, refuses.
#[test]
fn a_changed_chain_refuses() {
    let mut reads = 0;
    let changing = move |pid: u32| match pid {
        10 => {
            reads += 1;
            Ok(own(20, Some(if reads == 1 { "b" } else { "c" })))
        }
        20 => Ok(own(1, Some("d"))),
        _ => Ok(Inspection::Foreign),
    };
    assert_eq!(walk(10, changing), Err(Refusal::UnreadableAncestry));
    let rows = [
        (10, own(20, Some("b"))),
        (20, own(1, Some("d"))),
        (1, Inspection::Foreign),
    ];
    let chain = walk(10, table(&rows)).unwrap();
    let later = [
        (10, own(20, Some("x"))),
        (20, own(1, Some("d"))),
        (1, Inspection::Foreign),
    ];
    assert_eq!(
        unchanged(&chain, table(&later)),
        Err(Refusal::UnreadableAncestry)
    );
    let adopted = [
        (10, own(20, Some("b"))),
        (20, own(1, Some("d"))),
        (1, own(0, None)),
    ];
    assert_eq!(
        unchanged(&chain, table(&adopted)),
        Err(Refusal::UnreadableAncestry),
        "the top's parent became own"
    );
}

/// A recorded host matches an ancestor by process ID and birth; another birth
/// is not that host; a record without a birth whose ID is an ancestor's
/// refuses and names its file.
#[test]
fn a_recorded_host_matches_an_ancestor_by_id_and_birth() {
    let rows = [
        (10, own(20, Some("b10"))),
        (20, own(1, Some("b20"))),
        (1, Inspection::Foreign),
    ];
    let chain = walk(10, table(&rows)).unwrap();
    let host = |pid: u32, birth: Option<&str>| Recorded {
        named: named(pid, birth),
        record: PathBuf::from(format!("/state/dsh/hosts/{pid}/host.identity.json")),
    };
    assert_eq!(
        hosted(&chain, &[host(20, Some("b20"))]),
        Ok(Some(Host {
            pid: 20,
            birth: "b20".into()
        }))
    );
    assert_eq!(hosted(&chain, &[host(20, Some("other"))]), Ok(None));
    assert_eq!(hosted(&chain, &[host(30, Some("b30"))]), Ok(None));
    assert_eq!(hosted(&chain, &[]), Ok(None));
    assert_eq!(
        hosted(&chain, &[host(10, None)]),
        Err(Refusal::BirthlessRecord(PathBuf::from(
            "/state/dsh/hosts/10/host.identity.json"
        )))
    );
}

/// This process's own walk reaches a top of another user.
#[test]
fn this_process_walks_to_a_top() {
    let chain = walk(std::process::id(), inspect).unwrap();
    assert!(chain.last().unwrap().top);
    assert_eq!(chain[0].pid, std::process::id());
    assert_eq!(unchanged(&chain, inspect), Ok(()));
}

/// Every cause has its own sentence; a record's names the exact path and how
/// to clear it.
#[test]
fn each_refusal_names_its_cause_and_how_to_clear_it() {
    let path = PathBuf::from("/state/dsh/hosts/0000000000000001/host.identity.json");
    let folder = PathBuf::from("/state/dsh/hosts");
    for refusal in [
        Refusal::UnreadableRecord(path.clone()),
        Refusal::BirthlessRecord(path.clone()),
    ] {
        let sentence = refusal.sentence();
        assert!(
            sentence.contains(&format!("remove {}.", path.display())),
            "{sentence}"
        );
    }
    let folders = Refusal::TooManyFolders(folder.clone()).sentence();
    assert!(
        folders.contains("more DeepSeek agent host folders than the check reads (64"),
        "{folders}"
    );
    let entries = Refusal::TooManyEntries(folder).sentence();
    assert!(entries.contains("more than 1024 entries"), "{entries}");
    for sentence in [&folders, &entries] {
        assert!(sentence.contains("from /state/dsh/hosts."), "{sentence}");
    }
    assert!(Refusal::AncestryBound
        .sentence()
        .contains("more than 64 processes"));
    assert!(Refusal::UnreadableBirth(4242)
        .sentence()
        .contains("process 4242"));
    assert!(Refusal::UnreadableAncestry
        .sentence()
        .contains("cannot be read"));
    let all = [
        Refusal::TooManyFolders(PathBuf::new()),
        Refusal::TooManyEntries(PathBuf::new()),
        Refusal::UnreadableRecord(PathBuf::new()),
        Refusal::BirthlessRecord(PathBuf::new()),
        Refusal::AncestryBound,
        Refusal::UnreadableBirth(1),
        Refusal::UnreadableAncestry,
    ]
    .map(|refusal| refusal.sentence());
    let distinct: std::collections::BTreeSet<_> = all.iter().collect();
    assert_eq!(distinct.len(), all.len());
    assert!(!all.iter().any(|sentence| sentence == UNDER_HOST));
}
