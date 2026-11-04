//! Actor-owned notification dispatch using the existing effect ledger. Native
//! adapters receive only original queued work, never generic desktop requests.
use super::store::Store;
use anyhow::{ensure, Context, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{BTreeSet, VecDeque};
use std::sync::{
    atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant};
use zerus_client_core::{
    attention,
    desktop::notifications::{
        self as wire, Action, Activation, Binding, Command, Outcome, Receipt, Refusal, Session,
        Uncertainty,
    },
};
use zerus_desktop_ledger::{self as ledger, Attempt, Identity, Kind, Reserve, State, Target};

#[cfg(target_os = "linux")]
mod closed;
#[cfg(target_os = "linux")]
pub(super) mod frontend;
#[cfg(target_os = "linux")]
pub(super) mod linux;

pub const COMMAND_WAIT: Duration = Duration::from_secs(10);
const EVENTS: usize = 32;
const QUEUED: u8 = 0;
const STARTED: u8 = 1;
const CANCELLED: u8 = 2;

pub(super) mod activation;
pub(super) mod attachment;
#[cfg(unix)]
pub(super) mod host;
pub(super) mod preparation;
pub(super) mod service;
#[cfg(any(windows, test))]
pub(super) mod windows;

struct Shared {
    cancel: AtomicBool,
    admission: AtomicU8,
    _owner: Option<Arc<dyn Send + Sync>>,
    reply: Mutex<Option<Finished>>,
}
#[derive(Clone)]
struct Finished {
    at: Instant,
    receipt: Receipt,
}
impl Shared {
    fn cancel_queued(&self) {
        let _ =
            self.admission
                .compare_exchange(QUEUED, CANCELLED, Ordering::AcqRel, Ordering::Acquire);
    }
    fn finish(&self, receipt: Receipt) {
        if let Ok(mut slot) = self.reply.lock() {
            if slot.is_none() {
                *slot = Some(Finished {
                    at: Instant::now(),
                    receipt,
                });
            }
        }
    }
}

/// A queue admission is not a native invocation. Dropping unstarted work is a
/// confirmed refusal; dropping a started Dispatch without a receipt is unknown.
pub(super) struct Work {
    command: Option<Command>,
    shared: Arc<Shared>,
    until: Instant,
}
impl Work {
    pub fn begin(mut self) -> Option<Dispatch> {
        if self.shared.cancel.load(Ordering::Acquire)
            || Instant::now() >= self.until
            || self
                .shared
                .admission
                .compare_exchange(QUEUED, STARTED, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return None;
        }
        Some(Dispatch {
            command: self.command.take().expect("original queued command"),
            shared: self.shared.clone(),
            until: self.until,
            finished: false,
        })
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        if let Some(command) = &self.command {
            self.shared.finish(Receipt {
                binding: command.binding.clone(),
                outcome: Outcome::Refused {
                    code: Refusal::Cancelled,
                },
            });
        }
    }
}

/// Retain resources() with every original native callback/child/pipe. Completing
/// the request does not release its charge; the actor waits for those resources.
pub(super) struct Dispatch {
    command: Command,
    shared: Arc<Shared>,
    until: Instant,
    finished: bool,
}
impl Dispatch {
    pub fn command(&self) -> &Command {
        &self.command
    }
    pub fn deadline(&self) -> Instant {
        self.until
    }
    pub fn cancelled(&self) -> bool {
        self.shared.cancel.load(Ordering::Acquire) || Instant::now() >= self.until
    }
    pub fn resources(&self) -> Arc<dyn Send + Sync> {
        self.shared.clone()
    }
    pub fn finish(self, outcome: Outcome) {
        let receipt = Receipt {
            binding: self.command.binding.clone(),
            outcome,
        };
        self.finish_receipt(receipt);
    }
    pub fn finish_receipt(mut self, receipt: Receipt) {
        let receipt = if receipt.matches(&self.command) {
            receipt
        } else {
            Receipt {
                binding: self.command.binding.clone(),
                outcome: Outcome::Unknown {
                    code: Uncertainty::Transport,
                },
            }
        };
        self.shared.finish(receipt);
        self.finished = true;
    }
}
impl Drop for Dispatch {
    fn drop(&mut self) {
        if !self.finished {
            self.shared.finish(Receipt {
                binding: self.command.binding.clone(),
                outcome: Outcome::Unknown {
                    code: Uncertainty::HostLost,
                },
            });
        }
    }
}

pub(super) enum Event {
    Clicked(Activation),
    Closed(Activation),
}
struct QueuedEvent {
    event: Event,
    count: Arc<AtomicUsize>,
}
impl Drop for QueuedEvent {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
    }
}
/// Created with Outbox, held only by the original native adapter/host bridge.
/// This object is not deserializable and has no generic IPC constructor.
pub(super) struct Endpoint {
    receive: mpsc::Receiver<Work>,
    events: mpsc::SyncSender<QueuedEvent>,
    event_count: Arc<AtomicUsize>,
    stopping: Arc<AtomicBool>,
    platform: wire::Platform,
    _owner: Option<Arc<dyn Send + Sync>>,
}
impl Endpoint {
    pub fn try_next(&self) -> std::result::Result<Work, mpsc::TryRecvError> {
        self.receive.try_recv()
    }
    pub fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }
    pub fn emit(&self, event: Event) -> bool {
        let (Event::Clicked(value) | Event::Closed(value)) = &event;
        // Closing an already admitted notification may complete during host
        // shutdown. Keep that exact tombstone drainable; never open a late UI.
        if (self.stopping() && !matches!(event, Event::Closed(_)))
            || !value.valid_for(self.platform)
            || (matches!(event, Event::Closed(_)) && value.platform_context.is_some())
        {
            return false;
        }
        // The same slot travels through the channel AND actor deferral. Moving
        // an early callback off the channel must not double the event budget.
        if self
            .event_count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < EVENTS).then_some(n + 1)
            })
            .is_err()
        {
            return false;
        }
        self.events
            .try_send(QueuedEvent {
                event,
                count: self.event_count.clone(),
            })
            .is_ok()
    }
}

