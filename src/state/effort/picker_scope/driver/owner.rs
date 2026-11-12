//! One original Root, then one original Scope. Helpers charge this owner;
//! neither an error nor removal of the caller's context drops their resources.
use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    mpsc,
};

struct RootPhase {
    writer: Option<Guard>,
    state: Directory,
    original: Original,
}
impl RootPhase {
    fn check(&self) -> Result<()> {
        self.state.verify()?;
        if !self.state.private()? {
            return Err("Original Effort root is not private".into());
        }
        self.original.verify(&self.state)?;
        let writer = self
            .writer
            .as_ref()
            .ok_or("Original Effort writer was transferred")?;
        if writer.identity().map_err(|e| e.to_string())?
            != crate::platform::FileIdentity::of(&self.original.file).map_err(|e| e.to_string())?
        {
            return Err("Original Effort writer changed".into());
        }
        Ok(())
    }
}

enum Phase {
    Root(RootPhase),
    Native(Box<Scope<'static>>),
}

struct Completion(mpsc::Sender<()>);
impl Drop for Completion {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

pub(super) struct Owner {
    // FIRST: every actual Guard, SID proof and retained namespace drops before
    // Completion. No Owner::Drop sends a premature lifetime notification.
    phase: Mutex<Phase>,
    pub(super) control: Arc<host::Control>,
    pub(super) thread: std::thread::ThreadId,
    pub(super) until: Instant,
    pub(super) ended: AtomicBool,
    pub(super) epoch: AtomicU64,
    helpers: AtomicUsize,
    failure: Mutex<Option<String>>,
    pub(super) reservation: Mutex<Option<transaction::Reservation>>,
    pub(super) send: Mutex<Option<send::Reservation>>,
    _complete: Completion,
}

impl Owner {
    pub(super) fn new(
        writer: Guard,
        control: Arc<host::Control>,
    ) -> Result<(Arc<Self>, mpsc::Receiver<()>)> {
        control.check()?;
        let state =
            Directory::open(&crate::platform::canonical(root()).map_err(|e| e.to_string())?)?;
        let original = Original::open(&state, ".lock".into())?;
        let phase = RootPhase {
            writer: Some(writer),
            state,
            original,
        };
        phase.check()?;
        let (send, done) = mpsc::channel();
        let until = control.frame().deadline.instant()?;
        Ok((
            Arc::new(Self {
                phase: Mutex::new(Phase::Root(phase)),
                control,
                thread: std::thread::current().id(),
                until,
                ended: AtomicBool::new(false),
                epoch: AtomicU64::new(0),
                helpers: AtomicUsize::new(0),
                failure: Mutex::new(None),
                reservation: Mutex::new(None),
                send: Mutex::new(None),
                _complete: Completion(send),
            }),
            done,
        ))
    }

    pub(super) fn actor(&self) -> Result<()> {
        if self.ended.load(Ordering::Acquire) || std::thread::current().id() != self.thread {
            return Err("Original Effort owner is ended or belongs to another thread".into());
        }
        if let Some(error) = self
            .failure
            .lock()
            .map_err(|_| "Original Effort refusal is unavailable")?
            .as_ref()
        {
            return Err(error.clone());
        }
        Ok(())
    }

    /// Source checks are pure original-object reads. The retirement poller
    /// never calls a backend or treats this check as physical settlement.
    pub(super) fn check(&self) -> Result<()> {
        if let Some(error) = self
            .failure
            .lock()
            .map_err(|_| "Original Effort refusal is unavailable")?
            .as_ref()
        {
            return Err(error.clone());
        }
        self.control.check()?;
        {
            let phase = self
                .phase
                .lock()
                .map_err(|_| "Original Effort source is unavailable")?;
            match &*phase {
                Phase::Root(root) => root.check()?,
                Phase::Native(scope) => {
                    scope.current()?;
                }
            }
        }
        self.control.check()
    }

    pub(super) fn with_scope<T>(
        &self,
        body: impl FnOnce(&mut Scope<'static>) -> Result<T>,
    ) -> Result<T> {
        self.actor()?;
        let mut phase = self
            .phase
            .lock()
            .map_err(|_| "Original Effort source is unavailable")?;
        match &mut *phase {
            Phase::Native(scope) => body(scope),
            Phase::Root(_) => Err("Original Effort SID scope is not yet admitted".into()),
        }
    }

    pub(super) fn snapshot(&self) -> Result<(Value, Value)> {
        self.with_scope(|scope| Ok((scope.current()?, scope.record.version.clone())))
    }

    pub(super) fn adopt_record(&self, committed: publication::Committed) -> Result<()> {
        self.with_scope(|scope| {
            let next = committed.adopt(self, &scope.sessions, &scope.record)?;
            if !scope.binding.matches(&next.record(&scope.sessions)?) {
                return scope.retain(Err("Own commit changed the original Effort binding".into()));
            }
            scope.record = next;
            let result = scope.current().map(drop);
            scope.retain(result)
        })
    }

    pub(super) fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) fn refuse<T>(&self, error: String) -> Result<T> {
        self.invalidate();
        self.failure
            .lock()
            .map_err(|_| "Original Effort refusal is unavailable")?
            .get_or_insert_with(|| error.clone());
        let mut phase = self
            .phase
            .lock()
            .map_err(|_| "Original Effort source is unavailable")?;
        if let Phase::Native(scope) = &mut *phase {
            return scope.retain(Err(error));
        }
        Err(error)
    }

    /// Recovery's read helpers have already returned actual physical Reports.
    /// Move the SAME actual writer; no clone, reopen or SID reacquisition.
    pub(super) fn open_scope(&self, record: &Value) -> Result<()> {
        self.actor()?;
        self.check()?;
        if self.helpers.load(Ordering::Acquire) != 0 {
            return Err("Original Effort helpers have not settled".into());
        }
        let mut phase = self
            .phase
            .lock()
            .map_err(|_| "Original Effort source is unavailable")?;
        let Phase::Root(root) = &mut *phase else {
            return Err("Original Effort scope was already acquired".into());
        };
        root.check()?;
        let writer = root
            .writer
            .take()
            .ok_or("Original Effort writer unavailable")?;
        let mut scope = Scope::owned(
            writer,
            record,
            match self.control.frame().operation {
                host::frame::Operation::Effort => Receipts::Effort,
                host::frame::Operation::Settings => Receipts::Settings,
                host::frame::Operation::Send => Receipts::Send,
            },
            self.until,
        )?;
        scope.host = Some(host::Hold::barrier(self.control.clone()));
        *phase = Phase::Native(Box::new(scope));
        Ok(())
    }

    pub(super) fn charge(self: &Arc<Self>) -> Arc<Charge> {
        self.helpers.fetch_add(1, Ordering::AcqRel);
        Arc::new(Charge {
            owner: self.clone(),
        })
    }

    pub(super) fn native(&self) -> Result<bool> {
        self.actor()?;
        Ok(matches!(
            &*self
                .phase
                .lock()
                .map_err(|_| "Original Effort source is unavailable")?,
            Phase::Native(_)
        ))
    }
}

/// Core Admission retains this exact owner until its original wait and pipes
/// settle. The counter only prevents a phase transfer while a helper exists;
/// a counter or Arc count NEVER supplies settlement or delivery authority.
pub(super) struct Charge {
    owner: Arc<Owner>,
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.owner.helpers.fetch_sub(1, Ordering::AcqRel);
    }
}
