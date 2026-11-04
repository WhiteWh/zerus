//! Windows notification state. Only an original Endpoint supplies effects;
//! native callbacks can select an existing delivered mapping, never create one.
use super::{wire, Dispatch};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use wire::{
    Action, Activation, Command, NativeId, Outcome, Platform, Refusal, Session, Uncertainty,
};

#[cfg(windows)]
mod activation;
mod callbacks;
#[cfg(windows)]
mod native;
pub(in crate::desktop) mod registration;
pub(in crate::desktop) mod runtime;

/// Fixed cold activation is deliberately passive; the root lifecycle parser
/// owns routing here and cannot promote it to a service-owner launch.
#[cfg(windows)]
pub(in crate::desktop) const ACTIVATION_ENTRY: &str = activation::ENTRY;
#[cfg(windows)]
pub(in crate::desktop) fn cold_entry() -> i32 {
    activation::cold_entry()
}

/// Tag and group limits are UTF-16 limits in the native Toast API. Our values
/// are fixed ASCII, while restored values must be accepted without truncation.
const ID_UNITS: usize = 16;

trait Native {
    /// No posting/withdrawal effect is allowed during preparation. Registration
    /// and interactive-session checks remain native reads, not JSON authority.
    fn prepare(&mut self, command: &Command, native_id: &NativeId) -> Result<(), Refusal>;
    /// Once entered, a failure has uncertain delivery. The implementation owns
    /// its delegates independently of the command's original resource charge.
    fn post(&mut self, command: &Command, native_id: &NativeId) -> Result<NativeId, ()>;
    fn withdraw(&mut self, native_id: &NativeId) -> Result<(), ()>;
    fn retire(&mut self, _request_id: &str) {}
    fn close(&mut self) -> bool;
    fn unavailable(&self) -> bool {
        false
    }
    fn callbacks_current(&mut self) -> bool {
        true
    }
}

struct Mappings {
    session: Session,
    restored: bool,
    closing: bool,
    last_seq: i64,
    delivered: BTreeMap<String, Activation>,
    native: BTreeMap<(String, String), String>,
}

fn key(native_id: &NativeId) -> Option<(String, String)> {
    let NativeId::Windows { tag, group } = native_id else {
        return None;
    };
    if [tag, group]
        .iter()
        .any(|value| value.is_empty() || value.encode_utf16().count() > ID_UNITS)
    {
        return None;
    }
    Some((tag.clone(), group.clone()))
}

fn identifier(command: &Command) -> NativeId {
    // These are correlation labels, not credentials. Check collisions against
    // both retained mappings and native history before any posting effect.
    let tag = format!(
        "{:x}",
        Sha256::digest(command.binding.request_id.as_bytes())
    );
    NativeId::Windows {
        tag: tag[..ID_UNITS].into(),
        group: command.binding.session.configuration[..ID_UNITS].into(),
    }
}

impl Mappings {
    fn new(session: Session) -> Option<Self> {
        (session.valid() && session.platform == Platform::Windows).then(|| Self {
            session,
            restored: false,
            closing: false,
            last_seq: 0,
            delivered: BTreeMap::new(),
            native: BTreeMap::new(),
        })
    }

    /// Called only with delivered mappings read from the original actor's
    /// fixed-highwater restoration pages. This path never calls a native API.
    fn restore(&mut self, activation: Activation) -> bool {
        if self.restored
            || self.closing
            || !activation.valid_for(Platform::Windows)
            || activation.platform_context.is_some()
        {
            return false;
        }
        let Some(id) = key(&activation.native_id) else {
            return false;
        };
        if let Some(old) = self.delivered.get(&activation.request_id) {
            return old == &activation;
        }
        if self.delivered.len() >= wire::MAPPINGS || self.native.contains_key(&id) {
            return false;
        }
        self.native.insert(id, activation.request_id.clone());
        self.delivered
            .insert(activation.request_id.clone(), activation);
        true
    }

