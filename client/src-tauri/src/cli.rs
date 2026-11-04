//! The command line of Qt's `hgs-tray` (`tray/src/AppConfig.cpp`), so the
//! LaunchAgent (`--hgs PATH`), the updater (`--build-info`) and launchers
//! (`--sessions`, `--background`) start this client unchanged.
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// `--hgs PATH`: the `hgs` to run; the LaunchAgent always passes it.
    pub hgs: Option<PathBuf>,
    /// `--sessions`: open the window, here or in the running instance.
    pub sessions: bool,
    /// Exact UI navigation data, never a notification callback/permission.
    #[cfg(all(feature = "live", target_os = "linux"))]
    pub target: Option<zerus_client_core::desktop::activation::Target>,
    /// `--background`: start in the menu bar only.
    pub background: bool,
    /// Qt options this client accepts and does not use.
    pub ignored: Vec<String>,
}

impl Options {
    /// Whether a start asks for the window or just the background shell.
    /// The switch bundle starts hidden on macOS; Windows follows Qt and opens
    /// normally unless --background was given. A second background start sends
    /// only the non-visible `show` activation.
    pub fn opens_sessions(&self, switch: bool) -> bool {
        self.opens_sessions_on(switch, cfg!(windows))
    }
    pub fn opens_sessions_on(&self, switch: bool, windows: bool) -> bool {
        self.sessions || ((!switch || windows) && !self.background)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Run(Options),
    Help,
    Version,
    BuildInfo,
    LoginItem { enabled: bool },
}

/// Consume only this new GUI process's launch input. The environment is never
/// a callback authority; reject conflicts rather than silently changing targets.
#[cfg(all(feature = "live", target_os = "linux"))]
pub fn take_navigation_environment(options: &mut Options) -> Result<(), String> {
    use zerus_client_core::desktop::activation::{decode_navigation, NAVIGATION_ENV};
    let raw = std::env::var_os(NAVIGATION_ENV);
    std::env::remove_var(NAVIGATION_ENV);
    let Some(raw) = raw else {
        return Ok(());
    };
    if options.target.is_some() || options.background {
        return Err("Conflicting desktop navigation inputs".into());
    }
    let value = raw
        .into_string()
        .map_err(|_| "Invalid desktop target environment")?;
    let target = decode_navigation(&value).ok_or("Invalid desktop target environment")?;
    options.target = Some(target);
    options.sessions = true;
    Ok(())
}

/// Called once before GUI workers start; never mutates the service's environment.
#[cfg(all(feature = "live", target_os = "linux"))]
pub fn take_activation_token(navigation: bool) -> Option<String> {
    if !navigation {
        return None;
    }
    let value = std::env::var("XDG_ACTIVATION_TOKEN").ok();
    std::env::remove_var("XDG_ACTIVATION_TOKEN");
    value.filter(|value| valid_activation_token(value))
}

#[cfg(all(feature = "live", target_os = "linux"))]
pub fn valid_activation_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= zerus_client_core::desktop::notifications::ACTIVATION_TOKEN_BYTES
        && !value.chars().any(char::is_control)
}

/// Qt options with a value that this client does not use.
const IGNORED_WITH_VALUE: [&str; 5] = [
    "--local-poll",
    "--peer-poll",
    "--terminal",
    "--icon-color",
    "--open-mode",
];

