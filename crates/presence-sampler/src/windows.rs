//! Current interactive Windows session only; no input hook or event contents.
use super::{Idle, IdleSource, Sample, MAX_IDLE};
use std::time::Duration;

const TICK_WRAP: u64 = 1 << 32;

#[derive(Clone, Copy)]
struct Session {
    id: u32,
    active: bool,
    locked: Option<bool>,
}

#[derive(Clone, Copy)]
struct Observation {
    session: u32,
    now: u64,
    last: u32,
    origin: Option<(u64, bool)>,
}

#[derive(Default)]
pub(super) struct WindowsSampler {
    previous: Option<Observation>,
}
impl WindowsSampler {
    fn observe(&mut self, session: Option<Session>, ticks: Option<(u64, u32)>) -> Sample {
        let Some(session) = session.filter(|s| s.id != 0 && s.active) else {
            self.previous = None;
            return Sample::default();
        };
        let idle = ticks.and_then(|(now, last)| self.idle(session.id, now, last));
        if ticks.is_none() {
            self.previous = None;
        }
        Sample {
            idle,
            locked: session.locked,
        }
    }

    fn idle(&mut self, session: u32, now: u64, last: u32) -> Option<Idle> {
        let elapsed = u64::from((now as u32).wrapping_sub(last));
        let origin = match self.previous.filter(|p| p.session == session) {
            Some(p) if now < p.now => None,
            Some(p) if p.last == last => p.origin,
            // GetLastInputInfo can regress (including SendInput timestamps).
            // A changed DWORD is recent-input evidence only inside the interval
            // since the preceding observation, before another DWORD wrap.
            Some(p) if now - p.now < TICK_WRAP && elapsed <= now - p.now => {
                now.checked_sub(elapsed).map(|at| (at, last != 0))
            }
            Some(_) => None,
            // Before the first wrap the OS timestamp has an unambiguous epoch.
            // After it, the initial value is only an idle lower bound: a small
            // DWORD must never turn an ancient input into recent presence.
            None => now
                .checked_sub(elapsed)
                .map(|at| (at, now < TICK_WRAP && last != 0)),
        };
        self.previous = Some(Observation {
            session,
            now,
            last,
            origin,
        });
        let (at, since_input) = origin?;
        let elapsed = Duration::from_millis(now.checked_sub(at)?);
        (elapsed <= MAX_IDLE).then_some(Idle {
            elapsed,
            source: IdleSource::WindowsLastInput,
            since_input,
        })
    }

    #[cfg(windows)]
    pub(super) fn sample(&mut self) -> Sample {
        let session = os::session();
        let ticks = session
            .filter(|s| s.id != 0 && s.active)
            .and_then(|_| os::ticks());
        self.observe(session, ticks)
    }
}

#[cfg(windows)]
mod os {
    use super::Session;
    use std::{mem::size_of, ptr};
    use windows_sys::Win32::{
        System::{
            RemoteDesktop::{
                ProcessIdToSessionId, WTSActive, WTSFreeMemory, WTSQuerySessionInformationW,
                WTSSessionInfoEx, WTSINFOEXW, WTS_CURRENT_SERVER_HANDLE,
            },
            SystemInformation::GetTickCount64,
            Threading::GetCurrentProcessId,
        },
        UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
    };

    struct Buffer(*mut u16);
    impl Drop for Buffer {
        fn drop(&mut self) {
            // SAFETY: WTSQuerySessionInformationW returned this allocation.
            if !self.0.is_null() {
                unsafe { WTSFreeMemory(self.0.cast()) };
            }
        }
    }

