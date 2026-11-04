//! Shared quit-journal schema and negative write planning, without a store.
//!
//! Every record, clock, usage observation and plan here is DATA. None proves a
//! current producer, global namespace, durable reservation, native permission,
//! receipt, private file, custody or drain. A future shared store must re-read
//! its real indexed rows/usage and commit the entire expected before-image in
//! one transaction. A host must separately qualify every native boundary.
//!
//! This module opens no file, generates no nonce, calls no process, releases no
//! resources and cannot reconstruct a runtime dispatcher from serialized state.
use crate::{creation, end_action::Captured, paths, termination_cleanup, Action, Target};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{self, Write},
};

mod planning;
pub use planning::*;

pub const VERSION: u32 = 1;
pub const QUIT_NS: u64 = 5_000_000_000;
pub const MAX_GROUPS: usize = 5;
pub const MAX_MEMBERS: usize = 50_000;
pub const CAPTURE_BYTES: usize = 16 << 20;
pub const CHARGED_BYTES: u64 = 64 << 20;
pub const RETAINED_ROWS: u64 = 500_000;
pub const RETAINED_SUMMARIES: u64 = 16_384;
pub const EXCHANGES: u32 = 4;
pub const MAX_REVISION: u64 = 9_007_199_254_740_991;
/// Reserved metadata budget per row, separate from unqualified SQLite pages.
pub const ROW_CHARGE: u64 = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    TooMany,
    TooLarge,
    Capacity,
    Busy,
    Conflict,
    Unavailable,
    Expired,
    Unsupported,
    Spent,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "Quit action data could not be verified.",
            Self::TooMany => "Too many quit actions selected.",
            Self::TooLarge => "Quit action data exceeds its byte limit.",
            Self::Capacity => "The quit journal has no reserved capacity.",
            Self::Busy => "An earlier quit generation still holds its resources.",
            Self::Conflict => "An original quit request or target is already retained.",
            Self::Unavailable => "Quit reservation evidence is unavailable.",
            Self::Expired => "The original quit admission window is closed.",
            Self::Unsupported => "Durable quit delivery is unavailable on this platform.",
            Self::Spent => "This quit member or generation cannot be issued again.",
        })
    }
}
impl std::error::Error for Error {}

// Existing shared Action/Target serde is permissive for older callers. This
// new versioned journal closes its own wire without changing those callers.
fn closed<'de, D: Deserializer<'de>, T: DeserializeOwned + Serialize>(
    de: D,
) -> Result<T, D::Error> {
    let raw = Box::<serde_json::value::RawValue>::deserialize(de)?;
    // Parse the typed value from the original bytes too: an intermediate Value
    // alone would erase duplicate native action/target keys before validation.
    let value: T = serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)?;
    let input: serde_json::Value =
        serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)?;
    if serde_json::to_value(&value).map_err(serde::de::Error::custom)? != input {
        return Err(serde::de::Error::custom(
            "Noncanonical or future quit descriptor",
        ));
    }
    Ok(value)
}

