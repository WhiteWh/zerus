use super::*;
#[test]
fn a_turn_timeout_or_cancellation_proves_no_start() {
    let _held = spawn_turn();
    assert!(matches!(
        spawn_turn_before(Instant::now() + Duration::from_millis(30), &|| false),
        Err(SpawnTurnNotStarted::Deadline)
    ));
    assert!(matches!(
        spawn_turn_before(Instant::now() + Duration::from_secs(1), &|| true),
        Err(SpawnTurnNotStarted::Cancelled)
    ));
}
#[test]
fn a_reserved_turn_runs_the_same_bounded_child() {
    let until = Instant::now() + Duration::from_secs(3);
    let turn = spawn_turn_before(until, &|| false).unwrap();
    let mut command = Command::new("sh");
    command.args(["-c", "read value || printf closed; printf error >&2"]);
    let (status, out, error) =
        run_bounded_in_turn(&mut command, Stdio::null(), [64, 64], until, &|| false, turn)
            .unwrap();
    assert!(status.success());
    assert_eq!(out, b"closed");
    assert_eq!(error, b"error");
}
