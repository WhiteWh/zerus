//! Task #406 (owner, 2026-11-09): the team worker's tick is measured. Every
//! phase of a tick (the session listing, a team's hand-outs, its outbox, its
//! other steps, its merge queue) is timed. The phase the worker is in right now
//! and the last ticks with their slowest phase are kept in `tick.json` in the
//! worker's folder, bounded, so `hgs team status` shows them and the lead's
//! inbox can say which step holds the worker when nothing is handed out.
use serde_json::{json, Value};
use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Ticks kept in `tick.json`.
const RECENT: usize = 30;
/// A tick at least this long is written to the worker's log.
pub(crate) const SLOW_TICK: Duration = Duration::from_secs(2);
/// A phase that has held the worker this long, while a task is ready, is an
/// alarm in the lead's inbox.
pub(crate) const STALL_SECONDS: f64 = 300.0;

struct Clock {
    started: Instant,
    at: f64,
    slowest: (String, Duration),
}

thread_local! {
    static CLOCK: RefCell<Option<Clock>> = const { RefCell::new(None) };
    #[cfg(test)]
    static DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_dir(dir: Option<PathBuf>) {
    DIR.with(|cell| *cell.borrow_mut() = dir);
}

fn path() -> PathBuf {
    // A test reads and writes only the folder it named, never the real
    // worker's file.
    #[cfg(test)]
    return DIR
        .with(|cell| cell.borrow().clone())
        .unwrap_or_else(|| std::env::temp_dir().join("zerus-tick-clock-unset"))
        .join("tick.json");
    #[cfg(not(test))]
    super::dispatcher::worker_dir().join("tick.json")
}

fn read() -> Value {
    std::fs::read_to_string(path()).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_else(|| json!({}))
}

fn write(value: &Value) {
    let path = path();
    let temp = path.with_extension("json.tmp");
    if std::fs::write(&temp, value.to_string()).is_ok() {
        let _ = std::fs::rename(&temp, &path);
    }
}

/// A tick starts: its clock, and `running` in `tick.json`.
pub(crate) fn begin() {
    let at = crate::state::now();
    CLOCK.with(|cell| *cell.borrow_mut() = Some(Clock { started: Instant::now(), at, slowest: (String::new(), Duration::ZERO) }));
    let mut state = read();
    state["running"] = json!({"tick_started":at,"step":"start","step_started":at});
    write(&state);
}

/// How long the current tick has run (zero outside a tick).
pub(crate) fn elapsed() -> Duration {
    CLOCK.with(|cell| cell.borrow().as_ref().map(|clock| clock.started.elapsed()).unwrap_or_default())
}

/// Runs one phase of the tick: `tick.json` names it while it runs, and the
/// slowest phase is kept for the tick's record.
pub(crate) fn phase<T>(name: &str, run: impl FnOnce() -> T) -> T {
    let ticking = CLOCK.with(|cell| cell.borrow().is_some());
    if ticking {
        let mut state = read();
        state["running"]["step"] = json!(name);
        state["running"]["step_started"] = json!(crate::state::now());
        write(&state);
    }
    let started = Instant::now();
    let out = run();
    let took = started.elapsed();
    CLOCK.with(|cell| {
        if let Some(clock) = cell.borrow_mut().as_mut() {
            if took > clock.slowest.1 {
                clock.slowest = (name.to_owned(), took);
            }
        }
    });
    out
}

/// A step the tick already timed itself: only its time counts, nothing is written.
pub(crate) fn note(name: &str, took: Duration) {
    CLOCK.with(|cell| {
        if let Some(clock) = cell.borrow_mut().as_mut() {
            if took > clock.slowest.1 {
                clock.slowest = (name.to_owned(), took);
            }
        }
    });
}

/// One finished tick, as `tick.json` keeps it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Record {
    pub ms: u128,
    pub slowest: String,
    pub slowest_ms: u128,
}

