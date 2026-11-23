//! Task #7 of team c536f0e5 (P1.6, owner and lead 2026-11-23): the team
//! worker reads its session listing in its own process, without the peers
//! when no team needs them, and reuses it only while nothing it reads moved
//! and no row changes by time alone; what it acts on is never older than 3 s.
use super::*;
use serde_json::json;

const AT: f64 = 1_000_000.0;

fn inputs(stamps: u64) -> ListingInputs {
    ListingInputs { stamps, journal: Some(7), structure: 11 }
}

fn row(state: &str) -> Value {
    json!({"name":"codex/x/dev","session_state":state,"last_active":AT - 60.0,"cache_ttl":{"seconds":3600}})
}

fn local(rows: Vec<Value>) -> Value {
    json!([{"host":"here","ok":true,"sessions":rows}])
}

fn cached<'a>(value: &'a Value, inputs: &'a ListingInputs, derived: u64, current: u64) -> CachedListing<'a> {
    CachedListing {
        value,
        peers: false,
        inputs: Some(inputs),
        derived_ago: Duration::from_secs(derived),
        current_ago: Duration::from_secs(current),
    }
}

#[test]
fn an_unchanged_idle_listing_is_reused_and_anything_else_is_derived_unless_fresh() {
    let idle = local(vec![row("cold"), row("cooling"), row("user"), row("stopped")]);
    let same = inputs(1);
    let choice = |cached: Option<&CachedListing>, now: Option<&ListingInputs>, probe: bool, peers: bool| {
        listing_choice(cached, now, probe, peers, AT)
    };
    // Nothing moved, no row changes by time: reused, however long ago it was
    // last confirmed, until the 30 s safety net.
    assert_eq!(choice(Some(&cached(&idle, &same, 10, 10)), Some(&same), true, false), ListingChoice::Reuse);
    assert_eq!(choice(Some(&cached(&idle, &same, 29, 3)), Some(&same), true, false), ListingChoice::Reuse);
    assert_eq!(choice(Some(&cached(&idle, &same, 30, 4)), Some(&same), true, false), ListingChoice::Derive("safety"));
    // No listing yet.
    assert_eq!(choice(None, Some(&same), true, false), ListingChoice::Derive("first"));
    // A moved input, an unreadable one, or a probe that saw a change: derived,
    // unless the listing was current at most 3 s ago.
    for (now, probe, why) in [(Some(&inputs(2)), true, "inputs"), (None, true, "inputs"), (Some(&same), false, "probe")] {
        assert_eq!(choice(Some(&cached(&idle, &same, 10, 4)), now, probe, false), ListingChoice::Derive(why));
        assert_eq!(choice(Some(&cached(&idle, &same, 10, 3)), now, probe, false), ListingChoice::Fresh);
    }
    // A team that needs the peers: a local listing does not do.
    assert_eq!(choice(Some(&cached(&idle, &same, 1, 1)), Some(&same), true, true), ListingChoice::Derive("peers"));
    // A listing with the peers is never reused: nothing here sees them change.
    let mut fleet = cached(&idle, &same, 1, 4);
    fleet.peers = true;
    assert_eq!(choice(Some(&fleet), Some(&same), true, true), ListingChoice::Derive("peers"));
    fleet.current_ago = Duration::from_secs(2);
    assert_eq!(choice(Some(&fleet), Some(&same), true, false), ListingChoice::Fresh);
    // A working row changes by time: derived every tick, as before.
    let working = local(vec![row("cold"), row("working")]);
    assert_eq!(choice(Some(&cached(&working, &same, 5, 4)), Some(&same), true, false), ListingChoice::Derive("time"));
    assert_eq!(choice(Some(&cached(&working, &same, 5, 2)), Some(&same), true, false), ListingChoice::Fresh);
}

