//! Context for component fixtures, never native caller authentication.
//! Re-execute the exact test before any fixture/cached mode is constructed;
//! changing this shared process's environment would invalidate live snapshots.
//! One failed child fences later isolated fixtures. Read the FIRST
//! "did not settle successfully" diagnostic; later fences are consequences.
use std::{
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex, Weak,
    },
    time::{Duration, Instant},
};
use zerus_client_core::run;
mod process;

const AGENT: [&str; 3] = ["HGS_SESSION", "HGS_SESSION_ID", "HGS_RUN_ID"];
const LIMIT: usize = 2;
const REENTRY: &str = "ZERUS_TEST_CONTEXT_ORIGINAL_CASE";
static SPAWNS: AtomicUsize = AtomicUsize::new(0);
struct State {
    active: usize,
    failed: Option<String>,
}
static CHILDREN: (Mutex<State>, Condvar) = (
    Mutex::new(State {
        active: 0,
        failed: None,
    }),
    Condvar::new(),
);
struct Permit;
impl Permit {
    fn acquire() -> Arc<Self> {
        let mut state = CHILDREN.0.lock().unwrap();
        loop {
            if let Some(case) = &state.failed {
                let case = case.clone();
                drop(state);
                panic!("isolated fixture {case} failed; further fixture launches are fenced");
            }
            if state.active < LIMIT {
                state.active += 1;
                return Arc::new(Self);
            }
            state = CHILDREN.1.wait(state).unwrap();
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = CHILDREN.0.lock().unwrap_or_else(|e| e.into_inner());
        state.active -= 1;
        CHILDREN.1.notify_all();
    }
}

pub(crate) fn non_agent() -> bool {
    isolate(Context::NonAgent, Duration::from_secs(90), false)
}
pub(crate) fn native() -> bool {
    isolate(Context::Native, Duration::from_secs(30), false)
}
/// Desktop component models own their configuration and terminal context.
/// Remove inherited context only in a fresh original test process, before its
/// fixture, frozen environment or process-wide mode/caller cache is created.
pub(crate) fn desktop_fixture() -> bool {
    isolate(Context::DesktopFixture, Duration::from_secs(90), false)
}
/// Static executable fixtures must be staged before ANY sibling can fork and
/// inherit their temporary writers, even when the caller's context is clean.
pub(crate) fn isolated_desktop_fixture() -> bool {
    isolate(Context::DesktopFixture, Duration::from_secs(90), true)
}
#[derive(Clone, Copy)]
enum Context {
    NonAgent,
    Native,
    DesktopFixture,
}
fn desktop_keys() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| {
            let bytes = key.as_encoded_bytes();
            bytes.starts_with(b"HGS_") || bytes.starts_with(b"TMUX")
        })
        .collect()
}
fn isolate(context: Context, timeout: Duration, always: bool) -> bool {
    let thread = std::thread::current();
    let case = thread.name().expect("named libtest fixture");
    let native = matches!(context, Context::Native);
    let keys = if matches!(context, Context::DesktopFixture) {
        desktop_keys()
    } else {
        AGENT.map(std::ffi::OsString::from).into()
    };
    let clean = keys.iter().all(|key| std::env::var_os(key).is_none())
        && (!native
            || std::env::var(zerus_client_core::desktop::service::MODE_ENV).as_deref() == Ok("0"));
    if let Some(original) = std::env::var_os(REENTRY) {
        assert_eq!(original.as_os_str(), std::ffi::OsStr::new(case));
        assert!(clean, "isolated original still has inherited context");
        return false;
    }
    if clean && !always {
        return false;
    }
    let original = case.to_owned();
    run_case(case, timeout, move |command| {
        for key in keys {
            command.env_remove(key);
        }
        // Select Native explicitly. Absence alone would depend on the
        // application's current default.
        if native {
            command.env(zerus_client_core::desktop::service::MODE_ENV, "0");
        }
        // A broken cleanup must fail in this original child, never recurse
        // into another chain of test programs with the same dirty context.
        command.env(REENTRY, original);
    });
    true
}

