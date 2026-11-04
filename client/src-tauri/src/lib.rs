//! Zerus Next: the Tauri shell around the SolidJS frontend.
//!
//! The window is created here rather than in `tauri.conf.json`, so platform
//! code can hand the webview its configuration before it exists (the macOS
//! frame-rate preference, plan Task 7).

#[cfg(feature = "live")]
mod action_delay;
#[cfg(feature = "live")]
mod actions;
#[cfg(feature = "live")]
mod analytics;
#[cfg(feature = "live")]
mod approvals;
#[path = "../../core/src/child_environment.rs"]
#[allow(dead_code)]
mod child_environment;
mod cli;
mod clipboard;
mod commands;
#[cfg(feature = "live")]
mod composer;
#[cfg(feature = "live")]
mod content_search;
#[cfg(feature = "live")]
mod creations;
#[cfg(feature = "live")]
mod desk;
#[cfg(all(
    feature = "live",
    any(target_os = "linux", target_os = "macos", windows)
))]
mod desktop_hello;
#[cfg(feature = "live")]
mod desktop_notification_controls;
#[cfg(feature = "live")]
mod desktop_ownership;
#[cfg(feature = "live")]
mod desktop_service;
mod display;
#[cfg(feature = "live")]
mod drops;
#[cfg(any(target_os = "macos", test))]
pub mod entry;
mod exit_flush;
#[cfg(feature = "live")]
mod external_open;
#[cfg(feature = "live")]
mod files;
mod identity;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod instance;
#[cfg(windows)]
#[path = "instance_windows.rs"]
mod instance;
mod lifecycle;
#[cfg(feature = "live")]
mod login_item;
#[cfg(target_os = "macos")]
mod macos_notification_diagnostics;
mod metrics;
#[cfg(feature = "live")]
mod mobile;
#[cfg(feature = "live")]
mod native_text;
pub mod native_ui;
#[cfg(feature = "live")]
mod newsession;
#[cfg(target_os = "macos")]
pub mod notification_host;
#[cfg(feature = "live")]
mod notifier;
#[cfg(feature = "live")]
mod organization;
mod platform;
#[cfg(feature = "live")]
mod processes;
#[cfg(feature = "live")]
mod pump;
#[cfg(feature = "live")]
mod readsync;
#[cfg(feature = "live")]
mod recovery;
pub mod renderer;
#[cfg(feature = "live")]
mod replay;
#[cfg(feature = "live")]
mod screens;
#[cfg(feature = "live")]
mod session_files;
#[cfg(feature = "live")]
mod session_folders;
#[cfg(feature = "live")]
pub mod settings;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod signals;
#[cfg(feature = "live")]
mod speech_settings;
#[cfg(feature = "live")]
mod startup_error;
#[cfg(feature = "live")]
mod task_details;
#[cfg(feature = "live")]
mod task_editor;
#[cfg(feature = "live")]
mod task_executor;
#[cfg(feature = "live")]
mod task_forms;
#[cfg(feature = "live")]
mod task_settings;
#[cfg(feature = "live")]
mod task_start;
#[cfg(feature = "live")]
mod terminal;
#[cfg(feature = "live")]
mod termination_cleanup;
#[cfg(feature = "live")]
mod updates;
#[cfg(feature = "media")]
mod voice;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod watchdog;
#[cfg(feature = "live")]
mod window_geometry;
#[cfg(feature = "live")]
mod window_layer;
mod window_visibility;
#[cfg(any(windows, test))]
mod windows_shell;
#[cfg(feature = "live")]
mod worktree_cleanup;
#[cfg(feature = "live")]
mod worktree_policy;

use tauri::{AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// The app's one draft store, shared by the composers (and voice), so every
/// draft key has one writer.
#[cfg(feature = "live")]
pub struct Drafts(pub std::sync::Arc<zerus_client_core::drafts::DraftStore>);

/// The label of the one window this phase opens.
pub const MAIN_WINDOW: &str = "main";

/// The root of the client's disposable files, the attachment cache: Tauri's
/// `app_cache_dir` formula with the running bundle's identifier, as
/// [`data_dir`]. Tauri's own names the compile-time identifier, which a
/// switch build promotes at signing, so the client never calls it.
#[cfg(feature = "live")]
pub(crate) fn cache_dir(app: &AppHandle) -> std::path::PathBuf {
    use tauri::Manager;
    app.state::<identity::Identity>().cache_dir_now()
}

/// The client's attachment cache under its cache folder: `attachments`, or
/// `attachments-replay` in a replay. Qt's cache (`AttachmentViewer.h:28`,
/// `hgdev/hgs-tray/attachments`) is another folder this client never touches.
#[cfg(feature = "live")]
fn attachments_root(cache_dir: &std::path::Path, replay: bool) -> std::path::PathBuf {
    cache_dir.join(if replay {
        "attachments-replay"
    } else {
        "attachments"
    })
}

/// The client's own draft store under its data directory: `drafts`, or
/// `drafts-replay` in a replay, so a replay never touches the real drafts.
/// Qt's store is only ever read (Saved drafts and its recovery).
#[cfg(feature = "live")]
fn drafts_root(data_dir: &std::path::Path, replay: bool) -> std::path::PathBuf {
    data_dir.join(zerus_client_core::drafts::dir(replay))
}

/// Tests that start child processes hold this shared; the tests that sample
/// this process's tree hold it alone, since a child that exits mid-sample
/// leaves the tree's memory total unknown.
#[cfg(all(test, unix))]
pub(crate) static PROCESS_TREE: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// What `run` settled before the app starts.
pub struct Startup {
    pub identity: identity::Identity,
    /// Started with `--sessions`, or a build that opens its window by default.
    pub opens_sessions: bool,
    #[cfg(all(feature = "live", target_os = "linux"))]
    navigation: Option<zerus_client_core::desktop::activation::Target>,
    #[cfg(all(feature = "live", target_os = "linux"))]
    activation_token: Option<String>,
    /// The claimed single-instance socket and the activations it receives.
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    pub instance: Option<instance::Claimed>,
    #[cfg(all(
        feature = "live",
        any(target_os = "linux", target_os = "macos", windows)
    ))]
    pub desktop_responder: Option<std::sync::Arc<zerus_client_core::desktop::Responder>>,
    #[cfg(all(feature = "live", any(target_os = "linux", target_os = "macos")))]
    pub desktop_bootstrap: Option<zerus_client_core::desktop::startup::Bootstrap>,
    #[cfg(all(
        feature = "live",
        any(target_os = "linux", target_os = "macos", windows)
    ))]
    native_ownership: Option<desktop_ownership::NativeReady>,
}

