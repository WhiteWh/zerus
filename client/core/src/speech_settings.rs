//! Settings > Voice input: the speech providers configured on a processing
//! computer, ported from `tray/src/speech/SpeechSettings.cpp`.
//!
//! [`Providers`] is a state machine. The page's actions and the calls'
//! answers go in, [`Effect`]s come out, and [`Providers::view`] is what the
//! page shows. The caller runs every call off the UI thread with [`run`].
//!
//! A change (`profile_upsert`, `profile_remove`, `profile_test`) carries a
//! request ID that is stored durably under [`PENDING_KEY`] before it is
//! sent, and it is never sent again: Check operation status only reads its
//! receipt. An editor's API key passes from [`EditorForm`] into the call's
//! standard input and nowhere else; [`Call`] and [`EditorForm`] never print it.

use std::cell::Cell;
use std::collections::HashSet;
use std::fmt;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::actions::Exit;
use crate::creation::{canonical_uuid, uuid};
use crate::settings::Settings;
use crate::voice;
pub use crate::voice_composer::Platform;
use crate::ComputerId;

/// Where the change under way is kept until its result is known
/// (SpeechSettings.cpp `pendingKey`).
pub const PENDING_KEY: &str = "speech/providerOperation";

const DOCUMENTATION: &str = "https://github.com/ufna/zerus/blob/main/docs/speech-setup.md";

/// A receipt still running is checked once a second, at most 60 times and
/// for 90 seconds (SpeechSettings.cpp:375).
const POLLS: u32 = 60;
const POLLING: Duration = Duration::from_secs(90);

/// The provider documentation, at the section for `adapter`
/// (SpeechSettings.cpp `documentationUrl`).
pub fn documentation_url(adapter: &str) -> String {
    match adapter {
        "gigaam" => format!("{DOCUMENTATION}#local-gigaam"),
        "openai_http" => format!("{DOCUMENTATION}#openai-compatible-server"),
        _ => DOCUMENTATION.into(),
    }
}

/// What the page shows when no browser opened the documentation
/// (SpeechSettings.cpp `openDocumentationLink`).
pub fn documentation_failure(url: &str) -> String {
    format!("Could not open the browser. Open this link manually:\n{url}")
}

/// How long a change's identity may take to reach the disk before the
/// change is given up unsent.
pub const STORE_PENDING: Duration = Duration::from_secs(2);

/// [`Effect::StorePending`], first half: the record into the store, under
/// the caller's lock so it is ordered with [`Effect::ClearPending`]. False
/// when the store refuses it.
pub fn keep_pending(settings: &Settings, text: &str) -> bool {
    settings.set(PENDING_KEY, json!(text)).is_ok()
}

/// [`Effect::StorePending`], second half, off the caller's lock: whether
/// the record reached the disk within [`STORE_PENDING`]. Answer
/// [`Providers::stored`] with it.
pub fn pending_written(settings: &Settings) -> bool {
    settings.flush_within(STORE_PENDING)
}

/// [`Effect::ClearPending`], under the caller's lock.
pub fn clear_pending(settings: &Settings) {
    let _ = settings.set(PENDING_KEY, json!(""));
}

/// What the computer list calls this computer.
pub fn local_computer_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Macos => "This Mac",
        Platform::Windows => "This Windows computer",
        Platform::Linux => "This computer (Linux)",
    }
}

/// The provider new recordings use (`speech/machine_id`, `speech/profile`)
/// and the computer it was chosen on (`speech/host`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Defaults {
    pub machine_id: String,
    pub profile: String,
    pub host: String,
}

/// Which request an answer belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallId {
    /// Voice statistics, for the refresh with this generation.
    Stats(u64),
    /// The provider configuration, for the refresh with this generation.
    List(u64),
    /// The change under way.
    Change,
    /// A read of the change's receipt.
    Status,
}

/// `hgs [@host] speech --json` with `payload` on standard input.
#[derive(Clone, PartialEq)]
pub struct Call {
    pub id: CallId,
    pub host: String,
    pub payload: Value,
}

impl fmt::Debug for Call {
    // The payload may hold an API key.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Call")
            .field("id", &self.id)
            .field("host", &self.host)
            .field("action", &self.payload.get("action"))
            .finish_non_exhaustive()
    }
}

/// A call's answer as Qt's `SpeechClient` gives it: the reply and the error
/// text, empty when the call succeeded. A change refused before it was sent
/// replies `client_dispatch_started: false`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Answer {
    pub reply: Map<String, Value>,
    pub error: String,
}

impl Answer {
    pub fn failed(error: impl Into<String>) -> Answer {
        Answer {
            reply: Map::new(),
            error: error.into(),
        }
    }
}

/// What the caller does.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Run the call ([`run`]) and answer [`Providers::answered`].
    Call(Call),
    /// Store this text under [`PENDING_KEY`] and wait until it is written,
    /// then answer [`Providers::stored`]. Nothing is sent before.
    StorePending(String),
    /// Remove [`PENDING_KEY`].
    ClearPending,
    /// Write these settings.
    Set(Vec<(&'static str, Value)>),
    /// Answer [`Providers::poll`] with this request ID in one second.
    PollLater(String),
}

/// A computer the page can manage providers on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostOption {
    /// The SSH alias, empty for this computer.
    pub value: String,
    pub label: String,
}