struct Pending {
    binding: Binding,
    command: Option<Command>,
    request: Reserve,
    shared: Arc<Shared>,
    until: Instant,
    terminal: Option<Receipt>,
    committed: bool,
}
pub(super) enum Offered {
    Queued { request_id: String },
    Existing(Attempt),
}
pub(super) struct RestorePage {
    pub after: i64,
    pub through: i64,
    pub complete: bool,
    pub mappings: Vec<Activation>,
}
pub(super) struct Outbox {
    session: Session,
    send: mpsc::SyncSender<Work>,
    events: mpsc::Receiver<QueuedEvent>,
    callbacks: VecDeque<QueuedEvent>,
    event_count: Arc<AtomicUsize>,
    stopping: Arc<AtomicBool>,
    pending: Vec<Pending>,
    error: Option<&'static str>,
    owner: Option<Arc<dyn Send + Sync>>,
    retiring: bool,
}
impl Outbox {
    /// No native constructor, OS call, writer grant or delivery happens here.
    /// The platform lifecycle binds Endpoint to its original owned host and
    /// retains/joins that host independently of operation_drained().
    pub fn new(session: Session) -> Result<(Self, Endpoint)> {
        Self::with_owner(session, None)
    }
    /// The actor supplies its ORIGINAL service lifetime hold. Keep it in the
    /// native endpoint and each original command, beyond Rust handle loss.
    pub fn new_owned(session: Session, owner: Arc<dyn Send + Sync>) -> Result<(Self, Endpoint)> {
        Self::with_owner(session, Some(owner))
    }
    fn with_owner(
        session: Session,
        owner: Option<Arc<dyn Send + Sync>>,
    ) -> Result<(Self, Endpoint)> {
        ensure!(session.valid(), "invalid notification host session");
        let (send, receive) = mpsc::sync_channel(wire::IN_FLIGHT);
        let (events, event_receive) = mpsc::sync_channel(EVENTS);
        let event_count = Arc::new(AtomicUsize::new(0));
        let stopping = Arc::new(AtomicBool::new(false));
        let platform = session.platform;
        Ok((
            Self {
                session,
                send,
                events: event_receive,
                callbacks: VecDeque::new(),
                event_count: event_count.clone(),
                stopping: stopping.clone(),
                pending: Vec::new(),
                error: None,
                owner: owner.clone(),
                retiring: false,
            },
            Endpoint {
                receive,
                events,
                event_count,
                stopping,
                platform,
                _owner: owner,
            },
        ))
    }

