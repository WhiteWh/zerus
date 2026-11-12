//! Private actor-thread binding. An ended/lost/foreign context never changes
//! this process back into a legacy command lane.
use super::*;
use std::{
    cell::RefCell,
    sync::atomic::{AtomicBool, Ordering},
};

static ENTERED: AtomicBool = AtomicBool::new(false);
thread_local! {static CURRENT:RefCell<Option<State>>=const{RefCell::new(None)};}

struct State {
    owner: Arc<owner::Owner>,
    pending: bool,
    turn: Option<Turn>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind { Picker, PendingSettings(uuid::Uuid), SendReady, SendAfterPaste }

#[derive(Clone)]
pub(super) struct Turn {
    pub(super) kind: Kind,
    pub(super) record: Value,
    pub(super) version: Value,
    pub(super) epoch: u64,
    pub(super) started: Instant,
}

pub(super) fn entered() -> bool {
    ENTERED.load(Ordering::Acquire)
}

pub(super) fn owner() -> Result<Option<Arc<owner::Owner>>> {
    if !entered() {
        return Ok(None);
    }
    let held = CURRENT
        .with(|slot| slot.borrow().as_ref().map(|s| s.owner.clone()))
        .ok_or("Original Effort keeper context is missing or belongs to another thread")?;
    held.actor()?;
    Ok(Some(held))
}

pub(super) struct Entered {
    owner: Arc<owner::Owner>,
}
impl Entered {
    /// Only the fixed Effort/Settings keeper calls this constructor; no environment,
    /// serialized callback, endpoint prefix or renderer value creates it.
    pub(super) fn install(owner: Arc<owner::Owner>) -> Result<Self> {
        owner.actor()?;
        if ENTERED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            owner.invalidate();
            return Err("Nested or repeated Effort keeper refused".into());
        }
        CURRENT.with(|slot| {
            *slot.borrow_mut() = Some(State {
                owner: owner.clone(),
                pending: false,
                turn: None,
            })
        });
        Ok(Self { owner })
    }
}
impl Drop for Entered {
    fn drop(&mut self) {
        self.owner.invalidate();
        self.owner.ended.store(true, Ordering::Release);
        CURRENT.with(|slot| {
            slot.borrow_mut().take();
        });
        // ENTERED intentionally remains true. Loss/end is never legacy mode,
        // and dropping this TLS entry is not a helper or scope drain proof.
    }
}

pub(super) struct Observation {
    owner: Arc<owner::Owner>,
    turn: Turn,
    complete: bool,
}
impl Observation {
    pub(super) fn begin() -> Result<Option<Self>> {
        let kind = match owner()? {
            Some(owner) if owner.control.frame().operation == host::frame::Operation::Send =>
                Kind::PendingSettings(send::settings_nonce(&owner)?),
            _ => Kind::Picker,
        };
        Self::begin_kind(kind)
    }
    pub(super) fn begin_kind(kind: Kind) -> Result<Option<Self>> {
        let Some(owner) = owner()? else {
            return Ok(None);
        };
        let started = Instant::now();
        owner.invalidate();
        let reentrant = CURRENT.with(|slot| {
            let mut state = slot.borrow_mut();
            let state = state.as_mut().expect("same checked actor context");
            let prior = state.pending;
            state.pending = true;
            state.turn = None;
            prior
        });
        if reentrant {
            return owner.refuse("Reentrant original Effort observation refused".into());
        }
        let (record, version) = match owner.snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                CURRENT.with(|slot| {
                    if let Some(state) = slot.borrow_mut().as_mut() {
                        state.pending = false;
                        state.turn = None;
                    }
                });
                return owner.refuse(error);
            }
        };
        let epoch = owner.epoch.load(Ordering::Acquire);
        Ok(Some(Self {
            owner,
            turn: Turn {
                kind,
                record,
                version,
                epoch,
                started,
            },
            complete: false,
        }))
    }

    /// Caller invokes this ONLY after both existing checked_settings and
    /// checked_terminal succeed. Capture bytes alone cannot complete a turn.
    pub(super) fn complete(mut self, record: &Value) -> Result<()> {
        self.owner.actor()?;
        let current = self.owner.snapshot()?;
        if current != (self.turn.record.clone(), self.turn.version.clone())
            || !same_target(record, &current.0)
            || self.owner.epoch.load(Ordering::Acquire) != self.turn.epoch
        {
            return self
                .owner
                .refuse("Original Effort observation changed before completion".into());
        }
        CURRENT.with(|slot| {
            let mut state = slot.borrow_mut();
            let state = state.as_mut().expect("same checked actor context");
            state.pending = false;
            state.turn = Some(self.turn.clone());
        });
        self.complete = true;
        Ok(())
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        if self.complete {
            return;
        }
        self.owner.invalidate();
        let _ = self
            .owner
            .refuse::<()>("Original Effort observation did not complete".into());
        CURRENT.with(|slot| {
            if let Some(state) = slot.borrow_mut().as_mut() {
                state.pending = false;
                state.turn = None;
            }
        });
    }
}

pub(super) fn take_turn(owner: &Arc<owner::Owner>) -> Result<Turn> {
    let kind = if owner.control.frame().operation == host::frame::Operation::Send {
        Kind::PendingSettings(send::settings_nonce(owner)?)
    } else { Kind::Picker };
    take_kind(owner,kind)
}

pub(super) fn take_kind(owner: &Arc<owner::Owner>, kind: Kind) -> Result<Turn> {
    owner.actor()?;
    let turn = CURRENT
        .with(|slot| {
            let mut state = slot.borrow_mut();
            let state = state.as_mut().ok_or("Original Effort context is missing")?;
            if !Arc::ptr_eq(&state.owner, owner) || state.pending {
                return Err("Original Effort observation belongs to another operation");
            }
            state
                .turn
                .take()
                .ok_or("A fresh original Effort observation is required")
        })
        .map_err(str::to_owned)?;
    if turn.kind != kind || turn.epoch != owner.epoch.load(Ordering::Acquire)
        || owner.snapshot()? != (turn.record.clone(), turn.version.clone())
    {
        return owner.refuse("Original Effort observation expired before use".into());
    }
    owner.invalidate();
    Ok(turn)
}