/// Parses Qt's command line and answers `--help`, `--version` and
/// `--build-info`. It then takes the single-instance socket, or hands the
/// activation to the running instance and returns, and runs the app.
pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let context: tauri::Context<tauri::Wry> = tauri::generate_context!();
    let options = match cli::parse(&args) {
        Ok(cli::Command::Run(options)) => options,
        Ok(cli::Command::Help) => {
            print!("{}", cli::usage());
            return;
        }
        Ok(cli::Command::Version) => {
            print!("{}", cli::version_line(&cli::BUILD));
            return;
        }
        Ok(cli::Command::BuildInfo) => {
            #[cfg(any(target_os = "linux", windows))]
            let info = cli::build_info_for(&cli::BUILD, &context.config().identifier);
            #[cfg(not(any(target_os = "linux", windows)))]
            let info = cli::build_info(&cli::BUILD);
            println!("{info}");
            return;
        }
        Ok(cli::Command::LoginItem { enabled }) => {
            #[cfg(all(windows, feature = "live"))]
            {
                let identity = identity::current(&context.config().identifier);
                if !identity.is_switch() {
                    eprintln!("Login registration requires the installed Zerus identity");
                    std::process::exit(2);
                }
                let result = std::env::current_exe()
                    .map_err(|e| e.to_string())
                    .and_then(|exe| platform::windows::startup::login_item(enabled, &exe, &args));
                match result {
                    Ok(()) => println!(
                        "Zerus login item {}",
                        if enabled { "installed" } else { "removed" }
                    ),
                    Err(error) => {
                        eprintln!("zerus-client: {error}");
                        std::process::exit(3);
                    }
                }
                return;
            }
            #[cfg(not(all(windows, feature = "live")))]
            {
                let _ = enabled;
                eprintln!("Login registration is available in the Windows desktop build only");
                std::process::exit(2);
            }
        }
        Err(error) => {
            eprint!("zerus-client: {error}\n\n{}", cli::usage());
            std::process::exit(2);
        }
    };
    #[cfg(all(feature = "live", target_os = "linux"))]
    let options = {
        let mut options = options;
        if let Err(error) = cli::take_navigation_environment(&mut options) {
            eprint!("zerus-client: {error}\n\n{}", cli::usage());
            std::process::exit(2);
        }
        options
    };
    for option in &options.ignored {
        eprintln!("zerus-client: {option} is accepted for the Qt GUI's command line and not used");
    }
    // Before any thread starts: the core's `default_hgs` reads it.
    if let Some(hgs) = &options.hgs {
        std::env::set_var("ZERUS_CLIENT_HGS", hgs);
    }
    // Consume the new process's startup token before any worker can inherit it.
    // Instance forwarding carries navigation only, never a notification token.
    #[cfg(all(feature = "live", target_os = "linux"))]
    let activation_token = cli::take_activation_token(options.target.is_some());
    let identity = identity::current(&context.config().identifier);
    let opens_sessions = options.opens_sessions(identity.is_switch());
    #[cfg(all(
        feature = "live",
        any(target_os = "linux", target_os = "macos", windows)
    ))]
    let mut desktop_responder = None;
    #[cfg(all(feature = "live", any(target_os = "linux", target_os = "macos")))]
    let mut desktop_bootstrap = None;
    #[cfg(all(
        feature = "live",
        any(target_os = "linux", target_os = "macos", windows)
    ))]
    let mut native_ownership = None;
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    let instance = match identity.instance_name(identity::flag("ZERUS_CLIENT_SINGLE_INSTANCE")) {
        None => {
            renderer::choose();
            None
        }
        Some(name) => {
            let activation = if opens_sessions {
                instance::Activation::Sessions
            } else {
                instance::Activation::Show
            };
            #[cfg(all(feature = "live", target_os = "linux"))]
            let activation = options
                .target
                .clone()
                .map(instance::Activation::Navigation)
                .unwrap_or(activation);
            #[cfg(all(
                feature = "live",
                any(target_os = "linux", target_os = "macos", windows)
            ))]
            let (reply, responder) = desktop_hello::prepare(&identity);
            #[cfg(all(
                feature = "live",
                any(target_os = "linux", target_os = "macos", windows)
            ))]
            let reply = Some(reply);
            #[cfg(not(all(
                feature = "live",
                any(target_os = "linux", target_os = "macos", windows)
            )))]
            let reply = None;
            match instance::claim(
                &name,
                identity.launchd_label(),
                activation,
                reply,
                renderer::choose,
            ) {
                instance::Claim::Claimed(claimed) => {
                    #[cfg(all(feature = "live", any(target_os = "linux", target_os = "macos")))]
                    {
                        let settled = match desktop_hello::hold(identity.is_switch(), &responder) {
                            Ok(held) => held,
                            Err(_) => {
                                eprintln!("zerus-client: desktop startup is unconfirmed; writers remain stopped");
                                startup_error::show(context);
                                return;
                            }
                        };
                        desktop_bootstrap = settled.0;
                        native_ownership = settled.1;
                        desktop_responder = Some(responder);
                    }
                    #[cfg(all(windows, feature = "live"))]
                    {
                        desktop_responder = Some(responder);
                    }
                    Some(claimed)
                }
                instance::Claim::Forwarded => {
                    eprintln!("zerus-client: already running");
                    return;
                }
                instance::Claim::Failed(error) => {
                    eprintln!("zerus-client: cannot claim the socket: {error}");
                    std::process::exit(3);
                }
            }
        }
    };
    #[cfg(all(windows, feature = "live"))]
    {
        let replay = std::env::var_os("ZERUS_CLIENT_REPLAY").is_some_and(|list| !list.is_empty());
        let hgs = platform::windows::startup::select_hgs(
            options.hgs.as_deref(),
            identity.is_switch() && !replay,
        );
        std::env::set_var("ZERUS_CLIENT_HGS", hgs);
        platform::windows::startup::prepare(&args, &identity.bundle_id);
        if identity.is_switch() && !replay && !desktop_service::mode() {
            if let Some(responder) = &desktop_responder {
                native_ownership = match desktop_ownership::native_takeover(
                    zerus_client_core::default_hgs(),
                    responder.clone(),
                ) {
                    Ok(proof) => Some(proof),
                    Err(code) => {
                        eprintln!("zerus-client: native ownership takeover is unconfirmed: {code}");
                        startup_error::show(context);
                        return;
                    }
                };
            }
        }
    }
    #[cfg(all(
        feature = "live",
        any(target_os = "linux", target_os = "macos", windows)
    ))]
    if native_ownership
        .as_ref()
        .is_some_and(|proof| proof.helper().is_err())
    {
        eprintln!("zerus-client: native ownership helper changed before GUI startup");
        startup_error::show(context);
        return;
    }
    #[cfg(target_os = "macos")]
    platform::macos::raise_main_thread_qos();
    launch(
        context,
        Startup {
            identity,
            opens_sessions,
            #[cfg(all(feature = "live", target_os = "linux"))]
            navigation: options.target,
            #[cfg(all(feature = "live", target_os = "linux"))]
            activation_token,
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            instance,
            #[cfg(all(
                feature = "live",
                any(target_os = "linux", target_os = "macos", windows)
            ))]
            desktop_responder,
            #[cfg(all(feature = "live", any(target_os = "linux", target_os = "macos")))]
            desktop_bootstrap,
            #[cfg(all(
                feature = "live",
                any(target_os = "linux", target_os = "macos", windows)
            ))]
            native_ownership,
        },
    );
}

/// The client's private data directory: the running bundle's identifier
/// (Decision 5). Tauri's `app_data_dir` names the compile-time identifier,
/// which a switch build promotes at signing, so the client never calls it.
#[cfg_attr(not(feature = "live"), allow(dead_code))]
pub(crate) fn data_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    use tauri::Manager;
    app.state::<identity::Identity>().data_dir_now()
}

