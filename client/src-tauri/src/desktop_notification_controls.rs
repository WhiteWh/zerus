//! Native app-window observations and the separate authenticated control sender.
use std::{
    sync::{Arc, Mutex, PoisonError, Weak},
    time::{Duration, Instant},
};
use zerus_client_core::desktop::notification_control::{
    Command, FocusReply, FocusState, NativeFocus, Scope, FOCUS_REFRESH_MS,
};

const WINDOWS: usize = 64;

pub(crate) use crate::desktop_service::notification_controls::{Controller, Setup};

type Sink = Arc<dyn Fn(FocusState) + Send + Sync>;
type Refresh = Arc<dyn Fn(Source, tokio::sync::watch::Sender<bool>) + Send + Sync>;
#[derive(Default)]
struct SourceState {
    sink: Option<(String, Sink)>,
    refresh: Option<Refresh>,
    refreshing: Option<tokio::sync::watch::Receiver<bool>>,
}
/// One native application source, one current service observer, no event queue.
/// Only Rust window callbacks supply observations; reconnect asks for a NEW
/// native enumeration rather than replaying the old connection's cached focus.
#[derive(Clone, Default)]
pub(crate) struct Source(Arc<Mutex<SourceState>>);
pub(crate) struct Subscription(Weak<Mutex<SourceState>>, String);
pub(crate) struct RefreshPending(tokio::sync::watch::Receiver<bool>);
impl RefreshPending {
    pub async fn wait(mut self) -> bool {
        self.0.wait_for(|complete| *complete).await.is_ok()
    }
}
impl Source {
    pub fn set_refresh(&self, refresh: Refresh) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .refresh = Some(refresh);
    }
    /// Coalesce main-thread enumerations within the current subscription.
    /// Its acknowledgement follows native observation, never mere scheduling.
    pub fn refresh(&self) -> Option<RefreshPending> {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let refresh = state.refresh.clone()?;
        if let Some(pending) = &state.refreshing {
            if !*pending.borrow() && pending.has_changed().is_ok() {
                return Some(RefreshPending(pending.clone()));
            }
        }
        let (complete, pending) = tokio::sync::watch::channel(false);
        state.refreshing = Some(pending.clone());
        drop(state);
        refresh(self.clone(), complete);
        Some(RefreshPending(pending))
    }
    pub fn observe(&self, state: FocusState) {
        let sink = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sink
            .as_ref()
            .map(|(_, sink)| sink.clone());
        if let Some(sink) = sink {
            sink(state);
        }
    }
    pub(crate) fn subscribe(&self, id: String, sink: Sink) -> Subscription {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        // An old callback may have read its windows before this connection.
        // Its eventual completion cannot replace our first fresh observation.
        state.refreshing = None;
        state.sink = Some((id.clone(), sink));
        Subscription(Arc::downgrade(&self.0), id)
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(source) = self.0.upgrade() {
            let mut state = source.lock().unwrap_or_else(PoisonError::into_inner);
            if state.sink.as_ref().is_some_and(|(id, _)| id == &self.1) {
                state.sink = None;
            }
        }
    }
}

pub(crate) fn observe_windows(app: &tauri::AppHandle, source: &Source) {
    use tauri::Manager;
    source.observe(aggregate(app.webview_windows().values().map(|window| {
        window.is_visible().ok().and_then(|visible| {
            if visible {
                window.is_focused().ok()
            } else {
                Some(false)
            }
        })
    })));
}

/// Complete fresh native app-window observations, never webview focus or JS.
/// A known focused window proves Foreground even if another query failed.
/// Empty means an actual completed enumeration with no windows, not disconnect.
pub(super) fn aggregate(windows: impl IntoIterator<Item = Option<bool>>) -> FocusState {
    let mut unknown = false;
    for (index, window) in windows.into_iter().enumerate() {
        if index == WINDOWS {
            return FocusState::Unknown;
        }
        match window {
            Some(true) => return FocusState::Foreground,
            Some(false) => (),
            None => unknown = true,
        }
    }
    if unknown {
        FocusState::Unknown
    } else {
        FocusState::Background
    }
}

