//! `run` against a fake `hgs` (tests/support/fake_hgs_actions.py), never the real CLI.
//! The fake is a Python script run through its shebang and a symlink.
#![cfg(unix)]
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use zerus_client_core::actions::{change_notifications, classify, invocation};
use zerus_client_core::attachments::Attachment;
use zerus_client_core::{run, run_with_env, Action, Exit, Outcome, Target};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Fake {
    dir: PathBuf,
    hgs: PathBuf,
}

impl Fake {
    fn new(config: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "zerus-fake-hgs-actions-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let hgs = dir.join("hgs");
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        // A symlink to the checked-in (executable) script, never a copy: writing
        // a new executable while another test thread forks lets the child
        // inherit the write fd, and exec then fails with ETXTBSY. The fake finds
        // its config and log beside the link, not beside the target.
        std::os::unix::fs::symlink(manifest.join("tests/support/fake_hgs_actions.py"), &hgs)
            .unwrap();
        std::fs::write(dir.join("config.json"), config).unwrap();
        Self { dir, hgs }
    }

    fn log(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.join("log.ndjson"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn target() -> Target {
    Target {
        computer: String::new(),
        name: "claude/sample-project/review".into(),
        run_id: "run-1".into(),
        conversation_id: "conv-1".into(),
    }
}

#[test]
fn an_interrupt_through_the_fake_is_done_and_logged_once() {
    let fake = Fake::new(r#"{"reply":{"exit":0,"stdout":"echo_interrupt"}}"#);
    let t = target();
    let a = Action::Interrupt { turn_started: 1.5 };
    let inv = invocation(&t, &a, "req-1").unwrap();
    assert!(matches!(
        classify(&t, &a, &inv, run(&fake.hgs, &inv)),
        Outcome::Done { .. }
    ));
    assert_eq!(fake.log().len(), 1);
    assert_eq!(
        fake.log()[0]["argv"],
        serde_json::json!(["interrupt", "claude/sample-project/review", "--json"])
    );
}

#[test]
fn a_terminate_with_a_cleanup_line_reports_it() {
    let fake = Fake::new(
        r#"{"reply":{"exit":0,"stderr":"ZERUS_WORKTREE_CLEANUP {\"status\":\"unconfirmed\",\"path\":\"/home/example/wt\",\"common_dir\":\"/home/example/repo/.git\",\"reason\":\"in use\"}\n"}}"#,
    );
    let t = target();
    let inv = invocation(&t, &Action::Terminate, "u").unwrap();
    match classify(&t, &Action::Terminate, &inv, run(&fake.hgs, &inv)) {
        Outcome::Done {
            cleanup: Some(c), ..
        } => assert_eq!(c.status, "unconfirmed"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_untracked_kill_refuses_without_calling_the_fake() {
    let fake = Fake::new(r#"{"reply":{"exit":0}}"#);
    let mut t = target();
    t.run_id.clear();
    t.conversation_id.clear();
    assert_eq!(
        invocation(&t, &Action::Kill, "u").unwrap_err(),
        zerus_client_core::untracked_shell::UNVERIFIED
    );
    assert!(
        fake.log().is_empty(),
        "an untracked kill must never reach the fake"
    );
}

#[test]
fn a_tracked_kill_through_the_fake_is_one_call_without_input() {
    let fake = Fake::new(r#"{"reply":{"exit":0}}"#);
    let t = target();
    let inv = invocation(&t, &Action::Kill, "u").unwrap();
    assert!(matches!(
        classify(&t, &Action::Kill, &inv, run(&fake.hgs, &inv)),
        Outcome::Done { cleanup: None, .. }
    ));
    let log = fake.log();
    assert_eq!(log.len(), 1);
    assert_eq!(
        log[0]["argv"],
        serde_json::json!(["kill", "claude/sample-project/review"])
    );
    assert_eq!(log[0]["stdin"], "");
}

#[test]
fn pause_all_through_the_fake_is_one_computer_wide_call() {
    let fake = Fake::new(r#"{"reply":{"exit":0}}"#);
    let mut t = target();
    t.name.clear();
    t.run_id.clear();
    t.conversation_id.clear();
    for (action, argv) in [
        (
            Action::PauseAll { count: 2 },
            serde_json::json!(["pause", "--all"]),
        ),
        (
            Action::ResumeAll { count: 2 },
            serde_json::json!(["resume", "--all", "-d"]),
        ),
    ] {
        let inv = invocation(&t, &action, "u").unwrap();
        assert!(matches!(
            classify(&t, &action, &inv, run(&fake.hgs, &inv)),
            Outcome::Done { cleanup: None, .. }
        ));
        assert_eq!(fake.log().last().unwrap()["argv"], argv);
    }
    assert_eq!(fake.log().len(), 2);
}

#[test]
fn a_hanging_hgs_times_out_and_is_killed() {
    let fake = Fake::new(r#"{"reply":{"exit":0,"sleep":30}}"#);
    let mut inv = invocation(&target(), &Action::Pause, "u").unwrap();
    inv.timeout = std::time::Duration::from_millis(500);
    let started = std::time::Instant::now();
    assert!(matches!(run(&fake.hgs, &inv), Exit::TimedOut));
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
}

#[test]
fn a_missing_hgs_fails_to_start() {
    let inv = invocation(&target(), &Action::Pause, "u").unwrap();
    assert!(matches!(
        run(Path::new("/nonexistent/hgs"), &inv),
        Exit::StartFailed(_)
    ));
}

#[test]
fn the_agent_attribution_is_not_inherited() {
    let fake = Fake::new(r#"{"reply":{"exit":0,"log_env":["HGS_SESSION","HGS_RUN_ID"]}}"#);
    let inv = invocation(&target(), &Action::Pause, "u").unwrap();
    assert!(matches!(
        run_with_env(
            &fake.hgs,
            &inv,
            &[("HGS_SESSION", "x"), ("HGS_RUN_ID", "y")]
        ),
        Exit::Code { code: 0, .. }
    ));
    assert_eq!(
        fake.log()[0]["env"],
        serde_json::json!({"HGS_SESSION": null, "HGS_RUN_ID": null})
    );
}

#[test]
fn output_survives_a_grandchild_that_keeps_the_pipes_open() {
    let fake =
        Fake::new(r#"{"reply":{"exit":0,"stdout":{"status":"ok"},"stderr":"note","linger":5}}"#);
    let inv = invocation(&target(), &Action::Pause, "u").unwrap();
    let started = std::time::Instant::now();
    match run(&fake.hgs, &inv) {
        Exit::Code {
            code: 0,
            stdout,
            stderr,
        } => {
            assert_eq!(stdout, br#"{"status": "ok"}"#);
            assert_eq!(stderr, "note");
        }
        other => panic!("{other:?}"),
    }
    // One 2 s drain budget, not one per pipe; the grandchild ends by itself.
    assert!(started.elapsed() < std::time::Duration::from_millis(3500));
}

#[test]
fn an_early_exit_without_reading_stdin_reports_its_real_exit() {
    let fake =
        Fake::new(r#"{"reply":{"exit":1,"stderr":"hgs: no such session\n","read_stdin":false}}"#);
    let mut inv = invocation(&target(), &Action::Pause, "u").unwrap();
    inv.stdin = Some(vec![b'x'; 256 * 1024]);
    match run(&fake.hgs, &inv) {
        Exit::Code {
            code: 1, stderr, ..
        } => assert!(stderr.contains("no such session")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_reference_through_the_fake_is_the_uri() {
    use zerus_client_core::actions::{parse_reference, reference_invocation};
    let fake =
        Fake::new(r#"{"reply":{"exit":0,"stdout":{"uri":"zerus://session?machine=m&name=n"}}}"#);
    let inv = reference_invocation(&target(), None);
    assert_eq!(
        parse_reference(run(&fake.hgs, &inv)),
        Ok("zerus://session?machine=m&name=n".to_owned())
    );
    let log = fake.log();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["argv"][0], "reference");
}

fn preferences(muted: bool, revision: i64) -> Value {
    serde_json::json!({"conversation_id": "conv-1", "muted": muted, "revision": revision, "suppressed": {
        "reply_id": null, "attention_signature": null, "optional_question_ids": [],
        "main_attention": null, "child_attention": {}}})
}

fn notifications_config(help: &str, reply: Value) -> String {
    serde_json::json!({
        "reply": {"exit": 1, "stderr": "unexpected call"},
        "replies": [
            {"match": ["--help"], "exit": 0, "stdout_text": help},
            {"match": ["notifications"], "exit": 0, "stdout": reply},
        ]
    })
    .to_string()
}

#[test]
fn a_notification_change_probes_the_local_help_then_calls_once() {
    let reply = serde_json::json!({"ok": true, "applied": true, "run_id": "run-1",
        "conversation_id": "conv-1", "preferences": preferences(true, 5)});
    let fake = Fake::new(&notifications_config(
        "usage\nsession-notifications-v1\n",
        reply,
    ));
    let mut t = target();
    t.computer = "mac".into();
    let out = change_notifications(&fake.hgs, &t, true, Some(4)).unwrap();
    assert_eq!(out, preferences(true, 5));
    let log = fake.log();
    assert_eq!(log.len(), 2);
    // The probe is local and has no input; the call goes to the peer.
    assert_eq!(log[0]["argv"], serde_json::json!(["--help"]));
    assert_eq!(log[0]["stdin"], "");
    assert_eq!(
        log[1]["argv"],
        serde_json::json!([
            "@mac",
            "notifications",
            "claude/sample-project/review",
            "--json"
        ])
    );
    assert_eq!(
        serde_json::from_str::<Value>(log[1]["stdin"].as_str().unwrap()).unwrap(),
        serde_json::json!({"run_id": "run-1", "conversation_id": "conv-1", "muted": true, "expected_revision": 4})
    );
}

#[test]
fn a_cli_without_the_marker_is_refused_before_the_call() {
    let fake = Fake::new(&notifications_config("usage\n", serde_json::json!({})));
    let error = change_notifications(&fake.hgs, &target(), true, Some(4)).unwrap_err();
    assert_eq!(
        error,
        "Update the configured local hgs to support shared notification preferences."
    );
    assert_eq!(fake.log().len(), 1, "only the probe ran");
}

#[test]
fn a_failing_help_probe_is_refused_and_a_bad_identity_runs_nothing() {
    let fake = Fake::new(r#"{"reply":{"exit":2,"stdout_text":"session-notifications-v1\n"}}"#);
    assert!(change_notifications(&fake.hgs, &target(), true, Some(4))
        .unwrap_err()
        .starts_with("Update the configured local hgs"));
    let none = Fake::new(r#"{"reply":{"exit":0}}"#);
    let mut t = target();
    t.run_id.clear();
    assert!(change_notifications(&none.hgs, &t, true, Some(4))
        .unwrap_err()
        .starts_with("The session notification identity"));
    assert!(none.log().is_empty());
}

#[test]
fn an_unconfirmed_reply_reports_stderr_and_a_huge_help_is_refused() {
    let bad = serde_json::json!({"ok": true, "applied": true, "run_id": "run-1",
        "conversation_id": "conv-1", "preferences": preferences(true, 9)});
    let fake = Fake::new(&notifications_config("session-notifications-v1\n", bad));
    assert_eq!(
        change_notifications(&fake.hgs, &target(), true, Some(4)).unwrap_err(),
        "The shared notification change was not confirmed. Refresh the session before trying again."
    );
    let huge = "x".repeat(300 * 1024);
    let big = Fake::new(&notifications_config(
        &format!("{huge}\nsession-notifications-v1\n"),
        serde_json::json!({}),
    ));
    assert!(change_notifications(&big.hgs, &target(), true, Some(4))
        .unwrap_err()
        .starts_with("Update the configured local hgs"));
}

fn send(text: &str) -> Action {
    Action::Send {
        text: text.into(),
        attachments: vec![Attachment {
            name: "notes.txt".into(),
            mime: "text/plain".into(),
            bytes: b"file bytes".as_slice().into(),
            reference: "[File #1]".into(),
        }],
        agent_id: Some("agent-7".into()),
        expected_compaction_id: None,
    }
}

#[test]
fn a_send_through_the_fake_is_done_with_the_message_only_on_stdin() {
    let fake = Fake::new(r#"{"reply":{"exit":0,"stdout":"echo_send"}}"#);
    let t = target();
    let a = send("private message text");
    let inv = invocation(&t, &a, "req-1").unwrap();
    let Outcome::Done { receipt, .. } = classify(&t, &a, &inv, run(&fake.hgs, &inv)) else {
        panic!("a done send")
    };
    assert_eq!(receipt["agent_id"], "agent-7");
    let log = fake.log();
    assert_eq!(log.len(), 1);
    assert_eq!(
        log[0]["argv"],
        serde_json::json!(["send", "claude/sample-project/review", "--json"])
    );
    let stdin: Value = serde_json::from_str(log[0]["stdin"].as_str().unwrap()).unwrap();
    assert_eq!(stdin["text"], "private message text");
    assert_eq!(stdin["attachments"][0]["data_base64"], "ZmlsZSBieXRlcw==");
}

#[test]
fn an_unconfirmed_send_is_uncertain_and_runs_once() {
    // The fake acknowledges another run: the message may have arrived.
    let fake = Fake::new(
        r#"{"reply":{"exit":0,"stdout":{"status":"submitted","request_id":"req-1","agent_id":"agent-7","name":"claude/sample-project/review","run_id":"run-2","conversation_id":"conv-1"}}}"#,
    );
    let t = target();
    let a = send("hello");
    let inv = invocation(&t, &a, "req-1").unwrap();
    assert_eq!(
        classify(&t, &a, &inv, run(&fake.hgs, &inv)),
        Outcome::Uncertain {
            detail: "Delivery acknowledgement is invalid. Inspect Terminal before retrying.".into()
        }
    );
    assert_eq!(fake.log().len(), 1);
}

#[test]
fn staged_files_through_the_fake_are_done() {
    let fake = Fake::new(r#"{"reply":{"exit":0,"stdout":"echo_stage"}}"#);
    let file = fake.dir.join("drop.txt");
    std::fs::write(&file, b"dropped").unwrap();
    let t = target();
    let a = Action::StageFiles {
        paths: vec![file],
        expected_created: None,
    };
    let inv = invocation(&t, &a, "req-1").unwrap();
    assert!(matches!(
        classify(&t, &a, &inv, run(&fake.hgs, &inv)),
        Outcome::Done { .. }
    ));
    let log = fake.log();
    assert_eq!(
        log[0]["argv"],
        serde_json::json!([
            "attachment",
            "claude/sample-project/review",
            "--stage",
            "--json"
        ])
    );
    let stdin: Value = serde_json::from_str(log[0]["stdin"].as_str().unwrap()).unwrap();
    assert_eq!(stdin["attachments"][0]["name"], "drop.txt");
}

#[test]
fn a_process_stop_through_the_fake_is_done_once_without_input() {
    let fake = Fake::new(
        r#"{"reply":{"exit":0,"stdout":{"id":"job-1","run_id":"run-1","conversation_id":"conv-1","status":"requested"}}}"#,
    );
    let t = target();
    let a = Action::StopProcess {
        process_id: "job-1".into(),
        archive_id: None,
        generation: None,
    };
    let inv = invocation(&t, &a, "u").unwrap();
    assert!(matches!(
        classify(&t, &a, &inv, run(&fake.hgs, &inv)),
        Outcome::Done { .. }
    ));
    let log = fake.log();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["stdin"], "");
    assert_eq!(
        log[0]["argv"],
        serde_json::json!([
            "processes",
            "claude/sample-project/review",
            "--stop",
            "job-1",
            "--run",
            "run-1",
            "--conversation",
            "conv-1"
        ])
    );
}

#[test]
fn a_forget_binding_through_the_fake_sends_one_request_and_reads_the_receipt() {
    for (status, error, done) in [
        ("completed", None, true),
        ("failed", Some("busy"), false),
        ("uncertain", None, false),
    ] {
        let mut reply = serde_json::json!({"exit": 0, "stdout": "echo_session_action", "receipt_status": status});
        if let Some(error) = error {
            reply["receipt_error"] = error.into();
        }
        let fake = Fake::new(&serde_json::json!({ "reply": reply }).to_string());
        let t = target();
        let a = Action::ForgetBinding;
        let inv = invocation(&t, &a, "req-7").unwrap();
        let outcome = classify(&t, &a, &inv, run(&fake.hgs, &inv));
        assert_eq!(
            matches!(outcome, Outcome::Done { .. }),
            done,
            "{status}: {outcome:?}"
        );
        let log = fake.log();
        assert_eq!(log.len(), 1);
        assert_eq!(
            log[0]["argv"],
            serde_json::json!(["session-action", "claude/sample-project/review", "--json"])
        );
        let stdin: Value = serde_json::from_str(log[0]["stdin"].as_str().unwrap()).unwrap();
        assert_eq!(stdin["action"], "forget");
        assert_eq!(stdin["request_id"], "req-7");
    }
}

#[test]
fn start_fresh_and_forget_run_their_hgs_arguments() {
    for (action, argv) in [
        (
            Action::StartFresh,
            serde_json::json!([
                "resume",
                "claude/sample-project/review",
                "--fresh",
                "--expected-run-id",
                "run-1",
                "-d"
            ]),
        ),
        (
            Action::Forget,
            serde_json::json!(["kill", "claude/sample-project/review"]),
        ),
    ] {
        let fake = Fake::new(r#"{"reply":{"exit":0}}"#);
        let t = target();
        let inv = invocation(&t, &action, "r").unwrap();
        assert!(matches!(
            classify(&t, &action, &inv, run(&fake.hgs, &inv)),
            Outcome::Done { .. }
        ));
        assert_eq!(fake.log()[0]["argv"], argv);
    }
}

#[test]
fn an_archive_page_through_the_fake_is_checked_and_typed() {
    use zerus_client_core::reads::{archive_page, ReadError, ARCHIVE_IDENTITY_INVALID};
    use zerus_client_core::ComputerId;
    let id = "00000000-0000-4000-8000-000000000001";
    let page = |at: f64| {
        serde_json::json!({"version": 1, "from": 1000, "until": 2000, "query": "",
            "next_cursor": "after-1", "total": 1, "previous_cursor": "", "position": 0,
            "sessions": [{"name": "claude/sample-project/review", "state": "archived",
                "archive_id": id, "archived_at": at}]})
    };
    let config = |at: f64| {
        serde_json::json!({"reply": {"exit": 1}, "replies": [{"match": ["archives", "--json"], "exit": 0, "stdout": page(at)}]}).to_string()
    };
    let fake = Fake::new(&config(1500.0));
    let studio = ComputerId::from("studio");
    let got = archive_page(&fake.hgs, &studio, 1000.0, 2000.0, "", "").unwrap();
    assert_eq!(got.rows.len(), 1);
    assert_eq!(got.rows[0].archive_id.as_deref(), Some(id));
    assert_eq!(got.rows[0].archived_at, Some(1500.0));
    assert_eq!(
        (got.next_cursor.as_str(), got.total, got.position),
        ("after-1", 1, Some(0))
    );
    let log = fake.log();
    assert_eq!(
        log[0]["argv"],
        serde_json::json!(["@studio", "archives", "--json"])
    );
    let sent: Value = serde_json::from_str(log[0]["stdin"].as_str().unwrap()).unwrap();
    assert_eq!(
        sent,
        serde_json::json!({"from": 1000, "until": 2000, "limit": 50, "cursor": "", "query": ""})
    );
    let out = Fake::new(&config(2000.0));
    assert_eq!(
        archive_page(&out.hgs, &studio, 1000.0, 2000.0, "", ""),
        Err(ReadError::ArchiveInvalid {
            message: ARCHIVE_IDENTITY_INVALID.into()
        })
    );
}

#[test]
fn a_failed_archive_page_reads_as_qt_words_it() {
    use zerus_client_core::reads::archive_page;
    use zerus_client_core::ComputerId;
    let text = |reply: Value| {
        let fake = Fake::new(&serde_json::json!({ "reply": reply }).to_string());
        archive_page(&fake.hgs, &ComputerId::from("studio"), 1.0, 2.0, "", "")
            .unwrap_err()
            .to_string()
    };
    assert_eq!(
        text(serde_json::json!({"exit": 3, "stderr": "  no archive here \n"})),
        "no archive here"
    );
    assert_eq!(text(serde_json::json!({"exit": 4})), "exit code 4");
}

#[test]
fn a_cancelled_archive_page_ends_its_child_promptly() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use zerus_client_core::reads::{archive_page_cancellable, ReadError};
    use zerus_client_core::ComputerId;
    let config = serde_json::json!({"reply": {"exit": 0, "sleep": 30, "stdout": {}}}).to_string();
    let fake = Fake::new(&config);
    let cancel = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let result = std::thread::scope(|scope| {
        let job = scope.spawn(|| {
            archive_page_cancellable(
                &fake.hgs,
                &ComputerId::from("studio"),
                1.0,
                2.0,
                "",
                "",
                &cancel,
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        cancel.store(true, Ordering::Relaxed);
        job.join().unwrap()
    });
    assert_eq!(result, Err(ReadError::Superseded));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn restore_and_forget_archive_run_their_hgs_arguments_and_report_outcomes() {
    let restore = Action::Restore {
        archive_id: "a1".into(),
    };
    let forget = Action::ForgetArchive {
        archive_id: "a1".into(),
    };
    for (action, argv) in [
        (
            &restore,
            serde_json::json!([
                "resume",
                "claude/sample-project/review",
                "--archive",
                "a1",
                "-d"
            ]),
        ),
        (
            &forget,
            serde_json::json!(["kill", "claude/sample-project/review", "--archive", "a1"]),
        ),
    ] {
        let t = target();
        let fake = Fake::new(r#"{"reply":{"exit":0}}"#);
        let inv = invocation(&t, action, "r").unwrap();
        assert!(matches!(
            classify(&t, action, &inv, run(&fake.hgs, &inv)),
            Outcome::Done { .. }
        ));
        assert_eq!(fake.log()[0]["argv"], argv);

        let fake = Fake::new(r#"{"reply":{"exit":2,"stderr":"archive is gone\n"}}"#);
        assert_eq!(
            classify(&t, action, &inv, run(&fake.hgs, &inv)),
            Outcome::Failed {
                detail: "archive is gone".into()
            }
        );

        let fake = Fake::new(r#"{"reply":{"exit":0,"sleep":30}}"#);
        let mut slow = invocation(&t, action, "r").unwrap();
        slow.timeout = std::time::Duration::from_millis(500);
        assert_eq!(
            classify(&t, action, &slow, run(&fake.hgs, &slow)),
            Outcome::Failed {
                detail: "timed out".into()
            }
        );
    }
}
