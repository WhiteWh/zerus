//! Stage under the original directory descriptor. Data writes/fsync may block;
//! the caller's fresh original/native/lifeline fence runs AFTER them and LAST
//! before the one namespace commit. Ordinary path-based atomic is no fallback.
use super::*;
use std::{
    ffi::CString,
    io::Write,
    os::fd::{AsRawFd, FromRawFd},
};

/// Same-operation data fence, not a callback after name verification. Native
/// observation/source checks finish before Staged::commit checks rename names.
pub(super) struct CommitFence {
    owner: Arc<owner::Owner>,
    epoch: u64,
    turn: Option<context::Turn>,
    temporary: CString,
    sources: Vec<crate::state::guarded_recovery::Original>,
    schedule: Option<(Value, Value, settings::Liveness, Value)>,
    send: Option<send::Publication>,
}
impl CommitFence {
    pub(super) fn native(owner: &Arc<owner::Owner>, staged: &Staged) -> Result<Self> {
        let turn = context::take_turn(owner)?;
        if turn.started < staged.prepared_at {
            return owner.refuse("Effort commit requires observation after staging".into());
        }
        owner.check()?;
        Ok(Self {
            owner: owner.clone(),
            epoch: owner.epoch.load(std::sync::atomic::Ordering::Acquire),
            turn: Some(turn),
            temporary: staged.temporary.clone(),
            sources: Vec::new(),
            schedule: None,
            send: None,
        })
    }
    pub(super) fn recovery(
        owner: &Arc<owner::Owner>,
        staged: &Staged,
        sources: &[&crate::state::guarded_recovery::Original],
    ) -> Result<Self> {
        owner.actor()?;
        if owner.native()? {
            return owner
                .refuse("Recovery fence cannot bypass original Effort native checks".into());
        }
        owner.check()?;
        owner.invalidate();
        Ok(Self {
            owner: owner.clone(),
            epoch: owner.epoch.load(std::sync::atomic::Ordering::Acquire),
            turn: None,
            temporary: staged.temporary.clone(),
            sources: sources.iter().map(|source| (*source).clone()).collect(),
            schedule: None,
            send: None,
        })
    }
    pub(super) fn schedule(
        owner: &Arc<owner::Owner>,
        staged: &Staged,
        step: &settings::Schedule,
    ) -> Result<Self> {
        owner.actor()?;
        owner.invalidate();
        let (record, version) = step.check(owner, staged)?;
        Ok(Self {
            owner: owner.clone(),
            epoch: owner.epoch.load(std::sync::atomic::Ordering::Acquire),
            turn: None,
            temporary: staged.temporary.clone(),
            sources: Vec::new(),
            schedule: Some((record, version, step.class, staged.json()?)),
            send: None,
        })
    }
    pub(super) fn send(owner: &Arc<owner::Owner>, staged: &Staged, step: send::Publication) -> Result<Self> {
        owner.actor()?;
        step.check(owner,staged)?;
        owner.invalidate();
        Ok(Self { owner:owner.clone(),epoch:owner.epoch.load(std::sync::atomic::Ordering::Acquire),
            turn:None,temporary:staged.temporary.clone(),sources:Vec::new(),schedule:None,send:Some(step) })
    }
    pub(super) fn record(&self, record: &Value) -> Result<()> {
        if self.turn.as_ref().is_none_or(|turn| turn.record != *record) {
            return Err("Original Effort commit observation changed".into());
        }
        Ok(())
    }
    fn sources(&self, staged: &Staged) -> Result<()> {
        self.owner.actor()?;
        if self.temporary != staged.temporary {
            return Err("Original Effort fence belongs to another staged write".into());
        }
        // This runs at commit, outside the Scope mutex, after staging and any
        // native observation. An earlier minted fence is not a current source.
        self.owner.check()?;
        for source in &self.sources {
            source.verify()?;
        }
        if let Some(step) = &self.send { step.check(&self.owner,staged)?; }
        if let Some(turn) = &self.turn {
            if self.owner.snapshot()? != (turn.record.clone(), turn.version.clone()) {
                return Err("Original Effort commit source changed after observation".into());
            }
        }
        if let Some((record, version, class, content)) = &self.schedule {
            if self.owner.control.frame().operation != host::frame::Operation::Settings
                || !self.owner.native()?
                || self
                    .owner
                    .reservation
                    .lock()
                    .map_err(|_| "Settings reservation unavailable")?
                    .is_some()
                || self.owner.snapshot()? != (record.clone(), version.clone())
                || settings::liveness(record)? != *class
            {
                return Err("Original schedule source or liveness changed before commit".into());
            }
            staged.schedule_content(&self.owner, record, version, *class)?;
            if staged.json()? != *content {
                return Err("Original schedule content changed".into());
            }
            // Native read/recipe validation above can block. Recheck the SAME
            // scope and record afterwards, before names and the pure EOF fence.
            self.owner.check()?;
            if self.owner.snapshot()? != (record.clone(), version.clone()) {
                return Err("Original schedule source changed during commit observation".into());
            }
        }
        Ok(())
    }
    fn finish(&self) -> Result<()> {
        self.owner.actor()?;
        let held = context::owner()?.ok_or("Original Effort commit context is missing")?;
        if !Arc::ptr_eq(&held, &self.owner)
            || self.owner.epoch.load(std::sync::atomic::Ordering::Acquire) != self.epoch
        {
            return Err("Original Effort commit fence belongs to an ended observation".into());
        }
        self.owner.control.commit_boundary()
    }
}

