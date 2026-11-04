//! One computer's catalog and followed Activity, kept from its watch frames.
//!
//! [`CatalogState::apply`] turns each frame into keyed [`Change`]s: catalog
//! snapshot pages are staged until the page with `last` and then replace the
//! rows at once, so a `gap` never empties the list; a delta gives one upsert or
//! removal per item; Activity frames of followed sessions become
//! [`Change::Activity`], processes frames [`Change::Processes`], tasks frames
//! [`Change::Tasks`], a changed `catalog.meta` [`Change::Machine`] and the
//! organization's frames [`Change::Organization`]. Frame types and topics it
//! does not know change nothing.
use crate::{
    view, ActivityUpdate, Change, ComputerId, ComputerStatus, ProcessesState, TasksState, ViewRow,
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};
use zerus_watch_protocol::{reason, topic, Body, Frame, Hello};

/// Failures further apart than this start the restart delays over.
pub const FAILURE_WINDOW: Duration = Duration::from_secs(600);
/// Provider messages a merged summary keeps, newest by `at`.
pub const PROVIDER_MESSAGES: usize = 100;
/// Two provider messages of one type and text this close are one repeat.
pub const REPEAT_SECONDS: f64 = 3.0;
/// Event revisions remembered per followed session.
const EVENT_REVISIONS: usize = 1024;

/// What an inspection holds besides the summary, as the server leaves out of
/// Activity `state` frames.
const NOT_STATE: [&str; 6] = [
    "events",
    "processes",
    "process_work",
    "cursor",
    "history_truncated",
    "replace_events",
];

/// The removal reasons a [`Change::Remove`] keeps; others become None.
const REASONS: [&str; 4] = [
    reason::REPLACED,
    reason::ARCHIVED,
    reason::FORGOTTEN,
    reason::ENDED,
];

/// A followed session: its event window position and latest summary.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActivityState {
    /// The journal `seq` to resume after.
    pub cursor: i64,
    /// The latest `state` value with `event` frames merged into its
    /// `provider_messages`.
    pub state: Option<Value>,
    /// The `rev` of the latest `state` frame.
    pub rev: u64,
    staged: Option<Vec<Value>>,
    revisions: HashMap<String, u64>,
}

impl ActivityState {
    /// A `state` frame: it replaces the summary, provider messages included.
    pub fn replace_state(&mut self, value: Value, rev: u64) {
        self.state = Some(value);
        self.rev = rev;
    }

    /// Merges an activity `event` frame (a provider message, `AgentMessage` or
    /// `AgentThinking`) into the summary's `provider_messages`; true when the
    /// list changed. A frame whose `rev` is not higher than the last one of
    /// its `id` is stale. The message replaces the one with the same non-empty
    /// `message_id` and type; otherwise one of the same type and `detail` with
    /// `at` within 3 s makes it a repeat; otherwise it is inserted in `at`
    /// order and the newest 100 stay. Without a summary yet nothing merges:
    /// the first `state` carries the message.
    pub fn merge_event(&mut self, id: &str, rev: u64, event: Value) -> bool {
        let Some(Value::Object(state)) = self.state.as_mut() else {
            return false;
        };
        if self.revisions.get(id).is_some_and(|seen| *seen >= rev) {
            return false;
        }
        if self.revisions.len() >= EVENT_REVISIONS {
            self.revisions.clear();
        }
        self.revisions.insert(id.to_owned(), rev);
        merge_provider_message(state, event)
    }
}

fn field<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or("")
}

fn at(value: &Value) -> f64 {
    value.get("at").and_then(Value::as_f64).unwrap_or(f64::MAX)
}

fn merge_provider_message(state: &mut Map<String, Value>, message: Value) -> bool {
    let entry = state
        .entry("provider_messages")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !entry.is_array() {
        *entry = Value::Array(Vec::new());
    }
    let Value::Array(list) = entry else {
        return false;
    };
    let id = field(&message, "message_id");
    let kind = field(&message, "type");
    if !id.is_empty() {
        let same = list
            .iter()
            .position(|old| field(old, "message_id") == id && field(old, "type") == kind);
        if let Some(index) = same {
            list[index] = message;
            return true;
        }
    }
    let detail = message.get("detail");
    let when = at(&message);
    let repeat = list.iter().any(|old| {
        field(old, "type") == kind
            && old.get("detail") == detail
            && (at(old) - when).abs() <= REPEAT_SECONDS
    });
    if repeat {
        return false;
    }
    let index = list
        .iter()
        .position(|old| at(old) > when)
        .unwrap_or(list.len());
    list.insert(index, message);
    while list.len() > PROVIDER_MESSAGES {
        let oldest = (0..list.len())
            .min_by(|a, b| at(&list[*a]).total_cmp(&at(&list[*b])))
            .unwrap_or(0);
        list.remove(oldest);
    }
    true
}

/// The state of one computer's stream, kept across its restarts.
#[derive(Debug, Default)]
pub struct CatalogState {
    hello: Option<Hello>,
    rows: BTreeMap<String, ViewRow>,
    staged: Option<BTreeMap<String, ViewRow>>,
    meta: Option<Value>,
    /// The card data last sent for this computer.
    machine: Option<crate::MachineMeta>,
    activity: HashMap<String, ActivityState>,
    processes: ProcessesState,
    tasks: TasksState,
    organization: crate::OrganizationState,
    live: bool,
    failed: bool,
    failures: u32,
    last_failure: Option<Instant>,
}

