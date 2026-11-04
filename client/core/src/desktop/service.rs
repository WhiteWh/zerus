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

pub fn enabled(value: Option<&str>) -> bool {
    value == Some("1")
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Native original-intent presentation only, never accepted from Root input.
    #[serde(skip_deserializing)]
    pub notification_preferences: Option<Value>,
    /// Native ownership controller presentation only, never a writer permit.
    #[serde(skip_deserializing)]
    pub notification_ownership: Option<Value>,
    /// Native-client transfer state, attached locally; never a service writer grant.
    pub pending_read_import: Option<Value>,
    /// Native managed-source report progress only. Never deserialized authority.
    #[serde(skip_deserializing)]
    pub retained_source_import: Option<Value>,
    pub connected: bool,
    pub epoch: Option<String>,
    pub generation: Option<u64>,
    pub background: BTreeMap<String, Component>,
    /// Shared host presentation only. Null and incomplete counts stay unknown.
    pub attention_counts: Option<Value>,
    pub automatic_approvals: Option<bool>,
    pub details_available: bool,
    pub code: String,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            notification_preferences: None,
            notification_ownership: None,
            pending_read_import: None,
            retained_source_import: None,
            connected: false,
            epoch: None,
            generation: None,
            background: BTreeMap::new(),
            attention_counts: None,
            automatic_approvals: None,
            details_available: false,
            code: "unavailable".into(),
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
            notification_ownership: None,
            notification_preferences: None,
            connected: true,
            pending_read_import: None,
            retained_source_import: None,
            epoch: Some(epoch.into()),
            generation: Some(generation),
            background,
            attention_counts: counts,
            automatic_approvals: value["automatic_approvals"].as_bool(),
            details_available: self.snapshot.details_available
                && value["detail_subscriptions"]["abi"] == details::ABI
                && value["detail_subscriptions"]["stopping"] != true,
            code: String::new(),
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
            _ => Ok(Update::Other),
        }
    }
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
