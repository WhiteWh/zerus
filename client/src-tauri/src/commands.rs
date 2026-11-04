//! The commands the frontend calls: `start`, `next_batch`, `follow`,
//! `unfollow` and `metrics`. Blocking work runs off the main thread.
//! `next_batch` hands the followed processes to the Processes hub
//! (`processes.rs`) and sends its view as `processes_view`.
//!
//! Data comes from zerus-client-core (feature `live`): `Fleet` runs
//! `hgs [@host] watch --json` per computer, or, with `ZERUS_CLIENT_REPLAY`
//! set, a synthetic fixture replays through the same `CatalogState` and
//! coalescer (see `replay.rs`). `ZERUS_CLIENT_HGS` names the `hgs` to run.

#[cfg(feature = "live")]
use std::sync::Arc;

use serde_json::Value;
use tauri::State;

#[derive(Default)]
pub struct AppState {
    #[cfg(feature = "live")]
    live: Arc<live::Live>,
}

#[cfg(feature = "live")]
#[tauri::command]
pub fn fleet_refresh(state: State<'_, AppState>) -> usize {
    state.refresh()
}

#[cfg(not(feature = "live"))]
#[tauri::command]
pub fn fleet_refresh() -> usize {
    0
}

#[cfg(feature = "live")]
impl AppState {
    #[allow(dead_code)] // No authenticated producer invokes role freeze yet.
    pub(crate) fn freeze_fleet(&self) -> Result<live::FrozenFleet, live::barrier::Refused> {
        self.live.freeze()
    }
    /// A manual offline Refresh nudges only its existing source; never starts
    /// another fleet or changes an online source.
    pub fn reconnect(&self, computer: &str) -> bool {
        self.live.reconnect(computer)
    }
    /// One visible Refresh: one shared service subscription, or the legacy
    /// Fleet's existing offline sources. Display choices never multiply it.
    pub fn refresh(&self) -> usize {
        self.live.refresh()
    }

    /// The change pump, whose mirror is the screens hub's view of the fleet.
    pub fn pump(&self) -> crate::pump::Pump {
        self.live.pump.clone()
    }

    /// Starts the data source now, before any window loads: the menu bar and
    /// notifications need the fleet while the window is closed. A later
    /// `start` from the webview finds it running and resends the catalog.
    pub fn start_source(&self) -> Result<String, String> {
        self.live.start()
    }

    pub fn desktop_service(&self) -> Option<std::sync::Arc<crate::desktop_service::Service>> {
        self.live.desktop_service()
    }
    pub(crate) fn set_pending_read_source(&self, actions: crate::actions::Actions) {
        let _ = self.live.pending_read_source.set(actions);
    }
    pub(crate) fn register_notification_controls(
        &self,
        setup: crate::desktop_notification_controls::Setup,
    ) {
        let _ = self.live.notification_controls.set(setup);
    }
    pub(crate) fn set_retained_import_source(
        &self,
        inputs: crate::desktop_service::retained_import::Inputs,
    ) {
        let _ = self.live.retained_import_source.set(inputs);
    }

