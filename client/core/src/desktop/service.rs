//! Read-only frontend state for desktop-v1. IPC and display data grant no writer authority.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

mod catalog;
pub use catalog::{
    CatalogIdentity, CatalogItem, CatalogPage, ChangeItem, ChangePage, Revision, CHANGES_ABI,
    CHANGES_HELP, RELAY_ABI, RELAY_HELP,
};
pub mod details;

pub const MODE_ENV: &str = "ZERUS_CLIENT_DESKTOP_SERVICE";
pub const ABI: &str = "desktop-v1";
pub const FRAME_BYTES: usize = zerus_watch_protocol::MAX_FRAME_BYTES + 4096;
pub const CATALOG_ROWS: usize = 128;
pub const COMPONENTS: [&str; 5] = ["fleet", "presence", "reads", "attention", "approvals"];

/// The registration observation selects a source; it never grants writer authority.
pub fn enabled(value: Option<&str>, owned_registration: bool) -> bool {
    match value {
        None => owned_registration && cfg!(any(target_os = "linux", target_os = "macos")),
        Some("1") => true,
        _ => false,
    }
}

/// Transport success alone is not an actor commit. Read receipts and the
/// nested result of a read-only lookup use the original durable identity.
pub fn read_committed(value: &Value, request_id: &str) -> bool {
    value["request_id"] == request_id
        && value["op"] == "read_intent"
        && value["accepted"] == true
        && value["durable"] == true
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    pub ready: bool,
    pub code: String,
}

/// A closed, fixed-size display reason. It never authorizes a transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutableUpgrade {
    Draining,
    ManualRefreshRequired,
}
impl ExecutableUpgrade {
    fn parse(value: &Value) -> Option<Self> {
        if value["code"] != "hgs_replaced" || value["new_admission"] != false {
            return None;
        }
        match value["state"].as_str()? {
            "draining" => Some(Self::Draining),
            "manual_refresh_required" => Some(Self::ManualRefreshRequired),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Bounded presentation from this configuration/epoch's checked status.
    #[serde(default)]
    pub executable_upgrade: Option<ExecutableUpgrade>,
    /// Native original-intent presentation only, never accepted from Root input.
    #[serde(skip_deserializing)]
    pub notification_preferences: Option<Value>,
    /// Native ownership controller presentation only, never a writer permit.
    #[serde(skip_deserializing)]
    pub notification_ownership: Option<Value>,
    /// Native-client transfer state, attached locally; never a service writer grant.
    pub pending_read_import: Option<Value>,
    pub connected: bool,
    #[serde(default)]
    pub reconnecting: bool,
    pub epoch: Option<String>,
    pub generation: Option<u64>,
    pub background: BTreeMap<String, Component>,
    /// Bounded connection presentation, including configured ungreeted peers.
    /// This contains no source generations or writer authority.
    #[serde(default)]
    pub fleet_computers: Option<Vec<FleetComputer>>,
    /// The restart delays the service's supervisors use; None from an older
    /// service.
    #[serde(default)]
    pub fleet_retry_delays: Option<FleetRetryDelays>,
    /// Shared host presentation only. Null and incomplete counts stay unknown.
    pub attention_counts: Option<Value>,
    pub automatic_approvals: Option<bool>,
    /// The person's global switch of the owner-approved question route
    /// (zerus-sa9qn.32); null where the service does not route.
    #[serde(default)]
    pub question_routes: Option<bool>,
    /// The approval categories the service's committed policy keeps on, by
    /// ID (zerus-sz42x); None from an older service or without a policy.
    #[serde(default)]
    pub automatic_approval_categories: Option<Vec<String>>,
    /// Held task message copies a person may review. Display only: it never
    /// authorizes a release, which names a fresh review's observation.
    #[serde(default)]
    pub task_message_inputs: Option<HeldInputs>,
    pub details_available: bool,
    pub code: String,
    /// Task 176: hgs refused this computer's desktop state folder as not
    /// private (its words name the folder, the access entry and the repair),
    /// and the repair the client ran did not fix it. Native presentation only.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub state_refusal: Option<String>,
}
/// Display only: an unconnected configured alias is not a catalog source.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FleetComputer {
    pub id: String,
    pub status: String,
    pub attempt: u32,
    /// The source a reconnect request names; None from an older service or
    /// before the computer has a supervisor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incarnation: Option<u64>,
    /// The supervisor's restart schedule; None from an older service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<FleetRetry>,
}

/// A service supervisor's restart schedule, as [`crate::RetryOut`] without
/// its computer: `remaining_ms` was measured from the supervisor's monotonic
/// deadline when the service built the frame.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FleetRetry {
    pub phase: String,
    #[serde(default)]
    pub serial: u64,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_ms: Option<u64>,
}