/// Scheduling only; the eventual worker must independently retain/admit its
/// original child and every pipe before using this result. This owns no native
/// cache revision: only the Responder may produce/invalidate that evidence.
#[derive(Default)]
pub(super) struct Schedule {
    last: Option<(NativeFocus, Instant)>,
    uncertain: bool,
}
impl Schedule {
    pub fn due(&self, current: &NativeFocus, now: Instant) -> bool {
        if !current.valid() {
            return false;
        }
        let Some((last, at)) = &self.last else {
            return true;
        };
        if current.connection_id != last.connection_id {
            return true;
        }
        if current.revision < last.revision
            || (current.revision == last.revision && current.state != last.state)
        {
            return false;
        }
        if (current.state == FocusState::Foreground && last.state == FocusState::Foreground)
            || (self.uncertain && current == last)
        {
            return now.saturating_duration_since(*at) >= Duration::from_millis(FOCUS_REFRESH_MS);
        }
        current != last
    }
    pub fn attempted(&mut self, observation: NativeFocus, now: Instant) {
        self.last = Some((observation, now));
        self.uncertain = false;
    }
    /// A lost reply needs a fresh observation/UUID, never replay or lookup.
    /// This changes timing only; it does not release the original resource gate.
    pub fn lost_reply(&mut self) {
        self.uncertain = true;
    }
    pub fn disconnected(&mut self) {
        self.last = None;
        self.uncertain = false;
    }
}