    pub(crate) fn creation_capture(
        &self,
    ) -> Option<Arc<zerus_client_core::migration::preload::Capture>> {
        self.live
            .retained_import_source
            .get()
            .map(|inputs| inputs.creations.clone())
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    pub fn set_desktop_responder(&self, responder: Arc<zerus_client_core::desktop::Responder>) {
        let _ = self.live.desktop_responder.set(responder);
    }

    #[allow(dead_code)] // .66.3.36 registers its maintenance projection after that adapter lands.
    pub fn register_service_projection(
        &self,
        observer: crate::desktop_service::Projection,
    ) -> Result<(), String> {
        self.live.register_service_projection(observer)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn register_service_activation(
        &self,
        sink: crate::desktop_service::activation::Sink,
    ) -> Result<(), String> {
        self.live.register_service_activation(sink)
    }

    /// The peers this computer's listing names; None before a listing named
    /// them, and for a replay. Service mode supplies verified display-only
    /// runtime names, retaining last-known choices while unavailable.
    pub fn peers(&self) -> Option<std::collections::BTreeSet<String>> {
        self.live.peers()
    }

    /// The organization hub: `start` gives it the snapshot source and the
    /// organization updates. Set once, before the webview starts the source.
    pub fn set_organization(&self, organization: crate::organization::Organization) {
        let _ = self.live.organization.set(organization);
    }

    /// Set once before the source starts. The host's observer never blocks
    /// the source drain or takes the pump/organization locks.
    pub fn set_creations(&self, host: crate::creations::CreationHost) {
        let _ = self.live.creations.set(host);
    }

    /// Applies the preferences the host confirmed for a Mute or Unmute to the
    /// session's row at once, for the window and the menu bar alike
    /// (`FleetState::applyNotificationPreferences`). A replay has no hosts.
    pub fn apply_notifications(
        &self,
        target: &zerus_client_core::actions::Target,
        preferences: &Value,
    ) {
        self.live.apply_notifications(target, preferences);
    }

    /// Applies the resumed run to the replay after the fake accepted Resume.
    pub fn smoke_resume(&self, key: &str, ready: Value) -> Result<(), String> {
        self.live.smoke_resume(key, ready)
    }

    /// Follows the processes of the session the Processes tab shows.
    pub fn follow_processes(&self, target: &crate::processes::ProcessTarget) -> Result<(), String> {
        self.live.follow_processes(target)
    }

    pub fn set_processes_interval(
        &self,
        target: &crate::processes::ProcessTarget,
        ms: u64,
    ) -> Result<(), String> {
        self.live.set_processes_interval(target, ms)
    }

    pub fn refresh_processes(
        &self,
        target: &crate::processes::ProcessTarget,
    ) -> Result<(), String> {
        self.live.refresh_processes(target)
    }

    pub fn unfollow_processes(&self, target: &crate::processes::ProcessTarget) {
        self.live.unfollow_processes(target);
    }
}

#[cfg(feature = "live")]
mod live {
    use std::collections::BTreeSet;
    use std::sync::{mpsc, Arc, Mutex, OnceLock, PoisonError};
    use std::time::Duration;

    use serde_json::Value;
    use zerus_client_core::{default_hgs, ComputerId, Fleet, FollowError, OrganizationSnapshot};
    use zerus_watch_protocol::SessionRef;

    use crate::organization::Organization;
    use crate::processes::ProcessTarget;
    use crate::pump::Pump;
    use crate::replay::Replay;

    enum Source {
        Fleet(Fleet),
        Replay(Replay),
        Service(Arc<crate::desktop_service::Service>),
    }

    pub struct Live {
        pub pump: Pump,
        source: Mutex<Option<Source>>,
        pub organization: OnceLock<Organization>,
        pub creations: OnceLock<crate::creations::CreationHost>,
        pub(super) desktop_responder: OnceLock<Arc<zerus_client_core::desktop::Responder>>,
        pub(super) pending_read_source: OnceLock<crate::actions::Actions>,
        pub(super) notification_controls: OnceLock<crate::desktop_notification_controls::Setup>,
        pub(super) retained_import_source:
            OnceLock<crate::desktop_service::retained_import::Inputs>,
        service_projections: Mutex<Vec<crate::desktop_service::Projection>>,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        service_activation: OnceLock<crate::desktop_service::activation::Sink>,
        tasks_project: Mutex<Option<String>>,
        controls: zerus_client_core::desktop::drain::Gate,
        resources: zerus_client_core::desktop::drain::Gate,
        #[cfg_attr(not(unix), allow(dead_code))]
        barrier: zerus_client_core::desktop::drain::Gate,
        core_control: OnceLock<zerus_client_core::fleet::FleetControl>,
    }
    impl Default for Live {
        fn default() -> Self {
            use zerus_client_core::desktop::drain::Gate;
            Self {
                pump: Pump::default(),
                source: Mutex::new(None),
                organization: OnceLock::new(),
                creations: OnceLock::new(),
                desktop_responder: OnceLock::new(),
                pending_read_source: OnceLock::new(),
                retained_import_source: OnceLock::new(),
                service_projections: Mutex::new(Vec::new()),
                notification_controls: OnceLock::new(),
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                service_activation: OnceLock::new(),
                tasks_project: Mutex::new(None),
                controls: Gate::new(64).unwrap(),
                resources: Gate::new(1).unwrap(),
                barrier: Gate::new(1).unwrap(),
                core_control: OnceLock::new(),
            }
        }
    }
    impl Live {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        pub(super) fn register_service_activation(
            &self,
            sink: crate::desktop_service::activation::Sink,
        ) -> Result<(), String> {
            let source = self.source.lock().unwrap_or_else(PoisonError::into_inner);
            if source.is_some() {
                return Err("Service activation must be registered before startup.".into());
            }
            self.service_activation
                .set(sink)
                .map_err(|_| "Service activation is already registered.".into())
        }
        #[allow(dead_code)] // Reserved setup hook for the .66.3.36 maintenance consumer.
        pub(super) fn register_service_projection(
            &self,
            observer: crate::desktop_service::Projection,
        ) -> Result<(), String> {
            let source = self.source.lock().unwrap_or_else(PoisonError::into_inner);
            if source.is_some() {
                return Err("Service projections must be registered before startup.".into());
            }
            let mut projections = self
                .service_projections
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if projections.len() >= 8 {
                return Err("Too many service projections.".into());
            }
            projections.push(observer);
            Ok(())
        }
        fn admit(&self) -> Result<zerus_client_core::desktop::drain::Charge, String> {
            self.controls
                .admit(zerus_client_core::desktop::drain::Role::Fleet)
                .map_err(|_| "Background source is not accepting changes.".into())
        }
    }
    pub(crate) use barrier::FrozenFleet;
    pub(crate) mod barrier;

    /// Follows this computer's organization: the catalog is shared, so its
    /// copy is the one the hub edits. A Fleet starts once, so this logs at
    /// most once.
    fn follow_organization(fleet: &Fleet) {
        if let Err(error) = fleet.follow_organization(&ComputerId::local()) {
            eprintln!("zerus-client: project sync cannot follow this computer's catalog: {error}");
        }
    }

    fn replay_interval() -> Duration {
        let ms = std::env::var("ZERUS_CLIENT_REPLAY_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(900);
        Duration::from_millis(ms)
    }

    impl Live {
        /// The board follows one local replica project, including across reconnects.
        pub fn follow_tasks(&self, project: Option<String>) -> Result<(), String> {
            let _admission = self.admit()?;
            if project
                .as_deref()
                .is_some_and(|p| !zerus_client_core::tasks::valid_project(p))
            {
                return Err("Invalid task project.".into());
            }
            let source = self.source.lock().unwrap_or_else(PoisonError::into_inner);
            let mut current = self
                .tasks_project
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match source.as_ref() {
                Some(Source::Fleet(fleet)) => {
                    if let Some(old) = current.take() {
                        fleet.unfollow_tasks(&ComputerId::local(), &old);
                    }
                    if let Some(next) = project {
                        fleet
                            .follow_tasks(&ComputerId::local(), &next)
                            .map_err(|e| e.to_string())?;
                        *current = Some(next);
                    }
                }
                Some(Source::Service(service)) => {
                    if let Some(old) = current.take() {
                        service.unfollow_detail("", zerus_watch_protocol::topic::TASKS, &old);
                    }
                    if let Some(next) = project {
                        service.follow_detail(
                            "",
                            zerus_watch_protocol::Subscription {
                                topic: zerus_watch_protocol::topic::TASKS.into(),
                                project: Some(next.clone()),
                                ..Default::default()
                            },
                        )?;
                        *current = Some(next);
                    }
                }
                _ => return Err("Tasks need a live hgs event stream.".into()),
            }
            Ok(())
        }
        pub fn refresh(&self) -> usize {
            let Ok(_admission) = self.admit() else {
                return 0;
            };
            match self
                .source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                Some(Source::Service(service)) => service.reconnect("") as usize,
                Some(Source::Fleet(fleet)) => {
                    let local = fleet.reconnect(&ComputerId::local()) as usize;
                    local
                        + fleet
                            .peers()
                            .into_iter()
                            .flatten()
                            .filter(|peer| fleet.reconnect(&ComputerId::from(peer.as_str())))
                            .count()
                }
                _ => 0,
            }
        }
        pub fn reconnect(&self, computer: &str) -> bool {
            let Ok(_admission) = self.admit() else {
                return false;
            };
            match self
                .source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                Some(Source::Fleet(fleet)) => fleet.reconnect(&ComputerId::from(computer)),
                Some(Source::Service(service)) => service.reconnect(computer),
                Some(Source::Replay(_)) | None => false,
            }
        }

        /// Starts the data source once; a later call (a reloaded webview)
        /// queues the last-known catalog again. Returns "live" or "replay".
        /// This computer's organization goes to the hub, which reads its
        /// base and versions back through [`Live::organization_snapshot`].
        /// The hub asks without its own lock held, and its source is set
        /// here before `source` is taken, so the two locks never nest.
        pub fn start(self: &Arc<Self>) -> Result<String, String> {
            self.start_from(
                default_hgs,
                std::env::var_os("ZERUS_CLIENT_REPLAY")
                    .filter(|list| !list.is_empty())
                    .map(std::path::PathBuf::from),
            )
        }
        fn start_from(
            self: &Arc<Self>,
            hgs: impl FnOnce() -> std::path::PathBuf,
            replay: Option<std::path::PathBuf>,
        ) -> Result<String, String> {
            let _admission = self.admit()?;
            let organization = self.organization.get().cloned();
            let creations = self.creations.get().cloned();
            if let Some(organization) = organization
                .as_ref()
                .filter(|_| !crate::desktop_service::mode())
            {
                let live = Arc::downgrade(self);
                organization
                    .set_initial_source(Arc::new(move || live.upgrade()?.organization_snapshot()));
            }
            let mut source = self.source.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(running) = source.as_ref() {
                self.pump.resend();
                return Ok(match running {
                    Source::Fleet(_) => "live",
                    Source::Replay(_) => "replay",
                    Source::Service(_) => "service",
                }
                .into());
            }
            if crate::desktop_service::mode() {
                let (sink, changes) = mpsc::sync_channel(256);
                let pump = self.pump.clone();
                let observe = organization.clone();
                let drain = pump.drain_service(changes, move |change| {
                    if let Some(organization) = &observe {
                        organization.observe(change);
                    }
                });
                let allowed = replay.is_none()
                    || std::env::var_os("ZERUS_CLIENT_HGS").is_some_and(|value| !value.is_empty());
                let service = crate::desktop_service::Service::start_observed(
                    hgs(),
                    pump,
                    sink,
                    allowed,
                    drain,
                    self.desktop_responder.get().cloned(),
                    crate::desktop_service::Observers {
                        notification_controls: self.notification_controls.get().cloned(),
                        retained_sources: self.retained_import_source.get().cloned(),
                        projections: self
                            .service_projections
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .clone(),
                        #[cfg(any(target_os = "linux", target_os = "macos"))]
                        activation: self.service_activation.get().cloned(),
                    },
                );
                if let Some(actions) = self.pending_read_source.get() {
                    service.start_pending_read_import(actions.clone());
                }
                if let Some(organization) = &organization {
                    // The observer runs under detail admission. Its source
                    // callback must not re-enter Live.source or that admission.
                    let weak = Arc::downgrade(&service);
                    organization.set_initial_source(Arc::new(move || {
                        weak.upgrade()?.organization_snapshot()
                    }));
                    service.follow_detail(
                        "",
                        zerus_watch_protocol::Subscription {
                            topic: zerus_watch_protocol::topic::ORGANIZATION.into(),
                            ..Default::default()
                        },
                    )?;
                }
                *source = Some(Source::Service(service));
                return Ok("service".into());
            }
            let resources: Arc<dyn Send + Sync> = Arc::new(
                self.resources
                    .admit(zerus_client_core::desktop::drain::Role::Fleet)
                    .map_err(|_| "Background source is not accepting changes.")?,
            );
            let (sink, changes) = mpsc::channel();
            let (started, mode) = match replay {
                Some(list) => {
                    let replay = Replay::start(list.as_os_str(), replay_interval(), sink)
                        .map_err(|error| format!("replay: {error}"))?;
                    (Source::Replay(replay), "replay")
                }
                None => {
                    let hgs = hgs();
                    #[cfg(unix)]
                    let fleet = Fleet::start_accounted(
                        hgs,
                        sink,
                        zerus_client_core::Timing::default(),
                        resources.clone(),
                    );
                    #[cfg(not(unix))]
                    let fleet = Fleet::start(hgs, sink);
                    let _ = self.core_control.set(fleet.control());
                    // An already admitted start can register after freeze.
                    // It must see the sticky close before following anything.
                    if self.controls.snapshot().map_or(true, |state| state.closed) {
                        fleet.control().request_stop();
                    } else if organization.is_some() {
                        follow_organization(&fleet);
                    }
                    (Source::Fleet(fleet), "live")
                }
            };
            let after = creations.clone();
            self.pump.drain_accounted(
                changes,
                move |change| {
                    if let Some(organization) = &organization {
                        organization.observe(change);
                    }
                    creations.is_some() && crate::creations::CreationHost::observes(change)
                },
                move || {
                    if let Some(creations) = &after {
                        creations.catalog_changed();
                    }
                },
                resources,
            );
            *source = Some(started);
            Ok(mode.into())
        }

        pub fn desktop_service(&self) -> Option<Arc<crate::desktop_service::Service>> {
            match self
                .source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                Some(Source::Service(service)) => Some(service.clone()),
                _ => None,
            }
        }

        /// The base and versions of this computer's latest organization state.
        fn organization_snapshot(&self) -> Option<OrganizationSnapshot> {
            match self
                .source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()?
            {
                Source::Fleet(fleet) => fleet.organization_snapshot(&ComputerId::local()),
                Source::Replay(replay) => replay.organization_snapshot(),
                Source::Service(service) => service.organization_snapshot(),
            }
        }

        /// The peers this computer's listing names; None before a listing
        /// named them, and for a replay. Service rosters supply choices only;
        /// no background worker or writer permission follows from this list.
        pub fn peers(&self) -> Option<BTreeSet<String>> {
            match self
                .source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                Some(Source::Fleet(fleet)) => fleet.peers(),
                Some(Source::Service(service)) => service.peers(),
                _ => None,
            }
        }