/// A provider in the list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfileRow {
    pub id: String,
    pub name: String,
    /// "HTTP server" or "Local GigaAM".
    pub kind: String,
    pub model: String,
    /// "Ready" or "Not ready".
    pub readiness: String,
    /// Why the provider is not ready; the row's tooltip.
    pub reason: String,
}

/// An adapter the processing computer offers, with its models and devices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AdapterView {
    pub id: String,
    pub label: String,
    pub models: Vec<String>,
    pub devices: Vec<String>,
}

/// The editor's starting values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EditorStart {
    pub name: String,
    pub id: String,
    pub adapter: String,
    pub model: String,
    pub language: String,
    pub device: String,
    pub base_url: String,
    pub allow_insecure_http: bool,
}

/// The provider editor (SpeechSettings.cpp `ProfileEditor`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EditorView {
    pub ticket: u64,
    /// "Add speech provider" or "Edit speech provider".
    pub title: String,
    pub create: bool,
    /// "Processing computer: …".
    pub computer: String,
    pub readiness: String,
    /// The adapters that can be edited, as the type list offers them.
    pub adapters: Vec<AdapterView>,
    /// A saved adapter this version cannot edit, listed as
    /// "Unsupported provider type".
    pub unsupported: Option<String>,
    pub start: EditorStart,
    /// A saved model or device the computer no longer offers: listed while
    /// the saved type stays selected.
    pub extra_model: Option<String>,
    pub extra_device: Option<String>,
    /// Whether an API key is stored ("Keep stored API key").
    pub key_stored: bool,
    pub status: String,
    pub busy: bool,
    pub conflict: bool,
    pub check_visible: bool,
    pub check_enabled: bool,
}

/// What the page shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct View {
    /// Grows with every view, so a page keeps the newest of views that
    /// reach it out of order.
    pub seq: u64,
    pub hosts: Vec<HostOption>,
    pub host: String,
    /// The list takes input only once loaded.
    pub loaded: bool,
    pub profiles: Vec<ProfileRow>,
    pub selected: Option<usize>,
    /// The selected provider's location and language; empty hides it.
    pub details: String,
    pub can_add: bool,
    pub can_edit: bool,
    pub can_remove: bool,
    pub can_test: bool,
    pub can_use_default: bool,
    pub default_text: String,
    pub status: String,
    /// The last change's result; empty hides it.
    pub result: String,
    /// Whether that result is a failure.
    pub result_failed: bool,
    pub can_refresh: bool,
    /// The change under way; empty hides it and Check operation status.
    pub pending: String,
    pub can_check: bool,
    /// Interface prose with this computer's counters; None before the first read.
    pub statistics: Option<crate::text::Text>,
    pub editor: Option<EditorView>,
}

/// The editor's fields, from the webview. The key is cleared there as soon
/// as Save is pressed and lives here only until the call is built.
#[derive(Clone, Default, Deserialize)]
pub struct EditorForm {
    pub name: String,
    pub id: String,
    pub adapter: String,
    pub model: String,
    pub language: String,
    pub device: String,
    pub base_url: String,
    pub allow_insecure_http: bool,
    /// `keep`, `replace` or `remove`.
    pub credential: String,
    pub key: String,
}

impl fmt::Debug for EditorForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EditorForm")
            .field("id", &self.id)
            .field("adapter", &self.adapter)
            .field("credential", &self.credential)
            .finish_non_exhaustive()
    }
}

fn id_valid(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn language_valid(language: &str) -> bool {
    language.is_empty() || (language.len() == 2 && language.chars().all(|c| c.is_ascii_lowercase()))
}

/// Qt's endpoint rule (SpeechSettings.cpp:139-141): a strict URL with a
/// host, no user information, query or fragment, over HTTPS, or HTTP with
/// explicit consent.
pub fn endpoint_valid(url: &str, insecure: bool) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let scheme = scheme.to_ascii_lowercase();
    if !(scheme == "https" || (scheme == "http" && insecure)) {
        return false;
    }
    if url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || "<>\\^`{|}\"".contains(c))
        || url.contains(['?', '#'])
    {
        return false;
    }
    let bytes = url.as_bytes();
    let escapes_valid = bytes.iter().enumerate().all(|(at, &byte)| {
        byte != b'%'
            || (bytes.get(at + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(at + 2).is_some_and(u8::is_ascii_hexdigit))
    });
    let authority = rest.split('/').next().unwrap_or("");
    if !escapes_valid || authority.contains('@') {
        return false;
    }
    // An IPv6 address in brackets, or a name without a colon.
    let (host_valid, port) = if let Some(inner) = authority.strip_prefix('[') {
        match inner.split_once(']') {
            Some((host, after)) if after.is_empty() || after.starts_with(':') => (
                !host.is_empty()
                    && host
                        .chars()
                        .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.'),
                after.strip_prefix(':'),
            ),
            _ => return false,
        }
    } else {
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        (!host.is_empty(), port)
    };
    let port_valid = port.is_none_or(|port| {
        port.is_empty() || (port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok())
    });
    host_valid && port_valid
}

impl EditorForm {
    fn remote(&self) -> bool {
        self.adapter == "openai_http"
    }

