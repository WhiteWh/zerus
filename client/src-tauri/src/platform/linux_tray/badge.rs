//! Optional badge-only resource scope. No native lease, producer or drain permit.
use super::*;
use crate::notifier::accounting::{Completion, Control as AccountingControl, Resources};
use std::sync::Weak;
use zerus_client_core::desktop::drain::{Charge, Role};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    WrongRole,
    Duplicate,
    Closed,
    Capacity,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub closed: bool,
    pub pinned: bool,
    pub ready: bool,
}
#[derive(Clone)]
pub(crate) struct Control(Weak<Scope>);
struct State {
    closed: bool,
    ready: bool,
    queries: usize,
    desired: Option<(i64, Completion)>,
    fence: Option<Completion>,
    finished: bool,
    may_have_badge: bool,
    wake: Option<std::sync::mpsc::SyncSender<()>>,
}
pub(super) struct Scope {
    resources: Arc<Resources>,
    accounting: AccountingControl,
    state: Mutex<State>,
}
fn unavailable(_: crate::notifier::accounting::Refused) -> Refused {
    Refused::Unavailable
}
impl Scope {
    pub fn new(host: Charge) -> Result<(Arc<Self>, Control, Completion), Refused> {
        if host.role() != Role::Badges {
            return Err(Refused::WrongRole);
        }
        let (resources, accounting) =
            Resources::new_for(host, Role::Badges).map_err(unavailable)?;
        let startup = resources.start().map_err(unavailable)?;
        let scope = Arc::new(Self {
            resources,
            accounting,
            state: Mutex::new(State {
                closed: false,
                ready: false,
                queries: 0,
                desired: None,
                fence: None,
                finished: false,
                may_have_badge: false,
                wake: None,
            }),
        });
        Ok((scope.clone(), Control(Arc::downgrade(&scope)), startup))
    }
    pub fn control(self: &Arc<Self>) -> Control {
        Control(Arc::downgrade(self))
    }
    pub fn bind_worker(&self, wake: std::sync::mpsc::SyncSender<()>) {
        lock(&self.state).wake = Some(wake);
    }
    pub fn ready(&self) {
        let mut state = lock(&self.state);
        state.ready = true;
        Self::wake(&state);
    }
    pub fn closed(&self) -> bool {
        lock(&self.state).closed
    }
    pub fn finished(&self) -> bool {
        lock(&self.state).finished
    }
    fn wake(state: &State) {
        if let Some(wake) = &state.wake {
            let _ = wake.try_send(());
        }
    }
    pub fn offer(&self, count: i64) -> Result<(), Refused> {
        let mut state = self.state.lock().map_err(|_| Refused::Unavailable)?;
        if state.closed {
            return Err(Refused::Closed);
        }
        if let Some((desired, _)) = &mut state.desired {
            *desired = count;
        } else {
            state.desired = Some((count, self.resources.start().map_err(unavailable)?));
        }
        Self::wake(&state);
        Ok(())
    }
    pub fn take(&self) -> Option<(i64, Completion)> {
        let mut state = lock(&self.state);
        if state.closed {
            None
        } else {
            let next = state.desired.take();
            if next.as_ref().is_some_and(|(count, _)| *count > 0) {
                state.may_have_badge = true;
            }
            next
        }
    }
    pub fn sent(&self, count: i64) {
        lock(&self.state).may_have_badge = count > 0;
    }
    pub fn needs_clear(&self) -> bool {
        lock(&self.state).may_have_badge
    }
    /// Runs on the existing native worker after stream/interface removal and
    /// after every admitted Query reply. A returned None keeps the fence held.
    pub fn fence(&self) -> Option<Completion> {
        let mut state = lock(&self.state);
        if !state.closed || !state.ready || state.queries != 0 || state.finished {
            return None;
        }
        state.finished = true;
        state.fence.take()
    }
    pub fn query(self: &Arc<Self>) -> Result<Query, Refused> {
        let mut state = self.state.lock().map_err(|_| Refused::Unavailable)?;
        if state.closed {
            return Err(Refused::Closed);
        }
        if state.queries >= 16 {
            return Err(Refused::Capacity);
        }
        let work = self.resources.start().map_err(unavailable)?;
        state.queries += 1;
        Ok(Query {
            scope: self.clone(),
            work: Some(work),
        })
    }
}
impl Control {
    /// Closes only badge admission. Native work/futures own the remaining charge.
    pub fn close(&self) -> Result<Snapshot, Refused> {
        let scope = self.0.upgrade().ok_or(Refused::Unavailable)?;
        let mut state = scope.state.lock().map_err(|_| Refused::Unavailable)?;
        if !state.closed {
            state.fence = Some(scope.resources.start().map_err(unavailable)?);
            state.closed = true;
            // Coalesced work not yet issued is verifiably cancelled locally.
            if let Some((_, work)) = state.desired.take() {
                work.complete();
            }
            scope.accounting.close().map_err(unavailable)?;
            Self::wake_checked(&mut state)?;
        }
        drop(state);
        self.snapshot()
    }
    fn wake_checked(state: &mut State) -> Result<(), Refused> {
        if state.wake.as_ref().is_some_and(|wake| {
            matches!(
                wake.try_send(()),
                Err(std::sync::mpsc::TrySendError::Disconnected(_))
            )
        }) {
            // No worker can execute this admitted fence. Unknown, not idle.
            drop(state.fence.take());
            state.finished = true;
            return Err(Refused::Unavailable);
        }
        Ok(())
    }
    pub fn snapshot(&self) -> Result<Snapshot, Refused> {
        let scope = self.0.upgrade().ok_or(Refused::Unavailable)?;
        let state = scope.state.lock().map_err(|_| Refused::Unavailable)?;
        let resources = scope.accounting.snapshot().map_err(unavailable)?;
        Ok(Snapshot {
            closed: state.closed,
            pinned: resources.pinned,
            ready: state.ready,
        })
    }
}
pub(super) struct Query {
    scope: Arc<Scope>,
    work: Option<Completion>,
}
impl Query {
    fn complete(mut self) {
        if let Some(work) = self.work.take() {
            work.complete();
        }
    }
}
impl Drop for Query {
    fn drop(&mut self) {
        let mut state = lock(&self.scope.state);
        state.queries -= 1;
        Scope::wake(&state);
        // Dropping an armed work obligation pins through Completion::drop.
    }
}