/// Echoed UUID/scope alone cannot bind a reply to the actual attached native
/// observation. This check supplies integrity only, never ownership/authority.
pub(super) fn reply_matches(
    reply: &FocusReply,
    command: &Command,
    scope: &Scope,
    attached: &NativeFocus,
) -> bool {
    reply.matches(command, scope) && &reply.observation == attached
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerus_client_core::desktop::notification_control::ABI;

    fn observation(state: FocusState, revision: u64) -> NativeFocus {
        NativeFocus {
            connection_id: "00000000-0000-4000-8000-000000000001".into(),
            revision,
            state,
        }
    }

    #[test]
    fn another_focused_native_window_keeps_app_foreground_and_query_failure_is_unknown() {
        assert_eq!(aggregate([Some(false), Some(true)]), FocusState::Foreground);
        assert_eq!(aggregate([None, Some(true)]), FocusState::Foreground);
        assert_eq!(aggregate([Some(false), None]), FocusState::Unknown);
        assert_eq!(
            aggregate([Some(false), Some(false)]),
            FocusState::Background
        );
        assert_eq!(aggregate([]), FocusState::Background);
        assert_eq!(
            aggregate(std::iter::repeat_n(Some(false), WINDOWS + 1)),
            FocusState::Unknown
        );
    }

    #[test]
    fn refresh_waits_for_native_completion_and_coalesces_an_unrun_callback() {
        tauri::async_runtime::block_on(async {
            let source = Source::default();
            assert!(source.refresh().is_none());
            let completion = Arc::new(Mutex::new(None));
            let held = completion.clone();
            source.set_refresh(Arc::new(move |_, done| {
                assert!(held.lock().unwrap().replace(done).is_none());
            }));
            let first = source.refresh().unwrap();
            let second = source.refresh().unwrap();
            let done = completion.lock().unwrap().take().unwrap();
            assert!(!*first.0.borrow());
            done.send(true).unwrap();
            assert!(first.wait().await);
            assert!(second.wait().await);
            let failed = source.refresh().unwrap();
            drop(completion.lock().unwrap().take().unwrap());
            assert!(
                !failed.wait().await,
                "a dropped callback is not an observation"
            );
        });
    }

    #[test]
    fn a_new_subscription_gets_a_new_check_after_the_old_callback_already_observed() {
        tauri::async_runtime::block_on(async {
            let source = Source::default();
            let completions = Arc::new(Mutex::new(Vec::new()));
            let held = completions.clone();
            source.set_refresh(Arc::new(move |source, complete| {
                let mut checks = held.lock().unwrap();
                source.observe(if checks.is_empty() {
                    FocusState::Foreground
                } else {
                    FocusState::Background
                });
                checks.push(complete); // Observation precedes delayed completion.
            }));
            let observations = Arc::new(Mutex::new(Vec::new()));
            let first = observations.clone();
            let old = source.subscribe(
                "old".into(),
                Arc::new(move |s| first.lock().unwrap().push((1, s))),
            );
            let old_check = source.refresh().unwrap();
            let next = observations.clone();
            let _new = source.subscribe(
                "new".into(),
                Arc::new(move |s| next.lock().unwrap().push((2, s))),
            );
            let new_check = source.refresh().unwrap();
            let coalesced = source.refresh().unwrap();
            assert_eq!(completions.lock().unwrap().len(), 2);
            assert_eq!(
                *observations.lock().unwrap(),
                [(1, FocusState::Foreground), (2, FocusState::Background)]
            );
            drop(old);
            source.observe(FocusState::Unknown);
            assert_eq!(
                observations.lock().unwrap().last(),
                Some(&(2, FocusState::Unknown))
            );
            let mut complete = std::mem::take(&mut *completions.lock().unwrap()).into_iter();
            complete.next().unwrap().send(true).unwrap();
            assert!(old_check.wait().await);
            assert!(
                !*new_check.0.borrow(),
                "old completion cannot acknowledge the new check"
            );
            complete.next().unwrap().send(true).unwrap();
            assert!(new_check.wait().await);
            assert!(coalesced.wait().await);
        });
    }

    #[test]
    fn blur_and_unknown_bypass_renewal_delay_and_new_connections_do_not_inherit_timing() {
        let now = Instant::now();
        let mut schedule = Schedule::default();
        let front = observation(FocusState::Foreground, 1);
        assert!(schedule.due(&front, now));
        schedule.attempted(front.clone(), now);
        assert!(!schedule.due(&front, now + Duration::from_millis(1999)));
        assert!(schedule.due(&front, now + Duration::from_millis(2000)));
        let background = observation(FocusState::Background, 2);
        assert!(schedule.due(&background, now));
        schedule.attempted(background.clone(), now);
        assert!(!schedule.due(&background, now));
        schedule.lost_reply();
        assert!(!schedule.due(&background, now + Duration::from_millis(1999)));
        assert!(schedule.due(&background, now + Duration::from_millis(2000)));
        let unknown = observation(FocusState::Unknown, 3);
        assert!(schedule.due(&unknown, now));
        schedule.attempted(unknown, now);
        assert!(!schedule.due(&front, now + Duration::from_secs(3)));
        let mut replaced = observation(FocusState::Unknown, 0);
        replaced.connection_id = "00000000-0000-4000-8000-000000000002".into();
        assert!(schedule.due(&replaced, now));
        schedule.disconnected();
        assert!(schedule.due(&replaced, now));
        assert!(!schedule.due(&observation(FocusState::Foreground, 0), now));
    }

    #[test]
    fn reply_must_bind_the_exact_native_evidence_in_addition_to_its_uuid_and_scope() {
        let scope = Scope {
            configuration: "a".repeat(64),
            service_epoch: uuid::Uuid::new_v4().to_string(),
        };
        let command = Command::Focus {
            request_id: uuid::Uuid::new_v4().to_string(),
        };
        let attached = observation(FocusState::Foreground, 1);
        let mut reply = FocusReply {
            abi: ABI.into(),
            request_id: command.request_id().into(),
            scope: scope.clone(),
            observation: attached.clone(),
            ttl_ms: 5000,
        };
        assert!(reply_matches(&reply, &command, &scope, &attached));
        reply.observation.revision += 1;
        assert!(reply.matches(&command, &scope));
        assert!(!reply_matches(&reply, &command, &scope, &attached));
        reply.observation = attached.clone();
        reply.observation.connection_id = uuid::Uuid::new_v4().to_string();
        assert!(!reply_matches(&reply, &command, &scope, &attached));
        reply.observation = attached.clone();
        reply.ttl_ms = 5001;
        assert!(!reply_matches(&reply, &command, &scope, &attached));
    }
}