struct Parent {
    file: File,
    path: PathBuf,
    object: (u64, u64),
}
impl Parent {
    fn borrow(original: &Directory) -> Result<Self> {
        original.verify()?;
        if !original.private()? {
            return Err("Original publication namespace is not private".into());
        }
        let raw = unsafe { libc::fcntl(original.descriptor(), libc::F_DUPFD_CLOEXEC, 3) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // This is only a namespace descriptor. The original owner retains the
        // SAME actual Root/SID locks; neither lock is cloned or transferred.
        let file = unsafe { File::from_raw_fd(raw) };
        let id = crate::platform::FileIdentity::of(&file).map_err(|e| e.to_string())?;
        Ok(Self {
            file,
            path: original.path.clone(),
            object: (id.device, id.inode),
        })
    }
    fn verify(&self, original: &Directory) -> Result<()> {
        original.verify()?;
        let held = crate::platform::FileIdentity::of(&self.file).map_err(|e| e.to_string())?;
        let mut native: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(original.descriptor(), &mut native) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if original.path != self.path
            || !original.private()?
            || self.object != (held.device, held.inode)
            || self.object != (native.st_dev as u64, native.st_ino as u64)
        {
            return Err("Original publication namespace changed".into());
        }
        Ok(())
    }
    fn current(&self) -> Result<()> {
        self.verify(&Directory::open(&self.path)?)
    }
    fn read(&self, name: &CString) -> Result<Option<File>> {
        let raw = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error.to_string())
            };
        }
        let file = unsafe { File::from_raw_fd(raw) };
        if !crate::platform::verify_private_file(&file).map_err(|e| e.to_string())? {
            return Err("Original publication file is not private and regular".into());
        }
        Ok(Some(file))
    }
}

struct Expected {
    file: File,
    version: Value,
}

pub(super) struct Staged {
    parent: Parent,
    file: File,
    temporary: CString,
    destination: CString,
    expected: Option<Expected>,
    prepared: Value,
    prepared_at: Instant,
    published: bool,
}

fn name(value: &str) -> Result<CString> {
    if value.is_empty() || value.contains(['/', '\\', ':']) || matches!(value, "." | "..") {
        return Err("Invalid original publication filename".into());
    }
    CString::new(value).map_err(|e| e.to_string())
}

