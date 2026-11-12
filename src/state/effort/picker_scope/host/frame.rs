//! Closed data frame, never a command, callback, permission or replay token.
use super::*;
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};

pub(in crate::state::effort::picker_scope) const FRAME_BYTES: usize = 48 << 10;
const REQUEST_BYTES: usize = 4096;
const MAX_BUDGET: u64 = 30_000_000_000;
type RootObject = (u64, u64, u32, u32, u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::state::effort::picker_scope) enum Operation {
    Effort,
    Settings,
    Send,
}
impl Operation {
    pub(in crate::state::effort::picker_scope) fn receipt_directory(self) -> &'static str {
        match self {
            Self::Effort => "effort_receipts",
            Self::Settings => "settings_receipts",
            Self::Send => "input_receipts",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::state::effort::picker_scope) struct Image {
    pub(in crate::state::effort::picker_scope) file: (u64, u64, i64, i64),
}
impl Image {
    pub(in crate::state::effort::picker_scope) fn own() -> Result<Self> {
        let image =
            crate::platform::image_identity().ok_or("Original picker host image unavailable")?;
        Ok(Self {
            file: (
                image.device,
                image.inode,
                image.modified,
                image.modified_nanos,
            ),
        })
    }
    pub(in crate::state::effort::picker_scope) fn check(&self) -> Result<()> {
        if Self::own()? != *self {
            return Err("Original picker host image changed".into());
        }
        Ok(())
    }
}

pub(in crate::state::effort::picker_scope) fn monotonic() -> Result<u64> {
    let mut clock: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut clock) } != 0
        || clock.tv_sec < 0
        || clock.tv_nsec < 0
    {
        return Err("Original picker monotonic clock unavailable".into());
    }
    (clock.tv_sec as u64)
        .checked_mul(1_000_000_000)
        .and_then(|v| v.checked_add(clock.tv_nsec as u64))
        .ok_or("Original picker clock overflow".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::state::effort::picker_scope) struct Deadline {
    pub(in crate::state::effort::picker_scope) nanos: u64,
}
impl Deadline {
    pub(in crate::state::effort::picker_scope) fn after(duration: Duration) -> Result<Self> {
        let budget = u64::try_from(duration.as_nanos()).map_err(|_| "Invalid picker budget")?;
        if budget == 0 || budget > MAX_BUDGET {
            return Err("Invalid original picker budget".into());
        }
        Ok(Self {
            nanos: monotonic()?
                .checked_add(budget)
                .ok_or("Picker deadline overflow")?,
        })
    }
    pub(in crate::state::effort::picker_scope) fn remaining(&self) -> Result<Duration> {
        let now = monotonic()?;
        let remaining = self
            .nanos
            .checked_sub(now)
            .filter(|v| *v > 0 && *v <= MAX_BUDGET)
            .ok_or("Original picker deadline expired or invalid")?;
        Ok(Duration::from_nanos(remaining))
    }
    pub(in crate::state::effort::picker_scope) fn instant(&self) -> Result<Instant> {
        // Snapshot Instant BEFORE the native clock query: conversion can only
        // shorten this original deadline, never renew it after self-exec.
        let instant = Instant::now();
        instant
            .checked_add(self.remaining()?)
            .ok_or("Invalid picker deadline".into())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::state::effort::picker_scope) struct Frame {
    pub(in crate::state::effort::picker_scope) schema: u8,
    pub(in crate::state::effort::picker_scope) nonce: String,
    pub(in crate::state::effort::picker_scope) image: Image,
    pub(in crate::state::effort::picker_scope) deadline: Deadline,
    pub(in crate::state::effort::picker_scope) configuration: String,
    pub(in crate::state::effort::picker_scope) caller: super::super::caller::Snapshot,
    pub(in crate::state::effort::picker_scope) root: String,
    pub(in crate::state::effort::picker_scope) root_object: RootObject,
    pub(in crate::state::effort::picker_scope) operation: Operation,
    /// Original stdin, byte-for-byte. Never deserialize and reserialize it for
    /// the existing receipt digest or an immutable original request identity.
    pub(in crate::state::effort::picker_scope) raw: Vec<u8>,
    /// Send alone transports raw as a separate binary segment. This immutable
    /// header is checked cheaply thereafter; the raw SHA is verified once on
    /// each side, never re-hashed by Control::check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::state::effort::picker_scope) send: Option<input::native_send::Header>,
    pub(in crate::state::effort::picker_scope) request_id: String,
    /// Only original record binding fields, preserving absent versus null.
    pub(in crate::state::effort::picker_scope) binding: Value,
    /// Only identities explicitly supplied by the original public selection,
    /// independent of the current native record and the request payload.
    pub(in crate::state::effort::picker_scope) selection: Option<Selection>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::state::effort::picker_scope) struct Selection {
    session_id: String,
    run: String,
    conversation: String,
}

fn canonical_uuid(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok_and(|u| !u.is_nil() && u.to_string() == s)
}
fn bounded(s: &str, max: usize, empty: bool) -> bool {
    (empty || !s.is_empty()) && s.len() <= max && !s.chars().any(char::is_control)
}
pub(in crate::state::effort::picker_scope) fn namespace() -> Result<(String, RootObject)> {
    let path = crate::platform::canonical(root()).map_err(|e| e.to_string())?;
    let directory = Directory::open(&path)?;
    if !directory.owner_controls_entries()? {
        return Err("Original picker state entries are not owner-controlled".into());
    }
    let file = directory.descriptor();
    let mut m: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file, &mut m) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok((
        path.to_str().ok_or("Picker root is not UTF-8")?.into(),
        (
            m.st_dev as u64,
            m.st_ino as u64,
            m.st_mode as u32,
            m.st_uid,
            m.st_gid,
        ),
    ))
}

