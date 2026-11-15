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

#[test]
fn first_end_of_an_interrupted_codex_names_its_holder_and_accepts_after_release() {
    let actions = actions();
    let delays = Delays::default();
    let row: zerus_client_core::ViewRow = serde_json::from_value(serde_json::json!({
        "name":"codex/example/interrupted", "agent":"codex", "state":"running",
        "phase":"interrupted", "pid":null, "run_id":"after-clear-run",
        "conversation_id":"after-clear-conversation", "machine":"example-machine",
        "tracked":true, "git_worktree":true
    }))
    .unwrap();
    let c = Captured {
        target: Target {
            computer: String::new(),
            name: row.name.clone(),
            run_id: row.run_id.clone().unwrap(),
            conversation_id: row.conversation_id.clone().unwrap(),
        },
        action: Action::Terminate,
        state: row.state.clone(),
    };
    assert!(c.valid());
    let clearing = Action::ClearContext {
        dismiss_prompt: false,
    };
    let held = actions
        .begin(Lane::key(&c.target, &clearing), &clearing)
        .unwrap();
    let refused = delays.begin(&actions, id(), vec![c.clone()]).unwrap_err();
    assert!(
        refused.contains("Clear session"),
        "the refusal must name its actual holder: {refused}"
    );
    assert!(
        refused.contains("started_utc"),
        "the refusal must identify when that holder started: {refused}"
    );
    drop(held);
    let end = id();
    delays
        .begin(&actions, end.clone(), vec![c.clone()])
        .unwrap();
    due(&delays, &end);
    assert!(delays
        .take(&Reference { id: end, index: 0 }, &c.target, &c.action)
        .is_ok());
}
fn due(delays: &Delays, id: &str) {
    lock(&delays.0).groups.get_mut(id).unwrap().window = Window::new(Instant::now() - UNDO_WINDOW);
    assert!(delays.wait(id));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn explicit_remove_undo_keeps_the_same_original_claim_without_a_second_activation() {
    let actions = actions();
    let delays = Delays::default();
    let mut capture = captured("programs");
    capture.action = Action::Terminate;
    capture.state = "running".into();
    let running = Arc::new(
        actions
            .begin(Lane::key(&capture.target, &capture.action), &capture.action)
            .unwrap(),
    );
    assert!(delays.begin(&actions, id(), vec![capture.clone()]).is_err());
    let original = running.claim();
    let group = id();
    delays
        .begin_with(
            &actions,
            group.clone(),
            vec![capture.clone()],
            Some(original.clone()),
        )
        .unwrap();
    due(&delays, &group);
    let (_, delayed) = delays
        .take(
            &Reference {
                id: group,
                index: 0,
            },
            &capture.target,
            &capture.action,
        )
        .unwrap();
    assert!(Arc::ptr_eq(&original, &delayed));
    assert!(actions
        .begin(Lane::key(&capture.target, &capture.action), &capture.action)
        .is_none());
    drop(delayed);
    drop(original);
    assert!(
        actions
            .begin(Lane::key(&capture.target, &capture.action), &capture.action)
            .is_none(),
        "delay settlement does not retire original Source custody"
    );
    drop(running);
    assert!(actions
        .begin(Lane::key(&capture.target, &capture.action), &capture.action)
        .is_some());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn explicit_remove_delay_cannot_borrow_another_target_or_actions_claim() {
    let actions = actions();
    let other_actions = self::actions();
    let delays = Delays::default();
    let mut capture = captured("programs");
    capture.action = Action::Terminate;
    capture.state = "running".into();
    let running = Arc::new(
        actions
            .begin(Lane::key(&capture.target, &capture.action), &capture.action)
            .unwrap(),
    );
    let mut changed = capture.clone();
    changed.target.name.push_str("-other");
    assert!(delays
        .begin_with(&actions, id(), vec![changed], Some(running.claim()))
        .is_err());
    assert!(delays
        .begin_with(
            &other_actions,
            id(),
            vec![capture.clone()],
            Some(running.claim())
        )
        .is_err());
    capture.action = Action::Archive;
    assert!(delays
        .begin_with(&actions, id(), vec![capture], Some(running.claim()))
        .is_err());
}

#[test]
fn an_abandoned_undo_window_releases_the_session_lane_for_the_next_end() {
    // The frontend can lose its pending reference (a reloaded or crashed
    // webview): the group then never takes or cancels, and without an expiry
    // its claim blocks every later end for this session until the app exits.
    let actions = actions();
    let delays = Delays::default();
    let c = captured("one");
    let first = id();
    delays
        .begin(&actions, first.clone(), vec![c.clone()])
        .unwrap();
    due(&delays, &first);
    lock(&delays.0).groups.get_mut(&first).unwrap().created =
        Instant::now() - UNDO_WINDOW - STALE_GRACE - Duration::from_millis(1);
    let second = id();
    delays
        .begin(&actions, second.clone(), vec![c.clone()])
        .expect("an abandoned undo window must not hold the session lane");
    assert!(
        !lock(&delays.0).groups.contains_key(&first),
        "the abandoned window is discarded"
    );
    assert!(
        actions
            .begin(Lane::key(&c.target, &Action::Pause), &Action::Pause)
            .is_none(),
        "the new end still holds the lane"
    );
    delays.cancel(&second);
    assert!(actions
        .begin(Lane::key(&c.target, &Action::Pause), &Action::Pause)
        .is_some());
}

#[test]
fn reclaiming_one_abandoned_bulk_member_preserves_the_other_sessions_and_indices() {
    let actions = actions();
    let delays = Delays::default();
    let a = captured("one");
    let b = captured("two");
    let mut peer = a.clone();
    peer.target.computer = "another-example-peer".into();
    let original = id();
    delays
        .begin(
            &actions,
            original.clone(),
            vec![a.clone(), b.clone(), peer.clone()],
        )
        .unwrap();
    due(&delays, &original);
    lock(&delays.0).groups.get_mut(&original).unwrap().created =
        Instant::now() - UNDO_WINDOW - STALE_GRACE - Duration::from_millis(1);
    let replacement = id();
    delays
        .begin(&actions, replacement.clone(), vec![a.clone()])
        .unwrap();
    let reference = |index| Reference {
        id: original.clone(),
        index,
    };
    assert_eq!(
        delays
            .take(&reference(0), &a.target, &a.action)
            .err()
            .as_deref(),
        Some(STATE_CHANGED),
        "the replaced member cannot replay"
    );
    for (index, c) in [(1, &b), (2, &peer)] {
        assert!(
            actions.reserve_end_for(&c.target, &c.action).is_err(),
            "the other member retains its lane"
        );
        let (kept, claim) = delays.take(&reference(index), &c.target, &c.action)
            .expect("ending one session again must keep other bulk members pending at their original indices");
        assert_eq!(&kept, c);
        assert!(delays
            .take(&reference(index), &c.target, &c.action)
            .is_err());
        drop(claim);
    }
    assert!(!lock(&delays.0).groups.contains_key(&original));
    delays.cancel(&replacement);
    assert_eq!(lock(&delays.0).bytes, 0);
    for c in [&a, &b, &peer] {
        assert!(actions.reserve_end_for(&c.target, &c.action).is_ok());
    }
    assert!(
        delays.begin(&actions, original, vec![a]).is_err(),
        "the original UUID remains one-use"
    );
}

#[test]
fn reclaiming_an_abandoned_bulk_claim_never_releases_a_taken_call() {
    let actions = actions();
    let delays = Delays::default();
    let a = captured("one");
    let b = captured("two");
    let original = id();
    delays
        .begin(&actions, original.clone(), vec![a.clone(), b.clone()])
        .unwrap();
    due(&delays, &original);
    let (_, active) = delays
        .take(
            &Reference {
                id: original.clone(),
                index: 0,
            },
            &a.target,
            &a.action,
        )
        .unwrap();
    lock(&delays.0).groups.get_mut(&original).unwrap().created =
        Instant::now() - UNDO_WINDOW - STALE_GRACE - Duration::from_millis(1);
    assert!(
        delays.begin(&actions, id(), vec![a.clone()]).is_err(),
        "the taken call still owns its exact claim"
    );
    drop(active);
    let replacement = id();
    delays
        .begin(&actions, replacement.clone(), vec![a.clone()])
        .expect("a finished member must not stay locked behind another session's untaken member");
    assert!(delays
        .take(
            &Reference {
                id: original.clone(),
                index: 0
            },
            &a.target,
            &a.action
        )
        .is_err());
    let (kept, claim) = delays
        .take(
            &Reference {
                id: original,
                index: 1,
            },
            &b.target,
            &b.action,
        )
        .expect("an active first call must not discard the other session's pending action");
    assert_eq!(kept, b);
    drop(claim);
    delays.cancel(&replacement);
    assert_eq!(lock(&delays.0).bytes, 0);
}

#[test]
fn reclaiming_abandoned_same_name_archives_keeps_other_session_members() {
    let actions = actions();
    let delays = Delays::default();
    let mut a = captured("one");
    a.state = "archived".into();
    a.action = Action::ForgetArchive { archive_id: id() };
    let mut older = a.clone();
    older.target.run_id = "older-run".into();
    older.action = Action::ForgetArchive { archive_id: id() };
    let b = captured("two");
    let original = id();
    delays
        .begin(
            &actions,
            original.clone(),
            vec![a.clone(), older.clone(), b.clone()],
        )
        .unwrap();
    due(&delays, &original);
    lock(&delays.0).groups.get_mut(&original).unwrap().created =
        Instant::now() - UNDO_WINDOW - STALE_GRACE - Duration::from_millis(1);
    let replacement = id();
    delays
        .begin(&actions, replacement.clone(), vec![a.clone()])
        .unwrap();
    for (index, c) in [(0, &a), (1, &older)] {
        assert!(
            delays
                .take(
                    &Reference {
                        id: original.clone(),
                        index
                    },
                    &c.target,
                    &c.action
                )
                .is_err(),
            "every untaken archive sharing the replaced claim is retired without shifting indices"
        );
    }
    let (kept, claim) = delays
        .take(
            &Reference {
                id: original,
                index: 2,
            },
            &b.target,
            &b.action,
        )
        .expect("same-name archive claims must not discard another session");
    assert_eq!(kept, b);
    drop(claim);
    delays.cancel(&replacement);
    assert_eq!(lock(&delays.0).bytes, 0);
}

#[test]
fn a_refused_end_names_the_running_lane_instead_of_bare_busy() {
    let actions = actions();
    let delays = Delays::default();
    let c = captured("one");
    let held = actions
        .begin(Lane::key(&c.target, &Action::Pause), &Action::Pause)
        .unwrap();
    let error = delays.begin(&actions, id(), vec![c.clone()]).unwrap_err();
    assert_ne!(error, "busy");
    assert!(error.contains("Pause session"), "{error}");
    drop(held);
    delays.begin(&actions, id(), vec![c]).unwrap();
}

#[test]
fn an_already_submitted_end_says_so() {
    let actions = actions();
    let delays = Delays::default();
    let c = captured("one");
    let id = id();
    delays.begin(&actions, id.clone(), vec![c.clone()]).unwrap();
    let error = delays.begin(&actions, id, vec![c]).unwrap_err();
    assert_ne!(error, "busy");
    assert!(error.contains("already submitted"), "{error}");
}

#[test]
fn a_closed_delay_list_tells_the_person_to_restart() {
    let actions = actions();
    let delays = Delays::default();
    delays.cancel_all(true);
    let error = delays
        .begin(&actions, id(), vec![captured("one")])
        .unwrap_err();
    assert_ne!(error, "busy");
    assert!(error.contains("Restart the app"), "{error}");
}

#[test]
fn an_exhausted_pending_id_list_tells_the_person_to_restart() {
    let actions = actions();
    let delays = Delays::default();
    {
        let mut model = lock(&delays.0);
        for n in 0..REGISTRATIONS {
            model
                .seen
                .insert(format!("{n:08}-0000-0000-0000-000000000000"));
        }
    }
    let error = delays
        .begin(&actions, id(), vec![captured("one")])
        .unwrap_err();
    assert_ne!(error, "busy");
    assert!(error.contains("Restart the app"), "{error}");
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
        queue_when_unconfirmed: false,
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