/// Builds the app and runs its event loop. A build with a menu-bar item
/// starts hidden unless asked for the window, and runs until it quits.
fn launch(context: tauri::Context<tauri::Wry>, startup: Startup) {
    let identity = startup.identity;
    let opens_sessions = startup.opens_sessions;
    #[cfg(all(
        feature = "live",
        any(target_os = "linux", target_os = "macos", windows)
    ))]
    let native_ownership = startup.native_ownership;
    #[cfg(all(feature = "live", target_os = "linux"))]
    let navigation = startup.navigation;
    #[cfg(all(feature = "live", target_os = "linux"))]
    let activation_token = startup.activation_token;
    #[cfg(all(feature = "live", any(target_os = "linux", target_os = "macos")))]
    let desktop_bootstrap = startup.desktop_bootstrap;
    #[cfg(all(feature = "live", any(target_os = "linux", target_os = "macos")))]
    let startup_responder = startup.desktop_responder.clone();
    // Linux and macOS keep the live fleet in the tray/menu bar. A build without the data source has
    // none and keeps its window as the way back.
    let menu_bar = cfg!(all(
        any(target_os = "macos", target_os = "linux", windows),
        feature = "live"
    ));
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    let (listening, activations) = match startup.instance {
        Some(claimed) => (Some(claimed.listening), Some(claimed.activations)),
        None => (None, None),
    };
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        // Opened from Rust only (the composer's `composer_pick` and New
        // session's `pick_local_folder`): the capabilities grant the webview
        // none of its permissions.
        .plugin(tauri_plugin_dialog::init())
        // Rust opens attachments; the webview's links stay its own (no injected click handler).
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .manage(commands::AppState::default())
        .manage(window_visibility::Visibility::default())
        .manage(lifecycle::Lifecycle { menu_bar })
        .manage(exit_flush::ExitFlush::default());
    #[cfg(target_os = "macos")]
    let builder = builder
        .menu(exit_flush::app_menu)
        .on_menu_event(|app, event| {
            if event.id().as_ref() == exit_flush::QUIT_ID {
                exit_flush::quit_with_flush(app, 0);
            }
        });
    // tao activates the app when it finishes launching, taking the focus from
    // the app in front; a menu-bar build activates only to show its window.
    #[cfg(target_os = "macos")]
    let builder = builder.activate_ignoring_other_apps(!menu_bar);
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    let builder = builder.manage(instance::InstanceState(std::sync::Mutex::new(listening)));
    // A page that loads (again) takes what the menu bar sent once it asks,
    // with the number it learns when it has loaded.
    #[cfg(feature = "live")]
    let builder = builder.on_page_load(|webview, payload| {
        use tauri::Manager;
        if webview.label() != MAIN_WINDOW {
            return;
        }
        match payload.event() {
            tauri::webview::PageLoadEvent::Started => desk::page_loading(webview.app_handle()),
            tauri::webview::PageLoadEvent::Finished => desk::page_loaded(webview),
        }
    });
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut app = builder
        .invoke_handler(main_commands(handler()))
        // Draft attachments and cached files as raw bytes (`composer/files.rs`),
        // read off the main thread.
        .register_asynchronous_uri_scheme_protocol("zerus-file", |ctx, request, responder| {
            #[cfg(feature = "live")]
            {
                use tauri::Manager;
                let webview = ctx.webview_label().to_owned();
                let app = ctx.app_handle().clone();
                tauri::async_runtime::spawn_blocking(move || {
                    let response = match app.try_state::<composer::Composers>() {
                        Some(composers) => composer::files::respond(&composers, &webview, &request, |computer, key, message| {
                            app.try_state::<commands::AppState>()?.pump().poster(computer, key, message)
                        }),
                        None => tauri::http::Response::builder().status(404).body(Vec::new()).expect("a bare 404"),
                    };
                    responder.respond(response);
                });
            }
            #[cfg(not(feature = "live"))]
            {
                let _ = (ctx, request);
                responder.respond(tauri::http::Response::builder().status(404).body(Vec::new()).expect("a bare 404"));
            }
        })
        .on_window_event(|window, event| {
            #[cfg(feature = "live")]
            if matches!(event,tauri::WindowEvent::Focused(_) | tauri::WindowEvent::Destroyed) {
                use tauri::Manager;
                if let Some(source)=window.try_state::<desktop_notification_controls::Source>() {
                    desktop_notification_controls::observe_windows(window.app_handle(),&source);
                }
            }
            #[cfg(feature = "live")]
            if window.label() == MAIN_WINDOW && matches!(event, tauri::WindowEvent::Destroyed) {
                use tauri::Manager;
                if let Some(delays) = window.try_state::<action_delay::Delays>() {
                    delays.cancel_all(false);
                }
            }
            #[cfg(all(target_os = "linux", feature = "live"))]
            if window.label() == MAIN_WINDOW {
                use tauri::Manager;
                if let tauri::WindowEvent::Focused(focused) = event { desk::set_focused(window.app_handle(), *focused); }
            }
            if window.label() == MAIN_WINDOW
                && matches!(event, tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Focused(_) | tauri::WindowEvent::Moved(_))
            {
                window_visibility::sync(window);
            }
            #[cfg(feature = "live")]
            if window.label() == MAIN_WINDOW && matches!(event, tauri::WindowEvent::Destroyed) {
                use tauri::Manager;
                window.state::<terminal::Terminals>().stop_all();
            }
            #[cfg(not(feature = "live"))]
            let _ = (window, event);
        })
        .setup(move |app| {
            use tauri::Manager;
            app.manage(MainWindow::default());
            // The one source of the client's data directory (Decision 5).
            app.manage(identity);
            #[cfg(all(feature = "live", any(target_os = "linux", target_os = "macos", windows)))]
            if let Some(proof) = native_ownership { app.manage(proof); }
            #[cfg(feature = "live")]
            {
                let dir = data_dir(app.handle());
                #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                if let Some(responder) = &startup.desktop_responder {
                    app.state::<commands::AppState>().set_desktop_responder(responder.clone());
                }
                std::fs::create_dir_all(&dir)?;
                let replay = std::env::var_os("ZERUS_CLIENT_REPLAY").is_some_and(|list| !list.is_empty());
                app.manage(settings::open(app.handle(), &dir, replay));
                if desktop_service::mode() {
                    let source=desktop_notification_controls::Source::default();
                    let handle=app.handle().clone();
                    source.set_refresh(std::sync::Arc::new(move|source,complete| {
                        let app=handle.clone();
                        let _=handle.run_on_main_thread(move|| {
                            desktop_notification_controls::observe_windows(&app,&source);
                            let _=complete.send(true);
                        });
                    }));
                    app.state::<commands::AppState>().register_notification_controls(desktop_notification_controls::Setup {
                        source:source.clone(),settings:app.state::<settings::Store>().0.clone()
                    });
                    app.manage(source);
                    if let Some(inputs) = desktop_service::retained_import::Inputs::native(
                        &app.state::<identity::Identity>().bundle_id, replay, dir.clone(),
                        app.state::<settings::Store>().0.clone(),
                    ) {
                        app.state::<commands::AppState>().set_retained_import_source(inputs);
                    }
                }
                app.manage(speech_settings::SpeechPage::default());
                app.manage(termination_cleanup::TerminationCleanup::default());
                app.manage(worktree_cleanup::WorktreeCleanup::default());
                app.manage(recovery::start(app.handle()));
                app.manage(files::start());
                app.manage(analytics::start(app.handle()));
                app.manage(worktree_policy::start(app.handle()));
                app.manage(mobile::start(app.handle()));
                // A replay never touches the real read or attention marks.
                let actions = actions::Actions::new(dir.join(actions::marks_file(replay)));
                if desktop_service::mode() {
                    actions.disarm_read_delivery()?;
                    if !replay {
                        app.state::<commands::AppState>().set_pending_read_source(actions.clone());
                    }
                }
                let notice_app = app.handle().clone();
                if desktop_service::mode() {
                    app.manage(actions.read_delivery.clone());
                } else {
                app.manage(readsync::start(actions.clone(), std::sync::Arc::new(move |health, saturated| {
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    if let Some(responder) = &startup.desktop_responder {
                        responder.read_delivery(health);
                    }
                    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                    let _ = health;
                    if saturated {
                        desk::window_notice(&notice_app, "Read marks paused",
                            "All four read-delivery slots are waiting for child processes or pipes to close. See hgs desktop status for details.");
                    }
                })));
                }
                app.manage(actions);
                app.manage(action_delay::Delays::default());
                app.manage(actions::ArchiveReads::default());
                app.manage(content_search::Searches::default());
                app.manage(native_ui::runtime::NativeUi::default());
                app.manage(task_details::TaskReads::default());
                app.manage(task_details::TaskWrites::default());
                app.manage(task_editor::TaskEditor::default());
                app.manage(task_forms::TaskForms::default());
                app.manage(task_start::TaskStarts::default());
                app.manage(actions::ActivityReads::default());
                app.manage(actions::Attention::new(dir.join(actions::attention_file(replay))));
                let pump = app.state::<commands::AppState>().pump();
                app.manage(approvals::start(
                    pump.clone(),
                    app.state::<actions::Actions>().inner().clone(),
                    app.state::<settings::Store>().0.clone(),
                ));
                app.state::<actions::Actions>().set_termination_pump(pump.clone());
                let allowed = actions::actions_allowed(
                    std::env::var("ZERUS_CLIENT_REPLAY").ok().as_deref(),
                    std::env::var("ZERUS_CLIENT_HGS").ok().as_deref(),
                );
                let draft_path = dir.join(organization::draft_file(replay));
                if replay {
                    // A replay starts from the fixture's catalog: no draft of an earlier replay.
                    let _ = zerus_client_core::swarm::draft::store(
                        &draft_path,
                        &zerus_client_core::swarm::draft::DraftWrite::Remove,
                    );
                }
                let organization = organization::Organization::new(
                    pump.clone(),
                    zerus_client_core::default_hgs(),
                    allowed,
                    dir.join(organization::layout_file(replay)),
                    draft_path,
                    replay,
                );
                let qt_source = organization::run_seed_source(
                    &app.state::<identity::Identity>().bundle_id, replay,
                );
                organization.seed_runs(qt_source.clone());
                organization.recover_qt(qt_source,app.state::<settings::Store>().0.clone());
                // Before any start of the source (the menu bar's below, or the webview's).
                app.state::<commands::AppState>().set_organization(organization.clone());
                let screens = screens::Screens::tauri(app.handle().clone(), pump.clone());
                // Projects shows the organization the Sessions list groups by. The
                // call only takes the screens hub's lock and emits, so it never
                // waits for the main thread (organization::Presented).
                let projects = screens.clone();
                let swarm_app = app.handle().clone();
                organization.set_presented(std::sync::Arc::new(move |value, default, status| {
                    projects.organization(value, default, status);
                    let _ = tauri::Emitter::emit(&swarm_app, "project-swarm-changed", ());
                }));
                app.manage(organization);
                app.manage(organization::clones::Clones::default());
                app.manage(organization::overview::Overview::default());
                app.manage(screens);
                app.manage(newsession::NewSession::tauri(Some(dir.join(if replay {
                    "worktree-snapshots-replay.json"
                } else { "worktree-snapshots.json" }))));
                let creations = creations::start(app.handle(), &dir, replay, allowed);
                app.state::<commands::AppState>().set_creations(creations.clone());
                app.manage(creations);
                app.manage(terminal::Terminals::new(zerus_client_core::default_hgs(), allowed));
                // A replay never touches the real drafts either.
                let drafts = std::sync::Arc::new(zerus_client_core::drafts::DraftStore::open(
                    drafts_root(&dir, replay),
                ));
                // The switch build takes Qt's and the dev app's drafts once,
                // as the store's first job (zerus-5wmh.13.1); the dev identity
                // and a replay never import.
                if app.state::<identity::Identity>().seeds_qt(replay) {
                    zerus_client_core::drafts::import_at_start(&drafts);
                }
                #[cfg(feature = "media")]
                app.manage(voice::Voices::new(std::sync::Arc::clone(&drafts)));
                app.manage(Drafts(drafts));
                let drafts = std::sync::Arc::clone(&app.state::<Drafts>().0);
                let notify_pump = app.state::<commands::AppState>().pump();
                // A replay never touches the real attachment cache either.
                let cache = attachments_root(&cache_dir(app.handle()), replay);
                let smoke_clock = crate::actions::smoke_env_allowed()
                    .then(|| {
                        std::env::var("ZERUS_CLIENT_SMOKE_NOW")
                            .ok()
                            .and_then(|value| value.parse::<f64>().ok())
                    })
                    .flatten()
                    .map(|now| (now, std::time::Instant::now()));
                // Saved drafts reads Qt's store and the dev app's, never
                // writing to them; a replay lists neither.
                let qt_drafts = composer::qt_store(
                    drafts.root(),
                    (!replay).then(zerus_client_core::drafts::qt_root).flatten(),
                );
                let dev_drafts = if replay {
                    Vec::new()
                } else {
                    composer::dev_stores(drafts.root(), zerus_client_core::drafts::dev_roots())
                };
                app.manage(composer::Composers::new(
                    drafts,
                    composer::Config {
                        cache,
                        qt_drafts,
                        dev_drafts,
                    },
                    composer::Deps {
                        spawn: std::sync::Arc::new(|job: composer::Job| {
                            tauri::async_runtime::spawn_blocking(job);
                        }),
                        notify: std::sync::Arc::new(move || notify_pump.poke()),
                        clock: std::sync::Arc::new(move || {
                            if let Some((base, started)) = smoke_clock {
                                return base + started.elapsed().as_secs_f64();
                            }
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0.0, |d| d.as_secs_f64())
                        }),
                        rows: {
                            let pump = app.state::<commands::AppState>().pump();
                            std::sync::Arc::new(move |computer: &str, key: &str, name: &str| {
                                composer::archived_row(key, name)
                                    .or_else(|| pump.row(computer, key))
                                    .or_else(|| pump.row_named(computer, name))
                                    .map(|row| (row, pump.online(computer)))
                            })
                        },
                        facts: {
                            let actions = app.state::<actions::Actions>().inner().clone();
                            let pump = app.state::<commands::AppState>().pump();
                            std::sync::Arc::new(move |computer: &str, name: &str| {
                                actions.session_write(computer, name, || {
                                    (pump.row_named(computer, name), pump.online(computer))
                                })
                            })
                        },
                        reconnect: {
                            let app = app.handle().clone();
                            std::sync::Arc::new(move |computer| {
                                app.state::<commands::AppState>().reconnect(computer);
                            })
                        },
                        inspect: {
                            // A replay without an explicit ZERUS_CLIENT_HGS never reaches the real CLI.
                            let allowed = actions::actions_allowed(
                                std::env::var("ZERUS_CLIENT_REPLAY").ok().as_deref(),
                                std::env::var("ZERUS_CLIENT_HGS").ok().as_deref(),
                            );
                            let hgs = zerus_client_core::default_hgs();
                            std::sync::Arc::new(
                                move |target: &zerus_client_core::Target, archive: Option<&str>, agent: Option<&str>| {
                                    if !allowed {
                                        return Err(actions::REPLAY_NEEDS_HGS.to_owned());
                                    }
                                    let inv = match agent {
                                        Some(id) => zerus_client_core::details::subagent_identity_invocation(target, archive, id)
                                            .ok_or_else(|| "Invalid subagent identity".to_owned())?,
                                        None => zerus_client_core::details::details_invocation(target, archive),
                                    };
                                    composer::send::inspection(
                                        &hgs,
                                        zerus_client_core::run_limited(&hgs, &inv, &[], 8 << 20),
                                    )
                                },
                            )
                        },
                        // The guarded send: it runs on the hub's blocking
                        // threads, never on an async worker.
                        dispatch: {
                            let actions = app.state::<actions::Actions>().inner().clone();
                            std::sync::Arc::new(
                                move |target: zerus_client_core::Target,
                                      action: zerus_client_core::Action,
                                      id: String| {
                                    tauri::async_runtime::block_on(actions::dispatch_as(
                                        &actions, target, action, id,
                                    ))
                                },
                            )
                        },
                    },
                ));
                // A write mark shows in the composer at once.
                app.manage(task_executor::TaskExecutors::start(app.handle()));
                let composers = app.state::<composer::Composers>();
                app.state::<actions::Actions>().on_writes(composers.changed_hook());
                // The composer's session part once a second (the cache chip)
                // and the inspect fallback; it sleeps while no composer is open.
                composers.start_ticker();
                app.manage(processes_hub(app.handle(), allowed));
                // Activity attachments share the composers' cache: one root,
                // one set of pins and one prune lane, which keeps Retry's files.
                let retry = std::sync::Arc::downgrade(&composers.0);
                composers.cache().set_extra(std::sync::Arc::new(move || {
                    retry
                        .upgrade()
                        .map(|shared| composer::Composers(shared).retry_files())
                        .unwrap_or_default()
                }));
                app.manage(composer::fetch::Fetcher::new(
                    composers.cache(),
                    {
                        // A replay without an explicit ZERUS_CLIENT_HGS never reaches the real CLI.
                        let allowed = actions::actions_allowed(
                            std::env::var("ZERUS_CLIENT_REPLAY").ok().as_deref(),
                            std::env::var("ZERUS_CLIENT_HGS").ok().as_deref(),
                        );
                        let hgs = zerus_client_core::default_hgs();
                        std::sync::Arc::new(move |computer: &zerus_client_core::ComputerId, file: &zerus_client_core::reads::AttachmentRef, root: &std::path::Path| {
                            if !allowed {
                                return Err(actions::REPLAY_NEEDS_HGS.to_owned());
                            }
                            zerus_client_core::reads::fetch_attachment(&hgs, computer, file, root)
                        })
                    },
                    std::sync::Arc::new(|job: composer::Job| {
                        tauri::async_runtime::spawn_blocking(job);
                    }),
                ));
                let opener = {
                    use tauri_plugin_opener::OpenerExt;
                    let (open, reveal) = (app.handle().clone(), app.handle().clone());
                    composer::fetch::Opener {
                        open: std::sync::Arc::new(move |path: &std::path::Path| {
                            crate::external_open::path(&open, path).is_ok()
                        }),
                        reveal: std::sync::Arc::new(move |path: &std::path::Path| {
                            reveal.opener().reveal_item_in_dir(path).is_ok()
                        }),
                    }
                };
                app.manage(session_files::SessionFiles::new(
                    app.handle().clone(),
                    dir.clone(),
                    zerus_client_core::default_hgs(),
                    opener.clone(),
                    std::sync::Arc::new(|job: composer::Job| {
                        tauri::async_runtime::spawn_blocking(job);
                    }),
                ));
                app.manage(session_folders::SessionFolders::new(dir.clone(), opener.clone()));
                app.manage(opener);
                // The menu-bar item, before the fleet starts so its first catalog reaches it.
                #[cfg(target_os = "macos")]
                {
                    let conf = desk::tray_conf();
                    let configured = conf
                        .as_deref()
                        .and_then(|text| zerus_client_core::menubar::tray_conf_value(text, "icon-color"))
                        .filter(|value| !value.is_empty())
                        .and_then(|value| {
                            let color = zerus_client_core::trayicon::parse_color(&value);
                            if color.is_none() {
                                eprintln!("zerus-client: icon-color {value:?} in tray.conf is not #rgb or #rrggbb; using the menu bar's colour");
                            }
                            color
                        });
                    platform::macos_menubar::install(app.handle(), configured)?;
                    // Attention notifications under the running identity: the
                    // switch bundle, or another build on request; never a replay.
                    let identity = app.state::<identity::Identity>().inner().clone();
                    let posts = !desktop_service::mode() && (!identity.is_switch() || app.try_state::<desktop_ownership::NativeReady>().is_some()) && identity.notifications(identity::flag("ZERUS_CLIENT_NOTIFICATIONS"), replay);
                    let bundled = platform::macos::bundle_identifier().is_some();
                    if posts && !bundled {
                        eprintln!("zerus-client: notifications need an app bundle; this process has none, so none post");
                    }
                    let attention = identity::notifier_runs(posts, bundled)
                        .then(|| {
                            let path = dir.join(desk::ATTENTION_FILE);
                            // Qt's edges seed the switch bundle's first start
                            // (Decision 11): its `attention/observed`, which the
                            // defaults domain keeps as `attention.observed`.
                            let switch = identity.is_switch();
                            Box::new(move || {
                                desk::Attention::load(path, || {
                                    switch.then(|| platform::macos::qt_default_data("attention.observed")).flatten()
                                })
                            }) as desk::LoadAttention
                        });
                    let notifies = attention.is_some();
                    desk::start(app.handle(), &pump, dir.clone(), desk::load_mode(&dir, conf.as_deref()), attention);
                    if desktop_service::mode() {
                        if let Some(sender) = desk::sender(app.handle()) {
                            app.state::<commands::AppState>().register_service_activation(std::sync::Arc::new(move |delivery| {
                                let _ = sender.send(desk::DeskInput::ServiceClicked(Box::new(delivery)));
                            })).map_err(std::io::Error::other)?;
                        }
                    }
                    if notifies {
                        if let Some(sender) = desk::sender(app.handle()) {
                            platform::macos_notify::install(app.handle(), sender);
                        }
                    }
                }
                #[cfg(target_os = "linux")]
                {
                    let conf = desk::tray_conf();
                    platform::linux_tray::install(app.handle(), conf.as_deref());
                    let identity = app.state::<identity::Identity>().inner().clone();
                    let posts = !desktop_service::mode() && (!identity.is_switch() || app.try_state::<desktop_ownership::NativeReady>().is_some()) && identity.notifications(identity::flag("ZERUS_CLIENT_NOTIFICATIONS"), replay);
                    let switch = identity.is_switch();
                    let attention = posts.then(|| {
                        let path = dir.join(desk::ATTENTION_FILE);
                        Box::new(move || desk::Attention::load(path, || {
                            switch.then(|| platform::linux_notify::qt_values().ok().flatten()
                                .and_then(|values| values.get("attention/observed").map(|v| v.to_text().into_bytes()))).flatten()
                        })) as desk::LoadAttention
                    });
                    desk::start(app.handle(), &pump, dir.clone(), desk::load_linux_mode(&dir, conf.as_deref()), attention);
                    if let Some(target) = navigation {
                        if let Some(sender) = desk::sender(app.handle()) {
                            let _ = sender.send(desk::DeskInput::Navigate { target, activation: activation_token });
                        }
                    }
                    if desktop_service::mode() {
                        if let Some(sender) = desk::sender(app.handle()) {
                            app.state::<commands::AppState>().register_service_activation(std::sync::Arc::new(move |delivery| {
                                let _ = sender.send(desk::DeskInput::ServiceClicked(Box::new(delivery)));
                            })).map_err(std::io::Error::other)?;
                        }
                    }
                    if posts {
                        if let Some(sender) = desk::sender(app.handle()) { platform::linux_notify::install(app.handle(), dir.clone(), switch, sender); }
                    }
                }
                #[cfg(windows)]
                {
                    let conf = desk::tray_conf();
                    platform::windows::tray::install(app.handle(), conf.as_deref())?;
                    let identity = app.state::<identity::Identity>().inner().clone();
                    let posts = !desktop_service::mode() && (!identity.is_switch() || app.try_state::<desktop_ownership::NativeReady>().is_some()) && identity.notifications(identity::flag("ZERUS_CLIENT_NOTIFICATIONS"), replay);
                    let switch = identity.is_switch();
                    let attention = posts.then(|| {
                        let path = dir.join(desk::ATTENTION_FILE);
                        Box::new(move || desk::Attention::load(path, || {
                            if !switch { return None; }
                            zerus_client_core::settings::QtStore::here().and_then(|store| store.snapshot().ok().flatten())
                                .and_then(|snapshot| snapshot.values.get("attention/observed").map(|v| v.to_text().into_bytes()))
                        })) as desk::LoadAttention
                    });
                    desk::start(app.handle(), &pump, dir.clone(), desk::load_windows_mode(&dir, conf.as_deref()), attention);
                    if switch && !replay && !desktop_service::mode() {
                        match platform::windows::worker::Worker::start(zerus_client_core::default_hgs()) {
                            Ok(worker) => { app.manage(worker); }
                            Err(error) => eprintln!("zerus-client: cannot supervise swarm worker: {error}"),
                        }
                    }
                }
                app.manage(updates::start(app.handle()));
                if menu_bar {
                    lifecycle::start_fleet(|| app.state::<commands::AppState>().start_source());
                }
            }
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            {
                if let Some(activations) = activations {
                    forward_activations(app.handle().clone(), activations);
                }
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                exit_on_signals(app.handle().clone());
            }
            let display = display::display_name();
            if !display::headless_allowed(std::env::var("ZERUS_CLIENT_HEADLESS").is_ok_and(|v| v == "1"), display.as_deref()) {
                eprintln!("zerus-client: ZERUS_CLIENT_HEADLESS=1 but the display is {display:?}, not an X server; no window opens (set GDK_BACKEND=x11)");
                std::process::exit(3);
            }
            // Keep above is decided once from the display; the stored value
            // applies when the main window is created (`lifecycle::show_main`).
            #[cfg(feature = "live")]
            window_layer::init(app.handle(), display.as_deref());
            if lifecycle::window_at_start(opens_sessions, menu_bar) {
                let window = lifecycle::show_main(app.handle())?;
                // An unattended measurement needs frames: keep its window in front
                // and focused for the run, which ends by exiting the app.
                if std::env::var("ZERUS_CLIENT_PERF").is_ok_and(|mode| mode == "auto") {
                    if let Err(error) = window.set_always_on_top(true).and_then(|()| window.set_focus()) {
                        eprintln!("zerus-client perf: could not bring the window to the front: {error}");
                    }
                }
            }
            lifecycle::sync_policy(app.handle());
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if app.state::<identity::Identity>().watchdog(identity::flag("ZERUS_CLIENT_WATCHDOG")) {
                app.manage(start_watchdog()?);
            }
            #[cfg(all(feature = "live", any(target_os = "linux", target_os = "macos")))]
            if let (Some(held), Some(responder)) = (desktop_bootstrap, startup_responder) {
                desktop_hello::finish(held, responder);
            }
            Ok(())
        })
        .build(context)
        .expect("Zerus Next failed to start");
    #[cfg(target_os = "macos")]
    lifecycle::prepare(&mut app, menu_bar, opens_sessions);
    app.run(|app, event| {
        lifecycle::on_event(app, &event);
    });
}