pub(in crate::state::effort::picker_scope) fn configuration() -> Result<String> {
    let home = crate::platform::home().ok_or("Picker configuration has no home")?;
    let dir = crate::config::config_dir(&home);
    let location =
        zerus_desktop_migration::configuration(&root(), &dir).map_err(|e| e.to_string())?;
    let mut env = std::collections::BTreeMap::new();
    let mut size = 0usize;
    for (key, value) in std::env::vars_os() {
        let Some(name) = key.to_str() else { continue };
        if matches!(
            name,
            "HGS_PICKER_HOST_FD" | "HGS_PICKER_HOST_SLOT" | "HGS_PICKER_HOST_CONTEXT"
        ) {
            continue;
        }
        if name.starts_with("HGS_")
            || name.starts_with("LC_")
            || matches!(
                name,
                "HOME"
                    | "PATH"
                    | "LANG"
                    | "TMPDIR"
                    | "TMUX"
                    | "TMUX_TMPDIR"
                    | "XDG_STATE_HOME"
                    | "XDG_CONFIG_HOME"
            )
        {
            size = size.saturating_add(name.len() + value.as_encoded_bytes().len());
            if size > 256 << 10 || env.len() >= 4096 {
                return Err("Picker configuration exceeds its bound".into());
            }
            env.insert(name.to_owned(), value.as_encoded_bytes().to_vec());
        }
    }
    let config_file = dir.join("config");
    let bytes = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&config_file)
    {
        Ok(mut file) => {
            let m = file.metadata().map_err(|e| e.to_string())?;
            if !m.is_file() || m.len() > 1 << 20 {
                return Err("Original picker configuration is unavailable".into());
            }
            let before = (
                m.dev(),
                m.ino(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            );
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take((1 << 20) + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            let m = file.metadata().map_err(|e| e.to_string())?;
            if bytes.len() > 1 << 20
                || before
                    != (
                        m.dev(),
                        m.ino(),
                        m.len(),
                        m.mtime(),
                        m.mtime_nsec(),
                        m.ctime(),
                        m.ctime_nsec(),
                    )
            {
                return Err("Original picker configuration changed".into());
            }
            Some((before, bytes))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("Original picker configuration is unavailable".into()),
    };
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!({"location":location,"env":env,"config":bytes}))
                .map_err(|e| e.to_string())?
        )
    ))
}

