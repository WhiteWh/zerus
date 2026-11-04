use super::*;
use std::sync::mpsc;
fn fixture() -> (tempfile::TempDir, Location) {
    let temp = tempfile::tempdir().unwrap();
    let location = Location {
        root: temp.path().canonicalize().unwrap().join("state/desktop"),
        configuration: "a".repeat(64),
    };
    location.prepare().unwrap();
    (temp, location)
}
#[test]
fn stopped_selection_keeps_the_original_service_and_handoff_locks_until_real_join() {
    let _serial = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (_temp, location) = fixture();
    let service = Arc::new(location.lock().unwrap());
    let owner = Owner::acquire(service, &location).unwrap();
    let (entered, ready) = mpsc::sync_channel(1);
    let (release, resume) = mpsc::sync_channel(1);
    let mut preparation = Preparation::spawn(owner, move |_| {
        entered.send(()).unwrap();
        resume.recv().unwrap();
        anyhow::bail!("fixture selection refusal")
    })
    .unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    preparation.stop();
    assert!(preparation.poll().is_none());
    assert!(!preparation.drained());
    assert!(location.lock().is_err());
    assert!(location.handoff_lock().is_err());
    release.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(result) = preparation.poll() {
            assert!(result.is_err());
            break;
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(preparation.drained());
    assert!(preparation.owner().is_none());
    assert!(location.lock().is_ok());
    assert!(location.handoff_lock().is_ok());
}
#[test]
fn a_busy_handoff_does_not_leak_a_preparation_admission() {
    let _serial = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (_temp, location) = fixture();
    let service = Arc::new(location.lock().unwrap());
    let handoff = location.handoff_lock().unwrap();
    assert!(Owner::acquire(service.clone(), &location).is_err());
    drop(handoff);
    let owner = Owner::acquire(service.clone(), &location).unwrap();
    assert!(location.handoff_lock().is_err());
    drop(owner);
    drop(service);
    assert!(location.lock().is_ok());
    assert!(location.handoff_lock().is_ok());
}
