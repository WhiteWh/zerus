//! Task #17 of team c536f0e5 (P2.6a): what wakes the team worker and when a
//! wake runs a full tick.
use super::*;

fn hint(paths: &[&str]) -> Hint {
    Hint { paths: paths.iter().map(PathBuf::from).collect(), overflow: false }
}

#[test]
fn the_stop_request_the_store_and_session_inputs_wake_the_worker_and_its_own_files_do_not() {
    for path in ["teams/worker.stop", "teams/teams.sqlite3", "teams/teams.sqlite3-wal", "events.sqlite3-wal"] {
        assert!(keep_root(Path::new(path)), "{path}");
    }
    // The worker writes these every tick; shared memory changes on reads.
    for path in ["teams", "teams/tick.json", "teams/tick.json.tmp", "teams/worker.heartbeat", "teams/worker.log", "teams/teams.sqlite3-shm"] {
        assert!(!keep_root(Path::new(path)), "{path}");
    }
    assert_eq!(classify(&hint(&["teams/worker.stop"])), BTreeSet::from([Event::Stop]));
    assert_eq!(classify(&hint(&["teams\\teams.sqlite3-wal", "events.sqlite3-wal"])), BTreeSet::from([Event::Store, Event::Session]));
    assert_eq!(classify(&Hint { paths: BTreeSet::new(), overflow: true }), BTreeSet::from([Event::Overflow]));
}

struct Idle {
    since_tick: Duration,
}

impl Idle {
    fn reason(&self, events: &BTreeSet<Event>) -> Option<&'static str> {
        tick_reason(
            events,
            Check {
                listening: true,
                last_tick_quiet: true,
                since_tick: self.since_tick,
                store_moved: || false,
                in_flight: || None,
                outboxes_moved: || false,
                listing_current: || true,
            },
        )
    }
}

#[test]
fn a_wake_ticks_for_an_event_a_moved_input_or_work_in_flight_and_otherwise_on_the_safety_net() {
    let none = BTreeSet::new();
    let idle = Idle { since_tick: Duration::from_secs(3) };
    assert_eq!(idle.reason(&none), None, "nothing moved");
    assert_eq!(Idle { since_tick: SAFETY }.reason(&none), Some("safety"));
    for (event, why) in [(Event::Store, "store"), (Event::Session, "session"), (Event::Outbox, "outbox"), (Event::Overflow, "overflow")] {
        assert_eq!(idle.reason(&BTreeSet::from([event])), Some(why));
    }
    // The stop request and a replaced executable end the worker before any
    // tick; as events they tick nothing themselves.
    assert_eq!(idle.reason(&BTreeSet::from([Event::Stop, Event::Executable])), None);
    let check = |listening, quiet, store, flight: Option<&'static str>, outbox, listing| {
        tick_reason(
            &none,
            Check {
                listening,
                last_tick_quiet: quiet,
                since_tick: Duration::from_secs(3),
                store_moved: || store,
                in_flight: || flight,
                outboxes_moved: || outbox,
                listing_current: || listing,
            },
        )
    };
    assert_eq!(check(false, true, false, None, false, true), Some("no bus"), "without the bus every wake ticks");
    assert_eq!(check(true, false, false, None, false, true), Some("busy"), "the last tick changed a team");
    assert_eq!(check(true, true, true, None, false, true), Some("store"), "a commit the bus missed");
    assert_eq!(check(true, true, false, Some("delivering"), false, true), Some("delivering"));
    assert_eq!(check(true, true, false, None, true, true), Some("outbox"));
    assert_eq!(check(true, true, false, None, false, false), Some("listing"), "a session moved or changes by time");
    // The listing's probe, the costliest check, runs only when the others
    // found nothing.
    let probed = std::cell::Cell::new(false);
    let reason = tick_reason(
        &none,
        Check {
            listening: true,
            last_tick_quiet: true,
            since_tick: Duration::ZERO,
            store_moved: || true,
            in_flight: || None,
            outboxes_moved: || false,
            listing_current: || {
                probed.set(true);
                true
            },
        },
    );
    assert_eq!((reason, probed.get()), (Some("store"), false));
}

#[test]
fn an_idle_team_ticks_on_the_safety_net_alone() {
    // An hour of wakes, WAKE apart, on a team where nothing moves.
    let wakes = (3600 / WAKE.as_secs()) as usize;
    let mut idle = Idle { since_tick: Duration::ZERO };
    let mut ticks = 0;
    for _ in 0..wakes {
        idle.since_tick += WAKE;
        if idle.reason(&BTreeSet::new()).is_some() {
            ticks += 1;
            idle.since_tick = Duration::ZERO;
        }
    }
    assert_eq!(ticks, 240, "one full tick each 15 s");
    // Without the bus every wake is a tick, as before.
    let unwatched = (0..wakes)
        .filter(|_| {
            tick_reason(
                &BTreeSet::new(),
                Check {
                    listening: false,
                    last_tick_quiet: true,
                    since_tick: WAKE,
                    store_moved: || false,
                    in_flight: || None,
                    outboxes_moved: || false,
                    listing_current: || true,
                },
            )
            .is_some()
        })
        .count();
    assert_eq!(unwatched, 1200);
}

#[test]
fn wakes_and_ticks_are_counted_by_reason() {
    let mut counts = Counts::new(5.0);
    counts.tick("start");
    counts.wake("timer");
    counts.wake("timer");
    counts.wake("own commit");
    counts.wake("store");
    counts.tick("store");
    assert_eq!(
        counts.value(true),
        json!({"since":5.0,"bus":true,"wakes":{"own commit":1,"store":1,"timer":2},"ticks":{"start":1,"store":1}})
    );
}
