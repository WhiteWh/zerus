//! Update checks apart from the UI: installation detection, verified catalogs,
//! persisted state and notices, on one worker (`UpdateController`).
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State as TauriState};
use zerus_client_core::attention::Notice;
use zerus_client_core::settings::Settings;
use zerus_client_core::updates::{
    automatic_interval, comparison_message, dev_build_is_newer, download_url,
    homebrew_installation, installation_guide, installation_instructions_url, is_arch_linux,
    macos_bundle_installation, macos_installation, macos_update_available, packaged_bundle_release,
    packaged_bundle_replaced, plain_stable_is_newer, portable_update_available, preferred_channel,
    running_platform, update_command, update_token, windows_build, Channel, Feed, Installation,
    InstallationGuide, CATALOG_LIMIT,
};

const TIMER: Duration = Duration::from_secs(15 * 60);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    pub app_version: String,
    /// The version, its source commit and any local changes, as interface prose.
    pub installed: zerus_client_core::text::Text,
    pub dev_number: u64,
    pub source_commit: String,
}

impl Build {
    fn current() -> Self {
        let info = crate::cli::build_info(&crate::cli::BUILD);
        let app_version = info["versionName"].as_str().unwrap_or("").to_owned();
        let dev_number = info["devBuildNumber"].as_u64().unwrap_or(0);
        let source_commit = info["sourceCommit"].as_str().unwrap_or("").to_owned();
        let mut installed = zerus_client_core::text::Text::verbatim(app_version.clone());
        if dev_number == 0 && info.get("previewLabel").is_none() && !source_commit.is_empty() {
            let short = source_commit[..source_commit.len().min(7)].to_owned();
            installed = if info["sourceDirty"].as_bool() == Some(true) {
                zerus_client_core::text!("%1 (local changes %2)", app_version.clone(), short)
            } else {
                zerus_client_core::text!("%1 (%2)", app_version.clone(), short)
            };
        }
        Self {
            app_version,
            installed,
            dev_number,
            source_commit,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct UpdatesView {
    pub busy: bool,
    pub available: bool,
    /// Interface prose; the webview translates it (`client/src/text.ts`).
    pub heading: zerus_client_core::text::Text,
    pub installed: zerus_client_core::text::Text,
    /// The published version as it is, or "Not checked".
    pub latest: zerus_client_core::text::Text,
    pub comparison: Option<zerus_client_core::text::Text>,
    pub last_checked: String,
    pub cached: bool,
    pub error: Option<zerus_client_core::text::Text>,
    pub channel: Channel,
    pub automatic: bool,
    pub release_label: zerus_client_core::text::Text,
    pub release_enabled: bool,
    pub download_label: Option<zerus_client_core::text::Text>,
    pub details: InstallationGuide,
    pub restart_required: bool,
    #[serde(default)]
    pub login_item_deprioritized: bool,
    pub version: String,
    /// A source or unknown dev-app installation keeps its status in Settings only.
    pub quiet: bool,
}

#[derive(Clone)]
struct UpdateState {
    installation: Installation,
    feed: Feed,
    channel: Channel,
    automatic: bool,
    busy: bool,
    available: bool,
    comparison: Option<zerus_client_core::text::Text>,
    last_success: i64,
    cached: bool,
    error: Option<zerus_client_core::text::Text>,
    restart_required: bool,
    login_item_deprioritized: bool,
    fresh_for_notification: bool,
    automatic_in_flight: bool,
    from_service: bool,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            installation: Installation {
                kind: "checking".into(),
                ..Installation::default()
            },
            feed: Feed::default(),
            channel: Channel::Stable,
            automatic: true,
            busy: false,
            available: false,
            comparison: None,
            last_success: 0,
            cached: false,
            error: None,
            restart_required: false,
            login_item_deprioritized: false,
            fresh_for_notification: false,
            automatic_in_flight: false,
            from_service: false,
        }
    }
}

type Fetch = Arc<dyn Fn(Channel) -> Result<Vec<u8>, String> + Send + Sync>;
type Detect = Arc<dyn Fn(&Path) -> Installation + Send + Sync>;
type Compare = Arc<dyn Fn(&Feed, &Installation) -> Option<i32> + Send + Sync>;
type Changed = Arc<dyn Fn(UpdatesView) + Send + Sync>;
type Notify = Arc<dyn Fn(Notice) + Send + Sync>;
type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;
type ProjectionEvent = (u64, Option<zerus_client_core::maintenance::Projection>);
type MaintenanceChanged =
    Arc<dyn Fn(Option<&zerus_client_core::maintenance::Projection>) + Send + Sync>;

#[derive(Clone)]
struct Hooks {
    fetch: Fetch,
    detect: Detect,
    compare: Compare,
    changed: Changed,
    notify: Notify,
    now: Clock,
    maintenance_changed: MaintenanceChanged,
}

struct Shared {
    state: Mutex<UpdateState>,
    replay_view: Mutex<Option<UpdatesView>>,
    settings: Settings,
    build: Build,
    executable: PathBuf,
    started_release: String,
    generation: AtomicU64,
    sender: Sender<Request>,
    hooks: Hooks,
    dev_identity: bool,
    service_mode: bool,
    bundle_id: String,
    projection_sender: SyncSender<ProjectionEvent>,
    projection_receiver: Mutex<Receiver<ProjectionEvent>>,
    projection_version: AtomicU64,
    projection_wake: AtomicBool,
}

#[derive(Clone)]
pub struct Updates(Arc<Shared>);

enum Request {
    Check(bool),
    Channel(Channel),
    Automatic(bool),
    Projection,
    Preferences {
        channel: Option<Channel>,
        automatic: Option<bool>,
        done: tokio::sync::oneshot::Sender<()>,
    },
}

fn lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn quiet_prompts(dev_identity: bool, installation: &Installation) -> bool {
    dev_identity && matches!(installation.kind.as_str(), "source" | "unknown")
}

fn catalog_key(kind: &str, channel: Channel) -> String {
    format!("updates/{kind}/{}", channel.as_str())
}

fn checked_text(at: i64) -> String {
    if at <= 0 {
        return "Never".into();
    }
    chrono::DateTime::from_timestamp(at, 0)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| "Never".into())
}

impl UpdatesView {
    fn from(state: &UpdateState, build: &Build) -> Self {
        let platform = running_platform();
        let url = download_url(&state.installation, &state.feed, platform);
        let download_label = (!url.is_empty()).then(|| {
            if platform == "macos" && !state.feed.macos_zip_url.is_empty() {
                if state.feed.macos_dmg_url.is_empty() {
                    zerus_client_core::text!("Download macOS ZIP")
                } else {
                    zerus_client_core::text!("Download DMG")
                }
            } else if state.installation.arch {
                zerus_client_core::text!("Download dev package")
            } else {
                zerus_client_core::text!("Download dev source")
            }
        });
        let installed =
            if macos_installation(&state.installation) && !state.installation.version.is_empty() {
                zerus_client_core::text::Text::verbatim(state.installation.version.clone())
            } else {
                build.installed.clone()
            };
        Self {
            busy: state.busy,
            available: state.available,
            heading: if state.busy {
                zerus_client_core::text!("Checking for updates…")
            } else if state.available {
                zerus_client_core::text!("Update available")
            } else if state.feed.valid {
                zerus_client_core::text!("Release status")
            } else {
                zerus_client_core::text!("Updates have not been checked")
            },
            installed,
            latest: if state.feed.valid {
                zerus_client_core::text::Text::verbatim(state.feed.version.clone())
            } else {
                zerus_client_core::text!("Not checked")
            },
            comparison: state.comparison.clone(),
            last_checked: checked_text(state.last_success),
            cached: state.cached,
            error: state.error.clone(),
            channel: state.channel,
            automatic: state.automatic,
            release_label: if state.channel == Channel::Dev {
                zerus_client_core::text!("View build")
            } else {
                zerus_client_core::text!("View release")
            },
            release_enabled: state.feed.valid,
            download_label,
            details: installation_guide(
                &state.installation,
                state.channel,
                &state.feed,
                &build.app_version,
            ),
            restart_required: state.restart_required,
            login_item_deprioritized: state.login_item_deprioritized,
            version: state.feed.version.clone(),
            quiet: false,
        }
    }
}

