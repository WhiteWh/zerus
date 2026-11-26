//! The full-Zerus route gate (phase 5). Every `hgs` command that acts on this
//! computer passes [`admit`] after remote forwarding and before it touches
//! state. On a legacy host the gate admits everything, as before. On a
//! multi-person host:
//!
//! - a broken policy refuses every gated route;
//! - the state folder must belong to the caller's verified OS identity, so a
//!   redirected `HGS_STATE_DIR` cannot reach another person's sessions,
//!   accounts, logs or usage;
//! - a bound caller's state folder may carry an explicit binding
//!   (`hgs host-access bind`) to one host, person and context; a folder bound
//!   to anyone else is refused for every session and account route, so an OS
//!   account rebound by the operator never inherits the previous person's
//!   sessions or accounts;
//! - shared project routes need a bound caller and an explicit binding;
//!   historical, unbound state stays private;
//! - moving provider credentials (`account copy`, `_export`, `_import`) is
//!   refused for a bound caller: a rotating grant must not gain a second
//!   refresh writer on a shared host.
//!
//! Refusals are typed: `host-access/<code>: <text>`, which the desktop core
//! parses (`client/core/src/host_access.rs`).
use super::runtime::{owned_by, Mode, Principal};
use super::CONTRACT;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The help marker of this capability (`hgs --help`). A client checks it
/// before any shared-history or remote-compute route; an older host answers
/// with a typed unsupported error, never a fallback.
pub const MARKER: &str = "Multi-person access ABI: multi-person-access-v1";
pub const CAPABILITY: &str = "multi-person-access-v1";
pub const BINDING_SCHEMA: &str = "zerus-host-access-binding-v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub detail: String,
}

impl Refusal {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self { code, detail: detail.into() }
    }
    pub fn message(&self) -> String {
        format!("host-access/{}: {} ({CONTRACT})", self.code, self.detail)
    }
}

impl From<Refusal> for crate::cli::Error {
    fn from(refusal: Refusal) -> Self {
        crate::cli::Error::new(1, refusal.message())
    }
}

/// What a command does, for the gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Reads compiled-in text or this computer's identity, or does its own
    /// checks (`host-access`), or is a session's own hook (`__state`, `--run`).
    Ungated,
    /// Acts on the caller's sessions, accounts, logs, usage or settings.
    Private,
    /// Moves provider credentials between computers.
    CredentialTransfer,
}

const UNGATED: &[&str] = &[
    "-h", "--help", "help", "--version", "version", "status", "guide", "path-ref", "self", "host-access", "__state",
    "__peer", "--run", "--expect-machine",
];

pub fn classify(command: &str, args: &[String]) -> Class {
    if UNGATED.contains(&command) {
        return Class::Ungated;
    }
    if command == "account" && args.first().is_some_and(|op| matches!(op.as_str(), "copy" | "_export" | "_import")) {
        return Class::CredentialTransfer;
    }
    // Every other command, including an agent launch (the CLI's fallback arm),
    // acts on the caller's own state.
    Class::Private
}

/// The result of an admitted call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    Legacy,
    /// A multi-person host and an OS identity without a binding.
    Private,
    Bound(Principal),
}

/// The binding of a state folder to one host, person and context.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub schema: String,
    pub host: String,
    pub person: String,
    pub context: String,
    pub os_identity: String,
    pub policy_revision: u64,
    pub bound_at: u64,
}

pub fn binding_path(state_root: &Path) -> PathBuf {
    state_root.join("host-access").join("binding.json")
}

/// The state folder's binding: None when there is none; an unreadable or
/// malformed binding is a refusal (fail closed).
pub fn read_binding(state_root: &Path) -> Result<Option<Binding>, Refusal> {
    let path = binding_path(state_root);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Refusal::new("binding-unreadable", "the state folder's host binding cannot be read")),
    };
    if bytes.len() > 4096 {
        return Err(Refusal::new("binding-malformed", "the state folder's host binding is malformed"));
    }
    let binding: Binding = serde_json::from_slice(&bytes)
        .map_err(|_| Refusal::new("binding-malformed", "the state folder's host binding is malformed"))?;
    if binding.schema != BINDING_SCHEMA {
        return Err(Refusal::new("binding-malformed", "the state folder's host binding is malformed"));
    }
    Ok(Some(binding))
}

fn matches(binding: &Binding, host: &str, principal: &Principal) -> bool {
    binding.host == host
        && binding.person == principal.person
        && binding.context == principal.context
        && binding.os_identity == principal.os.0
}