/// Qt's `AppConfig::load` (`AppConfig.cpp:486-587`): options in order, the
/// first help, version or error answers.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut options = Options::default();
    let mut login_item = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "-h" | "--help" => return Ok(Command::Help),
            "--version" => return Ok(Command::Version),
            "--build-info" => return Ok(Command::BuildInfo),
            "--sessions" => options.sessions = true,
            "--background" => options.background = true,
            #[cfg(all(feature = "live", target_os = "linux"))]
            zerus_client_core::desktop::activation::NAVIGATION_ARG => {
                if options.target.is_some() {
                    return Err("Give only one desktop target".into());
                }
                let value = args
                    .get(index + 1)
                    .ok_or("--desktop-target requires a value")?;
                options.target = Some(
                    zerus_client_core::desktop::activation::decode_navigation(value)
                        .ok_or("Invalid desktop target")?,
                );
                options.sessions = true;
                index += 1;
            }
            "--install-login-item" | "--remove-login-item" if cfg!(windows) => {
                if login_item.is_some() {
                    return Err("Give only one login-item command".into());
                }
                login_item = Some(arg == "--install-login-item");
            }
            "--selftest" => return Err("--selftest is not available in this client".to_owned()),
            "--hgs" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| format!("{arg} requires a value"))?;
                options.hgs = Some(PathBuf::from(value));
                index += 1;
            }
            _ if IGNORED_WITH_VALUE.contains(&arg) => {
                args.get(index + 1)
                    .ok_or_else(|| format!("{arg} requires a value"))?;
                options.ignored.push(arg.to_owned());
                index += 1;
            }
            // Older macOS passes a process serial number to apps it opens.
            _ if arg.starts_with("-psn_") => {}
            _ => return Err(format!("unknown option: {arg}")),
        }
        index += 1;
    }
    // Accepting both would silently ignore one of them.
    if options.sessions && options.background {
        return Err(
            "--sessions opens the session dashboard and --background starts without it; \
                    give only one of them"
                .to_owned(),
        );
    }
    Ok(
        login_item.map_or(Command::Run(options), |enabled| Command::LoginItem {
            enabled,
        }),
    )
}

#[cfg(not(windows))]
pub fn usage() -> String {
    let base = "Usage: hgs-tray [options]\n\n\
     \x20 --hgs <path>          path to the hgs binary; found in PATH by default\n\
     \x20 --sessions            open the Zerus window\n\
     \x20 --background          start in the menu bar only (the default for Zerus)\n\
     \x20 -h, --help            this text\n\
     \x20 --version             installed release version and exit\n\
     \x20 --build-info          embedded build/source identity as JSON and exit\n\n\
     Accepted for the Qt GUI's command line and not used: --local-poll, --peer-poll,\n\
     --terminal, --icon-color and --open-mode (each takes a value).\n";
    #[cfg(all(feature = "live", target_os = "linux"))]
    {
        format!(
            "{base}\n  {} <json>  open an exact computer/run/conversation\n{}\n{}\n",
            zerus_client_core::desktop::activation::NAVIGATION_ARG,
            zerus_client_core::desktop::activation::NAVIGATION_HELP_MARKER,
            zerus_client_core::desktop::activation::NAVIGATION_ENV_HELP_MARKER
        )
    }
    #[cfg(not(all(feature = "live", target_os = "linux")))]
    {
        base.to_owned()
    }
}

#[cfg(windows)]
pub fn usage() -> &'static str {
    "Usage: hgs-tray [options]\n\n  --hgs <path>          path to the hgs binary\n  --sessions            open the Zerus window\n  --background          start in the notification area without a window\n  --install-login-item  opt in to starting in the background at login\n  --remove-login-item   remove this app's per-user login registration\n  -h, --help            this text\n  --version             installed release version and exit\n  --build-info          embedded build/source identity as JSON and exit\n\nAccepted Qt options: --local-poll, --peer-poll, --terminal, --icon-color, --open-mode (each takes a value).\n"
}

/// What `build.rs` embedded.
pub struct BuildFacts {
    pub version: &'static str,
    pub dev_base: &'static str,
    pub dev_number: Option<&'static str>,
    pub source_commit: Option<&'static str>,
    pub source_dirty: Option<&'static str>,
    pub preview_label: Option<&'static str>,
}