    fn finish_restoration(&mut self) -> bool {
        if self.restored || self.closing {
            return false;
        }
        self.restored = true;
        true
    }

    fn request_stop(&mut self) {
        self.closing = true;
    }

    fn clicked(&self, request_id: &str) -> Option<Activation> {
        if !self.restored || self.closing {
            return None;
        }
        self.delivered.get(request_id).cloned()
    }

    fn closed(&mut self, request_id: &str) -> Option<Activation> {
        if !self.restored {
            return None;
        }
        let value = self.delivered.remove(request_id)?;
        self.native.remove(&key(&value.native_id)?);
        Some(value)
    }

    /// `Dispatch` is not deserializable and retains the original effect through
    /// the synchronous native return. Late failure never creates a new work item.
    fn dispatch(&mut self, dispatch: Dispatch, native: &mut impl Native, stop: &AtomicBool) {
        let command = dispatch.command().clone();
        let outcome = self.execute(&command, native, || {
            if Instant::now() >= dispatch.deadline() {
                Some(Uncertainty::Deadline)
            } else if stop.load(Ordering::Acquire) || dispatch.cancelled() {
                Some(Uncertainty::HostLost)
            } else {
                None
            }
        });
        dispatch.finish(outcome);
    }

    fn execute(
        &mut self,
        command: &Command,
        native: &mut impl Native,
        cancelled: impl Fn() -> Option<Uncertainty>,
    ) -> Outcome {
        let refuse = |code| Outcome::Refused { code };
        if !command.valid() || command.binding.session != self.session {
            return refuse(Refusal::InvalidCommand);
        }
        if self.closing || !self.restored {
            return refuse(Refusal::NotReady);
        }
        if command.binding.seq <= self.last_seq {
            return refuse(Refusal::InvalidCommand);
        }
        self.last_seq = command.binding.seq;
        if cancelled().is_some() {
            return refuse(Refusal::Cancelled);
        }

        let (native_id, previous) = match &command.action {
            Action::Post { .. } => {
                if self.delivered.len() >= wire::MAPPINGS {
                    return refuse(Refusal::Capacity);
                }
                let id = identifier(command);
                if self
                    .native
                    .contains_key(&key(&id).expect("fixed ASCII native id"))
                    || self.delivered.contains_key(&command.binding.request_id)
                {
                    return refuse(Refusal::InvalidCommand);
                }
                (id, None)
            }
            Action::Withdraw { token, native_id } => {
                let Some(original) = key(native_id)
                    .and_then(|id| self.native.get(&id))
                    .and_then(|request| self.delivered.get(request))
                    .filter(|value| value.token == *token && value.native_id == *native_id)
                else {
                    return refuse(Refusal::NotReady);
                };
                (native_id.clone(), Some(original.request_id.clone()))
            }
        };
        if let Err(code) = native.prepare(command, &native_id) {
            return refuse(code);
        }
        if cancelled().is_some() {
            return refuse(Refusal::Cancelled);
        }
        let result = match command.action {
            Action::Post { .. } => native.post(command, &native_id),
            Action::Withdraw { .. } => native.withdraw(&native_id).map(|()| native_id.clone()),
        };
        let Ok(actual) = result else {
            return Outcome::Unknown {
                code: Uncertainty::NativeError,
            };
        };
        if let Some(code) = cancelled() {
            return Outcome::Unknown { code };
        }
        if actual != native_id {
            return Outcome::Unknown {
                code: Uncertainty::Transport,
            };
        }
        if let Some(request_id) = previous {
            self.closed(&request_id);
            native.retire(&request_id);
        } else {
            self.native.insert(
                key(&actual).expect("checked native id"),
                command.binding.request_id.clone(),
            );
            self.delivered.insert(
                command.binding.request_id.clone(),
                Activation {
                    request_id: command.binding.request_id.clone(),
                    token: command.action.token().into(),
                    native_id: actual.clone(),
                    platform_context: None,
                },
            );
        }
        Outcome::Delivered { native_id: actual }
    }
}

#[cfg(test)]
mod tests;
