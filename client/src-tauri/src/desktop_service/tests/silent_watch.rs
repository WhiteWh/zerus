use super::*;

const CHILD: &str = "ZERUS_TEST_SILENT_WATCH_CLOCK";

#[test]
fn silent_watch_expires_and_explicit_retry_reconnects() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    let log_root = Fixture::new(json!({}));
    let log = fs::File::create(log_root.0.join("clock-child.log")).unwrap();
    // The helper has a DIFFERENT exact name and cannot recursively launch this
    // test. A separate process owns Tauri's once-only global runtime handle.
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "desktop_service::tests::silent_watch::clock_child",
            "--ignored",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= until {
            child.kill().unwrap(); // Only this original private Rust test child.
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let path = log_root.0.join("clock-child.log");
    assert!(fs::metadata(&path).unwrap().len() <= 64 << 10);
    let output = fs::read_to_string(path).unwrap();
    assert!(
        status.is_some_and(|status| status.success()),
        "{status:?}\n{output}"
    );
}

/// Yield real I/O and blocking workers without letting a paused Tokio clock
/// auto-advance. The wall-clock bound diagnoses a hung fixture, not the product
/// timeout; only explicit advance() calls may cross that timeout in this test.
async fn observed(label: &str, service: &Service, check: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while !check() {
        assert!(Instant::now() < until, "{label}: {:?}", service.snapshot());
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "owned subprocess with an isolated Tauri runtime; run by the parent test"]
fn clock_child() {
    assert_eq!(std::env::var(CHILD).as_deref(), Ok("1"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tauri::async_runtime::set(runtime.handle().clone());
    runtime.block_on(async {
        // Freeze before even spawning the helper: hello and last_frame share
        // this exact instant, regardless of hosted process-startup latency.
        tokio::time::pause();
        let fixture = Fixture::new(json!({"silent_watch": true}));
        let greeted = Arc::new(AtomicBool::new(false));
        let hello = greeted.clone();
        let pump = crate::pump::Pump::default();
        let (sink, rx) = std::sync::mpsc::sync_channel(256);
        let drain = pump.drain_service(rx, |_| {});
        let service = Service::start(
            fixture.0.join("hgs"),
            pump.clone(),
            sink,
            true,
            drain,
            None,
            vec![Arc::new(move |frame| {
                if frame.is_some_and(|frame| frame["t"] == "hello") {
                    hello.store(true, Ordering::Release);
                }
            })],
        );
        observed("silent watch hello", &service, || {
            let settled = {
                let dirty = service.shared.dirty.lock().unwrap();
                dirty.running.is_none() && dirty.pending.is_empty()
            };
            greeted.load(Ordering::Acquire)
                && service.snapshot().connected
                && pump.row("", "row").is_some()
                && settled
        })
        .await;

        tokio::time::advance(Duration::from_millis(29_999)).await;
        // Give the select loop a turn at the instant just BEFORE its deadline.
        tokio::task::yield_now().await;
        assert!(service.snapshot().connected);
        assert_eq!(watch_calls(&fixture.calls()), 1);

        // Tokio rounds timer deadlines up to its millisecond tick. Cross the
        // boundary by one tick, without waiting thirty real seconds.
        tokio::time::advance(Duration::from_millis(2)).await;
        observed(
            "silent watch must report service_stale at 30 seconds",
            &service,
            || {
                let snapshot = service.snapshot();
                !snapshot.connected && snapshot.code == "service_stale"
            },
        )
        .await;
        assert_eq!(
            watch_calls(&fixture.calls()),
            1,
            "silence cannot replay a watch"
        );

        fs::write(fixture.0.join("fixture.json"), "{}").unwrap();
        service.retry(); // The normal explicit Retry opens one fresh watch.
        observed("retry reconnects a fresh watch", &service, || {
            service.snapshot().connected && watch_calls(&fixture.calls()) == 2
        })
        .await;
        tokio::time::resume();
        service.join().await.unwrap();
    });
}
