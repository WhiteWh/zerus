use super::*;
use std::time::Instant;

fn endpoint(sender: &Sender, name: &str) -> Endpoint {
    Endpoint {
        sender: sender.clone(),
        identity: Identity {
            computer: name.into(),
            incarnation: 1,
        },
        resnapshot: Arc::default(),
        cancel: None,
        _worker: None,
    }
}

#[test]
fn legacy_ingress_pressure_obeys_its_replay_deadline() {
    let (sender, _receiver) = channel(Command::spawn);
    let _full = sender.reserve(DATA_BYTES, &AtomicBool::new(false)).unwrap();
    let began = Instant::now();
    assert!(!endpoint(&sender, "").legacy(
        1,
        Legacy::Complete,
        began,
        &AtomicBool::new(false),
        began + Duration::from_millis(100)
    ));
    assert!(began.elapsed() < Duration::from_secs(1));
    assert_eq!(sender.usage().bytes, DATA_BYTES);
}

#[test]
fn resnapshot_is_one_transient_request_for_an_exact_greeted_generation() {
    let reset = Resnapshot::default();
    assert!(!reset.request(0));
    assert!(!reset.request(1));
    reset.open(1);
    assert!(!reset.request(2));
    assert!(reset.request(1));
    assert!(!reset.request(1));
    assert!(!reset.take(2));
    assert!(reset.take(1));
    assert!(!reset.request(1));
    reset.open(2);
    assert!(!reset.request(1));
    assert!(reset.request(2));
    reset.close();
    assert!(!reset.take(2));
    reset.open(3);
    assert!(!reset.take(3));
}

#[test]
fn byte_budget_is_shared_and_cancellation_releases_waiters() {
    let (sender, _receiver) = channel(Command::spawn);
    let first = sender.reserve(DATA_BYTES, &AtomicBool::new(false)).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let task = {
        let sender = sender.clone();
        let stop = stop.clone();
        std::thread::spawn(move || sender.reserve(1, &stop).is_none())
    };
    stop.store(true, Ordering::Relaxed);
    assert!(task.join().unwrap());
    assert_eq!(sender.usage().bytes, DATA_BYTES);
    drop(first);
    assert_eq!(sender.usage().bytes, 0);
}

#[test]
fn lifecycle_status_has_headroom_when_all_data_is_in_input_queues() {
    let (sender, receiver) = channel(Command::spawn);
    let data = sender.reserve(DATA_BYTES, &AtomicBool::new(false)).unwrap();
    for n in 0..SOURCES {
        assert!(endpoint(&sender, &format!("peer-{n}")).status(
            1,
            ComputerStatus::Reconnecting { attempt: 1 },
            &AtomicBool::new(false)
        ));
    }
    assert_eq!(sender.usage().bytes, INGRESS_BYTES);
    for _ in 0..SOURCES {
        drop(receiver.recv_timeout(Duration::from_secs(1)).unwrap());
    }
    drop(data);
    assert_eq!(sender.usage().bytes, 0);
}

#[test]
fn tiny_items_are_bounded_and_receiver_close_wakes_a_full_channel() {
    let (sender, receiver) = channel(Command::spawn);
    let permits: Vec<_> = (0..DATA_ITEMS)
        .map(|_| sender.reserve(0, &AtomicBool::new(false)).unwrap())
        .collect();
    let other = sender.clone();
    let task = std::thread::spawn(move || other.reserve(1, &AtomicBool::new(false)).is_none());
    drop(receiver);
    assert!(task.join().unwrap());
    assert_eq!(sender.usage().items, DATA_ITEMS);
    drop(permits);
    assert_eq!(sender.usage().items, 0);
}

#[test]
fn consumer_owned_items_still_count_until_application_finishes() {
    let (sender, receiver) = channel(Command::spawn);
    assert!(endpoint(&sender, "").status(1, ComputerStatus::Probing, &AtomicBool::new(false)));
    let item = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(item.event(), Event::Status { generation: 1, .. }));
    assert_eq!(sender.usage().bytes, 512);
    assert_eq!(sender.usage().items, 1);
    drop(item);
    assert_eq!(sender.usage().bytes, 0);
}