/// The generated zbus dispatch future contains serialization AND reply().await.
/// Wrapping that future, rather than query(), retains the actual OS reply work.
pub(super) struct Service {
    pub inner: BadgeService,
    pub scope: Arc<Scope>,
}
#[zbus::export::async_trait::async_trait]
impl zbus::object_server::Interface for Service {
    fn name() -> zbus::names::InterfaceName<'static> {
        <BadgeService as zbus::object_server::Interface>::name()
    }
    async fn get(
        &self,
        name: &str,
        server: &zbus::ObjectServer,
        connection: &zbus::Connection,
        header: Option<&zbus::message::Header<'_>>,
        emitter: &zbus::object_server::SignalEmitter<'_>,
    ) -> Option<zbus::fdo::Result<OwnedValue>> {
        zbus::object_server::Interface::get(&self.inner, name, server, connection, header, emitter)
            .await
    }
    async fn get_all(
        &self,
        server: &zbus::ObjectServer,
        connection: &zbus::Connection,
        header: Option<&zbus::message::Header<'_>>,
        emitter: &zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::fdo::Result<HashMap<String, OwnedValue>> {
        zbus::object_server::Interface::get_all(&self.inner, server, connection, header, emitter)
            .await
    }
    async fn set_mut(
        &mut self,
        name: &str,
        value: &Value<'_>,
        server: &zbus::ObjectServer,
        connection: &zbus::Connection,
        header: Option<&zbus::message::Header<'_>>,
        emitter: &zbus::object_server::SignalEmitter<'_>,
    ) -> Option<zbus::fdo::Result<()>> {
        zbus::object_server::Interface::set_mut(
            &mut self.inner,
            name,
            value,
            server,
            connection,
            header,
            emitter,
        )
        .await
    }
    fn call<'a>(
        &'a self,
        server: &'a zbus::ObjectServer,
        connection: &'a zbus::Connection,
        message: &'a zbus::Message,
        name: zbus::names::MemberName<'a>,
    ) -> zbus::object_server::DispatchResult2<'a> {
        use zbus::object_server::DispatchResult2;
        if name.as_str() != "Query" {
            return self.inner.call(server, connection, message, name);
        }
        let work = match self.scope.query() {
            Ok(work) => work,
            Err(_) => {
                return DispatchResult2::new_async(connection, message, async {
                    Err::<(String, Props), _>(zbus::fdo::Error::LimitsExceeded(
                        "Badge admission is closed or unavailable".into(),
                    ))
                })
            }
        };
        match self.inner.call(server, connection, message, name) {
            DispatchResult2::Async(reply) => DispatchResult2::Async(Box::pin(async move {
                let result = reply.await;
                if result.is_ok() {
                    work.complete();
                }
                result
            })),
            other => {
                work.complete();
                other
            }
        }
    }
    fn call_mut<'a>(
        &'a mut self,
        server: &'a zbus::ObjectServer,
        connection: &'a zbus::Connection,
        message: &'a zbus::Message,
        name: zbus::names::MemberName<'a>,
    ) -> zbus::object_server::DispatchResult2<'a> {
        zbus::object_server::Interface::call_mut(&mut self.inner, server, connection, message, name)
    }
    fn introspect_to_writer(&self, writer: &mut dyn std::fmt::Write, level: usize) {
        zbus::object_server::Interface::introspect_to_writer(&self.inner, writer, level);
    }
}