/// The Processes tab's hub (`processes.rs`): the topic through the data
/// source, output reads through the CLI, stops through the guarded
/// dispatcher, and the five settings from the app's one settings store, now
/// and after each change. Its ticker runs while the tab shows a session.
#[cfg(feature = "live")]
fn processes_hub(app: &AppHandle, allowed: bool) -> processes::ProcessHub {
    use std::sync::Arc;
    use tauri::Manager;
    let pump = app.state::<commands::AppState>().pump();
    let start = std::time::Instant::now();
    let follows = app.clone();
    let unfollows = app.clone();
    let intervals = app.clone();
    let refreshes = app.clone();
    let actions = app.state::<actions::Actions>().inner().clone();
    let hgs = zerus_client_core::default_hgs();
    let hub = processes::ProcessHub::new(
        processes::ProcessDeps {
            spawn: Arc::new(|job: processes::Job| {
                tauri::async_runtime::spawn_blocking(job);
            }),
            notify: Arc::new(move || pump.poke()),
            clock_ms: Arc::new(move || start.elapsed().as_millis() as u64),
            epoch: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64())
            }),
            follow: Arc::new(move |target: &processes::ProcessTarget| {
                follows
                    .state::<commands::AppState>()
                    .follow_processes(target)
            }),
            unfollow: Arc::new(move |target: &processes::ProcessTarget| {
                unfollows
                    .state::<commands::AppState>()
                    .unfollow_processes(target)
            }),
            set_interval: Arc::new(move |target, ms| {
                let _ = intervals
                    .state::<commands::AppState>()
                    .set_processes_interval(target, ms);
            }),
            refresh: Arc::new(move |target| {
                let _ = refreshes
                    .state::<commands::AppState>()
                    .refresh_processes(target);
            }),
            read: Arc::new(
                move |computer: &zerus_client_core::ComputerId, read: &zerus_client_core::Read| {
                    zerus_client_core::reads::read(&hgs, computer, read)
                        .map_err(|error| error.to_string())
                },
            ),
            // One request id per stop: a stop is never sent twice.
            stop: Arc::new(
                move |target: zerus_client_core::Target, action: zerus_client_core::Action| {
                    tauri::async_runtime::block_on(actions::dispatch_as(
                        &actions,
                        target,
                        action,
                        uuid::Uuid::new_v4().to_string(),
                    ))
                },
            ),
        },
        allowed,
    );
    // Every change of a `processes/*` setting reads the five again; the
    // first read waits for the store to load, off the setup thread.
    let changed = hub.clone();
    let store = app.clone();
    app.state::<settings::Store>()
        .0
        .subscribe(Box::new(move |key, _, _| {
            if key.starts_with("processes/") {
                if let Some(store) = store.try_state::<settings::Store>() {
                    changed.settings_from(&store.0);
                }
            }
        }));
    let first = hub.clone();
    let store = app.state::<settings::Store>().0.clone();
    let started = std::thread::Builder::new()
        .name("zerus-processes-settings".into())
        .spawn(move || first.settings_from(&store));
    if let Err(error) = started {
        eprintln!("zerus-client: the Processes settings were not read: {error}");
    }
    hub.start_ticker();
    hub
}