/// `build.rs` sets each value, empty when the build has none.
const fn given(value: Option<&'static str>) -> Option<&'static str> {
    match value {
        Some(text) if !text.is_empty() => Some(text),
        _ => None,
    }
}

pub const BUILD: BuildFacts = BuildFacts {
    version: env!("ZERUS_VERSION"),
    dev_base: env!("ZERUS_DEV_BASE_VERSION"),
    dev_number: given(option_env!("ZERUS_DEV_BUILD_NUMBER")),
    source_commit: given(option_env!("ZERUS_SOURCE_COMMIT")),
    source_dirty: given(option_env!("ZERUS_SOURCE_DIRTY")),
    preview_label: given(option_env!("ZERUS_LOCAL_PREVIEW_LABEL")),
};

/// Qt's `UpdateController::buildInfo` (`tray/src/UpdateController.cpp:118-136`).
pub fn build_info(facts: &BuildFacts) -> Value {
    let number: u64 = facts
        .dev_number
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let commit = facts.source_commit.unwrap_or("");
    let version_name = match facts.preview_label {
        Some(label) => format!(
            "{}-preview.{label}.{}",
            facts.dev_base,
            commit.get(..7).unwrap_or(commit)
        ),
        None if number > 0 => format!("{}-dev.{number}", facts.dev_base),
        None => facts.version.to_owned(),
    };
    let mut info = json!({
        "version": facts.version,
        "versionName": version_name,
        "devBuildNumber": number,
        "sourceCommit": commit,
        "sourceDirty": facts.source_commit.map(|_| facts.source_dirty == Some("1")),
    });
    if let Some(label) = facts.preview_label {
        info["previewLabel"] = Value::from(label);
    }
    info
}

/// The build identity plus the running client's identity for the Linux updater.
/// macOS reads these fields from the signed bundle's Info.plist instead.
#[cfg(any(target_os = "linux", windows))]
pub fn build_info_for(facts: &BuildFacts, identifier: &str) -> Value {
    let mut info = build_info(facts);
    info["CFBundleIdentifier"] = Value::from(identifier);
    info["ZerusGUI"] = Value::from("tauri");
    // Older Tauri rollback builds must use stock libraries: they cannot restore
    // the launcher's environment before starting agents or external programs.
    info["localWebKitChildEnvironment"] = Value::from(1);
    info
}

/// `hgs-tray --version` (`main.cpp:196-200`).
pub fn version_line(facts: &BuildFacts) -> String {
    format!(
        "Zerus {}\n",
        build_info(facts)["versionName"]
            .as_str()
            .unwrap_or(facts.version)
    )
}

#[cfg(test)]
#[path = "../build_identity.rs"]
mod build_identity;

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[cfg(all(feature = "live", target_os = "linux"))]
    #[test]
    fn navigation_consumes_its_startup_inputs_in_an_isolated_child() {
        const CASE: &str =
            "cli::tests::navigation_consumes_its_startup_inputs_in_an_isolated_child";
        const GUARD: &str = "ZERUS_TEST_NAVIGATION_TOKEN";
        let target = zerus_client_core::desktop::activation::Target {
            computer: "peer.example".into(),
            run: "original-run".into(),
            conversation: "original-conversation".into(),
        };
        let encoded = zerus_client_core::desktop::activation::encode_navigation(&target).unwrap();
        use zerus_client_core::desktop::activation::NAVIGATION_ENV;
        if std::env::var(GUARD).as_deref() == Ok("1") {
            let mut options = Options::default();
            take_navigation_environment(&mut options).unwrap();
            assert_eq!(options.target, Some(target.clone()));
            assert!(options.opens_sessions(true));
            assert!(std::env::var_os(NAVIGATION_ENV).is_none());
            assert_eq!(
                std::env::var("XDG_ACTIVATION_TOKEN").as_deref(),
                Ok("synthetic-wayland")
            );
            assert_eq!(
                take_activation_token(true).as_deref(),
                Some("synthetic-wayland")
            );
            assert!(std::env::var_os("XDG_ACTIVATION_TOKEN").is_none());
            assert!(take_activation_token(true).is_none());
            for bad in [
                "{}".to_owned(),
                "x".repeat(8193),
                r#"{"computer":"peer","run":"r","conversation":"c","token":"injected"}"#.into(),
            ] {
                std::env::set_var(NAVIGATION_ENV, bad);
                let mut options = Options::default();
                assert!(take_navigation_environment(&mut options).is_err());
                assert_eq!(options, Options::default());
                assert!(std::env::var_os(NAVIGATION_ENV).is_none());
            }
            for mut options in [
                Options {
                    background: true,
                    ..Options::default()
                },
                Options {
                    target: Some(target),
                    ..Options::default()
                },
            ] {
                std::env::set_var(NAVIGATION_ENV, &encoded);
                let before = options.clone();
                assert!(take_navigation_environment(&mut options).is_err());
                assert_eq!(options, before);
                assert!(std::env::var_os(NAVIGATION_ENV).is_none());
            }
            use std::os::unix::ffi::OsStringExt;
            std::env::set_var(NAVIGATION_ENV, std::ffi::OsString::from_vec(vec![0xff]));
            assert!(take_navigation_environment(&mut Options::default()).is_err());
            assert!(std::env::var_os(NAVIGATION_ENV).is_none());
            return;
        }
        let invocation = zerus_client_core::actions::Invocation {
            args: vec![
                CASE.into(),
                "--exact".into(),
                "--test-threads=1".into(),
                "--color=never".into(),
            ],
            stdin: Some(Vec::new()),
            timeout: std::time::Duration::from_secs(30),
            request_id: None,
        };
        let output = zerus_client_core::run::run_owned(
            &std::env::current_exe().unwrap(),
            &invocation,
            &[
                (GUARD, "1"),
                ("XDG_ACTIVATION_TOKEN", "synthetic-wayland"),
                (NAVIGATION_ENV, &encoded),
            ],
            &std::sync::atomic::AtomicBool::new(false),
            |command| command.spawn(),
            std::sync::Arc::new(()),
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("1 passed; 0 failed; 0 ignored"), "{output}");
    }

    #[cfg(all(feature = "live", target_os = "linux"))]
    #[test]
    fn exact_desktop_navigation_is_explicit_and_never_parses_callback_or_token_data() {
        use zerus_client_core::desktop::activation::{self as wire, Target};
        let target = Target {
            computer: "peer.example".into(),
            run: "original-run".into(),
            conversation: "original-conversation".into(),
        };
        let encoded = wire::encode_navigation(&target).unwrap();
        let Command::Run(options) = parse(&args(&[
            "--hgs",
            "/fixture/hgs",
            wire::NAVIGATION_ARG,
            &encoded,
        ]))
        .unwrap() else {
            panic!("navigation");
        };
        assert_eq!(options.target, Some(target));
        assert!(options.opens_sessions(true));
        assert!(usage()
            .lines()
            .any(|line| line == wire::NAVIGATION_HELP_MARKER));
        assert!(usage()
            .lines()
            .any(|line| line == wire::NAVIGATION_ENV_HELP_MARKER));
        for argv in [
            vec![wire::NAVIGATION_ARG],
            vec![wire::NAVIGATION_ARG, "{}"],
            vec![
                wire::NAVIGATION_ARG,
                &encoded,
                wire::NAVIGATION_ARG,
                &encoded,
            ],
            vec![wire::NAVIGATION_ARG, &encoded, "--background"],
        ] {
            assert!(parse(&args(&argv)).is_err());
        }
        assert!(valid_activation_token("synthetic-wayland"));
        for token in ["".into(), "bad\nvalue".into(), "x".repeat(4097)] {
            assert!(!valid_activation_token(&token));
        }
    }

    #[test]
    fn the_launch_agent_command_line_runs() {
        assert_eq!(parse(&args(&[])), Ok(Command::Run(Options::default())));
        assert_eq!(
            parse(&args(&["--hgs", "/Users/example/.local/bin/hgs"])),
            Ok(Command::Run(Options {
                hgs: Some("/Users/example/.local/bin/hgs".into()),
                ..Options::default()
            }))
        );
        assert_eq!(
            parse(&args(&["--sessions", "-psn_0_12345"])),
            Ok(Command::Run(Options {
                sessions: true,
                ..Options::default()
            }))
        );
    }

    #[test]
    fn qt_options_the_client_does_not_use_are_accepted() {
        assert_eq!(
            parse(&args(&[
                "--local-poll",
                "2000",
                "--background",
                "--open-mode",
                "terminal"
            ])),
            Ok(Command::Run(Options {
                background: true,
                ignored: vec!["--local-poll".into(), "--open-mode".into()],
                ..Options::default()
            }))
        );
    }

    #[test]
    fn identity_and_help_answer_before_anything_starts() {
        assert_eq!(
            parse(&args(&["--hgs", "x", "--build-info"])),
            Ok(Command::BuildInfo)
        );
        assert_eq!(parse(&args(&["--version"])), Ok(Command::Version));
        assert_eq!(parse(&args(&["-h"])), Ok(Command::Help));
        assert_eq!(parse(&args(&["--help"])), Ok(Command::Help));
        // Qt answers the first of them, before a later mistake (`AppConfig.cpp:560-571`).
        assert_eq!(
            parse(&args(&["--sessions", "--background", "--help"])),
            Ok(Command::Help)
        );
    }

    #[test]
    fn mistakes_are_refused() {
        // Qt's own wording (`AppConfig.cpp:493`, `:570`, `:576-581`).
        assert_eq!(
            parse(&args(&["--hgs"])),
            Err("--hgs requires a value".to_owned())
        );
        assert_eq!(
            parse(&args(&["--open-mode"])),
            Err("--open-mode requires a value".to_owned())
        );
        assert_eq!(
            parse(&args(&["--frobnicate"])),
            Err("unknown option: --frobnicate".to_owned())
        );
        assert_eq!(
            parse(&args(&["--frobnicate", "--help"])),
            Err("unknown option: --frobnicate".to_owned())
        );
        assert_eq!(
            parse(&args(&["--selftest"])),
            Err("--selftest is not available in this client".to_owned())
        );
        assert_eq!(
            parse(&args(&["--sessions", "--background"])),
            Err("--sessions opens the session dashboard and --background starts without it; give only one of them"
                .to_owned())
        );
    }

    #[test]
    fn the_switch_bundle_starts_in_the_menu_bar_like_qt() {
        let plain = Options::default();
        assert!(
            !plain.opens_sessions_on(true, false),
            "Qt on macOS opens the window only with --sessions"
        );
        assert!(
            plain.opens_sessions(false),
            "other builds keep opening their window"
        );
        assert!(Options {
            sessions: true,
            ..Options::default()
        }
        .opens_sessions(true));
        assert!(!Options {
            background: true,
            ..Options::default()
        }
        .opens_sessions(false));
    }

    #[test]
    fn windows_opens_normally_but_duplicate_login_starts_stay_hidden() {
        let plain = Options::default();
        assert!(plain.opens_sessions_on(true, true));
        let background = Options {
            background: true,
            ..plain
        };
        assert!(!background.opens_sessions_on(true, true));
        assert!(!background.opens_sessions_on(false, true));
    }

    #[cfg(windows)]
    #[test]
    fn login_registration_is_explicit_and_not_a_run() {
        assert_eq!(
            parse(&args(&[
                "--install-login-item",
                "--hgs",
                "C:/Zerus/hgs.exe"
            ])),
            Ok(Command::LoginItem { enabled: true })
        );
        assert_eq!(
            parse(&args(&["--remove-login-item"])),
            Ok(Command::LoginItem { enabled: false })
        );
        assert!(parse(&args(&["--install-login-item", "--remove-login-item"])).is_err());
    }

    #[test]
    fn build_info_has_qt_shape() {
        let commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let dev = BuildFacts {
            version: "0.47.0",
            dev_base: "0.47.1",
            dev_number: Some("1413"),
            source_commit: Some(commit),
            source_dirty: Some("0"),
            preview_label: None,
        };
        assert_eq!(
            build_info(&dev),
            serde_json::json!({
                "version": "0.47.0", "versionName": "0.47.1-dev.1413", "devBuildNumber": 1413,
                "sourceCommit": commit, "sourceDirty": false,
            })
        );
        assert_eq!(version_line(&dev), "Zerus 0.47.1-dev.1413\n");
        let local = BuildFacts {
            version: "0.47.0",
            dev_base: "0.47.1",
            dev_number: None,
            source_commit: None,
            source_dirty: None,
            preview_label: None,
        };
        assert_eq!(
            build_info(&local),
            serde_json::json!({
                "version": "0.47.0", "versionName": "0.47.0", "devBuildNumber": 0,
                "sourceCommit": "", "sourceDirty": null,
            })
        );
        assert_eq!(version_line(&local), "Zerus 0.47.0\n");
        let dirty = BuildFacts {
            dev_number: None,
            source_dirty: Some("1"),
            ..dev
        };
        assert_eq!(build_info(&dirty)["sourceDirty"], true);
        // A local branch preview (`UpdateController.cpp:130-134`): the label and its own version name.
        let preview = BuildFacts {
            dev_number: None,
            preview_label: Some("task-board"),
            ..dev
        };
        assert_eq!(
            build_info(&preview)["versionName"],
            "0.47.1-preview.task-board.aaaaaaa"
        );
        assert_eq!(build_info(&preview)["previewLabel"], "task-board");
        assert_eq!(build_info(&preview)["devBuildNumber"], 0);
        #[cfg(target_os = "linux")]
        {
            let identified = build_info_for(&dev, "com.hgdev.hgs-tray");
            assert_eq!(identified["CFBundleIdentifier"], "com.hgdev.hgs-tray");
            assert_eq!(identified["ZerusGUI"], "tauri");
            assert_eq!(identified["localWebKitChildEnvironment"], 1);
        }
    }

    #[test]
    fn the_embedded_version_is_the_repository_version() {
        assert_eq!(BUILD.version, include_str!("../../../VERSION").trim());
        assert_eq!(BUILD.dev_base, include_str!("../../../DEV_VERSION").trim());
        assert_eq!(given(Some("")), None, "build.rs sets an absent value empty");
        assert_eq!(given(Some("1")), Some("1"));
    }

    #[test]
    fn build_identity_follows_the_cmake_rules() {
        use build_identity::*;
        assert_eq!(release_version(" 0.47.0\n").as_deref(), Some("0.47.0"));
        assert_eq!(release_version("0.47"), None);
        assert_eq!(release_version("0.047.0"), None);
        assert!(check_dev_base("0.47.0", "0.47.1").is_ok());
        assert!(check_dev_base("0.47.0", "0.47.0").is_err());
        assert!(check_dev_base("0.47.0", "0.9.9").is_err());
        assert_eq!(dev_number(""), Ok(None));
        assert_eq!(dev_number("1413"), Ok(Some(1413)));
        for bad in [
            "0",
            "012",
            "-1",
            "1e3",
            "1234567890123456",
            " 1413",
            "1413\n",
        ] {
            assert!(dev_number(bad).is_err(), "{bad}");
        }
        let commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert_eq!(
            source_commit(commit, "/src/example", "/src/example").as_deref(),
            Some(commit)
        );
        assert_eq!(
            source_commit(commit, "/src/parent", "/src/example"),
            None,
            "a parent repository's commit is not ours"
        );
        assert_eq!(source_commit("ABC", "/src/example", "/src/example"), None);
        assert!(check_numbered(Some(1), Some(commit), Some(false)).is_ok());
        assert!(check_numbered(Some(1), Some(commit), Some(true)).is_err());
        assert!(check_numbered(Some(1), None, None).is_err());
        assert!(check_numbered(None, None, None).is_ok());
        // `ZERUS_LOCAL_PREVIEW_LABEL` (`BuildVersion.cmake:9-18`).
        assert_eq!(preview_label(""), Ok(None));
        assert_eq!(
            preview_label("task-board"),
            Ok(Some("task-board".to_owned()))
        );
        for bad in [
            "Task",
            "task--board",
            "-task",
            "task-",
            "task_board",
            &"a".repeat(33),
        ] {
            assert!(preview_label(bad).is_err(), "{bad}");
        }
        assert!(check_preview(Some("task-board"), None, Some(commit), Some(false)).is_ok());
        assert!(
            check_preview(Some("task-board"), Some(7), Some(commit), Some(false)).is_err(),
            "never with a Dev number"
        );
        assert!(
            check_preview(Some("task-board"), None, Some(commit), Some(true)).is_err(),
            "never from local changes"
        );
        assert!(check_preview(None, None, None, None).is_ok());
        // A packed branch ref has no loose file: its folder is watched.
        let temp = std::env::temp_dir();
        assert_eq!(watched(&temp), temp.as_path());
        assert_eq!(
            watched(&temp.join("zci-absent/refs/heads/main")),
            temp.as_path()
        );
    }
}