/// Admits `command` for `mode` against `state_root`.
pub fn admit(mode: &Mode, command: &str, args: &[String], state_root: &Path) -> Result<Admission, Refusal> {
    let class = classify(command, args);
    if class == Class::Ungated {
        return Ok(match mode {
            Mode::Bound { principal, .. } => Admission::Bound(principal.clone()),
            Mode::Legacy => Admission::Legacy,
            _ => Admission::Private,
        });
    }
    match mode {
        Mode::Legacy => Ok(Admission::Legacy),
        Mode::Broken(code) => Err(Refusal::new(
            "policy-broken",
            format!("this multi-person host's access policy cannot be trusted ({code}); ask its operator"),
        )),
        Mode::Unbound { os, .. } => {
            if !owned_by(state_root, os) {
                return Err(Refusal::new("state-owner", "the state folder belongs to another OS account"));
            }
            // An unbound identity keeps its legacy private use, account copies
            // included: no person's shared resources are involved.
            Ok(Admission::Private)
        }
        Mode::Bound { policy, principal } => {
            if !owned_by(state_root, &principal.os) {
                return Err(Refusal::new("state-owner", "the state folder belongs to another OS account"));
            }
            if let Some(binding) = read_binding(state_root)? {
                if !matches(&binding, &policy.host, principal) {
                    return Err(Refusal::new(
                        "owner-mismatch",
                        "this state folder is bound to another person or context; its sessions and accounts stay theirs",
                    ));
                }
            }
            if class == Class::CredentialTransfer {
                return Err(Refusal::new(
                    "account-transfer-unsupported",
                    "provider credentials are not copied on a multi-person host; keep the account on your workstation",
                ));
            }
            Ok(Admission::Bound(principal.clone()))
        }
    }
}

/// Requires a bound caller whose state folder carries a matching binding: the
/// gate of every shared project route.
pub fn require_bound(mode: &Mode, state_root: &Path) -> Result<Principal, Refusal> {
    match mode {
        Mode::Legacy => Err(Refusal::new("multi-person-disabled", "this host does not share projects between people")),
        Mode::Broken(code) => Err(Refusal::new("policy-broken", format!("this host's access policy cannot be trusted ({code})"))),
        Mode::Unbound { .. } => Err(Refusal::new("binding-required", "this OS account is not bound to a person on this host")),
        Mode::Bound { policy, principal } => {
            if !owned_by(state_root, &principal.os) {
                return Err(Refusal::new("state-owner", "the state folder belongs to another OS account"));
            }
            match read_binding(state_root)? {
                Some(binding) if matches(&binding, &policy.host, principal) => Ok(principal.clone()),
                Some(_) => Err(Refusal::new("owner-mismatch", "this state folder is bound to another person or context")),
                None => Err(Refusal::new(
                    "binding-required",
                    "bind this state folder first (hgs host-access bind); unbound history stays private",
                )),
            }
        }
    }
}

/// Writes the explicit binding of `state_root` to the bound caller. An existing
/// binding to the same principal is kept; any other is refused.
pub fn bind(mode: &Mode, state_root: &Path, now: u64) -> Result<Binding, Refusal> {
    let Mode::Bound { policy, principal } = mode else {
        return Err(match mode {
            Mode::Broken(code) => Refusal::new("policy-broken", format!("this host's access policy cannot be trusted ({code})")),
            Mode::Legacy => Refusal::new("multi-person-disabled", "this host does not share projects between people"),
            _ => Refusal::new("binding-required", "this OS account is not bound to a person on this host"),
        });
    };
    if !owned_by(state_root, &principal.os) {
        return Err(Refusal::new("state-owner", "the state folder belongs to another OS account"));
    }
    if let Some(existing) = read_binding(state_root)? {
        if matches(&existing, &policy.host, principal) {
            return Ok(existing);
        }
        return Err(Refusal::new("owner-mismatch", "this state folder is already bound to another person or context"));
    }
    let binding = Binding {
        schema: BINDING_SCHEMA.into(),
        host: policy.host.clone(),
        person: principal.person.clone(),
        context: principal.context.clone(),
        os_identity: principal.os.0.clone(),
        policy_revision: policy.revision,
        bound_at: now,
    };
    let path = binding_path(state_root);
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        let dir = path.parent().expect("binding has a parent");
        std::fs::create_dir_all(dir)?;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        crate::platform::make_private(&file)?;
        file.write_all(&serde_json::to_vec(&binding).expect("binding serializes"))?;
        file.sync_all()
    };
    match write() {
        Ok(()) => Ok(binding),
        // A concurrent bind: accept it only if it is the same principal.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => match read_binding(state_root)? {
            Some(existing) if matches(&existing, &policy.host, principal) => Ok(existing),
            _ => Err(Refusal::new("owner-mismatch", "this state folder is already bound to another person or context")),
        },
        Err(_) => Err(Refusal::new("binding-write", "the binding could not be saved")),
    }
}