#[test]
fn frame_age_is_preserved_instead_of_renewed_at_consumption() {
    let (sender, receiver) = channel(Command::spawn);
    let received_at = Instant::now() - Duration::from_secs(60);
    let frame = Box::new(
        serde_json::from_value(serde_json::json!({
            "v":1,"seq":7,"t":"heartbeat","at":0
        }))
        .unwrap(),
    );
    let permit = sender.reserve(1024, &AtomicBool::new(false)).unwrap();
    assert!(endpoint(&sender, "peer").frame(1, frame, received_at, permit));
    let item = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(item.event(), Event::Frame { received_at:at,.. }
        if *at==received_at && at.elapsed()>=Duration::from_secs(60)));
    assert_eq!(sender.usage().bytes, 1024);
}

#[test]
fn at_most_four_connection_attempts_with_cancellable_wait() {
    let (sender, _receiver) = channel(Command::spawn);
    let mut slots: Vec<_> = (0..CONNECTIONS)
        .map(|_| sender.connect(&AtomicBool::new(false)).unwrap())
        .collect();
    let stop = Arc::new(AtomicBool::new(false));
    let task = {
        let sender = sender.clone();
        let stop = stop.clone();
        std::thread::spawn(move || sender.connect(&stop).is_none())
    };
    stop.store(true, Ordering::Relaxed);
    assert!(task.join().unwrap());
    assert_eq!(sender.usage().connecting, 4);
    slots.pop();
    assert_eq!(sender.usage().connecting, 3);
    let last = sender.connect(&AtomicBool::new(false)).unwrap();
    assert_eq!(sender.usage().connecting, 4);
    drop(last);
    drop(slots);
    assert_eq!(sender.usage().connecting, 0);
}

#[test]
fn stalled_readers_cannot_accumulate_on_retry_or_new_registration() {
    let (sender, _receiver) = channel(Command::spawn);
    let first = endpoint(&sender, "peer");
    let slot = Arc::new(first.reader().unwrap());
    let stderr = slot.clone();
    let mut replacement = first.clone();
    replacement.identity.incarnation = 2;
    assert!(replacement.reader().is_none());
    drop(slot);
    assert!(
        replacement.reader().is_none(),
        "stderr still owns this reader group"
    );
    let slots: Vec<_> = (1..SOURCES)
        .map(|n| endpoint(&sender, &format!("peer-{n}")).reader().unwrap())
        .collect();
    assert!(endpoint(&sender, "extra").reader().is_none());
    assert_eq!(sender.usage().readers, SOURCES);
    drop(stderr);
    assert!(replacement.reader().is_some());
    drop(slots);
    assert_eq!(sender.usage().readers, 0);
}

#[test]
fn duplicate_alias_and_closed_channel_refuse_before_child_creation() {
    let (sender, receiver) = channel(Command::spawn);
    let path = Path::new("missing-hgs-source-fixture");
    let first = sender
        .spawn(path, &ComputerId::local(), Timing::default())
        .unwrap();
    assert!(sender
        .spawn(path, &ComputerId::local(), Timing::default())
        .is_err());
    let identity = first.identity().clone();
    let at = Instant::now();
    first.stop();
    assert!(at.elapsed() < Duration::from_secs(2));
    let second = sender
        .spawn(path, &ComputerId::local(), Timing::default())
        .unwrap();
    assert!(second.identity().incarnation > identity.incarnation);
    second.stop();
    for alias in ["@peer", "-option", "two words"] {
        assert!(sender
            .spawn(path, &alias.into(), Timing::default())
            .is_err());
    }
    drop(receiver);
    assert!(sender
        .spawn(path, &"other".into(), Timing::default())
        .is_err());
}

