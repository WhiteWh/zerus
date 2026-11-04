//! Native callback lifetime and admission, separate from the eight effect slots.
use std::sync::{
    atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    mpsc, Arc,
};
use std::thread::Thread;

pub(super) const PREFIX: &str = "hgs-desktop:";
// Three Toast delegates per retained mapping, one class factory, and the
// existing in-flight bound for independently retained COM activation objects.
const NATIVE_OBJECTS: usize = super::wire::MAPPINGS * 3 + super::wire::IN_FLIGHT + 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Candidate {
    Clicked(String),
    Closed(String),
}

struct Shared {
    phase: AtomicU8,
    retained: AtomicUsize,
    overflow: AtomicBool,
    wake: Thread,
    // The original Runtime's ownership charge, not one of its eight command
    // charges. A native reference survives even an abandoned Rust handle.
    _resources: Arc<dyn Send + Sync>,
}
#[derive(Clone)]
pub(super) struct Slot {
    shared: Arc<Shared>,
    sender: mpsc::SyncSender<Candidate>,
}

/// Captured by each actual native delegate/class object. Copies share the same
/// charge, released only after the last copy (including an executing callback).
#[derive(Clone)]
pub(super) struct Retained(Arc<Charge>);
struct Charge(Arc<Shared>);
impl Drop for Charge {
    fn drop(&mut self) {
        self.0.retained.fetch_sub(1, Ordering::AcqRel);
        self.0.wake.unpark();
    }
}

impl Slot {
    pub fn new(wake: Thread, resources: Arc<dyn Send + Sync>) -> (Self, mpsc::Receiver<Candidate>) {
        let (sender, receiver) = mpsc::sync_channel(super::super::EVENTS);
        (
            Self {
                shared: Arc::new(Shared {
                    phase: AtomicU8::new(0),
                    retained: AtomicUsize::new(0),
                    overflow: AtomicBool::new(false),
                    wake,
                    _resources: resources,
                }),
                sender,
            },
            receiver,
        )
    }
    pub fn finish_restoration(&self) -> bool {
        self.shared
            .phase
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    pub fn accepting_activations(&self) -> bool {
        self.shared.phase.load(Ordering::Acquire) == 1
    }
    pub fn request_stop(&self) {
        self.shared.phase.store(2, Ordering::Release);
        self.shared.wake.unpark();
    }
    pub fn retained(&self) -> usize {
        self.shared.retained.load(Ordering::Acquire)
    }
    pub fn overflow(&self) -> bool {
        self.shared.overflow.load(Ordering::Acquire)
    }
    pub fn hold(&self) -> Option<Retained> {
        self.shared
            .retained
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < NATIVE_OBJECTS).then_some(count + 1)
            })
            .ok()?;
        Some(Retained(Arc::new(Charge(self.shared.clone()))))
    }
    pub fn send(&self, value: Candidate) -> bool {
        let phase = self.shared.phase.load(Ordering::Acquire);
        if phase != 1 && !(phase == 2 && matches!(value, Candidate::Closed(_))) {
            return false;
        }
        // Successful enqueue is only bounded correlation delivery. The runtime
        // still joins it to its original known Delivered mapping and Endpoint.
        // Windows delegates cannot safely block/backpressure a native callback.
        // Overflow of this user-paced queue is deliberately fatal to this host;
        // it must not silently lose an activation or Closed edge.
        if self.sender.try_send(value).is_err() {
            self.shared.overflow.store(true, Ordering::Release);
            self.shared.wake.unpark();
            return false;
        }
        self.shared.wake.unpark();
        true
    }
    pub fn activate(&self, application: &str, arguments: &str, input_count: u32) -> bool {
        if application != super::wire::APPLICATION || input_count != 0 {
            return false;
        }
        let Some(request_id) = arguments.strip_prefix(PREFIX) else {
            return false;
        };
        if !zerus_desktop_migration::canonical_uuid(request_id) {
            return false;
        }
        self.send(Candidate::Clicked(request_id.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_callback_admission_waits_for_restore_and_closes_before_unregister() {
        let (slot, receive) = Slot::new(std::thread::current(), Arc::new(()));
        let request = uuid::Uuid::new_v4().to_string();
        let args = format!("{PREFIX}{request}");
        assert!(!slot.activate(super::super::wire::APPLICATION, &args, 0));
        assert!(slot.finish_restoration());
        assert!(!slot.activate("other.application", &args, 0));
        assert!(!slot.activate(super::super::wire::APPLICATION, "hgs-desktop:invalid", 0));
        assert!(!slot.activate(super::super::wire::APPLICATION, &args, 1));
        assert!(receive.try_recv().is_err());
        assert!(slot.activate(super::super::wire::APPLICATION, &args, 0));
        assert_eq!(
            receive.try_recv().unwrap(),
            Candidate::Clicked(request.clone())
        );
        slot.request_stop();
        assert!(!slot.finish_restoration());
        assert!(!slot.activate(super::super::wire::APPLICATION, &args, 0));
        assert!(slot.send(Candidate::Closed(request.clone())));
        assert_eq!(receive.try_recv().unwrap(), Candidate::Closed(request));
    }
    #[test]
    fn callbacks_and_copies_hold_the_native_resource_until_actual_return() {
        let (slot, _) = Slot::new(std::thread::current(), Arc::new(()));
        let factory = slot.hold().unwrap();
        let executing = factory.clone();
        assert_eq!(Arc::strong_count(&factory.0), 2);
        slot.request_stop();
        drop(factory);
        assert_eq!(slot.retained(), 1);
        drop(executing);
        assert_eq!(slot.retained(), 0);
    }
    #[test]
    fn callback_queue_is_bounded_and_overflow_is_sticky() {
        let (slot, receive) = Slot::new(std::thread::current(), Arc::new(()));
        slot.finish_restoration();
        for _ in 0..super::super::super::EVENTS {
            assert!(slot.send(Candidate::Clicked(uuid::Uuid::new_v4().to_string())));
        }
        assert!(!slot.send(Candidate::Clicked(uuid::Uuid::new_v4().to_string())));
        assert!(slot.overflow());
        receive.try_recv().unwrap();
        assert!(slot.overflow());
    }

    #[test]
    fn an_outstanding_native_object_retains_the_runtime_ownership_charge() {
        struct Ownership(Arc<AtomicBool>);
        impl Drop for Ownership {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let released = Arc::new(AtomicBool::new(false));
        let (slot, _) = Slot::new(
            std::thread::current(),
            Arc::new(Ownership(released.clone())),
        );
        let native = slot.hold().unwrap();
        slot.request_stop();
        drop(slot);
        assert!(!released.load(Ordering::Acquire));
        drop(native);
        assert!(released.load(Ordering::Acquire));
    }
}