/// Accounted-only owner streams use explicit match ownership. SDK MessageStream
/// Drop would detach RemoveMatch; AsyncDrop hides its error. Raw stream Drop has
/// no deferred cleanup; an explicit failed RemoveMatch pins the fixed fence.
pub(super) struct ShellStream {
    stream: zbus::MessageStream,
    rule: zbus::MatchRule<'static>,
    work: Completion,
}
impl ShellStream {
    #[cfg(test)]
    pub fn new(connection: &Connection, service: &str, scope: &Arc<Scope>) -> zbus::Result<Self> {
        Ok(Self::prepare(connection, service, scope)?.activate(connection))
    }
    pub fn prepare(
        connection: &Connection,
        service: &str,
        scope: &Arc<Scope>,
    ) -> zbus::Result<Prepared> {
        let work = scope
            .resources
            .start()
            .map_err(|_| zbus::Error::Failure("Badge stream admission closed".into()))?;
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")?
            .path("/org/freedesktop/DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .add_arg(service.to_owned())?
            .build()
            .to_owned();
        zbus::blocking::fdo::DBusProxy::new(connection)?.add_match_rule(rule.clone())?;
        Ok(Prepared { rule, work })
    }
    pub fn matches(&self, message: &zbus::Message) -> bool {
        message.header().sender().map(|name| name.as_str()) == Some("org.freedesktop.DBus")
            && self.rule.matches(message).unwrap_or(false)
    }
    pub fn poll(
        &mut self,
        context: &mut TaskContext<'_>,
    ) -> Poll<Option<zbus::Result<zbus::Message>>> {
        Pin::new(&mut self.stream).poll_next(context)
    }
    /// Drop all raw receivers before issuing any RemoveMatch. Otherwise a full
    /// bounded raw queue could stall the reader carrying that method's reply.
    pub fn stop(self) -> Removal {
        let Self { stream, rule, work } = self;
        drop(stream);
        Removal { rule, work }
    }
}
/// All blocking AddMatch acknowledgements must precede activation of the
/// first bounded raw receiver. Startup cannot drain those receivers yet.
pub(super) struct Prepared {
    rule: zbus::MatchRule<'static>,
    work: Completion,
}
impl Prepared {
    pub fn activate(self, connection: &Connection) -> ShellStream {
        ShellStream {
            stream: zbus::MessageStream::from(connection.inner().clone()),
            rule: self.rule,
            work: self.work,
        }
    }
}
pub(super) struct Removal {
    rule: zbus::MatchRule<'static>,
    work: Completion,
}
impl Removal {
    pub fn close(self, connection: &Connection) -> zbus::Result<()> {
        let Self { rule, work } = self;
        let result = zbus::blocking::fdo::DBusProxy::new(connection)
            .and_then(|bus| Ok(bus.remove_match_rule(rule)?));
        if result.is_ok() {
            work.complete();
        }
        // An error leaves work armed: unknown cleanup pins one scope.
        result
    }
}

#[cfg(test)]
mod tests;
