//! Host access contract `zerus-host-access-v1` (epic zerus-un7n): the person,
//! device, host, execution-context and project-membership policy that full
//! Zerus and the model-free artifact executor share. The contract and its
//! fixtures are in `contracts/host-access-v1/`; the executor's Go
//! implementation must agree with this one on every fixture.
//!
//! The caller is always derived from a verified device: a request's person,
//! owner or role field is never consulted. A person's key signs the enrollment
//! of each of their devices; the host operator's private policy remains the
//! membership authority.
//!
//! Full Zerus consults it through `gate` (every local route), `shared`
//! (project history and memory), `person` (person operations of operator-bound
//! OS accounts) and `enrollment` (device enrollment requests).
#![allow(dead_code)] // Consumed by the later full-host milestone (zerus-un7n.3).

use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub mod command;
pub mod enrollment;
pub mod person;
pub mod compute;
pub mod gate;
pub mod providers;
pub mod runtime;
pub mod shared;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod route_tests;

pub const CONTRACT: &str = "zerus-host-access-v1";
pub const MAX_POLICY_BYTES: usize = 1024 * 1024;
const MAX_TIME: u64 = (1 << 53) - 1;
const MAX_PERSONS: usize = 256;
const MAX_DEVICES: usize = 1024;
const MAX_PROJECTS: usize = 256;
const MAX_MEMBERS: usize = 256;
const MAX_OPERATORS: usize = 64;
const MAX_RIGHTS: usize = 4;
const MAX_CONTEXTS: usize = 256;
const MAX_REVOKED_DEVICES: usize = 4096;
const MAX_REVOKED_PERSONS: usize = 256;

/// A verification failure, by its contract code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyError {
    TooLarge,
    Malformed,
    Contract,
    Host,
    HostMismatch,
    Revision,
    Limits,
    Identifier,
    PersonKey,
    PersonId,
    Duplicate,
    UnknownPerson,
    Window,
    Signature,
    Role,
    Right,
}

impl PolicyError {
    pub fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "too-large",
            Self::Malformed => "malformed",
            Self::Contract => "contract",
            Self::Host => "host",
            Self::HostMismatch => "host-mismatch",
            Self::Revision => "revision",
            Self::Limits => "limits",
            Self::Identifier => "identifier",
            Self::PersonKey => "person-key",
            Self::PersonId => "person-id",
            Self::Duplicate => "duplicate",
            Self::UnknownPerson => "unknown-person",
            Self::Window => "window",
            Self::Signature => "signature",
            Self::Role => "role",
            Self::Right => "right",
        }
    }
}