impl Frame {
    pub(in crate::state::effort::picker_scope) fn resolve_send_label(&self) -> bool {
        self.operation==Operation::Send && self.selection.is_none()
    }
    pub(in crate::state::effort::picker_scope) fn new(
        operation: Operation,
        raw: Vec<u8>,
        record: &Value,
        deadline: Deadline,
    ) -> Result<Self> {
        Self::assemble(operation, raw, record, deadline, None)
    }

    pub(in crate::state::effort::picker_scope) fn new_send(
        raw: Vec<u8>,
        candidate: input::native_send::Candidate,
        deadline: Deadline,
    ) -> Result<Self> {
        candidate.header.verify_payload(&candidate.payload)?;
        Self::assemble(Operation::Send, raw, &candidate.record, deadline, Some(candidate.header))
    }

    fn assemble(
        operation: Operation,
        raw: Vec<u8>,
        record: &Value,
        deadline: Deadline,
        send: Option<input::native_send::Header>,
    ) -> Result<Self> {
        // A picker receipt is looked up before ANY current native binding.
        // The wire carries only the original verified selector; the actual
        // native record is captured under the one Root on a cache miss.
        let fields = &["session_id", "layout", "name"][..];
        let binding = fields
            .iter()
            .filter_map(|k| record.get(*k).map(|v| ((*k).into(), v.clone())))
            .collect::<serde_json::Map<_, _>>();
        let id = if let Some(header) = &send { header.request_id.clone() } else {
            serde_json::from_slice::<Value>(&raw).map_err(|e| e.to_string())?["request_id"]
            .as_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .ok_or("Invalid picker request UUID")?
            .to_string()
        };
        let (root, root_object) = namespace()?;
        let selection = crate::state::session_id::selected()
            .filter(|selection| selection.name == string(record, "name"))
            .map(|selection| Selection {
                session_id: selection.session_id,
                run: selection.run,
                conversation: selection.conversation,
            });
        let frame = Self {
            schema: 1,
            nonce: uuid::Uuid::new_v4().to_string(),
            image: Image::own()?,
            deadline,
            configuration: configuration()?,
            caller: super::super::caller::Snapshot::capture()?,
            root,
            root_object,
            operation,
            raw,
            send,
            request_id: id,
            binding: Value::Object(binding),
            selection,
        };
        frame.validate()?;
        Ok(frame)
    }
    pub(in crate::state::effort::picker_scope) fn validate(&self) -> Result<()> {
        self.caller.validate()?;
        if let Some(selection) = &self.selection {
            if (!selection.session_id.is_empty() && !canonical_uuid(&selection.session_id))
                || !bounded(&selection.run, 128, true)
                || !bounded(&selection.conversation, 2048, true)
            {
                return Err("Invalid original public picker selection".into());
            }
        }
        if self.schema != 1
            || !canonical_uuid(&self.nonce)
            || !uuid::Uuid::parse_str(&self.request_id)
                .is_ok_and(|id| id.to_string() == self.request_id)
            || self.raw.len() > if self.operation == Operation::Send { input::native_send::RAW_BYTES } else { REQUEST_BYTES }
            || self.raw.is_empty()
            || (self.operation == Operation::Send) != self.send.is_some()
            || self.configuration.len() != 64
            || !self.configuration.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid original picker frame".into());
        }
        let map = self.binding.as_object().ok_or("Invalid picker binding")?;
        let lookup_only = map.len() == 1 && map.contains_key("name");
        if map
            .keys()
            .any(|key| !["name", "session_id", "layout"].contains(&key.as_str()))
            || (!lookup_only
                && (!canonical_uuid(string(&self.binding, "session_id"))
                    || !is_id_layout_record(&self.binding, string(&self.binding, "session_id"))))
            || !bounded(string(&self.binding, "name"), 2048, false)
        {
            return Err("Invalid original picker selector".into());
        }
        let id = match self.operation {
            Operation::Effort => {
                let r: crate::state::effort::Request =
                    serde_json::from_slice(&self.raw).map_err(|e| e.to_string())?;
                if !valid(&r.effort)
                    || r.expected_run_id.is_empty()
                    || r.expected_conversation_id.is_empty()
                {
                    return Err("Invalid original effort request".into());
                }
                r.request_id
            }
            Operation::Settings => {
                let r: crate::state::effort::SettingsRequest =
                    serde_json::from_slice(&self.raw).map_err(|e| e.to_string())?;
                if r.expected_run_id.is_empty()
                    || (r.model.is_none() && r.effort.is_none())
                    || r.model.as_deref().is_some_and(|v| !safe_model(v))
                    || r.effort
                        .as_deref()
                        .is_some_and(|v| !v.is_empty() && !valid(v))
                {
                    return Err("Invalid original settings request".into());
                }
                r.request_id
            }
            Operation::Send => {
                let header = self.send.as_ref().ok_or("Original Send envelope missing")?;
                header.validate()?;
                if self.raw.len() != header.raw_len || string(&self.binding,"name") != header.dispatch_name {
                    return Err("Original Send selector or payload length changed".into());
                }
                header.request_id.clone()
            }
        };
        if uuid::Uuid::parse_str(&id)
            .map_err(|e| e.to_string())?
            .to_string()
            != self.request_id
        {
            return Err("Original picker request and selector disagree".into());
        }
        self.deadline.remaining()?;
        Ok(())
    }
    pub(in crate::state::effort::picker_scope) fn validate_live_request(
        &self,
        record: &Value,
    ) -> Result<()> {
        let request = self.request_guard()?;
        if !is_id_layout_record(record, string(record, "session_id"))
            || (!string(&self.binding, "session_id").is_empty()
                && record["session_id"] != self.binding["session_id"])
            || string(record, "name") != string(&self.binding, "name")
            || request.expected_run_id != string(record, "run_id")
            || request.expected_conversation_id != string(record, "conversation_id")
        {
            return Err("Session identity changed; refresh before changing effort".into());
        }
        Ok(())
    }

    pub(in crate::state::effort::picker_scope) fn validate_public_selection(
        &self,
        record: &Value,
    ) -> Result<()> {
        if let Some(selection) = &self.selection {
            if [
                (&selection.session_id, "session_id"),
                (&selection.run, "run_id"),
                (&selection.conversation, "conversation_id"),
            ]
            .iter()
            .any(|(expected, key)| !expected.is_empty() && expected.as_str() != string(record, key))
            {
                return Err("The original session reference no longer names this session; refresh and retry".into());
            }
        }
        Ok(())
    }

    pub(in crate::state::effort::picker_scope) fn request_guard(&self) -> Result<Request> {
        match &self.send {
            Some(header) => Ok(Request {
                request_id: header.request_id.clone(),
                expected_run_id: header.run.clone(),
                expected_conversation_id: header.conversation.clone(),
                effort: String::new(),
            }),
            None => super::super::driver::request_guard(self.operation, &self.raw),
        }
    }
    pub(in crate::state::effort::picker_scope) fn check_context(&self) -> Result<()> {
        self.validate()?;
        self.check_reconcile_context()
    }

    /// Read-only terminal-receipt reconciliation may outlive the input clock.
    /// This grants no native observation, effect, lock or publication.
    pub(in crate::state::effort::picker_scope) fn check_reconcile_context(&self) -> Result<()> {
        self.caller.validate()?;
        self.image.check()?;
        if namespace()? != (self.root.clone(), self.root_object)
            || configuration()? != self.configuration
        {
            return Err("Original picker configuration or namespace changed".into());
        }
        Ok(())
    }
}