impl Staged {
    pub(super) fn destination_is(&self, parent: &Directory, destination: &str) -> Result<bool> {
        self.parent.verify(parent)?;
        Ok(self.parent.path == parent.path && self.destination == name(destination)?)
    }
    pub(super) fn content_sha(&self) -> Result<String> {
        let size = self.file.metadata().map_err(|e| e.to_string())?.len();
        if size > ID_RECORD_BYTES { return Err("Original Send stage exceeds its bound".into()); }
        let mut bytes = vec![0;size as usize];
        self.file.read_exact_at(&mut bytes,0).map_err(|e| e.to_string())?;
        Ok(format!("{:x}",Sha256::digest(&bytes)))
    }
    pub(super) fn prepare(original: &Directory, destination: &str, bytes: &[u8]) -> Result<Self> {
        if bytes.len() > ID_RECORD_BYTES as usize {
            return Err("Original publication exceeds its bound".into());
        }
        let parent = Parent::borrow(original)?;
        let destination = name(destination)?;
        let expected = parent
            .read(&destination)?
            .map(|file| version(&file).map(|version| Expected { file, version }))
            .transpose()?;
        let temporary = name(&format!(".hgs-native-stage-{}", uuid::Uuid::new_v4()))?;
        crate::state::storage::cut_write()?;
        let raw = unsafe {
            libc::openat(
                parent.file.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if raw < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let file = unsafe { File::from_raw_fd(raw) };
        // Install owned cleanup before any possibly blocking write or fsync.
        let mut staged = Self {
            parent,
            file,
            temporary,
            destination,
            expected,
            prepared: Value::Null,
            prepared_at: Instant::now(),
            published: false,
        };
        staged
            .file
            .write_all(bytes)
            .and_then(|()| staged.file.sync_all())
            .map_err(|e| e.to_string())?;
        staged.prepared = version(&staged.file)?;
        staged.prepared_at = Instant::now();
        staged.parent.verify(original)?;
        Ok(staged)
    }

    pub(super) fn expected_version(&self) -> Option<&Value> {
        self.expected.as_ref().map(|e| &e.version)
    }

    pub(super) fn json(&self) -> Result<Value> {
        use std::os::unix::fs::FileExt as _;
        let size = self.file.metadata().map_err(|e| e.to_string())?.len();
        if size > ID_RECORD_BYTES {
            return Err("Settings stage exceeds its bound".into());
        }
        let mut bytes = vec![0; size as usize];
        self.file
            .read_exact_at(&mut bytes, 0)
            .map_err(|e| e.to_string())?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }

    pub(super) fn schedule_content(
        &self,
        owner: &Arc<owner::Owner>,
        record: &Value,
        record_version: &Value,
        class: settings::Liveness,
    ) -> Result<()> {
        let frame = owner.control.frame();
        let value = self.json()?;
        owner.with_scope(|scope| {
            if self.parent.path==scope.sessions.path && self.destination==name(&scope.record.name)? {
                if self.expected_version()!=Some(record_version) {return Err("Schedule record preimage changed".into());}
                let mut original=record.clone(); let mut next=value.clone();
                for key in ["pending_settings","resume_settings","updated"] {
                    original.as_object_mut().ok_or("Invalid schedule record")?.remove(key);
                    next.as_object_mut().ok_or("Invalid schedule stage")?.remove(key);
                }
                if original!=next || !same_target(record,&value) {return Err("Schedule fence cannot change native settings or marker".into());}
                let request:SettingsRequest=serde_json::from_slice(&frame.raw).map_err(|e|e.to_string())?;
                if request.expected_pending_id.is_some() {return Err("Manual pending scheduling cannot replace its original record".into());}
                if string(&value["pending_settings"],"request_id")!=frame.request_id
                    || value["pending_settings"]["run_id"]!=record["run_id"]
                    || value["pending_settings"]["conversation_id"]!=record["conversation_id"]
                    || value["pending_settings"]["receipt_name"]!=record["name"]
                    || value["pending_settings"]["receipt_run_id"]!=record["run_id"]
                    || value["resume_settings"]!=json!({"model":value["pending_settings"]["model"],"effort":value["pending_settings"]["effort"]}) {
                    return Err("Schedule stage does not name its original request".into());
                }
            } else if self.parent.path==scope.receipts.path
                && self.destination==name(&format!("{}.json",frame.request_id))? {
                if value["request_id"]!=frame.request_id || value["digest"]!=format!("{:x}",Sha256::digest(&frame.raw))
                    || value["name"]!=record["name"] || value["run_id"]!=record["run_id"]
                    || value["conversation_id"]!=record["conversation_id"] || value["scope"]!="session"
                    || !matches!(value["status"].as_str(),Some("in_progress"|"scheduled"))
                    || value["apply_when"]!=if class==settings::Liveness::Running {"ready"} else {"resume"} {
                    return Err("Schedule fence cannot publish this receipt".into());
                }
            } else { return Err("Schedule fence cannot write another destination".into()); }
            Ok(())
        })
    }

    /// The final scheduled bytes were fsynced before W in_progress. Advance
    /// only their receipt preimage from the SAME known successful own commit.
    pub(super) fn follow_own_settings_receipt(
        &mut self,
        owner: &Arc<owner::Owner>,
        original: &Original,
    ) -> Result<()> {
        let frame = owner.control.frame();
        if frame.operation != host::frame::Operation::Settings
            || self.expected.is_some()
            || self.destination != name(&format!("{}.json", frame.request_id))?
            || self.json()?["status"] != "scheduled"
        {
            return Err("Invalid scheduled receipt transition".into());
        }
        owner.with_scope(|scope| {
            original.verify(&scope.receipts)?;
            let value = original.record(&scope.receipts)?;
            if self.parent.path != scope.receipts.path
                || original.name != format!("{}.json", frame.request_id)
                || value["status"] != "in_progress"
                || value["request_id"] != frame.request_id
                || value["digest"] != format!("{:x}", Sha256::digest(&frame.raw))
            {
                return Err("Original schedule reservation changed".into());
            }
            self.expected = Some(Expected {
                file: original.file.try_clone().map_err(|e| e.to_string())?,
                version: original.version.clone(),
            });
            Ok(())
        })
    }

    /// The fence contains the completed source/native observation. Its last
    /// check does only actor/epoch, original clock and direct EOF, AFTER the
    /// prepared names below. No arbitrary later callback or IO is accepted.
    /// A syscall already admitted by that last fence is never called again.
    pub(super) fn commit(self, fence: &CommitFence) -> Result<Committed> {
        self.commit_once(fence)
            .or_else(|error| fence.owner.refuse(error))
    }

    fn commit_once(mut self, fence: &CommitFence) -> Result<Committed> {
        fence.sources(&self)?;
        self.parent.current()?;
        if version(&self.file)? != self.prepared
            || self
                .parent
                .read(&self.temporary)?
                .as_ref()
                .map(version)
                .transpose()?
                != Some(self.prepared.clone())
        {
            return Err("Original staged publication changed".into());
        }
        let current = self.parent.read(&self.destination)?;
        match (&self.expected, current.as_ref()) {
            (None, None) => {}
            (Some(expected), Some(current))
                if version(&expected.file)? == expected.version
                    && version(current)? == expected.version => {}
            _ => return Err("Original publication destination changed".into()),
        }
        fence.finish()?;
        let directory = self.parent.file.as_raw_fd();
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                directory,
                self.temporary.as_ptr(),
                directory,
                self.destination.as_ptr(),
                if self.expected.is_some() {
                    0
                } else {
                    libc::RENAME_NOREPLACE
                },
            )
        };
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                directory,
                self.temporary.as_ptr(),
                directory,
                self.destination.as_ptr(),
                if self.expected.is_some() {
                    0
                } else {
                    libc::RENAME_EXCL
                },
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        self.published = true;
        // Sync failure is not permission to roll back/retry an uncertain
        // commit. The caller preserves its original in_progress/Unknown rule.
        self.parent.file.sync_all().map_err(|e| e.to_string())?;
        self.parent.current()?;
        let after = version(&self.file)?;
        if self
            .parent
            .read(&self.destination)?
            .as_ref()
            .map(version)
            .transpose()?
            != Some(after.clone())
        {
            return Err("Own committed publication was replaced".into());
        }
        Ok(Committed {
            parent: Parent {
                file: self.parent.file.try_clone().map_err(|e| e.to_string())?,
                path: self.parent.path.clone(),
                object: self.parent.object,
            },
            file: self.file.try_clone().map_err(|e| e.to_string())?,
            destination: self.destination.clone(),
            before: self
                .expected
                .as_ref()
                .map(|expected| expected.version.clone()),
            after,
            owner: fence.owner.clone(),
            epoch: fence.epoch,
        })
    }
}