#[test]
fn rows_whose_state_changes_by_time_alone() {
    let driven = |row: Value| time_driven_rows(&local(vec![row]), AT, TIME_HORIZON);
    for state in ["working", "build", "", "something new"] {
        assert!(driven(row(state)), "{state}");
    }
    for state in ["cold", "stopped", "offline", "user"] {
        assert!(!driven(row(state)), "{state}");
    }
    // Cooling: only when its cache TTL ends before the next tick, with room.
    assert!(!driven(json!({"session_state":"cooling","last_active":AT - 60.0,"cache_ttl":{"seconds":300}})));
    assert!(driven(json!({"session_state":"cooling","last_active":AT - 297.0,"cache_ttl":{"seconds":300}})));
    assert!(driven(json!({"session_state":"cooling","last_active":AT - 60.0})), "no TTL: the default may have ended");
    // Waiting for dependencies: subagents, a long tool call, a job (its
    // budget) and a usage limit about to reset change by time; a team's
    // reason or a coordination wait changes with the store or a file.
    for kind in ["subagents", "long_operation", "job"] {
        assert!(driven(json!({"session_state":"deps","state_reason":{"kind":kind}})), "{kind}");
    }
    assert!(driven(json!({"session_state":"deps","state_reason":{"kind":"usage_limit"}})));
    assert!(driven(json!({"session_state":"deps","state_reason":{"kind":"usage_limit","resets_at":AT + 2.0}})));
    assert!(!driven(json!({"session_state":"deps","state_reason":{"kind":"usage_limit","resets_at":AT + 600.0}})));
    assert!(!driven(json!({"session_state":"deps","state_reason":{"kind":"coordination"}})));
    assert!(!driven(json!({"session_state":"deps","state_reason":{"kind":"task","text":"Waiting for dependencies: #3"}})));
    // Only this computer's rows count; an empty listing changes nothing.
    assert!(!time_driven_rows(&json!([{"sessions":[]},{"sessions":[row("working")]}]), AT, TIME_HORIZON));
}

#[test]
fn the_peers_are_listed_only_for_a_member_or_watched_session_on_another_computer() {
    let mut team = Team { id: "t".into(), ..Default::default() };
    team.members.push(Member { id: "dev".into(), ..Default::default() });
    let watched = |host: &str| super::super::owner_queue::Watched { host: host.into(), name: "s".into(), minutes: 5 };
    assert!(!needs_peers(std::slice::from_ref(&team), &[]));
    assert!(!needs_peers(std::slice::from_ref(&team), &[watched("")]));
    assert!(needs_peers(std::slice::from_ref(&team), &[watched("mac")]));
    team.members[0].session = Some(SessionRef { host: "mac".into(), name: "codex/x/dev".into(), ..Default::default() });
    assert!(needs_peers(std::slice::from_ref(&team), &[]));
    team.members[0].session = None;
    team.members[0].computer = "mac".into();
    assert!(needs_peers(&[team], &[]));
}

#[test]
fn the_listing_starts_as_long_before_the_tick_as_it_takes() {
    use std::sync::atomic::Ordering;
    let before = LISTING_MS.load(Ordering::Relaxed);
    LISTING_MS.store(1_400, Ordering::Relaxed);
    assert_eq!(prefetch_lead(), Duration::from_millis(1_700));
    LISTING_MS.store(0, Ordering::Relaxed);
    assert_eq!(prefetch_lead(), Duration::from_millis(500));
    LISTING_MS.store(60_000, Ordering::Relaxed);
    assert_eq!(prefetch_lead(), TICK, "never more than a tick: the sleep is never negative");
    LISTING_MS.store(before, Ordering::Relaxed);
}

#[test]
fn listings_are_counted_with_their_reason() {
    let dir = tempfile::tempdir().unwrap();
    super::super::tick_clock::set_test_dir(Some(dir.path().to_path_buf()));
    super::super::tick_clock::count_listing("derived", "first");
    super::super::tick_clock::count_listing("reused", "");
    super::super::tick_clock::count_listing("reused", "");
    super::super::tick_clock::count_listing("derived", "time");
    let counts = super::super::tick_clock::status()["listing"].clone();
    super::super::tick_clock::set_test_dir(None);
    assert_eq!((counts["reused"].as_u64(), counts["derived"].as_u64()), (Some(2), Some(2)));
    assert_eq!(counts["why"], json!({"first":1,"time":1}));
}
