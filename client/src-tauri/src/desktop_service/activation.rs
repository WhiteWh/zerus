//! One authenticated, serial click read per original service connection.
//! A lost read is never replayed. Navigation has no writer or callback authority.
use super::*;
use std::{fmt, sync::atomic::AtomicBool};
use zerus_client_core::{
    desktop::{activation as wire, drain::Charge, relay, ui_requests},
    run as runner, Invocation,
};

const RETRY: Duration = Duration::from_secs(60);

mod diagnostic;
use diagnostic::Reason;

#[derive(Debug, PartialEq)]
enum ReadError {
    Permanent,
    Retryable(Reason),
}
impl ReadError {
    fn preparation(error: std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidInput => {
                Self::Permanent
            }
            // Capacity and unknown registry errors do not establish a permanent
            // identity refusal. A retry is a fresh read, never a replay.
            _ => Self::Retryable(Reason::Preparation),
        }
    }
}

pub(crate) type Sink = Arc<dyn Fn(Delivery) + Send + Sync>;

/// Only this module can create a delivery, after the original reply matches.
/// The non-Clone envelope keeps the exact original Open and its correlation.
pub(crate) struct Delivery {
    completion: Completion,
}
impl Delivery {
    pub(crate) fn open(&self) -> &wire::Open {
        &self.completion.0.open
    }
    pub(crate) fn completion(&self) -> Completion {
        self.completion.clone()
    }
    pub(crate) fn current(&self) -> bool {
        self.completion.current()
    }
}
#[derive(Clone)]
pub(crate) struct Completion(Arc<Hold>);
struct Hold {
    // Preserve the original post/click/token correlation through the last
    // desk/native-main callback. This remains ephemeral memory only.
    open: wire::Open,
    shared: Weak<Shared>,
    stop: Arc<AtomicBool>,
    epoch: String,
    done: Mutex<Option<oneshot::Sender<()>>>,
}
impl Drop for Hold {
    fn drop(&mut self) {
        if let Some(done) = self
            .done
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = done.send(());
        }
    }
}
impl Completion {
    pub(crate) fn current(&self) -> bool {
        let Some(shared) = self.0.shared.upgrade() else {
            return false;
        };
        let snapshot = shared
            .snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        !self.0.stop.load(Ordering::Acquire)
            && !shared.stopped.load(Ordering::Acquire)
            && snapshot.connected
            && snapshot.epoch.as_deref() == Some(&self.0.epoch)
    }
}
impl fmt::Debug for Completion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ActivationCompletion")
    }
}
impl PartialEq for Completion {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

async fn admit(shared: &Shared, stop: &AtomicBool) -> Option<Charge> {
    loop {
        if shared.stopped.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
            return None;
        }
        if let Ok(charge) = shared.activation_rpc.admit(Role::Fleet) {
            return Some(charge);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
async fn backoff(shared: &Shared, stop: &AtomicBool) -> bool {
    #[cfg(test)]
    shared.activation_backoffs.fetch_add(1, Ordering::Release);
    // Test-only clock seam; production always waits a full minute. This is not
    // a configurable poll interval or an automatic replay of a previous UUID.
    #[cfg(test)]
    let delay = shared
        .activation_retry
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .unwrap_or(RETRY);
    #[cfg(not(test))]
    let delay = RETRY;
    let until = tokio::time::Instant::now() + delay;
    loop {
        if shared.stopped.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
            return false;
        }
        let remaining = until.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return true;
        }
        tokio::time::sleep(remaining.min(Duration::from_millis(50))).await;
    }
}

fn invocation(args: &[&str], input: Vec<u8>, timeout: Duration) -> Invocation {
    Invocation {
        args: args.iter().map(|s| (*s).into()).collect(),
        stdin: Some(input),
        timeout,
        request_id: None,
    }
}
fn supported(relay: &relay::Connection, stop: &AtomicBool, charge: Charge) -> bool {
    let Ok(hgs) = relay.executable() else {
        return false;
    };
    let Ok(bytes) = runner::run_owned(
        hgs,
        &invocation(&["--help"], vec![], RPC_WAIT),
        &[],
        stop,
        |command| command.spawn(),
        Arc::new(charge),
    ) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
    };
    [ui_requests::HELP_MARKER, wire::HELP_MARKER]
        .iter()
        .all(|wanted| text.lines().any(|line| line.trim() == *wanted))
}
fn read(
    shared: &Shared,
    relay: &relay::Connection,
    stop: &AtomicBool,
    charge: Charge,
    configuration: &str,
    epoch: &str,
) -> Result<Option<wire::Open>, ReadError> {
    let id = uuid::Uuid::new_v4().to_string();
    let registration = shared
        .responder
        .as_ref()
        .ok_or(ReadError::Permanent)?
        .prepare_ui_request(
            uuid::Uuid::new_v4().to_string(),
            ui_requests::Request::ActivationWait {
                request_id: id.clone(),
            },
        )
        .map_err(ReadError::preparation)?;
    let input = serde_json::to_vec(registration.envelope()).map_err(|_| ReadError::Permanent)?;
    if input.len() > zerus_watch_protocol::CONTROL_LINE_BYTES {
        return Err(ReadError::Permanent);
    }
    let resources: Arc<dyn Send + Sync> = Arc::new((charge, registration.resources()));
    let mut failure = None;
    let bytes = runner::run_owned_observed(
        relay
            .executable()
            .map_err(|_| ReadError::Retryable(Reason::Selection))?,
        &invocation(
            &["desktop", "frontend-request", "--json"],
            input,
            wire::CLIENT_WAIT,
        ),
        &[],
        stop,
        move |command| {
            let child = command.spawn()?;
            // Binding refusal still returns the ORIGINAL child to its owner.
            let _ = registration.bind_original(&child);
            Ok(child)
        },
        resources,
        |error, stderr| failure = Some(Reason::runner(error, stderr)),
    )
    .map_err(|error| {
        ReadError::Retryable(failure.unwrap_or_else(|| Reason::runner(&error, None)))
    })?;
    if bytes.len() > zerus_watch_protocol::CONTROL_LINE_BYTES {
        return Err(ReadError::Retryable(Reason::OutputLimit));
    }
    let reply: wire::Reply = serde_json::from_slice(&bytes)
        .map_err(|_| ReadError::Retryable(Reason::reply(&bytes, &id)))?;
    if !reply.matches(&id, configuration, epoch) {
        return Err(ReadError::Retryable(Reason::ReplyIdentity));
    }
    // Wayland activation tokens belong only to Linux. A Mac delivery comes
    // from its native host and has no platform context; never interpret a
    // foreign platform's presentation hint as a Mac click.
    #[cfg(target_os = "macos")]
    if reply
        .activation
        .as_ref()
        .is_some_and(|open| open.platform_context.is_some())
    {
        return Err(ReadError::Retryable(Reason::ForeignContext));
    }
    Ok(reply.activation)
}