/// Constructible only after the one actual rename and directory sync. This is
/// evidence of our own replacement, not a fresh permission or replay token.
pub(super) struct Committed {
    parent: Parent,
    file: File,
    destination: CString,
    before: Option<Value>,
    after: Value,
    owner: Arc<owner::Owner>,
    epoch: u64,
}
impl Committed {
    pub(super) fn receipt(
        self,
        owner: &owner::Owner,
        parent: &Directory,
        name: &str,
    ) -> Result<Original> {
        self.parent.verify(parent)?;
        owner.actor()?;
        if !std::ptr::eq(Arc::as_ptr(&self.owner), owner)
            || owner.epoch.load(std::sync::atomic::Ordering::Acquire) != self.epoch
            || self.destination.to_str().map_err(|e| e.to_string())? != name
            || version(&self.file)? != self.after
            || self
                .parent
                .read(&self.destination)?
                .as_ref()
                .map(version)
                .transpose()?
                != Some(self.after.clone())
        {
            return Err("Own committed receipt proof changed".into());
        }
        Ok(Original {
            file: self.file,
            version: self.after,
            name: name.into(),
        })
    }
    pub(super) fn adopt(
        self,
        owner: &owner::Owner,
        parent: &Directory,
        original: &Original,
    ) -> Result<Original> {
        self.parent.verify(parent)?;
        owner.actor()?;
        let before =
            crate::platform::FileIdentity::of(&original.file).map_err(|e| e.to_string())?;
        if self.destination.to_str().map_err(|e| e.to_string())? != original.name
            || self.before.as_ref() != Some(&original.version)
            || !std::ptr::eq(Arc::as_ptr(&self.owner), owner)
            || owner.epoch.load(std::sync::atomic::Ordering::Acquire) != self.epoch
            || original.version[0].as_u64() != Some(before.device)
            || original.version[1].as_u64() != Some(before.inode)
            || version(&self.file)? != self.after
            || self
                .parent
                .read(&self.destination)?
                .as_ref()
                .map(version)
                .transpose()?
                != Some(self.after.clone())
        {
            return Err("Own committed record proof does not match the original".into());
        }
        Ok(Original {
            file: self.file,
            version: self.after,
            name: original.name.clone(),
        })
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        // An incomplete write has no stable prepared before-image. Preserve
        // it for review rather than deleting unknown changed data.
        if self.prepared.is_null() {
            return;
        }
        // Only this run's exact UUID staging inode can be discarded. A missing
        // or replaced name is never followed or treated as equivalent custody.
        let same = self
            .parent
            .read(&self.temporary)
            .ok()
            .flatten()
            .is_some_and(|named| {
                match (
                    crate::platform::FileIdentity::of(&self.file),
                    crate::platform::FileIdentity::of(&named),
                ) {
                    (Ok(a), Ok(b)) => {
                        a.device == b.device
                            && a.inode == b.inode
                            && version(&self.file).ok().as_ref() == Some(&self.prepared)
                            && version(&named).ok().as_ref() == Some(&self.prepared)
                    }
                    _ => false,
                }
            });
        if same {
            unsafe { libc::unlinkat(self.parent.file.as_raw_fd(), self.temporary.as_ptr(), 0) };
        }
    }
}
