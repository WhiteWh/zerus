//! Reusable desktop presence inputs. This crate never writes presence, starts a
//! service, or changes idle inhibitors, the screen lock, or power policy.
//!
//! Keep one [`Sampler`] and call [`Sampler::sample`] on a background thread at
//! least once every five seconds. Linux D-Bus calls have a 500 ms reply timeout;
//! connection setup is ordinary local IPC, not a hard real-time operation.
//! Missing inputs stay unknown independently. A caller must not turn an unknown
//! lock into `false` or an unknown idle duration into zero.
//!
//! Wayland reports a conservative idle lower bound, starting only after an
//! input-idle event. Starting/reconnecting the sampler is never user activity.
//! No application/service integration or presence publication is selected here.

use std::time::Duration;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, test))]
mod windows;

/// The distinction matters when interpreting a small idle duration: a Wayland
/// lower bound is not proof that input happened recently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleSource {
    WaylandInputLowerBound,
    Mutter,
    KdeScreenSaver,
    MacEventSource,
    WindowsLastInput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Idle {
    pub elapsed: Duration,
    pub source: IdleSource,
    /// True only when this duration follows observed input (or an OS query of
    /// the actual last input). False for a lower bound measured from monitor
    /// startup/reconnect. A short false value must never establish presence;
    /// a sufficiently long lower bound can still establish absence.
    pub since_input: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sample {
    pub idle: Option<Idle>,
    pub locked: Option<bool>,
}

/// Platform resources are lazy: constructing a sampler performs no IPC.
#[derive(Default)]
pub struct Sampler {
    #[cfg(target_os = "linux")]
    linux: linux::LinuxSampler,
    #[cfg(windows)]
    windows: windows::WindowsSampler,
}

impl Sampler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sample(&mut self) -> Sample {
        #[cfg(target_os = "linux")]
        return self.linux.sample();
        #[cfg(target_os = "macos")]
        return macos::sample();
        #[cfg(windows)]
        return self.windows.sample();
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        Sample::default()
    }
}

// Also bounds invalid OS values before they can reach the existing hgs presence
// input contract. This is a validity check, not an away threshold.
#[cfg(any(target_os = "linux", target_os = "macos", windows, test))]
const MAX_IDLE: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

#[cfg(any(target_os = "macos", test))]
fn seconds(value: f64) -> Option<Duration> {
    if !value.is_finite() || value < 0.0 || value > MAX_IDLE.as_secs_f64() {
        return None;
    }
    Duration::try_from_secs_f64(value).ok()
}

#[cfg(any(target_os = "macos", test))]
fn session_locked(locked: Option<bool>, on_console: Option<bool>) -> Option<bool> {
    match (locked, on_console) {
        (Some(true), _) | (_, Some(false)) => Some(true),
        (Some(false), Some(true)) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_idle_is_unknown_instead_of_fresh_activity() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 1e12] {
            assert_eq!(seconds(bad), None);
        }
        assert_eq!(seconds(0.0), Some(Duration::ZERO));
        assert_eq!(seconds(1.25), Some(Duration::from_millis(1250)));
        assert_eq!(seconds(MAX_IDLE.as_secs_f64()), Some(MAX_IDLE));
    }

    #[test]
    fn lock_and_console_are_independent_evidence() {
        assert_eq!(session_locked(Some(true), None), Some(true));
        assert_eq!(session_locked(None, Some(false)), Some(true));
        assert_eq!(session_locked(Some(false), Some(true)), Some(false));
        assert_eq!(session_locked(None, Some(true)), None);
        assert_eq!(session_locked(Some(false), None), None);
        assert_eq!(session_locked(None, None), None);
    }

    #[test]
    fn missing_inputs_are_never_defaulted_to_present_and_unlocked() {
        assert_eq!(
            Sample::default(),
            Sample {
                idle: None,
                locked: None
            }
        );
        let sample = Sample {
            locked: Some(true),
            ..Sample::default()
        };
        assert_eq!(sample.idle, None);
        assert_eq!(sample.locked, Some(true));
    }
}
