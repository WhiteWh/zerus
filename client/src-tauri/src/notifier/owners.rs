//! Optional routing/accounting partition, not a native job or ID ownership proof.
//! The current SDK adapter keeps using Notifier::new. A future caller must still
//! verify original post mappings, categories/actions and actual platform drain.
#![allow(dead_code)] // No production caller until the adapter is qualified.
use super::{AccountingRefused, Center, Control, Done, Failed, Hop, Notifier};
use std::sync::{Arc, Mutex};
use zerus_client_core::{attention::Notice, desktop::drain::Charge};

pub(crate) use zerus_client_core::native_notice::Owner;
fn index(owner: Owner) -> usize {
    match owner {
        Owner::AgentAttention => 0,
        Owner::SoftwareUpdate => 1,
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    TokenOwner,
    Accounting(AccountingRefused),
}

/// Each handle has a distinct Delivery state and failure sink. Cloning keeps
/// that same owner; it cannot turn a GUI update into an agent operation.
#[derive(Clone)]
pub(crate) struct Owned {
    owner: Owner,
    notifier: Notifier,
}
impl Owned {
    /// Validate the whole batch before any withdrawal, post or authorization.
    /// Tokens remain exact: no decode/re-encode and no arbitrary native ID input.
    /// This is routing only; the caller must supply separately admitted work.
    pub fn deliver(&self, cleared: &[String], raised: &[Notice]) -> Result<(), Refused> {
        if cleared
            .iter()
            .map(String::as_str)
            .chain(raised.iter().map(|n| n.token.as_str()))
            .any(|token| !self.owner.accepts_token(token))
        {
            return Err(Refused::TokenOwner);
        }
        self.notifier
            .try_deliver(cleared, raised)
            .map_err(Refused::Accounting)
    }
}

pub(crate) struct Partitioned {
    pub agent: Owned,
    pub updates: Owned,
}
impl Partitioned {
    /// Separate staged path: admitted agent work can settle after input close,
    /// while GUI updates keep their own unchanged delivery state and sink.
    pub fn new_retiring(
        center: Box<dyn Center>,
        hop: Hop,
        agent_failed: Failed,
        update_failed: Failed,
        charge: Charge,
    ) -> Result<
        (
            super::retiring::Retiring,
            Owned,
            super::retiring::InputControl,
        ),
        AccountingRefused,
    > {
        let backend = Arc::new(Backend {
            center,
            authorization: Mutex::new(Authorization::default()),
        });
        let (agent, control) = super::retiring::Retiring::new_accounted(
            Box::new(Proxy {
                owner: Owner::AgentAttention,
                backend: backend.clone(),
            }),
            hop.clone(),
            agent_failed,
            charge,
        )?;
        let updates = Owned {
            owner: Owner::SoftwareUpdate,
            notifier: Notifier::new(
                Box::new(Proxy {
                    owner: Owner::SoftwareUpdate,
                    backend,
                }),
                hop,
                update_failed,
            ),
        };
        Ok((agent, updates, control))
    }

    /// Only the agent side has a NativePosting scope. Update work stays GUI
    /// owned, sharing the backend and its current authorization query only.
    pub fn new_accounted(
        center: Box<dyn Center>,
        hop: Hop,
        agent_failed: Failed,
        update_failed: Failed,
        charge: Charge,
    ) -> Result<(Self, Control), AccountingRefused> {
        let backend = Arc::new(Backend {
            center,
            authorization: Mutex::new(Authorization::default()),
        });
        let (agent, control) = Notifier::new_accounted(
            Box::new(Proxy {
                owner: Owner::AgentAttention,
                backend: backend.clone(),
            }),
            hop.clone(),
            agent_failed,
            charge,
        )?;
        let updates = Notifier::new(
            Box::new(Proxy {
                owner: Owner::SoftwareUpdate,
                backend,
            }),
            hop,
            update_failed,
        );
        Ok((
            Self {
                agent: Owned {
                    owner: Owner::AgentAttention,
                    notifier: agent,
                },
                updates: Owned {
                    owner: Owner::SoftwareUpdate,
                    notifier: updates,
                },
            },
            control,
        ))
    }
}

type AuthorizationDone = Done<(bool, Option<String>)>;
#[derive(Default)]
struct Authorization {
    active: bool,
    waiters: [Option<AuthorizationDone>; 2],
}
struct Backend {
    center: Box<dyn Center>,
    authorization: Mutex<Authorization>,
}
struct Proxy {
    owner: Owner,
    backend: Arc<Backend>,
}
const UNAVAILABLE: &str = "Notification authorization is unavailable.";
impl Backend {
    /// Fixed two-owner fanout, never a per-token/per-session wait collection.
    /// Delivery permits at most one outstanding check from each owner. A native
    /// result is global permission data, never a receipt for either owner's post.
    fn authorize(self: &Arc<Self>, owner: Owner, done: AuthorizationDone) {
        let start = {
            let state = self.authorization.lock().ok();
            let Some(mut state) = state else {
                done((false, Some(UNAVAILABLE.into())));
                return;
            };
            let slot = &mut state.waiters[index(owner)];
            if slot.is_some() {
                // Refuse an impossible duplicate without replacing its original
                // completion or growing the queue. Call no sink under the lock.
                drop(state);
                done((false, Some(UNAVAILABLE.into())));
                return;
            }
            *slot = Some(done);
            let start = !state.active;
            state.active = true;
            start
        };
        if start {
            let backend = self.clone();
            self.center.authorize(Box::new(move |result| {
                let waiters = {
                    // Poisoned/abandoned native work keeps the old callbacks
                    // (and their charges) retained, never a false empty drain.
                    let Ok(mut state) = backend.authorization.lock() else {
                        return;
                    };
                    state.active = false;
                    std::mem::take(&mut state.waiters)
                };
                for done in waiters.into_iter().flatten() {
                    done(result.clone());
                }
            }));
        }
    }
}
impl Center for Proxy {
    fn authorize(&self, done: AuthorizationDone) {
        self.backend.authorize(self.owner, done);
    }
    fn add(&self, token: &str, title: &str, body: &str, done: Done<Option<String>>) {
        if !self.owner.accepts_token(token) {
            done(Some(UNAVAILABLE.into()));
            return;
        }
        self.backend.center.add(token, title, body, done);
    }
    fn remove(&self, token: &str) {
        if self.owner.accepts_token(token) {
            self.backend.center.remove(token);
        }
    }
}
