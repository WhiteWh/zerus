use super::*;
use crate::actions::Lane;
use std::time::Duration;
use zerus_client_core::end_action::UNDO_WINDOW;

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn actions() -> Actions {
    let root = std::env::temp_dir().join(format!("zerus-undo-marks-{}", id()));
    std::fs::create_dir(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    Actions::new(root.join("read-marks.json"))
}
fn captured(tag: &str) -> Captured {
    Captured {
        target: Target {
            computer: "example-peer".into(),
            name: format!("codex/example/{tag}"),
            run_id: format!("run-{tag}"),
            conversation_id: format!("conversation-{tag}"),
        },
        action: Action::Archive,
        state: "paused".into(),
    }
}
fn due(delays: &Delays, id: &str) {
    lock(&delays.0).groups.get_mut(id).unwrap().window = Window::new(Instant::now() - UNDO_WINDOW);
    assert!(delays.wait(id));
}

#[test]
fn pending_reserves_the_existing_write_lane_without_blocking_messages() {
    let actions = actions();
    let delays = Delays::default();
    let c = captured("one");
    let id = id();
    delays.begin(&actions, id.clone(), vec![c.clone()]).unwrap();
    assert!(actions
        .begin(Lane::key(&c.target, &Action::Pause), &Action::Pause)
        .is_none());
    assert_eq!(actions.write_kind(&c.target.computer, &c.target.name), None);
    let send = Action::Send {
        text: "still allowed".into(),
        attachments: Vec::new(),
        agent_id: None,
        expected_compaction_id: None,
    };
    assert!(actions.begin(Lane::key(&c.target, &send), &send).is_some());
    assert!(delays
        .take(
            &Reference {
                id: id.clone(),
                index: 0
            },
            &c.target,
            &c.action
        )
        .is_err());
    delays.cancel(&id);
    assert!(actions
        .begin(Lane::key(&c.target, &Action::Pause), &Action::Pause)
        .is_some());
}

#[test]
fn partial_batch_reservation_is_released_and_invalid_input_reads_no_source() {
    let actions = actions();
    let delays = Delays::default();
    let a = captured("one");
    let b = captured("two");
    let _busy = actions
        .begin(Lane::key(&b.target, &Action::Pause), &Action::Pause)
        .unwrap();
    assert!(delays.begin(&actions, id(), vec![a.clone(), b]).is_err());
    assert!(actions
        .begin(Lane::key(&a.target, &Action::Pause), &Action::Pause)
        .is_some());
    assert!(lock(&delays.0).groups.is_empty());
    let mut bad = a.clone();
    bad.target.run_id.clear();
    assert!(delays.begin(&actions, id(), vec![bad]).is_err());
    assert!(delays
        .begin(&actions, "not-a-uuid".into(), vec![a.clone()])
        .is_err());
    assert!(delays.begin(&actions, id(), Vec::new()).is_err());
    assert!(delays.begin(&actions, id(), vec![a; TARGETS + 1]).is_err());
}

#[test]
fn undo_before_async_registration_still_prevents_the_late_window() {
    let actions = actions();
    let delays = Delays::default();
    let id = id();
    delays.cancel(&id);
    assert!(delays.begin(&actions, id, vec![captured("one")]).is_err());
}

#[test]
fn close_and_quit_cancel_waiters_and_never_flush_them_into_actions() {
    let actions = actions();
    let delays = Delays::default();
    let c = captured("one");
    let id = id();
    delays.begin(&actions, id.clone(), vec![c.clone()]).unwrap();
    let worker = delays.clone();
    let pending = id.clone();
    let wait = std::thread::spawn(move || worker.wait(&pending));
    delays.cancel_all(false);
    assert!(!wait.join().unwrap());
    assert!(actions
        .begin(Lane::key(&c.target, &Action::Pause), &Action::Pause)
        .is_some());
    assert!(
        delays.begin(&actions, id.clone(), vec![c.clone()]).is_err(),
        "an old waiter cannot bind to a reused UUID"
    );
    let id = uuid::Uuid::new_v4().to_string();
    delays.begin(&actions, id.clone(), vec![c.clone()]).unwrap();
    due(&delays, &id);
    delays.cancel_all(true);
    assert!(delays
        .take(
            &Reference {
                id: id.clone(),
                index: 0
            },
            &c.target,
            &c.action
        )
        .is_err());
    assert!(
        delays.begin(&actions, id, vec![c]).is_err(),
        "exit flush cannot admit another pending action"
    );
}

#[test]
fn reservation_is_exact_and_one_use_after_the_original_deadline() {
    let actions = actions();
    let delays = Delays::default();
    let c = captured("one");
    let id = id();
    delays.begin(&actions, id.clone(), vec![c.clone()]).unwrap();
    due(&delays, &id);
    let reference = Reference {
        id: id.clone(),
        index: 0,
    };
    assert!(delays
        .take(
            &Reference {
                id: id.clone(),
                index: 1
            },
            &c.target,
            &c.action
        )
        .is_err());
    assert!(delays
        .take(&reference, &captured("replacement").target, &c.action)
        .is_err());
    assert!(delays
        .take(&reference, &c.target, &Action::Terminate)
        .is_err());
    let (source, claim) = delays.take(&reference, &c.target, &c.action).unwrap();
    assert_eq!(source, c);
    assert!(delays.take(&reference, &c.target, &c.action).is_err());
    assert!(actions
        .begin(Lane::key(&c.target, &Action::Pause), &Action::Pause)
        .is_none());
    drop(claim);
    assert!(actions
        .begin(Lane::key(&c.target, &Action::Pause), &Action::Pause)
        .is_some());
}

#[test]
fn same_name_archives_share_one_lane_until_every_batch_member_finishes() {
    let actions = actions();
    let delays = Delays::default();
    let id = id();
    let mut a = captured("one");
    a.state = "archived".into();
    a.action = Action::ForgetArchive {
        archive_id: uuid::Uuid::new_v4().to_string(),
    };
    let mut b = a.clone();
    b.target.run_id = "older-run".into();
    b.action = Action::ForgetArchive {
        archive_id: uuid::Uuid::new_v4().to_string(),
    };
    delays
        .begin(&actions, id.clone(), vec![a.clone(), b.clone()])
        .unwrap();
    due(&delays, &id);
    let (_, first) = delays
        .take(
            &Reference {
                id: id.clone(),
                index: 0,
            },
            &a.target,
            &a.action,
        )
        .unwrap();
    drop(first);
    assert!(actions
        .begin(Lane::key(&a.target, &Action::Pause), &Action::Pause)
        .is_none());
    let (_, last) = delays
        .take(&Reference { id, index: 1 }, &b.target, &b.action)
        .unwrap();
    drop(last);
    assert!(actions
        .begin(Lane::key(&a.target, &Action::Pause), &Action::Pause)
        .is_some());
}

#[test]
fn native_wait_never_releases_early_or_restarts_when_the_webview_is_hidden() {
    let actions = actions();
    let delays = Delays::default();
    let c = captured("one");
    let id = id();
    let start = Instant::now();
    delays.begin(&actions, id.clone(), vec![c]).unwrap();
    // No webview timer or visibility state participates in this wait.
    assert!(delays.wait(&id));
    assert!(start.elapsed() >= UNDO_WINDOW);
    assert!(start.elapsed() < Duration::from_secs(15));
    assert!(!delays.wait(&id));
}

#[test]
fn groups_are_bounded_and_poisoned_accounting_refuses_all_pending_work() {
    let actions = actions();
    let delays = Delays::default();
    for n in 0..WINDOWS {
        delays
            .begin(&actions, id(), vec![captured(&n.to_string())])
            .unwrap();
    }
    let sixth = id();
    let extra = captured("extra");
    let before = lock(&delays.0).bytes;
    assert_eq!(
        delays
            .begin(&actions, sixth.clone(), vec![extra.clone()])
            .unwrap_err(),
        FIVE_WAITING
    );
    assert!(!lock(&delays.0).seen.contains(&sixth));
    assert_eq!(lock(&delays.0).bytes, before);
    assert!(actions
        .begin(Lane::key(&extra.target, &Action::Pause), &Action::Pause)
        .is_some());
    let poisoned = delays.clone();
    assert!(std::thread::spawn(move || {
        let _guard = poisoned.0.model.lock().unwrap();
        panic!("synthetic accounting failure");
    })
    .join()
    .is_err());
    assert!(delays
        .begin(&actions, id(), vec![captured("later")])
        .is_err());
    for n in 0..WINDOWS {
        let c = captured(&n.to_string());
        assert!(actions
            .begin(Lane::key(&c.target, &Action::Pause), &Action::Pause)
            .is_some());
    }
}

fn many_captures(count: usize) -> Vec<Captured> {
    (0..count)
        .map(|n| Captured {
            target: Target {
                computer: "example-peer".into(),
                name: format!("codex/example-project/review-{n:05}"),
                run_id: format!("00000000-0000-4000-8000-{n:012}"),
                conversation_id: format!("10000000-0000-4000-8000-{n:012}"),
            },
            action: Action::Archive,
            state: "paused".into(),
        })
        .collect()
}
fn memory_kib(field: &str) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // This qualification reads only its own process, never another
        // session's memory, configuration or application state.
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        status.lines().find_map(|line| {
            line.strip_prefix(field)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = field;
        None
    }
}

#[test]
fn batch_bounds_measure_1024_and_50000_and_reject_50001_before_reservation() {
    for count in [1024, 50_000, 50_001] {
        let actions = actions();
        let delays = Delays::default();
        let id = id();
        let before = memory_kib("VmRSS:");
        let captures = many_captures(count);
        let bytes = capture_bytes(&captures, usize::MAX).unwrap();
        let after_input = memory_kib("VmRSS:");
        let start = Instant::now();
        let result = delays.begin(&actions, id.clone(), captures);
        println!(
            "undo-registration {}",
            serde_json::json!({"targets":count,"captured_json_bytes":bytes,"registration_us":start.elapsed().as_micros(),
            "rss_before_kib":before,"rss_input_kib":after_input,"rss_registered_kib":memory_kib("VmRSS:"),"peak_kib":memory_kib("VmHWM:")})
        );
        if count > TARGETS {
            assert_eq!(
                result.unwrap_err(),
                "Too many sessions selected at once (50001). Select at most 50,000."
            );
            assert!(lock(&delays.0).groups.is_empty());
            assert_eq!(lock(&delays.0).bytes, 0);
            let c = many_captures(1).pop().unwrap();
            assert!(
                actions
                    .begin(Lane::key(&c.target, &Action::Pause), &Action::Pause)
                    .is_some(),
                "an oversized registration must not reserve a prefix"
            );
        } else {
            result.unwrap();
            assert_eq!(lock(&delays.0).groups[&id].members.len(), count);
            assert_eq!(lock(&delays.0).bytes, bytes);
            let line = quit_line(&lock(&delays.0).groups[&id]).unwrap();
            assert!(!line.contains('\n'));
            assert!(line.len() < 3000);
            assert!(line.ends_with(&format!("and {} more", count - 5)));
            assert_eq!(line.matches('"').count(), 10, "only five quoted names");
            delays.cancel(&id);
            assert_eq!(lock(&delays.0).bytes, 0);
        }
    }
}

fn at_byte_edge() -> Vec<Captured> {
    let mut captures = many_captures(TARGETS);
    let mut remaining = CAPTURE_BYTES
        .checked_sub(capture_bytes(&captures, usize::MAX).unwrap())
        .unwrap();
    for capture in &mut captures {
        let add = remaining.min(512 - capture.target.computer.len());
        capture.target.computer.push_str(&"x".repeat(add));
        remaining -= add;
        if remaining == 0 {
            break;
        }
    }
    assert_eq!(remaining, 0);
    assert_eq!(capture_bytes(&captures, usize::MAX).unwrap(), CAPTURE_BYTES);
    captures
}

#[test]
fn exact_aggregate_byte_budget_and_one_byte_over_are_atomic() {
    let actions = actions();
    let delays = Delays::default();
    let full = id();
    let edge = at_byte_edge();
    let before = memory_kib("VmRSS:");
    let start = Instant::now();
    delays.begin(&actions, full.clone(), edge).unwrap();
    println!(
        "undo-byte-edge {}",
        serde_json::json!({"captured_json_bytes":CAPTURE_BYTES,"targets":TARGETS,"registration_us":start.elapsed().as_micros(),
        "rss_input_kib":before,"rss_registered_kib":memory_kib("VmRSS:"),"peak_kib":memory_kib("VmHWM:")})
    );
    assert_eq!(lock(&delays.0).bytes, CAPTURE_BYTES);
    let extra = captured("extra");
    assert_eq!(
        delays
            .begin(&actions, id(), vec![extra.clone()])
            .unwrap_err(),
        TOO_MUCH_DATA
    );
    assert!(actions
        .begin(Lane::key(&extra.target, &Action::Pause), &Action::Pause)
        .is_some());
    assert_eq!(lock(&delays.0).groups.len(), 1);
    delays.cancel(&full);
    assert_eq!(lock(&delays.0).bytes, 0);
    let mut over = at_byte_edge();
    over.iter_mut()
        .find(|c| c.target.computer.len() < 512)
        .unwrap()
        .target
        .computer
        .push('x');
    let first = over[0].target.clone();
    assert_eq!(capture_bytes(&over, usize::MAX).unwrap(), CAPTURE_BYTES + 1);
    assert_eq!(
        delays.begin(&actions, id(), over).unwrap_err(),
        TOO_MUCH_DATA
    );
    assert!(lock(&delays.0).groups.is_empty());
    assert_eq!(lock(&delays.0).bytes, 0);
    assert!(actions
        .begin(Lane::key(&first, &Action::Pause), &Action::Pause)
        .is_some());
    delays.begin(&actions, id(), vec![extra]).unwrap();
}

#[test]
fn settled_tombstones_roll_over_fifo_without_evicting_active_windows() {
    let actions = actions();
    let delays = Delays::default();
    let active = id();
    let source = captured("active");
    delays
        .begin(&actions, active.clone(), vec![source.clone()])
        .unwrap();
    let oldest = id();
    delays.cancel(&oldest);
    for _ in 0..REGISTRATIONS {
        delays.cancel(&id());
    }
    let model = lock(&delays.0);
    assert!(model.groups.contains_key(&active));
    assert!(model.seen.contains(&active));
    assert!(!model.seen.contains(&oldest));
    assert!(model.seen.len() <= REGISTRATIONS);
    assert!(model.settled.len() < REGISTRATIONS);
    drop(model);
    delays
        .begin(&actions, id(), vec![captured("later")])
        .unwrap();
    assert!(actions
        .begin(Lane::key(&source.target, &Action::Pause), &Action::Pause)
        .is_none());
}
