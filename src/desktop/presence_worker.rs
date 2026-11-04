//! One sampler/writer, outside the service actor. Its fence lives on the worker
//! until sampling and the last atomic write have actually returned.
use super::{presence_admission, store::Location};
use crate::presence;
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zerus_presence_sampler::{Sample, Sampler};

const INTERVAL: Duration = Duration::from_secs(2);
const HEARTBEAT: Duration = Duration::from_secs(30);
const ACTIVE_SECONDS: f64 = 120.0;

#[derive(Clone, Copy)]
struct Observation {
    code: &'static str,
    written: Option<Instant>,
    sampled: Option<Instant>,
}

struct Published {
    at: Instant,
    idle: f64,
    locked: bool,
}
impl Published {
    /// Match Qt DesktopPresence::transition: lock changes and return from
    /// aged-away to active publish immediately. Going idle needs no write.
    fn due(&self, at: Instant, idle: f64, locked: bool) -> bool {
        let age = at.saturating_duration_since(self.at);
        age >= HEARTBEAT
            || locked != self.locked
            || (!locked && self.idle + age.as_secs_f64() >= ACTIVE_SECONDS && idle < ACTIVE_SECONDS)
    }
}

pub(super) struct Runtime {
    stop: Arc<AtomicBool>,
    wake: SyncSender<()>,
    worker: Option<JoinHandle<()>>,
    observation: Arc<Mutex<Observation>>,
    failed: bool,
}

impl Runtime {
    pub fn start(location: &Location) -> Result<Self> {
        Self::spawn(location, INTERVAL, || {
            let mut sampler = Sampler::new();
            move || sampler.sample()
        })
    }