/// The restart delays a service uses, with their source: `default`, `file`,
/// or `kept` when its saved timings file could not be used (`diagnostic`).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FleetRetryDelays {
    pub delays_seconds: [u64; 4],
    pub source: String,
    #[serde(default)]
    pub diagnostic: Option<String>,
}

/// The longest restart delay a frame may report, in milliseconds.
const RETRY_MS: u64 = crate::stream::RESTART_MAX_SECS * 1000;

fn checked_retry(value: &Value) -> Option<FleetRetry> {
    let phase = value["phase"].as_str()?;
    let serial = value["serial"].as_u64().unwrap_or(0);
    let attempt = u32::try_from(value["attempt"].as_u64().unwrap_or(0)).ok()?;
    match phase {
        "idle" => Some(FleetRetry {
            phase: phase.into(),
            serial: 0,
            attempt: 0,
            delay_ms: None,
            remaining_ms: None,
        }),
        "attempting" => Some(FleetRetry {
            phase: phase.into(),
            serial,
            attempt,
            delay_ms: None,
            remaining_ms: None,
        }),
        "waiting" => {
            let delay = value["delay_ms"].as_u64().filter(|ms| *ms <= RETRY_MS)?;
            let remaining = value["remaining_ms"].as_u64().filter(|ms| *ms <= delay)?;
            Some(FleetRetry {
                phase: phase.into(),
                serial,
                attempt,
                delay_ms: Some(delay),
                remaining_ms: Some(remaining),
            })
        }
        _ => None,
    }
}

fn checked_retry_delays(value: &Value) -> Option<FleetRetryDelays> {
    let seconds: Vec<u64> = value["delays_seconds"]
        .as_array()?
        .iter()
        .map(Value::as_u64)
        .collect::<Option<_>>()?;
    crate::stream::checked_restart_delays(&seconds).ok()?;
    let source = value["source"]
        .as_str()
        .filter(|source| matches!(*source, "default" | "file" | "kept"))?;
    let diagnostic = match &value["diagnostic"] {
        Value::Null => None,
        // Bounded presentation of the service's own report about the file.
        Value::String(text) => Some(text.chars().take(512).collect()),
        _ => return None,
    };
    Some(FleetRetryDelays {
        delays_seconds: <[u64; 4]>::try_from(seconds).ok()?,
        source: source.into(),
        diagnostic,
    })
}

/// The service's held task message copies awaiting review.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HeldInputs {
    /// Orphaned or failed copies that stay charged until reviewed.
    pub review: u64,
    /// No further copy fits: large task messages are paused.
    pub full: bool,
}
impl HeldInputs {
    fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            review: value["review"].as_u64()?,
            full: value["full"].as_bool()?,
        })
    }
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            executable_upgrade: None,
            notification_preferences: None,
            notification_ownership: None,
            pending_read_import: None,
            connected: false,
            reconnecting: false,
            epoch: None,
            generation: None,
            background: BTreeMap::new(),
            fleet_computers: None,
            fleet_retry_delays: None,
            attention_counts: None,
            automatic_approvals: None,
            question_routes: None,
            automatic_approval_categories: None,
            task_message_inputs: None,
            details_available: false,
            code: "unavailable".into(),
            state_refusal: None,
        }
    }
}
impl Snapshot {
    pub fn component_ready(&self, name: &str) -> bool {
        self.connected
            && self
                .background
                .get(name)
                .is_some_and(|component| component.ready)
    }
    pub fn background_ready(&self) -> bool {
        COMPONENTS.iter().all(|name| self.component_ready(name))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    Status,
    Catalog { computer: String, removed: bool },
    Other,
}
/// Source identities from the service runtime, independent of display counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogSource {
    pub computer: String,
    pub incarnation: u64,
    pub generation: u64,
    pub online: bool,
}