// The wire form. Every member defaults to its empty value, so a missing member
// fails the same semantic check in both implementations; unknown, duplicate
// and mistyped members (including null) are refused by serde.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
#[derive(Default)]
struct Wire {
    contract: String,
    host: String,
    revision: u64,
    persons: Vec<WirePerson>,
    devices: Vec<WireDevice>,
    projects: Vec<WireProject>,
    operators: Vec<WireOperator>,
    contexts: Vec<WireContext>,
    revoked_devices: Vec<String>,
    revoked_persons: Vec<String>,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct WirePerson {
    person: String,
    public_key: String,
    label: String,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct WireDevice {
    device: String,
    person: String,
    issued: u64,
    expires: u64,
    signature: String,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct WireProject {
    project: String,
    members: Vec<WireMember>,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct WireMember {
    person: String,
    role: String,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct WireOperator {
    person: String,
    rights: Vec<String>,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct WireContext {
    person: String,
    context: String,
    backend: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Reader,
    Contributor,
}
impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Contributor => "contributor",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Enrollment {
    pub person: String,
    pub issued: u64,
    pub expires: u64,
}

#[derive(Clone, Debug)]
pub struct Context {
    pub context: String,
    pub backend: String,
}

/// A verified policy for one host.
#[derive(Clone, Debug)]
pub struct Policy {
    pub host: String,
    pub revision: u64,
    pub labels: BTreeMap<String, String>,
    pub devices: BTreeMap<String, Enrollment>,
    pub projects: BTreeMap<String, BTreeMap<String, Role>>,
    pub cancel_operators: BTreeSet<String>,
    pub contexts: BTreeMap<String, Context>,
    pub revoked_devices: BTreeSet<String>,
    pub revoked_persons: BTreeSet<String>,
}

pub fn host_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(b))
}
pub(crate) fn label(value: &str) -> bool {
    let count = value.chars().count();
    (1..=160).contains(&count) && !value.chars().any(char::is_control)
}
pub(crate) fn decode_b64<const N: usize>(value: &str) -> Option<[u8; N]> {
    use base64::Engine;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value).ok()?;
    // Re-encoding rejects non-canonical trailing bits as well as padding.
    if base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&raw) != value {
        return None;
    }
    raw.try_into().ok()
}

/// The person key, accepted only as a canonical encoding (y < p) of a point
/// that is not of small order, the same rule as the mobile protocol's keys.
pub fn person_key(value: &str) -> Option<VerifyingKey> {
    let bytes = decode_b64::<32>(value)?;
    let mut y = bytes;
    y[31] &= 0x7f;
    let mut p = [0xff; 32];
    p[0] = 0xed;
    p[31] = 0x7f;
    if y.iter().rev().cmp(p.iter().rev()) != std::cmp::Ordering::Less {
        return None;
    }
    let key = VerifyingKey::from_bytes(&bytes).ok()?;
    (!key.is_weak()).then_some(key)
}

pub fn person_id(key: &VerifyingKey) -> String {
    hex(&Sha256::digest(key.as_bytes()))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The exact bytes a person's key signs to enroll a device.
pub fn enrollment_statement(host: &str, person: &str, device: &str, issued: u64, expires: u64) -> Vec<u8> {
    format!(
        "{CONTRACT}/device-enrollment\nhost={host}\nperson={person}\ndevice={device}\nissued={issued}\nexpires={expires}\n"
    )
    .into_bytes()
}

/// Verify a policy document for `expected_host`.
pub fn verify(bytes: &[u8], expected_host: &str) -> Result<Policy, PolicyError> {
    use PolicyError as E;
    if bytes.len() > MAX_POLICY_BYTES {
        return Err(E::TooLarge);
    }
    let wire: Wire = serde_json::from_slice(bytes).map_err(|_| E::Malformed)?;
    if wire.contract != CONTRACT {
        return Err(E::Contract);
    }
    if !host_id(&wire.host) {
        return Err(E::Host);
    }
    if wire.host != expected_host {
        return Err(E::HostMismatch);
    }
    if wire.revision < 1 || wire.revision > MAX_TIME {
        return Err(E::Revision);
    }
    if wire.persons.len() > MAX_PERSONS
        || wire.devices.len() > MAX_DEVICES
        || wire.projects.len() > MAX_PROJECTS
        || wire.projects.iter().any(|p| p.members.len() > MAX_MEMBERS)
        || wire.operators.len() > MAX_OPERATORS
        || wire.operators.iter().any(|o| o.rights.len() > MAX_RIGHTS)
        || wire.contexts.len() > MAX_CONTEXTS
        || wire.revoked_devices.len() > MAX_REVOKED_DEVICES
        || wire.revoked_persons.len() > MAX_REVOKED_PERSONS
    {
        return Err(E::Limits);
    }
    let mut keys = BTreeMap::new();
    let mut labels = BTreeMap::new();
    for person in &wire.persons {
        if !host_id(&person.person) || !label(&person.label) {
            return Err(E::Identifier);
        }
        let key = person_key(&person.public_key).ok_or(E::PersonKey)?;
        if person_id(&key) != person.person {
            return Err(E::PersonId);
        }
        if keys.insert(person.person.clone(), key).is_some() {
            return Err(E::Duplicate);
        }
        labels.insert(person.person.clone(), person.label.clone());
    }
    let mut devices = BTreeMap::new();
    for device in &wire.devices {
        if !host_id(&device.device) || !host_id(&device.person) {
            return Err(E::Identifier);
        }
        if devices.contains_key(&device.device) {
            return Err(E::Duplicate);
        }
        let key = keys.get(&device.person).ok_or(E::UnknownPerson)?;
        if device.issued >= device.expires || device.expires > MAX_TIME {
            return Err(E::Window);
        }
        let signature = decode_b64::<64>(&device.signature).ok_or(E::Signature)?;
        let statement = enrollment_statement(&wire.host, &device.person, &device.device, device.issued, device.expires);
        key.verify_strict(&statement, &Signature::from_bytes(&signature)).map_err(|_| E::Signature)?;
        devices.insert(
            device.device.clone(),
            Enrollment { person: device.person.clone(), issued: device.issued, expires: device.expires },
        );
    }
    let mut projects = BTreeMap::new();
    for project in &wire.projects {
        if !name(&project.project) {
            return Err(E::Identifier);
        }
        if projects.contains_key(&project.project) {
            return Err(E::Duplicate);
        }
        let mut members = BTreeMap::new();
        for member in &project.members {
            if !host_id(&member.person) {
                return Err(E::Identifier);
            }
            if !keys.contains_key(&member.person) {
                return Err(E::UnknownPerson);
            }
            if members.contains_key(&member.person) {
                return Err(E::Duplicate);
            }
            let role = match member.role.as_str() {
                "reader" => Role::Reader,
                "contributor" => Role::Contributor,
                _ => return Err(E::Role),
            };
            members.insert(member.person.clone(), role);
        }
        projects.insert(project.project.clone(), members);
    }
    let mut operators = BTreeSet::new();
    let mut cancel_operators = BTreeSet::new();
    for operator in &wire.operators {
        if !host_id(&operator.person) {
            return Err(E::Identifier);
        }
        if !keys.contains_key(&operator.person) {
            return Err(E::UnknownPerson);
        }
        if !operators.insert(operator.person.clone()) {
            return Err(E::Duplicate);
        }
        let mut rights = BTreeSet::new();
        for right in &operator.rights {
            if right != "resource-cancel" {
                return Err(E::Right);
            }
            if !rights.insert(right.clone()) {
                return Err(E::Duplicate);
            }
        }
        if rights.contains("resource-cancel") {
            cancel_operators.insert(operator.person.clone());
        }
    }
    let mut contexts = BTreeMap::new();
    for context in &wire.contexts {
        if !host_id(&context.person) || !name(&context.context) || !name(&context.backend) {
            return Err(E::Identifier);
        }
        if !keys.contains_key(&context.person) {
            return Err(E::UnknownPerson);
        }
        if contexts.contains_key(&context.person) {
            return Err(E::Duplicate);
        }
        contexts.insert(
            context.person.clone(),
            Context { context: context.context.clone(), backend: context.backend.clone() },
        );
    }
    let mut revoked_devices = BTreeSet::new();
    for device in &wire.revoked_devices {
        if !host_id(device) {
            return Err(E::Identifier);
        }
        if !revoked_devices.insert(device.clone()) {
            return Err(E::Duplicate);
        }
    }
    let mut revoked_persons = BTreeSet::new();
    for person in &wire.revoked_persons {
        if !host_id(person) {
            return Err(E::Identifier);
        }
        if !keys.contains_key(person) {
            return Err(E::UnknownPerson);
        }
        if !revoked_persons.insert(person.clone()) {
            return Err(E::Duplicate);
        }
    }
    Ok(Policy {
        host: wire.host,
        revision: wire.revision,
        labels,
        devices,
        projects,
        cancel_operators,
        contexts,
        revoked_devices,
        revoked_persons,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Capabilities,
    JobsList,
    JobsRead,
    ArtifactsRead,
    JobsSubmit,
    JobsControl,
    JobsCancel,
}
impl Operation {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "capabilities" => Self::Capabilities,
            "jobs.list" => Self::JobsList,
            "jobs.read" => Self::JobsRead,
            "artifacts.read" => Self::ArtifactsRead,
            "jobs.submit" => Self::JobsSubmit,
            "jobs.control" => Self::JobsControl,
            "jobs.cancel" => Self::JobsCancel,
            _ => return None,
        })
    }
}

/// How an allowed caller holds the operation's authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Device,
    Member,
    Operator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub person: String,
    pub role: Option<Role>,
    pub via: Via,
}

pub fn authorize(
    policy: &Policy,
    device: &str,
    now: u64,
    operation: &str,
    project: &str,
) -> Result<Decision, &'static str> {
    let operation = Operation::parse(operation).ok_or("operation-unknown")?;
    let enrollment = policy.devices.get(device).ok_or("device-unknown")?;
    if policy.revoked_devices.contains(device) {
        return Err("device-revoked");
    }
    if policy.revoked_persons.contains(&enrollment.person) {
        return Err("person-revoked");
    }
    if now < enrollment.issued {
        return Err("enrollment-not-yet-valid");
    }
    if now >= enrollment.expires {
        return Err("enrollment-expired");
    }
    let person = enrollment.person.clone();
    if matches!(operation, Operation::Capabilities | Operation::JobsList) {
        return Ok(Decision { person, role: None, via: Via::Device });
    }
    let members = policy.projects.get(project).ok_or("project-unknown")?;
    let role = members.get(&person).copied();
    if operation == Operation::JobsCancel && role.is_none() && policy.cancel_operators.contains(&person) {
        return Ok(Decision { person, role: None, via: Via::Operator });
    }
    let role = role.ok_or("not-a-member")?;
    let needs_contributor = matches!(operation, Operation::JobsSubmit | Operation::JobsControl | Operation::JobsCancel);
    if needs_contributor && role != Role::Contributor {
        return Err("role-insufficient");
    }
    Ok(Decision { person, role: Some(role), via: Via::Member })
}

/// A private provider account reference: `(person, context, provider,
/// account)`. Two people's `native-codex` are different references. It never
/// carries an auth home, credential path or token.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AccountRef {
    pub person: String,
    pub context: String,
    pub provider: String,
    pub account: String,
}
impl AccountRef {
    pub fn new(person: &str, context: &str, provider: &str, account: &str) -> Option<Self> {
        let account_ok = !account.is_empty()
            && account.len() <= 80
            && !account.starts_with('-')
            && account.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b));
        (host_id(person) && name(context) && name(provider) && account_ok).then(|| Self {
            person: person.into(),
            context: context.into(),
            provider: provider.into(),
            account: account.into(),
        })
    }
    pub fn key(&self) -> String {
        format!("zerus-account-v1:{}/{}/{}/{}", self.person, self.context, self.provider, self.account)
    }
}
