use super::*;
use crate::state::listing_fixture as producer;

const NAME: &str = "codex/example/member";
const RUN: &str = "exit-run";
const SESSION: &str = "00000000-0000-4000-8000-000000000051";
const LAUNCH: &str = "00000000-0000-4000-8000-000000000052";
const OTHER: &str = "00000000-0000-4000-8000-000000000053";
const CASE: &str = "state::archive_catalog::teams::tests::archive_case_child";
const WORKER: &str = "state::archive_catalog::teams::tests::private_repair_worker";

fn run(case: &str) {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    crate::platform::private_dir(&state).unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CASE, "--ignored", "--test-threads=1"])
        .env("HGS_STATE_DIR", state)
        .env("HGS_ARCHIVE_TEST_CASE", case)
        .output()
        .unwrap();
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn record() -> Value {
    let mut record = producer::launched_record(
        "codex",
        NAME,
        LAUNCH,
        SESSION,
        RUN,
        "/fixture/worktree",
        "test-account",
    );
    producer::confirmed(&mut record, "first-conversation");
    producer::save(&mut record).unwrap();
    record
}

fn catalog_gap() -> String {
    let id = producer::archive_with_catalog_gap(&record()).unwrap();
    assert!(root().join("archive-catalog-v1.sqlite3").is_file());
    assert!(archive::path(&id).unwrap().is_file());
    id
}

#[test]
fn one_native_run_with_two_catalog_identities_is_refused() {
    run("duplicate");
}

#[test]
fn an_incomplete_disk_probe_never_decides_a_negative() {
    run("incomplete");
}

#[test]
fn damaged_and_future_reservations_recover_one_attempt() {
    run("reservation");
}

#[test]
fn a_teams_reader_spawns_the_bounded_worker_with_a_scrubbed_environment() {
    run("spawn");
}

#[test]
fn gone_members_share_one_member_archive_probe_per_pass() {
    run("batch");
}

#[test]
fn an_earlier_archive_of_the_session_cannot_settle_a_later_run() {
    run("historical");
}

#[test]
#[ignore = "run in a private state root"]
fn archive_case_child() {
    match std::env::var("HGS_ARCHIVE_TEST_CASE").unwrap().as_str() {
        "duplicate" => {
            let mut record = record();
            let first = producer::archive(&record).unwrap();
            assert_eq!(find_run(&root(), NAME, RUN).unwrap(), Some(first.clone()));
            producer::confirmed(&mut record, "second-conversation");
            let second = producer::archive(&record).unwrap();
            assert_ne!(first, second);
            assert_eq!(
                find_run(&root(), NAME, RUN).unwrap_err(),
                "No exact unique exit archive"
            );
        }
        "incomplete" => {
            let id = catalog_gap();
            // Model damage to the actual published original in place.
            fs::write(archive::path(&id).unwrap(), b"{broken").unwrap();
            assert_eq!(
                member_archive_pending(&root(), OTHER, LAUNCH).unwrap_err(),
                INCOMPLETE
            );
        }
        "reservation" => {
            catalog_gap();
            let stamp = root().join("archive-catalog-v2/teams-attempt.json");
            for damaged in [
                b"{broken".to_vec(),
                br#"{"at":999999999999}"#.to_vec(),
                vec![b'x'; 4097],
            ] {
                fs::write(&stamp, &damaged).unwrap();
                assert!(find_run(&root(), NAME, RUN)
                    .unwrap_err()
                    .starts_with(INCOMPLETE));
                let current = fs::read(&stamp).unwrap();
                let at = serde_json::from_slice::<Value>(&current).unwrap()["at"]
                    .as_f64()
                    .unwrap();
                assert!((0.0..30.0).contains(&(now() - at)), "{at}");
                assert!(find_run(&root(), NAME, RUN).is_err());
                assert_eq!(
                    fs::read(&stamp).unwrap(),
                    current,
                    "healthy cooldown across reads"
                );
            }
        }
        "spawn" => {
            let id = catalog_gap();
            std::env::set_var("HGS_ARCHIVE_SPAWN_SENTINEL", "parent-only");
            refresh::TEST_WORKER.with(|worker| worker.set(Some(WORKER)));
            assert!(find_run(&root(), NAME, RUN)
                .unwrap_err()
                .starts_with(INCOMPLETE));
            let settled = root().join("repair-exit-ok");
            let deadline = Instant::now() + Duration::from_secs(10);
            while !settled.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                settled.is_file(),
                "the reader's child did not finish the worker"
            );
            assert_eq!(find_run(&root(), NAME, RUN).unwrap(), Some(id));
        }
        "historical" => {
            let id = catalog_gap();
            // The advisory sighting is for an archive from the session.
            assert!(member_archive_pending(&root(), SESSION, LAUNCH).unwrap());
            assert_eq!(team_worker().unwrap(), 0);
            assert_eq!(find_run(&root(), NAME, RUN).unwrap(), Some(id));
            assert_eq!(find_run(&root(), NAME, "later-run").unwrap(), None);
        }
        "batch" => {
            catalog_gap();
            with_member_archive_probe(|| {
                assert!(member_archive_pending(&root(), SESSION, LAUNCH).unwrap());
                assert!(!member_archive_pending(&root(), OTHER, LAUNCH).unwrap());
                assert_eq!(PROBE_READS.with(std::cell::Cell::get), 1);
            });
            with_member_archive_probe(|| {
                assert!(!member_archive_pending(&root(), OTHER, LAUNCH).unwrap());
                assert_eq!(PROBE_READS.with(std::cell::Cell::get), 2);
            });
        }
        case => panic!("{case}"),
    }
}

#[test]
#[ignore = "launched by the Teams reader fixture"]
fn private_repair_worker() {
    let keys: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| key.to_string_lossy().starts_with("HGS_"))
        .map(|(key, _)| key)
        .collect();
    assert_eq!(keys, ["HGS_STATE_DIR"]);
    assert_eq!(
        std::env::current_dir().unwrap(),
        crate::platform::canonical(root()).unwrap()
    );
    let args: Vec<String> =
        serde_json::from_str(&std::env::var("TEST_ARCHIVE_WORKER_ARGV").unwrap()).unwrap();
    assert_eq!(args, ["__state", "archive-catalog-worker", "--teams-repair"]);
    assert_eq!(crate::state::dispatch(&args[1..]).unwrap(), 0);
    fs::write(root().join("repair-exit-ok"), b"").unwrap();
}