impl Updates {
    fn new(
        settings: Settings,
        build: Build,
        executable: PathBuf,
        dev_identity: bool,
        service_mode: bool,
        bundle_id: String,
        hooks: Hooks,
    ) -> (Self, Receiver<Request>) {
        let (sender, receiver) = mpsc::channel();
        let (projection_sender, projection_receiver) = mpsc::sync_channel(1);
        let started_release = packaged_bundle_release(
            &executable
                .parent()
                .unwrap_or(&executable)
                .join("../Info.plist"),
        );
        let state = UpdateState {
            channel: Channel::parse(&settings.text("updates/channel")).unwrap_or_default(),
            automatic: settings.flag("updates/automatic"),
            login_item_deprioritized: crate::login_item::deprioritized(),
            ..UpdateState::default()
        };
        (
            Self(Arc::new(Shared {
                state: Mutex::new(state),
                replay_view: Mutex::new(None),
                settings,
                build,
                executable,
                started_release,
                generation: AtomicU64::new(0),
                sender,
                hooks,
                dev_identity,
                service_mode,
                bundle_id,
                projection_sender,
                projection_receiver: Mutex::new(projection_receiver),
                projection_version: AtomicU64::new(0),
                projection_wake: AtomicBool::new(false),
            })),
            receiver,
        )
    }

    fn view(&self) -> UpdatesView {
        if let Some(view) = lock(&self.0.replay_view).clone() {
            return view;
        }
        let state = lock(&self.0.state);
        let mut view = UpdatesView::from(&state, &self.0.build);
        view.quiet = quiet_prompts(self.0.dev_identity, &state.installation);
        view
    }