/// A `sessions` line from a later start opens and raises the window; `show`
/// changes nothing visible (`main.cpp:424-433`).
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn forward_activations(
    app: tauri::AppHandle,
    activations: std::sync::mpsc::Receiver<instance::Activation>,
) {
    std::thread::Builder::new()
        .name("zerus-client-activations".into())
        .spawn(move || {
            for activation in activations {
                #[cfg(all(feature = "live", target_os = "linux"))]
                if let instance::Activation::Navigation(target) = activation {
                    if let Some(sender) = desk::sender(&app) {
                        let _ = sender.send(desk::DeskInput::Navigate {
                            target,
                            activation: None,
                        });
                    }
                    continue;
                }
                if activation == instance::Activation::Sessions {
                    let target = app.clone();
                    let _ = app.run_on_main_thread(move || {
                        if let Err(error) = lifecycle::request_main(&target) {
                            eprintln!("zerus-client: could not open the window: {error}");
                        }
                    });
                }
            }
        })
        .expect("the activation thread starts");
}

/// The main-thread watchdog (Task 8): Qt's thresholds, a heartbeat on the main
/// thread and, on macOS, the hold around sleep. The returned value keeps it running.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn start_watchdog() -> std::io::Result<watchdog::Watchdog> {
    let limits = watchdog::Limits::QT;
    let dog = watchdog::Watchdog::start(limits, watchdog::now_ms, |silent| {
        watchdog::abort_on_hang(silent)
    })?;
    let pulse = dog.pulse();
    #[cfg(target_os = "macos")]
    {
        platform::macos::heartbeat(
            objc2::MainThreadMarker::new().expect("setup runs on the main thread"),
            limits.beat_every(),
            move || pulse.beat(watchdog::now_ms()),
        );
        platform::macos_power::observe(dog.pulse());
    }
    #[cfg(target_os = "linux")]
    platform::linux::heartbeat(limits.beat_every(), move || pulse.beat(watchdog::now_ms()));
    eprintln!(
        "zerus-client: main-thread watchdog on ({} s)",
        limits.hang_ms / 1000
    );
    Ok(dog)
}

