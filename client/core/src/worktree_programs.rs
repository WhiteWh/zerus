//! Bounded native discovery data for explicit Terminate/Remove. None of these
//! serialized values is a stop permit. The native confirmation owns its original
//! selection and claim; the helper must freshly prove object membership again.
use crate::{
    creation::canonical_uuid, desktop::termination_outcome::hash, termination_cleanup::Capture,
};
use serde::{Deserialize, Serialize};

pub mod cleanup;
pub mod custody;
pub mod invocation;
pub mod presentation;
pub mod stop;

pub const ABI: &str = "worktree-programs-v1";
pub const HELP_MARKER: &str = "Worktree programs ABI: worktree-programs-v1";
pub const MAX_PROGRAMS: usize = 64;
pub const MAX_BYTES: usize = 64 << 10;
pub const MANIFEST_SECONDS: u64 = 300;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryId {
    pub device: u64,
    pub inode: u64,
    /// Native mount identity, never a folder-name prefix.
    pub mount: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub pid: u32,
    /// The platform's native birth identity. The manifest also binds the boot.
    pub birth: String,
    /// Display only; neither a command name nor ancestry authorizes a signal.
    pub label: String,
}
impl Program {
    fn valid(&self) -> bool {
        self.pid > 1 && bounded(&self.birth, 128) && bounded(&self.label, 512)
    }
    pub fn same_process(&self, other: &Self) -> bool {
        self.pid == other.pid && self.birth == other.birth
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub abi: String,
    pub id: String,
    pub capture: Capture,
    pub configuration: String,
    pub boot: String,
    pub checkout: DirectoryId,
    pub issued: u64,
    pub expires: u64,
    /// The COMPLETE bounded list, not the review's five display examples.
    pub programs: Vec<Program>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discovery {
    pub capture: Capture,
}
impl Discovery {
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_BYTES {
            return Err("program_discovery_oversized");
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| "program_discovery_invalid")?;
        capture_valid(&value.capture)
            .then_some(value)
            .ok_or("program_discovery_invalid")
    }
}
fn bounded(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn absolute(value: &str) -> bool {
    bounded(value, 32 << 10)
        && (crate::paths::absolute(value, crate::paths::Style::Posix)
            || crate::paths::absolute(value, crate::paths::Style::Windows))
}
fn capture_valid(capture: &Capture) -> bool {
    let target = &capture.target;
    canonical_uuid(&capture.machine)
        && canonical_uuid(&capture.owner_id)
        && bounded(&target.name, 512)
        && (target.computer.is_empty() || bounded(&target.computer, 1024))
        && bounded(&target.run_id, 128)
        && bounded(&target.conversation_id, 256)
        && absolute(&capture.root)
        && absolute(&capture.common_dir)
}
impl Manifest {
    /// Shape/freshness only. Callers still need native origin, exact successful
    /// termination, current ownership, process pins and fresh membership.
    pub fn valid(&self, now: u64) -> bool {
        self.abi == ABI
            && canonical_uuid(&self.id)
            && canonical_uuid(&self.boot)
            && capture_valid(&self.capture)
            && self.configuration.len() == 64
            && self
                .configuration
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && self.checkout.inode != 0
            && bounded(&self.checkout.mount, 4096)
            && self.issued <= now
            && now < self.expires
            && self.expires.checked_sub(self.issued) == Some(MANIFEST_SECONDS)
            && self.programs.len() <= MAX_PROGRAMS
            && self.programs.iter().all(Program::valid)
            && self
                .programs
                .windows(2)
                .all(|pair| pair[0].pid < pair[1].pid)
            && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= MAX_BYTES)
    }
    pub fn decode(bytes: &[u8], now: u64) -> Result<Self, &'static str> {
        if bytes.len() > MAX_BYTES {
            return Err("program_manifest_oversized");
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| "program_manifest_invalid")?;
        value
            .valid(now)
            .then_some(value)
            .ok_or("program_manifest_invalid")
    }
    /// Integrity/correlation data; equality never supplies execution consent.
    pub fn binding(&self) -> String {
        hash(&serde_json::to_vec(self).expect("closed native manifest fields"))
    }
    /// Disappeared originals are allowed. Any newly discovered/replaced PID
    /// needs a new review; an old empty list grants no newly found program.
    pub fn contains_originals(&self, current: &[Program]) -> bool {
        current.len() <= MAX_PROGRAMS
            && current.iter().all(Program::valid)
            && current.windows(2).all(|pair| pair[0].pid < pair[1].pid)
            && current
                .iter()
                .all(|p| self.programs.iter().any(|old| old.same_process(p)))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    const UUID: &str = "11111111-1111-4111-8111-111111111111";
    pub(crate) fn manifest() -> Manifest {
        Manifest {
            abi: ABI.into(),
            id: UUID.into(),
            capture: Capture {
                target: crate::Target {
                    computer: String::new(),
                    name: "codex/example/original".into(),
                    run_id: UUID.into(),
                    conversation_id: UUID.into(),
                },
                machine: UUID.into(),
                root: "/srv/example/work".into(),
                common_dir: "/srv/example/main/.git".into(),
                owner_id: UUID.into(),
            },
            configuration: "a".repeat(64),
            boot: UUID.into(),
            checkout: DirectoryId {
                device: 7,
                inode: 9,
                mount: "native-mount-1".into(),
            },
            issued: 1000,
            expires: 1300,
            programs: vec![Program {
                pid: 7,
                birth: "original-birth".into(),
                label: "editor".into(),
            }],
        }
    }
    #[test]
    fn complete_originals_are_bounded_and_new_or_recycled_processes_never_fit() {
        let mut m = manifest();
        assert!(m.valid(1000));
        assert!(m.valid(1299));
        assert!(!m.valid(1300));
        assert!(!m.valid(999));
        assert!(m.contains_originals(&m.programs));
        let duplicate = vec![m.programs[0].clone(), m.programs[0].clone()];
        assert!(!m.contains_originals(&duplicate));
        assert!(m.contains_originals(&[]));
        let mut replacement = m.programs[0].clone();
        replacement.birth = "replacement".into();
        assert!(!m.contains_originals(&[replacement]));
        let mut new = m.programs[0].clone();
        new.pid += 1;
        assert!(!m.contains_originals(&[new]));
        m.programs.clear();
        assert!(m.valid(1000));
        assert!(!m.contains_originals(&manifest().programs));
    }
    #[test]
    fn duplicates_partial_large_and_unbound_manifests_are_refused_without_truncation() {
        let original = manifest();
        let bytes = serde_json::to_vec(&original).unwrap();
        assert_eq!(Manifest::decode(&bytes, 1000).unwrap(), original);
        let duplicate = String::from_utf8(bytes.clone()).unwrap().replacen(
            "{",
            "{\"abi\":\"worktree-programs-v1\",",
            1,
        );
        assert!(Manifest::decode(duplicate.as_bytes(), 1000).is_err());
        let mut raw = serde_json::to_value(&original).unwrap();
        raw["complete"] = true.into();
        assert!(Manifest::decode(&serde_json::to_vec(&raw).unwrap(), 1000).is_err());
        for key in ["boot", "configuration"] {
            let mut raw = serde_json::to_value(&original).unwrap();
            raw[key] = "".into();
            assert!(Manifest::decode(&serde_json::to_vec(&raw).unwrap(), 1000).is_err());
        }
        let mut m = original.clone();
        m.programs.push(m.programs[0].clone());
        assert!(!m.valid(1000));
        let mut m = original.clone();
        m.programs = (2..=(MAX_PROGRAMS as u32 + 2))
            .map(|pid| Program {
                pid,
                ..original.programs[0].clone()
            })
            .collect();
        assert!(!m.valid(1000));
        let mut m = original;
        m.capture.owner_id.clear();
        assert!(!m.valid(1000));
        assert!(Manifest::decode(&vec![b' '; MAX_BYTES + 1], 1000).is_err());
    }
    #[test]
    fn discovery_is_closed_bounded_and_requires_the_original_launch_owner() {
        let original = Discovery {
            capture: manifest().capture,
        };
        let bytes = serde_json::to_vec(&original).unwrap();
        assert_eq!(Discovery::decode(&bytes).unwrap(), original);
        let mut changed = serde_json::to_value(&original).unwrap();
        changed["stop"] = true.into();
        assert!(Discovery::decode(&serde_json::to_vec(&changed).unwrap()).is_err());
        changed.as_object_mut().unwrap().remove("stop");
        changed["capture"]["target"]["permit"] = true.into();
        assert!(Discovery::decode(&serde_json::to_vec(&changed).unwrap()).is_err());
        let mut changed = original;
        changed.capture.owner_id.clear();
        assert!(Discovery::decode(&serde_json::to_vec(&changed).unwrap()).is_err());
        assert!(Discovery::decode(&vec![b' '; MAX_BYTES + 1]).is_err());
    }
    #[test]
    fn object_owner_and_display_are_integrity_bound_but_labels_are_not_process_identity() {
        let m = manifest();
        let digest = m.binding();
        let mut changed = m.clone();
        changed.checkout.inode += 1;
        assert_ne!(digest, changed.binding());
        let mut changed = m.clone();
        changed.capture.owner_id = "22222222-2222-4222-8222-222222222222".into();
        assert_ne!(digest, changed.binding());
        let mut changed = m.clone();
        changed.programs[0].label = "different display".into();
        assert_ne!(digest, changed.binding());
        assert!(m.contains_originals(&changed.programs));
    }

    #[test]
    fn pid_one_or_zero_manifest_members_are_refused_for_terminate_remove() {
        let mut m = manifest();
        m.programs.insert(
            0,
            Program {
                pid: 1,
                birth: "init-birth".into(),
                label: "init".into(),
            },
        );
        assert!(!m.valid(1000));
        m.programs[0].pid = 0;
        assert!(!m.valid(1000));
    }
}