    /// Actor-only convenience: authority stays in this original private Session.
    /// A new request ID is not a retry grant; reserve returns consumed edges.
    pub fn offer_intent(
        &mut self,
        store: &mut Store,
        intent_id: &str,
        revision: u64,
        now: Instant,
    ) -> Result<Offered> {
        self.require_current_writer(store)?;
        self.offer(
            store,
            Reserve {
                request_id: uuid::Uuid::new_v4().to_string(),
                intent_id: intent_id.into(),
                revision,
                epoch: self.session.writer_epoch.clone(),
                service_epoch: self.session.service_epoch.clone(),
                adapter: self.session.adapter.clone(),
                lease: self.session.lease.clone(),
                retry_of: None,
            },
            now,
        )
    }

    pub fn available(&self) -> bool {
        !self.stopping.load(Ordering::Acquire)
            && self.error.is_none()
            && self.pending.len() < wire::IN_FLIGHT
    }

    /// Revoke callbacks and cancel this original queued work. This does not
    /// manufacture a native close receipt or release its retained resources.
    pub fn retire_intent(&mut self, store: &mut Store, intent_id: &str) -> Result<()> {
        ledger::withdraw(&mut store.db, intent_id, 1)?;
        for pending in &self.pending {
            if pending.binding.intent_id == intent_id {
                pending.shared.cancel_queued();
            }
        }
        Ok(())
    }

    /// Bound the policy-off race independently of the live-edge walk. Work
    /// already begun retains its receipt/resources and is withdrawn afterward.
    pub fn cancel_queued_posts(&self) {
        for pending in &self.pending {
            if pending
                .command
                .as_ref()
                .is_some_and(|c| matches!(c.action, Action::Post { .. }))
            {
                pending.shared.cancel_queued();
            }
        }
    }

    /// Called by the actor for an intent derived from its own committed fleet
    /// observations. Existing/historic/unknown attempts never enqueue a send.
    pub fn offer(&mut self, store: &mut Store, request: Reserve, now: Instant) -> Result<Offered> {
        ensure!(
            !self.stopping.load(Ordering::Acquire),
            "notification outbox stopping"
        );
        ensure!(
            self.pending.len() < wire::IN_FLIGHT,
            "notification resources busy"
        );
        ensure!(
            request.service_epoch == self.session.service_epoch
                && request.epoch == self.session.writer_epoch
                && request.adapter == self.session.adapter
                && request.lease == self.session.lease,
            "notification host lease changed"
        );
        let reserved = if self.retiring {
            ledger::reserve_withdrawal(&mut store.db, &request)?
        } else {
            ledger::reserve(&mut store.db, &request)?
        };
        if !reserved.fresh {
            return Ok(Offered::Existing(reserved.attempt));
        }
        // Any error after this commit consumes the attempt. Recovery marks it
        // unknown; neither another host nor a later revision can replay it.
        let binding = Binding {
            session: self.session.clone(),
            request_id: request.request_id.clone(),
            intent_id: request.intent_id.clone(),
            revision: request.revision,
            seq: reserved.attempt.seq,
        };
        let prepared = (|| -> Result<Command> {
            let snapshot = ledger::native::inspect_original(&mut store.db, reserved)?;
            let saved = snapshot.snapshot();
            let action: Action = serde_json::from_str(saved.payload_json())?;
            check_identity(saved.identity(), &action)?;
            let command = Command {
                binding: binding.clone(),
                action,
            };
            ensure!(command.valid(), "invalid notification command");
            wire::encode(&wire::ToHost::Command(command.clone())).map_err(anyhow::Error::msg)?;
            Ok(command)
        })();
        let shared = Arc::new(Shared {
            cancel: AtomicBool::new(false),
            admission: AtomicU8::new(QUEUED),
            _owner: self.owner.clone(),
            reply: Mutex::new(None),
        });
        let until = now + COMMAND_WAIT;
        let command = match prepared {
            Ok(command) => command,
            Err(error) => {
                // No native work was created. Retain the original reservation
                // and frozen refusal even if its SQL settlement must wait.
                self.pending.push(Pending {
                    binding: binding.clone(),
                    command: None,
                    request,
                    shared,
                    until,
                    terminal: Some(Receipt {
                        binding,
                        outcome: Outcome::Refused {
                            code: Refusal::InvalidCommand,
                        },
                    }),
                    committed: false,
                });
                return Err(error);
            }
        };
        let work = Work {
            command: Some(command.clone()),
            shared: shared.clone(),
            until,
        };
        self.pending.push(Pending {
            binding,
            command: Some(command),
            request: request.clone(),
            shared,
            until,
            terminal: None,
            committed: false,
        });
        // On channel refusal, the original Work is dropped before native input.
        drop(self.send.try_send(work));
        Ok(Offered::Queued {
            request_id: request.request_id,
        })
    }