/// Before the app quits (`lifecycle::exit`, before the socket goes): the
/// last settings change gets a bounded wait, pending project edits and the
/// layout reach the disk, recordings under way are cancelled, the hubs take
/// no more changes (unsent process stops are dropped), every draft save
/// already queued reaches the disk, and the unsent read marks and the
/// attention marks are saved (a bounded wait).
#[cfg(feature = "live")]
pub(crate) fn exit_work(app: &AppHandle) {
    use tauri::Manager;
    if let Some(settings) = app.try_state::<settings::Store>() {
        settings.0.flush_within(settings::EXIT_FLUSH);
    }
    if let Some(organization) = app.try_state::<organization::Organization>() {
        organization.flush();
    }
    if let Some(processes) = app.try_state::<processes::ProcessHub>() {
        processes.close();
    }
    #[cfg(feature = "media")]
    if let Some(voices) = app.try_state::<voice::Voices>() {
        voices.close_all();
    }
    let composers = app.try_state::<composer::Composers>();
    close_drafts(composers.as_deref(), || {
        if let Some(drafts) = app.try_state::<Drafts>() {
            drafts.0.flush();
        }
    });
    creations::flush_on_exit(app);
    actions::flush_on_exit(app);
}

/// The composers close before the store's last flush: an attach read under
/// way ends and its save is queued first, and nothing is queued after it.
#[cfg(feature = "live")]
fn close_drafts(composers: Option<&composer::Composers>, flush_store: impl FnOnce()) {
    if let Some(composers) = composers {
        composers.flush();
    }
    flush_store();
}

/// SIGTERM, SIGINT or SIGHUP (`signals.rs`, Qt's `TerminationSignals`):
/// the app quits as Qt's posted quit does, through the page flush helper
/// before `RunEvent::Exit` runs the one final exit hook.
/// Should the process not end within five seconds, the fallback ends it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exit_on_signals(app: AppHandle) {
    let installed = signals::install(move |_signal| {
        let stalled = app.clone();
        std::thread::spawn(move || {
            force_exit_after(
                std::time::Duration::from_secs(5),
                || flush_drafts(&stalled),
                || std::process::exit(0),
            )
        });
        exit_flush::quit_with_flush(&app, 0);
    });
    if let Err(error) = installed {
        eprintln!("zerus-client: termination signals keep their default action: {error}");
    }
}

/// The fallback of a quit that stalls (the event loop is stuck, so the exit
/// hook never runs): after `grace`, the draft store writes what is queued,
/// then the process ends. In this order, so the owner's typed drafts reach
/// the disk first. Only the store is flushed here, since it is thread-safe
/// and a second flush is harmless; voice and anything else bound to the
/// main thread is left alone. A flush that hangs keeps the process up, and
/// a second signal still ends it (SA_RESETHAND), as in Qt.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn force_exit_after(grace: std::time::Duration, flush_drafts: impl FnOnce(), end: impl FnOnce()) {
    std::thread::sleep(grace);
    flush_drafts();
    end();
}

/// Writes every queued draft save of the app's one store, if it has one.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn flush_drafts(app: &AppHandle) {
    #[cfg(feature = "live")]
    {
        use tauri::Manager;
        if let Some(drafts) = app.try_state::<Drafts>() {
            drafts.0.flush();
        }
    }
    #[cfg(not(feature = "live"))]
    let _ = app;
}

/// The commands the frontend may call. The session actions need the core's
/// `hgs` runner, so they exist only with the `live` feature.
fn main_commands(
    handler: impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static,
) -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    move |invoke| {
        if !native_ui::app_commands_allowed(invoke.message.webview().label()) {
            invoke.resolver.reject("not allowed");
            return true;
        }
        handler(invoke)
    }
}

