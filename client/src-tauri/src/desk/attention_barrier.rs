//! Ordered retirement of the sole Desk Attention value, not a native drain.
//! No controller calls this barrier yet. Before the final checkpoint the host
//! must fence and actually drain agent native calls/callbacks independently.
use super::{Attention, DeskInput, DeskLoop, LoadAttention};
use std::{
    sync::{mpsc, Arc, Mutex, Weak},
    time::Instant,
};
use zerus_client_core::{
    attention_store::Checkpoint,
    desktop::drain::{Charge, Gate, Observation, Role},
};

/// The same unbounded Desk input channel as before; only this module can add
/// either of its two one-use control messages. No extra queue or input copy.
#[derive(Clone)]
pub struct InputSender(Arc<mpsc::Sender<Message>>);
impl InputSender {
    pub fn send(&self, input: DeskInput) -> Result<(), mpsc::SendError<DeskInput>> {
        self.0.send(Message::Input(input)).map_err(|error| {
            let Message::Input(input) = error.0 else {
                unreachable!("send only wraps an ordinary Desk input")
            };
            mpsc::SendError(input)
        })
    }
}
pub(super) enum Message {
    Input(DeskInput),
    Barrier(Command),
}

/// Lazy input batching stops at the first barrier. The caller must apply the
/// preceding round (including its native offers) before running that barrier.
pub(super) struct Batch<'a> {
    first: Option<Message>,
    inputs: &'a mpsc::Receiver<Message>,
    pub barrier: Option<Command>,
}
impl<'a> Batch<'a> {
    pub fn new(first: Option<Message>, inputs: &'a mpsc::Receiver<Message>) -> Self {
        Self {
            first,
            inputs,
            barrier: None,
        }
    }
}
impl Iterator for Batch<'_> {
    type Item = DeskInput;
    fn next(&mut self) -> Option<Self::Item> {
        if self.barrier.is_some() {
            return None;
        }
        match self.first.take().or_else(|| self.inputs.try_recv().ok())? {
            Message::Input(input) => Some(input),
            Message::Barrier(command) => {
                self.barrier = Some(command);
                None
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    Unsupported,
    Closed,
    Pending,
    Unavailable,
    Checkpoint,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Absent,
    Loading,
    Running,
    StopQueued,
    Stopped,
    Finalizing,
    Finished,
    Failed,
}
pub(super) struct Shared {
    sender: Weak<mpsc::Sender<Message>>,
    gate: Gate,
    phase: Mutex<Phase>,
}
impl Shared {
    fn send(&self, command: Command) -> Result<(), Refused> {
        self.sender
            .upgrade()
            .ok_or(Refused::Unavailable)?
            .send(Message::Barrier(command))
            .map_err(|_| Refused::Unavailable)
    }
}
pub(super) struct Owner(Arc<Shared>);
/// Weak control handles never keep a retired resource group charged.
#[derive(Clone)]
pub(crate) struct Control(Weak<Shared>);
/// One-use, local and non-serializing. This is not proof of native completion,
/// an authenticated producer context or a role/profile handoff permit.
pub(crate) struct Stopped(Arc<Shared>);

pub(super) fn channel(present: bool) -> (InputSender, mpsc::Receiver<Message>, Owner) {
    let (sender, inputs) = mpsc::channel();
    let sender = Arc::new(sender);
    let owner = Owner(Arc::new(Shared {
        sender: Arc::downgrade(&sender),
        gate: Gate::new(1).expect("one fixed Attention owner"),
        phase: Mutex::new(if present {
            Phase::Loading
        } else {
            Phase::Absent
        }),
    }));
    (InputSender(sender), inputs, owner)
}
impl Owner {
    #[allow(dead_code)] // No authenticated profile controller exists yet.
    pub fn control(&self) -> Control {
        Control(Arc::downgrade(&self.0))
    }
    /// Admit before spawning or reading the original source, including Qt's
    /// fallback. The guard stays in the sole original Attention value.
    pub fn track(&self, load: LoadAttention) -> LoadAttention {
        let lifetime = Lifetime {
            shared: self.0.clone(),
            charge: Some(
                self.0
                    .gate
                    .admit(Role::Attention)
                    .expect("initial owner admitted once"),
            ),
        };
        Box::new(move || {
            let mut attention = load();
            if let Ok(mut phase) = lifetime.shared.phase.lock() {
                if *phase == Phase::Loading {
                    *phase = Phase::Running;
                }
            }
            attention.lifetime = Some(lifetime);
            attention
        })
    }
}

/// Last field of Attention: source/tracker data drops before the charge. An
/// abandoned/panicking owner cannot look drained even after all handles drop.
pub(super) struct Lifetime {
    shared: Arc<Shared>,
    charge: Option<Charge>,
}
impl Lifetime {
    fn complete(&mut self) {
        drop(self.charge.take());
    }
}
impl Drop for Lifetime {
    fn drop(&mut self) {
        if let Some(charge) = self.charge.take() {
            if let Ok(mut phase) = self.shared.phase.lock() {
                *phase = Phase::Failed;
            }
            // One fixed charge only, retained until process exit on unknown
            // completion; no growing per-input or per-session collection.
            std::mem::forget(charge);
        }
    }
}

fn remaining(deadline: Instant) -> Result<std::time::Duration, Refused> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(Refused::Pending)
}
fn receive<T>(reply: mpsc::Receiver<Result<T, Refused>>, deadline: Instant) -> Result<T, Refused> {
    let result = reply
        .recv_timeout(remaining(deadline)?)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => Refused::Pending,
            mpsc::RecvTimeoutError::Disconnected => Refused::Unavailable,
        })?;
    remaining(deadline)?;
    result
}
#[allow(dead_code)] // Only internal barrier tests call stop in this slice.
impl Control {
    /// Host background wait. Unsupported platforms refuse before closing or
    /// queueing anything. A timeout never reopens or cancels the ordered stop.
    pub fn stop(&self, deadline: Instant) -> Result<Stopped, Refused> {
        if !cfg!(unix) {
            return Err(Refused::Unsupported);
        }
        remaining(deadline)?;
        let shared = self.0.upgrade().ok_or(Refused::Unavailable)?;
        let (reply, received) = mpsc::sync_channel(1);
        {
            let mut phase = shared.phase.lock().map_err(|_| Refused::Unavailable)?;
            match *phase {
                Phase::Loading | Phase::Running => (),
                Phase::Absent | Phase::Failed => return Err(Refused::Unavailable),
                _ => return Err(Refused::Closed),
            }
            *phase = Phase::StopQueued;
            shared.gate.close();
            if shared.send(Command::Stop(shared.clone(), reply)).is_err() {
                *phase = Phase::Failed;
                return Err(Refused::Unavailable);
            }
        }
        receive(received, deadline)?;
        Ok(Stopped(shared))
    }
}
#[allow(dead_code)] // No production native-drain controller calls this yet.
impl Stopped {
    /// PRECONDITION: actual agent-native work and every completion sender have
    /// drained. Enqueue after those sends return. No boolean/phase/timeout or
    /// this local token substitutes for that separate proof. Off GUI thread.
    pub fn checkpoint_after_native_drain(self, deadline: Instant) -> Result<Checkpoint, Refused> {
        remaining(deadline)?;
        let shared = self.0;
        let (reply, received) = mpsc::sync_channel(1);
        {
            let mut phase = shared.phase.lock().map_err(|_| Refused::Unavailable)?;
            if *phase != Phase::Stopped {
                return Err(Refused::Closed);
            }
            *phase = Phase::Finalizing;
            if shared
                .send(Command::Checkpoint(shared.clone(), deadline, reply))
                .is_err()
            {
                *phase = Phase::Failed;
                return Err(Refused::Unavailable);
            }
        }
        let checkpoint = receive(received, deadline)?;
        match shared.gate.close().wait_until(deadline) {
            Observation::Idle => {
                remaining(deadline)?;
                Ok(checkpoint)
            }
            Observation::Pending => Err(Refused::Pending),
            Observation::Unavailable => Err(Refused::Unavailable),
        }
    }
}