/// One stream and one current projection. A reconnect must greet again.
pub struct State {
    configuration: String,
    snapshot: Snapshot,
    seq: Option<u64>,
    sources: Option<Vec<CatalogSource>>,
}
impl State {
    pub fn new(configuration: String) -> Result<Self, &'static str> {
        if !super::digest(&configuration) {
            return Err("invalid_configuration");
        }
        Ok(Self {
            configuration,
            snapshot: Snapshot::default(),
            seq: None,
            sources: None,
        })
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    pub fn sources(&self) -> Option<&[CatalogSource]> {
        self.sources.as_deref()
    }
    pub fn disconnect(&mut self, code: &str) {
        self.snapshot = Snapshot {
            notification_preferences: None,
            code: code.into(),
            ..Snapshot::default()
        };
        self.seq = None;
        self.sources = None;
    }
    pub fn status(&mut self, value: &Value) -> Result<(), &'static str> {
        let result = self.status_inner(value);
        if let Err(code) = result {
            self.disconnect(code);
        }
        result
    }
    fn status_inner(&mut self, value: &Value) -> Result<(), &'static str> {
        let lifecycle = &value["lifecycle"];
        let epoch = value["epoch"]
            .as_str()
            .filter(|epoch| zerus_desktop_migration::canonical_uuid(epoch))
            .ok_or("service_unavailable")?;
        let generation = value["generation"].as_u64().ok_or("invalid_lifecycle")?;
        if value["abi"] != ABI
            || value["configuration"] != self.configuration
            || lifecycle["abi"] != "desktop-lifecycle-v1"
            || lifecycle["configuration"] != self.configuration
            || lifecycle["epoch"] != epoch
            || lifecycle["generation"] != generation
            || lifecycle["ipc_ready"] != true
            || lifecycle["phase"] != "serving"
        {
            return Err("incompatible_service");
        }
        if self
            .snapshot
            .epoch
            .as_deref()
            .is_some_and(|old| old != epoch)
        {
            return Err("service_epoch_changed");
        }
        if self.snapshot.generation.is_some_and(|old| generation < old) {
            return Err("stale_service_generation");
        }
        let mut background = BTreeMap::new();
        // Cache maintenance is independently configured; it must not make the
        // existing five-component background readiness depend on cache policy.
        for name in COMPONENTS.into_iter().chain(["cache_maintenance"]) {
            let component = &lifecycle["background"][name];
            let code = component["code"]
                .as_str()
                .filter(|code| code.len() <= 128 && !code.chars().any(char::is_control))
                .unwrap_or("unknown");
            background.insert(
                name.into(),
                Component {
                    ready: component["ready"] == true,
                    code: code.into(),
                },
            );
        }
        // A display projection failure is not a lifecycle/transport failure.
        // Quarantine it as unknown; it never supplies source discovery.
        let counts = checked_counts(&value["attention_counts"], epoch).unwrap_or_default();
        self.sources = checked_sources(&value["background_runtime"], epoch);
        self.snapshot = Snapshot {
            executable_upgrade: ExecutableUpgrade::parse(&value["executable_upgrade"]),
            notification_ownership: None,
            notification_preferences: None,
            connected: true,
            reconnecting: false,
            pending_read_import: None,
            epoch: Some(epoch.into()),
            generation: Some(generation),
            background,
            fleet_computers: checked_connections(&value["background_runtime"], epoch),
            fleet_retry_delays: checked_retry_delays(&value["background_runtime"]["retry_delays"]),
            attention_counts: counts,
            automatic_approvals: value["automatic_approvals"].as_bool(),
            question_routes: value["question_routes"].as_bool(),
            automatic_approval_categories: value["automatic_approval_categories"]
                .as_array()
                .and_then(|ids| {
                    let ids: Option<Vec<String>> = ids
                        .iter()
                        .map(|id| id.as_str().map(str::to_owned))
                        .collect();
                    ids.filter(|ids| crate::approvals::automatic::Set::from_ids(ids).is_some())
                }),
            task_message_inputs: HeldInputs::parse(
                &value["operation_queue"]["kinds"]["task_message"]["inputs"],
            ),
            details_available: self.snapshot.details_available
                && value["detail_subscriptions"]["abi"] == details::ABI
                && value["detail_subscriptions"]["stopping"] != true,
            code: String::new(),
            state_refusal: None,
        };
        self.snapshot.code = if value["background_start"]["admitted"] == false
            && value["background_start"]["code"] == "explicit_handoff_required"
        {
            "explicit_handoff_required"
        } else if !self.snapshot.background_ready() {
            "background_unready"
        } else if self.sources.is_none() {
            "catalog_sources_unavailable"
        } else {
            ""
        }
        .into();
        Ok(())
    }
    pub fn frame(&mut self, value: &Value) -> Result<Update, &'static str> {
        let result = self.frame_inner(value);
        if let Err(code) = result {
            self.disconnect(code);
        }
        result
    }
    fn frame_inner(&mut self, value: &Value) -> Result<Update, &'static str> {
        if value["abi"] != ABI {
            return Err("incompatible_service");
        }
        let seq = value["seq"].as_u64().ok_or("invalid_service_frame")?;
        let kind = value["t"].as_str().ok_or("invalid_service_frame")?;
        if kind == "hello" {
            if self.snapshot.connected {
                return Err("unexpected_service_hello");
            }
            self.status(&value["value"])?;
            self.snapshot.details_available = details::Lease::parse(
                &value["value"]["details"],
                self.snapshot.epoch.as_deref().unwrap(),
            )
            .is_some();
            if value["epoch"].as_str() != self.snapshot.epoch.as_deref() {
                return Err("service_epoch_changed");
            }
        } else {
            if !self.snapshot.connected || value["epoch"].as_str() != self.snapshot.epoch.as_deref()
            {
                return Err("service_epoch_changed");
            }
            // Targeted detail frames can skip the service-wide sequence.
            // Their own detail_seq, not this outer sequence, detects detail gaps.
            if self.seq.is_some_and(|old| seq < old) {
                return Err("stale_service_frame");
            }
            if matches!(kind, "status" | "heartbeat") {
                self.status(&value["value"])?;
            }
        }
        self.seq = Some(seq);
        match kind {
            "hello" | "status" | "heartbeat" => Ok(Update::Status),
            "catalog_changed" => {
                let body = &value["value"];
                let computer = body["computer"]
                    .as_str()
                    .filter(|name| valid_computer(name))
                    .ok_or("invalid_catalog_change")?;
                self.snapshot.attention_counts = checked_counts(
                    &body["attention_counts"],
                    self.snapshot.epoch.as_deref().unwrap(),
                )
                .unwrap_or_default();
                Ok(Update::Catalog {
                    computer: computer.into(),
                    removed: body["removed"] == true,
                })
            }
            "error" | "gap" => Err("service_resnapshot_required"),
            // An early connection presentation between heartbeats. Display
            // only: one that does not check is ignored, never a disconnect.
            "fleet_connections" => {
                let epoch = self.snapshot.epoch.as_deref().unwrap_or_default();
                let Some(rows) = checked_connections(&value["value"], epoch) else {
                    return Ok(Update::Other);
                };
                self.snapshot.fleet_computers = Some(rows);
                if let Some(delays) = checked_retry_delays(&value["value"]["retry_delays"]) {
                    self.snapshot.fleet_retry_delays = Some(delays);
                }
                Ok(Update::Status)
            }
            _ => Ok(Update::Other),
        }
    }
}