    pub fn poll(&mut self, store: &mut Store, now: Instant) -> Vec<Activation> {
        for pending in &mut self.pending {
            if !pending.committed {
                if pending.terminal.is_none() {
                    let released = Arc::strong_count(&pending.shared) == 1;
                    // The original completion and native resource release are
                    // independent. A retained callback/pipe must keep its charge,
                    // never hide an on-time receipt until the deadline wins.
                    let completed = pending
                        .shared
                        .reply
                        .lock()
                        .ok()
                        .and_then(|r| r.clone())
                        .filter(|r| {
                            r.at < pending.until
                                && pending
                                    .command
                                    .as_ref()
                                    .is_some_and(|command| r.receipt.matches(command))
                        });
                    if let Some(finished) = completed {
                        // Scheduling/SQL latency is not a native timeout. The
                        // timestamp comes from the original completion handle.
                        pending.terminal = Some(finished.receipt);
                    } else if now >= pending.until {
                        pending.shared.cancel.store(true, Ordering::Release);
                        pending.terminal = Some(Receipt {
                            binding: pending.binding.clone(),
                            outcome: if pending.shared.admission.load(Ordering::Acquire)
                                == CANCELLED
                            {
                                Outcome::Refused {
                                    code: Refusal::Cancelled,
                                }
                            } else {
                                Outcome::Unknown {
                                    code: Uncertainty::Deadline,
                                }
                            },
                        });
                    } else if released {
                        pending.terminal = Some(Receipt {
                            binding: pending.binding.clone(),
                            outcome: Outcome::Unknown {
                                code: Uncertainty::HostLost,
                            },
                        });
                    }
                }
                if let Some(receipt) = &pending.terminal {
                    let state = match receipt.outcome {
                        Outcome::Delivered { .. } => State::Delivered,
                        Outcome::Refused { .. } => State::Refused,
                        Outcome::Unknown { .. } => State::Unknown,
                    };
                    let result = serde_json::to_value(receipt)
                        .map_err(anyhow::Error::from)
                        .and_then(|receipt| {
                            ledger::finish(&mut store.db, &pending.request, state, &receipt)
                        });
                    if result.is_ok() {
                        pending.committed = true;
                    } else {
                        self.error = Some("notification_receipt_uncommitted");
                    }
                }
            }
        }
        self.pending
            .retain(|p| !p.committed || Arc::strong_count(&p.shared) != 1);
        let mut clicks = Vec::new();
        for _ in 0..EVENTS {
            let Ok(callback) = self.events.try_recv() else {
                break;
            };
            self.callbacks.push_back(callback);
        }
        // Visit every retained callback at most once per actor turn. Preserve
        // FIFO ordering while the original native/SQL settlement is pending.
        let mut blocked = BTreeSet::new();
        for _ in 0..self.callbacks.len() {
            let callback = self.callbacks.pop_front().expect("bounded callback queue");
            let (Event::Clicked(value) | Event::Closed(value)) = &callback.event;
            if blocked.contains(&value.request_id) || self.pending.iter().any(|pending| {
                !pending.committed
                    && pending.binding.request_id == value.request_id
                    && pending.command.as_ref().is_some_and(|command| {
                        matches!(&command.action, Action::Post { token, .. } if *token == value.token)
                    })
            }) {
                blocked.insert(value.request_id.clone());
                self.callbacks.push_back(callback);
                continue;
            }
            let retry = match self.current_activation(store, value) {
                Ok(Some(_)) if matches!(callback.event, Event::Clicked(_)) => {
                    clicks.push(value.clone());
                    false
                }
                Ok(Some(binding)) => {
                    if ledger::withdraw(&mut store.db, &binding.intent_id, binding.revision)
                        .is_err()
                    {
                        self.error = Some("notification_close_uncommitted");
                        true
                    } else {
                        false
                    }
                }
                Ok(None) => false,
                Err(_) => {
                    self.error = Some("notification_callback_unavailable");
                    true
                }
            };
            if retry {
                blocked.insert(value.request_id.clone());
                self.callbacks.push_back(callback);
            }
        }
        clicks
    }

