//! Closed cache intents. A prepared binding/hash is correlation data: only a
//! fresh native frontend permit plus the service's original frozen worker can
//! admit its exact bounded pass. No generic request can execute these values.
use super::digest;
use serde::{Deserialize, Serialize};
use zerus_desktop_migration::canonical_uuid;

pub const ABI: &str = "desktop-cache-maintenance-v1";
pub const HELP_MARKER: &str = "Desktop cache maintenance ABI: desktop-cache-maintenance-v1";
fn absolute_root(root: &str) -> bool {
    use crate::paths::{absolute, Style};
    absolute(root, Style::Posix)
        || absolute(root, Style::Windows)
        || root.strip_prefix("\\\\?\\").is_some_and(|path| {
            // std::fs::canonicalize preserves the native Windows verbatim
            // spelling. Accept disk/UNC roots, never arbitrary device names.
            if let Some(share) = path.strip_prefix("UNC\\") {
                absolute(&format!("\\\\{share}"), Style::Windows)
            } else {
                path.as_bytes().get(1) == Some(&b':') && absolute(path, Style::Windows)
            }
        })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Timing {
    Immediate,
    AfterOpeningGrace,
}
impl Timing {
    pub fn delay_ms(self) -> u64 {
        match self {
            Self::Immediate => 0,
            Self::AfterOpeningGrace => 65_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Prepare {
        operation_id: String,
        configuration: String,
        root: String,
        limit: u64,
        revision: u64,
        timing: Timing,
        policy_change: bool,
    },
    Execute {
        configuration: String,
        preparation_id: String,
        binding_sha256: String,
    },
}
impl Command {
    pub fn valid(&self, request_id: &str) -> bool {
        if !canonical_uuid(request_id) {
            return false;
        }
        match self {
            Self::Prepare {
                operation_id,
                configuration,
                root,
                revision,
                limit,
                ..
            } => {
                canonical_uuid(operation_id)
                    && operation_id != request_id
                    && digest(configuration)
                    && *revision < i64::MAX as u64
                    && *limit <= i64::MAX as u64
                    && root.len() <= 8192
                    && !root.chars().any(char::is_control)
                    && absolute_root(root)
            }
            Self::Execute {
                configuration,
                preparation_id,
                binding_sha256,
            } => {
                digest(configuration)
                    && canonical_uuid(preparation_id)
                    && preparation_id != request_id
                    && digest(binding_sha256)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Prepared,
    Reserved,
    Settled,
    Refused,
    Unknown,
}

/// Counts and exact original IDs only. Read-only queries cannot supply a path,
/// alter consent, recreate preparation or clear a terminal unknown fence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub abi: String,
    pub configuration: String,
    pub preparation_id: String,
    pub operation_id: String,
    pub revision: u64,
    pub limit: u64,
    pub policy_saved: bool,
    pub phase: Phase,
    pub binding_sha256: Option<String>,
    pub removed: usize,
    pub kept: usize,
    pub unknown: usize,
    pub drained: bool,
}
impl Status {
    /// A correlated response is data, never execution permission. Unknown body
    /// fields and incomplete receipts are refused rather than defaulted.
    pub fn response(
        value: &serde_json::Value,
        request_id: &str,
        command: &Command,
    ) -> Option<Self> {
        if value["request_id"] != request_id
            || value["op"] != "cache_maintenance"
            || value["durable"] != true
        {
            return None;
        }
        let mut body = value.as_object()?.clone();
        for field in ["request_id", "op", "accepted", "durable", "pending"] {
            body.remove(field);
        }
        let status: Self = serde_json::from_value(serde_json::Value::Object(body)).ok()?;
        let admitted = matches!(
            status.phase,
            Phase::Reserved | Phase::Settled | Phase::Unknown
        );
        let pending = matches!(
            status.phase,
            Phase::Preparing | Phase::Prepared | Phase::Reserved
        ) || !status.drained;
        if status.abi != ABI
            || value["accepted"] != admitted
            || value["pending"] != pending
            || status.revision > i64::MAX as u64
            || status.limit > i64::MAX as u64
            || status
                .removed
                .checked_add(status.kept)?
                .checked_add(status.unknown)?
                > 16
            || status.binding_sha256.as_deref().is_some_and(|v| !digest(v))
        {
            return None;
        }
        let matches = match command {
            Command::Prepare {
                configuration,
                operation_id,
                revision,
                policy_change,
                limit,
                ..
            } => {
                status.configuration == *configuration
                    && status.preparation_id == request_id
                    && status.operation_id == *operation_id
                    && status.policy_saved == *policy_change
                    && status.limit == *limit
                    && status.revision == revision.checked_add(u64::from(*policy_change))?
            }
            Command::Execute {
                configuration,
                preparation_id,
                binding_sha256,
            } => {
                status.configuration == *configuration
                    && status.preparation_id == *preparation_id
                    && status.operation_id == request_id
                    && status.binding_sha256.as_ref() == Some(binding_sha256)
            }
        };
        matches.then_some(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const PREPARE: &str = "11111111-1111-4111-8111-111111111111";
    const OPERATION: &str = "22222222-2222-4222-8222-222222222222";
    #[test]
    fn identities_phases_and_unknown_fields_stay_closed() {
        let mut command = json!({"phase":"prepare","operation_id":OPERATION,
            "configuration":"a".repeat(64),"root":"/srv/example/cache","limit":1,"revision":0,"timing":"immediate","policy_change":true});
        let parsed: Command = serde_json::from_value(command.clone()).unwrap();
        assert!(parsed.valid(PREPARE));
        assert!(!parsed.valid(OPERATION));
        command["confirmed"] = json!(true);
        assert!(serde_json::from_value::<Command>(command).is_err());
        let command = Command::Execute {
            configuration: "a".repeat(64),
            preparation_id: PREPARE.into(),
            binding_sha256: "b".repeat(64),
        };
        assert!(command.valid(OPERATION));
        assert!(!command.valid(PREPARE));
    }
    #[test]
    fn canonical_windows_disk_and_unc_roots_do_not_admit_device_names() {
        for root in [
            r"C:\cache",
            r"\\server\share\cache",
            r"\\?\C:\cache",
            r"\\?\UNC\server\share\cache",
        ] {
            assert!(absolute_root(root), "{root}");
        }
        for root in [
            r"C:cache",
            r"\\.\pipe\cache",
            r"\\?\GLOBALROOT\Device\cache",
            r"\\?\cache",
        ] {
            assert!(!absolute_root(root), "{root}");
        }
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn preference_data_and_effect_receipts_are_correlated_without_defaulted_authority() {
        let prepare = "11111111-1111-4111-8111-111111111111";
        let operation = "22222222-2222-4222-8222-222222222222";
        let command = Command::Prepare {
            operation_id: operation.into(),
            configuration: "a".repeat(64),
            root: "/srv/example/cache".into(),
            limit: 1,
            revision: 7,
            timing: Timing::Immediate,
            policy_change: true,
        };
        let value = json!({"abi":ABI,"request_id":prepare,"op":"cache_maintenance","durable":true,"accepted":false,
            "pending":true,"configuration":"a".repeat(64),"preparation_id":prepare,"operation_id":operation,
            "revision":8,"limit":1,"policy_saved":true,"phase":"prepared","binding_sha256":"b".repeat(64),
            "removed":0,"kept":0,"unknown":0,"drained":false});
        let receipt = Status::response(&value, prepare, &command).unwrap();
        assert!(receipt.policy_saved);
        assert_eq!(receipt.phase, Phase::Prepared);
        for (key, replacement) in [
            ("configuration", json!("c".repeat(64))),
            ("operation_id", json!(prepare)),
            ("revision", json!(7)),
            ("policy_saved", json!(false)),
            ("accepted", json!(true)),
            ("kept", json!(17)),
            ("role", json!("owner")),
        ] {
            let mut corrupt = value.clone();
            corrupt[key] = replacement;
            assert!(
                Status::response(&corrupt, prepare, &command).is_none(),
                "{key}"
            );
        }
    }
}