    fn send(&self, request: Request) {
        let _ = self.0.sender.send(request);
    }
    async fn preferences(
        &self,
        channel: Option<Channel>,
        automatic: Option<bool>,
    ) -> Result<(), String> {
        let (done, acknowledged) = tokio::sync::oneshot::channel();
        let unavailable =
            || zerus_client_core::source!("Could not save update preferences.").to_owned();
        self.0
            .sender
            .send(Request::Preferences {
                channel,
                automatic,
                done,
            })
            .map_err(|_| unavailable())?;
        tokio::time::timeout(COMMAND_TIMEOUT, acknowledged)
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())
    }

    fn changed(&self) {
        (self.0.hooks.changed)(self.view());
    }

    fn current(&self, generation: u64) -> bool {
        self.0.generation.load(Ordering::SeqCst) == generation
    }

    fn restore(&self) {
        let channel = lock(&self.0.state).channel;
        let raw = self.0.settings.text(&catalog_key("catalog", channel));
        let checked = self.0.settings.number(&catalog_key("lastSuccess", channel)) as i64;
        let parsed = zerus_client_core::updates::parse_feed(raw.as_bytes(), channel);
        let now = (self.0.hooks.now)();
        if !parsed.valid || checked <= 0 || checked > now {
            return;
        }
        {
            let mut state = lock(&self.0.state);
            if state.channel != channel || state.feed.valid {
                return;
            }
            state.feed = parsed;
            state.last_success = checked;
            state.cached = true;
            state.from_service = self.0.service_mode;
        }
        self.changed();
        self.compare();
    }

    fn detect(&self) {
        let installation = (self.0.hooks.detect)(&self.0.executable);
        let preferred = (!self.0.settings.contains("updates/channel"))
            .then(|| preferred_channel(&installation))
            .flatten();
        if let Some(channel) = preferred {
            let _ = self
                .0
                .settings
                .set("updates/channel", channel.as_str().into());
            let mut state = lock(&self.0.state);
            if state.channel != channel {
                self.0.generation.fetch_add(1, Ordering::SeqCst);
                state.channel = channel;
                state.feed = Feed::default();
                state.last_success = 0;
                state.cached = false;
                state.available = false;
                state.comparison = None;
            }
        }
        lock(&self.0.state).installation = installation;
        self.changed();
        self.compare();
    }

    fn refresh_restart(&self) {
        let replaced = packaged_bundle_replaced(&self.0.executable, &self.0.started_release);
        let mut state = lock(&self.0.state);
        if state.restart_required != replaced {
            state.restart_required = replaced;
            drop(state);
            self.changed();
        }
    }

    /// Explicit native build/preferences for the authenticated frontend owner.
    /// This value is data; it cannot grant request authority by itself.
    pub(crate) fn native_configuration(
        &self,
    ) -> zerus_client_core::maintenance::UpdateConfiguration {
        let state = lock(&self.0.state);
        zerus_client_core::maintenance::UpdateConfiguration {
            gui: zerus_client_core::maintenance::GuiIdentity {
                bundle_id: self.0.bundle_id.clone(),
                executable: self.0.executable.to_string_lossy().into_owned(),
                app_version: self.0.build.app_version.clone(),
                dev_number: self.0.build.dev_number,
                source_commit: self.0.build.source_commit.clone(),
            },
            channel: state.channel,
            automatic: state.automatic,
        }
    }
    pub(crate) async fn configuration_for_intent(
        &self,
    ) -> Result<zerus_client_core::maintenance::UpdateConfiguration, String> {
        // Also fence a prior preference request whose caller timed out. A
        // later explicit Apply must not submit the old queued preference pair.
        self.preferences(None, None).await?;
        Ok(self.native_configuration())
    }
    /// Called off GUI by the validated service stream. Never waits for a lock
    /// or I/O: one bounded pending projection and one coalesced worker wake.
    fn receive_projection(&self, frame: Option<&serde_json::Value>) {
        if !self.0.service_mode {
            return;
        }
        if frame.is_some_and(|f| !matches!(f["t"].as_str(), Some("hello" | "status" | "heartbeat")))
        {
            return;
        }
        let projection = frame.and_then(|frame| {
            let epoch = frame["epoch"].as_str()?;
            let value: zerus_client_core::maintenance::Projection =
                serde_json::from_value(frame["value"]["maintenance"].clone()).ok()?;
            value.valid_for(epoch).then_some(value)
        });
        let version = self
            .0
            .projection_version
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_or(u64::MAX, |n| n + 1);
        // Replace the older pending frame without waiting for the consumer.
        // On contention the version fence still invalidates stale positives.
        if let Ok(receiver) = self.0.projection_receiver.try_lock() {
            let _ = receiver.try_recv();
            let _ = self.0.projection_sender.try_send((version, projection));
        }
        if !self.0.projection_wake.swap(true, Ordering::AcqRel) {
            self.send(Request::Projection);
        }
    }
    fn consume_projection(&self) {
        self.0.projection_wake.store(false, Ordering::Release);
        let queued = lock(&self.0.projection_receiver).try_recv().ok();
        let latest = self.0.projection_version.load(Ordering::Acquire);
        let projection = queued
            .filter(|(version, _)| *version == latest && latest != u64::MAX)
            .and_then(|(_, view)| view);
        let projection = projection.as_ref().filter(|view| view.ready);
        (self.0.hooks.maintenance_changed)(projection);
        let config = self.native_configuration();
        let current = projection.filter(|view| {
            view.configuration.as_ref().and_then(|c| c.update.as_ref()) == Some(&config)
        });
        let Some(view) = current else {
            self.service_unavailable();
            return;
        };
        let update = &view.updates;
        let mut state = lock(&self.0.state);
        if update.channel != Some(state.channel) || update.automatic != state.automatic {
            drop(state);
            self.service_unavailable();
            return;
        }
        if state.from_service || update.last_success >= state.last_success {
            state.feed = update.feed.clone();
            state.installation = update.installation.clone();
            state.last_success = update.last_success;
            state.from_service = true;
            state.available = update.ready && update.current_origin && update.available;
            state.cached = update.cached || !update.current_origin && update.feed.valid;
            state.comparison = update.feed.valid.then(|| {
                comparison_message(
                    &update.installation,
                    &update.feed,
                    &self.0.build.app_version,
                    update.comparison,
                    update.compared,
                    running_platform(),
                )
            });
        }
        state.busy = update.busy;
        state.error = update
            .error
            .as_ref()
            .map(|_| zerus_client_core::text!("Check failed. Try again later."));
        state.fresh_for_notification = false;
        state.automatic_in_flight = false;
        drop(state);
        self.changed();
    }
    fn service_unavailable(&self) {
        let mut state = lock(&self.0.state);
        if state.from_service {
            state.available = false;
            state.cached = state.feed.valid;
        }
        state.busy = false;
        state.fresh_for_notification = false;
        state.automatic_in_flight = false;
        state.error = Some(zerus_client_core::text!(
            "Automatic update checks are unavailable from the desktop service."
        ));
        drop(state);
        self.changed();
    }

    fn check_automatic(&self) {
        if self.0.service_mode {
            return;
        }
        let state = lock(&self.0.state).clone();
        if !state.automatic || state.installation.kind == "checking" {
            return;
        }
        let now = (self.0.hooks.now)();
        let last = self
            .0
            .settings
            .number(&catalog_key("lastAttempt", state.channel)) as i64;
        let interval = if windows_build(&state.installation) {
            automatic_interval(Channel::Stable)
        } else {
            automatic_interval(state.channel)
        } as i64;
        if last <= 0 || last > now || now - last >= interval {
            self.check(true);
        }
    }

    fn check(&self, automatic: bool) {
        let (generation, channel) = {
            let mut state = lock(&self.0.state);
            if state.busy || (automatic && !state.automatic) {
                return;
            }
            state.busy = true;
            state.fresh_for_notification = false;
            state.automatic_in_flight = automatic;
            state.error = None;
            state.comparison = Some(zerus_client_core::text!(
                "Checking the published release catalog…"
            ));
            (
                self.0.generation.fetch_add(1, Ordering::SeqCst) + 1,
                state.channel,
            )
        };
        let now = (self.0.hooks.now)();
        let _ = self
            .0
            .settings
            .set(&catalog_key("lastAttempt", channel), now.into());
        self.changed();
        let fetched = (self.0.hooks.fetch)(channel);
        if !self.current(generation) {
            return;
        }
        match fetched {
            Ok(raw) => {
                let feed = zerus_client_core::updates::parse_feed(&raw, channel);
                if !feed.valid {
                    self.failed_check(generation);
                    return;
                }
                let now = (self.0.hooks.now)();
                let may_notify = {
                    let mut state = lock(&self.0.state);
                    if state.channel != channel {
                        return;
                    }
                    let may_notify = !state.automatic_in_flight || state.automatic;
                    state.busy = false;
                    state.automatic_in_flight = false;
                    state.fresh_for_notification = may_notify;
                    state.cached = false;
                    state.available = false;
                    state.feed = feed;
                    state.from_service = false;
                    state.last_success = now;
                    state.error = None;
                    state.comparison = None;
                    may_notify
                };
                let _ = may_notify;
                let raw_text = String::from_utf8(raw).unwrap_or_default();
                let _ = self
                    .0
                    .settings
                    .set(&catalog_key("catalog", channel), raw_text.into());
                let _ = self
                    .0
                    .settings
                    .set(&catalog_key("lastSuccess", channel), now.into());
                self.changed();
                self.compare();
            }
            Err(error) => {
                eprintln!("zerus-client: update check: {error}");
                self.failed_check(generation);
            }
        }
    }

    fn failed_check(&self, generation: u64) {
        if !self.current(generation) {
            return;
        }
        {
            let mut state = lock(&self.0.state);
            state.busy = false;
            state.automatic_in_flight = false;
            state.fresh_for_notification = false;
            state.error = Some(zerus_client_core::text!("Check failed. Try again later."));
            state.cached = state.feed.valid;
            state.comparison = state.feed.valid.then(|| {
                comparison_message(
                    &state.installation,
                    &state.feed,
                    &self.0.build.app_version,
                    0,
                    false,
                    running_platform(),
                )
            });
        }
        self.changed();
        self.compare();
    }

    fn compare(&self) {
        let (generation, state) = {
            let state = lock(&self.0.state).clone();
            if !state.feed.valid || state.installation.kind == "checking" {
                return;
            }
            (self.0.generation.load(Ordering::SeqCst), state)
        };
        let (newer, comparison, compared) = if macos_installation(&state.installation) {
            (
                macos_update_available(&state.installation, &state.feed),
                0,
                false,
            )
        } else if state.channel == Channel::Dev {
            (
                !windows_build(&state.installation)
                    && dev_build_is_newer(
                        &state.feed,
                        self.0.build.dev_number,
                        &self.0.build.source_commit,
                    ),
                0,
                false,
            )
        } else if state.installation.kind != "package" {
            (
                if windows_build(&state.installation) {
                    portable_update_available(&self.0.build.app_version, &state.feed)
                } else {
                    state.channel == Channel::Stable
                        && plain_stable_is_newer(&self.0.build.app_version, &state.feed.version)
                },
                0,
                false,
            )
        } else if state.installation.package == "zerus-git"
            || state.installation.version.is_empty()
            || !state
                .feed
                .packages
                .contains_key(&state.installation.package)
        {
            (false, 0, false)
        } else if let Some(value) = (self.0.hooks.compare)(&state.feed, &state.installation) {
            (value > 0, value, true)
        } else {
            (false, 0, false)
        };
        if !self.current(generation) {
            return;
        }
        let token = {
            let mut current = lock(&self.0.state);
            current.available = newer;
            current.comparison = Some(comparison_message(
                &current.installation,
                &current.feed,
                &self.0.build.app_version,
                comparison,
                compared,
                running_platform(),
            ));
            current.fresh_for_notification = false;
            let fresh = state.fresh_for_notification;
            let quiet = quiet_prompts(self.0.dev_identity, &current.installation);
            update_token(current.channel, &current.feed, newer && fresh && !quiet)
        };
        self.changed();
        if !self.0.service_mode && !token.is_empty() && self.claim_notification(&token) {
            let version = lock(&self.0.state).feed.version.clone();
            (self.0.hooks.notify)(Notice {
                token,
                title: "Zerus update available".into(),
                body: format!(
                    "Zerus {version} is available. Open Updates for installation instructions."
                ),
            });
        }
    }

    fn claim_notification(&self, token: &str) -> bool {
        if !zerus_client_core::updates::is_update_token(token) {
            return false;
        }
        let key = format!("updates/notified/{token}");
        if self.0.settings.flag(&key) {
            return false;
        }
        self.0.settings.set(&key, true.into()).is_ok()
            && self.0.settings.flush_within(Duration::from_secs(5))
    }

    fn set_channel(&self, channel: Channel) {
        self.0.generation.fetch_add(1, Ordering::SeqCst);
        {
            let mut state = lock(&self.0.state);
            state.channel = channel;
            state.feed = Feed::default();
            state.available = false;
            state.fresh_for_notification = false;
            state.automatic_in_flight = false;
            state.busy = false;
            state.error = None;
            state.comparison = Some(zerus_client_core::text!(
                "Updates have not been checked for this channel."
            ));
            state.last_success = 0;
            state.cached = false;
        }
        let _ = self
            .0
            .settings
            .set("updates/channel", channel.as_str().into());
        self.changed();
        self.restore();
        self.compare();
        self.check_automatic();
        if self.0.service_mode {
            self.service_unavailable();
        }
    }

    fn set_automatic(&self, enabled: bool) {
        {
            let mut state = lock(&self.0.state);
            state.automatic = enabled;
            if !enabled && state.automatic_in_flight {
                self.0.generation.fetch_add(1, Ordering::SeqCst);
                state.busy = false;
                state.automatic_in_flight = false;
                state.fresh_for_notification = false;
            }
        }
        let _ = self.0.settings.set("updates/automatic", enabled.into());
        self.changed();
        if enabled {
            self.check_automatic();
        }
        if self.0.service_mode {
            self.service_unavailable();
        }
    }

    fn invalidate(&self) {
        self.0.generation.fetch_add(1, Ordering::SeqCst);
        let mut state = lock(&self.0.state);
        state.busy = false;
        state.automatic_in_flight = false;
        state.fresh_for_notification = false;
    }

    fn worker(&self, receiver: Receiver<Request>) {
        self.restore();
        self.detect();
        self.refresh_restart();
        self.check_automatic();
        if self.0.service_mode {
            self.service_unavailable();
        }
        loop {
            let request = if self.0.service_mode {
                receiver.recv().map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                receiver.recv_timeout(TIMER)
            };
            match request {
                Ok(Request::Check(automatic)) => {
                    self.refresh_restart();
                    self.check(automatic);
                }
                Ok(Request::Channel(channel)) => self.set_channel(channel),
                Ok(Request::Automatic(enabled)) => self.set_automatic(enabled),
                Ok(Request::Projection) => self.consume_projection(),
                Ok(Request::Preferences {
                    channel,
                    automatic,
                    done,
                }) => {
                    if let Some(channel) = channel {
                        self.set_channel(channel);
                    }
                    if let Some(on) = automatic {
                        self.set_automatic(on);
                    }
                    // The following full native Configure may now read the
                    // installed preferences, after startup detection too.
                    let _ = done.send(());
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.refresh_restart();
                    self.check_automatic();
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn notification_current(&self, token: &str) -> bool {
        let state = lock(&self.0.state);
        state.available && update_token(state.channel, &state.feed, true) == token
    }
}

fn spawn(service: Updates, receiver: Receiver<Request>) {
    if let Err(error) = std::thread::Builder::new()
        .name("zerus-updates".into())
        .spawn(move || service.worker(receiver))
    {
        eprintln!("zerus-client: the update worker did not start: {error}");
    }
}

const REPLAY_VIEW_LIMIT: u64 = 64 * 1024;

fn read_replay_view(path: &Path) -> Result<UpdatesView, String> {
    use std::io::Read;
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(REPLAY_VIEW_LIMIT + 1)
        .read_to_end(&mut raw)
        .map_err(|error| error.to_string())?;
    if raw.len() as u64 > REPLAY_VIEW_LIMIT {
        return Err("the replay update view exceeds 64 KiB".into());
    }
    serde_json::from_slice(&raw).map_err(|error| error.to_string())
}

pub fn start(app: &AppHandle) -> Updates {
    let settings = app.state::<crate::settings::Store>().0.clone();
    let replay = std::env::var_os("ZERUS_CLIENT_REPLAY").is_some_and(|value| !value.is_empty());
    let dev_identity = app.state::<crate::identity::Identity>().bundle_id == "dev.zerus.next";
    let executable = std::env::current_exe().unwrap_or_default();
    let changed: Changed = {
        let app = app.clone();
        Arc::new(move |view| {
            if let Some(desk) = crate::desk::sender(&app) {
                let _ = desk.send(crate::desk::DeskInput::Updates {
                    version: (view.available && !view.quiet).then_some(view.version.clone()),
                    restart: view.restart_required && !view.quiet,
                });
            }
            let _ = app.emit("updates-changed", view);
        })
    };
    let notify: Notify = {
        let app = app.clone();
        Arc::new(move |notice| {
            #[cfg(target_os = "macos")]
            crate::platform::macos_notify::deliver(&app, Vec::new(), vec![notice]);
            #[cfg(target_os = "linux")]
            crate::platform::linux_notify::deliver(&app, Vec::new(), vec![notice]);
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            let _ = (&app, notice);
        })
    };
    let hooks = Hooks {
        fetch: Arc::new(fetch_catalog),
        detect: Arc::new(detect_installation),
        compare: Arc::new(compare_package),
        changed,
        notify,
        now: Arc::new(unix_now),
        maintenance_changed: {
            let app = app.clone();
            Arc::new(move |view| {
                crate::recovery::service_projection(&app, view.map(|v| &v.recovery))
            })
        },
    };
    let (service, receiver) = Updates::new(
        settings,
        Build::current(),
        executable,
        dev_identity,
        crate::desktop_service::mode(),
        app.state::<crate::identity::Identity>().bundle_id.clone(),
        hooks,
    );
    if replay {
        if let Some(path) = std::env::var_os("ZERUS_CLIENT_UPDATES_FIXTURE") {
            match read_replay_view(Path::new(&path)) {
                Ok(view) => {
                    *lock(&service.0.replay_view) = Some(view);
                    service.changed();
                }
                Err(error) => eprintln!("zerus-client: replay update view: {error}"),
            }
        }
    } else {
        if service.0.service_mode {
            let updates = service.clone();
            if let Err(error) = app
                .state::<crate::commands::AppState>()
                .register_service_projection(Arc::new(move |frame| {
                    updates.receive_projection(frame)
                }))
            {
                eprintln!("zerus-client: maintenance projection: {error}");
            }
        }
        spawn(service.clone(), receiver);
    }
    service
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

fn fetch_catalog(channel: Channel) -> Result<Vec<u8>, String> {
    use ureq::tls::{RootCerts, TlsConfig};
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .max_redirects(0)
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .new_agent();
    let schema = if channel == Channel::Dev { "v2" } else { "v1" };
    let url = format!(
        "https://zerus.dev/updates/{schema}/desktop/{}.json",
        channel.as_str()
    );
    let mut response = agent.get(&url).call().map_err(|error| error.to_string())?;
    if response.status().as_u16() != 200 {
        return Err(format!(
            "catalog returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(CATALOG_LIMIT as u64 + 1)
        .read_to_vec()
        .map_err(|error| error.to_string())?;
    if bytes.len() > CATALOG_LIMIT {
        return Err("catalog exceeds 256 KiB".into());
    }
    Ok(bytes)
}

fn find_program(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&paths) {
        let candidate = directory.join(name);
        let candidates = if cfg!(windows) {
            vec![candidate.with_extension("exe"), candidate]
        } else {
            vec![candidate]
        };
        for path in candidates {
            let Ok(metadata) = path.metadata() else {
                continue;
            };
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = true;
            if metadata.is_file() && executable {
                return Some(path);
            }
        }
    }
    None
}

fn run(program: &Path, args: &[&str]) -> Option<String> {
    let invocation = zerus_client_core::actions::Invocation {
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        stdin: None,
        timeout: COMMAND_TIMEOUT,
        request_id: None,
    };
    match zerus_client_core::run_limited(program, &invocation, &[], 32 * 1024) {
        Ok(zerus_client_core::actions::Exit::Code {
            code: 0, stdout, ..
        }) => Some(String::from_utf8_lossy(&stdout).trim().to_owned()),
        _ => None,
    }
}

fn valid_package(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 120
        && text
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && text
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'@' | b'.' | b'_' | b'+' | b'-'))
}

fn source_installation(executable: &Path) -> Installation {
    let mut installation = Installation::default();
    let mut directory = executable.parent().unwrap_or(executable);
    let windows_bundle = cfg!(windows) && directory.join("build-info.json").is_file();
    for _ in 0..20 {
        if !windows_bundle && directory.join(".git").exists() {
            installation.kind = "source".into();
            break;
        }
        let Some(parent) = directory.parent() else {
            break;
        };
        directory = parent;
    }
    if cfg!(windows) && installation.kind.is_empty() {
        let record = executable
            .parent()
            .unwrap_or(executable)
            .join("installation.json");
        installation.kind = if zerus_client_core::updates::installer_record_path(&record) {
            "installer"
        } else {
            "portable"
        }
        .into();
        if installation.kind == "portable" {
            installation.package =
                zerus_client_core::updates::winget_package(&executable.to_string_lossy());
        }
    }
    if installation.kind.is_empty() {
        installation.kind = "unknown".into();
    }
    installation
}

fn linux_facts(installation: &mut Installation) {
    if !cfg!(target_os = "linux") {
        return;
    }
    installation.arch = std::fs::read("/etc/os-release")
        .ok()
        .is_some_and(|bytes| is_arch_linux(&bytes));
    installation.helper = find_program("yay")
        .map(|_| "yay")
        .or_else(|| find_program("paru").map(|_| "paru"))
        .unwrap_or("")
        .into();
}

fn detect_installation(executable: &Path) -> Installation {
    let Ok(canonical) = executable.canonicalize() else {
        let mut unknown = Installation {
            kind: "unknown".into(),
            ..Installation::default()
        };
        linux_facts(&mut unknown);
        return unknown;
    };
    let mut installation = if cfg!(target_os = "macos") {
        let homebrew = homebrew_installation(
            &canonical,
            &[Path::new("/opt/homebrew"), Path::new("/usr/local")],
        );
        if macos_installation(&homebrew) {
            homebrew
        } else {
            macos_bundle_installation(&canonical)
        }
    } else {
        Installation::default()
    };
    if cfg!(target_os = "linux") {
        linux_facts(&mut installation);
        if let Some(pacman) = find_program("pacman") {
            if let Some(owner) = run(&pacman, &["-Qqo", "--", &canonical.to_string_lossy()])
                .filter(|owner| valid_package(owner))
            {
                if let Some(raw) = run(&pacman, &["-Q", "--", &owner]) {
                    if let Some(version) = raw.strip_prefix(&format!("{owner} ")) {
                        if !version.contains('\n')
                            && !version.is_empty()
                            && version.len() <= 160
                            && version.bytes().next().is_some_and(|c| c.is_ascii_digit())
                            && version.bytes().all(|c| {
                                c.is_ascii_alphanumeric()
                                    || matches!(c, b'.' | b'+' | b'_' | b'~' | b':' | b'-')
                            })
                        {
                            installation.version = version.into();
                        }
                    }
                }
                installation.kind = "package".into();
                installation.package = owner;
                return installation;
            }
        }
    }
    if installation.kind.is_empty() {
        let mut source = source_installation(&canonical);
        source.arch = installation.arch;
        source.helper = installation.helper;
        return source;
    }
    installation
}

fn compare_package(feed: &Feed, installation: &Installation) -> Option<i32> {
    let released = feed.packages.get(&installation.package)?;
    let vercmp = find_program("vercmp")?;
    let output = run(&vercmp, &[released, &installation.version])?;
    if output.is_empty()
        || output.bytes().any(|c| !c.is_ascii_digit() && c != b'-')
        || output.matches('-').count() > 1
        || output.starts_with('-') && output.len() == 1
        || output.contains('-') && !output.starts_with('-')
    {
        return None;
    }
    output.parse().ok()
}

#[tauri::command]
pub fn updates_view(updates: TauriState<'_, Updates>) -> UpdatesView {
    updates.view()
}

#[tauri::command]
pub fn updates_check(updates: TauriState<'_, Updates>) {
    updates.send(Request::Check(false));
}

#[tauri::command]
pub async fn updates_set_channel(
    updates: TauriState<'_, Updates>,
    channel: Channel,
) -> Result<(), String> {
    if updates.0.service_mode {
        return updates.preferences(Some(channel), None).await;
    }
    updates.invalidate();
    updates.send(Request::Channel(channel));
    Ok(())
}

#[tauri::command]
pub async fn updates_set_automatic(
    updates: TauriState<'_, Updates>,
    on: bool,
) -> Result<(), String> {
    if updates.0.service_mode {
        return updates.preferences(None, Some(on)).await;
    }
    if !on {
        updates.invalidate();
    }
    updates.send(Request::Automatic(on));
    Ok(())
}

#[tauri::command]
pub fn updates_open(updates: TauriState<'_, Updates>, target: String) -> Result<(), String> {
    let state = lock(&updates.0.state).clone();
    let url = match target.as_str() {
        "release" if state.feed.valid => state.feed.release_url.clone(),
        "download" => download_url(&state.installation, &state.feed, running_platform()),
        "instructions" => installation_instructions_url(&state.installation, running_platform()),
        _ => String::new(),
    };
    if url.is_empty() {
        return Err("That update link is not available.".into());
    }
    crate::settings::open_url(&url).map_err(|error| format!("Could not open {url}: {error}"))
}

#[tauri::command(async)]
pub fn updates_copy_command(
    app: AppHandle,
    updates: TauriState<'_, Updates>,
) -> Result<(), String> {
    let state = lock(&updates.0.state);
    let command = update_command(&state.installation, state.channel);
    drop(state);
    if command.is_empty() {
        return Err("No update command is available for this installation.".into());
    }
    crate::clipboard::copy_text(app, command)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const RESTART_SCRIPT: &str = "SECONDS=0; while kill -0 \"$1\" 2>/dev/null; do if [ \"$SECONDS\" -ge 120 ]; then exit 0; fi; sleep 0.2; done; exec /usr/bin/open -a \"$2\" --args --sessions";

#[cfg(target_os = "macos")]
fn restart_helper(bundle: &Path, pid: u32) -> std::io::Result<()> {
    use std::process::Stdio;
    crate::child_environment::command("/bin/sh")
        .args([
            "-c",
            RESTART_SCRIPT,
            "zerus-restart",
            &pid.to_string(),
            &bundle.to_string_lossy(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn restart_bundle(executable: &Path, ready: bool) -> Result<PathBuf, String> {
    if !ready {
        return Err("No replacement bundle is ready.".into());
    }
    executable
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|bundle| {
            bundle
                .extension()
                .is_some_and(|extension| extension == "app")
        })
        .map(Path::to_path_buf)
        .ok_or_else(|| "The running app bundle could not be found.".to_owned())
}

#[cfg(target_os = "macos")]
pub fn restart_app(app: &AppHandle) -> Result<(), String> {
    let updates = app.state::<Updates>();
    let bundle = restart_bundle(&updates.0.executable, updates.view().restart_required)?;
    restart_helper(&bundle, std::process::id()).map_err(|error| error.to_string())?;
    crate::exit_flush::quit_with_flush(app, 0);
    Ok(())
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub fn updates_restart(app: AppHandle) -> Result<(), String> {
    restart_app(&app)
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn updates_restart() -> Result<(), String> {
    Err("Restart to finish update is available on macOS only.".into())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn login_item_restart(
    executable: &Path,
    restart: bool,
    start: impl FnOnce(&Path) -> Result<(), String>,
) -> Option<String> {
    if !restart {
        return None;
    }
    restart_bundle(executable, true)
        .and_then(|bundle| start(&bundle))
        .err()
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn updates_fix_login_item(
    app: AppHandle,
    restart: bool,
) -> Result<Option<String>, String> {
    let updates = app.state::<Updates>().inner().clone();
    let executable = updates.0.executable.clone();
    tauri::async_runtime::spawn_blocking(move || crate::login_item::fix(&executable))
        .await
        .map_err(|error| error.to_string())??;
    lock(&updates.0.state).login_item_deprioritized = false;
    updates.changed();
    let restart_error = login_item_restart(&updates.0.executable, restart, |bundle| {
        restart_helper(bundle, std::process::id()).map_err(|error| error.to_string())
    });
    if restart_error.is_none() && restart {
        crate::exit_flush::quit_with_flush(&app, 0);
    }
    Ok(restart_error)
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn updates_fix_login_item() -> Result<(), String> {
    Err("Fix login item is available on macOS only.".into())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn notification_current(app: &AppHandle, token: &str) -> bool {
    zerus_client_core::updates::is_update_token(token)
        && app
            .try_state::<Updates>()
            .is_some_and(|updates| updates.notification_current(token))
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn open_page(app: &AppHandle) {
    if let Err(error) = crate::lifecycle::request_main(app) {
        eprintln!("zerus-client: could not open Updates: {error}");
    }
    crate::desk::open_updates(app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    static SCRATCH: AtomicUsize = AtomicUsize::new(0);

    fn stable() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schemaVersion": 1,
            "product": "zerus",
            "platform": "desktop",
            "channel": "stable",
            "versionName": "0.47.0",
            "releaseId": 1234,
            "publishedAt": "2026-10-29T12:00:00Z",
            "sourceCommit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "releaseUrl": "https://github.com/ufna/zerus/releases/tag/v0.47.0",
            "packages": {},
            "artifacts": [{
                "kind": "arch-package",
                "name": "zerus-ade-bin-0.47.0-1-x86_64.pkg.tar.zst",
                "url": "https://zerus.dev/downloads/v0.47.0/zerus-ade-bin-0.47.0-1-x86_64.pkg.tar.zst",
                "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "size": 100,
                "os": "linux",
                "arch": "x86_64"
            }]
        }))
        .unwrap()
    }

    fn dev() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "product": "zerus",
            "platform": "desktop",
            "channel": "dev",
            "channelTag": "dev",
            "versionName": "0.47.0",
            "buildId": 12345,
            "buildNumber": 50,
            "buildUrl": "https://github.com/ufna/zerus/actions/runs/12345",
            "publishedAt": "2026-10-29T12:00:00Z",
            "sourceCommit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "artifacts": [
                {"kind": "arch-package", "name": "zerus.pkg.tar.zst", "url": "https://zerus.dev/downloads/dev-desktop-12345/zerus.pkg.tar.zst", "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "size": 100, "os": "linux", "arch": "x86_64"},
                {"kind": "source", "name": "zerus-source.tar.gz", "url": "https://zerus.dev/downloads/dev-desktop-12345/zerus-source.tar.gz", "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc", "size": 100, "os": "any", "arch": "source"}
            ]
        }))
        .unwrap()
    }

    struct Fixture {
        service: Updates,
        receiver: Option<Receiver<Request>>,
        settings: Settings,
        notices: Arc<Mutex<Vec<Notice>>>,
        views: Arc<Mutex<Vec<UpdatesView>>>,
    }

    impl Fixture {
        fn new(fetch: Fetch, configure: impl FnOnce(&Settings)) -> Self {
            Self::with_identity(fetch, configure, false)
        }

        fn with_identity(
            fetch: Fetch,
            configure: impl FnOnce(&Settings),
            dev_identity: bool,
        ) -> Self {
            Self::with_modes(fetch, configure, dev_identity, false)
        }
        fn with_modes(
            fetch: Fetch,
            configure: impl FnOnce(&Settings),
            dev_identity: bool,
            service_mode: bool,
        ) -> Self {
            let root = std::env::temp_dir().join(format!(
                "zerus-native-updates-{}-{}",
                std::process::id(),
                SCRATCH.fetch_add(1, AtomicOrdering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            let settings = Settings::open(root.join("settings.json"), None);
            configure(&settings);
            let notices = Arc::new(Mutex::new(Vec::new()));
            let views = Arc::new(Mutex::new(Vec::new()));
            let hooks = Hooks {
                fetch,
                detect: Arc::new(|_| Installation {
                    kind: "source".into(),
                    ..Installation::default()
                }),
                compare: Arc::new(|_, _| None),
                changed: {
                    let views = Arc::clone(&views);
                    Arc::new(move |view| views.lock().unwrap().push(view))
                },
                notify: {
                    let notices = Arc::clone(&notices);
                    Arc::new(move |notice| notices.lock().unwrap().push(notice))
                },
                now: Arc::new(|| 1000),
                maintenance_changed: Arc::new(|_| {}),
            };
            let build = Build {
                app_version: "0.46.0".into(),
                installed: zerus_client_core::text::Text::verbatim("0.46.0"),
                dev_number: 0,
                source_commit: String::new(),
            };
            let (service, receiver) = Updates::new(
                settings.clone(),
                build,
                root.join("hgs-tray"),
                dev_identity,
                service_mode,
                if dev_identity {
                    "dev.zerus.next"
                } else {
                    "com.hgdev.hgs-tray"
                }
                .into(),
                hooks,
            );
            Self {
                service,
                receiver: Some(receiver),
                settings,
                notices,
                views,
            }
        }

        fn start(&mut self) {
            spawn(
                self.service.clone(),
                self.receiver.take().expect("the worker starts once"),
            );
        }
    }

    fn until(fixture: &Fixture, wanted: impl Fn(&UpdatesView) -> bool) -> UpdatesView {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let view = fixture.service.view();
            if wanted(&view) {
                return view;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "update state did not settle: {view:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn the_worker_verifies_stores_and_notifies_once() {
        let mut fixture = Fixture::new(Arc::new(|_| Ok(stable())), |_| {});
        fixture.start();
        let view = until(&fixture, |view| {
            view.available && !view.busy && fixture.notices.lock().unwrap().len() == 1
        });
        assert_eq!(view.latest.english(), "0.47.0");
        assert_eq!(view.last_checked, "1970-01-01T00:16:40Z");
        assert_eq!(view.release_label.english(), "View release");
        assert!(!view.cached && view.error.is_none());
        assert_eq!(fixture.notices.lock().unwrap().len(), 1);
        let notice = fixture.notices.lock().unwrap()[0].clone();
        assert_eq!(notice.token, "zerus-update:v1:stable:1234");
        assert_eq!(notice.title, "Zerus update available");
        assert!(fixture.service.notification_current(&notice.token));
        assert_eq!(
            fixture.settings.number("updates/lastAttempt/stable"),
            1000.0
        );
        assert_eq!(
            fixture.settings.number("updates/lastSuccess/stable"),
            1000.0
        );
        assert!(
            zerus_client_core::updates::parse_feed(
                fixture.settings.text("updates/catalog/stable").as_bytes(),
                Channel::Stable
            )
            .valid
        );
        fixture.service.check(false);
        assert_eq!(fixture.notices.lock().unwrap().len(), 1);
        assert!(fixture
            .views
            .lock()
            .unwrap()
            .iter()
            .any(|view| view.heading.english() == "Checking for updates…"));
    }

    #[test]
    fn a_failed_check_keeps_a_restored_verified_catalog_without_notifying() {
        let fixture = Fixture::new(Arc::new(|_| Err("offline".into())), |settings| {
            settings
                .set(
                    "updates/catalog/stable",
                    String::from_utf8(stable()).unwrap().into(),
                )
                .unwrap();
            settings
                .set("updates/lastSuccess/stable", 900.into())
                .unwrap();
        });
        fixture.service.restore();
        fixture.service.detect();
        assert!(fixture.service.view().cached);
        assert!(fixture.service.view().available);
        assert!(fixture.notices.lock().unwrap().is_empty());
        fixture.service.check(false);
        let view = fixture.service.view();
        assert!(view.cached && view.available);
        assert_eq!(
            view.error.as_ref().map(|t| t.english()).as_deref(),
            Some("Check failed. Try again later.")
        );
        assert_eq!(view.last_checked, "1970-01-01T00:15:00Z");
        assert!(fixture.notices.lock().unwrap().is_empty());
    }

    #[test]
    fn a_channel_change_drops_the_old_fetch_before_it_publishes_or_notifies() {
        let (started, waiting) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Arc::new(Mutex::new(released));
        let fetch: Fetch = Arc::new(move |_| {
            started.send(()).unwrap();
            released.lock().unwrap().recv().unwrap();
            Ok(stable())
        });
        let fixture = Fixture::new(fetch, |settings| {
            settings.set("updates/automatic", false.into()).unwrap();
        });
        fixture.service.detect();
        let worker = {
            let service = fixture.service.clone();
            std::thread::spawn(move || service.check(false))
        };
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        fixture.service.set_channel(Channel::Nightly);
        release.send(()).unwrap();
        worker.join().unwrap();
        let view = fixture.service.view();
        assert_eq!(view.channel, Channel::Nightly);
        assert!(!view.feed_valid_for_test());
        assert!(!view.available);
        assert!(fixture.notices.lock().unwrap().is_empty());
        assert!(!fixture.settings.contains("updates/catalog/stable"));
    }

    #[test]
    fn a_dev_build_uses_the_build_identity_and_view_build_action() {
        let fixture = Fixture::new(Arc::new(|_| Ok(dev())), |settings| {
            settings.set("updates/channel", "dev".into()).unwrap();
        });
        fixture.service.detect();
        fixture.service.check(false);
        let view = fixture.service.view();
        assert!(view.available);
        assert_eq!(view.version, "0.47.0-dev.50");
        assert_eq!(view.release_label.english(), "View build");
        assert_eq!(
            view.download_label.as_ref().map(|t| t.english()).as_deref(),
            Some("Download dev source")
        );
        assert_eq!(
            fixture.notices.lock().unwrap()[0].token,
            "zerus-update:v2:dev:12345"
        );
    }

    #[test]
    fn a_source_dev_identity_stays_out_of_notices_and_workspace_prompts() {
        let fixture = Fixture::with_identity(Arc::new(|_| Ok(stable())), |_| {}, true);
        fixture.service.detect();
        fixture.service.check(false);
        let view = fixture.service.view();
        assert!(view.available && view.quiet);
        assert!(fixture.notices.lock().unwrap().is_empty());
        assert!(!fixture
            .settings
            .flag("updates/notified/zerus-update:v1:stable:1234"));
        assert!(quiet_prompts(
            true,
            &Installation {
                kind: "unknown".into(),
                ..Installation::default()
            }
        ));
        assert!(!quiet_prompts(
            true,
            &Installation {
                kind: "package".into(),
                ..Installation::default()
            }
        ));
        assert!(!quiet_prompts(
            false,
            &Installation {
                kind: "source".into(),
                ..Installation::default()
            }
        ));
    }

    #[test]
    fn login_item_restart_failure_is_a_result_not_a_fix_error() {
        let executable = Path::new("/tmp/Zerus.app/Contents/MacOS/zerus-client");
        assert_eq!(
            login_item_restart(executable, false, |_| unreachable!()),
            None
        );
        assert_eq!(
            login_item_restart(executable, true, |_| Err("restart refused".into())),
            Some("restart refused".into())
        );
    }

    #[test]
    fn restart_waits_for_a_real_ready_bundle_and_the_helper_times_out() {
        let executable = Path::new("/tmp/Zerus.app/Contents/MacOS/zerus-client");
        assert_eq!(
            restart_bundle(executable, true).unwrap(),
            Path::new("/tmp/Zerus.app")
        );
        assert_eq!(
            restart_bundle(executable, false).unwrap_err(),
            "No replacement bundle is ready."
        );
        assert!(restart_bundle(Path::new("/tmp/zerus-client"), true).is_err());
        assert!(RESTART_SCRIPT.starts_with("SECONDS=0; while kill -0"));
        assert!(RESTART_SCRIPT.contains("[ \"$SECONDS\" -ge 120 ]"));
        assert!(RESTART_SCRIPT.contains("then exit 0; fi; sleep 0.2; done; exec /usr/bin/open"));
        #[cfg(unix)]
        {
            let script = RESTART_SCRIPT
                .replace("-ge 120", "-ge 0")
                .replace("exec /usr/bin/open", "exec /bin/echo");
            let output = crate::child_environment::command("/bin/sh")
                .args([
                    "-c",
                    &script,
                    "zerus-restart-test",
                    &std::process::id().to_string(),
                    "unused",
                ])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert!(
                output.stdout.is_empty(),
                "a timed-out helper reopened the app"
            );
        }
    }

    #[test]
    fn replay_views_are_bounded_json_and_replace_the_initial_snapshot() {
        let fixture = Fixture::new(Arc::new(|_| Ok(stable())), |_| {});
        fixture.service.detect();
        let view = fixture.service.view();
        let root = std::env::temp_dir().join(format!(
            "zerus-native-updates-replay-{}-{}",
            std::process::id(),
            SCRATCH.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("view.json");
        std::fs::write(&path, serde_json::to_vec(&view).unwrap()).unwrap();
        assert_eq!(read_replay_view(&path).unwrap(), view);
        *lock(&fixture.service.0.replay_view) = Some(view.clone());
        assert_eq!(fixture.service.view(), view);
        std::fs::write(&path, vec![b' '; REPLAY_VIEW_LIMIT as usize + 1]).unwrap();
        assert_eq!(
            read_replay_view(&path).unwrap_err(),
            "the replay update view exceeds 64 KiB"
        );
    }

    #[test]
    fn local_checkout_detection_does_not_claim_package_ownership() {
        let root = std::env::temp_dir().join(format!(
            "zerus-native-updates-source-{}-{}",
            std::process::id(),
            SCRATCH.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        let executable = root.join("target").join("hgs-tray");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, "").unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        assert_eq!(source_installation(&executable).kind, "source");
        assert_eq!(
            source_installation(Path::new("/nonexistent/zerus-updates/hgs-tray")).kind,
            "unknown"
        );
    }

    impl UpdatesView {
        fn feed_valid_for_test(&self) -> bool {
            self.release_enabled
        }
    }
    fn service_fixture(fetch: Fetch) -> Fixture {
        Fixture::with_modes(
            fetch,
            |settings| {
                settings.set("updates/automatic", true.into()).unwrap();
            },
            false,
            true,
        )
    }
    fn projected_frame(fixture: &Fixture) -> serde_json::Value {
        use zerus_client_core::maintenance as dto;
        let feed = zerus_client_core::updates::parse_feed(&stable(), Channel::Stable);
        serde_json::json!({"t":"status","epoch":"fixture-epoch","value":{"maintenance":dto::Projection {
            abi:dto::ABI.into(),epoch:"fixture-epoch".into(),revision:Some(1),ready:true,error:None,
            configuration:Some(dto::Configuration {update:Some(fixture.service.native_configuration()),recovery:None}),
            updates:dto::UpdateProjection {ready:true,current_origin:true,available:true,channel:Some(Channel::Stable),automatic:true,
                last_success:900,feed,installation:Installation {kind:"source".into(),..Installation::default()},..dto::UpdateProjection::default()},
            recovery:dto::RecoveryProjection::default(),
        }}})
    }
    #[test]
    fn service_mode_never_auto_fetches_on_start_channel_or_enable() {
        let mut fixture =
            service_fixture(Arc::new(|_| panic!("automatic UI fetch in service mode")));
        fixture.start();
        fixture.service.send(Request::Channel(Channel::Nightly));
        fixture.service.send(Request::Automatic(true));
        until(&fixture, |view| {
            view.channel == Channel::Nightly && view.error.is_some()
        });
        fixture.service.check_automatic();
        assert!(fixture.notices.lock().unwrap().is_empty());
    }
    #[test]
    fn service_mode_manual_check_stays_local_and_never_posts_native_notice() {
        let mut fixture = service_fixture(Arc::new(|_| Ok(stable())));
        fixture.start();
        fixture.service.send(Request::Check(false));
        until(&fixture, |view| !view.busy && view.available);
        assert!(fixture.notices.lock().unwrap().is_empty());
    }
    #[test]
    fn service_preferences_ack_waits_for_startup_and_the_complete_native_preference_pair() {
        let mut fixture = service_fixture(Arc::new(|_| panic!("no automatic GUI fetch")));
        let (entered, started) = mpsc::sync_channel(1);
        let (release, proceed) = mpsc::sync_channel(1);
        let proceed = Mutex::new(proceed);
        Arc::get_mut(&mut fixture.service.0).unwrap().hooks.detect = Arc::new(move |_| {
            entered.send(()).unwrap();
            lock(&proceed).recv_timeout(Duration::from_secs(5)).unwrap();
            Installation {
                kind: "source".into(),
                ..Installation::default()
            }
        });
        fixture.start();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        tauri::async_runtime::block_on(async {
            let preferences = async {
                fixture
                    .service
                    .preferences(Some(Channel::Nightly), Some(false))
                    .await?;
                fixture.service.configuration_for_intent().await
            };
            tokio::pin!(preferences);
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut preferences)
                    .await
                    .is_err()
            );
            assert_eq!(
                fixture.service.native_configuration().channel,
                Channel::Stable
            );
            release.send(()).unwrap();
            let native = preferences.await.unwrap();
            assert_eq!(native.channel, Channel::Nightly);
            assert!(!native.automatic);
            assert_eq!(fixture.settings.text("updates/channel"), "nightly");
            assert!(!fixture.settings.flag("updates/automatic"));
            assert!(fixture.notices.lock().unwrap().is_empty());
        });
    }
    #[test]
    fn service_projection_updates_existing_model_with_native_gui_build() {
        let fixture = service_fixture(Arc::new(|_| panic!("no fetch from watch")));
        let frame = projected_frame(&fixture);
        fixture.service.receive_projection(Some(&frame));
        fixture.service.consume_projection();
        assert!(fixture.service.view().available);
        assert_eq!(
            fixture.service.view().installed,
            fixture.service.0.build.installed
        );
        assert!(fixture.notices.lock().unwrap().is_empty());
        fixture.service.receive_projection(None);
        fixture.service.consume_projection();
        let view = fixture.service.view();
        assert!(!view.available);
        assert!(view.cached);
        assert!(view.error.is_some());
    }
    #[test]
    fn service_projection_rejects_wrong_gui_epoch_and_missing_abi_without_polling() {
        let fixture = service_fixture(Arc::new(|_| panic!("no fallback fetch")));
        let frame = projected_frame(&fixture);
        for field in ["gui", "epoch", "abi"] {
            let mut value = frame.clone();
            match field {
                "gui" => {
                    value["value"]["maintenance"]["configuration"]["update"]["gui"]["app_version"] =
                        "999.0.0".into()
                }
                "epoch" => value["value"]["maintenance"]["epoch"] = "other".into(),
                _ => {
                    value["value"]["maintenance"]
                        .as_object_mut()
                        .unwrap()
                        .remove("abi");
                }
            }
            fixture.service.receive_projection(Some(&value));
            fixture.service.consume_projection();
            assert!(!fixture.service.view().available);
            assert!(fixture.service.view().error.is_some());
        }
    }
    #[test]
    fn bounded_projection_slot_cannot_lose_disconnect_to_an_older_positive() {
        let fixture = service_fixture(Arc::new(|_| panic!("no fetch")));
        let frame = projected_frame(&fixture);
        fixture.service.receive_projection(Some(&frame));
        fixture.service.receive_projection(None);
        assert_eq!(
            fixture.receiver.as_ref().unwrap().try_iter().count(),
            1,
            "one wake, never an unbounded watch queue"
        );
        fixture.service.consume_projection();
        assert!(!fixture.service.view().available);
        fixture.service.receive_projection(Some(&frame));
        fixture.service.consume_projection();
        assert!(
            fixture.service.view().available,
            "next fresh status recovers"
        );
    }
    #[test]
    fn rapid_status_frames_keep_the_latest_projection_without_another_heartbeat() {
        let fixture = service_fixture(Arc::new(|_| panic!("no fallback fetch")));
        let mut frame = projected_frame(&fixture);
        frame["value"]["maintenance"]["updates"]["available"] = false.into();
        fixture.service.receive_projection(Some(&frame));
        frame["t"] = "heartbeat".into();
        frame["value"]["maintenance"]["updates"]["available"] = true.into();
        fixture.service.receive_projection(Some(&frame));
        assert_eq!(fixture.receiver.as_ref().unwrap().try_iter().count(), 1);
        fixture.service.consume_projection();
        assert!(fixture.service.view().available);
        assert!(fixture.service.view().error.is_none());
    }
    #[test]
    fn a_contended_projection_slot_refuses_stale_data_without_waiting() {
        let fixture = service_fixture(Arc::new(|_| panic!("no fallback fetch")));
        let frame = projected_frame(&fixture);
        fixture.service.receive_projection(Some(&frame));
        let receiver = lock(&fixture.service.0.projection_receiver);
        fixture.service.receive_projection(None);
        drop(receiver);
        fixture.service.consume_projection();
        assert!(!fixture.service.view().available);
        assert!(fixture.service.view().error.is_some());
    }
    #[test]
    fn older_service_cache_does_not_replace_a_newer_manual_check() {
        let fixture = service_fixture(Arc::new(|_| Ok(stable())));
        fixture.service.detect();
        fixture.service.check(false);
        let manual = fixture.service.view();
        let mut frame = projected_frame(&fixture);
        frame["value"]["maintenance"]["updates"]["feed"]["version"] = "0.1.0".into();
        fixture.service.receive_projection(Some(&frame));
        fixture.service.consume_projection();
        assert_eq!(fixture.service.view().version, manual.version);
        assert_eq!(fixture.service.view().last_checked, manual.last_checked);
    }
}