pub(super) async fn run(shared: Arc<Shared>, configuration: String, stop: Arc<AtomicBool>) {
    let Some(sink) = shared.activation.clone() else {
        return;
    };
    if shared.responder.is_none() {
        return;
    }
    // This is the SAME connection selected by the original catalog/watch loop.
    // executable() performs its cheap generation check before every child.
    let Some(relay) = shared
        .relay
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
    else {
        return;
    };
    let Some(charge) = admit(&shared, &stop).await else {
        return;
    };
    let (cached, cancelled) = (relay.clone(), stop.clone());
    if !tauri::async_runtime::spawn_blocking(move || supported(&cached, &cancelled, charge))
        .await
        .unwrap_or(false)
    {
        return;
    }
    loop {
        let Some(charge) = admit(&shared, &stop).await else {
            return;
        };
        let epoch = {
            let snapshot = shared
                .snapshot
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            snapshot.connected.then(|| snapshot.epoch.clone()).flatten()
        };
        let Some(epoch) = epoch else {
            drop(charge);
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let (state, cached, cancelled, config, expected) = (
            shared.clone(),
            relay.clone(),
            stop.clone(),
            configuration.clone(),
            epoch.clone(),
        );
        let result = tauri::async_runtime::spawn_blocking(move || {
            read(&state, &cached, &cancelled, charge, &config, &expected)
        })
        .await;
        let open = match result {
            Ok(Ok(Some(open))) => open,
            Ok(Ok(None)) => continue, // Idle is a new read, never a replay.
            Ok(Err(ReadError::Permanent)) => {
                #[cfg(test)]
                shared.activation_permanent.store(true, Ordering::Release);
                if !stop.load(Ordering::Acquire) && !shared.stopped.load(Ordering::Acquire) {
                    eprintln!("zerus-client: desktop activation identity unavailable");
                }
                return; // Only this reader stops; catalog/watch remain connected.
            }
            result => {
                let reason = match result {
                    Ok(Err(ReadError::Retryable(reason))) => reason,
                    _ => Reason::WorkerJoin,
                };
                if !stop.load(Ordering::Acquire) && !shared.stopped.load(Ordering::Acquire) {
                    eprintln!(
                        "zerus-client: desktop activation unavailable: {}",
                        reason.code()
                    );
                }
                if !backoff(&shared, &stop).await {
                    return;
                }
                // A new admission is still required. Unknown child/pipe guards
                // keep this dedicated slot occupied until their real release.
                continue;
            }
        };
        let (done, mut received) = oneshot::channel();
        let completion = Completion(Arc::new(Hold {
            open,
            shared: Arc::downgrade(&shared),
            stop: stop.clone(),
            epoch,
            done: Mutex::new(Some(done)),
        }));
        let delivery = Delivery { completion };
        if !delivery.current() {
            return;
        }
        sink(delivery);
        // At most ONE delivery is in the desk/native-main queues. A blocked
        // view cannot accumulate clicks. Stop invalidates the held envelope;
        // it does not wait for the UI before joining the original relay child.
        loop {
            if stop.load(Ordering::Acquire) || shared.stopped.load(Ordering::Acquire) {
                return;
            }
            tokio::select! {
                _ = &mut received => break,
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests;
