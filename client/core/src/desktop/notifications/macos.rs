//! Mac host command state, with no OS calls, process selection or authority.
//! The entry must establish its original owned service channel before creating
//! this state. Neither a Session nor a decoded frame establishes that ownership.
use super::{
    Action, Activation, Command, FromHost, Guard, NativeId, Outcome, Platform, Receipt, Refusal,
    Session, ToHost, Uncertainty, ABI, APPLICATION, IN_FLIGHT, MAPPINGS,
};
use crate::macos_notify::Envelope;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, PoisonError,
    },
    time::{Duration, Instant},
};

#[cfg(unix)]
pub mod channel;

const OUTPUTS: usize = 32;
const WAIT: Duration = Duration::from_secs(10);

/// A local resource hold, never a wire permission or drain receipt.
#[derive(Clone)]
pub struct Hold(Arc<()>);

#[derive(Clone, Copy)]
struct Completed {
    at: Instant,
    accepted: bool,
}
struct Original {
    value: Arc<Mutex<Option<Completed>>>,
    fired: AtomicBool,
    _hold: Hold,
}
/// All native block copies share one original completion. The capture retains
/// its resource even after completion, until its final copy and call return.
#[derive(Clone)]
pub struct Callback(Arc<Original>);
impl Callback {
    pub fn complete(&self, accepted: bool) {
        if !self.0.fired.swap(true, Ordering::AcqRel) {
            let mut value = self.0.value.lock().unwrap_or_else(PoisonError::into_inner);
            *value = Some(Completed {
                at: Instant::now(),
                accepted,
            });
        }
    }
}

/// Implemented only by the signed native entry, after channel establishment.
/// A false post completion means unknown scheduling, never permission to retry.
pub trait Center {
    fn authorize(&self, reply: Callback);
    fn post(&self, envelope: &Envelope, title: &str, body: &str, reply: Callback);
    /// Both native removals have void asynchronous APIs, with no receipt.
    fn withdraw(&self, token: &str);
}

enum Phase {
    Authorizing,
    Posting,
}
struct Pending {
    command: Command,
    phase: Phase,
    hold: Hold,
    observed: Arc<Mutex<Option<Completed>>>,
    until: Instant,
    terminal: Option<Outcome>,
    sent: bool,
}
impl Pending {
    fn capture(&mut self) -> Callback {
        self.observed = Arc::new(Mutex::new(None));
        Callback(Arc::new(Original {
            value: self.observed.clone(),
            fired: AtomicBool::new(false),
            _hold: self.hold.clone(),
        }))
    }
    fn drained(&self) -> bool {
        Arc::strong_count(&self.hold.0) == 1
    }
}

/// Retain this with the original encoded output until every byte is written or
/// that exact transport is closed. Extracting the frame does not drain its job.
pub struct Emission {
    pub frame: FromHost,
    _hold: Option<Hold>,
}

pub struct Host {
    session: Session,
    guard: Guard,
    pending: Vec<Pending>,
    output: VecDeque<Emission>,
    // Callback comparison cache only, not an intent/delivery ledger. Entries
    // shrink ONLY by Withdraw, never TTL/age eviction. Factory/cutover must
    // preserve the 8192 cap and surface capacity, not silently drop mappings.
    mappings: BTreeMap<String, Activation>,
    native: Hold,
    highest: i64,
    stopping: bool,
    stopped_sent: bool,
}
impl Host {
    pub fn new(session: Session) -> Option<Self> {
        if session.platform != Platform::Macos {
            return None;
        }
        let guard = Guard::new(session.clone())?;
        let mut host = Self {
            session,
            guard,
            pending: Vec::new(),
            output: VecDeque::new(),
            mappings: BTreeMap::new(),
            native: Hold(Arc::new(())),
            highest: 0,
            stopping: false,
            stopped_sent: false,
        };
        host.output.push_back(Emission {
            frame: FromHost::Ready {
                abi: ABI.into(),
                application: APPLICATION.into(),
                session: host.session.clone(),
            },
            _hold: None,
        });
        Some(host)
    }

    /// Retained by actual delegate/observer captures until native teardown.
    pub fn native_resources(&self) -> Hold {
        self.native.clone()
    }

