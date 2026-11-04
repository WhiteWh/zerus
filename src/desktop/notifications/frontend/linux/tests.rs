use super::super::{Origin, SelectedFrontend};
use super::*;
use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
    },
    sync::Mutex,
};
use wire::{Open, Target};

static SERIAL: Mutex<()> = Mutex::new(());
struct Fixture {
    _root: tempfile::TempDir,
    prefix: std::path::PathBuf,
}
impl Fixture {
    fn new(help: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path().canonicalize().unwrap().join("private install");
        for dir in ["bin", "libexec", "share/hgs"] {
            fs::create_dir_all(prefix.join(dir)).unwrap();
            fs::set_permissions(prefix.join(dir), fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::set_permissions(&prefix, fs::Permissions::from_mode(0o755)).unwrap();
        fs::copy("/bin/dd", prefix.join("bin/hgs-tray")).unwrap();
        let release = CString::new(prefix.join("fixture-exit").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(release.as_ptr(), 0o600) }, 0);
        fs::write(prefix.join("bin/hgs"), b"private CLI image fixture").unwrap();
        fs::set_permissions(prefix.join("bin/hgs"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(prefix.join("share/hgs/local-webkit.json"), b"{}").unwrap();
        // No token/target output or file storage. Shell assertions qualify the
        // received argv/environment before execing the SAME owned child's ELF.
        let target = wire::encode_navigation(&open().target).unwrap();
        fs::write(prefix.join("libexec/hgs-tray-launch"), format!(
            "#!/bin/sh\nif [ \"$1\" = --help ]; then printf '%s\\n' '{help}'; exit 0; fi\n\
            [ \"$#\" = 3 ] && [ \"$1\" = --hgs ] && [ \"$2\" = '{}/bin/hgs' ] && \\\n            [ \"$3\" = --sessions ] && [ \"${{ZERUS_CLIENT_DESKTOP_TARGET-}}\" = '{target}' ] && \\\n            [ \"${{XDG_ACTIVATION_TOKEN-}}\" = synthetic-wayland ] && \\\n            [ \"${{ZERUS_CLIENT_DESKTOP_SERVICE-}}\" = 1 ] && [ -z \"${{HGS_SESSION-}}${{HGS_RUN_ID-}}\" ] || exit 31\n\
            exec '{}/bin/hgs-tray' 'if={}/fixture-exit' bs=1 count=1 status=none\n", prefix.display(), prefix.display(), prefix.display())).unwrap();
        fs::set_permissions(
            prefix.join("libexec/hgs-tray-launch"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        Self {
            _root: root,
            prefix,
        }
    }
    /// One byte releases the fixture; no autonomous short sleep and no EOF
    /// dependency on unrelated fork-before-exec descriptor aliases.
    fn hold(&self) -> File {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(self.prefix.join("fixture-exit"))
            .unwrap();
        assert!(file.metadata().unwrap().file_type().is_fifo());
        file
    }
    fn select(&self) -> SelectedFrontend {
        SelectedFrontend::capture(
            self.prefix.join("bin/hgs"),
            self.prefix.join("bin/hgs-tray"),
            "a".repeat(64),
            Origin::Fixture,
        )
        .unwrap()
    }
}
fn open() -> Open {
    let target = Target {
        computer: "peer.example".into(),
        run: "original-run".into(),
        conversation: "original-conversation".into(),
    };
    let token = zerus_client_core::attention::encode_token(
        serde_json::json!({
            "host":target.computer, "run":target.run, "conversation":target.conversation,
            "name":"not-a-selector", "notice":"fixture"
        })
        .as_object()
        .unwrap(),
    );
    Open {
        activation_id: uuid::Uuid::new_v4().to_string(),
        post_request_id: uuid::Uuid::new_v4().to_string(),
        target,
        token,
        platform_context: Some(PlatformContext::Wayland {
            activation_token: "synthetic-wayland".into(),
        }),
    }
}
fn launch() -> Launch {
    Launch::fixture(open(), "a".repeat(64))
}

#[test]
fn preparation_owner_is_released_after_spawn_not_charged_to_the_gui_lifetime() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new(wire::NAVIGATION_ENV_HELP_MARKER);
    let mut release = fixture.hold();
    let resources = Arc::new(AtomicBool::new(false));
    let mut intent = launch();
    intent.bind_preparation(resources.clone(), Arc::new(AtomicBool::new(false)));
    let mut owned = OwnedFrontend::launch(fixture.select(), intent).unwrap();
    observe_held(&mut owned);
    assert_eq!(
        Arc::strong_count(&resources),
        1,
        "a live GUI cannot retain the Root handoff exclusion"
    );
    release.write_all(b"x").unwrap();
    finish_released(&mut owned);
}

#[test]
fn a_cancelled_original_launch_never_starts_its_help_or_frontend_child() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new(wire::NAVIGATION_ENV_HELP_MARKER);
    let stopped = Arc::new(AtomicBool::new(true));
    let mut intent = launch();
    intent.bind_preparation(Arc::new(()), stopped);
    assert!(OwnedFrontend::launch(fixture.select(), intent).is_err());
    assert!(!ACTIVE.load(Ordering::Acquire));
}

#[test]
fn backend_handoff_keeps_the_original_gui_observer_until_actual_exit_without_killing_it() {
    use crate::{
        config::Config,
        desktop::{notifications::closed, store::Location},
    };
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new(wire::NAVIGATION_ENV_HELP_MARKER);
    let mut release = fixture.hold();
    let mut frontend = OwnedFrontend::launch(fixture.select(), launch()).unwrap();
    observe_held(&mut frontend);
    let config = Config {
        home: fixture.prefix.to_str().unwrap().into(),
        dir: fixture.prefix.join("config"),
        state: fixture.prefix.join("state"),
        selves: vec![],
        peers: vec![],
        base_peers: vec![],
        peers_from_environment: false,
        machines: Default::default(),
        tab: false,
        tab_colors: String::new(),
    };
    let location = Location {
        root: config.state.join("desktop"),
        configuration: "a".repeat(64),
    };
    let mut runtime = closed::Runtime::new(&config, &location);
    runtime.observe_for_test(frontend);
    let until = Instant::now() + Duration::from_secs(5);
    while runtime.status()["observing_gui"] != true {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(2));
    }
    runtime.stop();
    assert!(runtime.drained());
    assert!(
        ACTIVE.load(Ordering::Acquire),
        "the same live GUI still owns its admission"
    );
    drop(runtime);
    release.write_all(b"x").unwrap();
    while ACTIVE.load(Ordering::Acquire) {
        assert!(
            Instant::now() < until,
            "original GUI was not reaped after explicit release"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn observe_held(owned: &mut OwnedFrontend) {
    let until = Instant::now() + Duration::from_secs(5);
    while !owned.image_observed() {
        owned.poll();
        assert!(!owned.error(), "held fixture failed image observation");
        assert!(!owned.exited(), "fixture exited before explicit release");
        assert!(Instant::now() < until, "original fixture did not exec");
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(!owned.error());
    assert!(owned.child.as_mut().unwrap().try_wait().unwrap().is_none());
}
fn release(hold: &mut File) {
    // Keep this descriptor until actual exit. Closing the last descriptor before
    // dd opens the FIFO would discard a byte written before its reader starts.
    hold.write_all(b"x").unwrap();
}
fn finish_released(owned: &mut OwnedFrontend) {
    let until = Instant::now() + Duration::from_secs(5);
    while !owned.exited() {
        owned.poll();
        assert!(Instant::now() < until, "released fixture did not exit");
        std::thread::sleep(Duration::from_millis(25));
    }
    // After release, a normal exit may race the live-child revalidation. That
    // conservative refusal stays intact; only actual original exit settles it.
    assert!(owned.exit_status().unwrap().success());
    assert!(owned.child.is_none());
}
fn finish(owned: &mut OwnedFrontend, mut hold: File) {
    observe_held(owned);
    release(&mut hold);
    finish_released(owned);
}
#[test]
fn original_wrapper_exec_exact_target_and_child_only_token_are_retained_to_exit() {
    let _serial = SERIAL.lock().unwrap();
    let f = Fixture::new(wire::NAVIGATION_ENV_HELP_MARKER);
    let hold = f.hold();
    let mut owned = OwnedFrontend::launch(f.select(), launch()).unwrap();
    assert!(
        OwnedFrontend::launch(f.select(), launch()).is_err(),
        "one original admission"
    );
    finish(&mut owned, hold);
    drop(owned);
    let hold = f.hold();
    let mut next = OwnedFrontend::launch(f.select(), launch()).unwrap();
    finish(&mut next, hold);
}
#[test]
fn startup_timeout_retains_original_child_until_actual_exit_without_a_second_launch() {
    let _serial = SERIAL.lock().unwrap();
    let f = Fixture::new(wire::NAVIGATION_ENV_HELP_MARKER);
    let mut hold = f.hold();
    let mut owned = OwnedFrontend::launch(f.select(), launch()).unwrap();
    owned.until = Instant::now();
    owned.poll();
    assert!(owned.error());
    assert!(!owned.exited());
    assert!(owned.child.is_some());
    assert!(OwnedFrontend::launch(f.select(), launch()).is_err());
    release(&mut hold);
    finish_released(&mut owned);
    drop(owned);
    assert!(!ACTIVE.load(Ordering::Acquire));
}

#[test]
fn normal_exit_after_a_live_sample_keeps_the_dead_child_identity_refusal() {
    let _serial = SERIAL.lock().unwrap();
    let f = Fixture::new(wire::NAVIGATION_ENV_HELP_MARKER);
    let mut hold = f.hold();
    let mut owned = OwnedFrontend::launch(f.select(), launch()).unwrap();
    observe_held(&mut owned);
    let child = owned.child.as_mut().unwrap();
    assert!(child.try_wait().unwrap().is_none());
    release(&mut hold);
    let until = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(owned
        .hold
        .selected
        .revalidate_child(child, owned.observed.as_ref().unwrap())
        .is_err());
    owned.poll();
    assert!(owned.exited());
    assert!(owned.exit_status().unwrap().success());
    assert!(owned.child.is_none());
}

#[test]
fn missing_or_inexact_navigation_abi_refuses_before_target_start() {
    let _serial = SERIAL.lock().unwrap();
    for help in [
        "Usage: old frontend",
        wire::NAVIGATION_HELP_MARKER,
        "prefix Desktop target environment ABI: desktop-target-env-v1",
    ] {
        let f = Fixture::new(help);
        assert!(OwnedFrontend::launch(f.select(), launch()).is_err());
        assert!(!ACTIVE.load(Ordering::Acquire));
    }
}
#[test]
fn changed_selection_or_configuration_refuses_and_no_token_enters_argv() {
    let _serial = SERIAL.lock().unwrap();
    let f = Fixture::new(wire::NAVIGATION_ENV_HELP_MARKER);
    let selected = f.select();
    let event = launch();
    let command = command(&selected, &event).unwrap();
    let args: Vec<_> = command.get_args().map(|v| v.to_str().unwrap()).collect();
    assert_eq!(args.len(), 3);
    for id in [
        &event.open().target.computer,
        &event.open().target.run,
        &event.open().target.conversation,
    ] {
        assert!(
            !args.join(" ").contains(id),
            "target identifier in process arguments"
        );
    }
    assert!(!args.join(" ").contains("synthetic-wayland"));
    assert!(!args.join(" ").contains(&event.open().post_request_id));
    let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
    assert_eq!(
        env[std::ffi::OsStr::new(wire::NAVIGATION_ENV)]
            .and_then(|v| v.to_str())
            .and_then(wire::decode_navigation),
        Some(event.open().target.clone())
    );
    assert_eq!(
        env[std::ffi::OsStr::new("XDG_ACTIVATION_TOKEN")],
        Some(std::ffi::OsStr::new("synthetic-wayland"))
    );
    assert_eq!(env[std::ffi::OsStr::new("HGS_SESSION")], None);
    let mut no_context = open();
    no_context.platform_context = None;
    let absent = Launch::fixture(no_context, "a".repeat(64));
    assert!(command_env_removed(&selected, &absent));
    let foreign = Launch::fixture(open(), "b".repeat(64));
    assert!(OwnedFrontend::launch(f.select(), foreign).is_err());
    fs::write(selected.entry(), b"changed").unwrap();
    assert!(OwnedFrontend::launch(selected, event).is_err());
}
fn command_env_removed(selected: &SelectedFrontend, event: &Launch) -> bool {
    super::command(selected, event)
        .unwrap()
        .get_envs()
        .any(|(name, value)| name == "XDG_ACTIVATION_TOKEN" && value.is_none())
}
