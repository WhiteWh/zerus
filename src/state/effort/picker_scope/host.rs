//! Fixed-entry operation owner, entered before Root acquisition.
//!
//! Existing ID Effort and Settings lanes use this keeper and original raw stdin.
//! Host lifetime controls substitute only their final operation; actual request
//! controls use the production CLI.
//! Source/image/frame/deadline/lifeline/acquisition/lifetime methods are shared.
use super::*;
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    process::{Child, Command, Stdio},
    sync::{Arc, mpsc},
};
use zerus_client_core::run::custody::InheritedLock;
pub(super) mod frame;
use frame::{Deadline, Frame, Image, Operation};

pub(super) const ENTRY: &str = "native-picker-custodian-v1";
const CONTROL: &str = "HGS_PICKER_HOST_FD";
const SLOT: &str = "HGS_PICKER_HOST_SLOT";
const CONTEXT: &str = "HGS_PICKER_HOST_CONTEXT";
const SLOT_FILE: &str = ".native-picker-host.lock";
const RESPONSE_BYTES: usize = 96 << 10;

fn fd(text: Option<std::ffi::OsString>) -> Result<i32> {
    text.and_then(|v| v.to_str().map(str::to_owned))
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|v| v.parse().ok())
        .filter(|&v| v > 2 && v < 1 << 20)
        .ok_or("Invalid inherited picker descriptor".into())
}
fn cloexec(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err("Inherited picker descriptor is unavailable".into());
    }
    Ok(())
}
fn read_message<T: serde::de::DeserializeOwned>(
    stream: &UnixStream,
    deadline: Deadline,
    limit: usize,
) -> Result<T> {
    let mut header = [0; 4];
    read_exact(stream, &mut header, deadline)?;
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > limit {
        return Err("Picker frame exceeds its bound".into());
    }
    let mut bytes = vec![0; size];
    read_exact(stream, &mut bytes, deadline)?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
fn read_exact(mut stream: &UnixStream, mut out: &mut [u8], deadline: Deadline) -> Result<()> {
    while !out.is_empty() {
        deadline.remaining()?;
        match stream.read(out) {
            Ok(0) => return Err("Original picker endpoint closed".into()),
            Ok(n) => {
                out = &mut out[n..];
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}
fn write_message<T: Serialize>(
    mut stream: &UnixStream,
    value: &T,
    deadline: Deadline,
    limit: usize,
) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Picker response exceeds its bound".into());
    }
    let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
    framed.extend(bytes);
    let mut pending = framed.as_slice();
    while !pending.is_empty() {
        deadline.remaining()?;
        match stream.write(pending) {
            Ok(0) => return Err("Original picker endpoint closed".into()),
            Ok(n) => pending = &pending[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

/// Send's binary segment can fill the small native socket buffer thousands of
/// times. Wait for that same descriptor instead of adding a sleep per chunk.
/// Every wake still returns through the original deadline and endpoint I/O.
fn wait_send(stream: &UnixStream, events: libc::c_short, deadline: Deadline) -> Result<()> {
    loop {
        let remaining = deadline.remaining()?;
        let timeout = remaining.as_millis().saturating_add(1).min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if ready > 0 {
            deadline.remaining()?;
            return Ok(());
        }
        if ready < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
}

fn read_send(mut stream: &UnixStream, mut bytes: &mut [u8], deadline: Deadline) -> Result<()> {
    while !bytes.is_empty() {
        deadline.remaining()?;
        match stream.read(bytes) {
            Ok(0) => return Err("Original Send endpoint closed".into()),
            Ok(n) => bytes = &mut bytes[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                wait_send(stream, libc::POLLIN, deadline)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {},
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

fn write_bytes(mut stream: &UnixStream, mut bytes: &[u8], deadline: Deadline) -> Result<()> {
    while !bytes.is_empty() {
        deadline.remaining()?;
        match stream.write(bytes) {
            Ok(0) => return Err("Original Send endpoint closed".into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => wait_send(stream, libc::POLLOUT, deadline)?,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {},
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

fn write_blob(stream: &UnixStream, bytes: &[u8], deadline: Deadline, limit: usize) -> Result<()> {
    if bytes.is_empty() || bytes.len() > limit { return Err("Original Send segment exceeds its bound".into()); }
    write_bytes(stream, &(bytes.len() as u32).to_be_bytes(), deadline)?;
    write_bytes(stream, bytes, deadline)
}

fn read_blob(stream: &UnixStream, deadline: Deadline, limit: usize) -> Result<Vec<u8>> {
    let mut size = [0;4];
    read_send(stream,&mut size,deadline)?;
    let size = u32::from_be_bytes(size) as usize;
    if size == 0 || size > limit { return Err("Original Send segment exceeds its bound".into()); }
    let mut bytes = vec![0;size];
    read_send(stream,&mut bytes,deadline)?;
    Ok(bytes)
}

fn write_outcome(stream: &UnixStream, outcome: &Outcome, frame: &Frame) -> Result<()> {
    if frame.operation != Operation::Send { return write_message(stream,outcome,frame.deadline,RESPONSE_BYTES); }
    let limit = input::native_send::REPLY_BYTES;
    if outcome.bytes.len() + 8 > limit { return Err("Original Send outcome exceeds 1 MiB".into()); }
    let mut bytes = outcome.code.to_be_bytes().to_vec();
    bytes.extend_from_slice(&outcome.bytes);
    write_blob(stream,&bytes,frame.deadline,limit - 4)
}

struct Slot {
    file: File,
    identity: crate::platform::FileIdentity,
}

pub(super) struct SendSlot(Arc<InheritedLock>);

/// Still disposition: failure grants no host and silently selects legacy.
pub(super) fn send_slot() -> Option<SendSlot> {
    Slot::reserve().ok().map(SendSlot)
}
impl Slot {
    fn reserve() -> Result<Arc<InheritedLock>> {
        let root =
            Directory::open(&crate::platform::canonical(root()).map_err(|e| e.to_string())?)?;
        if !root.owner_controls_entries()? {
            return Err("Picker host namespace entries are not owner-controlled".into());
        }
        let file = match root.read_private_optional(SLOT_FILE)? {
            Some(file) => file,
            None => root.write_new_file(SLOT_FILE, b"")?,
        };
        if file.metadata().map_err(|e| e.to_string())?.len() != 0 {
            return Err("Picker host capacity object changed".into());
        }
        InheritedLock::try_lock(file)
            .map_err(|e| format!("Original picker host is busy or unavailable: {e}"))
    }
    fn adopt(raw: i32) -> Result<Self> {
        cloexec(raw)?;
        let file = unsafe { File::from_raw_fd(raw) };
        if !crate::platform::verify_private_file(&file).map_err(|e| e.to_string())?
            || file.metadata().map_err(|e| e.to_string())?.len() != 0
        {
            return Err("Inherited picker capacity object is invalid".into());
        }
        file.try_lock_exclusive().map_err(|e| e.to_string())?;
        let identity = crate::platform::FileIdentity::of(&file).map_err(|e| e.to_string())?;
        let held = Self { file, identity };
        held.check()?;
        Ok(held)
    }
    fn check(&self) -> Result<()> {
        let state =
            Directory::open(&crate::platform::canonical(root()).map_err(|e| e.to_string())?)?;
        let named = state.read_private(SLOT_FILE)?;
        if !state.owner_controls_entries()?
            || crate::platform::FileIdentity::of(&named).map_err(|e| e.to_string())?
                != self.identity
            || crate::platform::FileIdentity::of(&self.file).map_err(|e| e.to_string())?
                != self.identity
        {
            return Err("Original picker host capacity object changed".into());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    schema: u8,
    nonce: String,
    image: Image,
    pid: u32,
    birth: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Outcome {
    pub(super) code: i32,
    pub(super) bytes: Vec<u8>,
}

pub(super) struct Control {
    stream: UnixStream,
    frame: Frame,
    slot: Slot,
    caller: super::caller::Held,
    payload: Option<Arc<input::native_send::Payload>>,
}
impl Control {
    #[cfg(test)]
    pub(super) fn fixture(frame: Frame, stream: UnixStream) -> Result<Arc<Self>> {
        let capacity = Slot::reserve()?;
        let raw = unsafe { libc::fcntl(capacity.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        stream.set_nonblocking(true).map_err(|e| e.to_string())?;
        let caller = frame.caller.hold()?;
        let payload = frame.send.as_ref().map(|header| header.verify_raw(&frame.raw).map(Arc::new)).transpose()?;
        let held = Arc::new(Self {
            stream,
            frame,
            slot: Slot::adopt(raw)?,
            caller,
            payload,
        });
        held.check()?;
        Ok(held)
    }
    /// Last commit boundary only: no backend callback, catalog refresh or
    /// filesystem staging runs between this and the namespace syscall.
    pub(super) fn commit_boundary(&self) -> Result<()> {
        self.frame.deadline.remaining()?;
        self.endpoint()
    }
    pub(super) fn frame(&self) -> &frame::Frame {
        &self.frame
    }
    pub(super) fn payload(&self) -> Result<&input::native_send::Payload> {
        self.payload.as_deref().ok_or("Original Send payload is unavailable".into())
    }
    fn endpoint(&self) -> Result<()> {
        let mut byte = 0u8;
        let read = unsafe {
            libc::recv(
                self.stream.as_raw_fd(),
                (&mut byte as *mut u8).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if read >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock {
            return Err("Original picker frontend closed or sent unexpected input".into());
        }
        Ok(())
    }
    pub(super) fn check(&self) -> Result<()> {
        self.frame.deadline.remaining()?;
        // Direct observation of THIS original endpoint, never an asynchronous
        // cancellation flag, PID liveness or another connection's readiness.
        self.endpoint()?;
        self.frame.check_context()?;
        self.slot.check()?;
        self.caller.check()?;
        self.frame.deadline.remaining()?;
        self.endpoint()
    }
}

/// LAST field of Scope. Rust drops every preceding original before this Drop
/// can send completion. No Scope::Drop or elapsed deadline manufactures it.
pub(super) struct Hold {
    control: Arc<Control>,
    complete: Option<mpsc::Sender<()>>,
}
impl Hold {
    pub(super) fn barrier(control: Arc<Control>) -> Self {
        Self {
            control,
            complete: None,
        }
    }
    pub(super) fn check(&self) -> Result<()> {
        self.control.check()
    }
}
impl Drop for Hold {
    fn drop(&mut self) {
        if let Some(complete) = &self.complete {
            let _ = complete.send(());
        }
    }
}

/// A disposable frontend closes its own endpoint on Drop. It NEVER kills the
/// actual keeper, reconstructs its PID or claims native completion from exit.
struct Frontend {
    stream: UnixStream,
    child: Child,
    deadline: Deadline,
    offered: bool,
}
impl Drop for Frontend {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}
impl Frontend {
    fn launch(frame: Frame) -> Result<Self> {
        frame.check_context()?;
        if std::env::var_os(CONTEXT).is_some() {
            return Err("Nested picker host refused".into());
        }
        let slot = Slot::reserve()?;
        Self::launch_reserved(frame, slot, None)
    }
    fn launch_reserved(frame: Frame, slot: Arc<InheritedLock>, offered: Option<&std::cell::Cell<bool>>) -> Result<Self> {
        if frame.operation==Operation::Send {
            frame.check_context()?;
            if std::env::var_os(CONTEXT).is_some() { return Err("Nested picker host refused".into()); }
        }
        let original = frame.clone();
        let held = slot.clone();
        let (send, ready) = mpsc::sync_channel(1);
        // One same-config capacity charge covers this cold start even if the
        // spawn turn or std::Command::spawn blocks. This worker owns no Root,
        // SID or native operation, and no request frame has been offered yet.
        // Timeout drops the receiver. A late result drops its original endpoint;
        // a late keeper can never acquire Root without the absent frame.
        let _worker = std::thread::Builder::new()
            .name("picker-host-start".into())
            .spawn(move || {
                let _ = send.send(cold_start(&original, &held));
            })
            .map_err(|e| e.to_string())?;
        let (parent, child) = ready
            .recv_timeout(frame.deadline.remaining()?)
            .map_err(|_| "Original picker host startup did not settle before its deadline")??;
        let mut front = Self {
            stream: parent,
            child,
            deadline: frame.deadline,
            offered: false,
        };
        let hello: Hello = read_message(&front.stream, frame.deadline, 4096)?;
        let identity = crate::platform::process_identity(front.child.id())
            .ok_or("Original picker keeper is unavailable")?;
        if hello.schema != 1
            || hello.nonce != frame.nonce
            || hello.image != frame.image
            || hello.pid != front.child.id()
            || hello.birth != identity.birth
        {
            return Err("Original picker keeper/image handshake changed".into());
        }
        #[cfg(target_os = "linux")]
        {
            let loaded = crate::platform::file_identity(Path::new(&format!(
                "/proc/{}/exe",
                front.child.id()
            )))
            .ok_or("Original keeper image unavailable")?;
            if frame.image.file
                != (
                    loaded.device,
                    loaded.inode,
                    loaded.modified,
                    loaded.modified_nanos,
                )
            {
                return Err("Original keeper loaded another image".into());
            }
        }
        frame.check_context()?;
        front.offered = true; // Raw frame may reach the original; no no-start claim.
        if let Some(offered) = offered { offered.set(true); }
        if frame.operation == Operation::Send {
            let mut envelope = frame.clone();
            envelope.raw.clear();
            write_message(&front.stream, &envelope, frame.deadline, frame::FRAME_BYTES)?;
            write_blob(&front.stream, &frame.raw, frame.deadline, input::native_send::RAW_BYTES)?;
        } else {
            write_message(&front.stream, &frame, frame.deadline, frame::FRAME_BYTES)?;
        }
        Ok(front)
    }
    fn receive(&self) -> Result<Outcome> {
        read_message(&self.stream, self.deadline, RESPONSE_BYTES)
    }
    fn receive_send(&self) -> Result<Outcome> {
        let bytes = read_blob(&self.stream,self.deadline,input::native_send::REPLY_BYTES - 4)?;
        if bytes.len() < 4 { return Err("Original Send outcome is truncated".into()); }
        Ok(Outcome { code:i32::from_be_bytes(bytes[..4].try_into().map_err(|_| "Invalid Send outcome")?), bytes:bytes[4..].to_vec() })
    }
    fn settled(&mut self) -> Result<()> {
        loop {
            self.deadline.remaining()?;
            if self.child.try_wait().map_err(|e| e.to_string())?.is_some() { return Ok(()); }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

pub(super) fn request_send(frame: Frame, slot: SendSlot) -> Result<Value> {
    let offered = std::cell::Cell::new(false);
    let header = frame.send.as_ref().ok_or("Original Send header missing")?.clone();
    let result = (|| {
        let mut frontend = Frontend::launch_reserved(frame.clone(),slot.0,Some(&offered))?;
        let outcome = frontend.receive_send()?;
        // The keeper's response follows its Root/SID/helper completion, but its
        // inherited slot is released on exit. Cache reads and analytics wait
        // for that same original Child, never a reconstructed PID.
        frontend.settled()?;
        if outcome.code != 0 { return Err(String::from_utf8_lossy(&outcome.bytes).into_owned()); }
        let analytics_root=frame.check_reconcile_context().is_ok().then(||Path::new(&frame.root));
        // Decode handles advisory cache repair only after this same keeper
        // has exited; the report bit never grants a native effect or retry.
        input::native_send::decode(&header,&outcome.bytes,analytics_root)
    })();
    result.or_else(|error| {
        if !offered.get() { return Err(error); }
        if frame.check_reconcile_context().is_ok() {
            if let Ok(Some(receipt)) = input::native_send::reconcile(&header) { return Ok(receipt); }
        }
        Err(input::native_send::uncertain(error))
    })
}

pub(super) fn request(frame: Frame) -> Result<Vec<u8>> {
    let frontend = Frontend::launch(frame)?;
    let outcome = frontend.receive()?;
    if outcome.code == crate::state::session_id::AMBIGUOUS {
        let value: Value = serde_json::from_slice(&outcome.bytes).map_err(|e| e.to_string())?;
        let text = value["message"]
            .as_str()
            .ok_or("Invalid original ambiguity message")?
            .to_owned();
        if !value["report"].is_object() {
            return Err("Invalid original ambiguity report".into());
        }
        return Err(crate::state::session_id::refuse((
            text,
            value["report"].clone(),
        )));
    }
    if outcome.code == 0 {
        Ok(outcome.bytes)
    } else {
        Err(String::from_utf8_lossy(&outcome.bytes).into_owned())
    }
}

/// Capacity is already held. Keep all descriptor creation/inheritance in the
/// existing spawn turn (including macOS's non-atomic pipe CLOEXEC setup). There
/// is no native effect or request frame in this cold start.
fn cold_start(frame: &Frame, slot: &Arc<InheritedLock>) -> Result<(UnixStream, Child)> {
    let _turn = crate::platform::spawn_turn();
    frame.deadline.remaining()?;
    let (parent, child_end) = UnixStream::pair().map_err(|e| e.to_string())?;
    parent.set_nonblocking(true).map_err(|e| e.to_string())?;
    child_end.set_nonblocking(true).map_err(|e| e.to_string())?;
    let descriptor = child_end.as_raw_fd();
    #[cfg(target_os = "linux")]
    let program = PathBuf::from("/proc/self/exe");
    #[cfg(not(target_os = "linux"))]
    let program = std::env::current_exe().map_err(|e| e.to_string())?;
    let candidate =
        crate::platform::file_identity(&program).ok_or("Picker host executable unavailable")?;
    if frame.image.file
        != (
            candidate.device,
            candidate.inode,
            candidate.modified,
            candidate.modified_nanos,
        )
    {
        return Err("Picker executable changed before spawn".into());
    }
    let mut command = Command::new(program);
    #[cfg(not(test))]
    command.args(["__state", ENTRY]);
    #[cfg(test)]
    tests::entry_arguments(&mut command);
    command
        .env(CONTROL, descriptor.to_string())
        .env(SLOT, slot.as_raw_fd().to_string())
        .env("ZERUS_PICKER_HOST_NONCE", &frame.nonce)
        .env("ZERUS_PICKER_HOST_UNTIL", frame.deadline.nanos.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = {
        frame.deadline.remaining()?;
        slot.inherit(&mut command).map_err(|e| e.to_string())?;
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(descriptor, libc::F_GETFD);
                if flags < 0
                    || libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                    || libc::setsid() < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn().map_err(|e| e.to_string())?
    };
    drop(child_end);
    Ok((parent, child))
}

fn acquire(control: &Control) -> Result<Guard> {
    let state = Directory::open(&crate::platform::canonical(root()).map_err(|e| e.to_string())?)?;
    let file = state.existing_private_lock(".lock")?;
    loop {
        control.check()?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                let guard = Guard::held(file);
                control.check()?;
                return Ok(guard);
            }
            Err(e) if crate::platform::lock_contended(&e) => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

pub(super) fn original_record(frame: &Frame) -> Result<Value> {
    let record = if string(&frame.binding, "session_id").is_empty() {
        crate::state::storage::read_metadata(string(&frame.binding, "name"))?
    } else {
        let state =
            Directory::open(&crate::platform::canonical(root()).map_err(|e| e.to_string())?)?;
        let sessions = state.child("sessions")?;
        let name = format!("{}.json", string(&frame.binding, "session_id"));
        let original = Original::open(&sessions, name)?;
        original.record(&sessions)?
    };
    let target = is_id_layout_record(&record, string(&record, "session_id"))
        && (string(&frame.binding, "session_id").is_empty()
            || record["session_id"] == frame.binding["session_id"])
        && string(&record, "name") == string(&frame.binding, "name");
    if !target {
        return Err("Original picker target changed before Root admission".into());
    }
    frame.validate_public_selection(&record)?;
    frame.validate_live_request(&record)?;
    Ok(record)
}

fn original_receipt(frame: &Frame) -> Result<Option<Vec<u8>>> {
    let state = Directory::open(&crate::platform::canonical(root()).map_err(|e| e.to_string())?)?;
    let directory = match frame.operation {
        Operation::Effort => "effort_receipts",
        Operation::Settings => "settings_receipts",
        Operation::Send => return input::native_send::cached(frame.send.as_ref().ok_or("Original Send header missing")?),
    };
    if std::fs::symlink_metadata(state.path.join(directory))
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(None);
    }
    let receipts = state.child(directory)?;
    let name = format!("{}.json", frame.request_id);
    let Some(mut file) = receipts.read_private_optional(&name)? else {
        return Ok(None);
    };
    let version = super::version(&file)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(65_537)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 65_536
        || super::version(&file)? != version
        || super::version(&receipts.read_private(&name)?)? != version
    {
        return Err("Original picker receipt is unavailable".into());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let digest = format!("{:x}", Sha256::digest(&frame.raw));
    if value["name"] != frame.binding["name"] || value["digest"] != digest {
        return Err("request ID was already used for different content".into());
    }
    let complete = match frame.operation {
        Operation::Effort => value["status"] == "confirmed",
        Operation::Settings => matches!(value["status"].as_str(), Some("applied" | "scheduled")),
        Operation::Send => return Err("Send cache must use existing input receipt semantics".into()),
    };
    if !complete {
        return Err(match frame.operation {
            Operation::Effort => "effort change may already have reached the provider; inspect Terminal before retrying",
            Operation::Settings => "settings change may already have reached the provider; inspect Terminal before retrying",
            Operation::Send => "delivery uncertain: inspect Terminal before retrying",
        }.into());
    }
    Ok(Some(bytes))
}

#[cfg(test)]
fn fixed_operation(frame: &Frame, scope: Scope<'static>, control: Arc<Control>) -> Result<Vec<u8>> {
    tests::operation(frame, scope, control)
}

/// Fixed private entry seam for existing ID Effort and Settings controllers.
/// The caller supplies only the exact literal entry; descriptors are inherited
/// native objects, never a decoded executable, PID, callback or permission.
pub(super) fn entry() -> Result<i32> {
    let raw_control = fd(std::env::var_os(CONTROL))?;
    let raw_slot = fd(std::env::var_os(SLOT))?;
    if raw_control == raw_slot {
        return Err("Aliased picker descriptors refused".into());
    }
    cloexec(raw_control)?;
    let stream = unsafe { UnixStream::from_raw_fd(raw_control) };
    stream.set_nonblocking(true).map_err(|e| e.to_string())?;
    let slot = Slot::adopt(raw_slot)?;
    let nanos = std::env::var("ZERUS_PICKER_HOST_UNTIL")
        .map_err(|e| e.to_string())?
        .parse()
        .map_err(|_| "Invalid original picker deadline")?;
    let deadline = Deadline { nanos };
    deadline.remaining()?;
    let nonce = std::env::var("ZERUS_PICKER_HOST_NONCE").map_err(|e| e.to_string())?;
    let own = crate::platform::process_identity(std::process::id())
        .ok_or("Original picker keeper unavailable")?;
    write_message(
        &stream,
        &Hello {
            schema: 1,
            nonce: nonce.clone(),
            image: Image::own()?,
            pid: std::process::id(),
            birth: own.birth,
        },
        deadline,
        4096,
    )?;
    let mut frame: Frame = read_message(&stream, deadline, frame::FRAME_BYTES)?;
    if frame.nonce != nonce || frame.deadline != deadline {
        return Err("Original picker frame handshake changed".into());
    }
    let payload = if frame.operation == Operation::Send {
        if !frame.raw.is_empty() { return Err("Original Send payload must use its binary segment".into()); }
        let header = frame.send.as_ref().ok_or("Original Send header missing")?;
        header.validate()?;
        frame.raw = read_blob(&stream,deadline,input::native_send::RAW_BYTES)?;
        Some(Arc::new(header.verify_raw(&frame.raw)?))
    } else { None };
    frame.check_context()?;
    std::env::set_var(CONTEXT, "1");
    let control = Arc::new(Control {
        stream,
        frame: frame.clone(),
        slot,
        caller: frame.caller.hold()?,
        payload,
    });
    control.check()?;
    let writer = acquire(&control)?;
    // Do NOT call ordinary writer recovery. Cached UUID lookup comes first;
    // the Effort driver then uses only its explicit original-owner Effects.
    // Host-only controls prepare already-recovered synthetic state.
    control.check()?;
    let cached = match original_receipt(&frame) {
        Ok(None) => None,
        Ok(Some(bytes)) => Some(Outcome { code: 0, bytes }),
        Err(error) => Some(Outcome {
            code: 1,
            bytes: error.into_bytes(),
        }),
    };
    if let Some(outcome) = cached {
        // This SAME Root lookup has physically settled, before any
        // driver/helper/SID/reservation/key. Its original refusal must
        // reach the frontend as data, not turn into an endpoint failure.
        // A known cache error supplies no replay or no-start authority.
        drop(writer);
        let _ = write_outcome(&control.stream, &outcome, &frame);
        return Ok(outcome.code);
    }
    #[cfg(not(test))]
    {
        let result = driver::run(writer, control.clone());
        let outcome = match result {
            Ok(bytes) => Outcome { code: 0, bytes },
            Err(error) => match crate::state::session_id::refused() {
                Some((text, report)) => Outcome {
                    code: crate::state::session_id::AMBIGUOUS,
                    bytes: json!({"message":text,"report":report})
                        .to_string()
                        .into_bytes(),
                },
                None => Outcome {
                    code: 1,
                    bytes: error.into_bytes(),
                },
            },
        };
        let _ = write_outcome(&control.stream, &outcome, &frame);
        Ok(outcome.code)
    }
    #[cfg(test)]
    {
        let until = frame.deadline.instant()?;
        let record = original_record(&frame)?;
        let receipts = match frame.operation {
            Operation::Effort => Receipts::Effort,
            Operation::Settings => Receipts::Settings,
            Operation::Send => Receipts::Send,
        };
        let mut scope = Scope::owned(writer, &record, receipts, until)?;
        let (complete, done) = mpsc::channel();
        scope.host = Some(Hold {
            control: control.clone(),
            complete: Some(complete),
        });
        let result = catch_unwind(AssertUnwindSafe(|| {
            fixed_operation(&frame, scope, control.clone())
        }));
        // Logical error, expiry, EOF and panic do not substitute for the SAME
        // scope's last-field completion. Lost wait keeps this owner alive.
        loop {
            match done.recv_timeout(Duration::from_millis(20)) {
                Ok(()) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => loop {
                    std::thread::park_timeout(Duration::from_secs(1));
                },
            }
        }
        let outcome = match result {
            Ok(Ok(bytes)) => Outcome { code: 0, bytes },
            Ok(Err(error)) => Outcome {
                code: 1,
                bytes: error.into_bytes(),
            },
            Err(_) => Outcome {
                code: 1,
                bytes: b"Original picker operation panicked; no replay".to_vec(),
            },
        };
        // Only after original physical resources are gone can a broken frontend
        // endpoint or result-write error let this private owner process return.
        let code = outcome.code;
        let _ = write_outcome(&control.stream, &outcome, &frame);
        Ok(code)
    }
}

#[cfg(test)]
mod tests;