/// The tick ends: its record joins the last `RECENT` ones and nothing runs.
pub(crate) fn finish() -> Option<Record> {
    let clock = CLOCK.with(|cell| cell.borrow_mut().take())?;
    let record = Record { ms: clock.started.elapsed().as_millis(), slowest: clock.slowest.0, slowest_ms: clock.slowest.1.as_millis() };
    let mut state = read();
    let mut recent = state["recent"].as_array().cloned().unwrap_or_default();
    recent.push(json!({"at":clock.at,"ms":record.ms,"slowest":record.slowest,"slowest_ms":record.slowest_ms}));
    let drop = recent.len().saturating_sub(RECENT);
    recent.drain(..drop);
    let worst = recent.iter().max_by_key(|tick| tick["ms"].as_u64().unwrap_or(0)).cloned().unwrap_or(Value::Null);
    state["recent"] = json!(recent);
    state["last"] = recent.last().cloned().unwrap_or(Value::Null);
    state["worst"] = worst;
    state["running"] = Value::Null;
    write(&state);
    Some(record)
}

/// Task #7 of team c536f0e5 (P1.6): counts the worker's session listings in
/// `tick.json` (`listing`): `reused` when a tick acted on an unchanged one,
/// `derived` when one was read, with the reason (`first`, `inputs`, `probe`,
/// `time`, `safety`, `peers`, `prefetch`), since the worker started counting.
pub(crate) fn count_listing(kind: &str, why: &str) {
    let mut state = read();
    if !state["listing"].is_object() {
        state["listing"] = json!({"since":crate::state::now(),"reused":0,"derived":0,"why":{}});
    }
    let counts = &mut state["listing"];
    counts[kind] = json!(counts[kind].as_u64().unwrap_or(0) + 1);
    if !why.is_empty() {
        counts["why"][why] = json!(counts["why"][why].as_u64().unwrap_or(0) + 1);
    }
    write(&state);
}

/// What `hgs team status` shows of the worker's ticks: the phase running now
/// and since when, the last tick and the slowest of the recent ones.
pub(crate) fn status() -> Value {
    let state = read();
    json!({"running":state["running"],"last":state["last"],"worst":state["worst"],
        "recent":state["recent"].as_array().map_or(0, Vec::len),"listing":state["listing"]})
}

/// The phase holding the worker and for how long, once it has held it for
/// `STALL_SECONDS`: a tick still running, or none finished for that long.
pub(crate) fn stall(now: f64) -> Option<(String, f64)> {
    let state = read();
    let running = &state["running"];
    if let Some(since) = running["step_started"].as_f64() {
        let held = now - since;
        return (held >= STALL_SECONDS).then(|| (running["step"].as_str().unwrap_or("a tick").to_owned(), held));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tick_names_its_running_phase_and_keeps_its_slowest() {
        let dir = tempfile::tempdir().unwrap();
        set_test_dir(Some(dir.path().to_path_buf()));
        begin();
        phase("listing", || std::thread::sleep(Duration::from_millis(30)));
        phase("hand-outs: Team", || {
            assert_eq!(read()["running"]["step"], "hand-outs: Team", "the phase running now is named");
        });
        let record = finish().unwrap();
        assert_eq!(record.slowest, "listing");
        assert!(record.slowest_ms >= 30 && record.ms >= record.slowest_ms, "{record:?}");
        let first = status();
        assert!(first["running"].is_null());
        assert_eq!(first["last"]["slowest"], "listing");
        // The record is bounded.
        for _ in 0..(RECENT + 5) {
            begin();
            finish();
        }
        assert_eq!(status()["recent"], RECENT);
        // A phase outside a tick is still run, unmeasured.
        assert_eq!(phase("loose", || 7), 7);
        set_test_dir(None);
    }

    #[test]
    fn a_phase_that_held_the_worker_for_five_minutes_is_a_stall() {
        let dir = tempfile::tempdir().unwrap();
        set_test_dir(Some(dir.path().to_path_buf()));
        write(&json!({"running":{"tick_started":1000.0,"step":"outbox: Team","step_started":1000.0}}));
        assert_eq!(stall(1000.0 + STALL_SECONDS - 1.0), None);
        assert_eq!(stall(1000.0 + STALL_SECONDS + 1.0), Some(("outbox: Team".to_owned(), STALL_SECONDS + 1.0)));
        write(&json!({"running":null,"last":{"ms":10}}));
        assert_eq!(stall(1e12), None, "a finished tick holds nothing");
        set_test_dir(None);
    }
}