    fn spawn<F: FnMut() -> Sample + 'static>(
        location: &Location,
        interval: Duration,
        make_sampler: impl FnOnce() -> F + Send + 'static,
    ) -> Result<Self> {
        let state = location
            .root
            .parent()
            .context("missing state directory")?
            .to_owned();
        Self::spawn_with(
            location,
            interval,
            make_sampler,
            move |sample| presence::write_sample(&state, sample, presence::now()),
            Instant::now,
        )
    }

    fn spawn_with<F: FnMut() -> Sample + 'static>(
        location: &Location,
        interval: Duration,
        make_sampler: impl FnOnce() -> F + Send + 'static,
        mut write: impl FnMut(Sample) -> Result<bool> + Send + 'static,
        mut now: impl FnMut() -> Instant + Send + 'static,
    ) -> Result<Self> {
        let fence = presence_admission::fence(location)?;
        fence.verify(location)?;
        let stop = Arc::new(AtomicBool::new(false));
        let observation = Arc::new(Mutex::new(Observation {
            code: "starting",
            written: None,
            sampled: None,
        }));
        let (wake, receive) = mpsc::sync_channel(1);
        let worker = {
            let stop = stop.clone();
            let observation = observation.clone();
            thread::Builder::new()
                .name("desktop-presence".into())
                .spawn(move || {
                    let _fence = fence;
                    let mut sample = make_sampler();
                    let mut published: Option<Published> = None;
                    while !stop.load(Ordering::SeqCst) {
                        let input = sample();
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        let at = now();
                        let mut wrote = false;
                        let code = match presence::sample_values(input) {
                            Ok(Some((idle, locked))) => {
                                if published.as_ref().is_none_or(|p| p.due(at, idle, locked)) {
                                    match write(input) {
                                        Ok(true) => {
                                            published = Some(Published { at, idle, locked });
                                            wrote = true;
                                            "published"
                                        }
                                        Ok(false) => "inputs_unknown",
                                        Err(_) => "write_failed",
                                    }
                                } else {
                                    "published"
                                }
                            }
                            Ok(None) => "inputs_unknown",
                            Err(_) => "invalid_inputs",
                        };
                        let mut status = observation.lock().unwrap_or_else(|e| e.into_inner());
                        status.code = code;
                        status.sampled = Some(at);
                        if wrote {
                            status.written = Some(at);
                        }
                        drop(status);
                        if receive.recv_timeout(interval).is_ok() {
                            break;
                        }
                    }
                })?
        };
        Ok(Self {
            stop,
            wake,
            worker: Some(worker),
            observation,
            failed: false,
        })
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.wake.try_send(());
    }

    /// Never block the actor in a join of a worker still sampling or writing.
    pub fn stop(&mut self) -> Result<bool> {
        self.request_stop();
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            self.failed = self.worker.take().unwrap().join().is_err();
        }
        ensure!(!self.failed, "desktop presence worker panicked");
        Ok(self.worker.is_none())
    }

    pub fn status(&self) -> Value {
        let observation = self.observation.try_lock().ok().map(|s| *s);
        let unexpectedly_finished = !self.stop.load(Ordering::SeqCst)
            && self.worker.as_ref().is_some_and(JoinHandle::is_finished);
        let mut code = if self.failed || unexpectedly_finished {
            "worker_failed"
        } else if self.worker.is_none() {
            "drained"
        } else if self.stop.load(Ordering::SeqCst) {
            "draining"
        } else {
            observation.map_or("status_unavailable", |s| s.code)
        };
        if code == "published"
            && observation
                .and_then(|s| s.sampled)
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(5))
        {
            code = "sample_stale";
        }
        json!({"code":code,"ready":code=="published","drained":code=="drained",
            "writer_active":self.worker.is_some() && !self.failed && !unexpectedly_finished,
            "last_write_age_seconds":observation.and_then(|s|s.written).map(|at|at.elapsed().as_secs_f64()),
            "interval_seconds":INTERVAL.as_secs(),"heartbeat_seconds":HEARTBEAT.as_secs(),"workers":1})
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.request_stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerus_desktop_migration::canonical;
    use zerus_presence_sampler::{Idle, IdleSource};

    fn fixture() -> (tempfile::TempDir, Location) {
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: canonical(&temp.path().join("state/desktop")).unwrap(),
            configuration: "a".repeat(64),
        };
        (temp, location)
    }

    fn drain(runtime: &mut Runtime) {
        let end = Instant::now() + Duration::from_secs(5);
        while !runtime.stop().unwrap() {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn writer_fence_survives_sampling_and_releases_only_after_join() {
        let (_temp, location) = fixture();
        let (entered, observed) = mpsc::sync_channel(1);
        let (release, blocked) = mpsc::sync_channel(1);
        let mut runtime = Runtime::spawn(&location, INTERVAL, move || {
            move || {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                Sample {
                    idle: Some(Idle {
                        elapsed: Duration::ZERO,
                        source: IdleSource::Mutter,
                        since_input: true,
                    }),
                    locked: Some(false),
                }
            }
        })
        .unwrap();
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(presence_admission::fence(&location).is_err());
        assert!(!runtime.stop().unwrap());
        assert!(presence_admission::fence(&location).is_err());
        release.send(()).unwrap();
        drain(&mut runtime);
        assert!(presence_admission::fence(&location).is_ok());
        assert!(
            presence::listing(location.root.parent().unwrap(), presence::now()).is_none(),
            "cancellation during sample must not publish it"
        );
    }

    #[test]
    fn worker_publishes_without_actor_polling_then_stops() {
        let (_temp, location) = fixture();
        let mut runtime = Runtime::spawn(&location, INTERVAL, || {
            || Sample {
                idle: Some(Idle {
                    elapsed: Duration::from_secs(7),
                    source: IdleSource::Mutter,
                    since_input: true,
                }),
                locked: Some(true),
            }
        })
        .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while runtime.status()["ready"] != true {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
        let value = presence::listing(location.root.parent().unwrap(), presence::now()).unwrap();
        assert_eq!(value["locked"], true);
        assert!(value["idle_seconds"].as_f64().unwrap() >= 7.0);
        drain(&mut runtime);
        assert_eq!(runtime.status()["drained"], true);
    }

    #[test]
    fn failed_write_keeps_the_fence_until_shutdown() {
        let (_temp, location) = fixture();
        crate::platform::private_dir(location.root.parent().unwrap()).unwrap();
        let leaf = location
            .root
            .parent()
            .unwrap()
            .join("desktop-presence.json");
        std::fs::create_dir(&leaf).unwrap();
        let mut runtime = Runtime::spawn(&location, INTERVAL, || {
            || Sample {
                idle: Some(Idle {
                    elapsed: Duration::ZERO,
                    source: IdleSource::Mutter,
                    since_input: true,
                }),
                locked: Some(false),
            }
        })
        .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while runtime.status()["code"] != "write_failed" {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(presence_admission::fence(&location).is_err());
        assert!(
            leaf.is_dir(),
            "a failed write must not repair the obstructing object"
        );
        drain(&mut runtime);
        assert!(presence_admission::fence(&location).is_ok());
    }

    #[test]
    fn worker_panic_is_an_error_instead_of_a_drained_receipt() {
        let (_temp, location) = fixture();
        let mut runtime =
            Runtime::spawn(&location, INTERVAL, || || panic!("fixture sampler failure")).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while !runtime.worker.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(runtime.status()["code"], "worker_failed");
        assert!(runtime.stop().is_err());
        assert!(
            runtime.stop().is_err(),
            "later polls cannot clear a failed join"
        );
        assert_eq!(runtime.status()["drained"], false);
    }

    #[test]
    fn worker_samples_often_but_writes_on_heartbeat_lock_or_return_from_away() {
        use std::sync::atomic::AtomicU64;
        let (_temp, location) = fixture();
        let clock = Arc::new(AtomicU64::new(0));
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (send, receive) = mpsc::sync_channel::<(u64, Sample)>(1);
        let (ready, tick) = mpsc::sync_channel(1);
        let sampler_clock = clock.clone();
        let write_clock = clock.clone();
        let write_log = writes.clone();
        let base = Instant::now();
        let mut runtime = Runtime::spawn_with(
            &location,
            Duration::ZERO,
            move || {
                move || {
                    // Reaching the next sample proves the preceding write decision and
                    // status publication have finished, without sleeping for 30 s.
                    let _ = ready.send(());
                    let Ok((at, sample)) = receive.recv() else {
                        return Sample::default();
                    };
                    sampler_clock.store(at, Ordering::SeqCst);
                    sample
                }
            },
            move |sample| {
                write_log
                    .lock()
                    .unwrap()
                    .push((write_clock.load(Ordering::SeqCst), sample.locked.unwrap()));
                Ok(true)
            },
            move || base + Duration::from_secs(clock.load(Ordering::SeqCst)),
        )
        .unwrap();
        tick.recv_timeout(Duration::from_secs(5)).unwrap();
        let step = |at, idle, locked| {
            send.send((
                at,
                Sample {
                    idle: Some(Idle {
                        elapsed: Duration::from_secs(idle),
                        source: IdleSource::Mutter,
                        since_input: true,
                    }),
                    locked: Some(locked),
                },
            ))
            .unwrap();
            tick.recv_timeout(Duration::from_secs(5)).unwrap();
        };
        step(0, 0, false);
        for at in (2..30).step_by(2) {
            step(at, 0, false);
        }
        assert_eq!(
            *writes.lock().unwrap(),
            vec![(0, false)],
            "steady active input does not fsync on every sample"
        );
        step(30, 0, false);
        assert_eq!(*writes.lock().unwrap(), vec![(0, false), (30, false)]);
        step(32, 0, true);
        step(34, 2, true);
        assert_eq!(
            *writes.lock().unwrap(),
            vec![(0, false), (30, false), (32, true)],
            "lock flip publishes at the very next sample"
        );
        step(36, 121, false); // unlock also publishes immediately
        step(38, 0, false); // return from >=120 s idle, before the heartbeat
        step(40, 0, false);
        assert_eq!(
            *writes.lock().unwrap(),
            vec![
                (0, false),
                (30, false),
                (32, true),
                (36, false),
                (38, false)
            ]
        );
        runtime.request_stop();
        drop(send);
        drain(&mut runtime);
    }
}
