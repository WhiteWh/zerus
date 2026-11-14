//! The Accounts screen and the Overview account cards: the `account ls`
//! profiles of every host, each with its usage, grouped by the provider's
//! identity (tray/src/AccountCatalog.h), with Qt's member states and texts
//! (tray/src/AccountsPage.cpp) and the Overview's hidden-card rule
//! (tray/src/DashboardPage.cpp:348-350). When the reads run is decided in
//! [`super::schedule`].
use super::usage::{self, Identity, Usage};
use super::{flag, items, number, text, FleetEntry};
use crate::paths::{self, Style};
use crate::text::Text;
use crate::ComputerId;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

/// The most profiles a host's catalog shows.
const PROFILES_PER_HOST: usize = 256;
/// The most machines the account screens list.
const HOSTS: usize = 64;
/// The most accounts one projection carries; the rest is counted in `truncated`.
const ACCOUNTS: usize = 1024;
/// The most notices one projection carries.
const NOTICES: usize = 16;
/// The most OpenCode providers a row lists (OpenCodeModels::providers).
const OPENCODE_PROVIDERS: usize = 64;

/// A profile's usage is kept per host, profile ID and home (AccountUsageRef::key).
pub type UsageKey = (ComputerId, String, String);

/// The profile an inspect reads (AccountUsageRef::profile).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageRef {
    pub host: ComputerId,
    pub id: String,
    pub home: String,
    pub provider: String,
    pub auth_revision: String,
}

impl UsageRef {
    pub fn key(&self) -> UsageKey {
        (self.host.clone(), self.id.clone(), self.home.clone())
    }
}

/// One host's `account ls`.
#[derive(Clone, Debug, Default)]
pub struct CatalogSlot {
    /// When the last `account ls` started, on the hub's millisecond clock.
    pub requested: Option<u64>,
    pub pending: bool,
    /// A forced refresh arrived while one ran: one follow-up runs after it.
    pub queued: bool,
    /// The last good reply; a failure keeps it.
    pub data: Option<Value>,
    /// The last failure, cleared by a good reply.
    pub error: Option<String>,
}

/// One profile's inspect (an AccountUsageStore entry).
#[derive(Clone, Debug, Default)]
pub struct UsageSlot {
    /// The credential revision the data belongs to.
    pub auth_revision: String,
    pub attempted: Option<u64>,
    pub pending: bool,
    pub failed: bool,
    pub data: Option<Value>,
}

/// Everything the account reads produced.
#[derive(Clone, Debug, Default)]
pub struct AccountsState {
    pub catalogs: BTreeMap<ComputerId, CatalogSlot>,
    pub usage: BTreeMap<UsageKey, UsageSlot>,
    pub(crate) queue: VecDeque<UsageRef>,
    pub(crate) running: usize,
    pub(crate) warmed: BTreeSet<ComputerId>,
}

impl AccountsState {
    /// Claims one of the two shared usage-read slots, including a selected
    /// session's inspect. The matching completion is filed through the same hub.
    pub fn claim_usage_slot(&mut self) -> bool {
        if self.running >= 2 {
            return false;
        }
        self.running += 1;
        true
    }
}

/// A profile's state on its machine, as the mockup's machine tags show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemberState {
    Ready,
    Offline,
    NotInstalled,
    Apikey,
    SignedOut,
    Keychain,
    Desktop,
    Unavailable,
    Expired,
    Checking,
    Unknown,
}

/// A machine that keeps agent accounts.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Host {
    pub computer: ComputerId,
    pub name: String,
    /// `"<host> (this machine)"` for this computer (machineChoice, AccountsPage.cpp:178).
    pub label: String,
    pub online: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OpenCodeProvider {
    pub label: String,
    pub connection: String,
    pub models: String,
}

/// OpenCode's native configuration on one machine (AccountsPage.cpp:657-704).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OpenCode {
    pub providers: Vec<OpenCodeProvider>,
    /// "Showing the first N providers."
    pub truncated: bool,
    /// The last known catalog's time when the catalog is not ready ("Last known catalog from ...").
    pub stale_catalog_at: Option<f64>,
    /// Installed but not running ("Start OpenCode on ...").
    pub stopped: bool,
}

/// One profile on one machine.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Profile {
    /// `host + U+001F + id` (AccountCatalog::profileKey).
    pub key: String,
    pub host: ComputerId,
    pub machine: String,
    pub id: String,
    pub label: String,
    pub provider: String,
    pub native: bool,
    pub installed: bool,
    pub is_default: bool,
    pub home: String,
    pub auth_revision: String,
    pub online: bool,
    pub state: MemberState,
    /// The status line under the machine tags.
    pub detail: Text,
    pub signed_in: bool,
    /// Zerus/provider permission mode, including OpenCode's allow/ask/deny/custom.
    pub permission: String,
    /// Zerus override, separate from the provider's effective permission mode.
    pub permission_mode: String,
    /// The mode is the Zerus default, not a choice (owner, 2026-11-07,
    /// T-6hcfnu: bypass for Claude, Codex and Kimi).
    pub permission_default: bool,
    pub permission_label: Text,
    pub permission_hint: Text,
    pub model_supported: bool,
    pub compact_on_expiry_supported: bool,
    pub compact_on_expiry: bool,
    pub model_text: Text,
    /// This computer's model and effort defaults for the account, empty when
    /// it follows the provider (zerus-f0uz8).
    pub model: String,
    pub effort: String,
    /// `account` (shared by the account's computers), `computer`, or empty
    /// when this computer's hgs does not keep a scope.
    pub permission_scope: String,
    /// The account's color and emoji as this computer keeps them, empty when
    /// unset (zerus-sa9qn.6); `marks_supported` is false for an older hgs,
    /// whose marks stay this desktop's preferences.
    pub mark_color: String,
    pub mark_emoji: String,
    pub marks_supported: bool,
    pub usage: Usage,
    pub opencode: Option<OpenCode>,
}

/// One account: the profiles of one identity across machines.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Account {
    pub key: String,
    pub label: String,
    pub provider: String,
    pub installed: bool,
    pub machines: Vec<String>,
    pub members: Vec<Profile>,
    /// The better snapshot of the members (betterUsage).
    pub usage: Usage,
    /// Bypass, Auto, Plan, Default, or Varies when the members differ.
    pub permission: Text,
    /// The account keeps its permission mode per computer: any member says
    /// so (zerus-f0uz8). Otherwise one mode is shared by every computer.
    pub permissions_per_computer: bool,
    /// The members' model and effort defaults differ.
    pub model_differs: bool,
    /// The members' Zerus permission modes differ.
    pub permission_differs: bool,
    /// Identity values verbatim, then "Plan: %1" and "Sign-in: %1".
    pub identity_lines: Vec<Text>,
    /// Shown without identity lines: "Reading account details…" or
    /// "Account identity is unavailable.", else empty.
    pub identity_note: Option<Text>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BindingEvidence {
    pub host: ComputerId,
    pub provider: String,
    pub id: String,
    pub home: String,
    pub group: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AccountsView {
    /// Current and removed profiles disambiguate older home-only session bindings.
    /// They are evidence only, never extra chooser accounts (SessionAccounts.h).
    pub binding_evidence: Vec<BindingEvidence>,
    pub binding_evidence_complete: bool,
    pub catalog_loading: bool,
    pub hosts: Vec<Host>,
    pub accounts: Vec<Account>,
    /// The Overview's cards: the same grouping without the hidden native profiles.
    pub overview: Vec<Account>,
    /// A first snapshot exists ("Loading accounts…" until then).
    pub snapshot: bool,
    /// A shown profile's usage is still loading ("Loading account details…").
    pub loading: bool,
    /// Hosts and profiles still being read ("Updating… N remaining").
    pub pending: usize,
    pub loaded_machines: usize,
    /// A read runs; Refresh is disabled.
    pub busy: bool,
    pub notices: Vec<String>,
    /// Accounts left out beyond the most one projection carries.
    pub truncated: usize,
    /// Why there is no machine to list, when there is none.
    pub no_hosts: Option<String>,
}

/// What the previous projection showed: the rows held back while they
/// resolve, and whether a first snapshot exists.
#[derive(Clone, Debug, Default)]
pub struct AccountsDisplay {
    held: BTreeMap<String, Profile>,
    snapshot: bool,
}

/// AccountsPage::online: this computer unless it is a control console; a
/// peer until its stream failed (Qt counts a peer offline only after a failed poll).
pub fn online(entry: &FleetEntry) -> bool {
    if entry.computer.is_local() {
        return entry.local_sessions;
    }
    !(entry.status == "ended" || (entry.status == "reconnecting" && entry.attempt > 0))
}

/// AccountUsageStore::online: this computer always, a peer as [`online`].
pub fn reachable(entry: &FleetEntry) -> bool {
    entry.computer.is_local() || online(entry)
}

/// This computer keeps accounts only once its card says it runs sessions: before
/// that `local_sessions` is a default, and a control console must not be read.
fn keeps_accounts(entry: &FleetEntry) -> bool {
    !entry.computer.is_local() || (entry.host.is_some() && entry.local_sessions)
}

/// The machines that keep accounts, in fleet order, at most [`HOSTS`].
fn account_fleet(fleet: &[FleetEntry]) -> impl Iterator<Item = &FleetEntry> {
    fleet
        .iter()
        .filter(|entry| keeps_accounts(entry))
        .take(HOSTS)
}

/// The machines that keep agent accounts (accountHosts, AccountsPage.cpp:165-170).
pub fn hosts(fleet: &[FleetEntry]) -> Vec<Host> {
    account_fleet(fleet)
        .map(|entry| Host {
            computer: entry.computer.clone(),
            name: entry.name(),
            label: if entry.computer.is_local() {
                format!("{} (this machine)", entry.name())
            } else {
                entry.name()
            },
            online: online(entry),
        })
        .collect()
}

/// The local Windows computer does not run OpenCode (AccountCatalog::launchable).
/// It runs DeepSeek through the host's named pipe, so New session offers its
/// DeepSeek account. Its
/// folder style is the computer's (`paths::of`, MachinePlatform::of): this
/// computer before it reports a platform is the build's, a peer POSIX.
pub fn launchable(entry: &FleetEntry, provider: &str) -> bool {
    launchable_for(entry, provider, paths::build())
}

/// [`launchable`] with the build platform given, so both are tested anywhere.
fn launchable_for(entry: &FleetEntry, provider: &str, build: Style) -> bool {
    let local = entry.computer.is_local();
    let style = match entry.platform.as_deref() {
        None | Some("") if local => build,
        platform => paths::of(platform, local),
    };
    style == Style::Posix || provider != "opencode"
}

fn usage_ref(host: &ComputerId, raw: &Value) -> UsageRef {
    UsageRef {
        host: host.clone(),
        id: text(raw, "id"),
        home: text(raw, "home"),
        provider: text(raw, "provider"),
        auth_revision: text(raw, "auth_revision"),
    }
}

/// The installed profiles an inspect reads, with whether their host is reachable.
pub fn refs(fleet: &[FleetEntry], state: &AccountsState) -> Vec<(UsageRef, bool)> {
    let mut out = Vec::new();
    for entry in account_fleet(fleet) {
        let Some(data) = state
            .catalogs
            .get(&entry.computer)
            .and_then(|slot| slot.data.as_ref())
        else {
            continue;
        };
        for raw in items(data, "profiles").iter().take(PROFILES_PER_HOST) {
            let r = usage_ref(&entry.computer, raw);
            if flag(raw, "installed") && !r.id.is_empty() && launchable(entry, &r.provider) {
                out.push((r, reachable(entry)));
            }
        }
    }
    out
}

pub fn provider_name(provider: &str) -> String {
    match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        "kimi" => "Kimi",
        "opencode" => "OpenCode",
        "dsh" => "DeepSeek",
        "sh" => "Shell",
        other => other,
    }
    .to_owned()
}