// Compact capture tuples keep the real 50k target within the SAME whole-JSON
// 16MiB budget, including request UUIDs, digests and optional cleanup evidence.
// The original Captured validator is still authoritative for pure shape checks.
mod capture_wire {
    use super::*;
    #[derive(Deserialize)]
    struct Wire(
        [String; 4],
        #[serde(deserialize_with = "closed")] Action,
        String,
    );
    pub fn serialize<S: Serializer>(c: &Captured, s: S) -> Result<S::Ok, S::Error> {
        (
            (
                &c.target.computer,
                &c.target.name,
                &c.target.run_id,
                &c.target.conversation_id,
            ),
            &c.action,
            &c.state,
        )
            .serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Captured, D::Error> {
        let Wire([computer, name, run_id, conversation_id], action, state) = Wire::deserialize(d)?;
        Ok(Captured {
            target: Target {
                computer,
                name,
                run_id,
                conversation_id,
            },
            action,
            state,
        })
    }
}

fn present_option<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

fn bounded(s: &str, max: usize, required: bool) -> bool {
    (!required || !s.is_empty()) && s.len() <= max && !s.chars().any(char::is_control)
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn check(ok: bool) -> Result<(), Error> {
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Linux,
    Macos,
    Windows,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clock {
    pub boot: String,
    pub ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuiBirth {
    pub pid: u32,
    pub birth: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub quit_id: String,
    pub configuration_sha256: String,
    pub gui: GuiBirth,
    pub platform: Platform,
    /// The ORIGINAL real-Quit freeze, never an action confirmation/helper start.
    pub boot: String,
    pub quit_confirmed_ns: u64,
    pub cutoff_ns: u64,
}
impl Context {
    pub fn validate(&self) -> Result<(), Error> {
        check(
            creation::canonical_uuid(&self.quit_id)
                && digest(&self.configuration_sha256)
                && self.gui.pid != 0
                && bounded(&self.gui.birth, 256, true)
                && creation::canonical_uuid(&self.boot)
                && self.quit_confirmed_ns.checked_add(QUIT_NS) == Some(self.cutoff_ns),
        )
    }
}

/// An independently supplied observation, still not native/currentness proof.
#[derive(Clone, Debug)]
pub struct Observation {
    pub platform: Platform,
    pub clock: Clock,
}
impl Context {
    fn check_window(&self, observation: &Observation) -> Result<(), Error> {
        self.validate()?;
        if observation.platform == Platform::Windows || self.platform == Platform::Windows {
            return Err(Error::Unsupported);
        }
        if observation.platform != self.platform {
            return Err(Error::Invalid);
        }
        if observation.clock.boot != self.boot
            || observation.clock.ns < self.quit_confirmed_ns
            || observation.clock.ns >= self.cutoff_ns
        {
            return Err(Error::Expired);
        }
        Ok(())
    }
}

/// Historical negative evidence only. Deliberately has no conversion into a
/// live Capture, fresh inspection, cleanup permission or ownership decision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupEvidence {
    #[serde(deserialize_with = "closed")]
    pub target: Target,
    pub machine: String,
    pub root: String,
    pub common_dir: String,
    /// Empty is explicitly unknown legacy ownership, never path-inferred.
    pub owner_id: String,
}
impl From<&termination_cleanup::Capture> for CleanupEvidence {
    fn from(c: &termination_cleanup::Capture) -> Self {
        Self {
            target: c.target.clone(),
            machine: c.machine.clone(),
            root: c.root.clone(),
            common_dir: c.common_dir.clone(),
            owner_id: c.owner_id.clone(),
        }
    }
}
impl CleanupEvidence {
    fn validate(&self, captured: &Captured) -> Result<(), Error> {
        let path = |s: &str| {
            bounded(s, 32 * 1024, true)
                && (paths::absolute(s, paths::Style::Posix)
                    || paths::absolute(s, paths::Style::Windows))
        };
        check(
            self.target == captured.target
                && matches!(
                    captured.action,
                    Action::Archive | Action::Terminate | Action::Kill
                )
                && bounded(&self.machine, 1024, false)
                && bounded(&self.owner_id, 1024, false)
                && path(&self.root)
                && path(&self.common_dir),
        )
    }
}

/// Required negative intent, NOT proof that a durable hold was installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupRestriction {
    KeepForReview,
}

/// Original native confirmation choice, never inferred from settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalChoice {
    Keep,
    RemoveAfterTermination,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    #[serde(rename = "i")]
    pub ordinal: u32,
    #[serde(rename = "r")]
    pub request_id: String,
    #[serde(with = "capture_wire")]
    #[serde(rename = "c")]
    pub captured: Captured,
    #[serde(rename = "w")]
    pub removal_choice: RemovalChoice,
    #[serde(deserialize_with = "present_option")]
    #[serde(rename = "e")]
    pub cleanup: Option<CleanupEvidence>,
    #[serde(rename = "n")]
    pub restriction: CleanupRestriction,
}
impl Descriptor {
    fn validate(&self, action: &Action) -> Result<(), Error> {
        check(
            creation::canonical_uuid(&self.request_id)
                && self.captured.valid()
                && &self.captured.action == action,
        )?;
        match (self.removal_choice, &self.cleanup) {
            (RemovalChoice::Keep, None) => Ok(()),
            (RemovalChoice::Keep | RemovalChoice::RemoveAfterTermination, Some(cleanup)) => {
                cleanup.validate(&self.captured)
            }
            _ => Err(Error::Invalid),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    #[serde(rename = "d")]
    pub descriptor: Descriptor,
    /// Planning digest; NOT the root session-action canonical request_hash.
    #[serde(rename = "h")]
    pub planning_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: String,
    /// Frozen original native deadline order, never a UUID/map sort.
    pub ordinal: u32,
    #[serde(deserialize_with = "closed")]
    pub action: Action,
    pub members: Vec<Member>,
}

struct HashSink(Sha256);
impl Write for HashSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.update(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn hash(value: &impl Serialize) -> Result<String, Error> {
    let mut writer = HashSink(Sha256::new());
    serde_json::to_writer(&mut writer, value).map_err(|_| Error::Invalid)?;
    Ok(format!("{:x}", writer.0.finalize()))
}
fn member_hash(context: &Context, group: &Group, descriptor: &Descriptor) -> Result<String, Error> {
    hash(&(
        VERSION,
        context,
        &group.id,
        group.ordinal,
        &group.action,
        descriptor,
    ))
}
impl Group {
    /// Build data from supplied original UUIDs. Generates no identity/nonce.
    pub fn new(
        context: &Context,
        id: String,
        ordinal: u32,
        action: Action,
        descriptors: Vec<Descriptor>,
    ) -> Result<Self, Error> {
        context.validate()?;
        check(creation::canonical_uuid(&id))?;
        if descriptors.is_empty() || descriptors.len() > MAX_MEMBERS {
            return Err(Error::TooMany);
        }
        let mut group = Self {
            id,
            ordinal,
            action,
            members: Vec::new(),
        };
        let mut count = Count {
            bytes: 0,
            limit: CAPTURE_BYTES,
        };
        for descriptor in descriptors {
            descriptor.validate(&group.action)?;
            serde_json::to_writer(&mut count, &descriptor).map_err(|_| Error::TooLarge)?;
            let planning_sha256 = member_hash(context, &group, &descriptor)?;
            group.members.push(Member {
                descriptor,
                planning_sha256,
            });
        }
        Ok(group)
    }
}

/// Immutable, validated DATA only. Cached hashes are not native admission.
/// No Deserialize implementation: bounded decode is the only JSON entry point.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Record {
    version: u32,
    context: Context,
    groups: Vec<Group>,
    #[serde(skip)]
    binding: Binding,
    #[serde(skip)]
    bytes: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord {
    version: u32,
    context: Context,
    groups: Vec<Group>,
}
struct Count {
    bytes: usize,
    limit: usize,
}
impl Write for Count {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(data.len())
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| io::Error::other("quit byte limit"))?;
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Record {
    pub fn new(context: Context, groups: Vec<Group>) -> Result<Self, Error> {
        Self::from_wire(WireRecord {
            version: VERSION,
            context,
            groups,
        })
    }
    fn from_wire(wire: WireRecord) -> Result<Self, Error> {
        let mut record = Self {
            version: wire.version,
            context: wire.context,
            groups: wire.groups,
            binding: Binding {
                quit_id: String::new(),
                planning_sha256: String::new(),
            },
            bytes: 0,
        };
        record.bytes = record.count_bytes()?;
        record.validate()?;
        record.binding = Binding {
            quit_id: record.context.quit_id.clone(),
            planning_sha256: hash(&record)?,
        };
        Ok(record)
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }
    fn validate(&self) -> Result<(), Error> {
        check(self.version == VERSION)?;
        self.context.validate()?;
        if self.groups.is_empty() || self.groups.len() > MAX_GROUPS {
            return Err(Error::TooMany);
        }
        let mut groups = BTreeSet::new();
        let mut requests = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for (ordinal, group) in self.groups.iter().enumerate() {
            check(
                group.ordinal as usize == ordinal
                    && creation::canonical_uuid(&group.id)
                    && groups.insert(&group.id),
            )?;
            if group.members.is_empty() || group.members.len() > MAX_MEMBERS {
                return Err(Error::TooMany);
            }
            for (index, member) in group.members.iter().enumerate() {
                let d = &member.descriptor;
                let t = &d.captured.target;
                check(
                    d.ordinal as usize == index
                        && creation::canonical_uuid(&d.request_id)
                        && d.captured.valid()
                        && d.captured.action == group.action
                        && digest(&member.planning_sha256)
                        && member_hash(&self.context, group, d)? == member.planning_sha256,
                )?;
                if !requests.insert(&d.request_id)
                    || !targets.insert((&t.computer, &t.name, &t.run_id, &t.conversation_id))
                {
                    return Err(Error::Conflict);
                }
                d.validate(&group.action)?;
            }
        }
        Ok(())
    }
    /// Full serialized descriptors, digests, context and optional evidence.
    /// No second large JSON allocation is needed to count an over-limit input.
    fn count_bytes(&self) -> Result<usize, Error> {
        let mut count = Count {
            bytes: 0,
            limit: CAPTURE_BYTES,
        };
        serde_json::to_writer(&mut count, self).map_err(|_| Error::TooLarge)?;
        Ok(count.bytes)
    }
    /// Historical data only. The canonical round trip also closes nested
    /// existing Target/Action types that otherwise ignore future fields.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > CAPTURE_BYTES {
            return Err(Error::TooLarge);
        }
        Self::from_wire(serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?)
    }
    pub fn capture_bytes(&self) -> usize {
        self.bytes
    }
    pub fn binding(&self) -> Binding {
        self.binding.clone()
    }
    fn charge(&self) -> Result<u64, Error> {
        self.rows()
            .checked_mul(ROW_CHARGE)
            .and_then(|n| n.checked_add(self.bytes as u64))
            .ok_or(Error::Capacity)
    }
    fn member(&self, group: u32, member: u32) -> Result<(&Group, &Member), Error> {
        let g = self.groups.get(group as usize).ok_or(Error::Invalid)?;
        Ok((g, g.members.get(member as usize).ok_or(Error::Invalid)?))
    }
    fn rows(&self) -> u64 {
        self.groups.iter().map(|g| g.members.len() as u64).sum()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub quit_id: String,
    pub planning_sha256: String,
}
impl Binding {
    fn valid(&self) -> bool {
        creation::canonical_uuid(&self.quit_id) && digest(&self.planning_sha256)
    }
}

#[cfg(test)]
mod tests;