    /// Whether Save may send the form (`ProfileEditor::validate`), for the
    /// adapters the editor offered.
    pub fn valid(&self, adapters: &[AdapterView]) -> bool {
        let replace = self.remote() && self.credential == "replace";
        let lengths = self.name.chars().count() <= 128
            && self.model.chars().count() <= 128
            && self.base_url.chars().count() <= 2048
            && self.key.chars().count() <= 8192;
        let offered = adapters.iter().find(|adapter| adapter.id == self.adapter);
        let target = if self.remote() {
            endpoint_valid(self.base_url.trim(), self.allow_insecure_http)
        } else {
            offered.is_some_and(|adapter| {
                adapter.models.contains(&self.model) && adapter.devices.contains(&self.device)
            })
        };
        lengths
            && !self.name.trim().is_empty()
            && id_valid(&self.id)
            && !self.model.trim().is_empty()
            && language_valid(&self.language)
            && !self.adapter.is_empty()
            && matches!(self.credential.as_str(), "keep" | "replace" | "remove")
            && target
            && (!replace || !self.key.is_empty())
    }

    fn profile(&self) -> Value {
        let mut profile = json!({
            "id": self.id.trim(),
            "name": self.name.trim(),
            "adapter": self.adapter,
            "model": self.model.trim(),
            "language": self.language.trim(),
        });
        if self.remote() {
            profile["base_url"] = json!(self.base_url.trim());
            profile["allow_insecure_http"] = json!(self.allow_insecure_http);
        } else {
            profile["device"] = json!(self.device);
        }
        profile
    }

    fn secret(&self) -> Value {
        let mode = if self.remote() {
            self.credential.as_str()
        } else {
            "remove"
        };
        if mode == "replace" {
            json!({ "mode": mode, "value": self.key })
        } else {
            json!({ "mode": mode })
        }
    }
}

/// The remove confirmation the page asks (SpeechSettings.cpp:345-349):
/// "Remove speech provider?", "Remove provider" and Cancel (the default).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RemoveAsk {
    pub ticket: u64,
    pub text: String,
}

/// Whose change it is: the editor that saved it is told how it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Page,
    Editor(u64),
}

struct Editor {
    ticket: u64,
    create: bool,
    host: String,
    machine: String,
    revision: i64,
    ids: Vec<String>,
    computer: String,
    service: Map<String, Value>,
    adapters: Vec<AdapterView>,
    original: Map<String, Value>,
    start: EditorStart,
    extra_model: Option<String>,
    extra_device: Option<String>,
    status: String,
    busy: bool,
    conflict: bool,
    check_visible: bool,
}

struct Removal {
    ticket: u64,
    host: String,
    machine: String,
    revision: i64,
    profile: Value,
}

/// A change stored but not sent yet.
struct Storing {
    host: String,
    payload: Value,
}

fn text<'a>(object: &'a Map<String, Value>, key: &str) -> &'a str {
    object.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `QJsonValue::toString(fallback)`: the string, or the fallback for any
/// other type.
fn text_or<'a>(object: &'a Map<String, Value>, key: &str, fallback: &'a str) -> &'a str {
    object.get(key).and_then(Value::as_str).unwrap_or(fallback)
}

/// `QJsonValue::toInteger()`: a whole number, otherwise `fallback`.
fn integer(value: Option<&Value>, fallback: i64) -> i64 {
    match value {
        Some(Value::Number(number)) => number.as_i64().unwrap_or_else(|| {
            number
                .as_f64()
                .filter(|n| n.fract() == 0.0 && n.abs() < 9.2e18)
                .map_or(fallback, |n| n as i64)
        }),
        _ => fallback,
    }
}