#[test]
fn source_cap_refuses_without_starting_a_thread() {
    let (sender, _receiver) = channel(Command::spawn);
    {
        let mut state = sender.shared.lock();
        for n in 0..SOURCES {
            state.sources.insert(
                format!("peer-{n}").as_str().into(),
                Registered {
                    incarnation: n as u64,
                    cancel: None,
                },
            );
        }
    }
    assert!(sender
        .spawn(
            Path::new("missing-hgs-source-fixture"),
            &"extra".into(),
            Timing::default()
        )
        .is_err());
}

#[test]
fn pending_workers_keep_admission_after_handle_registrations_end() {
    let (sender, _receiver) = channel(Command::spawn);
    let mut workers = Vec::new();
    for _ in 0..SOURCES {
        sender.shared.lock().usage.workers += 1;
        workers.push(Arc::new(Worker(sender.shared.clone())));
    }
    assert_eq!(sender.usage().sources, 0);
    assert!(sender
        .spawn(
            Path::new("missing-hgs-source-fixture"),
            &"extra".into(),
            Timing::default()
        )
        .is_err());
    workers.pop();
    let handle = sender
        .spawn(
            Path::new("missing-hgs-source-fixture"),
            &"extra".into(),
            Timing::default(),
        )
        .unwrap();
    handle.stop();
    drop(workers);
    assert_eq!(sender.usage().workers, 0);
}

#[test]
fn a_cancelled_endpoint_cannot_admit_a_late_reader() {
    let (sender, _receiver) = channel(Command::spawn);
    let handle = sender
        .spawn(
            Path::new("missing-hgs-source-fixture"),
            &"peer".into(),
            Timing::default(),
        )
        .unwrap();
    let cancel = sender
        .shared
        .lock()
        .sources
        .get(&"peer".into())
        .unwrap()
        .cancel
        .clone()
        .unwrap();
    let mut endpoint = endpoint(&sender, "peer");
    endpoint.set_cancel(cancel.clone());
    cancel.cancel();
    assert!(endpoint.reader().is_none());
    handle.stop();
    assert_eq!(sender.usage().workers, 0);
}

#[test]
fn control_batches_pin_identity_and_finite_byte_item_admission_until_drop() {
    let (sender, _receiver) = channel(Command::spawn);
    let identity = Identity {
        computer: "peer".into(),
        incarnation: 7,
    };
    sender.shared.lock().sources.insert(
        identity.computer.clone(),
        Registered {
            incarnation: 7,
            cancel: None,
        },
    );
    let ping = Control::Ping(zerus_watch_protocol::Ping {
        id: Some(serde_json::json!("fence")),
    });
    assert!(sender
        .shared
        .control_batch(&identity, 0, std::slice::from_ref(&ping))
        .is_none());
    assert!(sender
        .shared
        .control_batch(
            &Identity {
                incarnation: 6,
                ..identity.clone()
            },
            1,
            std::slice::from_ref(&ping)
        )
        .is_none());
    assert!(sender.shared.control_batch(&identity, 1, &[]).is_none());
    assert!(sender
        .shared
        .control_batch(&identity, 1, &vec![ping.clone(); 4])
        .is_none());
    let oversized = Control::Ping(zerus_watch_protocol::Ping {
        id: Some(serde_json::json!("x".repeat(CONTROL_BATCH_BYTES))),
    });
    assert!(sender
        .shared
        .control_batch(&identity, 1, &[oversized])
        .is_none());
    let batches: Vec<_> = (0..CONTROL_BATCHES)
        .map(|_| {
            sender
                .shared
                .control_batch(&identity, 1, std::slice::from_ref(&ping))
                .unwrap()
        })
        .collect();
    assert_eq!(sender.usage().controls, CONTROL_BATCHES);
    assert!(sender.usage().control_bytes <= CONTROL_BYTES);
    assert!(sender
        .shared
        .control_batch(&identity, 1, std::slice::from_ref(&ping))
        .is_none());
    drop(batches);
    assert_eq!(sender.usage().controls, 0);
    assert_eq!(sender.usage().control_bytes, 0);
}