impl CatalogState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The changes one frame makes. A fatal `error` ends the stream:
    /// [`Self::failed`] is then true and the caller restarts it.
    pub fn apply(&mut self, computer: &ComputerId, frame: &Frame) -> Vec<Change> {
        let mut changes = Vec::new();
        match &frame.body {
            Body::Hello(hello) => {
                self.hello = Some(hello.clone());
                self.begin();
                // Retain display facts across reconnects, but bind the physical
                // identity to this hello. An unknown/legacy hello clears it,
                // including when the new host caption is absent.
                let mut machine = self.machine.clone().unwrap_or_default();
                if !hello.host.is_empty() {
                    machine.host = Some(hello.host.clone());
                }
                machine.machine_id = hello
                    .machine_id
                    .clone()
                    .filter(|id| crate::creation::canonical_uuid(id));
                if self.machine.as_ref() != Some(&machine) {
                    self.machine = Some(machine.clone());
                    changes.push(Change::Machine {
                        computer: computer.clone(),
                        machine,
                    });
                }
            }
            Body::Snapshot(snapshot) if snapshot.topic == topic::CATALOG => {
                let machine = self.machine(computer);
                if snapshot.page == 0 {
                    self.staged = Some(BTreeMap::new());
                }
                let staged = self.staged.get_or_insert_with(BTreeMap::new);
                for item in snapshot.items.iter().flatten() {
                    let (Some(key), Some(row)) =
                        (item.get("key").and_then(Value::as_str), item.get("row"))
                    else {
                        continue;
                    };
                    staged.insert(key.to_owned(), project(row, &machine, computer));
                }
                if snapshot.last {
                    let rows = self.staged.take().unwrap_or_default();
                    changes.extend(self.replace_rows(computer, rows));
                    if !self.live {
                        self.live = true;
                        changes.push(Change::Computer {
                            computer: computer.clone(),
                            status: ComputerStatus::Live,
                        });
                    }
                }
            }
            Body::Snapshot(snapshot) if snapshot.topic == topic::CATALOG_META => {
                if snapshot.last {
                    if let Some(meta) = snapshot.value.clone() {
                        changes.extend(self.set_meta(computer, meta));
                    }
                }
            }
            Body::Snapshot(snapshot) if snapshot.topic == topic::ACTIVITY => {
                let key = snapshot.key.as_deref().unwrap_or("");
                if let Some(activity) = self.activity.get_mut(key) {
                    if snapshot.page == 0 {
                        activity.staged = Some(Vec::new());
                    }
                    let staged = activity.staged.get_or_insert_with(Vec::new);
                    staged.extend(snapshot.items.iter().flatten().cloned());
                    if snapshot.last {
                        activity.cursor = snapshot.cursor;
                        let events = activity.staged.take().unwrap_or_default();
                        changes.push(activity_change(
                            computer,
                            key,
                            ActivityUpdate::Replace {
                                events,
                                cursor: snapshot.cursor,
                            },
                        ));
                    }
                }
            }
            Body::Delta(delta) if delta.topic == topic::CATALOG => {
                let machine = self.machine(computer);
                let mut upserts: Vec<(&String, ViewRow)> = delta
                    .upsert
                    .iter()
                    .map(|item| (&item.key, project(&item.row, &machine, computer)))
                    .collect();
                // The rows before this frame are Qt's `before`, read once per frame.
                Before::index(&self.rows, upserts.iter().map(|(_, row)| row))
                    .keep(upserts.iter_mut().map(|(_, row)| row));
                for (key, row) in upserts {
                    if let Some(staged) = self.staged.as_mut() {
                        staged.insert(key.clone(), row.clone());
                    }
                    self.rows.insert(key.clone(), row.clone());
                    changes.push(Change::Upsert {
                        computer: computer.clone(),
                        key: key.clone(),
                        row,
                    });
                }
                for removal in &delta.remove {
                    if let Some(staged) = self.staged.as_mut() {
                        staged.remove(&removal.key);
                    }
                    self.rows.remove(&removal.key);
                    let reason = removal
                        .reason
                        .as_deref()
                        .filter(|reason| REASONS.contains(reason))
                        .map(str::to_owned);
                    changes.push(Change::Remove {
                        computer: computer.clone(),
                        key: removal.key.clone(),
                        reason,
                    });
                    changes.extend(self.processes.end(computer, &removal.key));
                }
            }
            Body::Events(events) if events.topic == topic::ACTIVITY => {
                if let Some(activity) = self.activity.get_mut(&events.key) {
                    activity.cursor = events.cursor;
                    changes.push(activity_change(
                        computer,
                        &events.key,
                        ActivityUpdate::Events {
                            events: events.events.clone(),
                            cursor: events.cursor,
                        },
                    ));
                }
            }
            Body::Event(event) if event.topic == topic::ACTIVITY => {
                if let Some(activity) = self.activity.get_mut(&event.key) {
                    if activity.merge_event(&event.id, event.rev, event.event.clone()) {
                        let value = activity.state.clone().unwrap_or(Value::Null);
                        let rev = activity.rev;
                        changes.push(activity_change(
                            computer,
                            &event.key,
                            ActivityUpdate::State { value, rev },
                        ));
                    }
                }
            }
            Body::State(state) if state.topic == topic::ACTIVITY => {
                if let Some(activity) = self.activity.get_mut(&state.key) {
                    activity.replace_state(state.value.clone(), state.rev);
                    changes.push(activity_change(
                        computer,
                        &state.key,
                        ActivityUpdate::State {
                            value: state.value.clone(),
                            rev: state.rev,
                        },
                    ));
                }
            }
            Body::State(state) if state.topic == topic::PROCESSES => {
                changes.extend(
                    self.processes
                        .state(computer, &state.key, &state.value, state.rev),
                );
            }
            Body::Snapshot(snapshot) if snapshot.topic == topic::ORGANIZATION => {
                changes.extend(self.organization.snapshot_page(computer, snapshot));
            }
            Body::Delta(delta) if delta.topic == topic::ORGANIZATION => {
                changes.extend(self.organization.delta(computer, delta));
            }
            Body::Error(error) if error.topic.as_deref() == Some(topic::ORGANIZATION) => {
                changes.extend(self.organization.error(computer, error));
            }
            Body::Snapshot(snapshot) if snapshot.topic == topic::TASKS => {
                changes.extend(self.tasks.snapshot(computer, snapshot));
            }
            Body::Gap(gap) if gap.topic == topic::TASKS => {
                self.tasks.gap(gap.key.as_deref().unwrap_or(""));
            }
            Body::Gap(gap) if gap.topic == topic::CATALOG => {
                // The rows stay until the snapshot that follows is complete.
                self.staged = None;
            }
            Body::Gap(gap) if gap.topic == topic::ACTIVITY => {
                let key = gap.key.as_deref().unwrap_or("");
                if let Some(activity) = self.activity.get_mut(key) {
                    activity.staged = None;
                }
            }
            Body::Error(error) if error.fatal => {
                return self.end(computer, Instant::now());
            }
            Body::Error(error) if error.topic.as_deref() == Some(topic::PROCESSES) => {
                changes.extend(self.processes.error(computer, error));
            }
            Body::Error(error) if error.topic.as_deref() == Some(topic::TASKS) => {
                changes.extend(self.tasks.error(computer, error));
            }
            Body::Error(error) => {
                let ended = matches!(
                    error.code.as_str(),
                    "subscription_ended" | "session_changed" | "too_many_subscriptions"
                );
                let key = error.key.as_deref().unwrap_or("");
                if ended
                    && error.topic.as_deref() == Some(topic::ACTIVITY)
                    && self.activity.remove(key).is_some()
                {
                    changes.push(activity_change(computer, key, ActivityUpdate::Ended));
                }
            }
            _ => {}
        }
        changes
    }

    /// Replaces every row, as a complete catalog does; followed processes of
    /// keys it lacks end.
    fn replace_rows(
        &mut self,
        computer: &ComputerId,
        mut rows: BTreeMap<String, ViewRow>,
    ) -> Vec<Change> {
        Before::index(&self.rows, rows.values()).keep(rows.values_mut());
        self.rows = rows;
        let mut changes = vec![Change::Replace {
            computer: computer.clone(),
            rows: self
                .rows
                .iter()
                .map(|(key, row)| (key.clone(), row.clone()))
                .collect(),
        }];
        let rows = &self.rows;
        changes.extend(
            self.processes
                .retain(computer, |key| rows.contains_key(key)),
        );
        changes
    }

    /// `FleetState::applyNotificationPreferences` (`FleetState.cpp:203-215`):
    /// the preferences the host confirmed for a Mute or Unmute, applied to
    /// the session's row at once, so the window and the menu bar follow
    /// before the next listing, which the keep step then applies in full.
    /// Refused (no change) unless the listed row of that name, conversation
    /// and run is not archived and shares notifications, and the preferences
    /// are valid and neither older nor a different value of the same
    /// revision.
    pub fn apply_notification_preferences(
        &mut self,
        computer: &ComputerId,
        target: &crate::actions::Target,
        preferences: &Value,
    ) -> Vec<Change> {
        let Some(confirmed) = preferences.as_object() else {
            return Vec::new();
        };
        // Qt's matchingSession (`FleetState.cpp:187-195`) also compares the
        // archive and `created`; the action's target carries neither. A live
        // row has no archive, and one run ID names one run, which has one
        // `created`: the run check below covers them.
        let found = self.rows.iter_mut().find(|(_, row)| {
            row.name == target.name
                && row.conversation_id.as_deref().unwrap_or("") == target.conversation_id
                && row.state != "archived"
        });
        let Some((key, row)) = found else {
            return Vec::new();
        };
        let run = row.run_id.as_deref().unwrap_or("");
        if !row.shared_notifications
            || run.is_empty()
            || run != target.run_id
            || !view::preferences_valid(confirmed, &target.conversation_id)
        {
            return Vec::new();
        }
        let incoming = view::preferences_revision(confirmed.get("revision")).unwrap_or(-1);
        let current = row.notification_revision.unwrap_or(-1);
        let applied = view::Preferences {
            muted: confirmed.get("muted") == Some(&Value::Bool(true)),
            suppressed: confirmed.get("suppressed").cloned().unwrap_or(Value::Null),
        };
        let same = row
            .notify
            .preferences
            .as_ref()
            .is_some_and(|kept| kept.same(&applied));
        if incoming < current || (incoming == current && !same) {
            return Vec::new();
        }
        row.muted = applied.muted;
        row.notification_revision = Some(incoming);
        row.notify.preferences = Some(applied);
        row.notification_error = None;
        vec![Change::Upsert {
            computer: computer.clone(),
            key: key.clone(),
            row: row.clone(),
        }]
    }

    /// Keeps a `catalog.meta` value; a changed machine card is a change.
    fn set_meta(&mut self, computer: &ComputerId, meta: Value) -> Option<Change> {
        let mut machine = crate::MachineMeta::project(&meta);
        if let Some(hello) = &self.hello {
            machine.machine_id = hello
                .machine_id
                .clone()
                .filter(|id| crate::creation::canonical_uuid(id));
        }
        if machine.host.as_deref().is_none_or(str::is_empty) {
            machine.host = self
                .hello
                .as_ref()
                .map(|hello| hello.host.clone())
                .filter(|host| !host.is_empty());
        }
        self.meta = Some(meta);
        (self.machine.as_ref() != Some(&machine)).then(|| {
            self.machine = Some(machine.clone());
            Change::Machine {
                computer: computer.clone(),
                machine,
            }
        })
    }

    /// A listing of a computer without the stream (`hgs [@host] ls --json`):
    /// its sessions replace the rows and the rest is its `catalog.meta`, as the
    /// stream's snapshots would be. Followed sessions it lacks end.
    pub(crate) fn listing(&mut self, computer: &ComputerId, listing: &Value) -> Vec<Change> {
        let machine = match computer.alias() {
            Some(alias) => alias.to_owned(),
            None => listing["host"].as_str().unwrap_or_default().to_owned(),
        };
        let rows: BTreeMap<String, ViewRow> = listing["sessions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| {
                let name = row["name"].as_str()?;
                let key = zerus_watch_protocol::key_for(row["run_id"].as_str(), name);
                Some((key, project(row, &machine, computer)))
            })
            .collect();
        let mut changes = self.replace_rows(computer, rows);
        let gone: Vec<String> = self
            .activity
            .keys()
            .filter(|key| !self.rows.contains_key(*key))
            .cloned()
            .collect();
        for key in gone {
            self.activity.remove(&key);
            changes.push(activity_change(computer, &key, ActivityUpdate::Ended));
        }
        let mut meta = listing.clone();
        if let Some(object) = meta.as_object_mut() {
            object.remove("sessions");
        }
        changes.extend(self.set_meta(computer, meta));
        changes
    }

    /// An `inspect NAME --after CURSOR` of a followed session without the
    /// stream: new events after its cursor (a window replacement for its first
    /// read, a gap or a replaced window), and its summary when that changed.
    /// A read of another run ends the follow.
    pub(crate) fn activity_polled(
        &mut self,
        computer: &ComputerId,
        key: &str,
        run_id: Option<&str>,
        inspection: &Value,
        window: bool,
    ) -> Vec<Change> {
        let Some(activity) = self.activity.get_mut(key) else {
            return Vec::new();
        };
        let seen = inspection["run_id"].as_str().filter(|run| !run.is_empty());
        if run_id
            .zip(seen)
            .is_some_and(|(wanted, seen)| wanted != seen)
        {
            self.activity.remove(key);
            return vec![activity_change(computer, key, ActivityUpdate::Ended)];
        }
        let mut changes = Vec::new();
        let events = inspection["events"].as_array().cloned().unwrap_or_default();
        let cursor = inspection["cursor"].as_i64().unwrap_or(activity.cursor);
        let replace = window
            || inspection["history_truncated"] == true
            || inspection["replace_events"] == true;
        if replace {
            changes.push(activity_change(
                computer,
                key,
                ActivityUpdate::Replace { events, cursor },
            ));
        } else if !events.is_empty() {
            changes.push(activity_change(
                computer,
                key,
                ActivityUpdate::Events { events, cursor },
            ));
        }
        activity.cursor = cursor;
        let mut value = inspection.clone();
        if let Some(object) = value.as_object_mut() {
            for field in NOT_STATE {
                object.remove(field);
            }
        }
        let steady = |value: &Value| {
            let mut value = value.clone();
            if let Some(object) = value.as_object_mut() {
                object.remove("goal_observed_at");
            }
            value
        };
        if activity.state.as_ref().map(steady) != Some(steady(&value)) {
            let rev = activity.rev + 1;
            activity.replace_state(value.clone(), rev);
            changes.push(activity_change(
                computer,
                key,
                ActivityUpdate::State { value, rev },
            ));
        }
        changes
    }

    /// An `inspect NAME` of a followed session's processes without the
    /// stream: its `{processes, process_work}` when that changed apart from
    /// sampling times.
    pub(crate) fn processes_polled(
        &mut self,
        computer: &ComputerId,
        key: &str,
        inspection: &Value,
    ) -> Option<Change> {
        let value = serde_json::json!({
            "processes": inspection["processes"],
            "process_work": inspection["process_work"],
        });
        let steady = |value: &Value| {
            let mut value = value.clone();
            for part in ["processes", "process_work"] {
                if let Some(object) = value[part].as_object_mut() {
                    object.remove("sampled_at");
                }
            }
            value
        };
        let previous = self.processes.value(key);
        if previous.is_some_and(|(old, _)| steady(old) == steady(&value)) {
            return None;
        }
        let rev = previous.map_or(1, |(_, rev)| rev + 1);
        self.processes.state(computer, key, &value, rev)
    }

    /// The rows as last known, by key.
    pub fn rows(&self) -> &BTreeMap<String, ViewRow> {
        &self.rows
    }

    /// The latest `hello` of this computer.
    pub fn hello(&self) -> Option<&Hello> {
        self.hello.as_ref()
    }

    /// Indexed service bootstrap parts are original source metadata, not
    /// invented watch frames or a catalog snapshot. They carry no sequence.
    pub fn source_metadata(
        &mut self,
        computer: &ComputerId,
        part: &str,
        value: Value,
    ) -> Result<Vec<Change>, &'static str> {
        match part {
            "hello" => {
                let hello: Hello =
                    serde_json::from_value(value).map_err(|_| "invalid_source_hello")?;
                let mut machine = self.machine.clone().unwrap_or_default();
                if !hello.host.is_empty() {
                    machine.host = Some(hello.host.clone());
                }
                machine.machine_id = hello
                    .machine_id
                    .clone()
                    .filter(|id| crate::creation::canonical_uuid(id));
                self.hello = Some(hello);
                if self.machine.as_ref() == Some(&machine) {
                    return Ok(vec![]);
                }
                self.machine = Some(machine.clone());
                Ok(vec![Change::Machine {
                    computer: computer.clone(),
                    machine,
                }])
            }
            "meta" if value.is_object() => Ok(self.set_meta(computer, value).into_iter().collect()),
            _ => Err("invalid_source_metadata"),
        }
    }

    /// A new stream attempt starts (a probe or a spawn).
    pub fn begin(&mut self) {
        self.failed = false;
        self.live = false;
        self.staged = None;
        for activity in self.activity.values_mut() {
            activity.staged = None;
        }
        self.tasks.begin();
        self.organization.begin();
    }

    /// The stream ended, failed or stayed silent: the computer reconnects and
    /// its rows stay as last known. A second end of the same attempt (its exit
    /// after a fatal error) changes nothing.
    pub fn end(&mut self, computer: &ComputerId, now: Instant) -> Vec<Change> {
        if self.failed {
            return Vec::new();
        }
        self.begin();
        self.failed = true;
        self.failures = match self.last_failure {
            Some(last) if now.saturating_duration_since(last) <= FAILURE_WINDOW => {
                self.failures.saturating_add(1)
            }
            _ => 1,
        };
        self.last_failure = Some(now);
        let mut changes = vec![Change::Computer {
            computer: computer.clone(),
            status: ComputerStatus::Reconnecting {
                attempt: self.failures,
            },
        }];
        let keys: Vec<String> = self.tasks.keys().cloned().collect();
        for key in keys {
            changes.extend(self.tasks.unavailable(
                computer,
                &key,
                "disconnected",
                "The task connection was interrupted. Reconnecting…",
            ));
        }
        changes
    }

    /// Whether this attempt has ended.
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Consecutive failures (failures more than 10 minutes apart start over).
    pub fn attempt(&self) -> u32 {
        self.failures
    }

    /// Whether the catalog snapshot of this attempt has arrived.
    pub fn live(&self) -> bool {
        self.live
    }

    /// The latest `catalog.meta` value: the listing without its sessions.
    pub fn meta(&self) -> Option<&Value> {
        self.meta.as_ref()
    }

    /// The SSH aliases of the peers this computer lists, None when its
    /// `catalog.meta` has no `peers` array (an older host): keep the old list.
    pub fn peers(&self) -> Option<Vec<String>> {
        let peers = self.meta.as_ref()?.get("peers")?.as_array()?;
        Some(
            peers
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        )
    }

    /// Accepts Activity frames of this key from now on; a key followed before
    /// keeps its cursor and summary.
    pub fn follow(&mut self, key: &str) {
        self.activity.entry(key.to_owned()).or_default();
    }

    pub fn unfollow(&mut self, key: &str) {
        self.activity.remove(key);
    }

    pub fn is_followed(&self, key: &str) -> bool {
        self.activity.contains_key(key)
    }

    /// The journal `seq` to resume a followed key after.
    pub fn cursor(&self, key: &str) -> Option<i64> {
        self.activity.get(key).map(|activity| activity.cursor)
    }

    pub fn activity(&self, key: &str) -> Option<&ActivityState> {
        self.activity.get(key)
    }

    /// The followed processes; see [`crate::processes`].
    pub fn processes(&self) -> &ProcessesState {
        &self.processes
    }

    pub fn processes_mut(&mut self) -> &mut ProcessesState {
        &mut self.processes
    }

    /// The processes limit of the latest `hello`; None when it lacks the topic.
    pub fn processes_limit(&self) -> Option<usize> {
        let hello = self.hello.as_ref()?;
        hello
            .topics
            .iter()
            .any(|name| name == topic::PROCESSES)
            .then_some(hello.limits.processes_subscriptions)
    }

    /// The followed organization; see [`crate::organization`].
    pub fn organization(&self) -> &crate::OrganizationState {
        &self.organization
    }

    pub fn organization_mut(&mut self) -> &mut crate::OrganizationState {
        &mut self.organization
    }

    /// Whether the latest `hello` offers the organization topic.
    pub fn organization_offered(&self) -> bool {
        self.hello
            .as_ref()
            .is_some_and(|hello| hello.topics.iter().any(|name| name == topic::ORGANIZATION))
    }

    /// The followed task lists; see [`crate::tasks`].
    pub fn tasks(&self) -> &TasksState {
        &self.tasks
    }

    pub fn tasks_mut(&mut self) -> &mut TasksState {
        &mut self.tasks
    }

    /// The tasks limit of the latest `hello`; None when it lacks the topic.
    pub fn tasks_limit(&self) -> Option<usize> {
        let hello = self.hello.as_ref()?;
        hello
            .topics
            .iter()
            .any(|name| name == topic::TASKS)
            .then_some(hello.limits.tasks_subscriptions)
    }

    /// The name rows carry: a peer's alias, or this computer's host name.
    fn machine(&self, computer: &ComputerId) -> String {
        match computer.alias() {
            Some(alias) => alias.to_owned(),
            None => self
                .hello
                .as_ref()
                .map(|hello| hello.host.clone())
                .unwrap_or_default(),
        }
    }
}

