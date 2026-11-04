use super::*;
use std::future::Future;
use std::pin::{pin, Pin};
use std::task::{Context, Poll, Wake, Waker};
use zbus::export::futures_core::Stream;

pub(super) enum Exit {
    Clean,
    Unknown,
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
struct Driver<'a> {
    scope: &'a Scope,
    receiver: &'a mpsc::Receiver<()>,
    waker: Waker,
}
#[cfg(test)]
struct TickCapture<'a>(&'a Hooks);
#[cfg(test)]
impl Drop for TickCapture<'_> {
    fn drop(&mut self) {
        self.0.at(Hook::TickDropped);
    }
}
// Test-only runnable admitted to this startup's existing executor. It carries
// no native input, body or ID. The test consumes its one retained task waker
// before CleanExit, and the original worker still owns the task handle.
#[cfg(test)]
#[derive(Default)]
pub(super) struct RunnableProbe {
    ready: AtomicBool,
    waker: Mutex<Option<Waker>>,
    pub(super) pending: AtomicBool,
    pub(super) finished: AtomicBool,
    pub(super) dropped: AtomicBool,
}
#[cfg(test)]
impl RunnableProbe {
    pub(super) fn wake(&self) {
        let waker = {
            let mut waker = self.waker.lock().unwrap();
            self.ready.store(true, Ordering::Release);
            waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}
#[cfg(test)]
struct ProbeFuture(Arc<RunnableProbe>);
#[cfg(test)]
impl Future for ProbeFuture {
    type Output = ();
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let mut waker = self.0.waker.lock().unwrap();
        if self.0.ready.load(Ordering::Acquire) {
            self.0.finished.store(true, Ordering::Release);
            Poll::Ready(())
        } else {
            *waker = Some(context.waker().clone());
            self.0.pending.store(true, Ordering::Release);
            Poll::Pending
        }
    }
}
#[cfg(test)]
impl Drop for ProbeFuture {
    fn drop(&mut self) {
        let waker = self.0.waker.lock().unwrap().take();
        drop(waker);
        self.0.dropped.store(true, Ordering::Release);
    }
}
impl Driver<'_> {
    fn drive<T>(
        &self,
        executor: Option<&zbus::Executor<'static>>,
        deadline: Instant,
        cancellable: bool,
        work: impl Future<Output = T>,
    ) -> Result<T, ()> {
        let mut work = pin!(work);
        loop {
            if Instant::now() >= deadline
                || (cancellable && self.scope.cancel.load(Ordering::Acquire))
            {
                return Err(());
            }
            let mut context = Context::from_waker(&self.waker);
            if let Poll::Ready(value) = work.as_mut().poll(&mut context) {
                if Instant::now() >= deadline
                    || (cancellable && self.scope.cancel.load(Ordering::Acquire))
                {
                    return Err(());
                }
                return Ok(value);
            }
            if let Some(executor) = executor {
                for _ in 0..16 {
                    let tick = executor.tick();
                    if pin!(tick).poll(&mut context).is_pending() {
                        break;
                    }
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let _ = self
                .receiver
                .recv_timeout(remaining.min(Duration::from_millis(10)));
        }
    }
}
async fn match_call(connection: &zbus::Connection, method: &str, rule: &str) -> Result<(), ()> {
    let reply = connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            method,
            &(rule,),
        )
        .await
        .map_err(|_| ())?;
    if reply.header().sender().map(|s| s.as_str()) != Some("org.freedesktop.DBus") {
        return Err(());
    }
    reply.body().deserialize::<()>().map_err(|_| ())
}
fn rules() -> [String; 2] {
    ["type='signal',interface='org.freedesktop.Notifications',path='/org/freedesktop/Notifications'".into(),
     "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.freedesktop.Notifications'".into()]
}
pub(super) fn run(
    address: String,
    scope: Arc<Scope>,
    receiver: mpsc::Receiver<()>,
    setup: Instant,
    hooks: Hooks,
) -> Exit {
    hooks.at(Hook::BeforeNative);
    if Instant::now() >= setup || tokio::runtime::Handle::try_current().is_ok() {
        return Exit::Unknown;
    }
    // The narrow known-no-native-work path still needs the actual thread join.
    if scope.cancel.load(Ordering::Acquire) {
        return Exit::Clean;
    }
    let notify = Arc::new(Notify(scope.wake.clone()));
    let probe = Arc::downgrade(&notify);
    let driver = Driver {
        scope: &scope,
        receiver: &receiver,
        waker: Waker::from(notify),
    };
    let result = session(address, &driver, setup, &hooks);
    let deadline = scope.cleanup().ok();
    drop(driver);
    // Only test builds carry callback captures; production Hooks is empty.
    #[cfg(test)]
    drop(hooks);
    // Reactor readiness callbacks may briefly retain a waker after its task
    // completed. Its exact last capture must disappear, not just stop waking.
    while probe.strong_count() != 0 {
        if deadline.is_none_or(|d| Instant::now() >= d) {
            return Exit::Unknown;
        }
        let _ = receiver.recv_timeout(Duration::from_millis(1));
    }
    if result.is_ok() && deadline.is_some_and(|d| Instant::now() < d) {
        Exit::Clean
    } else {
        Exit::Unknown
    }
}
fn session(address: String, driver: &Driver<'_>, setup: Instant, hooks: &Hooks) -> Result<(), ()> {
    let (socket, ingress) = driver
        .drive(None, setup, true, ingress::connect(address))?
        .map_err(|_| ())?;
    let builder = zbus::connection::Builder::socket(socket)
        .internal_executor(false)
        .max_queued(RAW);
    let connection = driver
        .drive(None, setup, true, builder.build())?
        .map_err(|_| ())?;
    let executor = connection.executor().clone();
    // A live async-io socket reader is unfinished. Tokio's always-empty
    // executor cannot pass this check; construction was outside Tokio too.
    if connection.is_closed() || executor.is_empty() {
        return Err(());
    }
    #[cfg(test)]
    {
        driver.scope.state.lock().map_err(|_| ())?.peer =
            connection.unique_name().map(|name| name.to_string());
    }
    hooks.at(Hook::Connected);
    let rules = rules();
    let mut setup_ok = true;
    for rule in &rules {
        if driver
            .drive(
                Some(&executor),
                setup,
                true,
                match_call(&connection, "AddMatch", rule),
            )
            .and_then(|r| r)
            .is_err()
        {
            setup_ok = false;
            break;
        }
        hooks.at(Hook::Matched);
    }
    let mut good =
        setup_ok && Instant::now() < setup && !driver.scope.cancel.load(Ordering::Acquire);
    if good {
        let mut readers = [
            zbus::MessageStream::from(&connection),
            zbus::MessageStream::from(&connection),
        ];
        good &= readers.iter().all(|r| r.max_queued() == RAW);
        if good && driver.scope.ready(setup).is_ok() {
            hooks.at(Hook::Raw);
            // This lexical scope owns the ACTUAL pinned future, including
            // pin!'s hidden storage. It ends before DropReaders and removals.
            // A Pending async-executor tick owns its registered sleeper; merely
            // dropping its Pin reference would not destroy that future.
            #[cfg(test)]
            let _runnable = hooks
                .runnable
                .as_ref()
                .map(|probe| executor.spawn(ProbeFuture(probe.clone()), "private-idle-wake-probe"));
            let next_tick = || async {
                #[cfg(test)]
                let _capture = {
                    hooks.at(Hook::TickCreated);
                    TickCapture(hooks)
                };
                executor.tick().await
            };
            let mut tick = pin!(next_tick());
            while !driver.scope.cancel.load(Ordering::Acquire) {
                let mut progress = false;
                let mut context = Context::from_waker(&driver.waker);
                for reader in &mut readers {
                    if driver.scope.cancel.load(Ordering::Acquire) {
                        break;
                    }
                    match Pin::new(reader).poll_next(&mut context) {
                        Poll::Ready(Some(Ok(message))) => {
                            // No body decoding, copying, retention or routing.
                            hooks.at(Hook::Message);
                            #[cfg(test)]
                            if message.header().member().map(|m| m.as_str())
                                == Some("IdleWakeProbe")
                            {
                                hooks.at(Hook::ProbeSignal);
                            }
                            drop(message);
                            progress = true;
                        }
                        Poll::Ready(_) => {
                            good = false;
                            let _ = driver.scope.close();
                        }
                        Poll::Pending => (),
                    }
                }
                if driver.scope.cancel.load(Ordering::Acquire) {
                    break;
                }
                if tick.as_mut().poll(&mut context).is_ready() {
                    // Only a completed tick is reset. A Pending tick and its
                    // wake registration survive raw progress and idle recv.
                    tick.set(next_tick());
                    progress = true;
                }
                if driver.scope.cancel.load(Ordering::Acquire) {
                    break;
                }
                if progress {
                    continue;
                }
                #[cfg(test)]
                hooks.at(Hook::BeforePark);
                // Both readers and the retained tick were Pending. No token
                // pre-drain: a coalesced Full or a poll-to-park wake is retained.
                let received = driver.receiver.recv();
                #[cfg(test)]
                hooks.at(Hook::AfterPark);
                if received.is_err() {
                    good = false;
                    let _ = driver.scope.close();
                    break;
                }
            }
        } else {
            good = false;
        }
        hooks.at(Hook::DropReaders);
        drop(readers); // BOTH receivers before ANY RemoveMatch.
    }
    let cleanup = driver.scope.cleanup()?;
    if setup_ok {
        for rule in &rules {
            good &= driver
                .drive(
                    Some(&executor),
                    cleanup,
                    false,
                    match_call(&connection, "RemoveMatch", rule),
                )
                .and_then(|r| r)
                .is_ok();
        }
    }
    if good && Instant::now() < cleanup {
        // BOTH actual readers are gone and every sequential removal succeeded.
        // This private fact alone cannot waive an error, refund B or finish.
        ingress.removals_acked.store(true, Ordering::Release);
    }
    hooks.at(Hook::DropConnection);
    good &= driver
        .drive(Some(&executor), cleanup, false, connection.clone().close())
        .and_then(|r| r.map_err(|_| ()))
        .is_ok();
    // Retain this exact connection's task handle until the socket reader
    // returns its local-close EOF. Dropping it now would CANCEL a pending
    // frame, which is deliberately sticky Unknown even during local close.
    let finished = std::future::poll_fn(|context| {
        if executor.is_empty() {
            return Poll::Ready(());
        }
        let tick = executor.tick();
        let _ = pin!(tick).poll(context);
        if executor.is_empty() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    good &= driver
        .drive(Some(&executor), cleanup, false, finished)
        .is_ok();
    good &= executor.is_empty();
    drop(connection);
    drop(executor);
    good &= !ingress.refused();
    if good {
        Ok(())
    } else {
        Err(())
    }
}