#[test]
fn desktop_fixtures_drop_each_inherited_context_prefix_without_changing_the_parent() {
    const STAGE: &str = "ZERUS_DESKTOP_CONTEXT_FIXTURE";
    const RAW: &str = "ZERUS_DESKTOP_CONTEXT_RAW";
    if std::env::var_os(STAGE).is_some() {
        if desktop_fixture() {
            return;
        }
        let injected = std::env::var_os(STAGE).unwrap();
        assert!(
            std::env::var_os(injected).is_none(),
            "isolated original retained its injected context"
        );
        assert!(desktop_keys().is_empty());
        assert_eq!(std::env::var(RAW).unwrap(), "unrelated-value");
        return;
    }
    let before = run::FrozenEnvironment::capture().unwrap();
    let thread = std::thread::current();
    for key in [
        "HGS_SESSION_ID",
        "HGS_LAUNCH_ID",
        "HGS_STATE_DIR",
        "HGS_DESKTOP_SERVICE",
        "TMUX",
        "TMUX_PANE",
    ] {
        run_case(
            thread.name().unwrap(),
            Duration::from_secs(90),
            move |command| {
                for inherited in desktop_keys() {
                    command.env_remove(inherited);
                }
                command
                    .env(STAGE, key)
                    .env(RAW, "unrelated-value")
                    .env(key, "synthetic-context");
            },
        );
    }
    assert!(before.current(), "fixture changed its parent's environment");
}

#[test]
fn a_marked_dirty_desktop_fixture_refuses_without_a_successor() {
    const CHILD: &str = "ZERUS_DESKTOP_CONTEXT_REFUSAL";
    if std::env::var_os(CHILD).is_some() {
        let before = SPAWNS.load(Ordering::Relaxed);
        assert!(std::panic::catch_unwind(desktop_fixture).is_err());
        assert_eq!(SPAWNS.load(Ordering::Relaxed), before);
        return;
    }
    let thread = std::thread::current();
    let case = thread.name().unwrap();
    let original = case.to_owned();
    run_case(case, Duration::from_secs(90), move |command| {
        command
            .env(CHILD, "1")
            .env(REENTRY, original)
            .env("HGS_LAUNCH_ID", "synthetic-launch");
    });
}
fn run_case(case: &str, timeout: Duration, configure: impl FnOnce(&mut Command) + Send + 'static) {
    // Acquire before PROCESS_TREE: a sampler may hold its exclusive side while
    // other isolated tests queue. Keep that lock order everywhere in this file.
    let permit = Permit::acquire();
    let _process = crate::PROCESS_TREE.read().unwrap();
    let resources = Arc::downgrade(&permit);
    let environment = run::FrozenEnvironment::capture().unwrap();
    let until = Instant::now() + timeout;
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", case, "--test-threads=1", "--nocapture"]);
    environment.apply(&mut command);
    configure(&mut command);
    SPAWNS.fetch_add(1, Ordering::Relaxed);
    // Native's outer watchdog remains 30s. The non-agent fixtures include a
    // maintained 31s effect with original 45/60s limits and use a 90s outer bound.
    let mut report = process::run(command, process::Watchdog::new(until), permit);
    let valid = report.success();
    if valid && settled(&resources, until) {
        return;
    }
    if valid && Instant::now() >= until {
        report.stopped = Some(process::Stopped::TimedOut);
    }
    // An error can return before physical pipe drain. Its Permit stays in the
    // original runner's resources; never free it or keep queued fixtures
    // waiting forever on that uncertain original.
    CHILDREN
        .0
        .lock()
        .unwrap()
        .failed
        .get_or_insert_with(|| case.to_owned());
    CHILDREN.1.notify_all();
    panic!(
        "isolated fixture {case} did not settle successfully: {}",
        report.diagnostic()
    );
}
fn settled(resources: &Weak<Permit>, until: Instant) -> bool {
    let mut state = CHILDREN.0.lock().unwrap();
    loop {
        if resources.strong_count() == 0 {
            return Instant::now() < until;
        }
        let now = Instant::now();
        if now >= until {
            return false;
        }
        state = CHILDREN.1.wait_timeout(state, until - now).unwrap().0;
    }
}