/// AccountUsageStore::data plus the seeding of AccountsPage::profiles.
fn profile_usage(entry: &FleetEntry, raw: &Value, r: &UsageRef, state: &AccountsState) -> Usage {
    let slot = state.usage.get(&r.key());
    // A snapshot of another credential revision is not this profile's any more.
    let data = slot
        .filter(|slot| r.auth_revision.is_empty() || slot.auth_revision == r.auth_revision)
        // Only a real reply counts as a successful inspect, never a stub.
        .and_then(|slot| slot.data.as_ref())
        .filter(|data| data.is_object());
    let reachable = reachable(entry);
    let mut u = match data {
        Some(data) => usage::usage(data),
        None => Usage {
            status: if !reachable {
                "offline"
            } else if slot.is_some_and(|slot| slot.failed) {
                // The first inspect failed: settled, shown from the cache.
                "unavailable"
            } else {
                "loading"
            }
            .to_owned(),
            ..Usage::default()
        },
    };
    if u.provider.is_empty() {
        u.provider = r.provider.clone();
    }
    u.label = text(raw, "label");
    u.refreshing = slot.is_some_and(|slot| slot.pending);
    u.refresh_error = slot.is_some_and(|slot| slot.failed);
    u.offline = !reachable;
    if let Some(cached) = raw
        .get("account_status")
        .filter(|cached| cached.is_object())
    {
        let cached = usage::usage(cached);
        if data.is_none() {
            // Until the first inspect answers, the cached inspection stands in
            // (owner ruling; Qt seeds identity and checked_at only).
            u.identity = cached.identity;
            u.checked_at = cached.checked_at;
            u.windows = cached.windows;
            u.balances = cached.balances;
            u.balance_available = cached.balance_available;
            u.spend = cached.spend;
            u.local_usage = cached.local_usage;
            u.credits = cached.credits;
            u.retry_at = cached.retry_at;
            if cached.status == "rate_limited" {
                u.status = cached.status;
                u.auth_status = cached.auth_status;
            }
            u.seeded = true;
        } else if matches!(u.status.as_str(), "unavailable" | "offline")
            && u.identity == Identity::default()
        {
            u.identity = cached.identity;
            u.checked_at = cached.checked_at;
        }
    }
    u
}

/// The state and status line of a profile (AccountsPage.cpp:637-654).
fn member(
    online: bool,
    installed: bool,
    provider: &str,
    u: &Usage,
    machine: &str,
) -> (MemberState, Text, bool) {
    use MemberState::*;
    let status = u.status.as_str();
    let signed_in = status == "ok" || status == "expired" || u.auth_status == "signed_in";
    let m = || machine.to_owned();
    let (state, detail) = if !online {
        (Offline, crate::text!("%1 is offline.", m()))
    } else if !installed {
        (
            NotInstalled,
            crate::text!("%1 is not installed on %2.", provider_name(provider), m()),
        )
    } else {
        match status {
            "configured" => (Apikey, crate::text!("API key configured on %1.", m())),
            "signed_out" if provider == "dsh" => (SignedOut, crate::text!("Set a DeepSeek API key on %1 to use API models.", m())),
            "signed_out" => (SignedOut, crate::text!("Sign in on %1 to use this account.", m())),
            "credentials_locked" => (Keychain, crate::text!("The login Keychain on %1 is locked. Unlock it in Keychain Access, then refresh.", m())),
            "desktop_session_unavailable" => (Desktop, crate::text!("Desktop sign-in on %1 is unavailable. Log in to macOS and check the HGS installation.", m())),
            "credentials_unavailable" => (Unavailable, crate::text!("The saved sign-in on %1 could not be used. Check Keychain access or sign in again.", m())),
            "expired" => (Expired, crate::text!("Sign-in expired on %1.", m())),
            "loading" => (Checking, crate::text!("Checking sign-in on %1…", m())),
            _ if signed_in => (Ready, crate::text!("Ready on %1.", m())),
            _ => (Unknown, crate::text!("Account status is unavailable on %1.", m())),
        }
    };
    (state, detail, signed_in)
}

/// permissionMode (AccountsPage.cpp:72-74).
fn permission_mode(raw: &Value) -> String {
    if text(raw, "permission_mode") == "bypass" {
        return "bypass".into();
    }
    Some(text(raw, "effective_permission_mode"))
        .filter(|mode| !mode.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// permissionLabel (AccountsPage.cpp:75-78).
pub fn permission_label(mode: &str) -> &'static str {
    match mode {
        "bypass" => crate::source!("Bypass"),
        "auto" => crate::source!("Auto"),
        "plan" => crate::source!("Plan"),
        "mixed" => crate::source!("Varies"),
        "allow" => crate::source!("Allow tools"),
        "ask" => crate::source!("Ask before tools"),
        "deny" => crate::source!("Deny tools"),
        "custom" => crate::source!("Custom rules"),
        "unknown" => crate::source!("Unverified"),
        _ => crate::source!("Default"),
    }
}

/// permissionHint (AccountsPage.cpp:89-92).
fn permission_hint(raw: &Value) -> Text {
    if text(raw, "provider") == "opencode" {
        let detail = match text(raw, "effective_permission_mode").as_str() {
            "allow" if text(raw, "permission_detail") == "OpenCode native default" => {
                crate::text!("OpenCode allows tools by default; reads of .env files may still ask.")
            }
            "allow" => crate::text!("OpenCode account rules allow tools without asking."),
            "ask" => crate::text!("OpenCode account rules ask before tools run."),
            "deny" => crate::text!("OpenCode account rules deny tools."),
            "custom" => crate::text!("OpenCode uses custom permission rules."),
            _ => crate::text!("OpenCode permission configuration could not be verified."),
        };
        return crate::text!(
            "%1\n%2\n%3",
            crate::text!("From provider settings"),
            detail,
            crate::text!("Project, agent and session rules may use a different policy.")
        );
    }
    let source = if zerus_default(raw) {
        // Bypass for Claude, Codex and Kimi without a choice (T-6hcfnu).
        crate::text!("Zerus default")
    } else if text(raw, "permission_mode") == "bypass" {
        crate::text!("Set in Zerus")
    } else {
        crate::text!("From provider settings")
    };
    // The provider's own detail is shown as it reports it; the default says
    // what it is.
    let detail = if zerus_default(raw) {
        crate::text!("Without a choice, Claude, Codex and Kimi accounts bypass permissions.")
    } else {
        Some(text(raw, "permission_detail"))
            .filter(|detail| !detail.is_empty())
            .map_or_else(
                || crate::text!("Provider defaults have not been verified."),
                Text::verbatim,
            )
    };
    crate::text!(
        "%1\n%2\n%3",
        source,
        detail,
        crate::text!(
            "Applies to new and resumed sessions; a running session may use a different mode."
        )
    )
}

fn zerus_default(raw: &Value) -> bool {
    text(raw, "permission_mode_source") == "default" && text(raw, "permission_mode") == "bypass"
}

/// The model line (AccountsPage.cpp:610-612), with the effort in parentheses (the dots rule).
fn model_text(raw: &Value) -> Text {
    let defaults = raw.get("model_defaults").unwrap_or(&Value::Null);
    let (model, effort) = (text(defaults, "model"), text(defaults, "effort"));
    match (model.is_empty(), effort.is_empty()) {
        (true, _) => crate::text!("New sessions: Provider settings"),
        // Qt's "New sessions: %1%2", the effort in parentheses as its suffix.
        (false, true) => crate::text!("New sessions: %1%2", model, String::new()),
        (false, false) => crate::text!("New sessions: %1%2", model, format!(" ({effort})")),
    }
}

/// OpenCodeModels::kindText (OpenCodeModels.cpp:115-130).
fn kind_text(kind: &str) -> &'static str {
    match kind {
        "free" => "Free, no key",
        "key" => "API key",
        "oauth" => "Signed in",
        "env" => "Environment variable",
        "config" => "Configuration",
        "command" => "Command",
        "external" => "External credentials",
        "local" => "Detected locally",
        _ => "Connected",
    }
}