/// Qt's text when a listing names the confirmed revision with other
/// preferences (`FleetState.cpp:116`).
pub const NOTIFICATION_CONFLICT: &str =
    "Conflicting shared notification preferences. Refresh the session.";

/// The previous rows a listing's rows keep values from, as `FleetState`'s
/// `setLocal` and `setPeer` read `before`: indexed once per frame, and only
/// under the keys the incoming rows look up.
struct Before<'a> {
    /// The host-confirmed preferences by `cmd`, then conversation: the
    /// highest revision (`keepNotificationPreferences`, `FleetState.cpp:91-119`).
    preferences: Index<'a>,
    /// The first known optional questions by name, then archive and
    /// conversation (`keepOptionalQuestions`, `FleetState.cpp:73-89`).
    questions: HashMap<String, Vec<Question<'a>>>,
}

/// One looked-up session of the questions' index: its archive and
/// conversation, and the first previous row with a known count.
struct Question<'a> {
    archive: String,
    conversation: String,
    row: Option<&'a ViewRow>,
}

/// Rows by two string keys, looked up without allocating; a key the
/// incoming rows look up starts with no row.
type Index<'a> = HashMap<String, HashMap<String, Option<&'a ViewRow>>>;

/// Qt's `archiveId`: only an archived row has one (`HgsClient.cpp`).
fn archive(row: &ViewRow) -> &str {
    if row.state == "archived" {
        row.archive_id.as_deref().unwrap_or("")
    } else {
        ""
    }
}