#[cfg(feature = "live")]
fn handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        commands::start,
        commands::fleet_refresh,
        commands::next_batch,
        commands::follow,
        commands::unfollow,
        commands::metrics,
        commands::perf_config,
        commands::save_perf,
        commands::perf_log,
        commands::smoke_log,
        commands::smoke_resume,
        commands::display_hz,
        commands::exit_app,
        exit_flush::exit_flush_ack,
        commands::perf_away,
        commands::perf_resize,
        desktop_service::desktop_service_mode,
        desktop_service::desktop_service_retry,
        desktop_service::desktop_service_ownership_rearm,
        desktop_service::desktop_service_ownership_result,
        desktop_service::notification_controls::desktop_notification_preferences_set,
        desktop_service::notification_controls::desktop_notification_preferences_check,
        desktop_service::desktop_service_read_marks,
        desktop_service::desktop_service_automatic_approvals,
        desktop_service::frontend::desktop_service_maintenance_configure,
        desktop_service::frontend::desktop_service_maintenance_configuration,
        desktop_service::frontend::desktop_service_frontend_result,
        commands::build_info,
        commands::voice_support,
        renderer::renderer_frame,
        content_search::content_search,
        content_search::cancel_content_search,
        native_ui::runtime::native_ui_open,
        native_ui::runtime::native_ui_browser,
        native_ui::runtime::native_ui_retire,
        native_ui::runtime::native_ui_visible,
        native_ui::runtime::native_ui_bounds,
        settings::skins::user_skins_load,
        settings::skins::user_skin_import,
        settings::skins::user_skins_open,
        settings::settings_get,
        settings::settings_set,
        desk::desk_take_pending,
        window_layer::window_layer_state,
        settings::hgs_version,
        settings::open_repository,
        speech_settings::speech_input,
        files::files_usage,
        files::files_apply_limit,
        files::files_open_folder,
        analytics::analytics_view,
        analytics::analytics_refresh,
        analytics::analytics_rows,
        analytics::analytics_age,
        analytics::analytics_configure,
        analytics::analytics_configure_answer,
        analytics::analytics_purge_check,
        analytics::analytics_purge,
        analytics::analytics_review,
        analytics::analytics_review_cancel,
        analytics::analytics_save_review,
        approvals::approvals_list,
        approvals::approvals_detail,
        worktree_policy::worktree_policy_view,
        worktree_policy::worktree_policy_read,
        worktree_policy::worktree_policy_choose,
        worktree_policy::worktree_policy_apply,
        mobile::mobile_phone_open,
        mobile::mobile_phone_close,
        mobile::mobile_phone_invite,
        mobile::mobile_phone_revoke,
        mobile::mobile_computer_open,
        mobile::mobile_computer_close,
        mobile::mobile_computer_invite,
        mobile::mobile_computer_cancel,
        mobile::mobile_computer_check,
        mobile::mobile_workspace_open,
        mobile::mobile_workspace_check_open,
        mobile::mobile_workspace_close,
        mobile::mobile_workspace_apply,
        mobile::mobile_workspace_continue,
        mobile::mobile_connector_open,
        mobile::mobile_connector_apply,
        mobile::mobile_connector_close,
        mobile::mobile_legacy_open,
        mobile::mobile_legacy_apply,
        mobile::mobile_legacy_close,
        mobile::mobile_view,
        mobile::mobile_open,
        mobile::mobile_refresh_workspace,
        mobile::mobile_refresh_phones,
        mobile::mobile_delivery_set,
        recovery::recovery_view,
        recovery::recovery_load,
        recovery::recovery_save,
        recovery::recovery_reload,
        speech_settings::open_speech_documentation,
        actions::run_action,
        actions::untracked_shell_identity,
        action_delay::action_delay_begin,
        action_delay::action_delay_wait,
        action_delay::action_delay_cancel,
        actions::set_notifications,
        actions::inspect_session,
        actions::inspect_subagent_identity,
        session_folders::session_folder_status,
        session_folders::session_folder_open,
        actions::inspect_subagent_activity,
        commands::tasks_follow,
        task_details::task_details,
        task_details::task_assign,
        task_details::task_board_action,
        task_details::task_cancel_execution,
        task_start::task_start_open,
        task_start::task_start,
        task_start::task_start_release,
        task_executor::task_executor_open,
        task_executor::task_executor_submit,
        task_executor::task_executor_release,
        task_settings::task_settings,
        task_editor::task_editor_open,
        task_editor::task_editor_edit,
        task_editor::task_editor_files,
        task_editor::task_editor_pick,
        task_editor::task_editor_submit,
        task_editor::task_editor_release,
        task_forms::task_form_open,
        task_forms::task_form_edit,
        task_forms::task_form_submit,
        task_forms::task_form_cancel,
        task_details::task_attachment_open,
        actions::pending_read_marks,
        actions::queue_read_marks,
        actions::attention_marks,
        actions::update_attention_marks,
        actions::session_reference,
        actions::copy_command,
        actions::archive_page,
        actions::archived_activity,
        actions::cancel_archive_pages,
        actions::reset_archive_pages,
        clipboard::copy_text,
        clipboard::copy_verbatim,
        organization::move_to_project,
        organization::create_project,
        organization::rename_project,
        organization::set_project_color,
        organization::delete_project,
        organization::move_project,
        organization::sort_project,
        organization::set_project_collapsed,
        organization::set_default_project,
        organization::project_folders,
        worktree_cleanup::manual::worktree_overview_review,
        worktree_cleanup::manual::worktree_manual_view,
        worktree_cleanup::manual::worktree_manual_remove,
        worktree_cleanup::manual::worktree_manual_check,
        worktree_cleanup::manual::worktree_manual_stop,
        worktree_cleanup::manual::worktree_manual_inspect,
        organization::overview::worktree_overview_open,
        organization::swarm_dialog::project_swarm_open,
        organization::swarm_dialog::project_swarm_view,
        organization::swarm_dialog::project_swarm_preview,
        organization::swarm_dialog::project_swarm_join,
        organization::swarm_dialog::project_swarm_sync,
        organization::swarm_dialog::project_swarm_close,
        organization::swarm_dialog::conflicts::project_swarm_conflicts,
        organization::swarm_dialog::conflicts::project_swarm_conflict_review,
        organization::swarm_dialog::conflicts::project_swarm_resolve,
        organization::overview::worktree_overview_refresh,
        organization::overview::worktree_overview_page,
        organization::overview::worktree_overview_select,
        organization::overview::worktree_overview_candidates,
        organization::overview::worktree_overview_close,
        organization::overview::worktree_overview_inspect,
        organization::clones::project_clone_open,
        organization::clones::project_clone_status,
        organization::clones::project_clone_step,
        organization::clones::project_clone_close,
        organization::clones::project_clone_use,
        organization::folders::project_folder_change,
        organization::folders::project_folder_open,
        screens::screen_visible,
        screens::session_usage,
        screens::accounts_view,
        screens::account_edits::account_edit_open,
        screens::account_edits::account_edit_apply,
        screens::account_edits::account_edit_close,
        screens::account_edits::setup::account_setup_open,
        screens::account_edits::setup::account_setup_apply,
        screens::account_edits::setup::account_setup_close,
        screens::account_edits::distribution::account_distribution_open,
        screens::account_edits::distribution::account_distribution_apply,
        screens::account_edits::distribution::account_distribution_close,
        screens::account_filters_view,
        screens::machines_view,
        screens::machine_resolve,
        screens::machine_check,
        screens::machine_change,
        screens::machine_ssh,
        screens::machine_setup::machine_setup,
        screens::machine_setup::machine_pick_source,
        screens::machine_pick_key,
        screens::projects_view,
        screens::launch_accounts,
        screens::launch_account_rows,
        screens::launch_accounts_close,
        newsession::new_session_context,
        creations::creation_submit,
        creations::creations_view,
        creations::creation_details,
        creations::creation_action,
        creations::creation_plan,
        newsession::new_session_form,
        newsession::new_session_start,
        newsession::new_session_plan,
        newsession::launch_warning,
        newsession::repository_fields,
        newsession::worktree_snapshot,
        newsession::worktree_catalog,
        newsession::create_worktree,
        newsession::list_directories,
        newsession::create_directory,
        newsession::pick_local_folder,
        newsession::local_folder_exists,
        newsession::opencode_models,
        newsession::opencode_favorite,
        newsession::opencode_start,
        terminal::terminal_connect,
        terminal::terminal_write,
        terminal::terminal_resize,
        terminal::terminal_ack,
        terminal::terminal_disconnect,
        terminal::terminal_retarget,
        terminal::terminal_status,
        terminal::terminal_drop,
        terminal::terminal_shown,
        composer::composer_open,
        composer::composer_edit,
        composer::composer_pick,
        composer::composer_attach,
        composer::composer_paste_image,
        composer::composer_remove,
        composer::composer_undo_remove,
        composer::composer_allow_retry,
        composer::composer_refresh,
        composer::composer_clear_control,
        composer::composer_tab,
        drops::dropped_files,
        composer::fetch::attachment_fetch,
        composer::fetch::attachment_media,
        composer::fetch::attachment_pin,
        composer::fetch::attachment_unpin,
        composer::fetch::attachment_open,
        composer::fetch::attachment_open_draft,
        termination_cleanup::termination_cleanup_inspect,
        termination_cleanup::termination_cleanup_view,
        termination_cleanup::termination_cleanup_offer,
        termination_cleanup::termination_cleanup_cancel,
        termination_cleanup::termination_cleanup_confirm,
        termination_cleanup::termination_cleanup_check,
        termination_cleanup::termination_cleanup_remove,
        termination_cleanup::termination_cleanup_keep,
        termination_cleanup::termination_cleanup_restore,
        worktree_cleanup::worktree_cleanup_open,
        worktree_cleanup::reminder::worktree_cleanup_reminder,
        worktree_cleanup::worktree_cleanup_view,
        worktree_cleanup::worktree_cleanup_refresh,
        worktree_cleanup::worktree_cleanup_prepare,
        worktree_cleanup::worktree_cleanup_confirm,
        worktree_cleanup::worktree_cleanup_close,
        session_files::session_file_dialog,
        session_files::session_file_check,
        session_files::session_file_download,
        session_files::session_file_save,
        session_files::session_file_external,
        session_files::session_file_act,
        session_files::session_file_close,
        composer::saved::saved_drafts,
        composer::saved::saved_draft,
        composer::saved::draft_conflicts,
        composer::saved::draft_recover,
        composer::saved::draft_restore,
        composer::send::send_message,
        composer::queue::queue_send_now,
        composer::queue::send_push,
        composer::send::send_answer,
        composer::send::local_retry,
        composer::send::local_delete,
        composer::send::local_inspect,
        composer::send::send_cancel,
        composer::send::send_interrupted,
        composer::send::clear_context,
        composer::questions::question_edit,
        composer::questions::question_page,
        composer::questions::question_navigate,
        composer::questions::question_allow_retry,
        processes::processes_follow,
        processes::processes_shown,
        processes::processes_select,
        processes::processes_stop,
        composer::questions::question_submit,
        composer::questions::question_decide,
        composer::questions::question_skip,
        composer::questions::question_cold,
        updates::updates_view,
        updates::updates_check,
        updates::updates_set_channel,
        updates::updates_set_automatic,
        updates::updates_open,
        updates::updates_copy_command,
        updates::updates_restart,
        updates::updates_fix_login_item,
        #[cfg(feature = "media")]
        voice::voice_open,
        #[cfg(feature = "media")]
        voice::voice_input,
        #[cfg(feature = "media")]
        voice::voice_close,
        #[cfg(feature = "media")]
        voice::voice_preview,
        #[cfg(feature = "media")]
        voice::voice_permission,
        #[cfg(feature = "media")]
        voice::voice_open_settings,
        #[cfg(feature = "media")]
        voice::voice_microphones,
        #[cfg(feature = "media")]
        voice::voice_video,
    ]
}

#[cfg(not(feature = "live"))]
#[tauri::command]
fn desk_take_pending() -> serde_json::Value {
    serde_json::json!({ "open": null, "notices": [] })
}

#[cfg(not(feature = "live"))]
fn handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        commands::start,
        commands::fleet_refresh,
        commands::next_batch,
        commands::follow,
        commands::unfollow,
        commands::tasks_follow,
        commands::metrics,
        commands::perf_config,
        commands::save_perf,
        commands::perf_log,
        commands::display_hz,
        commands::exit_app,
        exit_flush::exit_flush_ack,
        commands::perf_away,
        commands::perf_resize,
        commands::build_info,
        commands::voice_support,
        renderer::renderer_frame,
        clipboard::copy_text,
        clipboard::copy_verbatim,
        desk_take_pending,
    ]
}

fn app_navigation_allowed(url: &tauri::Url) -> bool {
    // `tauri://localhost` is a non-special URL: its root path parses as "" rather than "/".
    if !matches!(url.path(), "" | "/" | "/index.html") {
        return false;
    }
    matches!(
        (url.scheme(), url.host_str(), url.port()),
        ("tauri", Some("localhost"), None)
            | ("http", Some("tauri.localhost"), None)
            | ("http", Some("127.0.0.1"), Some(1420))
    )
}

#[derive(Default)]
struct MainWindow(std::sync::Mutex<Option<WebviewWindow>>);

/// Tauri's get_webview_window only recognizes windows with exactly one
/// matching webview. Retain the original main pair when native children are
/// attached. A cached pair must still answer both native window and webview
/// queries; a destroyed resource can never authorize reuse of its old handle.
pub(crate) fn main_window(app: &AppHandle) -> Option<WebviewWindow> {
    use tauri::Manager;
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        return Some(window);
    }
    app.try_state::<MainWindow>()
        .and_then(|state| {
            state
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        })
        .filter(|window| window.is_visible().is_ok() && window.url().is_ok())
}