pub(super) enum Command {
    Stop(Arc<Shared>, mpsc::SyncSender<Result<(), Refused>>),
    Checkpoint(
        Arc<Shared>,
        Instant,
        mpsc::SyncSender<Result<Checkpoint, Refused>>,
    ),
}
impl Command {
    pub fn run(self, desk: &mut DeskLoop) {
        self.run_with(desk, Attention::checkpoint);
    }
    fn run_with(
        self,
        desk: &mut DeskLoop,
        sync: impl FnOnce(&mut Attention) -> std::io::Result<Checkpoint>,
    ) {
        let (shared, expected) = match &self {
            Self::Stop(shared, _) => (shared, Phase::StopQueued),
            Self::Checkpoint(shared, _, _) => (shared, Phase::Finalizing),
        };
        let correct_owner = desk
            .attention
            .as_ref()
            .and_then(|a| a.lifetime.as_ref())
            .is_some_and(|l| Arc::ptr_eq(&l.shared, shared));
        if !correct_owner || !shared.phase.lock().is_ok_and(|phase| *phase == expected) {
            // Dropping the reply refuses; never alter another Desk owner.
            return;
        }
        match self {
            Self::Stop(shared, reply) => {
                desk.observing_attention = false;
                if let Ok(mut phase) = shared.phase.lock() {
                    *phase = Phase::Stopped;
                }
                let _ = reply.send(Ok(()));
            }
            Self::Checkpoint(shared, deadline, reply) => {
                desk.attention_completions = false;
                desk.click = None;
                // Keep the same frozen value on every failure; never discard
                // private state or falsely release its lifetime charge.
                let attention = desk.attention.as_mut().expect("same owner checked above");
                let result = remaining(deadline)
                    .and_then(|_| sync(attention).map_err(|_| Refused::Checkpoint))
                    .and_then(|checkpoint| {
                        remaining(deadline)?;
                        Ok(checkpoint)
                    });
                let success = result.is_ok();
                let delivered = reply.send(result).is_ok();
                if success && delivered {
                    let mut attention = desk.attention.take().expect("sole owner");
                    let mut lifetime = attention.lifetime.take().expect("tracked owner");
                    drop(attention);
                    if let Ok(mut phase) = shared.phase.lock() {
                        *phase = Phase::Finished;
                    }
                    lifetime.complete();
                } else if let Ok(mut phase) = shared.phase.lock() {
                    *phase = Phase::Failed;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