    pub fn receive(
        &mut self,
        frame: ToHost,
        center: &impl Center,
        now: Instant,
    ) -> Result<(), &'static str> {
        match frame {
            ToHost::Hello { .. } => return Err("duplicate_notification_hello"),
            ToHost::Restore { session, mapping } => {
                if self.stopping || session != self.session || !Self::valid_activation(&mapping) {
                    return Err("invalid_notification_restore");
                }
                if let Some(old) = self.mappings.get(&mapping.request_id) {
                    if old != &mapping {
                        return Err("notification_mapping_changed");
                    }
                } else if self.mapping_slots() == MAPPINGS {
                    return Err("notification_mapping_capacity");
                } else {
                    self.mappings.insert(mapping.request_id.clone(), mapping);
                }
            }
            ToHost::Stop { session } => {
                if session != self.session {
                    return Err("notification_session_changed");
                }
                self.stopping = true;
            }
            ToHost::Command(command) => {
                if !command.valid() || command.binding.session != self.session {
                    return Err("invalid_notification_command");
                }
                if command.binding.seq <= self.highest
                    || (matches!(command.action, Action::Post { .. })
                        && self.mappings.contains_key(&command.binding.request_id))
                {
                    return Err("notification_command_replayed");
                }
                self.highest = command.binding.seq;
                let admission = self.guard.admit(&command);
                // A duplicate may refer to a native call already in flight or
                // delivered. Never turn it into a false before-call refusal.
                if admission == Err(Refusal::InvalidCommand) {
                    return Err("notification_command_replayed");
                }
                let refused = if self.stopping {
                    Some(Refusal::Cancelled)
                } else if self.pending.len() == IN_FLIGHT {
                    Some(Refusal::Capacity)
                } else {
                    admission.err()
                };
                if let Some(code) = refused {
                    if self.output.len() > OUTPUTS - 2 {
                        return Err("notification_output_capacity");
                    }
                    let receipt = Receipt {
                        binding: command.binding,
                        outcome: Outcome::Refused { code },
                    };
                    self.guard.complete(&receipt);
                    self.enqueue(Emission {
                        frame: FromHost::Receipt(receipt.clone()),
                        _hold: None,
                    })?;
                    return self.enqueue(Emission {
                        frame: FromHost::Released {
                            binding: receipt.binding,
                        },
                        _hold: None,
                    });
                }
                let mut pending = Pending {
                    command,
                    phase: Phase::Authorizing,
                    hold: Hold(Arc::new(())),
                    observed: Arc::new(Mutex::new(None)),
                    until: now + WAIT,
                    terminal: None,
                    sent: false,
                };
                match &pending.command.action {
                    Action::Post { .. } if self.mapping_slots() == MAPPINGS => {
                        pending.terminal = Some(Outcome::Refused {
                            code: Refusal::Capacity,
                        });
                    }
                    Action::Post { .. } => center.authorize(pending.capture()),
                    Action::Withdraw { token, .. } => {
                        center.withdraw(token);
                        self.mappings.retain(|_, value| value.token != *token);
                        pending.terminal = Some(Outcome::Unknown {
                            code: Uncertainty::NativeError,
                        });
                    }
                }
                self.pending.push(pending);
            }
        }
        Ok(())
    }

    pub fn poll(&mut self, center: &impl Center, now: Instant) {
        for pending in &mut self.pending {
            if pending.terminal.is_none() {
                let observed = pending
                    .observed
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                let expired = now >= pending.until;
                match pending.phase {
                    Phase::Authorizing => {
                        if self.stopping || expired {
                            pending.terminal = Some(Outcome::Refused {
                                code: Refusal::Cancelled,
                            });
                        } else if observed.is_some_and(|value| !value.accepted) {
                            pending.terminal = Some(Outcome::Refused {
                                code: Refusal::PermissionDenied,
                            });
                        } else if observed.is_some_and(|value| value.accepted) {
                            pending.phase = Phase::Posting;
                            let reply = pending.capture();
                            if let Action::Post { token, title, body } = &pending.command.action {
                                // Command.valid() checked these immutable values before any SDK use.
                                let envelope =
                                    Envelope::new(token, &pending.command.binding.request_id)
                                        .expect("validated original notification command");
                                center.post(&envelope, title, body, reply);
                            }
                        } else if pending.drained() {
                            pending.terminal = Some(Outcome::Refused {
                                code: Refusal::NotReady,
                            });
                        }
                    }
                    Phase::Posting => {
                        pending.terminal =
                            if observed.is_some_and(|value| value.at >= pending.until) {
                                Some(Outcome::Unknown {
                                    code: Uncertainty::Deadline,
                                })
                            } else if observed.is_some_and(|value| value.accepted) {
                                let mapping = Activation {
                                    platform_context: None,
                                    request_id: pending.command.binding.request_id.clone(),
                                    token: pending.command.action.token().into(),
                                    native_id: NativeId::Macos {
                                        request_id: crate::attention::request_id(
                                            pending.command.action.token(),
                                        ),
                                    },
                                };
                                self.mappings
                                    .insert(mapping.request_id.clone(), mapping.clone());
                                Some(Outcome::Delivered {
                                    native_id: mapping.native_id,
                                })
                            } else if observed.is_some() {
                                Some(Outcome::Unknown {
                                    code: Uncertainty::NativeError,
                                })
                            } else if expired {
                                Some(Outcome::Unknown {
                                    code: Uncertainty::Deadline,
                                })
                            } else if pending.drained() {
                                Some(Outcome::Unknown {
                                    code: Uncertainty::NativeError,
                                })
                            } else {
                                None
                            };
                    }
                }
            }
            if !pending.sent && self.output.len() < OUTPUTS {
                if let Some(outcome) = &pending.terminal {
                    let receipt = Receipt {
                        binding: pending.command.binding.clone(),
                        outcome: outcome.clone(),
                    };
                    self.guard.complete(&receipt);
                    self.output.push_back(Emission {
                        frame: FromHost::Receipt(receipt),
                        _hold: Some(pending.hold.clone()),
                    });
                    pending.sent = true;
                }
            }
        }
        self.pending.retain(|pending| {
            if pending.sent && pending.drained() && self.output.len() < OUTPUTS {
                self.output.push_back(Emission {
                    frame: FromHost::Released {
                        binding: pending.command.binding.clone(),
                    },
                    _hold: None,
                });
                false
            } else {
                true
            }
        });
        if self.stopping
            && self.pending.is_empty()
            && self.output.is_empty()
            && Arc::strong_count(&self.native.0) == 1
            && !self.stopped_sent
        {
            self.output.push_back(Emission {
                frame: FromHost::Stopped {
                    session: self.session.clone(),
                },
                _hold: None,
            });
            self.stopped_sent = true;
        }
    }

    /// Close native observers/delegate only after command and output teardown.
    pub fn native_close_requested(&self) -> bool {
        self.stopping && self.pending.is_empty() && self.output.is_empty()
    }
    pub fn next_output(&mut self) -> Option<Emission> {
        self.output.pop_front()
    }
    pub fn clicked(&mut self, envelope: &Envelope) -> Result<(), &'static str> {
        if self.stopping {
            return Err("notification_host_stopping");
        }
        // The SDK supplies its persisted original correlation, not a rebuilt
        // Binding. Only the service actor resolves the actual delivered row.
        self.enqueue(Emission {
            frame: FromHost::Clicked(Activation {
                platform_context: None,
                request_id: envelope.correlation().into(),
                token: envelope.token().into(),
                native_id: NativeId::Macos {
                    request_id: envelope.request_id(),
                },
            }),
            _hold: None,
        })
    }
    fn enqueue(&mut self, value: Emission) -> Result<(), &'static str> {
        if self.output.len() == OUTPUTS {
            return Err("notification_output_capacity");
        }
        self.output.push_back(value);
        Ok(())
    }
    fn valid_activation(value: &Activation) -> bool {
        Envelope::new(&value.token, &value.request_id).is_ok() && value.valid_for(Platform::Macos)
    }
    fn mapping_slots(&self) -> usize {
        self.mappings.len()
            + self
                .pending
                .iter()
                .filter(|pending| {
                    matches!(pending.command.action, Action::Post { .. })
                        && pending.terminal.is_none()
                })
                .count()
    }
}

#[cfg(test)]
mod tests;