    pub(super) fn session() -> Option<Session> {
        let mut id = 0;
        // SAFETY: a process-local query with a valid output pointer.
        if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id) } == 0 || id == 0 {
            return None;
        }
        let (mut data, mut bytes) = (ptr::null_mut(), 0);
        // SAFETY: local server, this process's session and valid output pointers.
        if unsafe {
            WTSQuerySessionInformationW(
                WTS_CURRENT_SERVER_HANDLE,
                id,
                WTSSessionInfoEx,
                &mut data,
                &mut bytes,
            )
        } == 0
        {
            return None;
        }
        let data = Buffer(data);
        if data.0.is_null() || bytes < size_of::<WTSINFOEXW>() as u32 || bytes > 4096 {
            return None;
        }
        // SAFETY: checked allocation size; copy the fixed structure before free.
        let info = unsafe { ptr::read_unaligned(data.0.cast::<WTSINFOEXW>()) };
        if info.Level != 1 {
            return None;
        }
        // SAFETY: the API identifies the level-1 union member above.
        let info = unsafe { info.Data.WTSInfoExLevel1 };
        if info.SessionId != id {
            return None;
        }
        Some(Session {
            id,
            active: info.SessionState == WTSActive,
            // Windows 10/11 semantics. Windows 7's reversed flags are outside
            // the supported desktop runtime, never a fallback here.
            locked: match info.SessionFlags {
                0 => Some(true),
                1 => Some(false),
                _ => None,
            },
        })
    }

    pub(super) fn ticks() -> Option<(u64, u32)> {
        let mut input = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: fixed structure with the API-required byte size; current
        // session only. Read uptime after the input query to avoid a future cut.
        (unsafe { GetLastInputInfo(&mut input) } != 0)
            .then(|| (unsafe { GetTickCount64() }, input.dwTime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> Option<Session> {
        Some(Session {
            id: 1,
            active: true,
            locked: Some(false),
        })
    }
    fn idle(s: &mut WindowsSampler, now: u64, last: u32) -> Idle {
        s.observe(session(), Some((now, last))).idle.unwrap()
    }
    #[test]
    fn session_zero_disconnected_and_unavailable_never_establish_presence() {
        for context in [
            None,
            Some(Session {
                id: 0,
                active: true,
                locked: Some(false),
            }),
            Some(Session {
                id: 1,
                active: false,
                locked: Some(false),
            }),
        ] {
            assert_eq!(
                WindowsSampler::default().observe(context, Some((100, 99))),
                Sample::default()
            );
        }
    }
    #[test]
    fn missing_input_preserves_lock_but_not_cached_idle() {
        let mut s = WindowsSampler::default();
        idle(&mut s, 100, 90);
        let sample = s.observe(session(), None);
        assert_eq!(sample.idle, None);
        assert_eq!(sample.locked, Some(false));
        let sample = s.observe(
            Some(Session {
                locked: None,
                ..session().unwrap()
            }),
            Some((110, 90)),
        );
        assert_eq!(sample.locked, None);
        assert_eq!(sample.idle.unwrap().elapsed, Duration::from_millis(20));
    }
    #[test]
    fn unchanged_input_never_renews_the_timestamp() {
        let mut s = WindowsSampler::default();
        assert_eq!(idle(&mut s, 100, 90).elapsed, Duration::from_millis(10));
        assert_eq!(idle(&mut s, 130, 90).elapsed, Duration::from_millis(40));
        assert!(idle(&mut s, 140, 90).since_input);
    }
    #[test]
    fn first_sample_after_wrap_is_only_a_lower_bound_until_new_input() {
        let mut s = WindowsSampler::default();
        assert!(!idle(&mut s, TICK_WRAP + 100, 90).since_input);
        assert_eq!(
            idle(&mut s, 2 * TICK_WRAP + 110, 90).elapsed,
            Duration::from_millis(TICK_WRAP + 20)
        );
        let observed = idle(&mut s, 2 * TICK_WRAP + 120, 119);
        assert!(observed.since_input);
        assert_eq!(observed.elapsed, Duration::from_millis(1));
    }
    #[test]
    fn known_input_age_survives_tick_wrap() {
        let mut s = WindowsSampler::default();
        idle(&mut s, TICK_WRAP - 2, u32::MAX - 5);
        let sample = idle(&mut s, TICK_WRAP + 4, u32::MAX - 5);
        assert!(sample.since_input);
        assert_eq!(sample.elapsed, Duration::from_millis(10));
    }
    #[test]
    fn future_or_regressing_input_and_clock_reset_remain_unknown() {
        let mut s = WindowsSampler::default();
        assert!(s.observe(session(), Some((100, 101))).idle.is_none());
        idle(&mut s, 110, 109);
        assert!(s.observe(session(), Some((120, 50))).idle.is_none());
        assert!(s.observe(session(), Some((121, 50))).idle.is_none());
        assert!(s.observe(session(), Some((119, 118))).idle.is_none());
        assert!(idle(&mut s, 130, 129).since_input);
    }
    #[test]
    fn reconnect_and_zero_input_never_synthesize_recent_input() {
        let mut s = WindowsSampler::default();
        assert!(!idle(&mut s, 100, 0).since_input);
        idle(&mut s, 110, 109);
        s.observe(None, None);
        assert!(!idle(&mut s, TICK_WRAP + 120, 109).since_input);
        let other = Some(Session {
            id: 2,
            ..session().unwrap()
        });
        assert!(
            !s.observe(other, Some((TICK_WRAP + 130, 109)))
                .idle
                .unwrap()
                .since_input
        );
    }
    #[test]
    fn invalid_os_duration_does_not_escape_the_contract_bound() {
        let mut s = WindowsSampler::default();
        idle(&mut s, 10, 1);
        assert!(s
            .observe(session(), Some((MAX_IDLE.as_millis() as u64 + 100, 1)))
            .idle
            .is_none());
    }
    #[cfg(windows)]
    #[test]
    fn actual_current_session_queries_are_optional_and_never_default_session_zero() {
        let mut s = WindowsSampler::default();
        let session = os::session();
        let sample = s.observe(
            session,
            session
                .filter(|s| s.id != 0 && s.active)
                .and_then(|_| os::ticks()),
        );
        if session.is_none_or(|session| session.id == 0 || !session.active) {
            assert_eq!(sample, Sample::default());
        }
        if let Some(idle) = sample.idle {
            assert!(idle.elapsed <= MAX_IDLE);
        }
    }
}