        pub fn smoke_resume(&self, key: &str, ready: Value) -> Result<(), String> {
            let _admission = self.admit()?;
            match self
                .source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_mut()
            {
                Some(Source::Replay(replay)) => replay.resume("", key, ready),
                _ => Err("not a replay".into()),
            }
        }

        pub fn follow(
            &self,
            computer: String,
            key: String,
            session: SessionRef,
            after: Option<i64>,
            tail: u32,
        ) -> Result<(), String> {
            let _admission = self.admit()?;
            match self
                .source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                Some(Source::Fleet(fleet)) => {
                    let computer = ComputerId(computer);
                    // Following the same session again is a no-op in the core, so
                    // an older page (`after`) starts a new subscription from there.
                    if after.is_some() {
                        fleet.unfollow(&computer, &key);
                    }
                    fleet
                        .follow(&computer, &key, session, after, tail)
                        .map_err(|error| error.to_string())?
                }
                Some(Source::Replay(replay)) => replay.follow(&computer, &key, after, tail),
                Some(Source::Service(service)) => {
                    if session.key() != key {
                        return Err(FollowError::KeyMismatch.to_string());
                    }
                    service.follow_detail(
                        &computer,
                        zerus_watch_protocol::Subscription {
                            topic: zerus_watch_protocol::topic::ACTIVITY.into(),
                            session: Some(session),
                            after,
                            tail: after.is_none().then_some(tail),
                            ..Default::default()
                        },
                    )?;
                }
                None => return Err("not started".into()),
            }
            Ok(())
        }

        pub fn apply_notifications(
            &self,
            target: &zerus_client_core::actions::Target,
            preferences: &serde_json::Value,
        ) {
            let Ok(_admission) = self.admit() else { return };
            if let Some(Source::Fleet(fleet)) = self
                .source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                fleet.apply_notifications(target, preferences.clone());
            }
        }

        /// Follows a session's processes (`Fleet::follow_processes`); the
        /// session's catalog key must be the target's. A replay serves the
        /// fixture's processes state.
        pub fn follow_processes(&self, target: &ProcessTarget) -> Result<(), String> {
            let _admission = self.admit()?;
            match self
                .source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                Some(Source::Fleet(fleet)) => {
                    let computer = ComputerId(target.computer.clone());
                    let filled = |id: &str| Some(id.to_owned()).filter(|id| !id.is_empty());
                    let session = SessionRef {
                        name: target.name.clone(),
                        run_id: filled(&target.run_id),
                        conversation_id: filled(&target.conversation_id),
                    };
                    let key = fleet
                        .follow_processes(&computer, session)
                        .map_err(|error| error.to_string())?;
                    if key != target.key {
                        fleet.unfollow_processes(&computer, &key);
                        return Err(FollowError::KeyMismatch.to_string());
                    }
                    Ok(())
                }
                Some(Source::Replay(replay)) => {
                    replay.follow_processes(&target.computer, &target.key);
                    Ok(())
                }
                Some(Source::Service(service)) => {
                    let session = SessionRef {
                        name: target.name.clone(),
                        run_id: (!target.run_id.is_empty()).then(|| target.run_id.clone()),
                        conversation_id: (!target.conversation_id.is_empty())
                            .then(|| target.conversation_id.clone()),
                    };
                    if session.key() != target.key {
                        return Err(FollowError::KeyMismatch.to_string());
                    }
                    service.follow_detail(
                        &target.computer,
                        zerus_watch_protocol::Subscription {
                            topic: zerus_watch_protocol::topic::PROCESSES.into(),
                            session: Some(session),
                            interval_ms: Some(10_000),
                            ..Default::default()
                        },
                    )
                }
                None => Err("not started".into()),
            }
        }

        pub fn set_processes_interval(
            &self,
            target: &ProcessTarget,
            ms: u64,
        ) -> Result<(), String> {
            let _admission = self.admit()?;
            match self
                .source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                Some(Source::Fleet(fleet)) => fleet
                    .set_processes_interval(&ComputerId(target.computer.clone()), &target.key, ms)
                    .map_err(|error| error.to_string()),
                Some(Source::Replay(_)) => Ok(()),
                Some(Source::Service(service)) => {
                    service.processes_update(&target.computer, &target.key, Some(ms))
                }
                None => Err("not started".into()),
            }
        }

        pub fn refresh_processes(&self, target: &ProcessTarget) -> Result<(), String> {
            let _admission = self.admit()?;
            match self
                .source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                Some(Source::Fleet(fleet)) => fleet
                    .refresh_processes(&ComputerId(target.computer.clone()), &target.key)
                    .map_err(|error| error.to_string()),
                Some(Source::Replay(_)) => Ok(()),
                Some(Source::Service(service)) => {
                    service.processes_update(&target.computer, &target.key, None)
                }
                None => Err("not started".into()),
            }
        }

        /// Not followed (it ended meanwhile) is not an error.
        pub fn unfollow_processes(&self, target: &ProcessTarget) {
            let Ok(_admission) = self.admit() else { return };
            match self
                .source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                Some(Source::Fleet(fleet)) => {
                    fleet.unfollow_processes(&ComputerId(target.computer.clone()), &target.key);
                }
                Some(Source::Replay(replay)) => {
                    replay.unfollow_processes(&target.computer, &target.key);
                }
                Some(Source::Service(service)) => service.unfollow_detail(
                    &target.computer,
                    zerus_watch_protocol::topic::PROCESSES,
                    &target.key,
                ),
                None => {}
            }
        }

        pub fn unfollow(&self, computer: String, key: String) -> Result<(), String> {
            let _admission = self.admit()?;
            match self
                .source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                // Not followed (it ended meanwhile) is not an error.
                Some(Source::Fleet(fleet)) => {
                    fleet.unfollow(&ComputerId(computer), &key);
                }
                Some(Source::Replay(replay)) => replay.unfollow(&computer, &key),
                Some(Source::Service(service)) => {
                    service.unfollow_detail(&computer, zerus_watch_protocol::topic::ACTIVITY, &key)
                }
                None => return Err("not started".into()),
            }
            Ok(())
        }
    }
}