fn short(value: &Value, key: &str, limit: usize) -> String {
    text(value, key)
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect()
}

/// OpenCode's providers (OpenCodeModels::providers, OpenCodeModels.cpp:64-80):
/// distinct valid IDs, at most 64; a provider ID here is 1 to 128 bytes
/// without spaces or control characters.
fn opencode(raw: &Value) -> Option<OpenCode> {
    if text(raw, "provider") != "opencode" {
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut providers = Vec::new();
    for row in items(raw, "providers") {
        if providers.len() >= OPENCODE_PROVIDERS {
            break;
        }
        let id = text(row, "id");
        if id.is_empty()
            || id.len() > 128
            || id.chars().any(|c| c.is_control() || c.is_whitespace())
            || !seen.insert(id.clone())
        {
            continue;
        }
        let label = Some(short(row, "label", 160))
            .filter(|label| !label.is_empty())
            .unwrap_or(id);
        let active = short(row, "active_label", 160);
        let credentials = row
            .get("credential_count")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        let mut connection = kind_text(&short(row, "kind", 32)).to_owned();
        if !active.is_empty() {
            connection.push_str(&format!(", {active}"));
        } else if credentials > 1 {
            connection.push_str(&format!(", {credentials} accounts"));
        }
        let models = match row.get("model_count").and_then(Value::as_i64).unwrap_or(-1) {
            count if count < 0 => String::new(),
            1 => "1 model".into(),
            count => format!("{count} models"),
        };
        providers.push(OpenCodeProvider {
            label,
            connection,
            models,
        });
    }
    let state = text(raw, "catalog_state");
    let at = number(raw, "catalog_at").filter(|at| *at > 0.0 && *at < 253_402_300_800.0);
    Some(OpenCode {
        providers,
        truncated: flag(raw, "providers_truncated"),
        stale_catalog_at: if state != "ready" { at } else { None },
        stopped: flag(raw, "installed") && state == "not_running",
    })
}

fn profile(entry: &FleetEntry, raw: &Value, state: &AccountsState) -> Profile {
    let r = usage_ref(&entry.computer, raw);
    let machine = entry.name();
    let installed = flag(raw, "installed");
    let online = online(entry);
    let usage = profile_usage(entry, raw, &r, state);
    let (member_state, mut detail, signed_in) =
        member(online, installed, &r.provider, &usage, &machine);
    if r.provider == "opencode" {
        // OpenCode keeps its own configuration (AccountsPage.cpp:657-663).
        let catalog = text(raw, "catalog_state");
        let reason = if !installed {
            text(raw, "install_reason")
        } else if !catalog.is_empty() && catalog != "ready" {
            text(raw, "catalog_reason")
        } else {
            String::new()
        };
        detail = if !online {
            crate::text!("%1 is offline.", machine.clone())
        } else if !reason.is_empty() {
            Text::verbatim(reason)
        } else if !installed {
            crate::text!("%1 is not installed on %2.", "OpenCode", machine.clone())
        } else {
            crate::text!("Configure providers, models and permissions with OpenCode on %1. Zerus uses that native configuration.", machine.clone())
        };
    }
    let permission = permission_mode(raw);
    Profile {
        key: format!("{}\u{1f}{}", entry.computer.0, r.id),
        host: entry.computer.clone(),
        machine,
        label: text(raw, "label"),
        native: flag(raw, "native"),
        installed,
        is_default: flag(raw, "is_default"),
        online,
        state: member_state,
        detail,
        signed_in,
        permission_label: Text::new(permission_label(&permission)),
        permission_hint: permission_hint(raw),
        permission,
        permission_mode: if text(raw, "permission_mode") == "bypass" {
            "bypass"
        } else {
            "provider"
        }
        .into(),
        permission_default: zerus_default(raw),
        model_supported: flag(raw, "model_defaults_supported"),
        compact_on_expiry_supported: flag(raw, "compact_on_expiry_supported"),
        compact_on_expiry: raw["compact_on_expiry"].as_bool().unwrap_or(true),
        model_text: model_text(raw),
        model: text(raw.get("model_defaults").unwrap_or(&Value::Null), "model"),
        effort: text(raw.get("model_defaults").unwrap_or(&Value::Null), "effort"),
        permission_scope: match text(raw, "permission_scope").as_str() {
            scope @ ("account" | "computer") => scope.to_owned(),
            _ => String::new(),
        },
        mark_color: raw
            .get("mark")
            .map(|mark| text(mark, "color"))
            .filter(|color| crate::account_actions::mark_color(color))
            .unwrap_or_default(),
        mark_emoji: raw
            .get("mark")
            .map(|mark| text(mark, "emoji"))
            .filter(|emoji| crate::account_actions::mark_emoji(emoji))
            .unwrap_or_default(),
        marks_supported: raw.get("mark").is_some_and(Value::is_object),
        usage,
        opencode: opencode(raw),
        id: r.id,
        home: r.home,
        provider: r.provider,
        auth_revision: r.auth_revision,
    }
}

/// AccountUsageStore::data, then NewSessionDialog::cachedUsage (971-978):
/// the profile's inspect, or `loading` (`offline` away from its computer)
/// before one answers; the catalog's cached `account_status` stands in while
/// that loads or is offline, or when it was checked later. Unlike the
/// Accounts page, a failed first inspect stays `loading` with its error.
fn launch_usage(entry: &FleetEntry, raw: &Value, r: &UsageRef, state: &AccountsState) -> Usage {
    let slot = state.usage.get(&r.key());
    let data = slot
        .filter(|slot| r.auth_revision.is_empty() || slot.auth_revision == r.auth_revision)
        .and_then(|slot| slot.data.as_ref())
        .filter(|data| data.as_object().is_some_and(|data| !data.is_empty()));
    let reachable = reachable(entry);
    let mut u = match data {
        Some(data) => usage::usage(data),
        None => Usage {
            status: if reachable { "loading" } else { "offline" }.to_owned(),
            provider: r.provider.clone(),
            ..Usage::default()
        },
    };
    let label = text(raw, "label");
    if !label.is_empty() {
        u.label = label;
    }
    u.refreshing = slot.is_some_and(|slot| slot.pending);
    u.refresh_error = slot.is_some_and(|slot| slot.failed);
    u.offline = !reachable;
    let cached = raw
        .get("account_status")
        .filter(|cached| cached.as_object().is_some_and(|cached| !cached.is_empty()));
    if let Some(cached) = cached {
        let cached = usage::usage(cached);
        if u.status == "loading"
            || u.status == "offline"
            || cached.checked_at.unwrap_or(0.0) > u.checked_at.unwrap_or(0.0)
        {
            u = Usage {
                seeded: true,
                ..cached
            };
        }
    }
    u
}

/// One computer's launchable profiles for New session, each with its raw
/// `account ls` row (NewSessionDialog::updateAccounts 883-896): `agents` in
/// their order, then any other provider the catalog lists, skipping the
/// shell, rows without an ID and what the computer does not start
/// ([`launchable`]); at most 256 rows of the catalog. Each carries the
/// dialog's usage (cachedUsage), so [`group`] gives its account rows.
pub fn host_profiles(
    entry: &FleetEntry,
    catalog: &Value,
    state: &AccountsState,
    agents: &[&str],
) -> Vec<(Profile, Value)> {
    let rows: Vec<&Value> = items(catalog, "profiles")
        .iter()
        .take(PROFILES_PER_HOST)
        .collect();
    let mut providers: Vec<String> = agents
        .iter()
        .filter(|provider| launchable(entry, provider))
        .map(|provider| (*provider).to_owned())
        .collect();
    for raw in &rows {
        let provider = text(raw, "provider");
        if !provider.is_empty()
            && provider != "sh"
            && !providers.contains(&provider)
            && launchable(entry, &provider)
        {
            providers.push(provider);
        }
    }
    let mut out = Vec::new();
    for provider in &providers {
        for raw in &rows {
            if text(raw, "provider") != *provider || text(raw, "id").is_empty() {
                continue;
            }
            let mut p = profile(entry, raw, state);
            p.usage = launch_usage(entry, raw, &usage_ref(&entry.computer, raw), state);
            out.push((p, (*raw).clone()));
        }
    }
    out
}

/// AccountCatalog::named.
fn named(label: &str) -> bool {
    !label.is_empty() && label != "Default account" && label != "Native DeepSeek account"
}

/// AccountCatalog::key, with or without the account ID.
fn identity_key(p: &Profile, with_account: bool) -> String {
    let identity = &p.usage.identity;
    let email = identity.email.trim().to_lowercase();
    let account = if with_account {
        identity.account_id.as_str()
    } else {
        ""
    };
    if p.usage.status == "signed_out" || (email.is_empty() && account.is_empty()) {
        return format!("profile:{}", p.key);
    }
    let who = if account.is_empty() {
        format!("email:{email}")
    } else {
        format!("id:{account}")
    };
    format!(
        "account:{}\n{who}\n{}",
        p.provider,
        identity.organization.trim().to_lowercase()
    )
}

fn quality(u: &Usage) -> u8 {
    if u.status == "rate_limited" && !u.windows.is_empty() {
        return if u.offline { 2 } else { 3 };
    }
    match u.status.as_str() {
        "ok" | "configured" if u.offline => 2,
        "ok" | "configured" => 3,
        _ => 1,
    }
}

/// AccountCatalog::betterUsage: a quota belongs to the account; pick a snapshot, never add them up.
fn better(usage: &Usage, previous: &Usage) -> bool {
    quality(usage) > quality(previous)
        || (quality(usage) == quality(previous)
            && (usage.checked_at.unwrap_or(0.0) > previous.checked_at.unwrap_or(0.0)
                || (usage.checked_at == previous.checked_at
                    && usage.retry_at.unwrap_or(0.0) > previous.retry_at.unwrap_or(0.0))))
}

/// accountPermission (AccountsPage.cpp:80-88).
fn account_permission(members: &[Profile]) -> String {
    let mut mode = String::new();
    for member in members {
        if !mode.is_empty() && mode != member.permission {
            return "mixed".into();
        }
        mode = member.permission.clone();
    }
    mode
}

/// The identity block (AccountsPage.cpp:625-636).
fn identity_lines(title: &str, u: &Usage) -> (Vec<Text>, Option<Text>) {
    let identity = &u.identity;
    let mut lines = Vec::new();
    let mut seen = vec![title.trim().to_lowercase()];
    for field in [&identity.name, &identity.email, &identity.organization] {
        let text = field.trim();
        if !text.is_empty() && !seen.contains(&text.to_lowercase()) {
            lines.push(Text::verbatim(text));
            seen.push(text.to_lowercase());
        }
    }
    if !identity.plan.is_empty() {
        lines.push(crate::text!("Plan: %1", identity.plan.clone()));
    }
    if !identity.auth_method.is_empty() && identity.auth_method != "none" {
        lines.push(crate::text!("Sign-in: %1", identity.auth_method.clone()));
    }
    let note = if !lines.is_empty() || *identity != Identity::default() {
        None
    } else if u.status == "loading" {
        Some(crate::text!("Reading account details…"))
    } else {
        Some(crate::text!("Account identity is unavailable."))
    };
    (lines, note)
}

/// AccountCatalog::group (AccountCatalog.h:37-79).
pub fn group(profiles: &[Profile]) -> Vec<Account> {
    // An email-only profile can join a known ID only when that email and
    // scope resolve to one account.
    let mut known: HashMap<String, BTreeSet<String>> = HashMap::new();
    for p in profiles {
        let identity = &p.usage.identity;
        if identity.account_id.is_empty()
            || identity.email.is_empty()
            || p.usage.status == "signed_out"
        {
            continue;
        }
        known
            .entry(identity_key(p, false))
            .or_default()
            .insert(identity_key(p, true));
    }
    let mut groups: Vec<Account> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    for p in profiles {
        let mut key = identity_key(p, true);
        if let Some(only) = known
            .get(&key)
            .filter(|ids| ids.len() == 1)
            .and_then(|ids| ids.iter().next())
        {
            key = only.clone();
        }
        let at = *positions.entry(key.clone()).or_insert_with(|| {
            groups.push(Account {
                key: key.clone(),
                label: p.label.clone(),
                provider: p.provider.clone(),
                installed: false,
                machines: Vec::new(),
                members: Vec::new(),
                usage: p.usage.clone(),
                permission: Text::default(),
                permissions_per_computer: false,
                model_differs: false,
                permission_differs: false,
                identity_lines: Vec::new(),
                identity_note: None,
            });
            groups.len() - 1
        });
        let group = &mut groups[at];
        if !group.machines.contains(&p.machine) {
            group.machines.push(p.machine.clone());
        }
        if better(&p.usage, &group.usage) {
            group.usage = p.usage.clone();
        }
        if !named(&group.label) && named(&p.label) {
            group.label = p.label.clone();
        }
        group.installed |= p.installed;
        group.members.push(p.clone());
    }
    for group in &mut groups {
        if !named(&group.label) && !group.usage.identity.email.is_empty() {
            group.label = group.usage.identity.email.clone();
        }
        group.permission = Text::new(permission_label(&account_permission(&group.members)));
        (
            group.permissions_per_computer,
            group.model_differs,
            group.permission_differs,
        ) = shared_settings(&group.members);
        (group.identity_lines, group.identity_note) = identity_lines(&group.label, &group.usage);
    }
    groups
}

/// Whether the account keeps permissions per computer, and whether its
/// members' model defaults or Zerus permission modes differ (zerus-f0uz8).
fn shared_settings(members: &[Profile]) -> (bool, bool, bool) {
    let models: BTreeSet<(&str, &str)> = members
        .iter()
        .filter(|m| m.model_supported)
        .map(|m| (m.model.as_str(), m.effort.as_str()))
        .collect();
    let modes: BTreeSet<&str> = members
        .iter()
        .filter(|m| m.provider != "opencode")
        .map(|m| m.permission_mode.as_str())
        .collect();
    (
        members.iter().any(|m| m.permission_scope == "computer"),
        models.len() > 1,
        modes.len() > 1,
    )
}

/// NewSessionDialog::refreshUsage (990-1000): the groups of an earlier
/// [`group`] with their members' current projections (by profile key) and
/// each group's best usage of them; labels, keys and membership stay, even
/// when the new usage would join two groups (the next catalog does). A member
/// that is gone leaves its group, and an empty group goes.
pub fn refresh_groups(groups: &[Account], profiles: &[Profile]) -> Vec<Account> {
    let current: HashMap<&str, &Profile> = profiles.iter().map(|p| (p.key.as_str(), p)).collect();
    let mut out = Vec::new();
    for old in groups {
        let members: Vec<Profile> = old
            .members
            .iter()
            .filter_map(|member| current.get(member.key.as_str()).map(|p| (*p).clone()))
            .collect();
        let Some(first) = members.first() else {
            continue;
        };
        let mut usage = first.usage.clone();
        let mut machines: Vec<String> = Vec::new();
        for member in &members {
            if better(&member.usage, &usage) {
                usage = member.usage.clone();
            }
            if !machines.contains(&member.machine) {
                machines.push(member.machine.clone());
            }
        }
        let (identity_lines, identity_note) = identity_lines(&old.label, &usage);
        let (permissions_per_computer, model_differs, permission_differs) =
            shared_settings(&members);
        out.push(Account {
            key: old.key.clone(),
            label: old.label.clone(),
            provider: old.provider.clone(),
            installed: members.iter().any(|m| m.installed),
            machines,
            permission: Text::new(permission_label(&account_permission(&members))),
            permissions_per_computer,
            model_differs,
            permission_differs,
            usage,
            identity_lines,
            identity_note,
            members,
        });
    }
    out
}

/// A native profile awaiting its identity is not another account on the
/// Overview (DashboardPage.cpp:348-350).
fn hidden_on_overview(p: &Profile) -> bool {
    p.native
        && !named(&p.label)
        && identity_key(p, true).starts_with("profile:")
        && matches!(
            p.usage.status.as_str(),
            "loading" | "unavailable" | "offline"
        )
}

fn resolving(p: &Profile) -> bool {
    p.installed && p.online && (p.usage.refreshing || p.usage.status == "loading")
}

/// The Accounts screen and the Overview cards from the fleet and the reads so far.
pub fn project(
    fleet: &[FleetEntry],
    state: &AccountsState,
    display: &mut AccountsDisplay,
) -> AccountsView {
    let hosts = hosts(fleet);
    let mut all = Vec::new();
    let mut evidence = Vec::new();
    let mut evidence_complete = true;
    for entry in account_fleet(fleet) {
        let Some(data) = state
            .catalogs
            .get(&entry.computer)
            .and_then(|slot| slot.data.as_ref())
        else {
            continue;
        };
        evidence_complete &= data.get("removed_profiles").is_some_and(Value::is_array)
            && ["profiles", "removed_profiles"]
                .iter()
                .all(|field| items(data, field).len() <= PROFILES_PER_HOST);
        for raw in items(data, "profiles").iter().take(PROFILES_PER_HOST) {
            if launchable(entry, &text(raw, "provider")) {
                let p = profile(entry, raw, state);
                evidence.push(p.clone());
                all.push(p);
            }
        }
        for raw in items(data, "removed_profiles")
            .iter()
            .take(PROFILES_PER_HOST)
        {
            if launchable(entry, &text(raw, "provider")) {
                evidence.push(profile(entry, raw, state));
            }
        }
    }
    // pendingAccounts (AccountsPage.cpp:419-433).
    let mut pending = hosts
        .iter()
        .filter(|host| {
            state
                .catalogs
                .get(&host.computer)
                .is_none_or(|slot| slot.requested.is_none() || slot.pending)
        })
        .count();
    pending += all.iter().filter(|p| resolving(p)).count();
    // This computer's card has not arrived: it is a host still to come, so the
    // first snapshot waits and the screen does not claim there are no hosts.
    let awaiting_card = fleet
        .iter()
        .any(|entry| entry.computer.is_local() && entry.host.is_none());
    pending += usize::from(awaiting_card);
    // Identities appear as they become known, without flashing unresolved
    // duplicates while another machine is still being inspected.
    let shown: Vec<Profile> = all
        .into_iter()
        .filter_map(|p| {
            let identified = !p.usage.identity.account_id.is_empty()
                || !p.usage.identity.email.trim().is_empty();
            if !resolving(&p) || identified {
                Some(p)
            } else {
                display.held.get(&p.key).cloned()
            }
        })
        .collect();
    if !awaiting_card && (!shown.is_empty() || pending == 0) {
        display.snapshot = true;
    }
    display.held = shown.iter().map(|p| (p.key.clone(), p.clone())).collect();
    let loaded: BTreeSet<&ComputerId> = shown.iter().map(|p| &p.host).collect();
    let visible: Vec<Profile> = shown
        .iter()
        .filter(|p| !hidden_on_overview(p))
        .cloned()
        .collect();
    let notices = hosts
        .iter()
        .filter_map(|host| {
            let error = state.catalogs.get(&host.computer)?.error.as_ref()?;
            let name = if host.computer.is_local() {
                "This machine".to_owned()
            } else {
                host.name.clone()
            };
            Some(format!("{name}: {error}"))
        })
        .take(NOTICES)
        .collect();
    let console = fleet.iter().find(|entry| entry.computer.is_local());
    let can_enable = console.is_some_and(|l| {
        !l.local_sessions && l.local_host && l.platform.as_deref() == Some("windows")
    });
    let no_hosts = (hosts.is_empty() && !awaiting_card).then(|| {
        if can_enable {
            "To manage agent accounts, enable local sessions for this computer in Machines or add a remote machine."
        } else {
            "Add a remote machine in Machines to manage agent accounts; this computer runs no agents."
        }
        .to_owned()
    });
    let mut accounts = group(&shown);
    let truncated = accounts.len().saturating_sub(ACCOUNTS);
    accounts.truncate(ACCOUNTS);
    let mut overview = group(&visible);
    overview.truncate(ACCOUNTS);
    AccountsView {
        catalog_loading: hosts
            .iter()
            .any(|h| state.catalogs.get(&h.computer).is_some_and(|s| s.pending)),
        binding_evidence_complete: evidence_complete,
        binding_evidence: group(&evidence)
            .into_iter()
            .flat_map(|a| {
                a.members.into_iter().map(move |p| BindingEvidence {
                    host: p.host,
                    provider: p.provider,
                    id: p.id,
                    home: p.home,
                    group: a.key.clone(),
                })
            })
            .collect(),
        loading: shown.iter().any(|p| p.usage.status == "loading"),
        busy: pending > 0 || state.catalogs.values().any(|slot| slot.pending),
        accounts,
        overview,
        truncated,
        snapshot: display.snapshot,
        pending,
        loaded_machines: loaded.len(),
        notices,
        no_hosts,
        hosts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::fixture;
    use serde_json::json;

    fn entry(computer: &str, status: &str, attempt: u32) -> FleetEntry {
        FleetEntry {
            computer: ComputerId::from(computer),
            status: status.into(),
            attempt,
            local_sessions: true,
            local_host: false,
            host: computer.is_empty().then(|| "workstation".to_owned()),
            platform: Some("linux".into()),
        }
    }

    fn loaded() -> AccountsState {
        let mut state = AccountsState::default();
        for (computer, file) in [
            ("", "accounts-local.json"),
            ("studio", "accounts-studio.json"),
        ] {
            state.catalogs.insert(
                ComputerId::from(computer),
                CatalogSlot {
                    requested: Some(0),
                    data: Some(fixture(file)),
                    ..CatalogSlot::default()
                },
            );
        }
        state
    }

    #[test]
    fn the_zerus_default_permission_mode_says_so() {
        // Owner, 2026-11-07 (T-6hcfnu): bypass without a choice.
        let mut state = with_inspects();
        let data = state
            .catalogs
            .get_mut(&ComputerId::from(""))
            .unwrap()
            .data
            .as_mut()
            .unwrap();
        let rows = data["profiles"].as_array_mut().unwrap();
        rows[0]["permission_mode"] = json!("bypass");
        rows[0]["permission_mode_source"] = json!("default");
        rows[1]["permission_mode"] = json!("provider");
        rows[1]["permission_mode_source"] = json!("zerus");
        let (first, second) = (rows[0]["id"].clone(), rows[1]["id"].clone());
        let view = project(
            &[entry("", "live", 0)],
            &state,
            &mut AccountsDisplay::default(),
        );
        let member = |id: &Value| {
            view.accounts
                .iter()
                .flat_map(|a| &a.members)
                .find(|m| json!(m.id) == *id)
                .unwrap()
                .clone()
        };
        let default = member(&first);
        assert!(default.permission_default);
        assert!(default
            .permission_hint
            .english()
            .starts_with("Zerus default\nWithout a choice"));
        assert!(!member(&second).permission_default);
    }

    #[test]
    fn removed_profiles_are_binding_evidence_only_and_never_new_chooser_accounts() {
        let mut state = loaded();
        let data = state
            .catalogs
            .get_mut(&ComputerId::from(""))
            .unwrap()
            .data
            .as_mut()
            .unwrap();
        data["removed_profiles"] = json!([{"id":"retired", "provider":"claude", "home":"/srv/example/reused", "label":"Old", "installed":false}]);
        let view = project(
            &[entry("", "live", 0)],
            &state,
            &mut AccountsDisplay::default(),
        );
        assert!(view
            .binding_evidence
            .iter()
            .any(|e| e.id == "retired" && e.home == "/srv/example/reused"));
        assert!(view
            .accounts
            .iter()
            .all(|a| a.members.iter().all(|p| p.id != "retired")));
        assert!(view.binding_evidence.len() <= HOSTS * PROFILES_PER_HOST * 2);
        assert!(view.binding_evidence_complete);
        state
            .catalogs
            .get_mut(&ComputerId::from(""))
            .unwrap()
            .data
            .as_mut()
            .unwrap()["removed_profiles"] = json!(vec![
            json!({"id":"old", "provider":"claude", "home":"/srv/example/reused"});
            PROFILES_PER_HOST + 1
        ]);
        let partial = project(
            &[entry("", "live", 0)],
            &state,
            &mut AccountsDisplay::default(),
        );
        assert!(
            !partial.binding_evidence_complete,
            "a capped history must never prove a home alias"
        );
    }

    fn inspected(
        state: &mut AccountsState,
        host: &str,
        id: &str,
        home: &str,
        revision: &str,
        reply: Value,
    ) {
        state.usage.insert(
            (ComputerId::from(host), id.into(), home.into()),
            UsageSlot {
                auth_revision: revision.into(),
                data: Some(reply),
                ..UsageSlot::default()
            },
        );
    }

    fn signed_out() -> Value {
        json!({"id": "native-claude", "home": "/home/example/.claude", "provider": "claude", "status": "signed_out",
           "identity": {"email": "person@example.com", "account_id": "acct-1"}, "windows": [], "balances": []})
    }

    /// Every installed profile has answered except claude-studio, which shows its cached status.
    fn with_inspects() -> AccountsState {
        let mut state = loaded();
        inspected(
            &mut state,
            "",
            "claude-work",
            "/home/example/.config/hgs/accounts/claude-work",
            "a1",
            fixture("inspect-claude-work.json"),
        );
        inspected(
            &mut state,
            "",
            "native-claude",
            "/home/example/.claude",
            "b1",
            signed_out(),
        );
        inspected(
            &mut state,
            "",
            "native-codex",
            "/home/example/.codex",
            "c1",
            fixture("inspect-native-codex.json"),
        );
        inspected(
            &mut state,
            "studio",
            "kimi-studio",
            "/home/example/.config/hgs/accounts/kimi-studio",
            "k1",
            json!({"id": "kimi-studio", "home": "/home/example/.config/hgs/accounts/kimi-studio", "provider": "kimi", "status": "ok",
               "identity": {"plan": "Pro"}, "windows": [{"id": "limit_month_total", "label": "Monthly", "used_percent": 50,
               "window_minutes": null, "resets_at": null}], "balances": []}),
        );
        state
    }

    // Review Focus 2.
    #[test]
    fn one_identity_on_two_hosts_is_one_account_and_signed_out_or_unidentified_profiles_stay_apart()
    {
        let fleet = [entry("", "live", 0), entry("studio", "live", 0)];
        let view = project(&fleet, &with_inspects(), &mut AccountsDisplay::default());
        let groups: Vec<(&str, Vec<&str>)> = view
            .accounts
            .iter()
            .map(|a| {
                (
                    a.label.as_str(),
                    a.members.iter().map(|m| m.id.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            groups,
            vec![
                ("Work", vec!["claude-work", "claude-studio"]),
                ("person@example.com", vec!["native-claude"]),
                ("Default account", vec!["native-codex"]),
                ("Native DeepSeek account", vec!["native-dsh"]),
                ("Moonshot", vec!["kimi-studio"]),
            ]
        );
        let work = &view.accounts[0];
        assert_eq!(work.machines, ["workstation", "studio"]);
        assert_eq!(work.permission.english(), "Varies");
        assert_eq!(
            work.usage.checked_at,
            Some(1793000300.0),
            "the answered inspect is the better snapshot"
        );
        assert_eq!(
            work.identity_lines
                .iter()
                .map(Text::english)
                .collect::<Vec<_>>(),
            [
                "Example Person",
                "person@example.com",
                "Plan: max",
                "Sign-in: oauth"
            ]
        );
        assert_eq!(
            view.accounts[2]
                .identity_note
                .as_ref()
                .map(Text::english)
                .as_deref(),
            Some("Account identity is unavailable.")
        );
        assert_eq!(
            view.accounts[3]
                .identity_note
                .as_ref()
                .map(Text::english)
                .as_deref(),
            Some("Reading account details…")
        );
        // The Overview leaves out a native, unnamed profile while it loads (native-dsh).
        let overview: Vec<&str> = view.overview.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(
            overview,
            ["Work", "person@example.com", "Default account", "Moonshot"]
        );
    }

    #[test]
    fn member_states_and_texts_follow_qt() {
        use MemberState::*;
        let member = |view: &AccountsView, id: &str| -> Profile {
            view.accounts
                .iter()
                .flat_map(|a| &a.members)
                .find(|m| m.id == id)
                .unwrap()
                .clone()
        };
        let state = |view: &AccountsView, id: &str| {
            let m = member(view, id);
            (m.state, m.detail.english())
        };
        let fleet = [entry("", "live", 0), entry("studio", "live", 0)];
        let view = project(&fleet, &with_inspects(), &mut AccountsDisplay::default());
        assert_eq!(
            state(&view, "claude-work"),
            (Ready, "Ready on workstation.".into())
        );
        assert_eq!(
            state(&view, "native-claude"),
            (
                SignedOut,
                "Sign in on workstation to use this account.".into()
            )
        );
        assert_eq!(
            state(&view, "native-codex"),
            (
                SignedOut,
                "Sign in on workstation to use this account.".into()
            )
        );
        assert_eq!(
            state(&view, "native-dsh"),
            (
                NotInstalled,
                "DeepSeek is not installed on workstation.".into()
            )
        );
        assert_eq!(
            state(&view, "claude-studio"),
            (Checking, "Checking sign-in on studio…".into())
        );
        assert_eq!(member(&view, "kimi-studio").state, Ready);
        assert_eq!(
            member(&view, "native-codex").permission_label.english(),
            "Bypass"
        );
        assert_eq!(
            member(&view, "claude-work").model_text.english(),
            "New sessions: opus (high)"
        );
        assert_eq!(
            member(&view, "claude-studio").model_text.english(),
            "New sessions: Provider settings"
        );
        assert_eq!(member(&view, "claude-work").permission_hint.english(),
        "From provider settings\nFrom ~/.claude/settings.json\nApplies to new and resumed sessions; a running session may use a different mode.");
        // studio's stream failed: its profiles are offline, and the cached inspection
        // stands in until the first inspect answers (ruling).
        let away = [entry("", "live", 0), entry("studio", "reconnecting", 1)];
        let view = project(&away, &with_inspects(), &mut AccountsDisplay::default());
        let studio = member(&view, "claude-studio");
        assert_eq!(
            (
                studio.state,
                studio.detail.english().as_str(),
                studio.online
            ),
            (Offline, "studio is offline.", false)
        );
        assert!(studio.usage.offline);
        assert_eq!(studio.usage.status, "offline");
        assert_eq!(
            studio
                .usage
                .windows
                .iter()
                .map(|w| w.resets_at)
                .collect::<Vec<_>>(),
            [Some(1793005200.0), Some(1793400000.0)]
        );
        assert_eq!(studio.usage.checked_at, Some(1793000100.0));
        assert!(
            studio.usage.seeded,
            "the cached inspection is marked until inspect answers"
        );
        assert!(
            !member(&view, "claude-work").usage.seeded,
            "an answered inspect is not seeded"
        );
        assert!(
            member(&view, "kimi-studio").usage.offline
                && member(&view, "kimi-studio").state == Offline
        );
        // Qt's lenient rule: a peer is offline only after its stream failed.
        for (status, attempt, up) in [
            ("reconnecting", 0, true),
            ("probing", 0, true),
            ("unsupported", 0, true),
            ("polled", 0, true),
            ("live", 0, true),
            ("reconnecting", 2, false),
            ("ended", 0, false),
        ] {
            assert_eq!(
                online(&entry("studio", status, attempt)),
                up,
                "{status} {attempt}"
            );
        }
        let mut console = entry("", "live", 0);
        console.local_sessions = false;
        assert!(!online(&console) && reachable(&console));
    }

    #[test]
    fn a_control_console_has_no_account_hosts_of_its_own() {
        let mut console = entry("", "live", 0);
        console.local_sessions = false;
        assert!(hosts(std::slice::from_ref(&console)).is_empty());
        let view = project(
            std::slice::from_ref(&console),
            &AccountsState::default(),
            &mut AccountsDisplay::default(),
        );
        assert_eq!(view.no_hosts.as_deref(), Some("Add a remote machine in Machines to manage agent accounts; this computer runs no agents."));
        console.local_host = true;
        console.platform = Some("windows".into());
        let view = project(
            &[console],
            &AccountsState::default(),
            &mut AccountsDisplay::default(),
        );
        assert_eq!(view.no_hosts.as_deref(),
        Some("To manage agent accounts, enable local sessions for this computer in Machines or add a remote machine."));
        let labels: Vec<String> = hosts(&[entry("", "live", 0), entry("studio", "live", 0)])
            .into_iter()
            .map(|h| h.label)
            .collect();
        assert_eq!(labels, ["workstation (this machine)", "studio"]);
        // The local Windows computer lists DeepSeek and never OpenCode.
        let mut windows = entry("", "live", 0);
        windows.platform = Some("windows".into());
        let view = project(&[windows], &loaded(), &mut AccountsDisplay::default());
        assert!(view.accounts.iter().any(|a| a.provider == "dsh"));
        assert!(view.accounts.iter().all(|a| a.provider != "opencode"));
    }

    #[test]
    fn pending_counts_unread_hosts_and_resolving_profiles_and_the_display_holds_them_back() {
        let fleet = [entry("", "live", 0), entry("studio", "live", 0)];
        let mut state = loaded();
        state.catalogs.remove(&ComputerId::from("studio"));
        state.catalogs.get_mut(&ComputerId::local()).unwrap().error =
            Some("hgs did not answer in time".into());
        let mut display = AccountsDisplay::default();
        let ids = |view: &AccountsView| -> Vec<String> {
            view.accounts
                .iter()
                .flat_map(|a| a.members.iter().map(|m| m.id.clone()))
                .collect()
        };
        let view = project(&fleet, &state, &mut display);
        // studio never answered; claude-work, native-claude and native-codex are loading.
        assert_eq!(view.pending, 4);
        assert!(view.busy && view.loading);
        assert_eq!(
            ids(&view),
            ["claude-work", "native-dsh"],
            "unidentified profiles wait while they resolve"
        );
        assert!(view.snapshot);
        assert_eq!(view.loaded_machines, 1);
        assert_eq!(view.notices, ["This machine: hgs did not answer in time"]);
        inspected(
            &mut state,
            "",
            "native-claude",
            "/home/example/.claude",
            "b1",
            signed_out(),
        );
        assert_eq!(
            ids(&project(&fleet, &state, &mut display)),
            ["claude-work", "native-claude", "native-dsh"]
        );
        // A refresh without data yet: the previous row stays.
        let slot = state
            .usage
            .get_mut(&(
                ComputerId::local(),
                "native-claude".into(),
                "/home/example/.claude".into(),
            ))
            .unwrap();
        slot.data = None;
        slot.pending = true;
        let view = project(&fleet, &state, &mut display);
        assert_eq!(ids(&view), ["claude-work", "native-claude", "native-dsh"]);
        assert_eq!(
            view.accounts[1].members[0].usage.status, "signed_out",
            "the held row"
        );
    }

    #[test]
    fn opencode_rows_show_its_native_configuration() {
        let raw = json!({"id": "native-opencode", "label": "OpenCode", "provider": "opencode", "native": true, "installed": true,
        "home": "/home/example/.local/share/opencode", "catalog_state": "not_running",
        "catalog_reason": "OpenCode is not running on studio.", "catalog_at": 1793000000.5, "providers_truncated": true,
        "providers": [{"id": "anthropic", "label": "Anthropic", "kind": "oauth", "credential_count": 1, "model_count": 12},
                      {"id": "openai", "label": "", "kind": "key", "credential_count": 3, "model_count": 1},
                      {"id": "anthropic", "label": "Again"}, {"id": "bad id"}]});
        let studio = ComputerId::from("studio");
        let mut state = AccountsState::default();
        state.catalogs.insert(
            studio.clone(),
            CatalogSlot {
                requested: Some(0),
                data: Some(json!({"profiles": [raw], "revision": "r"})),
                ..CatalogSlot::default()
            },
        );
        state.usage.insert((studio, "native-opencode".into(), "/home/example/.local/share/opencode".into()),
        UsageSlot { data: Some(json!({"id": "native-opencode", "provider": "opencode", "status": "configured"})), ..UsageSlot::default() });
        let view = project(
            &[entry("studio", "live", 0)],
            &state,
            &mut AccountsDisplay::default(),
        );
        let p = &view.accounts[0].members[0];
        assert_eq!(p.detail.english(), "OpenCode is not running on studio.");
        let oc = p.opencode.as_ref().unwrap();
        assert_eq!(
            oc.providers,
            vec![
                OpenCodeProvider {
                    label: "Anthropic".into(),
                    connection: "Signed in".into(),
                    models: "12 models".into()
                },
                OpenCodeProvider {
                    label: "openai".into(),
                    connection: "API key, 3 accounts".into(),
                    models: "1 model".into()
                },
            ]
        );
        assert!(oc.truncated && oc.stopped);
        assert_eq!(oc.stale_catalog_at, Some(1793000000.5));
    }

    #[test]
    fn a_failed_first_inspect_keeps_the_cached_windows_seeded_with_a_refresh_error() {
        let mut state = loaded();
        state.usage.insert(
            (
                ComputerId::from("studio"),
                "claude-studio".into(),
                "/home/example/.claude".into(),
            ),
            UsageSlot {
                auth_revision: "s1".into(),
                attempted: Some(1),
                failed: true,
                ..UsageSlot::default()
            },
        );
        let view = project(
            &[entry("", "live", 0), entry("studio", "live", 0)],
            &state,
            &mut AccountsDisplay::default(),
        );
        let studio = view
            .accounts
            .iter()
            .flat_map(|a| &a.members)
            .find(|m| m.id == "claude-studio")
            .unwrap();
        assert_eq!(studio.usage.windows.len(), 2);
        assert!(studio.usage.seeded && studio.usage.refresh_error);
        assert_eq!(studio.usage.checked_at, Some(1793000100.0));
        assert_eq!(studio.usage.status, "unavailable", "settled, not loading");
    }

    #[test]
    fn a_failed_first_inspect_settles_the_pending_count() {
        let fleet = [entry("", "live", 0), entry("studio", "live", 0)];
        let key = (
            ComputerId::from("studio"),
            "claude-studio".to_owned(),
            "/home/example/.claude".to_owned(),
        );
        let mut state = loaded();
        let before = project(&fleet, &state, &mut AccountsDisplay::default()).pending;
        state.usage.insert(
            key.clone(),
            UsageSlot {
                auth_revision: "s1".into(),
                attempted: Some(1),
                failed: true,
                ..UsageSlot::default()
            },
        );
        let after = project(&fleet, &state, &mut AccountsDisplay::default()).pending;
        assert_eq!(after + 1, before, "the failed profile no longer counts");
        // A retry in flight counts again.
        state.usage.get_mut(&key).unwrap().pending = true;
        let retry = project(&fleet, &state, &mut AccountsDisplay::default()).pending;
        assert_eq!(retry, before);
    }

    #[test]
    fn this_computer_is_not_an_account_host_until_its_card_arrived() {
        let mut local = entry("", "live", 0);
        local.host = None;
        assert!(hosts(std::slice::from_ref(&local)).is_empty());
        assert!(refs(std::slice::from_ref(&local), &loaded()).is_empty());
        local.host = Some("workstation".into());
        assert_eq!(hosts(std::slice::from_ref(&local)).len(), 1);
    }

    #[test]
    fn no_snapshot_before_this_computers_card() {
        let mut local = entry("", "live", 0);
        local.host = None;
        let mut display = AccountsDisplay::default();
        let early = project(
            std::slice::from_ref(&local),
            &AccountsState::default(),
            &mut display,
        );
        assert!(!early.snapshot && early.pending >= 1 && early.no_hosts.is_none());
        local.host = Some("workstation".into());
        let mut state = AccountsState::default();
        state.catalogs.insert(
            ComputerId::local(),
            CatalogSlot {
                requested: Some(0),
                data: Some(fixture("accounts-local.json")),
                ..CatalogSlot::default()
            },
        );
        let late = project(std::slice::from_ref(&local), &state, &mut display);
        assert!(late.snapshot && late.no_hosts.is_none() && !late.accounts.is_empty());
    }

    /// One computer's catalog with an `account_status` per row, as `account ls` caches it.
    fn launch_catalog(rows: &[(&str, &str)]) -> Value {
        let profiles: Vec<Value> = rows
            .iter()
            .map(|(id, provider)| {
                json!({"id": id, "provider": provider, "label": "", "home": format!("/home/example/{id}"),
                       "installed": true, "account_status": {"status": "ok", "checked_at": 100.0,
                       "identity": {"email": format!("{id}@example.com")}}})
            })
            .collect();
        json!({ "profiles": profiles })
    }

    #[test]
    fn cached_rate_limit_remains_visible_before_inspect_and_wins_a_same_sample_tie() {
        let state = AccountsState::default();
        let local = entry("", "live", 0);
        let mut catalog = launch_catalog(&[("claude-work", "claude")]);
        let quota = json!({"status":"rate_limited","auth_status":"signed_in","checked_at":100,"retry_at":300,
            "identity":{"email":"work@example.test"},"windows":[{"id":"five_hour","used_percent":42,"window_minutes":300}]});
        catalog["profiles"][0]["account_status"] = quota.clone();
        let raw = &catalog["profiles"][0];
        let reference = usage_ref(&local.computer, raw);
        let seeded = profile_usage(&local, raw, &reference, &state);
        assert!(seeded.seeded);
        assert_eq!(seeded.status, "rate_limited");
        assert_eq!(seeded.retry_at, Some(300.0));
        assert_eq!(seeded.checked_at, Some(100.0));
        assert_eq!(seeded.windows[0].used, 42.0);
        let launching = host_profiles(&local, &catalog, &state, &["claude"]);
        assert_eq!(launching[0].0.usage.retry_at, Some(300.0));
        let mut previous = usage::usage(&quota);
        previous.status = "ok".into();
        previous.retry_at = None;
        assert!(better(&seeded, &previous));
        assert!(!better(&previous, &seeded));
        previous.checked_at = Some(301.0);
        assert!(
            better(&previous, &seeded),
            "a genuinely newer sample still wins"
        );
    }

    #[test]
    fn cached_deepseek_api_key_balance_stands_in_before_inspect() {
        let state = AccountsState::default();
        let local = entry("", "live", 0);
        let mut catalog = launch_catalog(&[("native-dsh", "dsh")]);
        catalog["profiles"][0]["account_status"] = json!({"status":"configured","checked_at":100,
            "identity":{"auth_method":"API key","account_id":"api-key:abc"},"windows":[],
            "balances":[{"kind":"Total","balance":"0.00","currency":"USD"},
                        {"kind":"Topped up","balance":"0.00","currency":"USD"},
                        {"kind":"Granted","balance":"0.00","currency":"USD"}],
            "balance_available":false,
            "spend":{"since":100,"today":"2026-11-10","periods":[{"period":"today","amounts":[{"currency":"USD","spent":"0.14","granted":"0.00"}],"top_up":false,"gap_from":null}]},
            "local_usage":{"today":"2026-11-10","partial":false,"days":[{"day":"2026-11-10","models":[{"model":"deepseek-flash","requests":3,"cache_hit":10,"cache_miss":2,"output":1}]}]}});
        let raw = &catalog["profiles"][0];
        let reference = usage_ref(&local.computer, raw);
        let seeded = profile_usage(&local, raw, &reference, &state);
        assert!(seeded.seeded);
        assert_eq!(seeded.balances.len(), 3);
        assert_eq!(seeded.balances[0].kind, "Total");
        assert_eq!(seeded.balance_available, Some(false));
        assert_eq!(seeded.spend.unwrap().periods[0].amounts[0].spent, "0.14");
        assert_eq!(seeded.local_usage.unwrap().days[0].models[0].requests, 3);
    }

    fn ids(profiles: &[(Profile, Value)]) -> Vec<String> {
        profiles.iter().map(|(p, _)| p.id.clone()).collect()
    }

    // G3: NewSessionDialog::updateAccounts (883-896) groups only the selected
    // computer's launchable profiles, the dialog's agents first in their order.
    #[test]
    fn a_computers_launch_profiles_follow_the_dialogs_agent_order() {
        let catalog = launch_catalog(&[
            ("kimi-a", "kimi"),
            ("gemini-a", "gemini"),
            ("native-codex", "codex"),
            ("claude-work", "claude"),
            ("shell", "sh"),
            ("", "claude"),
            ("nameless", ""),
            ("native-dsh", "dsh"),
            ("native-opencode", "opencode"),
            ("claude-copy", "claude"),
        ]);
        let agents = ["codex", "claude", "kimi", "dsh", "opencode"];
        let state = AccountsState::default();
        let local = entry("", "live", 0);
        let profiles = host_profiles(&local, &catalog, &state, &agents);
        assert_eq!(
            ids(&profiles),
            [
                "native-codex",
                "claude-work",
                "claude-copy",
                "kimi-a",
                "native-dsh",
                "native-opencode",
                "gemini-a"
            ]
        );
        // Each comes with its raw row, for the OpenCode fields.
        assert_eq!(profiles[5].1["id"], "native-opencode");
        assert_eq!(profiles[0].0.key, "\u{1f}native-codex");
        // The local Windows computer does not start OpenCode; a peer does. It
        // starts DeepSeek through the host's named pipe.
        let mut windows = entry("", "live", 0);
        windows.platform = Some("windows".into());
        assert!(launchable(&windows, "dsh") && !launchable(&windows, "opencode"));
        assert!(launchable(&windows, "codex"));
        let shown = ids(&host_profiles(&windows, &catalog, &state, &agents));
        assert!(
            shown.contains(&"native-dsh".to_owned())
                && !shown.contains(&"native-opencode".to_owned())
        );
        let mut peer = entry("studio", "live", 0);
        peer.platform = Some("windows".into());
        assert!(ids(&host_profiles(&peer, &catalog, &state, &agents))
            .contains(&"native-dsh".to_owned()));
        // At most 256 rows of a catalog.
        let many: Vec<(String, &str)> = (0..300).map(|n| (format!("p{n}"), "kimi")).collect();
        let rows: Vec<(&str, &str)> = many.iter().map(|(id, p)| (id.as_str(), *p)).collect();
        assert_eq!(
            host_profiles(&local, &launch_catalog(&rows), &state, &agents).len(),
            256
        );
        assert!(host_profiles(&local, &json!({}), &state, &agents).is_empty());
    }

    // AccountCatalog::launchable with NewSessionDialog's folderStyle (108-110)
    // and MachinePlatform::of (33-39): this computer without a platform takes
    // the build's; a peer is POSIX.
    #[test]
    fn this_computer_without_a_platform_follows_the_build() {
        let mut bare = entry("", "live", 0);
        bare.platform = None;
        assert!(!launchable_for(&bare, "opencode", Style::Windows));
        for provider in ["dsh", "opencode"] {
            assert!(launchable_for(&bare, provider, Style::Posix), "{provider}");
        }
        assert!(launchable_for(&bare, "dsh", Style::Windows));
        assert!(launchable_for(&bare, "codex", Style::Windows));
        let mut linux = entry("", "live", 0);
        linux.platform = Some("linux".into());
        assert!(
            launchable_for(&linux, "dsh", Style::Windows),
            "a reported platform wins"
        );
        let mut windows = entry("", "live", 0);
        windows.platform = Some("windows".into());
        assert!(!launchable_for(&windows, "opencode", Style::Posix));
        let mut peer = entry("studio", "live", 0);
        peer.platform = None;
        assert!(
            launchable_for(&peer, "dsh", Style::Windows),
            "a peer is POSIX"
        );
        peer.platform = Some("windows".into());
        assert!(launchable_for(&peer, "opencode", Style::Windows));
        assert_eq!(
            launchable(&bare, "dsh"),
            launchable_for(&bare, "dsh", crate::paths::build())
        );
    }

    // NewSessionDialog::m_agents (221-222): a requested agent the list lacks
    // follows the dialog's agents, before other providers of the catalog.
    #[test]
    fn a_requested_agent_follows_the_dialogs_agents() {
        let catalog = launch_catalog(&[
            ("alpha-a", "alpha"),
            ("zeta-a", "zeta"),
            ("native-codex", "codex"),
        ]);
        let local = entry("", "live", 0);
        let state = AccountsState::default();
        let order = |agents: &[&str]| ids(&host_profiles(&local, &catalog, &state, agents));
        assert_eq!(
            order(&["codex", "zeta"]),
            ["native-codex", "zeta-a", "alpha-a"]
        );
        assert_eq!(order(&["codex"]), ["native-codex", "alpha-a", "zeta-a"]);
    }

    // NewSessionDialog::refreshUsage (990-1000): later limits refresh the
    // rows of the catalog's grouping, even when they show two rows to be one
    // account; each row takes its members' best usage.
    #[test]
    fn later_limits_refresh_the_groups_without_joining_them() {
        let catalog = launch_catalog(&[("claude-a", "claude"), ("claude-b", "claude")]);
        let agents = ["claude"];
        let local = entry("", "live", 0);
        let mut state = AccountsState::default();
        let members = |state: &AccountsState| -> Vec<Profile> {
            host_profiles(&local, &catalog, state, &agents)
                .into_iter()
                .map(|(p, _)| p)
                .collect()
        };
        let grouped = group(&members(&state));
        assert_eq!(grouped.len(), 2, "two emails, two accounts");
        let inspect = |id: &str, at: f64, used: f64| {
            json!({"id": id, "home": format!("/home/example/{id}"), "provider": "claude", "status": "ok",
                   "checked_at": at, "identity": {"email": "same@example.com"},
                   "windows": [{"id": "five_hour", "label": "5h", "used_percent": used, "window_minutes": 300, "resets_at": null}],
                   "balances": []})
        };
        inspected(
            &mut state,
            "",
            "claude-a",
            "/home/example/claude-a",
            "",
            inspect("claude-a", 200.0, 10.0),
        );
        inspected(
            &mut state,
            "",
            "claude-b",
            "/home/example/claude-b",
            "",
            inspect("claude-b", 300.0, 40.0),
        );
        let now = members(&state);
        assert_eq!(group(&now).len(), 1, "a fresh grouping would join them");
        let refreshed = refresh_groups(&grouped, &now);
        let shape: Vec<Vec<&str>> = refreshed
            .iter()
            .map(|g| g.members.iter().map(|m| m.id.as_str()).collect())
            .collect();
        assert_eq!(shape, [vec!["claude-a"], vec!["claude-b"]]);
        assert_eq!(refreshed[0].key, grouped[0].key);
        assert_eq!(refreshed[0].label, grouped[0].label);
        assert_eq!(
            refreshed[0].members[0].usage.identity.email,
            "same@example.com"
        );
        assert_eq!(
            refreshed[1].usage.checked_at,
            Some(300.0),
            "the row's best usage"
        );
        // A member the catalog no longer lists leaves its row.
        let only_a: Vec<Profile> = now.iter().filter(|p| p.id == "claude-a").cloned().collect();
        assert_eq!(refresh_groups(&grouped, &only_a).len(), 1);
    }

    // G3: NewSessionDialog::cachedUsage (971-978): the store's usage, unless it
    // loads, is offline or is older than the catalog's cached status.
    #[test]
    fn a_launch_profile_shows_the_cached_status_until_a_newer_inspect() {
        let catalog = launch_catalog(&[("claude-work", "claude")]);
        let agents = ["claude"];
        let local = entry("", "live", 0);
        let usage = |state: &AccountsState, at: &FleetEntry| {
            host_profiles(at, &catalog, state, &agents)[0]
                .0
                .usage
                .clone()
        };
        let mut state = AccountsState::default();
        let cached = usage(&state, &local);
        assert_eq!(
            (
                cached.status.as_str(),
                cached.identity.email.as_str(),
                cached.checked_at
            ),
            ("ok", "claude-work@example.com", Some(100.0)),
            "loading: the cached status stands in"
        );
        assert!(cached.seeded && !cached.offline && !cached.refreshing);
        let inspect = |at: f64| {
            json!({"id": "claude-work", "home": "/home/example/claude-work", "provider": "claude", "status": "ok",
                   "checked_at": at, "identity": {"email": "fresh@example.com"}, "windows": [], "balances": []})
        };
        inspected(
            &mut state,
            "",
            "claude-work",
            "/home/example/claude-work",
            "",
            inspect(200.0),
        );
        let fresh = usage(&state, &local);
        assert_eq!(
            fresh.identity.email, "fresh@example.com",
            "the newer inspect"
        );
        assert!(!fresh.seeded);
        inspected(
            &mut state,
            "",
            "claude-work",
            "/home/example/claude-work",
            "",
            inspect(50.0),
        );
        assert_eq!(
            usage(&state, &local).identity.email,
            "claude-work@example.com",
            "an older inspect"
        );
        // Without a cached status the store's own answer shows: loading, or offline.
        let bare = json!({"profiles": [{"id": "claude-work", "provider": "claude", "home": "/home/example/claude-work", "installed": true}]});
        let none = AccountsState::default();
        let loading = &host_profiles(&local, &bare, &none, &agents)[0].0.usage;
        assert_eq!(
            (loading.status.as_str(), loading.offline),
            ("loading", false)
        );
        let away = entry("studio", "ended", 0);
        let offline = &host_profiles(&away, &bare, &none, &agents)[0].0.usage;
        assert_eq!(
            (offline.status.as_str(), offline.offline),
            ("offline", true)
        );
        let mut failing = AccountsState::default();
        failing.usage.insert(
            (
                ComputerId::local(),
                "claude-work".into(),
                "/home/example/claude-work".into(),
            ),
            UsageSlot {
                attempted: Some(1),
                failed: true,
                ..UsageSlot::default()
            },
        );
        let failed = &host_profiles(&local, &bare, &failing, &agents)[0].0.usage;
        assert_eq!(
            (failed.status.as_str(), failed.refresh_error),
            ("loading", true),
            "the store's data, not the Accounts page's settled state"
        );
    }

    #[test]
    fn hosts_accounts_and_notices_are_bounded() {
        let fleet: Vec<FleetEntry> = (0..70)
            .map(|n| entry(&format!("h{n}"), "live", 0))
            .collect();
        assert_eq!(hosts(&fleet).len(), 64);
        let mut state = AccountsState::default();
        for n in 0..6 {
            let profiles: Vec<Value> = (0..256)
                .map(|p| {
                    json!({"id": format!("p{p}"), "label": format!("L{n}-{p}"), "provider": "kimi", "installed": true,
                        "account_status": {"status": "ok", "identity": {"account_id": format!("a{n}-{p}")}}})
                })
                .collect();
            state.catalogs.insert(
                ComputerId::from(format!("h{n}").as_str()),
                CatalogSlot {
                    requested: Some(0),
                    data: Some(json!({"profiles": profiles})),
                    error: Some("e".into()),
                    ..CatalogSlot::default()
                },
            );
        }
        for n in 6..30 {
            state.catalogs.insert(
                ComputerId::from(format!("h{n}").as_str()),
                CatalogSlot {
                    requested: Some(0),
                    error: Some("e".into()),
                    ..CatalogSlot::default()
                },
            );
        }
        let view = project(&fleet, &state, &mut AccountsDisplay::default());
        assert_eq!(view.accounts.len(), 1024);
        assert_eq!(view.truncated, 6 * 256 - 1024);
        assert_eq!(view.notices.len(), 16);
    }
}

#[cfg(test)]
mod permission_source_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn provider_bypass_is_distinct_from_a_zerus_override() {
        let entry = FleetEntry {
            computer: ComputerId::local(),
            status: "live".into(),
            attempt: 0,
            local_sessions: true,
            local_host: false,
            host: Some("workstation".into()),
            platform: Some("linux".into()),
        };
        let raw = json!({"id":"work","provider":"codex","home":"/accounts/example","installed":false,"permission_mode":"provider","effective_permission_mode":"bypass"});
        let inherited = profile(&entry, &raw, &AccountsState::default());
        assert_eq!(inherited.permission, "bypass");
        assert_eq!(inherited.permission_mode, "provider");
        assert!(inherited
            .permission_hint
            .english()
            .starts_with("From provider settings"));
        let mut explicit = raw;
        explicit["permission_mode"] = json!("bypass");
        assert_eq!(
            profile(&entry, &explicit, &AccountsState::default()).permission_mode,
            "bypass"
        );
    }

    #[test]
    fn opencode_native_policy_is_localized_and_never_labelled_as_a_zerus_default() {
        let entry = FleetEntry {
            computer: ComputerId::local(),
            status: "live".into(),
            attempt: 0,
            local_sessions: true,
            local_host: false,
            host: Some("workstation".into()),
            platform: Some("linux".into()),
        };
        for (mode, label) in [
            ("allow", "Allow tools"),
            ("ask", "Ask before tools"),
            ("deny", "Deny tools"),
            ("custom", "Custom rules"),
            ("unknown", "Unverified"),
        ] {
            let raw = json!({"id":"native-opencode","provider":"opencode","home":"/accounts/example","native":true,"installed":true,
                "permission_mode":"provider","permission_mode_source":"default","effective_permission_mode":mode,"permission_detail":"OpenCode native default"});
            let p = profile(&entry, &raw, &AccountsState::default());
            assert_eq!(p.permission, mode);
            assert_eq!(p.permission_label.source, label);
            assert!(!p.permission_default);
            assert_eq!(p.permission_mode, "provider");
            assert!(p
                .permission_hint
                .english()
                .starts_with("From provider settings"));
            assert!(!p.permission_hint.english().contains("Without a choice"));
            assert!(p
                .permission_hint
                .english()
                .contains("Project, agent and session rules"));
            if mode == "allow" {
                assert!(p.permission_hint.english().contains(".env"));
            }
            if mode == "unknown" {
                assert!(p
                    .permission_hint
                    .english()
                    .contains("could not be verified"));
            }
            assert!(p
                .permission_hint
                .args
                .iter()
                .all(|arg| !matches!(arg, crate::text::Arg::Verbatim(_))));
        }
    }
}