fn number(value: Option<&Value>) -> f64 {
    value.and_then(Value::as_f64).unwrap_or(0.0)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().unwrap_or("").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// The voice statistics text (SpeechSettings.cpp:284-287).
pub fn statistics_text(reply: &Map<String, Value>) -> crate::text::Text {
    let totals = reply.get("totals").map(object).unwrap_or_default();
    // QJsonValue::toInt(90): a whole number that fits an int.
    let days = i32::try_from(integer(reply.get("retention_days"), 90)).unwrap_or(90);
    let count = |key: &str| crate::text::Arg::Number(integer(totals.get(key), 0));
    crate::text!(
        "This computer (last %1 days)\n%2 recordings, %3 completed, %4 failed, %5 cancelled, %6 uncertain\n%7 minutes of audio, %8 seconds processing\nThese counters stay on the speech computer and contain no audio or recognized text.",
        i64::from(days),
        count("submitted"),
        count("completed"),
        count("failed"),
        count("cancelled"),
        count("uncertain"),
        format!("{:.1}", number(totals.get("audio_ms")) / 60000.0),
        format!("{:.1}", number(totals.get("processing_ms")) / 1000.0),
    )
}

/// The provider list of a processing computer and the changes made to it.
pub struct Providers {
    local: &'static str,
    ids: Box<dyn FnMut() -> String + Send>,
    peers: Vec<String>,
    hosts: Vec<HostOption>,
    host: String,
    defaults: Defaults,
    generation: u64,
    loaded: bool,
    refreshing: bool,
    machine: String,
    revision: i64,
    adapters: Vec<Value>,
    service: Map<String, Value>,
    profiles: Vec<Map<String, Value>>,
    selected: Option<usize>,
    select_after: String,
    status: String,
    statistics: Option<crate::text::Text>,
    result: String,
    result_failed: bool,
    pending: Option<Map<String, Value>>,
    storing: Option<Storing>,
    owner: Option<Owner>,
    busy: bool,
    waiting: bool,
    polls: u32,
    since: Option<Instant>,
    editor: Option<Editor>,
    removal: Option<Removal>,
    tickets: u64,
    views: Cell<u64>,
}

/// Decode an existing saved operation without constructing a provider page or
/// producing calls. The caller bounds its source; original private fields stay.
pub(crate) fn decode_pending(pending: &str) -> Option<Map<String, Value>> {
    serde_json::from_str::<Value>(pending)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .filter(|pending| {
            canonical_uuid(text(pending, "machine_id")) && uuid(text(pending, "request_id"))
        })
}

impl Providers {
    /// The page for `platform`, managing providers on `host` (empty for this
    /// computer), with the change `pending` names still unconfirmed. A
    /// stored change without a valid identity is ignored, as Qt does.
    pub fn new(
        platform: Platform,
        host: &str,
        defaults: Defaults,
        pending: &str,
        ids: impl FnMut() -> String + Send + 'static,
    ) -> Providers {
        let pending = decode_pending(pending);
        let mut providers = Providers {
            local: local_computer_name(platform),
            ids: Box::new(ids),
            peers: Vec::new(),
            hosts: Vec::new(),
            host: host.to_owned(),
            defaults,
            generation: 0,
            loaded: false,
            refreshing: false,
            machine: String::new(),
            revision: 0,
            adapters: Vec::new(),
            service: Map::new(),
            profiles: Vec::new(),
            selected: None,
            select_after: String::new(),
            status: String::new(),
            statistics: None,
            result: String::new(),
            result_failed: false,
            pending,
            storing: None,
            owner: None,
            busy: false,
            waiting: false,
            polls: 0,
            since: None,
            editor: None,
            removal: None,
            tickets: 0,
            views: Cell::new(0),
        };
        providers.set_peers(Vec::new());
        providers
    }

    /// The computers this one lists (`SpeechSettings::setPeers`). The
    /// selected computer stays listed even when it is no longer a peer.
    pub fn set_peers(&mut self, peers: Vec<String>) {
        self.peers = peers;
        let mut hosts = vec![HostOption {
            value: String::new(),
            label: self.local.into(),
        }];
        let mut available = self.peers.clone();
        if !self.host.is_empty() && !available.contains(&self.host) {
            available.push(self.host.clone());
        }
        hosts.extend(available.into_iter().map(|host| HostOption {
            value: host.clone(),
            label: host,
        }));
        self.hosts = hosts;
    }

    /// The provider new recordings use, as the settings hold it now.
    pub fn set_defaults(&mut self, defaults: Defaults) {
        self.defaults = defaults;
    }

    fn label(&self, host: &str) -> String {
        if host.is_empty() {
            self.local.into()
        } else {
            host.into()
        }
    }

    fn ticket(&mut self) -> u64 {
        self.tickets += 1;
        self.tickets
    }

    fn selected_profile(&self) -> Option<&Map<String, Value>> {
        self.selected.and_then(|index| self.profiles.get(index))
    }

    /// Manage the providers of another computer: remembered as
    /// `speech/manage_host`, then refreshed.
    pub fn choose_host(&mut self, host: &str) -> Vec<Effect> {
        if host == self.host || !self.hosts.iter().any(|option| option.value == host) {
            return Vec::new();
        }
        self.host = host.to_owned();
        let mut effects = vec![Effect::Set(vec![("speech/manage_host", json!(host))])];
        effects.extend(self.refresh());
        effects
    }

    /// Select a provider in the list.
    pub fn select(&mut self, index: usize) {
        if self.loaded && index < self.profiles.len() {
            self.selected = Some(index);
        }
    }

    /// Read the statistics and the provider configuration again
    /// (`SpeechSettings::refresh`). Answers of an earlier refresh are
    /// dropped.
    pub fn refresh(&mut self) -> Vec<Effect> {
        self.generation += 1;
        let generation = self.generation;
        let selected = if self.select_after.is_empty() {
            self.selected_profile()
                .map(|profile| text(profile, "id").to_owned())
                .unwrap_or_default()
        } else {
            std::mem::take(&mut self.select_after)
        };
        self.select_after = selected;
        self.loaded = false;
        self.refreshing = true;
        self.status = "Checking provider configuration…".into();
        self.statistics = Some(crate::text!("Loading this computer's voice statistics…"));
        vec![
            Effect::Call(Call {
                id: CallId::Stats(generation),
                host: self.host.clone(),
                payload: json!({ "action": "stats" }),
            }),
            Effect::Call(Call {
                id: CallId::List(generation),
                host: self.host.clone(),
                payload: json!({ "action": "config_list" }),
            }),
        ]
    }

    /// A call's answer.
    pub fn answered(&mut self, id: CallId, answer: Answer, now: Instant) -> Vec<Effect> {
        let effects = match id {
            CallId::Stats(generation) => {
                if generation == self.generation {
                    self.statistics = Some(if answer.error.is_empty() {
                        statistics_text(&answer.reply)
                    } else {
                        crate::text!("Voice statistics unavailable.")
                    });
                }
                Vec::new()
            }
            CallId::List(generation) => {
                if generation == self.generation {
                    self.listed(answer);
                }
                Vec::new()
            }
            CallId::Change => self.operation_result(answer, true, now),
            CallId::Status => self.operation_result(answer, false, now),
        };
        // SpeechClient::requestFinished: the editor learns whether its change
        // is still unconfirmed.
        let unconfirmed = self.pending.is_some() && !self.busy && !self.waiting;
        let pending = self.pending.is_some();
        if let Some(editor) = self.editor.as_mut() {
            editor.check_visible = pending;
            if unconfirmed {
                editor.status =
                    "The operation is not confirmed. Check its status before making another change."
                        .into();
            }
        }
        effects
    }

    fn listed(&mut self, answer: Answer) {
        let selected = std::mem::take(&mut self.select_after);
        self.refreshing = false;
        let reply = answer.reply;
        let revision = match reply.get("revision") {
            Some(value @ Value::Number(_)) => integer(Some(value), -1),
            _ => -1,
        };
        if !answer.error.is_empty() || !canonical_uuid(text(&reply, "machine_id")) || revision < 0 {
            self.status = if answer.error.is_empty() {
                "The computer returned an invalid provider configuration. Update hgs and refresh."
                    .into()
            } else {
                answer.error
            };
            return;
        }
        self.machine = text(&reply, "machine_id").into();
        self.revision = revision;
        self.adapters = reply
            .get("adapters")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self.service = reply.get("service").map(object).unwrap_or_default();
        self.profiles = reply
            .get("profiles")
            .and_then(Value::as_array)
            .map(|profiles| profiles.iter().map(object).collect())
            .unwrap_or_default();
        let preferred = if self.defaults.machine_id == self.machine {
            self.defaults.profile.as_str()
        } else {
            ""
        };
        let row = self.profiles.iter().rposition(|profile| {
            let id = text(profile, "id");
            id == selected || (selected.is_empty() && id == preferred)
        });
        self.selected = row.or((!self.profiles.is_empty()).then_some(0));
        self.loaded = true;
        self.status = if self.service.get("available").and_then(Value::as_bool) != Some(true) {
            text_or(
                &self.service,
                "reason",
                "The recognition service is not ready. You can still add and edit providers.",
            )
            .into()
        } else if self.profiles.is_empty() {
            "No providers yet. Add Local GigaAM or an OpenAI-compatible server.".into()
        } else {
            "Provider configuration is ready. Test a saved provider before using it.".into()
        };
    }

    /// Use as default: new recordings use the selected provider
    /// (SpeechSettings.cpp:312-316).
    pub fn use_default(&mut self) -> Vec<Effect> {
        let Some(profile) = self.selected_profile().filter(|_| self.loaded) else {
            return Vec::new();
        };
        let revision = profile.get("revision").map_or(0, |revision| {
            revision
                .as_i64()
                .or_else(|| revision.as_f64().map(|n| n.round() as i64))
                .unwrap_or(0)
        });
        let id = text(profile, "id").to_owned();
        self.defaults = Defaults {
            machine_id: self.machine.clone(),
            profile: id.clone(),
            host: self.host.clone(),
        };
        vec![Effect::Set(vec![
            ("speech/machine_id", json!(self.machine)),
            ("speech/profile", json!(id)),
            ("speech/host", json!(self.host)),
            ("speech/profile_revision", json!(revision)),
        ])]
    }

    /// Open the editor to add a provider or edit the selected one; false
    /// when it cannot open now.
    pub fn open_editor(&mut self, create: bool) -> bool {
        if !self.loaded || self.busy || self.pending.is_some() || self.editor.is_some() {
            return false;
        }
        let original = if create {
            Map::new()
        } else {
            match self.selected_profile() {
                Some(profile) => profile.clone(),
                None => return false,
            }
        };
        let adapters: Vec<AdapterView> = self
            .adapters
            .iter()
            .filter_map(|adapter| {
                let id = adapter.get("id").and_then(Value::as_str).unwrap_or("");
                let label = match id {
                    "gigaam" => "Local GigaAM",
                    "openai_http" => "OpenAI-compatible server",
                    _ => return None,
                };
                Some(AdapterView {
                    id: id.into(),
                    label: label.into(),
                    models: strings(adapter.get("models")),
                    devices: strings(adapter.get("devices")),
                })
            })
            .collect();
        let saved = text(&original, "adapter");
        let adapter = if create {
            adapters
                .first()
                .map(|adapter| adapter.id.clone())
                .unwrap_or_default()
        } else {
            saved.to_owned()
        };
        let offered = adapters.iter().find(|offered| offered.id == adapter);
        let models = offered
            .map(|offered| offered.models.as_slice())
            .unwrap_or(&[]);
        let devices = offered
            .map(|offered| offered.devices.as_slice())
            .unwrap_or(&[]);
        let remote = adapter == "openai_http";
        let saved_model = text(&original, "model");
        let (model, extra_model) = if create {
            (models.first().cloned().unwrap_or_default(), None)
        } else if remote || models.iter().any(|model| model == saved_model) {
            (saved_model.to_owned(), None)
        } else {
            (saved_model.to_owned(), Some(saved_model.to_owned()))
        };
        let saved_device = text_or(&original, "device", "cpu");
        let (device, extra_device) = if devices.iter().any(|device| device == saved_device) {
            (saved_device.to_owned(), None)
        } else if !create && !remote {
            (saved_device.to_owned(), Some(saved_device.to_owned()))
        } else {
            (devices.first().cloned().unwrap_or_default(), None)
        };
        let id = if create {
            let fresh = (self.ids)();
            format!("provider-{}", fresh.chars().take(8).collect::<String>())
        } else {
            text(&original, "id").to_owned()
        };
        let start = EditorStart {
            name: text(&original, "name").into(),
            id,
            adapter,
            model,
            language: text_or(&original, "language", "ru").into(),
            device,
            base_url: text(&original, "base_url").into(),
            allow_insecure_http: original
                .get("allow_insecure_http")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        let ticket = self.ticket();
        self.editor = Some(Editor {
            ticket,
            create,
            host: self.host.clone(),
            machine: self.machine.clone(),
            revision: self.revision,
            ids: self
                .profiles
                .iter()
                .map(|profile| text(profile, "id").to_owned())
                .collect(),
            computer: self.label(&self.host),
            service: self.service.clone(),
            adapters,
            original,
            start,
            extra_model,
            extra_device,
            status: String::new(),
            busy: false,
            conflict: false,
            check_visible: false,
        });
        true
    }

    /// Close the editor (Cancel). A change it saved goes on; its result
    /// shows on the page.
    pub fn close_editor(&mut self, ticket: u64) {
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.ticket == ticket)
        {
            self.editor = None;
        }
    }

    /// Save in the editor (SpeechSettings.cpp:326-338).
    pub fn save(&mut self, ticket: u64, form: EditorForm) -> Vec<Effect> {
        let Some(editor) = self
            .editor
            .as_mut()
            .filter(|editor| editor.ticket == ticket)
        else {
            return Vec::new();
        };
        let mut form = form;
        if editor.busy || editor.conflict || !form.valid(&editor.adapters) {
            return Vec::new();
        }
        if !editor.create && form.id != editor.start.id {
            return Vec::new();
        }
        if editor.create && editor.ids.iter().any(|id| id == form.id.trim()) {
            editor.status =
                "That profile ID already exists. Edit the existing provider or choose another ID."
                    .into();
            return Vec::new();
        }
        let payload = json!({
            "action": "profile_upsert",
            "expected_revision": editor.revision,
            "profile": form.profile(),
            "credential": form.secret(),
        });
        form.key.clear();
        editor.busy = true;
        editor.status = "Saving on the selected computer. If the result is not confirmed, use Check operation status.".into();
        editor.check_visible = true;
        let (host, machine) = (editor.host.clone(), editor.machine.clone());
        self.operation(&host, &machine, payload, Owner::Editor(ticket))
    }

    /// Remove: the confirmation to ask first (SpeechSettings.cpp:341-352).
    pub fn ask_remove(&mut self) -> Option<RemoveAsk> {
        if !self.loaded || self.pending.is_some() {
            return None;
        }
        let profile = self.selected_profile()?.clone();
        let ticket = self.ticket();
        let text = format!(
            "Remove {} from {}? Existing voice messages and recordings are kept. A recording using this provider will need an explicit replacement before sending.",
            text(&profile, "name"),
            self.label(&self.host)
        );
        self.removal = Some(Removal {
            ticket,
            host: self.host.clone(),
            machine: self.machine.clone(),
            revision: self.revision,
            profile: profile.get("id").cloned().unwrap_or(Value::Null),
        });
        Some(RemoveAsk { ticket, text })
    }

    /// The confirmation's answer: removes the provider if nothing changed
    /// while it was asked.
    pub fn remove(&mut self, ticket: u64, confirmed: bool) -> Vec<Effect> {
        let Some(removal) = self.removal.take() else {
            return Vec::new();
        };
        if !confirmed
            || removal.ticket != ticket
            || !self.loaded
            || self.machine != removal.machine
            || self.host != removal.host
            || self.revision != removal.revision
            || self.pending.is_some()
        {
            return Vec::new();
        }
        let mut payload = json!({
            "action": "profile_remove",
            "expected_revision": removal.revision,
        });
        // Qt omits a missing ID rather than sending null.
        if !removal.profile.is_null() {
            payload["profile_id"] = removal.profile;
        }
        self.operation(&removal.host, &removal.machine, payload, Owner::Page)
    }

    /// Test: a short synthetic recording on the selected computer
    /// (SpeechSettings.cpp:353-357).
    pub fn test(&mut self) -> Vec<Effect> {
        if !self.loaded || self.pending.is_some() {
            return Vec::new();
        }
        let Some(profile) = self.selected_profile() else {
            return Vec::new();
        };
        let mut payload = json!({ "action": "profile_test" });
        // Qt omits what the profile lacks rather than sending null.
        for (field, key) in [
            ("profile_id", "id"),
            ("expected_profile_revision", "revision"),
        ] {
            if let Some(value) = profile.get(key) {
                payload[field] = value.clone();
            }
        }
        let (host, machine) = (self.host.clone(), self.machine.clone());
        self.operation(&host, &machine, payload, Owner::Page)
    }

    fn operation(
        &mut self,
        host: &str,
        machine: &str,
        mut payload: Value,
        owner: Owner,
    ) -> Vec<Effect> {
        if self.pending.is_some() || self.busy {
            return Vec::new();
        }
        self.result.clear();
        let request = (self.ids)();
        payload["request_id"] = json!(request);
        payload["expected_machine_id"] = json!(machine);
        let pending = json!({
            "host": host,
            "computer": self.label(host),
            "machine_id": machine,
            "request_id": request,
            "action": payload["action"],
        });
        self.pending = Some(object(&pending));
        self.storing = Some(Storing {
            host: host.to_owned(),
            payload,
        });
        self.owner = Some(owner);
        self.busy = true;
        vec![Effect::StorePending(pending.to_string())]
    }

    /// Whether the change's identity was written; only then is it sent.
    pub fn stored(&mut self, written: bool, now: Instant) -> Vec<Effect> {
        let Some(storing) = self.storing.take() else {
            return Vec::new();
        };
        if !written {
            // Nothing was sent: the record, if the store took it, goes.
            self.pending = None;
            self.busy = false;
            let mut effects = vec![Effect::ClearPending];
            let failure = "Could not save the operation identity. No provider request was sent.";
            match self.owner.take() {
                Some(Owner::Editor(ticket)) => {
                    self.editor_done(ticket, &Map::new(), failure);
                    if let Some(editor) = self.editor.as_mut() {
                        editor.check_visible = false;
                    }
                }
                // Qt drops this text for Test and Remove; the page shows it.
                _ => {
                    self.result = failure.into();
                    self.result_failed = true;
                }
            }
            return std::mem::take(&mut effects);
        }
        self.waiting = false;
        self.polls = 0;
        self.since = Some(now);
        vec![Effect::Call(Call {
            id: CallId::Change,
            host: storing.host,
            payload: storing.payload,
        })]
    }

    /// Check operation status: reads the change's receipt; never sends the
    /// change again (SpeechSettings.cpp:392-399).
    pub fn check(&mut self, now: Instant) -> Vec<Effect> {
        let Some(pending) = self.pending.as_ref().filter(|_| !self.busy) else {
            return Vec::new();
        };
        let call = Call {
            id: CallId::Status,
            host: text(pending, "host").into(),
            payload: json!({
                "action": "config_status",
                "operation_id": pending.get("request_id").cloned().unwrap_or(Value::Null),
                "expected_machine_id": pending.get("machine_id").cloned().unwrap_or(Value::Null),
            }),
        };
        self.since.get_or_insert(now);
        self.busy = true;
        vec![Effect::Call(call)]
    }

    /// The poll timer for `request`.
    pub fn poll(&mut self, request: &str, now: Instant) -> Vec<Effect> {
        let current = self
            .pending
            .as_ref()
            .is_some_and(|pending| text(pending, "request_id") == request);
        if current && !self.busy {
            self.check(now)
        } else {
            Vec::new()
        }
    }

    fn operation_result(&mut self, answer: Answer, initial: bool, now: Instant) -> Vec<Effect> {
        self.busy = false;
        self.waiting = false;
        let Some(pending) = self.pending.clone() else {
            return Vec::new();
        };
        let reply = &answer.reply;
        let not_started =
            initial && reply.get("client_dispatch_started") == Some(&Value::Bool(false));
        let state = if not_started {
            "failed"
        } else {
            text(reply, "state")
        };
        let ours = reply.get("machine_id") == pending.get("machine_id")
            && reply.get("request_id") == pending.get("request_id");
        let mut effects = Vec::new();
        if answer.error.is_empty() && ours && state == "running" {
            let elapsed = self.since.map_or(Duration::ZERO, |since| now - since);
            if self.polls < POLLS && elapsed < POLLING {
                self.polls += 1;
                self.waiting = true;
                effects.push(Effect::PollLater(text(&pending, "request_id").into()));
            } else {
                self.polls += 1;
            }
        }
        if !not_started
            && (!answer.error.is_empty()
                || !ours
                || !matches!(state, "completed" | "failed")
                || text(reply, "error_code") == "operation_not_found")
        {
            return effects;
        }
        self.pending = None;
        effects.push(Effect::ClearPending);
        let failure = if not_started {
            answer.error.clone()
        } else if state == "failed" {
            text_or(reply, "error", "The provider request failed.").to_owned()
        } else {
            String::new()
        };
        if let Some(Owner::Editor(ticket)) = self.owner.take() {
            self.editor_done(ticket, reply, &failure);
        }
        let action = text(&pending, "action");
        if text(&pending, "host") == self.host {
            if state == "completed" && action == "profile_upsert" {
                self.select_after = text(reply, "profile_id").into();
            }
            effects.extend(self.refresh());
        }
        let computer = text(&pending, "computer");
        self.result_failed = state != "completed";
        self.result = if state == "completed" {
            if action == "profile_test" {
                format!("The provider test on {computer} completed successfully.")
            } else {
                format!("Provider configuration saved on {computer}.")
            }
        } else {
            failure
        };
        effects
    }

    /// The editor's change ended: a saved one closes it; otherwise it shows
    /// why and keeps the form.
    fn editor_done(&mut self, ticket: u64, reply: &Map<String, Value>, failure: &str) {
        let Some(editor) = self
            .editor
            .as_mut()
            .filter(|editor| editor.ticket == ticket)
        else {
            return;
        };
        if failure.is_empty() && text(reply, "state") == "completed" {
            self.editor = None;
            return;
        }
        editor.conflict = text(reply, "error_code") == "revision_conflict";
        editor.busy = false;
        editor.status = if editor.conflict {
            "The saved configuration changed. Your edits are kept here. Close and reopen the provider to review its current values before saving.".into()
        } else {
            failure.into()
        };
    }

    fn default_text(&self) -> String {
        let defaults = &self.defaults;
        if defaults.profile.is_empty() {
            return "Use as default selects a provider for new recordings. Existing recordings keep their selected provider.".into();
        }
        if defaults.machine_id == self.machine
            || (defaults.machine_id.is_empty() && self.host == defaults.host)
        {
            match self
                .profiles
                .iter()
                .find(|profile| text(profile, "id") == defaults.profile)
            {
                Some(profile) => format!("Default for new recordings: {}", text(profile, "name")),
                None => "The selected default provider is unavailable. Choose another explicitly or restore it on this computer.".into(),
            }
        } else {
            "The default provider is configured on another computer. Browsing these providers does not change it.".into()
        }
    }

    fn editor_view(&self) -> Option<EditorView> {
        let editor = self.editor.as_ref()?;
        let computer = editor.computer.clone();
        let readiness = if editor.service.get("available").and_then(Value::as_bool) == Some(true) {
            format!("Recognition service is available on {computer}. Test the saved profile to verify the selected provider.")
        } else {
            format!(
                "Recognition service is not ready on {computer}. {}",
                text_or(
                    &editor.service,
                    "reason",
                    "Prepare the speech service on this processing computer before testing."
                )
            )
        };
        Some(EditorView {
            ticket: editor.ticket,
            title: if editor.create {
                "Add speech provider"
            } else {
                "Edit speech provider"
            }
            .into(),
            create: editor.create,
            computer: format!("Processing computer: {computer}"),
            readiness,
            adapters: editor.adapters.clone(),
            unsupported: (!editor.create
                && !editor
                    .adapters
                    .iter()
                    .any(|adapter| adapter.id == editor.start.adapter))
            .then(|| editor.start.adapter.clone()),
            start: editor.start.clone(),
            extra_model: editor.extra_model.clone(),
            extra_device: editor.extra_device.clone(),
            key_stored: editor
                .original
                .get("credential")
                .and_then(|credential| credential.get("configured"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            status: editor.status.clone(),
            busy: editor.busy,
            conflict: editor.conflict,
            check_visible: editor.check_visible,
            check_enabled: !self.busy,
        })
    }

    /// What the page shows (`SpeechSettings::controls`).
    pub fn view(&self) -> View {
        let profile = self.selected_profile();
        let ready = self.loaded && !self.busy && self.pending.is_none();
        let mut details = profile
            .map(|profile| text(profile, "reason").to_owned())
            .unwrap_or_default();
        if let Some(profile) = profile {
            let location = if text(profile, "adapter") == "openai_http" {
                text(profile, "base_url").to_owned()
            } else {
                format!("Device: {}", text(profile, "device"))
            };
            let language = match text(profile, "language") {
                "" => "Auto-detect",
                language => language,
            };
            details = format!(
                "{location}\nLanguage: {language}{}",
                if details.is_empty() {
                    String::new()
                } else {
                    format!("\n{details}")
                }
            );
        }
        let pending = match &self.pending {
            None => String::new(),
            Some(_) if self.busy || self.waiting => {
                "Working on the provider request. You can keep using Zerus.".into()
            }
            Some(pending) => format!(
                "A provider request on {} is not yet confirmed. Check its status; it will not be sent again automatically.",
                text(pending, "computer")
            ),
        };
        self.views.set(self.views.get() + 1);
        View {
            seq: self.views.get(),
            hosts: self.hosts.clone(),
            host: self.host.clone(),
            loaded: self.loaded,
            profiles: self
                .profiles
                .iter()
                .map(|profile| ProfileRow {
                    id: text(profile, "id").into(),
                    name: text(profile, "name").into(),
                    kind: if text(profile, "adapter") == "openai_http" {
                        "HTTP server"
                    } else {
                        "Local GigaAM"
                    }
                    .into(),
                    model: text(profile, "model").into(),
                    readiness: if profile.get("available").and_then(Value::as_bool) == Some(true) {
                        "Ready"
                    } else {
                        "Not ready"
                    }
                    .into(),
                    reason: text(profile, "reason").into(),
                })
                .collect(),
            selected: self.selected,
            details: if self.loaded { details } else { String::new() },
            can_add: ready && !self.adapters.is_empty(),
            can_edit: ready && profile.is_some(),
            can_remove: ready && profile.is_some(),
            can_test: ready && profile.is_some(),
            can_use_default: self.loaded && profile.is_some(),
            default_text: self.default_text(),
            status: self.status.clone(),
            result: self.result.clone(),
            result_failed: self.result_failed,
            can_refresh: !self.refreshing,
            pending,
            can_check: !self.busy,
            statistics: self.statistics.clone(),
            editor: self.editor_view(),
        }
    }
}

/// Runs a call as Qt's `SpeechClient::request` does: `hgs [@host] --help`
/// first (remembered per computer, separately for provider management),
/// then the call with its payload on standard input. A refusal before the
/// change was sent says so (`client_dispatch_started: false`). Blocks; run
/// it off the UI thread.
pub fn run(hgs: &Path, host: &str, payload: &Value, supported: &mut HashSet<String>) -> Answer {
    let computer = ComputerId::from(host);
    let manage = voice::manages(payload.get("action").and_then(Value::as_str).unwrap_or(""));
    let refused = |error: String| Answer {
        reply: if manage {
            let mut reply = Map::new();
            reply.insert("client_dispatch_started".into(), Value::Bool(false));
            reply
        } else {
            Map::new()
        },
        error,
    };
    let cache = format!("{}\n{host}", if manage { "manage" } else { "speech" });
    if !supported.contains(&cache) {
        let exit = crate::run::run(hgs, &voice::help_invocation(&computer));
        if let Some(error) = voice::failure(&exit, manage) {
            return refused(error);
        }
        let Exit::Code { stdout, .. } = exit else {
            return refused("The speech computer returned an invalid response.".into());
        };
        if let Err(error) = voice::speech_supported(&String::from_utf8_lossy(&stdout), manage) {
            return refused(error);
        }
        if manage {
            supported.insert(format!("speech\n{host}"));
        }
        supported.insert(cache);
    }
    match voice::reply(
        crate::run::run(hgs, &voice::speech_invocation(&computer, payload)),
        manage,
    ) {
        Ok(reply) => Answer {
            reply,
            error: String::new(),
        },
        Err(error) => Answer::failed(error),
    }
}

#[cfg(test)]
mod tests;