fn conversation(row: &ViewRow) -> &str {
    row.conversation_id.as_deref().unwrap_or("")
}

fn unknown_questions(row: &ViewRow) -> bool {
    row.notify.optional_reported && row.notify.optional_count < 0
}

fn slot<'i, 'a>(
    index: &'i mut Index<'a>,
    first: &str,
    second: &str,
) -> Option<&'i mut Option<&'a ViewRow>> {
    index.get_mut(first)?.get_mut(second)
}

fn found<'a>(index: &Index<'a>, first: &str, second: &str) -> Option<&'a ViewRow> {
    *index.get(first)?.get(second)?
}

impl<'a> Before<'a> {
    /// Only the keys the incoming rows look up are indexed, so a delta of a
    /// few rows reads the previous rows once without building a large map.
    fn index<'n>(
        previous: &'a BTreeMap<String, ViewRow>,
        incoming: impl Iterator<Item = &'n ViewRow>,
    ) -> Self {
        let mut before = Self {
            preferences: HashMap::new(),
            questions: HashMap::new(),
        };
        for row in incoming {
            if !conversation(row).is_empty() {
                before
                    .preferences
                    .entry(row.notify.cmd.clone())
                    .or_default()
                    .insert(conversation(row).to_owned(), None);
            }
            if unknown_questions(row) {
                before
                    .questions
                    .entry(row.name.clone())
                    .or_default()
                    .push(Question {
                        archive: archive(row).to_owned(),
                        conversation: conversation(row).to_owned(),
                        row: None,
                    });
            }
        }
        if before.preferences.is_empty() && before.questions.is_empty() {
            return before;
        }
        for old in previous.values() {
            if old.notify.preferences.is_some() {
                if let Some(kept) =
                    slot(&mut before.preferences, &old.notify.cmd, conversation(old))
                {
                    let newer = kept.is_none_or(|kept| {
                        kept.notification_revision.unwrap_or(-1)
                            < old.notification_revision.unwrap_or(-1)
                    });
                    if newer {
                        *kept = Some(old);
                    }
                }
            }
            if old.notify.optional_count >= 0 {
                let same = |question: &&mut Question| {
                    question.archive == archive(old) && question.conversation == conversation(old)
                };
                for question in before
                    .questions
                    .get_mut(&old.name)
                    .into_iter()
                    .flatten()
                    .filter(same)
                {
                    question.row.get_or_insert(old);
                }
            }
        }
        before
    }

    /// Applies both keep steps to the incoming rows.
    fn keep<'r>(&self, rows: impl Iterator<Item = &'r mut ViewRow>) {
        if self.preferences.is_empty() && self.questions.is_empty() {
            return;
        }
        for row in rows {
            self.keep_questions(row);
            self.keep_preferences(row);
        }
    }

    /// A null count while the host's question index is incomplete keeps the
    /// last count of the same session and conversation, whatever its state;
    /// another conversation starts unknown.
    fn keep_questions(&self, row: &mut ViewRow) {
        if !unknown_questions(row) {
            return;
        }
        let old = self
            .questions
            .get(&row.name)
            .into_iter()
            .flatten()
            .find_map(|question| {
                (question.archive == archive(row) && question.conversation == conversation(row))
                    .then_some(question.row)
                    .flatten()
            });
        if let Some(old) = old {
            row.notify.optional_count = old.notify.optional_count;
            row.notify.optional_ids.clone_from(&old.notify.optional_ids);
            row.optional_question_count = Some(old.notify.optional_count);
        }
    }

    /// A listing without shared notifications (a downgrade, or unknown)
    /// keeps the last confirmed preferences for display and suppression,
    /// with `shared_notifications` still false; an older revision keeps the
    /// confirmed one; the same revision with other preferences keeps it too
    /// and is Qt's conflict.
    fn keep_preferences(&self, row: &mut ViewRow) {
        if conversation(row).is_empty() {
            return;
        }
        let Some(old) = found(&self.preferences, &row.notify.cmd, conversation(row)) else {
            return;
        };
        if row.shared_notifications {
            let previous = old.notification_revision.unwrap_or(-1);
            let incoming = row.notification_revision.unwrap_or(-1);
            let same = match (&row.notify.preferences, &old.notify.preferences) {
                (Some(new), Some(old)) => new.same(old),
                (new, old) => new == old,
            };
            if incoming > previous || (incoming == previous && same) {
                return;
            }
            if incoming == previous {
                row.notification_error = Some(NOTIFICATION_CONFLICT.to_owned());
            }
        }
        row.notify.preferences.clone_from(&old.notify.preferences);
        row.muted = old.muted;
        row.notification_revision = old.notification_revision;
    }
}

fn project(row: &Value, machine: &str, computer: &ComputerId) -> ViewRow {
    let mut view = view::row(row);
    view.machine = machine.to_owned();
    view.compact_keys = crate::row_preferences::keys(&computer.0, &view);
    view
}