#[cfg(not(feature = "live"))]
const NO_LIVE: &str = "this build has no live data source (cargo feature `live` is off)";

/// Runs blocking work on the blocking pool, off the main thread.
#[cfg(feature = "live")]
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| error.to_string())?
}

/// Starts the data source; "live" (hgs watch) or "replay" (a fixture). A
/// reloaded webview gets the catalog, the composer's view and the draft keys
/// again.
#[cfg(feature = "live")]
#[tauri::command]
pub async fn start(
    state: State<'_, AppState>,
    composers: State<'_, crate::composer::Composers>,
    processes: State<'_, crate::processes::ProcessHub>,
) -> Result<String, String> {
    let live = Arc::clone(&state.live);
    composers.resend();
    processes.resend();
    // A reloaded webview shows no attachment yet.
    composers.cache().clear_pins();
    blocking(move || live.start()).await
}

#[cfg(not(feature = "live"))]
#[tauri::command]
pub async fn start(state: State<'_, AppState>) -> Result<String, String> {
    let _ = &state;
    Err(NO_LIVE.into())
}

/// Codex's question replies become `question_replies` on their events
/// (R7's `annotate_replies`), so the webview never parses them.
#[cfg(feature = "live")]
pub(crate) fn annotate(batch: &mut zerus_client_core::Batch) {
    for out in &mut batch.activity {
        zerus_client_core::questions::annotate_replies(&mut out.payload);
    }
}

