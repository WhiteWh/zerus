//! Four fixed resource slots. A returned call cannot release retained I/O.
use super::{lock, Arc, Charge, Mutex};
use std::sync::OnceLock;
use zerus_client_core::desktop::producer::ReadDelivery;

pub(super) type Observe = Arc<dyn Fn(ReadDelivery, bool) + Send + Sync>;
#[derive(Clone)]
pub(super) struct Pool(Arc<Inner>);
struct Inner {
    state: Mutex<State>,
    published: Mutex<u64>,
    observe: OnceLock<Observe>,
}
struct State {
    slots: [Option<(u64, bool)>; 4],
    next: u64,
    revision: u64,
    stopped: bool,
    notified: bool,
}
pub(super) struct Resource {
    pool: Pool,
    pub slot: usize,
    generation: u64,
    _charge: Charge,
}
impl State {
    fn health(&mut self, observed: bool) -> (ReadDelivery, bool) {
        self.revision = self.revision.saturating_add(1);
        let pinned = self
            .slots
            .iter()
            .flatten()
            .filter(|(_, pinned)| *pinned)
            .count() as u8;
        let notify = observed && pinned == 4 && !self.notified;
        self.notified |= notify;
        (
            ReadDelivery {
                revision: self.revision,
                capacity: 4,
                in_flight: self.slots.iter().flatten().count() as u8,
                pinned,
                stopped: self.stopped,
            },
            notify,
        )
    }
}
impl Pool {
    pub fn new() -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                slots: [None; 4],
                next: 0,
                revision: 0,
                stopped: false,
                notified: false,
            }),
            published: Mutex::new(0),
            observe: OnceLock::new(),
        }))
    }
    /// The shared owner exists before app setup. Install its observer once,
    /// publishing current counts (including any calls admitted before setup).
    pub fn observe(&self, observe: Observe) {
        let mut revision = lock(&self.0.published);
        if self.0.observe.set(observe).is_ok() {
            let (health, notify) = lock(&self.0.state).health(true);
            *revision = health.revision;
            (self.0.observe.get().unwrap())(health, notify);
        }
    }
    fn health(&self, state: &mut State) -> (ReadDelivery, bool) {
        state.health(self.0.observe.get().is_some())
    }
    fn publish(&self, (health, notify): (ReadDelivery, bool)) {
        // Serialize callbacks and discard out-of-order snapshots. The observer
        // updates only cached health and the existing queued notice bus.
        let mut revision = lock(&self.0.published);
        let Some(observe) = self.0.observe.get() else {
            return;
        };
        if health.revision > *revision {
            *revision = health.revision;
            observe(health, notify);
        } else if notify {
            // The last slot really pinned, even if a release raced publication.
            // The responder ignores this stale health; the once-only notice stays.
            observe(health, true);
        }
    }
    pub fn take(&self, charge: Charge) -> Option<Arc<Resource>> {
        let (slot, generation, health) = {
            let mut state = lock(&self.0.state);
            if state.stopped {
                return None;
            }
            let slot = state.slots.iter().position(Option::is_none)?;
            state.next = state.next.checked_add(1)?;
            let generation = state.next;
            state.slots[slot] = Some((generation, false));
            (slot, generation, self.health(&mut state))
        };
        self.publish(health);
        Some(Arc::new(Resource {
            pool: self.clone(),
            slot,
            generation,
            _charge: charge,
        }))
    }
    pub fn finished(&self, resource: &Arc<Resource>) -> bool {
        if Arc::strong_count(resource) <= 1 {
            return false;
        }
        let health = {
            let mut state = lock(&self.0.state);
            let entry = &mut state.slots[resource.slot];
            if *entry != Some((resource.generation, false)) {
                return false;
            }
            *entry = Some((resource.generation, true));
            self.health(&mut state)
        };
        self.publish(health);
        true
    }
    pub fn stop(&self) {
        let health = {
            let mut state = lock(&self.0.state);
            state.stopped = true;
            self.health(&mut state)
        };
        self.publish(health);
    }
}
impl Drop for Resource {
    fn drop(&mut self) {
        let health = {
            let mut state = lock(&self.pool.0.state);
            if state.slots[self.slot].is_none_or(|(id, _)| id != self.generation) {
                return;
            }
            state.slots[self.slot] = None;
            self.pool.health(&mut state)
        };
        self.pool.publish(health);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use zerus_client_core::desktop::drain::{Gate, Observation, Role};
    #[test]
    fn retained_resources_pin_four_slots_and_notify_once_without_freeing_them() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let saved = events.clone();
        let pool = Pool::new();
        pool.observe(Arc::new(move |h, n| lock(&saved).push((h, n))));
        let gate = Gate::new(5).unwrap();
        let mut retained = Vec::new();
        for i in 0..4 {
            let call = pool.take(gate.admit(Role::ReadDelivery).unwrap()).unwrap();
            assert_eq!(call.slot, i);
            retained.push(call.clone());
            assert!(pool.finished(&call));
            assert!(!pool.finished(&call), "one transition/log per slot");
        }
        assert!(pool.take(gate.admit(Role::ReadDelivery).unwrap()).is_none());
        assert_eq!(lock(&events).last().unwrap().0.pinned, 4);
        assert_eq!(lock(&events).iter().filter(|(_, n)| *n).count(), 1);
        let closed = gate.close();
        assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
        pool.stop();
        retained.clear();
        assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
        let health = lock(&events).last().unwrap().0;
        assert_eq!(
            (health.in_flight, health.pinned, health.stopped),
            (0, 0, true)
        );
        assert!(pool
            .take(Gate::new(1).unwrap().admit(Role::ReadDelivery).unwrap())
            .is_none());
    }
    #[test]
    fn ordinary_completion_reuses_a_slot_and_older_health_cannot_replace_it() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let saved = events.clone();
        let pool = Pool::new();
        pool.observe(Arc::new(move |h, n| lock(&saved).push((h, n))));
        let gate = Gate::new(4).unwrap();
        let call = pool.take(gate.admit(Role::ReadDelivery).unwrap()).unwrap();
        assert!(!pool.finished(&call));
        let old = lock(&events)[0];
        drop(call);
        let replacement = pool.take(gate.admit(Role::ReadDelivery).unwrap()).unwrap();
        assert_eq!(replacement.slot, 0);
        let count = lock(&events).len();
        pool.publish(old);
        assert_eq!(lock(&events).len(), count);
        assert_eq!(lock(&events).last().unwrap().0.in_flight, 1);
    }

    #[test]
    fn installing_the_observer_late_keeps_counts_and_the_once_only_notice() {
        let pool = Pool::new();
        let gate = Gate::new(4).unwrap();
        let mut retained = Vec::new();
        for _ in 0..4 {
            let call = pool.take(gate.admit(Role::ReadDelivery).unwrap()).unwrap();
            retained.push(call.clone());
            assert!(pool.finished(&call));
        }
        let events = Arc::new(Mutex::new(Vec::new()));
        let saved = events.clone();
        pool.observe(Arc::new(move |h, n| lock(&saved).push((h, n))));
        assert_eq!(lock(&events).last().unwrap().0.pinned, 4);
        assert_eq!(lock(&events).iter().filter(|(_, n)| *n).count(), 1);
        pool.observe(Arc::new(|_, _| panic!("observer replaced")));
        retained.clear();
        assert_eq!(lock(&events).last().unwrap().0.in_flight, 0);
        assert_eq!(lock(&events).iter().filter(|(_, n)| *n).count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn actual_retained_output_pipes_keep_health_pinned_until_eof() {
        let _process_tree = crate::PROCESS_TREE.read().unwrap();
        use std::{path::Path, sync::atomic::AtomicBool, time::Duration};
        use zerus_client_core::{run::run_accounted, Exit, Invocation};
        struct Release(std::path::PathBuf);
        impl Drop for Release {
            fn drop(&mut self) {
                let _ = std::fs::write(self.0.join("release"), []);
                if !std::thread::panicking() {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
        }
        for pipe in ["stdout", "stderr"] {
            let root = std::env::temp_dir().join(format!("read-slot-{}", uuid::Uuid::new_v4()));
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            let release = Release(root);
            let file = release.0.join("release");
            let body = "import os,time\ndeadline=time.monotonic()+10\nwhile not os.path.exists(os.environ['READ_SLOT_RELEASE']) and time.monotonic()<deadline: time.sleep(.01)\n";
            let code = format!("import subprocess,sys\nsubprocess.Popen([sys.executable,'-c',{}],stdin=subprocess.DEVNULL,stdout={},stderr={})\nprint('done')",
                serde_json::to_string(body).unwrap(),
                if pipe == "stdout" {"sys.stdout"} else {"subprocess.DEVNULL"},
                if pipe == "stderr" {"sys.stderr"} else {"subprocess.DEVNULL"});
            let events = Arc::new(Mutex::new(Vec::new()));
            let saved = events.clone();
            let pool = Pool::new();
            pool.observe(Arc::new(move |h, n| lock(&saved).push((h, n))));
            let gate = Gate::new(4).unwrap();
            let call = pool.take(gate.admit(Role::ReadDelivery).unwrap()).unwrap();
            let result = run_accounted(
                Path::new("python3"),
                &Invocation {
                    args: vec!["-c".into(), code],
                    stdin: None,
                    timeout: Duration::from_secs(5),
                    request_id: None,
                },
                &[("READ_SLOT_RELEASE", file.to_str().unwrap())],
                1024,
                &AtomicBool::new(false),
                call.clone(),
            );
            assert!(
                matches!(result, Ok(Exit::Code { code: 0, .. })),
                "{result:?}"
            );
            assert!(pool.finished(&call), "{pipe}");
            drop(call);
            assert_eq!(lock(&events).last().unwrap().0.pinned, 1);
            std::fs::write(file, []).unwrap();
            assert_eq!(
                gate.close()
                    .wait_until(Instant::now() + Duration::from_secs(3)),
                Observation::Idle
            );
            assert_eq!(lock(&events).last().unwrap().0.pinned, 0);
        }
    }
}
