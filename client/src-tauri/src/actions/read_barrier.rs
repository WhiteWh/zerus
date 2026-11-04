//! The ReadDelivery store's one-way barrier. The native service-mode importer
//! retains this opaque handle through checkpoint/capture and owned helper I/O.
use super::Actions;
use std::{
    sync::{Arc, Mutex, MutexGuard, TryLockError},
    time::{Duration, Instant},
};
use zerus_client_core::{
    desktop::drain::{Charge, Closed, Observation, Role},
    readmarks::{Checkpoint, ReadMarks},
};

pub(super) const MARKS_CLOSED: &str = "Read marks are not accepting changes.";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    #[cfg(not(any(unix, windows)))]
    Unsupported,
    #[cfg(any(unix, windows))]
    Closed,
    Pending,
    Unavailable,
    Checkpoint,
}

/// Original one-way writer barrier, not a handoff or capture permit. The
/// charge survives timeout and is retained through capture retries.
pub(crate) struct FrozenReads {
    marks: Arc<Mutex<ReadMarks>>,
    mutations: Closed,
    native: Closed,
    _checkpoint: Charge,
}

pub(crate) struct CapturedReads {
    checkpoint: Checkpoint,
    pub capture: zerus_client_core::desktop::pending_reads::Capture,
    // Drop the Windows file/parent pins before releasing the original barrier
    // charge. An idle observation must not run ahead of those handle closes.
    frozen: Arc<FrozenReads>,
}
impl CapturedReads {
    pub fn unchanged(&self, deadline: Instant) -> Result<(), Refused> {
        let marks = lock_marks(&self.frozen.marks, deadline)?;
        marks
            .checkpoint_bytes(&self.checkpoint)
            .map(|_| ())
            .map_err(|_| Refused::Checkpoint)?;
        if Instant::now() >= deadline {
            return Err(Refused::Pending);
        }
        Ok(())
    }
}

/// Read-only overlays do not enter the writer barrier. Wait for their short
/// store borrows, using the same absolute deadline as the checkpoint/capture.
/// Poison is unavailable; timeout never recovers or reopens the store.
fn lock_marks(
    marks: &Mutex<ReadMarks>,
    deadline: Instant,
) -> Result<MutexGuard<'_, ReadMarks>, Refused> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(Refused::Pending);
        }
        match marks.try_lock() {
            Ok(guard) => {
                return if Instant::now() < deadline {
                    Ok(guard)
                } else {
                    Err(Refused::Pending)
                }
            }
            Err(TryLockError::Poisoned(_)) => return Err(Refused::Unavailable),
            Err(TryLockError::WouldBlock) => std::thread::sleep(
                deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(5)),
            ),
        }
    }
}
fn sharing_violation(error: &std::io::Error) -> bool {
    cfg!(windows) && error.raw_os_error() == Some(32)
}
impl Actions {
    pub(super) fn admit_marks(&self) -> Result<Charge, String> {
        self.mark_admissions
            .admit(Role::ReadDelivery)
            .map_err(|_| MARKS_CLOSED.into())
    }

    /// Close every mark writer before cancelling native calls. A failed wait
    /// never reopens anything. Unsupported platforms refuse before any close.
    pub(crate) fn freeze_reads(&self) -> Result<FrozenReads, Refused> {
        #[cfg(not(any(unix, windows)))]
        {
            Err(Refused::Unsupported)
        }
        #[cfg(any(unix, windows))]
        {
            let charge = self
                .checkpoint_admission
                .admit(Role::ReadDelivery)
                .map_err(|error| match error {
                    zerus_client_core::desktop::drain::Refused::Closed => Refused::Closed,
                    _ => Refused::Unavailable,
                })?;
            self.checkpoint_admission.close();
            let mutations = self.mark_admissions.close();
            let native = self.read_delivery.stop();
            Ok(FrozenReads {
                marks: self.marks.clone(),
                mutations,
                native,
                _checkpoint: charge,
            })
        }
    }
}
impl FrozenReads {
    // Keep the caller's original freeze handle on every refusal. It can retry
    // after a read-only borrow/deadline without a second freeze or new source.
    pub(crate) fn capture(self: &Arc<Self>, deadline: Instant) -> Result<CapturedReads, Refused> {
        let checkpoint = self.checkpoint_inner(deadline, ReadMarks::checkpoint)?;
        let bytes = lock_marks(&self.marks, deadline)?
            .checkpoint_bytes(&checkpoint)
            .map_err(|_| Refused::Checkpoint)?;
        let capture = zerus_client_core::desktop::pending_reads::Capture::from_bytes(&bytes)
            .map_err(|_| Refused::Checkpoint)?;
        if Instant::now() >= deadline {
            return Err(Refused::Pending);
        }
        Ok(CapturedReads {
            frozen: self.clone(),
            checkpoint,
            capture,
        })
    }
    /// Blocking, off the GUI thread. The same absolute deadline bounds both
    /// drains. Filesystem I/O may outlive it; self retains the barrier charge
    /// throughout, and a late completed sync still cannot return success.
    #[allow(dead_code)] // Checkpoint-only callers keep the original consuming API.
    pub(crate) fn checkpoint(self, deadline: Instant) -> Result<Checkpoint, Refused> {
        self.checkpoint_with(deadline, ReadMarks::checkpoint)
    }
    fn checkpoint_with(
        self,
        deadline: Instant,
        sync: impl FnMut(&mut ReadMarks) -> std::io::Result<Checkpoint>,
    ) -> Result<Checkpoint, Refused> {
        self.checkpoint_inner(deadline, sync)
    }
    fn checkpoint_inner(
        &self,
        deadline: Instant,
        mut sync: impl FnMut(&mut ReadMarks) -> std::io::Result<Checkpoint>,
    ) -> Result<Checkpoint, Refused> {
        for closed in [&self.mutations, &self.native] {
            match closed.wait_until(deadline) {
                Observation::Idle => (),
                Observation::Pending => return Err(Refused::Pending),
                Observation::Unavailable => return Err(Refused::Unavailable),
            }
        }
        if Instant::now() >= deadline {
            return Err(Refused::Pending);
        }
        // Read-only snapshots can hold the store too. Never block beyond the
        // drain wait or recover a poisoned store into a successful checkpoint.
        let mut sharing_retries = 0;
        loop {
            let result = {
                let mut marks = lock_marks(&self.marks, deadline)?;
                sync(&mut marks)
            };
            if Instant::now() >= deadline {
                return Err(Refused::Pending);
            }
            match result {
                Ok(checkpoint) => return Ok(checkpoint),
                Err(error) => {
                    if sharing_violation(&error) && sharing_retries < 20 {
                        // Brief antivirus/indexer contention only. Release the
                        // store borrow while sleeping; never reset the original
                        // capture deadline or retry an unknown publication.
                        sharing_retries += 1;
                        std::thread::sleep(
                            deadline
                                .saturating_duration_since(Instant::now())
                                .min(Duration::from_millis(10)),
                        );
                        continue;
                    }
                    return Err(Refused::Checkpoint);
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, not(any(unix, windows))))]
#[test]
fn unsupported_freeze_changes_no_admission() {
    let actions = Actions::new(
        std::env::temp_dir().join(format!("read-barrier-{}.json", uuid::Uuid::new_v4())),
    );
    assert!(matches!(actions.freeze_reads(), Err(Refused::Unsupported)));
    assert!(!actions.mark_admissions.snapshot().unwrap().closed);
    assert!(!actions.checkpoint_admission.snapshot().unwrap().closed);
    assert!(actions.read_delivery.admit().is_some());
}

#[cfg(all(test, windows))]
mod windows_tests;
