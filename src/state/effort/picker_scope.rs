//! The original ID input scope and its owned Effort/Settings controllers.
//! The borrowed facade retains its Cut A contract.
//!
//! The fixed driver reuses the SAME native/process/pane/panel checks and raw
//! request receipt rules. Borrowed callbacks remain trusted Rust call sites,
//! never decoded commands or permits.
//! The one logical deadline vetoes later effects; it cannot bound synchronous
//! helper drain or justify releasing its original physical resources. The only
//! post-deadline spawn is a pre-reserved Send buffer discard, within its fixed
//! two-second cleanup lease; it cannot write records or deliver input.
use super::*;
use crate::state::{
    repositories::directory::Directory,
    storage::{
        Guard, ID_RECORD_BYTES,
        native_domain::{Fresh, InputKind, KindProof},
        owner_transfer::input_domain::Input,
    },
};
use std::os::unix::fs::{FileExt, MetadataExt};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod helper;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod host;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod driver;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod caller;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn remember_caller_machine(config: &crate::config::Config, expected: &str) {
    caller::remember(config, expected);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn custodian_entry() -> Result<i32> {
    host::entry()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn front(name: &str, raw: &[u8]) -> Option<Result<i32>> {
    driver::front(name, raw)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn front_settings(name: &str, raw: &[u8]) -> Option<Result<i32>> {
    driver::front_settings(name, raw)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) fn front_send(
    _public: &input::PublicSend,
    candidate: input::native_send::Candidate,
    raw: &[u8],
) -> Option<Result<Value>> {
    // Reservation is still disposition. Failure silently preserves the old
    // blocking Root path, including the GUI auto-apply race.
    let slot = host::send_slot()?;
    Some((|| {
        let frame = host::frame::Frame::new_send(raw.to_vec(),candidate,
            host::frame::Deadline::after(Duration::from_secs(15))?)?;
        host::request_send(frame,slot)
    })())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn native_checked(name: &str, request: &Request, empty: bool) -> Result<Value> {
    driver::checked(name, request, empty)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn send_settings_ready(record: &Value) -> Option<Result<()>> {
    driver::send_settings_ready(record)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn close_claude_model(name: &str, request: &Request) -> Result<()> {
    driver::close_claude_model(name, request)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) use driver::read::Call as ReadCall;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) fn helper_read(
    call: ReadCall,
    args: &[String],
) -> Option<Result<std::process::Output>> {
    driver::helper_read(call, args)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) fn entered() -> bool {
    driver::entered()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) fn owned_send() -> bool {driver::owned_send()}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) fn refuse<T>(error: String) -> Result<T> {
    driver::refuse(error)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) fn own_marker(record: &Value) -> Result<bool> {
    driver::own_marker(record)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) fn deadline(until: Instant) -> Result<Instant> {
    driver::deadline(until)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Receipts {
    Effort,
    Settings,
    Send,
}

/// The two literal paths and the named-key path share the SAME checks.
/// This data grants no permission to choose or deliver a native key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Key<'a> {
    Literal(&'a str),
    Named(&'a str),
    Escape,
}

enum Exclusion {
    Fresh(Box<Fresh>),
    Moved(Box<Input>),
}
impl Exclusion {
    fn verify(&self) -> Result<()> {
        match self {
            Self::Fresh(held) => held.verify(),
            Self::Moved(held) => held.verify(),
        }
    }
}

fn version(file: &File) -> Result<Value> {
    let m = file.metadata().map_err(|e| e.to_string())?;
    Ok(json!([
        m.dev(),
        m.ino(),
        m.mode(),
        m.uid(),
        m.gid(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec()
    ]))
}

struct Original {
    file: File,
    version: Value,
    name: String,
}
impl Original {
    fn open(parent: &Directory, name: String) -> Result<Self> {
        parent.verify()?;
        let file = parent.read_private(&name)?;
        let original = Self {
            version: version(&file)?,
            file,
            name,
        };
        original.verify(parent)?;
        Ok(original)
    }

    fn verify(&self, parent: &Directory) -> Result<()> {
        parent.verify()?;
        if !parent.private()?
            || version(&self.file)? != self.version
            || version(&parent.read_private(&self.name)?)? != self.version
        {
            return Err("The original picker file or namespace changed".into());
        }
        Ok(())
    }

    fn record(&self, parent: &Directory) -> Result<Value> {
        self.verify(parent)?;
        let size = self.file.metadata().map_err(|e| e.to_string())?.len();
        if size > ID_RECORD_BYTES {
            return Err("The picker record exceeds its bound".into());
        }
        let mut bytes = vec![0; size as usize];
        self.file
            .read_exact_at(&mut bytes, 0)
            .map_err(|e| e.to_string())?;
        self.verify(parent)?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }
}

const BINDING_FIELDS: &[&str] = &[
    "version",
    "agent",
    "session_id",
    "layout",
    "name",
    "run_id",
    "conversation_id",
    "terminal_name",
    "pane",
    "pid",
    "process_start",
    "supervisor",
    "layout_transition",
];

pub(super) fn same_target(left: &Value, right: &Value) -> bool {
    BINDING_FIELDS
        .iter()
        .all(|key| left.get(*key) == right.get(*key))
}

/// Capture ONLY the bounded native record after borrowed caller comparison.
/// Option keeps absence distinct from explicit null. Exact empty conversation
/// is correlation, not readiness or historical delivery evidence.
struct Binding(Vec<Option<Value>>);
impl Binding {
    fn capture(native: &Value) -> Self {
        Self(
            BINDING_FIELDS
                .iter()
                .map(|key| native.get(*key).cloned())
                .collect(),
        )
    }
    fn matches(&self, native: &Value) -> bool {
        self.0
            .iter()
            .zip(BINDING_FIELDS)
            .all(|(value, key)| value.as_ref() == native.get(*key))
    }
}

/// Borrowed actual Root writer, original namespaces/proof and kernel lock.
/// A failed method never drops any of them, even after possible delivery.
/// Nested work borrows this object; it cannot reacquire it or renew its clock.
pub(super) struct Scope<'a> {
    writer: Writer<'a>,
    writer_file: Original,
    state: Directory,
    sessions: Directory,
    inputs: Directory,
    receipts: Directory,
    send_directories: Option<(Directory, Directory)>,
    record: Original,
    binding: Binding,
    proof: KindProof,
    exclusion: Exclusion,
    until: Instant,
    may_have_sent: bool,
    refused: Option<String>,
    // LAST: its completion is sent only after every original above is dropped.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    host: Option<host::Hold>,
}

enum Writer<'a> {
    Borrowed(&'a Guard),
    Owned(Guard),
}
impl Writer<'_> {
    fn get(&self) -> &Guard {
        match self {
            Self::Borrowed(writer) => writer,
            Self::Owned(writer) => writer,
        }
    }
}

impl<'a> Scope<'a> {
    /// Existing objects ONLY, with no legacy fallback or receipt reservation.
    /// Receipt parents must already have been prepared by the original writer.
    pub(super) fn open(
        writer: &'a Guard,
        expected: &Value,
        receipts: Receipts,
        until: Instant,
    ) -> Result<Self> {
        Self::acquire(Writer::Borrowed(writer), expected, receipts, until)
    }

    fn acquire(
        writer: Writer<'a>,
        expected: &Value,
        receipts: Receipts,
        until: Instant,
    ) -> Result<Self> {
        check_time(until)?;
        let state =
            Directory::open(&crate::platform::canonical(root()).map_err(|e| e.to_string())?)?;
        let writer_file = Original::open(&state, ".lock".into())?;
        if crate::platform::FileIdentity::of(&writer_file.file).map_err(|e| e.to_string())?
            != writer.get().identity().map_err(|e| e.to_string())?
        {
            return Err("The actual picker state writer changed".into());
        }
        let sid = string(expected, "session_id");
        if !is_id_layout_record(expected, sid) {
            return Err("This picker scope requires an existing ID-layout session".into());
        }
        let sessions = state.child("sessions")?;
        let inputs = state.child("native-input")?;
        let send_directories = if receipts == Receipts::Send {
            Some((state.child("settings_receipts")?,state.child("attachments")?))
        } else { None };
        let receipts = state.child(match receipts {
            Receipts::Effort => "effort_receipts",
            Receipts::Settings => "settings_receipts",
            Receipts::Send => "input_receipts",
        })?;
        let record = Original::open(&sessions, format!("{sid}.json"))?;
        let actual = record.record(&sessions)?;
        if !same_target(&actual, expected) {
            return Err("The original picker target changed".into());
        }
        let proof = KindProof::open(&state, &inputs, sid)?;
        let exclusion = match proof.kind() {
            InputKind::Fresh => Exclusion::Fresh(Box::new(Fresh::acquire(&actual, until)?)),
            InputKind::LegacyMoved => Exclusion::Moved(Box::new(Input::acquire(&actual, until)?)),
        };
        let scope = Self {
            writer,
            writer_file,
            state,
            sessions,
            inputs,
            receipts,
            send_directories,
            record,
            binding: Binding::capture(&actual),
            proof,
            exclusion,
            until,
            may_have_sent: false,
            refused: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            host: None,
        };
        scope.current()?;
        Ok(scope)
    }

    fn current(&self) -> Result<Value> {
        if let Some(error) = &self.refused {
            return Err(error.clone());
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(host) = &self.host {
            host.check()?;
        }
        check_time(self.until)?;
        for directory in [&self.state, &self.sessions, &self.inputs, &self.receipts] {
            directory.verify()?;
            if !directory.private()? {
                return Err("The original picker directory is not private".into());
            }
        }
        if let Some((settings,attachments)) = &self.send_directories {
            for directory in [settings,attachments] {
                directory.verify()?;
                if !directory.private()? { return Err("Original Send directory is not private".into()); }
            }
        }
        if crate::platform::canonical(root()).map_err(|e| e.to_string())? != self.state.path
            || self.writer.get().identity().map_err(|e| e.to_string())?
                != crate::platform::FileIdentity::of(&self.writer_file.file)
                    .map_err(|e| e.to_string())?
        {
            return Err("The original picker writer or root changed".into());
        }
        self.writer_file.verify(&self.state)?;
        self.proof.verify(&self.state, &self.inputs)?;
        self.exclusion.verify()?;
        let record = self.record.record(&self.sessions)?;
        if !self.binding.matches(&record) {
            return Err("The original picker binding changed".into());
        }
        check_time(self.until)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(host) = &self.host {
            host.check()?;
        }
        Ok(record)
    }

    fn retain<T>(&mut self, result: Result<T>) -> Result<T> {
        if let Err(error) = &result {
            // Sticky: restoring bytes after a refusal cannot revive this
            // operation. Only Drop physically releases its owned exclusion.
            self.refused.get_or_insert_with(|| error.clone());
        }
        result
    }

    fn before(&mut self, native: impl FnOnce(&Value) -> Result<()>) -> Result<Value> {
        let result = self.current().and_then(|record| {
            native(&record)?;
            // Native observation can take time or race a replacement. The
            // same originals and deadline are checked LAST before delivery.
            self.current()
        });
        self.retain(result)
    }

    pub(super) fn key(
        &mut self,
        key: Key<'_>,
        native: impl FnOnce(&Value) -> Result<()>,
        send: impl FnOnce(&Value, Key<'_>) -> Result<()>,
    ) -> Result<()> {
        let record = self.before(native)?;
        self.may_have_sent = true;
        let result = send(&record, key).and_then(|()| self.current().map(drop));
        self.retain(result)
    }

    /// Fresh original data and pinned target, never a stale admission snapshot.
    /// The callback remains responsible for its existing native write rules.
    /// The owned driver uses typed staged commits outside the Scope mutex.
    pub(super) fn record_commit(
        &mut self,
        native: impl FnOnce(&Value) -> Result<()>,
        commit: impl FnOnce(&Directory, &str, &Value) -> Result<()>,
    ) -> Result<()> {
        let record = self.before(native)?;
        let result = commit(&self.sessions, &self.record.name, &record).and_then(|()| {
            // An original commit may atomically replace its own record. Only
            // that successful callback can advance this retained descriptor.
            let next = Original::open(&self.sessions, self.record.name.clone())?;
            if !self.binding.matches(&next.record(&self.sessions)?) {
                return Err("The picker commit changed its original target".into());
            }
            self.record = next;
            self.current().map(drop)
        });
        self.retain(result)
    }

    /// No receipt creation/admission, status interpretation or replay proof.
    /// The existing receipt callback keeps its exact immutable request binding.
    pub(super) fn receipt_commit(
        &mut self,
        native: impl FnOnce(&Value) -> Result<()>,
        commit: impl FnOnce(&Directory, &Value) -> Result<()>,
    ) -> Result<()> {
        let record = self.before(native)?;
        let result = commit(&self.receipts, &record).and_then(|()| self.current().map(drop));
        self.retain(result)
    }

    /// Monotonic DATA, never a historical no-start or delivery receipt.
    pub(super) fn may_have_sent(&self) -> bool {
        self.may_have_sent
    }
}

impl Scope<'static> {
    /// Transfer the SAME actual writer, never a reopened or cloned lock guard.
    /// The caller must acquire it with its existing bounded writer method.
    /// Only this owned form may outlive a cancelled helper's original caller.
    fn owned(writer: Guard, expected: &Value, receipts: Receipts, until: Instant) -> Result<Self> {
        Self::acquire(Writer::Owned(writer), expected, receipts, until)
    }
}

fn check_time(until: Instant) -> Result<()> {
    if Instant::now() >= until {
        Err("The original picker operation budget expired".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
