use super::*;
use std::future::Future;
use std::pin::{pin, Pin};
use std::task::{Context, Poll, Wake, Waker};
use zbus::export::futures_core::Stream;
use zbus::{Connection, Executor, MessageStream};
use zerus_freedesktop_notify as native;

const BUS: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const RULES: [&str; 2] = [
    "type='signal',interface='org.freedesktop.Notifications',path='/org/freedesktop/Notifications'",
    "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.freedesktop.Notifications'",
];
const RAW: usize = 16;
const IDLE: Duration = Duration::from_millis(100);
enum Batch {
    Complete,
    Blocked,
    Stopped,
}
struct Notify(mpsc::SyncSender<()>);
impl Wake for Notify {
    fn wake(self: Arc<Self>) {
        let _ = self.0.try_send(());
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.0.try_send(());
    }
}
struct Driver {
    wake: Waker,
    receive: mpsc::Receiver<()>,
    events: std::collections::VecDeque<Signal>,
    owner: Option<String>,
    broken: bool,
    backpressured: bool,
}
impl Driver {
    fn new() -> Self {
        let (send, receive) = mpsc::sync_channel(1);
        Self {
            wake: Waker::from(Arc::new(Notify(send))),
            receive,
            events: std::collections::VecDeque::new(),
            owner: None,
            broken: false,
            backpressured: false,
        }
    }
    fn message(&mut self, message: zbus::Message) {
        let header = message.header();
        if header.message_type() == zbus::message::Type::Signal
            && header.sender().map(|s| s.as_str()) == Some(BUS)
            && header.path().map(|s| s.as_str()) == Some(BUS_PATH)
            && header.interface().map(|s| s.as_str()) == Some(BUS)
            && header.member().map(|s| s.as_str()) == Some("NameOwnerChanged")
        {
            match message.body().deserialize::<(String, String, String)>() {
                Ok((name, old, new)) if name == native::SERVICE => {
                    if self
                        .owner
                        .as_ref()
                        .is_some_and(|owner| old == *owner && new != *owner)
                    {
                        self.broken = true;
                    }
                }
                Ok(_) => (),
                Err(_) => self.broken = true,
            }
        }
        let Some(owner) = self
            .owner
            .as_deref()
            .and_then(|s| zbus::names::UniqueName::try_from(s).ok())
        else {
            return;
        };
        if let Some(signal) = native::decode_signal(&message, &owner) {
            debug_assert!(self.events.len() < CALLBACKS);
            self.events.push_back(signal);
        }
    }
    /// Stop before polling when decoded storage is full. The original SDK
    /// receiver and daemon provide backpressure; no signal is discarded.
    fn read_ready(&mut self, reader: &mut MessageStream, cx: &mut Context<'_>) -> Result<bool, ()> {
        for _ in 0..RAW {
            if self.events.len() == CALLBACKS {
                self.backpressured = true;
                return Ok(false);
            }
            match pin!(&mut *reader).poll_next(cx) {
                Poll::Ready(Some(Ok(message))) => self.message(message),
                Poll::Ready(_) => {
                    self.broken = true;
                    return Err(());
                }
                Poll::Pending => return Ok(true),
            }
        }
        Ok(false)
    }
    fn drive<T>(
        &mut self,
        executor: Option<&Executor<'static>>,
        mut reader: Option<&mut MessageStream>,
        deadline: Instant,
        cancelled: impl Fn() -> bool,
        work: impl Future<Output = T>,
    ) -> std::result::Result<T, ()> {
        let mut work = pin!(work);
        self.backpressured = false;
        loop {
            if cancelled() || Instant::now() >= deadline || self.broken && reader.is_some() {
                return Err(());
            }
            let wake = self.wake.clone();
            let mut context = Context::from_waker(&wake);
            if let Poll::Ready(result) = work.as_mut().poll(&mut context) {
                return if cancelled() || Instant::now() >= deadline {
                    Err(())
                } else {
                    Ok(result)
                };
            }
            let mut progressed = false;
            // Retain the pending tick THROUGH the wait. Dropping it first
            // unregisters the executor wake and turns socket IO into polling.
            let mut tick = pin!(executor.map(Executor::tick));
            for _ in 0..16 {
                let Some(next) = tick.as_mut().as_pin_mut() else {
                    break;
                };
                if next.poll(&mut context).is_pending() {
                    break;
                }
                progressed = true;
                tick.set(executor.map(Executor::tick));
            }
            if let Some(reader) = reader.as_mut() {
                progressed |= !self.read_ready(reader, &mut context)?;
                if self.backpressured {
                    return Err(());
                }
            }
            if progressed {
                continue;
            }
            let _ = self
                .receive
                .recv_timeout(deadline.saturating_duration_since(Instant::now()).min(IDLE));
        }
    }
}
type Lookup = Pin<Box<dyn Future<Output = Result<Namespace, ()>> + Send>>;
struct Bus {
    connection: Connection,
    executor: Executor<'static>,
    reader: Option<MessageStream>,
    gate: Arc<ingress::Ingress>,
    driver: Driver,
    matched: usize,
    identity: Option<Namespace>,
    lookup: Option<Lookup>,
    checked: usize,
    #[cfg(test)]
    batch_pause: Option<BatchPause>,
}
impl Bus {
    fn connect(
        address: String,
        deadline: Instant,
        cancelled: impl Fn() -> bool,
    ) -> std::result::Result<Self, ()> {
        let mut driver = Driver::new();
        let (socket, gate) =
            driver.drive(None, None, deadline, &cancelled, ingress::connect(address))??;
        let connection = driver
            .drive(
                None,
                None,
                deadline,
                &cancelled,
                zbus::connection::Builder::socket(socket)
                    .internal_executor(false)
                    .max_queued(RAW)
                    .build(),
            )?
            .map_err(|_| ())?;
        let executor = connection.executor().clone();
        if connection.is_closed() || executor.is_empty() {
            return Err(());
        }
        let reader = Some(MessageStream::from(&connection));
        Ok(Self {
            connection,
            executor,
            reader,
            gate,
            driver,
            matched: 0,
            identity: None,
            lookup: None,
            checked: 0,
            #[cfg(test)]
            batch_pause: None,
        })
    }
    fn drive<T>(
        &mut self,
        deadline: Instant,
        cancelled: impl Fn() -> bool,
        work: impl Future<Output = T>,
    ) -> std::result::Result<T, ()> {
        self.driver.drive(
            Some(&self.executor),
            self.reader.as_mut(),
            deadline,
            cancelled,
            work,
        )
    }
    fn matches(&mut self, add: bool, deadline: Instant, cancelled: impl Fn() -> bool) -> bool {
        let count = if add { RULES.len() } else { self.matched };
        for rule in RULES.iter().take(count) {
            let connection = self.connection.clone();
            let method = if add { "AddMatch" } else { "RemoveMatch" };
            let call = async {
                let reply = connection
                    .call_method(Some(BUS), BUS_PATH, Some(BUS), method, &(rule,))
                    .await
                    .map_err(|_| ())?;
                if reply.header().sender().map(|s| s.as_str()) != Some(BUS) {
                    return Err(());
                }
                reply.body().deserialize::<()>().map_err(|_| ())
            };
            if !matches!(self.drive(deadline, &cancelled, call), Ok(Ok(()))) {
                return false;
            }
            if add {
                self.matched += 1;
            }
        }
        true
    }
    fn namespace(
        &mut self,
        deadline: Instant,
        cancelled: impl Fn() -> bool,
    ) -> std::result::Result<Namespace, ()> {
        self.drive(
            deadline,
            cancelled,
            Self::lookup(self.connection.clone(), false),
        )?
    }
    async fn lookup(connection: Connection, activate: bool) -> Result<Namespace, ()> {
        let bus = connection
            .call_method(Some(BUS), BUS_PATH, Some(BUS), "GetId", &())
            .await
            .map_err(|_| ())?;
        async fn get_owner(connection: &Connection) -> zbus::Result<zbus::Message> {
            connection
                .call_method(
                    Some(BUS),
                    BUS_PATH,
                    Some(BUS),
                    "GetNameOwner",
                    &(native::SERVICE,),
                )
                .await
        }
        let owner = match get_owner(&connection).await {
            Ok(owner) => owner,
            Err(zbus::Error::MethodError(name, _, reply))
                if activate
                    && name.as_str() == "org.freedesktop.DBus.Error.NameHasNoOwner"
                    && reply.header().sender().map(|s| s.as_str()) == Some(BUS) =>
            {
                // Only the bus may activate this fixed desktop service. This
                // is setup readiness, never a notification or an ID redirect.
                let started = connection
                    .call_method(
                        Some(BUS),
                        BUS_PATH,
                        Some(BUS),
                        "StartServiceByName",
                        &(native::SERVICE, 0_u32),
                    )
                    .await
                    .map_err(|_| ())?;
                if !reply_from(&started, BUS)
                    || !matches!(started.body().deserialize::<u32>(), Ok(1 | 2))
                {
                    return Err(());
                }
                get_owner(&connection).await.map_err(|_| ())?
            }
            Err(_) => return Err(()),
        };
        if [bus.header(), owner.header()]
            .iter()
            .any(|h| h.sender().map(|s| s.as_str()) != Some(BUS))
        {
            return Err(());
        }
        let bus_id: String = bus.body().deserialize().map_err(|_| ())?;
        let owner: String = owner.body().deserialize().map_err(|_| ())?;
        if bus_id.len() != 32
            || !bus_id.bytes().all(|b| b.is_ascii_hexdigit())
            || zbus::names::UniqueName::try_from(owner.as_str()).is_err()
        {
            return Err(());
        }
        Ok(Namespace { bus_id, owner })
    }
    fn bind(&mut self, namespace: &Namespace) {
        self.driver.owner = Some(namespace.owner.clone());
        self.identity = Some(namespace.clone());
    }
    fn watched_namespace(&self, expected: &Namespace) -> bool {
        // This authenticated socket never reconnects. Its bus ID is fixed;
        // ordered bus-only NameOwnerChanged messages fence the pinned owner.
        self.identity.as_ref() == Some(expected)
            && self.driver.owner.as_deref() == Some(expected.owner.as_str())
            && !self.connection.is_closed()
            && !self.driver.broken
            && !self.gate.refused()
    }
    fn batch_namespace(
        &mut self,
        expected: &Namespace,
        until: Instant,
        cancelled: impl Fn() -> bool,
    ) -> bool {
        match self.namespace(until, cancelled) {
            Ok(current) => current == *expected && self.watched_namespace(expected),
            // A reply can be behind a signal burst on the SAME socket. Keep
            // bounded input flowing using its authenticated ordered loss watch;
            // never open a second connection or grow storage to reach a reply.
            Err(()) if self.driver.backpressured => self.watched_namespace(expected),
            Err(()) => false,
        }
    }
    fn queued(
        &mut self,
        mappings: &mut Mappings,
        endpoint: &Endpoint,
        closed_only: bool,
        limit: usize,
    ) -> bool {
        let count = self.driver.events.len().min(limit);
        for _ in 0..count {
            let Some(signal) = self.driver.events.front() else {
                break;
            };
            if (!closed_only || matches!(signal, Signal::NotificationClosed { .. }))
                && !mappings.signal(signal, endpoint)
            {
                return false;
            }
            self.driver.events.pop_front();
            self.checked = self.checked.saturating_sub(1);
        }
        true
    }
    fn route_batch(
        &mut self,
        mappings: &mut Mappings,
        endpoint: &Endpoint,
        until: Instant,
        cancelled: impl Fn() -> bool,
    ) -> Result<Batch, ()> {
        if !self.watched_namespace(&mappings.namespace) {
            return Err(());
        }
        if cancelled() {
            return Ok(Batch::Stopped);
        }
        if self.driver.events.is_empty() {
            return Ok(Batch::Complete);
        }
        if self.checked != 0 {
            return Ok(if self.queued(mappings, endpoint, false, self.checked) {
                Batch::Complete
            } else {
                Batch::Blocked
            });
        }
        let mut lookup = self
            .lookup
            .take()
            .unwrap_or_else(|| Box::pin(Self::lookup(self.connection.clone(), false)));
        #[cfg(test)]
        if let Some(pause) = self.batch_pause.take() {
            pause.reached.send(()).unwrap();
            pause.resume.recv_timeout(WAIT).unwrap();
        }
        loop {
            match self.drive(until, &cancelled, lookup.as_mut()) {
                Ok(Ok(current))
                    if current == mappings.namespace
                        && self.watched_namespace(&mappings.namespace) =>
                {
                    self.checked = self.driver.events.len();
                    return Ok(if self.queued(mappings, endpoint, false, self.checked) {
                        Batch::Complete
                    } else {
                        Batch::Blocked
                    });
                }
                Err(()) if cancelled() && self.watched_namespace(&mappings.namespace) => {
                    // Stopping cancels this read-only lookup, not the original
                    // connection's normal RemoveMatch/Closed drain.
                    return Ok(Batch::Stopped);
                }
                Err(())
                    if self.driver.backpressured && self.watched_namespace(&mappings.namespace) =>
                {
                    // Keep the SAME lookup alive while its reply is behind
                    // bounded signals. Retrying a blocked emit must not send
                    // another RPC or accumulate cancelled lookup replies.
                    if !self.queued(mappings, endpoint, false, CALLBACKS) {
                        // Actor backpressure is not a namespace timeout. Keep
                        // this one original lookup and signal for a later turn.
                        self.lookup = Some(lookup);
                        return Ok(Batch::Blocked);
                    }
                }
                _ => return Err(()),
            }
        }
    }
    fn drive_effect<T>(
        &mut self,
        mappings: &mut Mappings,
        endpoint: &Endpoint,
        until: Instant,
        cancelled: impl Fn() -> bool,
        posting: bool,
        work: impl Future<Output = T>,
    ) -> Result<T, ()> {
        let mut work = pin!(work);
        loop {
            match self.drive(until, &cancelled, work.as_mut()) {
                Err(())
                    if !cancelled()
                        && self.driver.backpressured
                        && self.watched_namespace(&mappings.namespace) =>
                {
                    for _ in 0..CALLBACKS {
                        let Some(signal) = self.driver.events.front() else {
                            break;
                        };
                        let id = match signal {
                            Signal::ActivationToken { id, .. }
                            | Signal::ActionInvoked { id, .. }
                            | Signal::NotificationClosed { id, .. } => id,
                        };
                        // A post's unknown ID may have closed BEFORE its reply.
                        // Keep that signal for adoption; never discard it just
                        // to reach the reply or grow the bounded queue. A full
                        // uncorrelated queue remains conservatively Unknown.
                        if posting && !mappings.rows.contains_key(id) {
                            return Err(());
                        }
                        if !mappings.signal(signal, endpoint) {
                            self.driver
                                .receive
                                .recv_timeout(
                                    IDLE.min(until.saturating_duration_since(Instant::now())),
                                )
                                .ok();
                            break;
                        }
                        self.driver.events.pop_front();
                        self.checked = self.checked.saturating_sub(1);
                    }
                    // Continue polling the SAME effect future. No native call
                    // is reissued, and its original resources/deadline remain.
                }
                result => return result,
            }
        }
    }
    fn close(
        mut self,
        was_good: bool,
        mut mappings: Option<&mut Mappings>,
        endpoint: &Endpoint,
        deadline: Instant,
    ) -> Exit {
        let mut good = was_good && self.matched == RULES.len();
        // Cancel only a read-only lookup. No native operation is replayed;
        // its pending reply still drains on the same original connection.
        drop(self.lookup.take());
        // Keep each ORIGINAL RemoveMatch future alive across backpressure. A
        // full actor queue cannot cause a second removal or a lost callback.
        for rule in RULES.iter().take(self.matched) {
            let connection = self.connection.clone();
            let call = async {
                let reply = connection
                    .call_method(Some(BUS), BUS_PATH, Some(BUS), "RemoveMatch", &(rule,))
                    .await?;
                if !reply_from(&reply, BUS) {
                    return Err(zbus::Error::ExcessData);
                }
                reply.body().deserialize::<()>()
            };
            let mut call = pin!(call);
            loop {
                match self.drive(deadline, || false, call.as_mut()) {
                    Ok(Ok(())) => break,
                    Err(()) if self.driver.backpressured && good => {
                        let Some(map) = mappings.as_deref_mut() else {
                            good = false;
                            break;
                        };
                        if !self.watched_namespace(&map.namespace) {
                            good = false;
                            break;
                        }
                        if !self.queued(map, endpoint, true, CALLBACKS) {
                            self.driver
                                .receive
                                .recv_timeout(
                                    IDLE.min(deadline.saturating_duration_since(Instant::now())),
                                )
                                .ok();
                        }
                    }
                    _ => {
                        good = false;
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    good = false;
                    break;
                }
            }
            if !good {
                break;
            }
        }
        // RemoveMatch replies form the ordered bus boundary. Drain ALL queued
        // raw messages until Pending, retaining refused actor emits for retry.
        while good && Instant::now() < deadline {
            let Some(map) = mappings.as_deref_mut() else {
                good = false;
                break;
            };
            if !self.watched_namespace(&map.namespace) {
                good = false;
                break;
            }
            if !self.queued(map, endpoint, true, CALLBACKS) {
                self.driver
                    .receive
                    .recv_timeout(IDLE.min(deadline.saturating_duration_since(Instant::now())))
                    .ok();
                continue;
            }
            let wake = self.driver.wake.clone();
            let mut cx = Context::from_waker(&wake);
            match self
                .reader
                .as_mut()
                .map(|r| self.driver.read_ready(r, &mut cx))
            {
                Some(Ok(true)) if self.driver.events.is_empty() => break,
                Some(Ok(_)) => (),
                _ => {
                    good = false;
                    break;
                }
            }
        }
        drop(self.reader.take());
        self.driver.events.clear();
        good &= !self.driver.broken && Instant::now() < deadline;
        if good {
            self.gate.removals_acked.store(true, Ordering::Release);
        }
        let connection = self.connection.clone();
        let closed = matches!(
            self.drive(deadline, || false, connection.close()),
            Ok(Ok(()))
        );
        let executor = self.executor.clone();
        let finished = std::future::poll_fn(|cx| {
            if executor.is_empty() {
                return Poll::Ready(());
            }
            let _ = pin!(executor.tick()).poll(cx);
            if executor.is_empty() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let finished = self.drive(deadline, || false, finished).is_ok();
        let drained = closed && finished && self.executor.is_empty();
        Exit {
            clean: good && drained && !self.gate.refused(),
            drained,
        }
    }
}

#[cfg(test)]
pub(super) fn test_close_with_queued_callbacks(
    address: String,
    endpoint: Endpoint,
    value: Activation,
    emit: impl FnOnce(),
) -> bool {
    let setup = Instant::now() + WAIT;
    let mut bus = Bus::connect(address, setup, || false).unwrap();
    assert!(bus.matches(true, setup, || false));
    let namespace = bus.namespace(setup, || false).unwrap();
    bus.bind(&namespace);
    let mut mappings = Mappings::new(namespace);
    assert!(mappings.restore(vec![value]));
    emit();
    bus.close(true, Some(&mut mappings), &endpoint, Instant::now() + WAIT)
        .clean
}

pub(super) fn run(
    session: Session,
    endpoint: Endpoint,
    address: String,
    controls: mpsc::Receiver<Control>,
    shared: Arc<Shared>,
    setup: Instant,
) -> Exit {
    let cancelled = || shared.stop.load(Ordering::Acquire) || endpoint.stopping();
    let Ok(mut bus) = Bus::connect(address, setup, cancelled) else {
        shared.failed.store(true, Ordering::Release);
        // No Bus/SDK executor was returned; the original setup future and its
        // socket captures have been destroyed before this worker can join.
        return Exit {
            clean: false,
            drained: true,
        };
    };
    let mut good = bus.matches(true, setup, cancelled);
    let namespace = good
        .then(|| {
            bus.drive(setup, cancelled, Bus::lookup(bus.connection.clone(), true))
                .and_then(|v| v)
        })
        .and_then(std::result::Result::ok);
    let Some(namespace) = namespace else {
        shared.failed.store(true, Ordering::Release);
        return bus.close(false, None, &endpoint, shared.cleanup_deadline());
    };
    bus.bind(&namespace);
    let mut mappings = Mappings::new(namespace);
    let mut restoring = true;
    while !cancelled() && good {
        match controls.try_recv() {
            #[cfg(test)]
            Ok(Control::PauseBatch(pause, reply)) => {
                bus.batch_pause = Some(pause);
                reply.send(()).unwrap();
            }
            Ok(Control::Restore(values, reply)) => {
                let current =
                    bus.batch_namespace(&mappings.namespace, Instant::now() + WAIT, cancelled);
                let accepted = restoring && current && mappings.restore(values);
                let _ = reply.try_send(accepted);
                good &= accepted;
            }
            Ok(Control::Finish(reply)) => {
                let current =
                    bus.batch_namespace(&mappings.namespace, Instant::now() + WAIT, cancelled);
                let accepted = restoring && current;
                if accepted {
                    restoring = false;
                    shared.ready.store(true, Ordering::Release);
                }
                let _ = reply.try_send(accepted);
                good &= accepted;
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => (),
        }
        if !bus.driver.events.is_empty() {
            match bus.route_batch(&mut mappings, &endpoint, Instant::now() + WAIT, cancelled) {
                Ok(Batch::Blocked) => {
                    bus.driver.receive.recv_timeout(IDLE).ok();
                    continue;
                }
                Ok(Batch::Stopped) => break,
                Ok(Batch::Complete) => (),
                Err(()) => {
                    good = false;
                    break;
                }
            }
        }
        if !restoring {
            match endpoint.try_next() {
                Ok(work) => {
                    let Some(dispatch) = work.begin() else {
                        continue;
                    };
                    let resources = dispatch.resources();
                    let command = dispatch.command().clone();
                    let until = dispatch.deadline().min(Instant::now() + WAIT);
                    let stop = || cancelled() || dispatch.cancelled();
                    let current = command.valid()
                        && command.binding.session == session
                        && bus.namespace(until, stop).ok().as_ref() == Some(&mappings.namespace);
                    let outcome = if !current {
                        Outcome::Refused {
                            code: Refusal::NotReady,
                        }
                    } else {
                        let connection = bus.connection.clone();
                        let owner =
                            zbus::names::UniqueName::try_from(mappings.namespace.owner.clone())
                                .unwrap();
                        match &command.action {
                            Action::Post { title, body, .. } => {
                                if mappings.rows.len() == wire::MAPPINGS {
                                    Outcome::Refused {
                                        code: Refusal::Capacity,
                                    }
                                } else {
                                    let escaped =
                                        zerus_client_core::linux_notify::escape_body(body);
                                    match bus.drive_effect(
                                        &mut mappings,
                                        &endpoint,
                                        until,
                                        stop,
                                        true,
                                        native::notify_async(&connection, owner, title, &escaped),
                                    ) {
                                        Ok(Ok(reply))
                                            if reply_from(&reply, &mappings.namespace.owner) =>
                                        {
                                            match reply.body().deserialize::<u32>() {
                                                Ok(id) => {
                                                    let collision = mappings.rows.contains_key(&id);
                                                    let outcome = mappings.adopt(&command, id);
                                                    good &= !collision;
                                                    outcome
                                                }
                                                Err(_) => Outcome::Unknown {
                                                    code: Uncertainty::NativeError,
                                                },
                                            }
                                        }
                                        Ok(Ok(_)) => Outcome::Unknown {
                                            code: Uncertainty::Transport,
                                        },
                                        Ok(Err(_)) => Outcome::Unknown {
                                            code: Uncertainty::NativeError,
                                        },
                                        Err(()) => Outcome::Unknown {
                                            code: if Instant::now() >= until {
                                                Uncertainty::Deadline
                                            } else {
                                                Uncertainty::Transport
                                            },
                                        },
                                    }
                                }
                            }
                            Action::Withdraw { token, native_id } => {
                                if !mappings.owns(token, native_id) {
                                    Outcome::Refused {
                                        code: Refusal::NotReady,
                                    }
                                } else {
                                    let id = mappings.id(native_id).unwrap();
                                    // The actor already tombstoned this original withdrawal.
                                    mappings.rows.remove(&id);
                                    mappings.tokens.remove(&id);
                                    match bus.drive_effect(
                                        &mut mappings,
                                        &endpoint,
                                        until,
                                        stop,
                                        false,
                                        native::close_notification_async(&connection, owner, id),
                                    ) {
                                        Ok(Ok(reply))
                                            if reply_from(&reply, &mappings.namespace.owner)
                                                && reply.body().deserialize::<()>().is_ok() =>
                                        {
                                            Outcome::Delivered {
                                                native_id: native_id.clone(),
                                            }
                                        }
                                        Ok(Ok(_)) => Outcome::Unknown {
                                            code: Uncertainty::Transport,
                                        },
                                        Ok(Err(_)) => Outcome::Unknown {
                                            code: Uncertainty::NativeError,
                                        },
                                        Err(()) => Outcome::Unknown {
                                            code: if Instant::now() >= until {
                                                Uncertainty::Deadline
                                            } else {
                                                Uncertainty::Transport
                                            },
                                        },
                                    }
                                }
                            }
                        }
                    };
                    // Route callbacks already captured by this original call
                    // while its input/output/callback charge is still retained.
                    // The actor can defer an early event until this exact post
                    // commits; it cannot be turned into a new post or binding.
                    // A stopped batch preserves good for the normal Closed
                    // drain; only an actual namespace/transport error taints it.
                    if good {
                        good &= bus
                            .route_batch(&mut mappings, &endpoint, until, stop)
                            .is_ok();
                    }
                    // The original native future and its captures have returned
                    // or been destroyed before the per-command charge is freed.
                    drop(resources);
                    dispatch.finish(outcome);
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => break,
                Err(mpsc::TryRecvError::Empty) => (),
            }
        }
        let _ = bus.drive(
            Instant::now() + IDLE,
            cancelled,
            std::future::pending::<()>(),
        );
        good &= !bus.driver.broken && !bus.gate.refused();
    }
    shared.ready.store(false, Ordering::Release);
    let exit = bus.close(
        good,
        Some(&mut mappings),
        &endpoint,
        shared.cleanup_deadline(),
    );
    drop(mappings);
    shared.failed.store(!exit.clean, Ordering::Release);
    exit
}

pub(super) fn reply_from(message: &zbus::Message, owner: &str) -> bool {
    let header = message.header();
    header.message_type() == zbus::message::Type::MethodReturn
        && header.sender().map(|sender| sender.as_str()) == Some(owner)
}