fn activity_change(computer: &ComputerId, key: &str, update: ActivityUpdate) -> Change {
    Change::Activity {
        computer: computer.clone(),
        key: key.to_owned(),
        update,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zerus_watch_protocol::decode;

    #[test]
    fn physical_computer_identity_survives_metadata_without_becoming_a_host_alias() {
        let computer = ComputerId::local();
        let mut state = CatalogState::new();
        let mut hello = fixture("basic.ndjson").remove(0);
        let Body::Hello(value) = &mut hello.body else {
            panic!("hello")
        };
        value.machine_id = Some("11111111-1111-4111-8111-111111111111".into());
        state.apply(&computer, &hello);
        let mut meta = serde_json::json!({"host":"display-alias","ok":true});
        state.set_meta(&computer, meta.clone());
        assert_eq!(
            state.machine.as_ref().unwrap().machine_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
        let Body::Hello(value) = &mut hello.body else {
            unreachable!()
        };
        value.machine_id = Some("22222222-2222-4222-8222-222222222222".into());
        state.apply(&computer, &hello);
        state.set_meta(&computer, meta.clone());
        assert_eq!(
            state.machine.as_ref().unwrap().machine_id.as_deref(),
            Some("22222222-2222-4222-8222-222222222222")
        );
        let Body::Hello(value) = &mut hello.body else {
            unreachable!()
        };
        value.machine_id = None;
        value.host.clear();
        state.apply(&computer, &hello);
        meta["machine_id"] = serde_json::json!("11111111-1111-4111-8111-111111111111");
        state.set_meta(&computer, meta);
        assert!(
            state.machine.as_ref().unwrap().machine_id.is_none(),
            "a legacy/unknown hello cannot retain the previous peer identity"
        );
    }

    #[test]
    fn raw_lifecycle_does_not_stage_or_retain_a_large_catalog() {
        let mut state = CatalogState::new();
        let computer = ComputerId::local();
        for n in 0..50_000 {
            let page = frame(json!({"v":1,"seq":n+1,"t":"snapshot","topic":"catalog",
                "page":n,"last":n==49_999,"cursor":1,"items":[{"key":format!("run:{n}"),
                "row":{"name":format!("s-{n}"),"run_id":format!("r-{n}")}}]}));
            assert!(crate::stream::source_lifecycle(&mut state, &computer, &page).is_empty());
        }
        assert!(state.rows.is_empty());
        assert!(state.staged.is_none());
        assert!(state.activity.is_empty());
        let fatal = frame(json!({"v":1,"seq":50001,"t":"error","code":"failed",
            "message":"fixture","topic":"organization","fatal":true}));
        crate::stream::source_lifecycle(&mut state, &computer, &fatal);
        assert!(state.failed());
        println!("raw lifecycle: 50000 source rows, zero resident or staged catalog rows");
    }

    fn fixture(name: &str) -> Vec<Frame> {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter_map(|line| decode(line).ok())
            .collect()
    }

    fn frame(value: Value) -> Frame {
        serde_json::from_value(value).unwrap()
    }

    fn apply_all(state: &mut CatalogState, computer: &ComputerId, frames: &[Frame]) -> Vec<Change> {
        frames
            .iter()
            .flat_map(|frame| state.apply(computer, frame))
            .collect()
    }

    fn keys(rows: &[(String, ViewRow)]) -> Vec<&str> {
        rows.iter().map(|(key, _)| key.as_str()).collect()
    }

    fn snapshot(seq: u64, rows: &[(&str, Value)]) -> Frame {
        let items: Vec<Value> = rows
            .iter()
            .map(|(key, row)| json!({"key": key, "row": row}))
            .collect();
        frame(
            json!({"v": 1, "seq": seq, "t": "snapshot", "topic": "catalog",
            "items": items, "page": 0, "last": true, "cursor": seq}),
        )
    }

    fn upsert(seq: u64, key: &str, row: Value) -> Frame {
        frame(json!({"v": 1, "seq": seq, "t": "delta", "topic": "catalog",
            "upsert": [{"key": key, "row": row}], "cursor": seq}))
    }

    fn preferences(muted: bool, revision: i64, main: Value) -> Value {
        json!({"conversation_id": "conversation", "muted": muted, "revision": revision, "suppressed": {
            "reply_id": null, "attention_signature": null, "optional_question_ids": [],
            "main_attention": main, "child_attention": {}}})
    }

    /// A Codex session asking for approval, as `hgs watch` lists it.
    fn asking_row(run: &str, attention: &str, preferences: Option<Value>) -> Value {
        let mut row = json!({"name": "codex/project/session", "cmd": "codex", "run_id": run,
            "conversation_id": "conversation", "created": 10, "tracked": true, "state": "running",
            "process_state": "running", "activity": "waiting", "phase": "approval",
            "attention_id": attention, "attention_started": 10});
        if let Some(preferences) = preferences {
            row["notification_preferences"] = preferences;
        }
        row
    }

    fn row_of<'a>(changes: &'a [Change], key: &str) -> &'a ViewRow {
        changes
            .iter()
            .find_map(|change| match change {
                Change::Upsert {
                    key: found, row, ..
                } if found == key => Some(row),
                Change::Replace { rows, .. } => rows
                    .iter()
                    .find(|(found, _)| found == key)
                    .map(|(_, row)| row),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no row {key} in {changes:?}"))
    }

    fn observe(
        tracker: &mut crate::attention::Tracker,
        state: &CatalogState,
        now: i64,
    ) -> crate::attention::Changes {
        let mut next = 0;
        tracker.observe(
            &crate::attention::Host {
                alias: "peer",
                label: "peer",
                ok: true,
                rows: state.rows(),
            },
            now,
            false,
            &mut || {
                next += 1;
                format!("notice-{now}-{next}")
            },
        )
    }

    #[test]
    fn a_downgraded_listing_keeps_the_confirmed_suppression() {
        // sharedSuppressionSurvivesCapabilityDowngrade
        let peer = ComputerId::from("peer");
        let main = json!({"run_id": "run", "phase": "approval", "request_id": "main-old", "started_at": 10});
        let confirmed = preferences(false, 2, main);
        let mut state = CatalogState::new();
        let mut tracker = crate::attention::Tracker::new(Map::new());
        state.apply(
            &peer,
            &snapshot(
                1,
                &[(
                    "run:run",
                    asking_row("run", "main-old", Some(confirmed.clone())),
                )],
            ),
        );
        assert!(
            observe(&mut tracker, &state, 100).raised.is_empty(),
            "suppressed"
        );
        let changes = state.apply(
            &peer,
            &snapshot(2, &[("run:run", asking_row("run", "main-old", None))]),
        );
        let kept = row_of(&changes, "run:run");
        assert!(
            !kept.shared_notifications,
            "no command authority without the capability"
        );
        assert!(!kept.muted);
        assert_eq!(kept.notification_revision, Some(2));
        assert_eq!(
            kept.notify
                .preferences
                .as_ref()
                .map(|p| p.suppressed.clone()),
            Some(confirmed["suppressed"].clone())
        );
        assert!(
            observe(&mut tracker, &state, 101).raised.is_empty(),
            "the baseline survives the downgrade"
        );
        state.apply(
            &peer,
            &upsert(3, "run:run", asking_row("run", "main-old", None)),
        );
        assert!(
            observe(&mut tracker, &state, 102).raised.is_empty(),
            "also across a delta"
        );
        state.apply(
            &peer,
            &upsert(4, "run:run", asking_row("run", "main-new", None)),
        );
        assert_eq!(
            observe(&mut tracker, &state, 103).raised.len(),
            1,
            "a newer request notifies"
        );
    }

    #[test]
    fn a_listing_older_than_a_confirmed_write_keeps_the_write() {
        let peer = ComputerId::from("peer");
        let mut state = CatalogState::new();
        state.apply(
            &peer,
            &snapshot(
                1,
                &[(
                    "run:run",
                    asking_row("run", "a", Some(preferences(false, 2, Value::Null))),
                )],
            ),
        );
        let target = crate::actions::Target {
            computer: "peer".into(),
            name: "codex/project/session".into(),
            run_id: "run".into(),
            conversation_id: "conversation".into(),
        };
        let muted = preferences(true, 3, Value::Null);
        let applied = state.apply_notification_preferences(&peer, &target, &muted);
        let row = row_of(&applied, "run:run");
        assert!(
            row.muted && row.notification_revision == Some(3),
            "applied at once"
        );
        // A poll started before the write still lists revision 2.
        let older = state.apply(
            &peer,
            &upsert(
                2,
                "run:run",
                asking_row("run", "a", Some(preferences(false, 2, Value::Null))),
            ),
        );
        let kept = row_of(&older, "run:run");
        assert!(
            kept.muted
                && kept.notification_revision == Some(3)
                && kept.notification_error.is_none()
        );
        let resumed = state.apply(
            &peer,
            &snapshot(
                3,
                &[(
                    "run:next",
                    asking_row("next", "a", Some(preferences(false, 2, Value::Null))),
                )],
            ),
        );
        assert!(
            row_of(&resumed, "run:next").muted,
            "by conversation, across a resume"
        );
        let newer = state.apply(
            &peer,
            &upsert(
                4,
                "run:next",
                asking_row("next", "a", Some(preferences(false, 4, Value::Null))),
            ),
        );
        assert!(!row_of(&newer, "run:next").muted, "a newer revision wins");
    }

    #[test]
    fn the_same_revision_with_other_preferences_is_a_conflict() {
        let peer = ComputerId::from("peer");
        let mut state = CatalogState::new();
        state.apply(
            &peer,
            &snapshot(
                1,
                &[(
                    "run:run",
                    asking_row("run", "a", Some(preferences(true, 2, Value::Null))),
                )],
            ),
        );
        let changes = state.apply(
            &peer,
            &upsert(
                2,
                "run:run",
                asking_row("run", "a", Some(preferences(false, 2, Value::Null))),
            ),
        );
        let row = row_of(&changes, "run:run");
        assert!(row.muted, "the confirmed value stays");
        assert_eq!(
            row.notification_error.as_deref(),
            Some(NOTIFICATION_CONFLICT)
        );
        let same = state.apply(
            &peer,
            &upsert(
                3,
                "run:run",
                asking_row("run", "a", Some(preferences(true, 2, Value::Null))),
            ),
        );
        assert_eq!(
            row_of(&same, "run:run").notification_error,
            None,
            "an agreeing listing clears it"
        );
        // Nothing was confirmed for another conversation.
        let mut other = asking_row("other", "a", None);
        other["conversation_id"] = json!("elsewhere");
        let changes = state.apply(&peer, &upsert(4, "run:other", other));
        assert!(row_of(&changes, "run:other").notify.preferences.is_none());
    }

    #[test]
    fn a_confirmed_write_needs_the_listed_run_and_a_newer_or_equal_value() {
        let peer = ComputerId::from("peer");
        let mut state = CatalogState::new();
        state.apply(
            &peer,
            &snapshot(
                1,
                &[(
                    "run:run",
                    asking_row("run", "a", Some(preferences(false, 2, Value::Null))),
                )],
            ),
        );
        let target = |run: &str| crate::actions::Target {
            computer: "peer".into(),
            name: "codex/project/session".into(),
            run_id: run.into(),
            conversation_id: "conversation".into(),
        };
        let muted = preferences(true, 3, Value::Null);
        assert!(
            state
                .apply_notification_preferences(&peer, &target("old"), &muted)
                .is_empty(),
            "another run"
        );
        assert!(
            state
                .apply_notification_preferences(
                    &peer,
                    &target("run"),
                    &preferences(true, 1, Value::Null)
                )
                .is_empty(),
            "older"
        );
        assert!(
            state
                .apply_notification_preferences(
                    &peer,
                    &target("run"),
                    &preferences(true, 2, Value::Null)
                )
                .is_empty(),
            "same revision, other value"
        );
        assert!(
            !state
                .apply_notification_preferences(
                    &peer,
                    &target("run"),
                    &preferences(false, 2, Value::Null)
                )
                .is_empty(),
            "same value again"
        );
        let mut invalid = muted.clone();
        invalid["conversation_id"] = json!("elsewhere");
        assert!(
            state
                .apply_notification_preferences(&peer, &target("run"), &invalid)
                .is_empty(),
            "invalid"
        );
        state.apply(&peer, &upsert(2, "run:run", asking_row("run", "a", None)));
        assert!(
            state
                .apply_notification_preferences(&peer, &target("run"), &muted)
                .is_empty(),
            "no shared notifications"
        );
    }

    #[test]
    fn optional_questions_are_kept_across_an_unknown_count_of_the_same_conversation() {
        // keepOptionalQuestions
        let local = ComputerId::local();
        let questions = |count: Value, conversation: &str, state: &str| {
            json!({"name": "codex/project/session", "cmd": "codex", "run_id": "run", "conversation_id": conversation,
                "state": state, "optional_questions": {"count": count, "ids": ["q1", "q2"]}})
        };
        let mut state = CatalogState::new();
        state.apply(
            &local,
            &snapshot(
                1,
                &[("run:run", questions(json!(2), "conversation", "running"))],
            ),
        );
        let unknown = state.apply(
            &local,
            &upsert(
                2,
                "run:run",
                questions(Value::Null, "conversation", "paused"),
            ),
        );
        let row = row_of(&unknown, "run:run");
        assert_eq!(row.optional_question_count, Some(2));
        assert_eq!(
            (row.notify.optional_count, row.notify.optional_ids.clone()),
            (2, vec!["q1".to_owned(), "q2".to_owned()]),
            "whatever the state"
        );
        let again = state.apply(
            &local,
            &snapshot(
                3,
                &[("run:run", questions(Value::Null, "conversation", "running"))],
            ),
        );
        assert_eq!(
            row_of(&again, "run:run").notify.optional_count,
            2,
            "also across a full listing"
        );
        assert_eq!(row_of(&again, "run:run").optional_question_count, Some(2));
        let other = state.apply(
            &local,
            &upsert(4, "run:run", questions(Value::Null, "other", "running")),
        );
        assert_eq!(
            row_of(&other, "run:run").notify.optional_count,
            -1,
            "another conversation starts unknown"
        );
        assert_eq!(row_of(&other, "run:run").optional_question_count, None);
    }

    #[test]
    fn preferences_compare_numbers_by_value_as_qt_does() {
        // QJsonObject ==: a baseline's 12 equals 12.0 (Qt 6 QJsonValue).
        let peer = ComputerId::from("peer");
        let main = |at: Value| json!({"run_id": "run", "phase": "approval", "request_id": "", "started_at": at});
        let mut state = CatalogState::new();
        state.apply(
            &peer,
            &snapshot(
                1,
                &[(
                    "run:run",
                    asking_row("run", "a", Some(preferences(true, 2, main(json!(12))))),
                )],
            ),
        );
        let listed = state.apply(
            &peer,
            &upsert(
                2,
                "run:run",
                asking_row("run", "a", Some(preferences(true, 2, main(json!(12.0))))),
            ),
        );
        assert_eq!(
            row_of(&listed, "run:run").notification_error,
            None,
            "the same preferences, no conflict"
        );
        let target = crate::actions::Target {
            computer: "peer".into(),
            name: "codex/project/session".into(),
            run_id: "run".into(),
            conversation_id: "conversation".into(),
        };
        let again = state.apply_notification_preferences(
            &peer,
            &target,
            &preferences(true, 2, main(json!(12))),
        );
        assert_eq!(again.len(), 1, "the same revision and value is accepted");
        let other = state.apply_notification_preferences(
            &peer,
            &target,
            &preferences(true, 2, main(json!(12.5))),
        );
        assert!(
            other.is_empty(),
            "another value of the same revision is not"
        );
    }

    #[test]
    fn the_highest_confirmed_revision_of_the_provider_is_kept() {
        // sharedPreferencesDoNotCrossProviders, and Qt's highest revision.
        let peer = ComputerId::from("peer");
        let mut archived = asking_row("old", "a", Some(preferences(false, 1, Value::Null)));
        archived["state"] = json!("archived");
        archived["archive_id"] = json!("archive-1");
        let mut claude = asking_row("claude-run", "a", Some(preferences(true, 9, Value::Null)));
        claude["cmd"] = json!("claude");
        claude["name"] = json!("claude/project/other");
        let mut state = CatalogState::new();
        state.apply(
            &peer,
            &snapshot(
                1,
                &[
                    ("a-archived", archived),
                    ("b-claude", claude),
                    (
                        "run:run",
                        asking_row("run", "a", Some(preferences(true, 3, Value::Null))),
                    ),
                ],
            ),
        );
        let downgraded = state.apply(&peer, &upsert(2, "run:next", asking_row("next", "a", None)));
        let kept = row_of(&downgraded, "run:next");
        assert!(
            kept.muted && kept.notification_revision == Some(3),
            "the highest codex revision, not claude's 9"
        );
    }

    #[test]
    fn a_confirmed_write_finds_the_live_row_and_clears_a_conflict() {
        let peer = ComputerId::from("peer");
        let mut archived = asking_row("old", "a", Some(preferences(false, 1, Value::Null)));
        archived["state"] = json!("archived");
        archived["archive_id"] = json!("archive-1");
        let mut state = CatalogState::new();
        state.apply(
            &peer,
            &snapshot(
                1,
                &[
                    ("a-archived", archived),
                    (
                        "run:run",
                        asking_row("run", "a", Some(preferences(true, 2, Value::Null))),
                    ),
                ],
            ),
        );
        let conflict = state.apply(
            &peer,
            &upsert(
                2,
                "run:run",
                asking_row("run", "a", Some(preferences(false, 2, Value::Null))),
            ),
        );
        assert!(row_of(&conflict, "run:run").notification_error.is_some());
        let target = crate::actions::Target {
            computer: "peer".into(),
            name: "codex/project/session".into(),
            run_id: "run".into(),
            conversation_id: "conversation".into(),
        };
        let applied = state.apply_notification_preferences(
            &peer,
            &target,
            &preferences(false, 3, Value::Null),
        );
        let row = row_of(&applied, "run:run");
        assert!(
            !row.muted && row.notification_error.is_none(),
            "an archived row of the conversation is never the one"
        );
    }

    #[test]
    fn a_kept_count_comes_from_a_row_that_knew_it() {
        let local = ComputerId::local();
        let questions = |count: Value| {
            json!({"name": "codex/project/session", "cmd": "codex", "run_id": "run", "conversation_id": "conversation",
                "optional_questions": {"count": count, "ids": ["q1"]}})
        };
        let mut state = CatalogState::new();
        state.apply(
            &local,
            &snapshot(
                1,
                &[
                    ("a-unknown", questions(Value::Null)),
                    ("b-known", questions(json!(1))),
                ],
            ),
        );
        let changes = state.apply(&local, &upsert(2, "c-next", questions(Value::Null)));
        assert_eq!(row_of(&changes, "c-next").notify.optional_count, 1);
    }

    #[test]
    fn a_confirmed_mute_takes_the_dock_count_and_the_banner_down_at_once() {
        let peer = ComputerId::from("peer");
        let mut state = CatalogState::new();
        let mut desk = crate::menubar::Desk::default();
        let mut tracker = crate::attention::Tracker::new(Map::new());
        for change in state.apply(
            &peer,
            &snapshot(
                1,
                &[(
                    "run:run",
                    asking_row("run", "a", Some(preferences(false, 2, Value::Null))),
                )],
            ),
        ) {
            desk.observe(&change);
        }
        let token = observe(&mut tracker, &state, 100).raised[0].token.clone();
        assert_eq!(desk.badge().as_deref(), Some("1"));
        let target = crate::actions::Target {
            computer: "peer".into(),
            name: "codex/project/session".into(),
            run_id: "run".into(),
            conversation_id: "conversation".into(),
        };
        for change in
            state.apply_notification_preferences(&peer, &target, &preferences(true, 3, Value::Null))
        {
            desk.observe(&change);
        }
        assert_eq!(desk.badge(), None, "before the next listing");
        let rows: BTreeMap<String, ViewRow> = desk.computers()[&peer].rows.clone();
        let mut next = 0;
        let changes = tracker.observe(
            &crate::attention::Host {
                alias: "peer",
                label: "peer",
                ok: true,
                rows: &rows,
            },
            101,
            false,
            &mut || {
                next += 1;
                format!("n{next}")
            },
        );
        assert_eq!(changes.cleared, vec![token]);
    }

    #[test]
    fn a_three_page_snapshot_gives_one_replace_after_the_last_page() {
        let frames = fixture("basic.ndjson");
        let local = ComputerId::local();
        let mut state = CatalogState::new();
        let hello = state.apply(&local, &frames[0]);
        assert!(
            matches!(&hello[..], [Change::Machine { computer, machine }] if computer.is_local() && machine.host.as_deref() == Some("workstation"))
        );
        assert!(state.rows().is_empty(), "hello adds no session rows");
        assert!(state.apply(&local, &frames[1]).is_empty(), "page 0");
        assert!(state.apply(&local, &frames[2]).is_empty(), "page 1");
        let changes = state.apply(&local, &frames[3]);
        let replaces: Vec<_> = changes
            .iter()
            .filter_map(|change| match change {
                Change::Replace { computer, rows } => Some((computer, rows)),
                _ => None,
            })
            .collect();
        assert_eq!(replaces.len(), 1);
        let (computer, rows) = replaces[0];
        assert_eq!(computer, &local);
        assert_eq!(
            keys(rows),
            vec![
                "name:sh/sample-project",
                "run:run-draft-1",
                "run:run-lint-1",
                "run:run-outline-1",
                "run:run-review-1",
                "run:run-tests-1"
            ]
        );
        // The local computer's rows carry its hello host.
        assert!(rows.iter().all(|(_, row)| row.machine == "workstation"));
        assert!(changes.contains(&Change::Computer {
            computer: local.clone(),
            status: ComputerStatus::Live
        }));
        assert_eq!(state.rows().len(), 6);
    }

    #[test]
    fn a_gap_keeps_the_rows_until_the_new_snapshot_is_complete() {
        let frames = fixture("gap.ndjson");
        let local = ComputerId::local();
        let mut state = CatalogState::new();
        apply_all(&mut state, &local, &frames[..2]);
        assert_eq!(state.rows().len(), 2);
        // gap, then the first page of the replacement and a heartbeat
        for frame in &frames[2..5] {
            assert!(state.apply(&local, frame).is_empty(), "{frame:?}");
            assert_eq!(
                state.rows().keys().collect::<Vec<_>>(),
                vec!["run:run-a", "run:run-b"]
            );
        }
        let changes = state.apply(&local, &frames[5]);
        assert_eq!(changes.len(), 1, "{changes:?}");
        let Change::Replace { rows, .. } = &changes[0] else {
            panic!("{changes:?}")
        };
        assert_eq!(
            keys(rows),
            vec!["name:sh/sample-project", "run:run-b", "run:run-c"]
        );
        assert_eq!(rows[1].1.status, "working");
    }

    #[test]
    fn removals_keep_known_reasons_and_report_unknown_ones_as_none() {
        let frames = fixture("basic.ndjson");
        let local = ComputerId::local();
        let mut state = CatalogState::new();
        apply_all(&mut state, &local, &frames[..6]);
        let changes = state.apply(&local, &frames[6]);
        let removes: Vec<_> = changes
            .iter()
            .filter_map(|change| match change {
                Change::Remove { key, reason, .. } => Some((key.as_str(), reason.as_deref())),
                _ => None,
            })
            .collect();
        assert_eq!(
            removes,
            vec![
                ("run:run-tests-1", Some("replaced")),
                ("run:run-outline-1", Some("archived")),
                ("run:run-draft-1", Some("forgotten")),
                ("name:sh/sample-project", Some("ended")),
                ("run:run-lint-1", None),
            ]
        );
        let upserts: Vec<_> = changes
            .iter()
            .filter_map(|change| match change {
                Change::Upsert { key, row, .. } => Some((key.as_str(), row.status.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            upserts,
            vec![
                ("run:run-review-1", "ready"),
                ("run:run-tests-2", "starting")
            ]
        );
        assert_eq!(
            state.rows().keys().collect::<Vec<_>>(),
            vec!["run:run-review-1", "run:run-tests-2"]
        );
        // `replaced`: the new run lists under the same name.
        assert_eq!(
            state.rows()["run:run-tests-2"].name,
            "codex/sample-project/tests"
        );
    }

    #[test]
    fn a_replaced_run_removes_the_old_key_and_upserts_the_new_one() {
        let frames = fixture("replaced.ndjson");
        let local = ComputerId::local();
        let mut state = CatalogState::new();
        apply_all(&mut state, &local, &frames[..2]);
        let changes = state.apply(&local, &frames[2]);
        assert_eq!(changes.len(), 2);
        assert!(matches!(&changes[0], Change::Upsert { key, row, .. }
            if key == "run:run-new" && row.name == "claude/sample-project/review"));
        assert!(matches!(&changes[1], Change::Remove { key, reason, .. }
            if key == "run:run-old" && reason.as_deref() == Some("replaced")));
    }

    #[test]
    fn a_burst_frame_gives_one_upsert_per_item() {
        let frames = fixture("burst.ndjson");
        let local = ComputerId::local();
        let mut state = CatalogState::new();
        apply_all(&mut state, &local, &frames[..2]);
        let changes = state.apply(&local, &frames[2]);
        assert_eq!(changes.len(), 5000);
        assert!(changes
            .iter()
            .all(|change| matches!(change, Change::Upsert { .. })));
        assert_eq!(state.rows().len(), 50);
        assert_eq!(state.rows()["run:b49"].updated_at, Some(1787844999.0));
    }

    #[test]
    fn a_fatal_error_reconnects_and_keeps_the_rows() {
        let frames = fixture("fatal.ndjson");
        let local = ComputerId::local();
        let mut state = CatalogState::new();
        apply_all(&mut state, &local, &frames[..2]);
        let changes = state.apply(&local, &frames[2]);
        assert_eq!(
            changes,
            vec![Change::Computer {
                computer: local.clone(),
                status: ComputerStatus::Reconnecting { attempt: 1 }
            }]
        );
        assert!(state.failed());
        assert_eq!(state.attempt(), 1);
        assert_eq!(state.rows().len(), 1, "rows stay as last known");
        // The stream's exit after its fatal error is the same failure.
        assert!(state.end(&local, Instant::now()).is_empty());
        assert_eq!(state.attempt(), 1);
    }

    #[test]
    fn failures_within_ten_minutes_count_up_and_later_ones_start_over() {
        let local = ComputerId::local();
        let mut state = CatalogState::new();
        let start = Instant::now();
        let hello = fixture("basic.ndjson").remove(0);
        let mut attempts = Vec::new();
        for offset in [0, 1, 2, 3, 4] {
            state.apply(&local, &hello);
            let changes = state.end(&local, start + Duration::from_secs(offset));
            attempts.push(changes[0].clone());
        }
        let expected: Vec<_> = (1..=5)
            .map(|attempt| Change::Computer {
                computer: local.clone(),
                status: ComputerStatus::Reconnecting { attempt },
            })
            .collect();
        assert_eq!(attempts, expected);
        state.apply(&local, &hello);
        state.end(
            &local,
            start + Duration::from_secs(4) + FAILURE_WINDOW + Duration::from_secs(1),
        );
        assert_eq!(state.attempt(), 1);
    }

    #[test]
    fn unknown_topics_and_non_fatal_errors_change_nothing() {
        let frames = fixture("basic.ndjson");
        let local = ComputerId::local();
        let mut state = CatalogState::new();
        apply_all(&mut state, &local, &frames[..4]);
        for value in [
            json!({"v":1,"seq":30,"t":"state","topic":"git","key":"/home/example/src/x","value":{},"rev":1}),
            json!({"v":1,"seq":31,"t":"snapshot","topic":"tasks","items":[],"page":0,"last":true,"cursor":1}),
            json!({"v":1,"seq":32,"t":"error","code":"unsupported_topic","message":"no","topic":"processes","fatal":false}),
            json!({"v":1,"seq":33,"t":"error","code":"too_many_subscriptions","message":"no","topic":"activity","fatal":false}),
            json!({"v":1,"seq":34,"t":"pong","id":3}),
            json!({"v":1,"seq":35,"t":"heartbeat","at":1.0}),
            json!({"v":1,"seq":36,"t":"delta","topic":"later","upsert":[],"remove":[],"cursor":1}),
        ] {
            assert!(state.apply(&local, &frame(value)).is_empty());
        }
        assert!(!state.failed());
    }

    #[test]
    fn catalog_meta_lists_the_peers_only_when_it_has_an_array() {
        let frames = fixture("basic.ndjson");
        let local = ComputerId::local();
        let mut state = CatalogState::new();
        assert_eq!(state.peers(), None);
        apply_all(&mut state, &local, &frames[..5]);
        assert_eq!(
            state.peers(),
            Some(vec!["studio".to_owned(), "build-01".to_owned()])
        );
        let older = frame(json!({"v":1,"seq":40,"t":"snapshot","topic":"catalog.meta",
            "value":{"host":"workstation"},"page":0,"last":true,"cursor":9}));
        state.apply(&local, &older);
        assert_eq!(state.peers(), None);
        assert_eq!(state.meta().unwrap()["host"], "workstation");
    }

    #[test]
    fn peer_rows_carry_the_alias_never_the_reported_host() {
        let frames = fixture("basic.ndjson");
        let peer = ComputerId::from("studio");
        let mut state = CatalogState::new();
        let changes = apply_all(&mut state, &peer, &frames[..4]);
        let Some(Change::Replace { computer, rows }) = changes
            .iter()
            .find(|change| matches!(change, Change::Replace { .. }))
        else {
            panic!("{changes:?}")
        };
        assert_eq!(computer, &peer);
        assert!(rows.iter().all(|(_, row)| row.machine == "studio"));
    }

    fn followed(state: &mut CatalogState, frames: &[Frame]) -> Vec<ActivityUpdate> {
        let local = ComputerId::local();
        frames
            .iter()
            .flat_map(|frame| state.apply(&local, frame))
            .filter_map(|change| match change {
                Change::Activity { key, update, .. } => {
                    assert_eq!(key, "run:run-review-1");
                    Some(update)
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn activity_frames_of_followed_sessions_become_activity_changes() {
        let frames = fixture("basic.ndjson");
        let mut ignored = CatalogState::new();
        assert!(followed(&mut ignored, &frames).is_empty(), "not followed");

        let mut state = CatalogState::new();
        state.follow("run:run-review-1");
        let updates = followed(&mut state, &frames);
        assert_eq!(updates.len(), 6, "{updates:#?}");
        let ActivityUpdate::Events { events, cursor } = &updates[0] else {
            panic!("{:?}", updates[0])
        };
        assert_eq!((events.len(), *cursor), (2, 41));
        let ActivityUpdate::State { value, rev } = &updates[1] else {
            panic!("{:?}", updates[1])
        };
        assert_eq!(*rev, 3);
        assert_eq!(value["provider_messages"].as_array().unwrap().len(), 1);
        // Both `event` frames merge into the summary's provider messages.
        for (update, detail) in updates[2..4]
            .iter()
            .zip(["Partial answer", "Complete answer"])
        {
            let ActivityUpdate::State { value, rev } = update else {
                panic!("{update:?}")
            };
            assert_eq!(*rev, 3);
            let messages = value["provider_messages"].as_array().unwrap();
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[1]["detail"], detail);
        }
        let ActivityUpdate::Replace { events, cursor } = &updates[4] else {
            panic!("{:?}", updates[4])
        };
        assert_eq!((events.len(), *cursor), (1, 45));
        assert_eq!(updates[5], ActivityUpdate::Ended);
        assert!(
            !state.is_followed("run:run-review-1"),
            "the subscription ended"
        );
    }

    fn processes_of(changes: Vec<Change>) -> Vec<(String, crate::ProcessesUpdate)> {
        changes
            .into_iter()
            .filter_map(|change| match change {
                Change::Processes { key, update, .. } => Some((key, update)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn processes_of_followed_sessions_change_until_they_leave_the_catalog() {
        use crate::ProcessesUpdate;
        let frames = fixture("basic.ndjson");
        let local = ComputerId::local();
        let mut state = CatalogState::new();
        for key in ["run:run-review-1", "run:run-lint-1", "run:missing"] {
            state.processes_mut().follow(key);
        }
        // A complete catalog without a followed key ends it.
        let ended = processes_of(apply_all(&mut state, &local, &frames[..4]));
        assert_eq!(ended, vec![("run:missing".into(), ProcessesUpdate::Ended)]);
        let value = json!({"processes": {"items": [], "active_count": 1, "sampled_at": 1.0}, "process_work": null});
        let states = processes_of(apply_all(
            &mut state,
            &local,
            &[
                frame(
                    json!({"v":1,"seq":20,"t":"state","topic":"processes","key":"run:run-review-1","value":value,"rev":4}),
                ),
                frame(
                    json!({"v":1,"seq":21,"t":"state","topic":"processes","key":"run:run-tests-1","value":value,"rev":1}),
                ),
                frame(
                    json!({"v":1,"seq":22,"t":"error","code":"inspect_failed","message":"ps failed","topic":"processes","key":"run:run-review-1","fatal":false}),
                ),
            ],
        ));
        assert_eq!(
            states,
            vec![
                (
                    "run:run-review-1".into(),
                    ProcessesUpdate::State {
                        value: value.clone(),
                        rev: 4
                    }
                ),
                (
                    "run:run-review-1".into(),
                    ProcessesUpdate::Failed {
                        code: "inspect_failed".into(),
                        message: "ps failed".into()
                    }
                ),
            ]
        );
        assert_eq!(
            state.processes().value("run:run-review-1"),
            Some((&value, 4))
        );
        // The basic fixture's processes error names no key; its delta removes
        // the lint session, which ends that follow.
        let changes = apply_all(&mut state, &local, &frames[4..]);
        assert_eq!(
            processes_of(changes),
            vec![("run:run-lint-1".into(), ProcessesUpdate::Ended)]
        );
        assert!(state.processes().is_followed("run:run-review-1"));
        assert!(!state.processes().is_followed("run:run-lint-1"));
        assert_eq!(
            state.processes_limit(),
            None,
            "the fixture's hello lacks the topic"
        );
    }

    #[test]
    fn a_changed_catalog_meta_gives_the_computer_card_once() {
        let frames = fixture("basic.ndjson");
        let local = ComputerId::local();
        let mut state = CatalogState::new();
        let machines = |changes: Vec<Change>| -> Vec<crate::MachineMeta> {
            changes
                .into_iter()
                .filter_map(|change| match change {
                    Change::Machine { computer, machine } => {
                        assert!(computer.is_local());
                        Some(machine)
                    }
                    _ => None,
                })
                .collect()
        };
        let first = machines(apply_all(&mut state, &local, &frames[..5]));
        assert_eq!(
            first.len(),
            2,
            "hello identity, then the complete machine card"
        );
        assert_eq!(first[0].host.as_deref(), Some("workstation"));
        // The same meta again, as after a reconnect, sends nothing; a new
        // sample does.
        assert!(machines(apply_all(&mut state, &local, &frames[4..5])).is_empty());
        let sampled = frame(
            json!({"v":1,"seq":50,"t":"snapshot","topic":"catalog.meta","page":0,"last":true,"cursor":1,
                   "value":{"host":"workstation","ok":true,"metrics":{"cpu_percent":42.0,"sampled_at":1787840100.0}}}),
        );
        let next = machines(state.apply(&local, &sampled));
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].metrics.cpu_percent, Some(42.0));
        assert_eq!(next[0].metrics.sampled_at, Some(1787840100.0));
        // A page before the last changes nothing.
        let partial = frame(
            json!({"v":1,"seq":51,"t":"snapshot","topic":"catalog.meta","page":0,"last":false,"cursor":1,
                   "value":{"host":"elsewhere"}}),
        );
        assert!(machines(state.apply(&local, &partial)).is_empty());
    }

    #[test]
    fn an_empty_catalog_keeps_its_hello_host_when_meta_omits_it_and_on_reconnect() {
        let frames = fixture("basic.ndjson");
        let local = ComputerId::local();
        let mut state = CatalogState::new();
        let initial = state.apply(&local, &frames[0]);
        assert!(
            matches!(&initial[..], [Change::Machine { machine, .. }] if machine.host.as_deref() == Some("workstation"))
        );
        let meta = frame(
            json!({"v":1,"seq":2,"t":"snapshot","topic":"catalog.meta","page":0,"last":true,"cursor":1,
            "value":{"local_sessions":false,"platform":"linux"}}),
        );
        let changed = state.apply(&local, &meta);
        assert!(
            matches!(&changed[..], [Change::Machine { machine, .. }] if machine.host.as_deref() == Some("workstation") && machine.local_sessions == Some(false))
        );
        assert!(
            state.apply(&local, &frames[0]).is_empty(),
            "hello preserves the card and emits no duplicate"
        );
        assert!(state.rows().is_empty());
        let peer = ComputerId::from("studio");
        let mut peer_state = CatalogState::new();
        let changed = peer_state.apply(&peer, &frames[0]);
        assert!(
            matches!(&changed[..], [Change::Machine { computer, machine }] if computer == &peer && machine.host.as_deref() == Some("workstation")),
            "the card still names the peer's real host; row grouping uses its alias"
        );
    }

    #[test]
    fn a_reconnect_resumes_activity_after_the_last_cursor() {
        let frames = fixture("basic.ndjson");
        let local = ComputerId::local();
        let mut state = CatalogState::new();
        state.follow("run:run-review-1");
        apply_all(&mut state, &local, &frames[..9]);
        assert_eq!(state.cursor("run:run-review-1"), Some(41));
        state.end(&local, Instant::now());
        state.apply(&local, &frames[0]);
        assert_eq!(state.cursor("run:run-review-1"), Some(41));
        state.unfollow("run:run-review-1");
        assert_eq!(state.cursor("run:run-review-1"), None);
    }

    fn message(kind: &str, id: &str, detail: &str, at: f64) -> Value {
        json!({"type": kind, "message_id": id, "detail": detail, "at": at, "source": "hooks", "agent_id": ""})
    }

    fn details(state: &ActivityState) -> Vec<String> {
        state.state.as_ref().unwrap()["provider_messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["detail"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn provider_messages_merge_by_id_and_rev() {
        let mut activity = ActivityState::default();
        assert!(!activity.merge_event("m-1", 1, message("AgentMessage", "m-1", "early", 10.0)));
        activity.replace_state(
            json!({"provider_messages": [message("AgentThinking", "t-1", "thinking", 5.0)]}),
            7,
        );
        assert!(activity.merge_event("m-1", 1, message("AgentMessage", "m-1", "draft", 10.0)));
        assert!(activity.merge_event("m-1", 2, message("AgentMessage", "m-1", "final", 11.0)));
        assert_eq!(details(&activity), vec!["thinking", "final"]);
        // A lower or equal rev of the same id changes nothing.
        assert!(!activity.merge_event("m-1", 2, message("AgentMessage", "m-1", "stale", 12.0)));
        assert!(!activity.merge_event("m-1", 1, message("AgentMessage", "m-1", "older", 12.0)));
        assert_eq!(details(&activity), vec!["thinking", "final"]);
        // The same message_id with another type is another message.
        assert!(activity.merge_event(
            "m-1t",
            1,
            message("AgentThinking", "m-1", "more thinking", 12.0)
        ));
        assert_eq!(
            details(&activity),
            vec!["thinking", "final", "more thinking"]
        );
        assert_eq!(activity.rev, 7);
        // The next state replaces the list wholesale.
        activity.replace_state(json!({"provider_messages": []}), 8);
        assert!(details(&activity).is_empty());
    }

    #[test]
    fn provider_messages_drop_repeats_within_three_seconds() {
        let mut activity = ActivityState::default();
        activity.replace_state(
            json!({"provider_messages": [message("AgentMessage", "", "Done.", 100.0)]}),
            1,
        );
        assert!(!activity.merge_event("a", 1, message("AgentMessage", "", "Done.", 102.5)));
        assert!(!activity.merge_event("b", 1, message("AgentMessage", "other-id", "Done.", 97.0)));
        assert!(
            activity.merge_event("c", 1, message("AgentThinking", "", "Done.", 101.0)),
            "another type"
        );
        assert!(
            activity.merge_event("d", 1, message("AgentMessage", "", "Done.", 103.5)),
            "3.5 s later"
        );
        assert_eq!(details(&activity).len(), 3);
    }

    #[test]
    fn provider_messages_stay_in_time_order_and_keep_the_newest_hundred() {
        let mut activity = ActivityState::default();
        activity.replace_state(json!({"name": "claude/sample-project/review"}), 1);
        for at in [30.0, 10.0, 20.0] {
            activity.merge_event(
                &format!("m{at}"),
                1,
                message("AgentMessage", &format!("m{at}"), &format!("{at}"), at),
            );
        }
        assert_eq!(details(&activity), vec!["10", "20", "30"]);
        for n in 0..150 {
            let at = 1000.0 + f64::from(n) * 10.0;
            activity.merge_event(
                &format!("n{n}"),
                1,
                message("AgentMessage", &format!("n{n}"), &format!("n{n}"), at),
            );
        }
        let kept = details(&activity);
        assert_eq!(kept.len(), 100);
        assert_eq!(kept.first().map(String::as_str), Some("n50"));
        assert_eq!(kept.last().map(String::as_str), Some("n149"));
        assert_eq!(
            activity.state.as_ref().unwrap()["name"],
            "claude/sample-project/review"
        );
    }
}