fn checked_connections(value: &Value, epoch: &str) -> Option<Vec<FleetComputer>> {
    if value["epoch"] != epoch
        || value["display_only"] != true
        || value["authority"] != false
        || value["state"] != "running"
    {
        return None;
    }
    let rows = value["connections"]
        .as_array()
        .filter(|rows| rows.len() <= 64)?;
    let mut seen = std::collections::BTreeSet::new();
    rows.iter()
        .map(|row| {
            let id = row["id"].as_str().filter(|id| valid_computer(id))?;
            if !seen.insert(id) {
                return None;
            }
            let status = row["status"].as_str().filter(|status| {
                matches!(
                    *status,
                    "live" | "polled" | "probing" | "unsupported" | "reconnecting" | "ended"
                )
            })?;
            Some(FleetComputer {
                id: id.into(),
                status: status.into(),
                attempt: u32::try_from(row["attempt"].as_u64()?).ok()?,
                incarnation: row["incarnation"]
                    .as_u64()
                    .filter(|n| *n > 0 && *n <= i64::MAX as u64),
                // A schedule that does not check is unknown, not a bad row.
                retry: checked_retry(&row["retry"]),
            })
        })
        .collect()
}

fn checked_sources(value: &Value, epoch: &str) -> Option<Vec<CatalogSource>> {
    if value["epoch"] != epoch || value["display_only"] != true || value["authority"] != false {
        return None;
    }
    let rows = value["computers"]
        .as_array()
        .filter(|rows| rows.len() <= 64)?;
    let mut seen = std::collections::BTreeSet::new();
    let mut sources = Vec::with_capacity(rows.len());
    for row in rows {
        let computer = row["computer"]
            .as_str()
            .filter(|name| valid_computer(name))?;
        if !seen.insert(computer) {
            return None;
        }
        let incarnation = row["incarnation"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= i64::MAX as u64)?;
        let generation = row["generation"]
            .as_u64()
            .filter(|n| *n <= i64::MAX as u64)?;
        let accepting = row["accepting"].as_bool()?;
        let status = row["status"].as_str()?;
        if !matches!(
            status,
            "live" | "polled" | "probing" | "unsupported" | "reconnecting" | "ended"
        ) {
            return None;
        }
        sources.push(CatalogSource {
            computer: computer.into(),
            incarnation,
            generation,
            online: accepting && matches!(status, "live" | "polled"),
        });
    }
    Some(sources)
}

