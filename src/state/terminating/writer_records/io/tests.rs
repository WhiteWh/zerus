use super::*;
use crate::state::storage;
use serde_json::{json, Value};
use std::{
    fs, io,
    os::fd::OwnedFd,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

const ENTRY: &str = "state::terminating::writer_records::io::tests::private_probe";
const SESSION: &str = "11111111-1111-4111-8111-111111111111";
const RUN: &str = "22222222-2222-4222-8222-222222222222";
const REQUEST: &str = "33333333-3333-4333-8333-333333333333";
fn scope() -> Run {
    Run::new(SESSION, RUN).unwrap()
}
fn end() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
fn probe(case: &str, home: &Path) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            ENTRY,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("HGS_CONFIG_DIR", home.join("config"))
        .env("HGS_STATE_DIR", home.join("state"))
        .env("HGS_TEST_WRITER_IO", case);
    let (status, output, errors) = crate::platform::run_bounded(
        &mut command,
        [64 << 10; 2],
        Instant::now() + Duration::from_secs(10),
        &|| false,
    )
    .unwrap();
    assert!(
        status.success(),
        "{case}: {} {}",
        String::from_utf8_lossy(&output),
        String::from_utf8_lossy(&errors)
    );
    assert!(String::from_utf8_lossy(&output).contains("test result: ok. 1 passed; 0 failed;"));
}
fn case(name: &str) {
    let home = tempfile::tempdir().unwrap();
    probe(name, home.path());
}

#[test]
fn one_original_exchange_keeps_its_row_until_settlement_and_explicit_retirement() {
    case("echo");
}
#[test]
fn the_original_native_source_is_rechecked_before_any_input() {
    case("changed-source");
    case("changed-during-check");
    case("refused-source");
}
#[test]
fn explicit_empty_input_retains_the_original_row_and_native_source_checks() {
    case("empty");
    case("empty-source-change");
}
#[test]
fn a_guarded_refusal_cannot_offer_input_after_it_physically_drains() {
    case("withheld");
}
#[test]
fn an_exited_helper_with_a_held_original_output_pipe_keeps_the_native_run_lease() {
    case("held-output");
}
#[test]
fn cancellation_and_exit_do_not_release_a_blocked_original_input_writer() {
    case("held-input");
}
#[test]
fn drop_retirement_releases_io_but_never_erases_the_original_pending_row() {
    case("dropped");
}
#[test]
fn pre_spawn_refusal_returns_the_same_original_reservation_without_an_effect() {
    case("expired-admission");
    case("damaged-admission");
}
#[test]
fn expired_admission_is_reached_after_slow_original_setup() {
    case("expired-admission-slow-setup");
}