/// Creates the main window and applies the platform display-rate preference:
/// macOS before the webview is built, Linux through its public settings API.
pub fn create_main_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    use tauri::Manager;
    // ZERUS_CLIENT_QUERY opens a given state (`select=<session name>`) for
    // headless screenshots and scripted measurements, as the mockup's URL
    // parameters do; ZERUS_CLIENT_PERF=auto adds `perf=auto`.
    let mut query: Vec<String> = std::env::var("ZERUS_CLIENT_QUERY")
        .ok()
        .filter(|q| !q.is_empty())
        .into_iter()
        .collect();
    if let Ok(mode) = std::env::var("ZERUS_CLIENT_PERF") {
        if !mode.is_empty() {
            query.push(format!("perf={mode}"));
        }
    }
    let page = match query.is_empty() {
        true => "index.html".to_owned(),
        false => format!("index.html?{}", query.join("&")),
    };
    #[allow(unused_mut)]
    let mut builder = WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::App(page.into()))
        .on_navigation(app_navigation_allowed)
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
        .title("Zerus Next")
        .visible(false)
        .inner_size(1440.0, 900.0)
        .min_inner_size(900.0, 560.0);
    // Page rendering at the display rate, not near 60 fps: the preference is set
    // on the configuration before the webview exists (toggling a live page is
    // unreliable). The measurement's control run keeps WebKit's default.
    #[cfg(target_os = "macos")]
    if !keep_default_preference() {
        if let Some(mtm) = objc2::MainThreadMarker::new() {
            builder =
                builder.with_webview_configuration(platform::macos::webview_configuration(mtm));
        }
    }
    let window = builder.build()?;
    #[cfg(feature = "live")]
    if let Some(store) = app.try_state::<settings::Store>() {
        window_geometry::attach(&window, store.0.clone());
    }
    if let Some(state) = app.try_state::<MainWindow>() {
        *state.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(window.clone());
    }
    #[cfg(target_os = "linux")]
    if !keep_default_preference() {
        window.with_webview(|webview| {
            if let Err(error) = platform::linux::disable_near_60fps(&webview.inner()) {
                eprintln!("zerus-client: could not disable the near-60 fps preference: {error}");
            }
        })?;
    }
    Ok(window)
}

/// `ZERUS_CLIENT_PERF_PREFERENCE=default` leaves the macOS and Linux WebKit
/// frame-rate preference at its default: the must-fail control run of the measurement.
pub fn keep_default_preference() -> bool {
    std::env::var("ZERUS_CLIENT_PERF_PREFERENCE").is_ok_and(|value| value == "default")
}

#[cfg(test)]
mod navigation_tests {
    use super::app_navigation_allowed;

    #[test]
    fn navigation_stays_on_the_app_entry() {
        for url in [
            "http://127.0.0.1:1420/",
            "http://127.0.0.1:1420/index.html?perf=auto",
            "tauri://localhost/index.html",
            "tauri://localhost",
            "tauri://localhost/",
            "http://tauri.localhost/index.html",
        ] {
            assert!(
                app_navigation_allowed(&tauri::Url::parse(url).unwrap()),
                "{url}"
            );
        }
        for url in [
            "https://example.com/",
            "http://127.0.0.1:1420/remote/file",
            "http://localhost:1420/",
            "tauri://localhost/remote/file",
            "file:///tmp/remote-file",
        ] {
            assert!(
                !app_navigation_allowed(&tauri::Url::parse(url).unwrap()),
                "{url}"
            );
        }
    }
}

#[cfg(all(test, feature = "live"))]
mod tests {
    use super::*;
    use composer::tests::{dir, drafts, session, until, KEY};
    use composer::{Composers, Deps, Job};
    use std::sync::Arc;
    use std::time::Duration;
    use zerus_client_core::attachments::Selection;
    use zerus_client_core::drafts::read_draft;

    #[test]
    fn the_exit_closes_the_composers_before_the_store_flushes() {
        let root = dir("exit-order");
        let file = root.join("notes.txt");
        std::fs::write(&file, "attached while quitting").unwrap();
        let store = drafts(&root);
        // An attach read still under way when the app quits.
        let composers = Composers::new(
            Arc::clone(&store),
            composer::Config {
                cache: root.join("cache"),
                qt_drafts: None,
                dev_drafts: Vec::new(),
            },
            Deps {
                spawn: Arc::new(|job: Job| {
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(200));
                        job();
                    });
                }),
                notify: Arc::new(|| {}),
                clock: Arc::new(|| 1_793_000_100.0),
                rows: Arc::new(|_, _, _| None),
                facts: Arc::new(|_, _| None),
                reconnect: std::sync::Arc::new(|_| {}),
                inspect: Arc::new(|_, _, _| Err("timed out".into())),
                dispatch: Arc::new(|_, _, _| Err("no call in these tests".into())),
            },
        );
        composers.open(Some(session()));
        let view = until(&composers, |v| v.loaded);
        composers.attach_paths(view.handle, vec![file], Selection::default());
        let mut at_flush = None;
        close_drafts(Some(&composers), || {
            at_flush = composers.current();
            store.flush();
        });
        let at_flush = at_flush.unwrap();
        assert!(
            !at_flush.attaching && at_flush.attachments.len() == 1,
            "the store flushed before the composers closed: {at_flush:?}"
        );
        assert_eq!(
            read_draft(&root.join("drafts"), KEY)
                .draft
                .draft
                .attachments
                .len(),
            1
        );
    }

    /// Runs only in a child that the signal tests start (`ZERUS_SIGNAL_PROBE`).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn signal_probe_in_a_child_only() {
        let (quit, quitting) = std::sync::mpsc::channel();
        let handled = match std::env::var("ZERUS_SIGNAL_PROBE").as_deref() {
            Ok(_) => signals::install(move |signal| {
                let _ = quit.send(signal);
            })
            .unwrap(),
            Err(_) => return,
        };
        println!("handled {handled:?}");
        if std::env::var("ZERUS_SIGNAL_PROBE").as_deref() == Ok("twice") {
            // SAFETY: raising a signal at this process.
            unsafe { libc::raise(libc::SIGTERM) };
            let signal = quitting.recv_timeout(Duration::from_secs(5)).unwrap();
            println!("quit {signal}");
            unsafe { libc::raise(libc::SIGTERM) };
            std::thread::sleep(Duration::from_secs(1));
            println!("survived the second");
        }
    }

    /// This test binary again, running only the probe, with SIGTERM and
    /// SIGINT at their default action and SIGHUP ignored, as `nohup` starts
    /// a program.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn probe(kind: &str) -> std::process::Output {
        use std::os::unix::process::CommandExt;
        // A child process: shared with the other tests that start one, never
        // while one samples this process's tree.
        let _tree = crate::PROCESS_TREE
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "tests::signal_probe_in_a_child_only",
                "--nocapture",
            ])
            .env("ZERUS_SIGNAL_PROBE", kind);
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                libc::signal(libc::SIGTERM, libc::SIG_DFL);
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                Ok(())
            });
        }
        command.output().unwrap()
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_signal_ignored_at_start_stays_ignored() {
        let output = probe("kept");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        let expected = format!("handled [{}, {}]", libc::SIGTERM, libc::SIGINT);
        assert!(stdout.contains(&expected), "{stdout}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_second_signal_ends_a_quitting_process() {
        use std::os::unix::process::ExitStatusExt;
        let output = probe("twice");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(&format!("quit {}", libc::SIGTERM)),
            "{stdout}"
        );
        assert!(!stdout.contains("survived the second"), "{stdout}");
        assert_eq!(output.status.signal(), Some(libc::SIGTERM));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_stalled_quit_writes_the_drafts_before_the_process_ends() {
        let root = dir("stalled-quit");
        let store = drafts(&root);
        // A save still queued behind a busy writer when the fallback fires.
        let release = composer::tests::hold(&store);
        let mut typed = zerus_client_core::drafts::StoredDraft::default();
        typed.draft.text = "typed before the signal".into();
        store.save(KEY, typed, |_| {});
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let _ = release.send(());
        });
        let mut at_exit = None;
        force_exit_after(
            Duration::ZERO,
            || store.flush(),
            || at_exit = Some(read_draft(&root.join("drafts"), KEY).draft.draft.text),
        );
        assert_eq!(at_exit.as_deref(), Some("typed before the signal"));
    }

    #[test]
    fn the_attachment_cache_is_the_clients_own() {
        let cache = std::path::Path::new("/home/example/.cache/dev.zerus.next");
        assert_eq!(attachments_root(cache, false), cache.join("attachments"));
        assert_eq!(
            attachments_root(cache, true),
            cache.join("attachments-replay"),
            "a replay never touches the real cache"
        );
        let qt = std::path::Path::new("/home/example/.cache/hgdev/hgs-tray/attachments");
        assert!(!attachments_root(cache, false).starts_with(qt) && !qt.starts_with(cache));
    }

    #[test]
    fn the_draft_store_is_the_clients_own() {
        let data = std::path::Path::new("/home/example/.local/share/dev.zerus.next");
        assert_eq!(drafts_root(data, false), data.join("drafts"));
        assert_eq!(
            drafts_root(data, true),
            data.join("drafts-replay"),
            "a replay never touches the real drafts"
        );
        if let Some(qt) = zerus_client_core::drafts::qt_root() {
            let root = drafts_root(data, false);
            assert!(
                !root.starts_with(&qt) && !qt.starts_with(&root),
                "Qt's store is only read"
            );
        }
    }

    /// Every `text!`/`source!` literal in the app is a catalog source, so the
    /// webview can translate it (as the core's own coverage test checks).
    #[test]
    fn every_marked_app_source_is_in_the_generated_catalog() {
        fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    collect(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let catalog: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(root.join("../src/i18n.ru.json")).unwrap(),
        )
        .unwrap();
        let mut files = Vec::new();
        collect(&root.join("src"), &mut files);
        let sources: Vec<String> = files
            .iter()
            .flat_map(|file| {
                zerus_client_core::text::marked_sources(&std::fs::read_to_string(file).unwrap())
            })
            .collect();
        assert!(sources.iter().any(|s| s == "Waiting to send…"));
        let missing: Vec<_> = sources
            .iter()
            .filter(|s| !catalog.contains_key(s.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "app sources missing from client/src/i18n.ru.json: {missing:#?}"
        );
    }
}
