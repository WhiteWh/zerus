//! Native service-connection and app-window observations. No request decoder
//! calls these setters; cached evidence never grants a native writer.
use super::{FocusState, NativeControl, NativeFocus, Scope, MAX_REVISION};
use crate::desktop::{ownership::Mode, Responder};
use std::{
    io,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub(crate) struct Cache(Arc<Mutex<Option<NativeControl>>>);

/// The retained native service connection owns this incarnation. Disconnect
/// invalidates evidence immediately, while registrations retain their original
/// resource charges. Neither this handle nor its cache is deserializable.
pub struct Connection {
    cache: Arc<Mutex<Option<NativeControl>>>,
    id: String,
    scope: Scope,
}
impl Connection {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn snapshot(&self) -> Option<NativeFocus> {
        let state = self.cache.try_lock().ok()?;
        state
            .as_ref()?
            .focus
            .as_ref()
            .filter(|v| v.connection_id == self.id)
            .cloned()
    }
    /// Only actual native toplevel focus/loss events call this method. A
    /// Foreground renewal reads snapshot(); it does not invent a new event.
    pub fn observe(&self, state: FocusState) -> io::Result<NativeFocus> {
        let mut cache = self.cache.lock().map_err(|_| io::ErrorKind::Other)?;
        let current = cache
            .as_mut()
            .and_then(|v| v.focus.as_mut())
            .filter(|v| v.connection_id == self.id)
            .ok_or(io::ErrorKind::NotConnected)?;
        if current.state != state {
            let revision = current
                .revision
                .checked_add(1)
                .filter(|v| *v <= MAX_REVISION);
            let Some(revision) = revision else {
                *cache = None;
                return Err(io::ErrorKind::Other.into());
            };
            current.revision = revision;
            current.state = state;
        }
        Ok(current.clone())
    }
    pub fn invalidate(&self) {
        if let Ok(mut state) = self.cache.lock() {
            if state
                .as_ref()
                .and_then(|v| v.focus.as_ref())
                .is_some_and(|v| v.connection_id == self.id)
            {
                *state = None;
            }
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.invalidate();
    }
}
impl Cache {
    fn begin(&self, scope: Scope) -> io::Result<Connection> {
        let id = uuid::Uuid::new_v4().to_string();
        let mut state = self.0.lock().map_err(|_| io::ErrorKind::Other)?;
        *state = Some(NativeControl {
            scope: scope.clone(),
            mode: Mode::Service,
            focus: Some(NativeFocus {
                connection_id: id.clone(),
                revision: 0,
                state: FocusState::Unknown,
            }),
        });
        Ok(Connection {
            cache: self.0.clone(),
            id,
            scope,
        })
    }
    pub(crate) fn evidence(&self, scope: &Scope, focus: bool) -> Option<NativeControl> {
        let mut value = self
            .0
            .try_lock()
            .ok()?
            .as_ref()
            .filter(|v| &v.scope == scope)?
            .clone();
        if !focus {
            value.focus = None;
        }
        Some(value)
    }
}
impl Responder {
    /// Called by the actual retained native service connection after selection
    /// and scope verification. Every new incarnation starts Unknown.
    pub fn notification_connect(&self, scope: Scope) -> io::Result<Connection> {
        if !scope.valid()
            || self.cached_configuration() != Some(scope.configuration.as_str())
            || self.ownership_mode.get() != Some(&Mode::Service)
            || !self.live
            || self.replay
            || self.application != "com.hgdev.hgs-tray"
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.notification_focus.begin(scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> Scope {
        Scope {
            configuration: "a".repeat(64),
            service_epoch: uuid::Uuid::new_v4().to_string(),
        }
    }
    #[test]
    fn native_loss_and_reconnect_invalidate_old_observations_without_adopting_old_state() {
        let cache = Cache::default();
        let scope = scope();
        let first = cache.begin(scope.clone()).unwrap();
        assert_eq!(first.snapshot().unwrap().state, FocusState::Unknown);
        let foreground = first.observe(FocusState::Foreground).unwrap();
        assert_eq!(foreground.revision, 1);
        assert_eq!(first.snapshot(), Some(foreground.clone()));
        let background = first.observe(FocusState::Background).unwrap();
        assert_eq!(background.revision, 2);
        let second = cache.begin(scope.clone()).unwrap();
        assert!(first.snapshot().is_none());
        assert!(first.observe(FocusState::Foreground).is_err());
        drop(first);
        assert_eq!(second.snapshot().unwrap().state, FocusState::Unknown);
        assert!(cache.evidence(&scope, false).unwrap().focus.is_none());
        second.invalidate();
        assert!(cache.evidence(&scope, true).is_none());
    }
    #[test]
    fn native_scope_and_mode_are_required_before_caching_a_connection() {
        let scope = scope();
        let responder = Responder::new(
            "com.hgdev.hgs-tray".into(),
            crate::desktop::Build {
                version: "1.2.3".into(),
                source_commit: "a".repeat(40),
                source_dirty: Some(false),
            },
            uuid::Uuid::new_v4().to_string(),
            true,
            false,
        );
        assert!(responder.notification_connect(scope.clone()).is_err());
        assert!(responder.configure_once(scope.configuration.clone()));
        assert!(responder.notification_connect(scope.clone()).is_err());
        assert!(responder.configure_ownership_mode_once(Mode::Service));
        let connection = responder.notification_connect(scope.clone()).unwrap();
        assert_eq!(connection.scope(), &scope);
        let wrong = Scope {
            service_epoch: uuid::Uuid::new_v4().to_string(),
            ..scope.clone()
        };
        assert!(responder
            .notification_focus
            .evidence(&wrong, true)
            .is_none());
    }
}