pub fn valid_computer(name: &str) -> bool {
    name.len() <= 255
        && !name.starts_with(['-', '@'])
        && !name.chars().any(|ch| ch.is_control() || ch.is_whitespace())
}

fn checked_counts(value: &Value, epoch: &str) -> Result<Option<Value>, &'static str> {
    if value.is_null() {
        return Ok(None);
    }
    if value["epoch"] != epoch
        || value["scope"] != "shared_host_state"
        || value["display_only"] != true
        || value["authority"] != false
        || value["local_marks_included"] != false
        || !value["computers"]
            .as_array()
            .is_some_and(|rows| rows.len() <= 64)
        || !value["aggregate"]["complete"].is_boolean()
        || ["total", "attention"]
            .iter()
            .any(|key| !optional_count(&value["aggregate"][key]))
        || ["known_total", "known_attention"]
            .iter()
            .any(|key| value["aggregate"][key].as_u64().is_none())
    {
        return Err("invalid_attention_projection");
    }
    for row in value["computers"].as_array().unwrap() {
        if !row["computer"].as_str().is_some_and(valid_computer)
            || !row["online"].is_boolean()
            || !row["complete"].is_boolean()
            || ["total", "attention"]
                .iter()
                .any(|key| !optional_count(&row[key]))
        {
            return Err("invalid_attention_projection");
        }
    }
    Ok(Some(value.clone()))
}

fn optional_count(value: &Value) -> bool {
    value.is_null() || value.as_u64().is_some()
}

#[cfg(test)]
mod tests;
