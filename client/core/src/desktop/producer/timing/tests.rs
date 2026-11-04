use super::*;

#[test]
fn agreed_boundaries_are_fixed_at_preparation_without_a_worker_or_wall_clock() {
    let start = Instant::now();
    let timing = Timing::new(start).unwrap();
    assert_eq!(HEARTBEAT_INTERVAL, Duration::from_secs(3));
    assert_eq!(IDLE_LIMIT, Duration::from_secs(15));
    assert_eq!(DRAIN_WAIT, Duration::from_secs(120));
    assert_eq!(ABSOLUTE_LIMIT, Duration::from_secs(600));
    assert_eq!(timing.heartbeat_due(), start + HEARTBEAT_INTERVAL);
    assert_eq!(timing.deadline(), start + IDLE_LIMIT);
    assert_eq!(timing.drain_deadline(), start + DRAIN_WAIT);
    assert_eq!(timing.absolute_deadline(), start + ABSOLUTE_LIMIT);
}

#[test]
fn ordinary_heartbeats_extend_idle_but_never_the_absolute_or_drain_boundary() {
    let start = Instant::now();
    let mut timing = Timing::new(start).unwrap();
    for seconds in (3..600).step_by(3) {
        let now = start + Duration::from_secs(seconds);
        assert_eq!(timing.heartbeat(now), Observation::WithinBounds);
        assert_eq!(timing.drain_deadline(), start + DRAIN_WAIT);
        assert_eq!(timing.absolute_deadline(), start + ABSOLUTE_LIMIT);
        assert!(timing.deadline() <= timing.absolute_deadline());
        assert!(timing.heartbeat_due() <= timing.absolute_deadline());
    }
    assert_eq!(
        timing.observe(start + ABSOLUTE_LIMIT - Duration::from_nanos(1)),
        Observation::WithinBounds
    );
    assert_eq!(
        timing.heartbeat(start + ABSOLUTE_LIMIT),
        Observation::Invalid(Reason::AbsoluteExpired)
    );
    assert_eq!(
        timing.heartbeat(start + ABSOLUTE_LIMIT + HEARTBEAT_INTERVAL),
        Observation::Invalid(Reason::AbsoluteExpired)
    );
}

#[test]
fn heartbeats_at_or_after_the_idle_boundary_cannot_revive_the_preparation() {
    let start = Instant::now();
    for late in [IDLE_LIMIT, IDLE_LIMIT + Duration::from_nanos(1)] {
        let mut timing = Timing::new(start).unwrap();
        assert_eq!(
            timing.observe(start + IDLE_LIMIT - Duration::from_nanos(1)),
            Observation::WithinBounds
        );
        assert_eq!(
            timing.heartbeat(start + late),
            Observation::Invalid(Reason::IdleExpired)
        );
        assert_eq!(timing.deadline(), start + IDLE_LIMIT);
        assert_eq!(
            timing.heartbeat(start + late + HEARTBEAT_INTERVAL),
            Observation::Invalid(Reason::IdleExpired)
        );
    }
    let mut early = Timing::new(start).unwrap();
    assert_eq!(
        early.heartbeat(start + Duration::from_secs(14)),
        Observation::WithinBounds
    );
    assert_eq!(early.deadline(), start + Duration::from_secs(29));
}

#[test]
fn drain_timeout_is_unknown_even_with_uninterrupted_heartbeats() {
    let start = Instant::now();
    let mut timing = Timing::new(start).unwrap();
    for seconds in (3..120).step_by(3) {
        let now = start + Duration::from_secs(seconds);
        assert_eq!(timing.heartbeat(now), Observation::WithinBounds);
        assert_eq!(timing.observe_drain(now), Observation::WithinBounds);
    }
    assert_eq!(
        timing.observe_drain(start + DRAIN_WAIT - Duration::from_nanos(1)),
        Observation::WithinBounds
    );
    assert_eq!(
        timing.observe_drain(start + DRAIN_WAIT),
        Observation::Invalid(Reason::DrainExpired)
    );
    assert_eq!(
        timing.heartbeat(start + DRAIN_WAIT),
        Observation::Invalid(Reason::DrainExpired)
    );
}

#[test]
fn every_explicit_invalidation_is_sticky_and_preserves_its_first_reason() {
    let start = Instant::now();
    for reason in [
        Reason::ConnectionLost,
        Reason::Revoked,
        Reason::OwnerChanged,
        Reason::ProofInvalidated,
        Reason::Unavailable,
    ] {
        let mut timing = Timing::new(start).unwrap();
        assert_eq!(timing.invalidate(reason), Observation::Invalid(reason));
        assert_eq!(
            timing.invalidate(Reason::Unavailable),
            Observation::Invalid(reason)
        );
        for now in [start, start + HEARTBEAT_INTERVAL, start + ABSOLUTE_LIMIT] {
            assert_eq!(timing.observe(now), Observation::Invalid(reason));
            assert_eq!(timing.observe_drain(now), Observation::Invalid(reason));
            assert_eq!(timing.heartbeat(now), Observation::Invalid(reason));
        }
    }
}

#[test]
fn local_time_cannot_move_backwards_or_accept_a_queued_old_observation() {
    let start = Instant::now();
    for (observed, stale) in [
        (start, start - Duration::from_nanos(1)),
        (start + HEARTBEAT_INTERVAL, start),
    ] {
        let mut timing = Timing::new(start).unwrap();
        assert_eq!(timing.heartbeat(observed), Observation::WithinBounds);
        assert_eq!(
            timing.observe(stale),
            Observation::Invalid(Reason::NonMonotonic)
        );
        assert_eq!(
            timing.heartbeat(observed + HEARTBEAT_INTERVAL),
            Observation::Invalid(Reason::NonMonotonic)
        );
    }
}