fn original(until: Instant) -> Reservation {
    let started = Instant::now();
    let _writer = storage::writer_until(until).unwrap().unwrap();
    reserve(
        &scope(),
        REQUEST,
        Kind::Input,
        &"a".repeat(64),
        &"b".repeat(64),
        until,
    )
    .unwrap_or_else(|error| {
        panic!(
            "original reservation setup: {error}; elapsed={:?}; remaining={:?}",
            started.elapsed(),
            until.saturating_duration_since(Instant::now())
        )
    })
}
fn admit(original: Reservation) -> IoAdmission {
    IoAdmission::reserve(original).unwrap_or_else(|(_, failure)| panic!("{failure:?}"))
}
fn command(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", script])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}
fn until(mut check: impl FnMut() -> bool) {
    let until = end();
    while !check() {
        assert!(
            Instant::now() < until,
            "original private fixture did not settle"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn finish(exchange: &mut IoExchange) -> Settled {
    let mut settled = None;
    until(|| {
        settled = exchange.poll();
        settled.is_some()
    });
    assert!(exchange.poll().is_none());
    settled.unwrap()
}
fn ledger_path() -> std::path::PathBuf {
    storage::root()
        .join("terminating-writers")
        .join(SESSION)
        .join(RUN)
        .join("pending.json")
}
fn replace_intent() {
    let mut ledger: Value = serde_json::from_slice(&fs::read(ledger_path()).unwrap()).unwrap();
    ledger["pending"][0]["intent_sha256"] = json!("c".repeat(64));
    fs::write(ledger_path(), serde_json::to_vec(&ledger).unwrap()).unwrap();
}
fn gate(case: &str) {
    probe(case, storage::root().parent().unwrap());
}

#[test]
#[ignore = "bounded private reexec; no real agent, installed scope or controller"]
fn private_probe() {
    let mut case = std::env::var("HGS_TEST_WRITER_IO").unwrap();
    let slow_setup = case == "expired-admission-slow-setup";
    if slow_setup {
        case = "expired-admission".into();
    }
    if matches!(case.as_str(), "gate-held" | "gate-free") {
        let store = Store::open(&scope(), end()).unwrap();
        let result = store.exclusive(Instant::now() + Duration::from_millis(80));
        assert_eq!(result.is_err(), case == "gate-held");
        return;
    }
    drop(storage::lock(None).unwrap());
    initialize(&scope(), end()).unwrap();
    let setup_deadline = end();
    if slow_setup {
        // Deterministic scheduling/storage delay within the original fixture
        // setup budget; admission expiry must not time this durable setup.
        std::thread::sleep(Duration::from_millis(75));
    }
    let mut reservation = original(setup_deadline);
    if case == "expired-admission" {
        // First construct the original, then shorten its lifetime. Starting a
        // 50 ms clock before reservation/fsync tested machine speed instead of
        // the intended expired-admission refusal. No production bound changes.
        reservation.until = Instant::now();
    }
    let deadline = reservation.until;
    let entry = reservation.entry.clone();
    let weak = Arc::downgrade(reservation.lease.as_ref().unwrap());
    if case.ends_with("admission") {
        if case != "expired-admission" {
            replace_intent();
        }
        let (mut returned, failure) = match IoAdmission::reserve(reservation) {
            Ok(_) => panic!("pre-spawn refusal was admitted"),
            Err(original) => original,
        };
        assert_eq!(returned.entry.request, REQUEST);
        assert_eq!(returned.entry, entry);
        assert_eq!(returned.until, deadline);
        assert_eq!(pending(&scope(), end()).unwrap().len(), 1);
        assert_eq!(weak.strong_count(), 1);
        if case == "expired-admission" {
            assert_eq!(failure, Failure::Expired);
            returned.retire(end()).unwrap();
        } else {
            assert_eq!(failure, Failure::SourceChanged);
            assert!(returned.retire(end()).is_err());
        }
        return;
    }
    match case.as_str() {
        "echo" => {
            let extra_loan = reservation.resources().unwrap();
            let admission = admit(reservation);
            assert_eq!(weak.strong_count(), 3);
            gate("gate-held");
            let child = command("exec /bin/cat").spawn().unwrap();
            let pid = child.id();
            let mut exchange = admission.attach(child);
            let mut checks = 0;
            exchange
                .send(b"original input".to_vec(), |original, until| {
                    assert_eq!(original.id(), pid);
                    assert_eq!(until, deadline);
                    checks += 1;
                    true
                })
                .unwrap();
            assert_eq!(
                exchange.send(b"replay".to_vec(), |_, _| panic!("second check")),
                Err(Failure::Input)
            );
            let mut settled = finish(&mut exchange);
            assert_eq!(checks, 1);
            assert!(settled.report().exit.success() && settled.report().input_written);
            assert_eq!(settled.report().stdout, b"original input");
            assert!(settled.report().stderr.is_empty() && settled.report().failure.is_none());
            assert_eq!(pending(&scope(), end()).unwrap().len(), 1);
            assert!(
                settled.retire(end()).is_err(),
                "an extra real source loan still owns the lease"
            );
            drop(extra_loan);
            gate("gate-held");
            settled.retire(end()).unwrap();
            assert!(pending(&scope(), end()).unwrap().is_empty());
            assert_eq!(weak.strong_count(), 0);
            gate("gate-free");
        }
        "empty" | "empty-source-change" => {
            let admission = admit(reservation);
            let child = command("exec /bin/cat").spawn().unwrap();
            let pid = child.id();
            let mut exchange = admission.attach(child);
            let result = exchange.close_empty_input(|original, until| {
                assert_eq!(original.id(), pid);
                assert_eq!(until, deadline);
                if case == "empty-source-change" {
                    replace_intent();
                }
                true
            });
            assert_eq!(result.is_ok(), case == "empty");
            let mut settled = finish(&mut exchange);
            assert!(settled.report().stdout.is_empty());
            assert_eq!(settled.report().input_written, case == "empty");
            if case == "empty" {
                assert!(settled.report().exit.success() && settled.report().failure.is_none());
                settled.retire(end()).unwrap();
            } else {
                assert_eq!(settled.report().failure, Some(Failure::SourceChanged));
                assert!(settled.retire(end()).is_err());
            }
        }
        "changed-source" | "changed-during-check" | "refused-source" | "withheld" => {
            let admission = admit(reservation);
            let child = command("exec /bin/cat").spawn().unwrap();
            let mut exchange = if case == "withheld" {
                admission.attach_withheld(child)
            } else {
                admission.attach(child)
            };
            if case == "changed-source" {
                replace_intent();
            }
            let mut checks = 0;
            assert_eq!(
                exchange.send(b"private withheld".to_vec(), |_, _| {
                    checks += 1;
                    if case == "changed-during-check" {
                        replace_intent();
                        true
                    } else {
                        false
                    }
                }),
                Err(Failure::SourceChanged)
            );
            assert_eq!(
                checks,
                usize::from(matches!(
                    case.as_str(),
                    "refused-source" | "changed-during-check"
                ))
            );
            assert_eq!(
                exchange.send(b"replay".to_vec(), |_, _| panic!("refusal replay")),
                Err(Failure::SourceChanged)
            );
            let mut settled = finish(&mut exchange);
            assert!(!settled.report().input_written && settled.report().stdout.is_empty());
            assert_eq!(settled.report().failure, Some(Failure::SourceChanged));
            if matches!(case.as_str(), "changed-source" | "changed-during-check") {
                assert!(settled.retire(end()).is_err());
                assert_eq!(pending(&scope(), end()).unwrap().len(), 1);
            } else {
                settled.retire(end()).unwrap();
            }
        }
        "held-output" | "dropped" => {
            let admission = admit(reservation);
            let (read, held_write) = io::pipe().unwrap();
            let mut command = command("read line");
            command.stdout(Stdio::from(held_write.try_clone().unwrap()));
            let mut child = command.spawn().unwrap();
            let pid = child.id();
            let birth = crate::platform::process_identity(pid).unwrap().birth;
            child.stdout = Some(OwnedFd::from(read).into());
            drop(command);
            let mut exchange = admission.attach(child);
            exchange
                .send(b"finish\n".to_vec(), |_, until| until == deadline)
                .unwrap();
            until(|| {
                assert!(exchange.poll().is_none());
                crate::platform::process_identity(pid).is_none_or(|now| now.birth != birth)
            });
            gate("gate-held");
            assert_eq!(pending(&scope(), end()).unwrap().len(), 1);
            if case == "held-output" {
                drop(held_write);
                let mut settled = finish(&mut exchange);
                assert!(settled.report().exit.success());
                assert_eq!(pending(&scope(), end()).unwrap().len(), 1);
                settled.retire(end()).unwrap();
                assert!(pending(&scope(), end()).unwrap().is_empty());
            } else {
                drop(exchange);
                assert!(
                    weak.strong_count() > 0,
                    "original I/O retains the native lease"
                );
                gate("gate-held");
                drop(held_write);
                until(|| weak.strong_count() == 0);
                gate("gate-free");
                assert_eq!(pending(&scope(), end()).unwrap().len(), 1);
                assert!(super::super::gate::RunWriterGate::acquire(&scope(), end()).is_err());
            }
        }
        "held-input" => {
            let admission = admit(reservation);
            let (held_read, write) = io::pipe().unwrap();
            let mut command = command("exec /bin/sleep 30");
            command.stdin(Stdio::from(held_read.try_clone().unwrap()));
            let mut child = command.spawn().unwrap();
            let pid = child.id();
            let birth = crate::platform::process_identity(pid).unwrap().birth;
            child.stdin = Some(OwnedFd::from(write).into());
            drop(command);
            let mut exchange = admission.attach(child);
            exchange
                .send(vec![b'x'; 1 << 20], |_, until| until == deadline)
                .unwrap();
            exchange.cancel();
            until(|| {
                assert!(exchange.poll().is_none());
                crate::platform::process_identity(pid).is_none_or(|now| now.birth != birth)
            });
            assert_eq!(exchange.failure(), Some(Failure::Cancelled));
            gate("gate-held");
            assert_eq!(pending(&scope(), end()).unwrap().len(), 1);
            drop(held_read);
            let mut settled = finish(&mut exchange);
            assert_eq!(settled.report().failure, Some(Failure::Cancelled));
            assert!(!settled.report().input_written);
            settled.retire(end()).unwrap();
            assert!(pending(&scope(), end()).unwrap().is_empty());
        }
        _ => panic!("unknown original writer I/O fixture"),
    }
}