    fn current_activation(&self, store: &Store, value: &Activation) -> Result<Option<Binding>> {
        self.activation_binding(store, value, false)
    }
    // Cleanup restoration may retain a withdrawn mapping for native removal.
    // The callback path above always requires the original intent to be live.
    fn activation_binding(
        &self,
        store: &Store,
        value: &Activation,
        cleanup: bool,
    ) -> Result<Option<Binding>> {
        if !value.valid_for(self.session.platform) {
            return Ok(None);
        }
        // Callbacks carry the original post UUID, including after a service or
        // native host upgrade. Resolve its exact delivered mapping by index;
        // never manufacture a new binding or repost to recover a lost mapping.
        type Mapping = (
            Option<String>,
            Option<String>,
            i64,
            String,
            u64,
            String,
            String,
            String,
            String,
        );
        let row: Option<Mapping> = store
            .db
            .query_row(
                "SELECT CASE WHEN length(CAST(i.payload AS BLOB))<=?4 THEN i.payload END,
             CASE WHEN length(CAST(a.receipt AS BLOB))<=?5 THEN a.receipt END,
             a.seq,substr(a.intent_id,1,65),a.revision,substr(a.service_epoch,1,37),
             substr(a.epoch,1,37),substr(a.adapter,1,37),substr(a.lease,1,37)
             FROM desktop_attempts a JOIN desktop_intents i ON i.id=a.intent_id
             JOIN desktop_writer w ON w.id=1 WHERE a.request_id=?1 AND a.state='delivered'
             AND i.revision=a.revision AND (i.withdrawn=0 OR ?6) AND i.blocked=0
             AND (w.revoked=0 OR ?7) AND w.service_epoch=?2 AND w.epoch=?3",
                params![
                    value.request_id,
                    self.session.service_epoch,
                    self.session.writer_epoch,
                    ledger::PAYLOAD_BYTES,
                    ledger::RECEIPT_BYTES,
                    cleanup,
                    cleanup && self.retiring,
                ],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                },
            )
            .optional()?;
        let Some((payload, receipt, seq, intent, revision, service, writer, adapter, lease)) = row
        else {
            return Ok(None);
        };
        let (payload, receipt) = payload
            .zip(receipt)
            .context("notification callback data unavailable")?;
        let Action::Post { token, .. } = serde_json::from_str(&payload)? else {
            return Ok(None);
        };
        let receipt: Receipt = serde_json::from_str(&receipt)?;
        let binding = &receipt.binding;
        let valid = token == value.token
            && binding.valid()
            && binding.request_id == value.request_id
            && binding.seq == seq
            && binding.intent_id == intent
            && binding.revision == revision
            && binding.session.service_epoch == service
            && binding.session.writer_epoch == writer
            && binding.session.adapter == adapter
            && binding.session.lease == lease
            && binding.session.configuration == self.session.configuration
            && binding.session.platform == self.session.platform
            && matches!(&receipt.outcome,Outcome::Delivered {native_id} if *native_id==value.native_id);
        Ok(valid.then_some(receipt.binding))
    }

    pub fn restore_head(&self, store: &Store) -> Result<i64> {
        self.require_current_writer(store)?;
        Ok(store.db.query_row(
            "SELECT COALESCE(MAX(seq),0) FROM desktop_attempts",
            [],
            |r| r.get(0),
        )?)
    }
    /// Schema-14 live projection: cursor is an attention edge ID, not an
    /// attempt sequence. The actor uses this after discovery is available;
    /// unknown/delivered historical tombstones are never scanned or replayed.
    pub fn restore_live_head(&self, store: &Store) -> Result<i64> {
        self.require_current_writer(store)?;
        Ok(store.db.query_row(
            "SELECT COALESCE(MAX(id),0) FROM desktop_attention_edges WHERE live=1",
            [],
            |r| r.get(0),
        )?)
    }
    pub fn restore_live_page(
        &self,
        store: &Store,
        after: i64,
        through: i64,
    ) -> Result<RestorePage> {
        self.require_current_writer(store)?;
        ensure!(
            after >= 0 && through >= after,
            "invalid live notification cursor"
        );
        let mut query=store.db.prepare("SELECT e.id,
          CASE WHEN length(CAST(e.payload AS BLOB))<=?3 THEN e.payload END,
          CASE WHEN length(CAST(a.receipt AS BLOB))<=?4 THEN a.receipt END,e.blocked
          FROM desktop_attention_edges e LEFT JOIN desktop_attempts a ON a.seq=(
            SELECT seq FROM desktop_attempts WHERE intent_id=e.intent AND state='delivered' ORDER BY seq DESC LIMIT 1)
          WHERE e.live=1 AND e.id>?1 AND e.id<=?2 ORDER BY e.id LIMIT 16")?;
        let rows = query
            .query_map(
                params![after, through, ledger::PAYLOAD_BYTES, ledger::RECEIPT_BYTES],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, bool>(3)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let complete = rows.len() < 16 || rows.last().is_some_and(|r| r.0 == through);
        let next = rows.last().map_or(through, |r| r.0);
        let mut mappings = Vec::new();
        for (_, payload, receipt, blocked) in rows {
            if blocked {
                continue;
            }
            let Some(receipt) = receipt else { continue };
            let receipt: Receipt = serde_json::from_str(&receipt)?;
            ensure!(receipt.binding.valid(), "invalid live notification binding");
            let Outcome::Delivered { native_id } = receipt.outcome else {
                continue;
            };
            let Action::Post { token, .. } =
                serde_json::from_str(&payload.context("live notification payload unavailable")?)?
            else {
                continue;
            };
            let activation = Activation {
                request_id: receipt.binding.request_id,
                token,
                native_id,
                platform_context: None,
            };
            // Revocation ends click authority immediately, but restoration
            // must still load the original mappings for the cleanup phase.
            if self
                .activation_binding(store, &activation, self.retiring)?
                .is_some()
            {
                mappings.push(activation);
            }
        }
        Ok(RestorePage {
            after: next,
            through,
            complete,
            mappings,
        })
    }
    /// Retired mappings still awaiting their first native withdrawal must be
    /// restored for cleanup after a crash. They never regain click authority.
    pub fn restore_cleanup_head(&self, store: &Store) -> Result<i64> {
        self.require_current_writer(store)?;
        Ok(store.db.query_row(
            "SELECT COALESCE(MAX(id),0) FROM desktop_attention_edges WHERE dirty=1",
            [],
            |r| r.get(0),
        )?)
    }
    pub fn restore_cleanup_page(
        &self,
        store: &Store,
        after: i64,
        through: i64,
    ) -> Result<RestorePage> {
        self.require_current_writer(store)?;
        ensure!(
            after >= 0 && through >= after,
            "invalid cleanup notification cursor"
        );
        // Limit indexed candidates BEFORE filtering or looking up attempts.
        let mut query = store.db.prepare("SELECT e.id,e.live,e.blocked,
          CASE WHEN length(CAST(e.identity AS BLOB))<=4096 THEN e.identity END,
          CASE WHEN length(CAST(e.payload AS BLOB))<=?3 THEN e.payload END,
          CASE WHEN length(CAST(a.receipt AS BLOB))<=?4 THEN a.receipt END
          FROM (SELECT * FROM desktop_attention_edges WHERE dirty=1 AND id>?1 AND id<=?2 ORDER BY id LIMIT 16) e
          LEFT JOIN desktop_attempts a ON a.seq=(SELECT seq FROM desktop_attempts
            WHERE intent_id=e.intent AND state='delivered' ORDER BY seq DESC LIMIT 1)
          ORDER BY e.id")?;
        let rows = query
            .query_map(
                params![after, through, ledger::PAYLOAD_BYTES, ledger::RECEIPT_BYTES],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, bool>(1)?,
                        r.get::<_, bool>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let complete = rows.len() < 16 || rows.last().is_some_and(|r| r.0 == through);
        let next = rows.last().map_or(through, |r| r.0);
        let mut mappings = Vec::new();
        for (_, live, blocked, identity, payload, receipt) in rows {
            if live && !blocked {
                continue;
            }
            let Some(receipt) = receipt else { continue };
            let receipt: Receipt = serde_json::from_str(&receipt)?;
            ensure!(
                receipt.binding.valid(),
                "invalid cleanup notification binding"
            );
            let Outcome::Delivered { native_id } = receipt.outcome else {
                continue;
            };
            let mut identity: Identity =
                serde_json::from_str(&identity.context("cleanup identity unavailable")?)?;
            ensure!(
                identity.kind == Kind::Post
                    && ledger::intent_id(&identity)? == receipt.binding.intent_id,
                "cleanup identity mismatch"
            );
            identity.kind = Kind::Withdrawal;
            let withdrawal = ledger::intent_id(&identity)?;
            let consumed: bool = store.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM desktop_attempts WHERE intent_id=?1 LIMIT 1)",
                [withdrawal],
                |r| r.get(0),
            )?;
            if consumed {
                continue;
            } // Never replay any previous withdrawal.
            let Action::Post { token, .. } =
                serde_json::from_str(&payload.context("cleanup payload unavailable")?)?
            else {
                continue;
            };
            let activation = Activation {
                request_id: receipt.binding.request_id,
                token,
                native_id,
                platform_context: None,
            };
            if self.activation_binding(store, &activation, true)?.is_some() {
                mappings.push(activation);
            }
        }
        Ok(RestorePage {
            after: next,
            through,
            complete,
            mappings,
        })
    }
    /// Fixed highwater, sixteen indexed candidates per turn. This rebuilds
    /// callback routing after upgrade, without a whole journal read or resend.
    pub fn restore_page(&self, store: &Store, after: i64, through: i64) -> Result<RestorePage> {
        self.require_current_writer(store)?;
        ensure!(
            after >= 0 && through >= after,
            "invalid notification restoration cursor"
        );
        let mut query=store.db.prepare("SELECT seq,CASE WHEN length(CAST(receipt AS BLOB))<=?3 THEN receipt END,substr(intent_id,1,65)
            FROM desktop_attempts WHERE state='delivered' AND seq>?1 AND seq<=?2 ORDER BY seq LIMIT 16")?;
        let rows = query
            .query_map(params![after, through, ledger::RECEIPT_BYTES], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let complete = rows.len() < 16 || rows.last().is_some_and(|r| r.0 == through);
        let next = rows.last().map_or(through, |r| r.0);
        let mut mappings = Vec::new();
        for (_, raw, intent_id) in rows {
            let (identity,payload):(Option<String>,Option<String>)=store.db.query_row("SELECT
                CASE WHEN length(CAST(identity AS BLOB))<=4096 THEN identity END,
                CASE WHEN length(CAST(payload AS BLOB))<=?2 THEN payload END FROM desktop_intents WHERE id=?1",
                params![intent_id,ledger::PAYLOAD_BYTES],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let identity: Identity = serde_json::from_str(
                &identity.context("notification restoration identity unavailable")?,
            )?;
            if identity.kind != Kind::Post {
                continue;
            }
            let receipt: Receipt = serde_json::from_str(
                &raw.context("notification restoration receipt unavailable")?,
            )?;
            ensure!(
                receipt.binding.valid(),
                "invalid notification restoration binding"
            );
            let Outcome::Delivered { native_id } = receipt.outcome else {
                continue;
            };
            let action: Action = serde_json::from_str(
                &payload.context("notification restoration payload unavailable")?,
            )?;
            let Action::Post { token, .. } = action else {
                continue;
            };
            let value = Activation {
                request_id: receipt.binding.request_id,
                token,
                native_id,
                platform_context: None,
            };
            if self
                .activation_binding(store, &value, self.retiring)?
                .is_some()
            {
                mappings.push(value);
            }
        }
        Ok(RestorePage {
            after: next,
            through,
            complete,
            mappings,
        })
    }

    fn require_current_writer(&self, store: &Store) -> Result<()> {
        let current: bool = store.db.query_row(
            "SELECT (revoked=0 OR ?3) AND service_epoch=?1 AND epoch=?2 FROM desktop_writer WHERE id=1",
            params![self.session.service_epoch, self.session.writer_epoch,self.retiring],
            |r| r.get(0),
        )?;
        ensure!(
            current && !self.stopping.load(Ordering::Acquire),
            "notification writer unavailable"
        );
        Ok(())
    }

    pub fn request_stop(&self) {
        self.stopping.store(true, Ordering::Release);
        for pending in &self.pending {
            pending.shared.cancel.store(true, Ordering::Release);
        }
    }
    pub fn retire(&mut self) {
        self.retiring = true;
        self.cancel_queued_posts();
    }
    /// Operation resources only. The platform must also join its original host
    /// worker/child and callback/pipe resources before lifecycle drain succeeds.
    pub fn operation_drained(&self) -> bool {
        self.stopping.load(Ordering::Acquire) && self.operations_idle()
    }
    /// No outstanding original operations/callbacks. This does not stop, join
    /// or release the platform host, which can remain connected while idle.
    pub fn operations_idle(&self) -> bool {
        self.pending.is_empty() && self.event_count.load(Ordering::Acquire) == 0
    }
    pub fn status(&self) -> Value {
        json!({"abi":wire::ABI,"pending":self.pending.len(),"limit":wire::IN_FLIGHT,
            "callbacks":self.event_count.load(Ordering::Acquire),"callback_limit":EVENTS,
            "stopping":self.stopping.load(Ordering::Acquire),"operation_drained":self.operation_drained(),
            "error":self.error})
    }
}
impl Drop for Outbox {
    fn drop(&mut self) {
        self.request_stop();
    }
}

fn check_identity(identity: &Identity, action: &Action) -> Result<()> {
    ensure!(
        matches!(
            (&identity.kind, action),
            (Kind::Post, Action::Post { .. }) | (Kind::Withdrawal, Action::Withdraw { .. })
        ),
        "notification intent kind mismatch"
    );
    let Target::Session {
        computer,
        run,
        conversation,
    } = &identity.target
    else {
        anyhow::bail!("notification target is not a session");
    };
    let target = attention::target(action.token()).context("invalid attention token")?;
    ensure!(
        target.get("host").and_then(Value::as_str) == Some(computer.as_str())
            && target.get("run").and_then(Value::as_str) == Some(run.as_str())
            && target
                .get("conversation")
                .is_none_or(|v| v.as_str() == Some(conversation.as_str())),
        "notification token identity mismatch"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