#[test]
fn a_fresh_context_preserves_service_mode_raw_values_and_the_parent_environment() {
    use std::os::unix::ffi::OsStringExt;
    const STAGE: &str = "ZERUS_TEST_CONTEXT_STAGE";
    const RAW: &str = "ZERUS_TEST_CONTEXT_RAW";
    fn cache_admission() -> bool {
        use zerus_client_core::desktop::{
            ownership::{Action, Mode},
            ui_requests::Request,
            Build, Responder,
        };
        let responder = Responder::new(
            "com.hgdev.hgs-tray".into(),
            Build {
                version: "1.0.0".into(),
                source_commit: "a".repeat(40),
                source_dirty: Some(false),
            },
            uuid::Uuid::new_v4().to_string(),
            true,
            false,
        );
        assert!(responder.configure_ownership_mode_once(Mode::Service));
        assert!(responder.configure_once("a".repeat(64)));
        responder
            .prepare_ui_request(
                uuid::Uuid::new_v4().to_string(),
                Request::DesktopOwnership {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    action: Action::Rearm,
                    journal: None,
                    revision: 0,
                },
            )
            .is_ok()
    }
    if std::env::var_os(STAGE).is_some() {
        // Cache admission is DATA only. Check the unchanged component refusal
        // before cleaning each marker, not a Root-native-authentication claim.
        let dirty = AGENT.iter().any(|key| std::env::var_os(key).is_some());
        assert_eq!(cache_admission(), !dirty);
        if non_agent() {
            return;
        }
        assert!(AGENT.iter().all(|key| std::env::var_os(key).is_none()));
        assert_eq!(
            std::env::var(zerus_client_core::desktop::service::MODE_ENV).unwrap(),
            "1"
        );
        assert_eq!(
            std::env::var_os(RAW).unwrap(),
            std::ffi::OsString::from_vec(vec![b'x', 0xff])
        );
        return;
    }
    let before = run::FrozenEnvironment::capture().unwrap();
    let before_agent = AGENT.map(std::env::var_os);
    let thread = std::thread::current();
    for source in AGENT {
        run_case(
            thread.name().unwrap(),
            Duration::from_secs(90),
            move |command| {
                for key in AGENT {
                    command.env_remove(key);
                }
                command
                    .env(source, "synthetic-agent-context")
                    .env(STAGE, "dirty")
                    .env(zerus_client_core::desktop::service::MODE_ENV, "1")
                    .env(RAW, std::ffi::OsString::from_vec(vec![b'x', 0xff]));
            },
        );
    }
    assert!(before.current(), "fixture changed its parent's environment");
    assert!(
        AGENT.map(std::env::var_os) == before_agent,
        "fixture changed its parent's agent context"
    );
}

#[test]
fn a_marked_dirty_child_refuses_before_any_successor_spawn() {
    const CHILD: &str = "ZERUS_TEST_CONTEXT_REFUSAL_CONTROL";
    if std::env::var_os(CHILD).is_some() {
        let before = SPAWNS.load(Ordering::Relaxed);
        assert!(std::panic::catch_unwind(non_agent).is_err());
        assert_eq!(SPAWNS.load(Ordering::Relaxed), before);
        return;
    }
    let thread = std::thread::current();
    let case = thread.name().unwrap();
    let original = case.to_owned();
    run_case(case, Duration::from_secs(90), move |command| {
        command
            .env(CHILD, "1")
            .env(REENTRY, original)
            .env("HGS_SESSION_ID", "synthetic-agent-context");
    });
}

#[test]
fn native_fixtures_select_explicit_mode_before_the_process_cache() {
    const CHILD: &str = "ZERUS_TEST_CONTEXT_NATIVE_CONTROL";
    if std::env::var_os(CHILD).is_some() {
        if native() {
            return;
        }
        assert_eq!(
            std::env::var(zerus_client_core::desktop::service::MODE_ENV).unwrap(),
            "0"
        );
        assert!(!crate::desktop_service::mode());
        return;
    }
    let thread = std::thread::current();
    run_case(thread.name().unwrap(), Duration::from_secs(30), |command| {
        command
            .env(CHILD, "1")
            .env(zerus_client_core::desktop::service::MODE_ENV, "1")
            .env("HGS_SESSION_ID", "synthetic-agent-context");
    });
}

#[test]
fn a_clean_desktop_fixture_can_require_its_own_original_process() {
    const STAGE: &str = "ZERUS_TEST_CONTEXT_FORCE_CLEAN";
    if std::env::var_os(STAGE).is_some() {
        assert!(desktop_keys().is_empty());
        let before = SPAWNS.load(Ordering::Relaxed);
        if isolated_desktop_fixture() {
            assert_eq!(SPAWNS.load(Ordering::Relaxed), before + 1);
        } else {
            assert!(std::env::var_os(REENTRY).is_some());
            assert_eq!(SPAWNS.load(Ordering::Relaxed), before);
        }
        return;
    }
    let before = run::FrozenEnvironment::capture().unwrap();
    let thread = std::thread::current();
    let keys = desktop_keys();
    run_case(
        thread.name().unwrap(),
        Duration::from_secs(90),
        move |command| {
            for key in keys {
                command.env_remove(key);
            }
            command.env_remove(REENTRY).env(STAGE, "clean");
        },
    );
    assert!(before.current());
}
