//! Dormant Fleet role barrier. No producer/control command calls it yet.
use super::{Live, Source};
use std::{sync::Arc, time::Instant};
#[cfg(unix)]
use zerus_client_core::desktop::drain::Role;
use zerus_client_core::desktop::drain::{Charge, Closed, Observation};
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    #[cfg(not(unix))]
    Unsupported,
    Replay,
    #[cfg(unix)]
    Closed,
    Pending,
    Unavailable,
}
pub(crate) struct FrozenFleet {
    live: Arc<Live>,
    controls: Closed,
    resources: Closed,
    _barrier: Charge,
}
impl Live {
    pub(crate) fn freeze(self: &Arc<Self>) -> Result<FrozenFleet, Refused> {
        #[cfg(not(unix))]
        {
            Err(Refused::Unsupported)
        }
        #[cfg(unix)]
        {
            if crate::desktop_service::mode() {
                // This dormant legacy barrier has no authority to certify a
                // service subscriber's joined transport and callbacks.
                return Err(Refused::Unavailable);
            }
            let charge = self
                .barrier
                .admit(Role::Fleet)
                .map_err(|_| Refused::Closed)?;
            self.barrier.close();
            let controls = self.controls.close();
            let resources = self.resources.close();
            if let Some(core) = self.core_control.get() {
                core.request_stop();
            }
            Ok(FrozenFleet {
                live: self.clone(),
                controls,
                resources,
                _barrier: charge,
            })
        }
    }
}
impl FrozenFleet {
    /// Off-thread, one-use, non-serializing. A return or timeout never clears
    /// pending helpers, retired stops, router work, queued changes or callbacks.
    #[allow(dead_code)] // Future authenticated profile controller only.
    pub(crate) fn drain_until(self, deadline: Instant) -> Result<(), Refused> {
        match self.controls.wait_until(deadline) {
            Observation::Idle => (),
            Observation::Pending => return Err(Refused::Pending),
            Observation::Unavailable => return Err(Refused::Unavailable),
        }
        let (source, healthy) = {
            let mut source = self
                .live
                .source
                .try_lock()
                .map_err(|_| Refused::Unavailable)?;
            if matches!(&*source, Some(Source::Replay(_))) {
                return Err(Refused::Replay);
            }
            let healthy = match &*source {
                Some(Source::Fleet(fleet)) => fleet.accounting_available(),
                _ => true,
            };
            (source.take(), healthy)
        };
        if let Some(Source::Fleet(fleet)) = source {
            fleet.stop();
        }
        if !healthy {
            return Err(Refused::Unavailable);
        }
        if Instant::now() >= deadline {
            return Err(Refused::Pending);
        }
        match self.resources.wait_until(deadline) {
            Observation::Idle => Ok(()),
            Observation::Pending => Err(Refused::Pending),
            Observation::Unavailable => Err(Refused::Unavailable),
        }
    }
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, not(unix)))]
#[test]
fn unsupported_freeze_changes_no_admissions() {
    let live = Arc::new(Live::default());
    assert!(matches!(live.freeze(), Err(Refused::Unsupported)));
    assert!(!live.controls.snapshot().unwrap().closed);
    assert!(!live.resources.snapshot().unwrap().closed);
    assert!(!live.barrier.snapshot().unwrap().closed);
}