/// What `next_batch` returns: the core's batch, plus the hubs' projected views
/// when they changed (latest value wins).
#[cfg(feature = "live")]
#[derive(serde::Serialize)]
struct Envelope<'a> {
    #[serde(flatten)]
    batch: &'a zerus_client_core::Batch,
    #[serde(skip_serializing_if = "Option::is_none")]
    composer: Option<crate::composer::ComposerView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    draft_keys: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    processes_view: Option<crate::processes::ProcessesView>,
    /// Notices and Terminal requests that arrived after their call (A2).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    composer_events: Vec<crate::composer::ComposerEvent>,
}

/// Everything pending since the last call; parks up to 250 ms while nothing is.
/// The frontend keeps one call in flight and applies one batch per frame.
/// The batch goes as raw JSON bytes: a first catalog batch is tens of KiB,
/// beyond what the evaluated-script path should carry.
#[cfg(feature = "live")]
#[tauri::command]
pub async fn next_batch(
    state: State<'_, AppState>,
    actions: State<'_, crate::actions::Actions>,
    composers: State<'_, crate::composer::Composers>,
    processes: State<'_, crate::processes::ProcessHub>,
) -> Result<tauri::ipc::Response, String> {
    let live = Arc::clone(&state.live);
    let actions = actions.inner().clone();
    let composers = composers.inner().clone();
    let processes = processes.inner().clone();
    let bytes = blocking(move || {
        let mut batch = live.pump.take(crate::pump::PARK);
        // Codex's question replies are parsed here, never in the webview.
        annotate(&mut batch);
        // The followed processes go to the Processes hub, not to the webview.
        processes.observe(&mut batch);
        // The host's rows settle pending read marks, whether they arrive as
        // single upserts or inside a computer's snapshot.
        actions.observe(&batch);
        if crate::desktop_service::mode() {
            actions.overlay_retained_reads(&mut batch);
        }
        // The followed session's Activity state and its row feed the
        // composer's block reason, before its view is taken.
        composers.observe(&batch);
        // Marks for computers the local listing no longer names can never be
        // written (Qt's `publishReadMarks`).
        actions.drop_orphans(|| live.peers());
        let envelope = Envelope {
            batch: &batch,
            composer: composers.take_view(),
            draft_keys: composers.take_draft_keys(),
            processes_view: processes.take_view(),
            composer_events: composers.take_events(),
        };
        serde_json::to_vec(&envelope).map_err(|error| error.to_string())
    })
    .await?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[cfg(not(feature = "live"))]
#[tauri::command]
pub async fn next_batch(state: State<'_, AppState>) -> Result<Value, String> {
    let _ = &state;
    Err(NO_LIVE.into())
}

/// Follows one session's Activity: the newest `tail` events, or the events after `after`.
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
pub async fn follow(
    state: State<'_, AppState>,
    computer: String,
    key: String,
    name: String,
    run_id: Option<String>,
    conversation_id: Option<String>,
    after: Option<i64>,
    tail: Option<u32>,
) -> Result<(), String> {
    #[cfg(feature = "live")]
    {
        let live = Arc::clone(&state.live);
        let session = zerus_watch_protocol::SessionRef {
            name,
            run_id,
            conversation_id,
        };
        let tail = tail.unwrap_or(zerus_watch_protocol::ACTIVITY_TAIL as u32);
        blocking(move || live.follow(computer, key, session, after, tail)).await
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = (
            &state,
            computer,
            key,
            name,
            run_id,
            conversation_id,
            after,
            tail,
        );
        Err(NO_LIVE.into())
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn unfollow(
    state: State<'_, AppState>,
    computer: String,
    key: String,
) -> Result<(), String> {
    #[cfg(feature = "live")]
    {
        let live = Arc::clone(&state.live);
        blocking(move || live.unfollow(computer, key)).await
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = (&state, computer, key);
        Err(NO_LIVE.into())
    }
}

/// Starts/stops the Tasks board's one project subscription; no list poller.
#[tauri::command]
pub async fn tasks_follow(
    state: State<'_, AppState>,
    project: Option<String>,
) -> Result<(), String> {
    #[cfg(feature = "live")]
    {
        let live = Arc::clone(&state.live);
        blocking(move || live.follow_tasks(project)).await
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = (&state, project);
        Err(NO_LIVE.into())
    }
}

/// The measurement run mode from the environment: `ZERUS_CLIENT_PERF` (`auto`
/// runs the Diagnostics scenarios, saves the report and exits),
/// `ZERUS_CLIENT_PERF_OUT` (where the report goes), `ZERUS_CLIENT_PERF_LABEL`
/// (the platform label), `ZERUS_CLIENT_PERF_REPRESENTATIVE=0` (Xvfb and other
/// non-representative runs) and `ZERUS_CLIENT_PERF_PREFERENCE` (`default`
/// leaves the macOS and Linux WebKit frame-rate preference unchanged). `replay`
/// tells the page that `ZERUS_CLIENT_REPLAY` supplies the data.
#[tauri::command]
pub fn perf_config() -> Value {
    let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    serde_json::json!({
        "mode": var("ZERUS_CLIENT_PERF"),
        "out": var("ZERUS_CLIENT_PERF_OUT"),
        "label": var("ZERUS_CLIENT_PERF_LABEL"),
        // `0` marks a run whose numbers do not represent the device (Xvfb, a remote session).
        "representative": var("ZERUS_CLIENT_PERF_REPRESENTATIVE").is_none_or(|value| value != "0"),
        "preference": if crate::keep_default_preference() { "default" } else { "display-rate" },
        // `:N` is an X server (Xvfb); `wayland-N` is a desktop session.
        "display": crate::display::display_name(),
        // The actions smoke runs only on a replay with an explicit fake hgs.
        "actions_smoke": actions_smoke_allowed(),
        "smoke_file": var("ZERUS_CLIENT_SMOKE_FILE"),
        // A replay run measures a synthetic Activity (the auto run tries every session for it).
        "replay": var("ZERUS_CLIENT_REPLAY").is_some(),
        // The Linux WebKit renderer decision (renderer, reason, nvidia, wayland, marker_state).
        "renderer": crate::renderer::diagnostics(),
    })
}

#[cfg(feature = "live")]
fn actions_smoke_allowed() -> bool {
    crate::actions::smoke_env_allowed()
}

#[cfg(not(feature = "live"))]
fn actions_smoke_allowed() -> bool {
    false
}

fn perf_mode() -> bool {
    std::env::var("ZERUS_CLIENT_PERF").is_ok_and(|mode| !mode.is_empty())
}

/// Writes a measurement report; only in a measurement run (`ZERUS_CLIENT_PERF`
/// set). An empty `path` means `ZERUS_CLIENT_PERF_OUT`. Returns the path written.
#[tauri::command(async)]
pub fn save_perf(path: String, json: String) -> Result<String, String> {
    if !perf_mode() {
        return Err("not a measurement run (ZERUS_CLIENT_PERF is not set)".into());
    }
    let path = match path.is_empty() {
        true => std::env::var("ZERUS_CLIENT_PERF_OUT")
            .map_err(|_| "ZERUS_CLIENT_PERF_OUT is not set".to_owned())?,
        false => path,
    };
    let path = std::path::PathBuf::from(path);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(&path, json).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(path.display().to_string())
}

/// The refresh rate of the main window's display in hertz, None when unknown
/// (`ZERUS_CLIENT_PERF_HZ` overrides it). Synchronous: it runs on the main
/// thread, where the platform display APIs must be called.
#[tauri::command]
pub fn display_hz(app: tauri::AppHandle) -> Option<f64> {
    crate::main_window(&app).and_then(|window| crate::display::display_hz(&window))
}

/// One progress line of a measurement run on stderr, so a stalled run shows
/// where it stopped. The frontend writes no session names into it.
#[tauri::command]
pub fn perf_log(line: String) {
    let line: String = line.chars().filter(|c| !c.is_control()).take(500).collect();
    eprintln!("zerus-client perf: {line}");
}

#[cfg(feature = "live")]
fn smoke_origin(label: &str) -> Result<(), String> {
    if label == crate::MAIN_WINDOW {
        Ok(())
    } else {
        Err("not the main webview".into())
    }
}

/// The fake `hgs` call log beside `ZERUS_CLIENT_HGS`, for the phase 4 smoke.
/// Refused outside a smoke run, so a real `hgs` folder is never read.
#[cfg(feature = "live")]
#[tauri::command]
pub async fn smoke_log(window: tauri::Webview) -> Result<Vec<serde_json::Value>, String> {
    smoke_origin(window.label())?;
    if !crate::actions::smoke_env_allowed() {
        return Err("not a smoke run".into());
    }
    let hgs =
        std::path::PathBuf::from(std::env::var("ZERUS_CLIENT_HGS").map_err(|e| e.to_string())?);
    tauri::async_runtime::spawn_blocking(move || {
        let log = hgs.with_file_name("log.ndjson");
        let text =
            std::fs::read_to_string(&log).map_err(|error| format!("{}: {error}", log.display()))?;
        Ok(text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Advances the replay after the fake's Resume, only in the phase 4 smoke.
#[cfg(feature = "live")]
#[tauri::command]
pub async fn smoke_resume(
    state: State<'_, AppState>,
    window: tauri::Webview,
    key: String,
) -> Result<(), String> {
    smoke_origin(window.label())?;
    if !crate::actions::smoke_env_allowed() {
        return Err("not a smoke run".into());
    }
    let hgs =
        std::path::PathBuf::from(std::env::var("ZERUS_CLIENT_HGS").map_err(|e| e.to_string())?);
    let ready = tauri::async_runtime::spawn_blocking(move || {
        let path = hgs.with_file_name("config.json");
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let config: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
        Ok::<Value, String>(config["resume_ready"].clone())
    })
    .await
    .map_err(|error| error.to_string())??;
    state.smoke_resume(&key, ready)
}

/// This build's identity, as `--build-info` prints it (Qt's
/// `UpdateController::buildInfo`): the About page shows it.
#[tauri::command]
pub fn build_info() -> Value {
    crate::cli::build_info(&crate::cli::BUILD)
}

/// Ends the app after a measurement run; refused outside one.
#[tauri::command]
pub fn exit_app(app: tauri::AppHandle, code: i32) -> Result<(), String> {
    if !perf_mode() {
        return Err("not a measurement run (ZERUS_CLIENT_PERF is not set)".into());
    }
    crate::exit_flush::quit_with_flush(&app, code);
    Ok(())
}

/// Takes the window away for a measurement: `how` is `hide` or `minimize`.
/// After a settling pause it samples the process tree over `window_ms`
/// (default 10 s), brings the window back and returns `{how, minimized,
/// sample}`; `minimized` says whether the window manager minimised it (a bare
/// X server has none). Refused outside a measurement run.
#[tauri::command(async, rename_all = "snake_case")]
pub fn perf_away(
    app: tauri::AppHandle,
    how: String,
    window_ms: Option<u64>,
) -> Result<Value, String> {
    if !perf_mode() {
        return Err("not a measurement run (ZERUS_CLIENT_PERF is not set)".into());
    }
    let window = crate::main_window(&app).ok_or("The main window is unavailable")?;
    match how.as_str() {
        "hide" => window.hide(),
        "close" => window.close(),
        "minimize" => window.minimize(),
        _ => return Err(format!("unknown way to take the window away: {how}")),
    }
    .map_err(|error| error.to_string())?;
    crate::window_visibility::sync_webview(&window);
    // The page sees the change and its last frame ends before the sample starts.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let minimized = window.is_minimized().unwrap_or(false);
    let ms = window_ms.unwrap_or(10_000).clamp(100, 60_000);
    let sample = crate::metrics::process_tree_metrics_over(
        std::process::id(),
        std::time::Duration::from_millis(ms),
    );
    let back = match how.as_str() {
        "hide" | "close" => window.show(),
        _ => window.unminimize(),
    };
    crate::window_visibility::sync_webview(&window);
    if let Err(error) = back.and_then(|()| window.set_focus()) {
        eprintln!("zerus-client perf: could not bring the window back: {error}");
    }
    Ok(serde_json::json!({ "how": how, "minimized": minimized, "sample": sample }))
}

#[tauri::command(async, rename_all = "snake_case")]
pub fn perf_resize(app: tauri::AppHandle, width: u32, height: u32) -> Result<(), String> {
    if !perf_mode() {
        return Err("not a measurement run (ZERUS_CLIENT_PERF is not set)".into());
    }
    let window = crate::main_window(&app).ok_or("The main window is unavailable")?;
    window
        .set_size(tauri::LogicalSize::new(
            width.clamp(640, 3840),
            height.clamp(480, 2160),
        ))
        .map_err(|error| error.to_string())?;
    crate::window_visibility::sync_webview(&window);
    Ok(())
}

/// RSS and CPU of the client's process tree (the app and its webview
/// processes). Sampling blocks for its 500 ms window, so it runs off the main thread.
/// `window_ms` sets the window between the two samples (default 500 ms; an idle
/// measurement uses 10 s, so CPU is the cpu_seconds delta over that wall time).
#[tauri::command(async, rename_all = "snake_case")]
pub fn metrics(window_ms: Option<u64>) -> Value {
    let pid = std::process::id();
    match window_ms {
        Some(ms) => crate::metrics::process_tree_metrics_over(
            pid,
            std::time::Duration::from_millis(ms.clamp(100, 60_000)),
        ),
        None => crate::metrics::process_tree_metrics(pid),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "live")]
    #[test]
    fn task_board_checks_the_project_and_needs_the_existing_live_source() {
        let live = super::live::Live::default();
        assert_eq!(
            live.follow_tasks(Some("bad\nproject".into())).unwrap_err(),
            "Invalid task project."
        );
        assert_eq!(
            live.follow_tasks(Some("project-1".into())).unwrap_err(),
            "Tasks need a live hgs event stream."
        );
    }
    #[test]
    fn build_info_answers_what_the_command_line_prints() {
        let info = super::build_info();
        assert_eq!(info, crate::cli::build_info(&crate::cli::BUILD));
        assert_eq!(info["version"], env!("ZERUS_VERSION"));
    }

    #[cfg(feature = "live")]
    #[test]
    fn smoke_commands_answer_only_the_main_webview() {
        assert_eq!(super::smoke_origin("main"), Ok(()));
        assert!(super::smoke_origin("native-ui").is_err());
    }

    #[cfg(feature = "live")]
    #[test]
    fn question_replies_are_annotated_before_the_webview() {
        use zerus_client_core::{ActivityOut, Batch, ComputerId};
        let detail = "<send_user_message_question_reply>{\"questionItemId\":\"q1\",\"question\":\"Which checks?\",\"answer\":\"Unit\"}</send_user_message_question_reply>";
        let mut batch = Batch {
            activity: vec![ActivityOut {
                computer: ComputerId::from("studio"),
                key: "run:run-1".into(),
                kind: "events".into(),
                payload: serde_json::json!({"events": [{"seq": 1, "type": "UserPromptSubmit", "detail": detail}], "cursor": 1}),
            }],
            ..Batch::default()
        };
        super::annotate(&mut batch);
        assert_eq!(
            batch.activity[0].payload["events"][0]["question_replies"],
            serde_json::json!([{"question": "Which checks?", "answer": "Unit"}])
        );
    }
}

/// Whether this build includes the native recorder; no device is opened.
#[tauri::command]
pub fn voice_support() -> Value {
    serde_json::json!({"available":cfg!(feature = "media"), "reason":"Voice recording is unavailable in this build."})
}
