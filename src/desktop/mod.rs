//! Durable desktop service kernel with explicit service-owned fleet activation.
mod approval_audit;
#[allow(dead_code)] // Discovery is bound to the original notification factory at cutover.
mod attention;
mod background;
mod cache_effects;
mod cache_maintenance;
mod catalog_read;
mod catalog_relay;
mod counts;
mod creation_clone;
mod creation_worktree;
pub(crate) mod creation_launch;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod custody;
mod delivery;
mod details;
mod fanout;
mod feed;
mod fleet;
mod frontend;
mod handoff;
mod import_evidence;
mod lifecycle;
mod notification_controls;
#[allow(dead_code)] // Platform adapters attach this outbox in the next integration slice.
mod notifications;
#[allow(dead_code)]
mod operations;
// The never-renewed overall deadline of a multi-child operation.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod operation_clock;
// One bounded original-child runner for the launch and task message leaves.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod original_child;
// The CLI state store one deferred original holds and reads.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod original_store;
// The deferred-input kinds' sealed negatives, owned by the service epoch.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod not_admitted;
mod owner;
mod ownership;
mod pending_reads;
pub(crate) mod portable_frontend;
mod presence_admission;
mod presence_worker;
#[allow(dead_code)]
mod producer_binding;
#[allow(dead_code)]
mod producer_io;
#[allow(dead_code)]
mod producer_prepared;
#[allow(dead_code)]
mod producer_session;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod program_stop;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)] // The private final-cleanup adapter is the next caller.
pub(crate) mod program_cleanup;
mod protocol;
/// The owner-approved question route leaf (zerus-sa9qn.8, slice a).
mod question_route;
mod registration;
mod revoke;
#[cfg(any(windows, test))]
pub(crate) mod startup_diagnostic;
#[allow(dead_code)]
mod startup_exclusion;
mod startup_stdio;
mod state_directory;
mod store;
#[cfg(windows)]
mod swarm_windows;
/// The deferred send leaf (zerus-5wmh.66.4.4, slice 3).
pub(crate) mod compact_to_send;
pub(crate) mod deferred_answer;
pub(crate) mod deferred_send;
/// The resume-then-send leaf (zerus-5wmh.66.4.9, slice 3).
pub(crate) mod session_resume;
pub(crate) mod task_message;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod task_start_reconcile;
pub(crate) mod task_controls;
mod termination_outcome;
mod transport;
mod ui_authority;
mod upgrade;
#[cfg(windows)]
mod windows_registration;
#[cfg(any(windows, test))]
mod windows_registration_policy;
#[cfg(all(test, not(windows)))]
#[path = "windows_registration/native/mode/stop.rs"]
mod windows_mode_stop;

use crate::{cli, config::Config, platform};
use anyhow::{ensure, Context, Result};
use fanout::{Fanout, Queue};
pub(crate) use presence_admission::legacy as legacy_presence;
use protocol::{Hello, ABI};
use serde_json::{json, Value};
use std::{
    io::{BufReader, Read, Write},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};
use store::{Location, Store};
use transport::{Listener, Stream, TIMEOUT};

// Native token regression uses the actual Store/guards without exposing them
// to another production caller or adding a mock ownership verdict.
#[cfg(all(windows, test))]
pub(crate) mod creation_test {
    pub(crate) use super::store::{Location, Store};
    pub(crate) fn database_files(location: &Location) -> anyhow::Result<()> {
        super::store::database_files(location)
    }
}

const USAGE: &str = "usage: hgs desktop serve [--config-dir DIR --state-dir DIR] [--background] | status [--json] | startup --json | owner-status --json | ownership-status --json | ownership-result --json | swarm-restart --json (Windows) | watch --json | feed --json | request --json | catalog-relay --json | registration --json [--background] | registration install|remove|status --json (Windows) | registration install --mode background|passive --json [--after-review UUID --request-id NEW_UUID] (Windows) | registration install --mode background --json --request-id NEW_UUID (Windows first choice) | registration cancel-rollback --json --request-id UUID (Windows) | registration review-mode --json --request-id UUID (Windows) | registration mode-result --json --request-id UUID (Windows) | notification-registration install|status --json | notification-registration remove --json --request-id UUID (Windows) | revoke --json [--request-id UUID] [--reason rollback] | import-evidence --json | import-status --json --import-id UUID";
const HEARTBEAT: Duration = Duration::from_secs(10);
pub const STARTUP_REFUSED: i32 = 78;

#[derive(Debug)]
struct StartupRefused;
impl std::fmt::Display for StartupRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("desktop startup refused")
    }
}
impl std::error::Error for StartupRefused {}

#[cfg(windows)]
pub(crate) fn windows_notification_activation(first: Option<&str>) -> Option<i32> {
    (first == Some(notifications::windows::ACTIVATION_ENTRY))
        .then(notifications::windows::cold_entry)
}

#[cfg(windows)]
pub(crate) fn windows_registration_entry(args: &[String]) -> Option<i32> {
    windows_registration::entry(args)
}

pub fn registered_command(args: &[String]) -> bool {
    let args = if args.len() == 7 && args[6] == "--background" {
        &args[..6]
    } else {
        args
    };
    matches!(args, [desktop, serve, config_flag, _, state_flag, _]
        if desktop == "desktop" && serve == "serve"
        && config_flag == "--config-dir" && state_flag == "--state-dir")
}

/// launchd's SuccessfulExit=false retries every nonzero status. Only its exact
/// registered invocation maps permanent startup refusals to a clean exit; the
/// diagnostic remains on stderr. Ordinary CLI errors retain a failure status.
pub fn launchd_command(args: &[String]) -> bool {
    args.last().is_some_and(|arg| arg == "--launchd") && registered_command(&args[..args.len() - 1])
}

/// Registrations pass the selected directories as argv, including on Windows
/// where a logon task cannot carry a custom environment block. Only this exact
/// local service form changes configuration before Config::load; ordinary hgs
/// routing and all existing HGS_* selection remain unchanged.
pub fn launch_configuration(args: &[String]) -> cli::Result<()> {
    let args = if args.len() == 7 && args[6] == "--background" {
        &args[..6]
    } else {
        args
    };
    if let [desktop, serve, config_flag, config, state_flag, state] = args {
        if desktop == "desktop"
            && serve == "serve"
            && config_flag == "--config-dir"
            && state_flag == "--state-dir"
        {
            if !std::path::Path::new(config).is_absolute()
                || !std::path::Path::new(state).is_absolute()
            {
                return Err(cli::Error::new(
                    STARTUP_REFUSED,
                    "desktop registration directories must be absolute",
                ));
            }
            std::env::set_var("HGS_CONFIG_DIR", config);
            std::env::set_var("HGS_STATE_DIR", state);
        }
    }
    Ok(())
}

pub fn dispatch(config: &Config, target: &str, args: &[String], dry: bool) -> cli::Result<i32> {
    #[cfg(test)]
    use swarm_cli_test::Location;
    let run = || -> Result<()> {
        ensure!(
            !dry && (target.is_empty() || config.is_self(target)),
            "desktop manages only this computer; remote desktop IPC is refused"
        );
        #[cfg(any(windows, test))]
        if args == ["swarm-restart", "--json"] {
            println!("{}", restart_swarm(config)?);
            return Ok(());
        }
        let location = Location::new(config).map_err(|error| {
            if args.first().is_some_and(|arg| arg == "serve") {
                error.context(StartupRefused)
            } else {
                error
            }
        })?;
        match args
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["serve"] => serve(config, location, false),
            ["serve", "--background"] => serve(config, location, true),
            ["serve", "--config-dir", dir, "--state-dir", state]
            | ["serve", "--config-dir", dir, "--state-dir", state, "--background"] => {
                if std::path::Path::new(dir) != config.dir
                    || std::path::Path::new(state) != config.state
                {
                    return Err(anyhow::anyhow!("desktop registration configuration was not selected; invoke its exact local command").context(StartupRefused));
                }
                serve(
                    config,
                    location,
                    args.last().is_some_and(|arg| arg == "--background"),
                )
            }
            ["registration", "--json"] => {
                println!("{}", registration::preview(config, &location)?);
                Ok(())
            }
            ["revoke-hold", "--json", "--request-id", id] => {
                #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                {
                    revoke::hold::cli(config, &location, id)
                }
                #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
                {
                    let _ = id;
                    anyhow::bail!(
                        "desktop revoke hold is supported on Linux, macOS and Windows only"
                    )
                }
            }
            ["registration", "--json", "--background"] => {
                ensure!(
                    cfg!(any(target_os = "linux", target_os = "macos", windows)),
                    "background registration is supported on Linux, macOS and Windows"
                );
                println!("{}", registration::preview_mode(config, &location, true)?);
                Ok(())
            }
            ["registration", "mode-check", "--json"] => {
                #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                {
                    let scope = ui_authority::Scope::new(config, &location);
                    #[cfg(not(windows))]
                    let reply = {
                        let (source, consumer) = ui_authority::registration::observation(&scope)?;
                        json!({"schema":1,"configuration":location.configuration,"admitted":true,
                            "source":source,"consumer":consumer})
                    };
                    #[cfg(windows)]
                    let reply = {
                        let source = ui_authority::registration::caller(&scope)?;
                        json!({"schema":1,"configuration":location.configuration,"admitted":true,"source":source})
                    };
                    println!("{reply}");
                    Ok(())
                }
                #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
                anyhow::bail!(
                    "desktop registration mode checks are supported on Linux, macOS and Windows only"
                )
            }
            ["registration", operation @ ("install" | "remove" | "status"), "--json"] => {
                #[cfg(windows)]
                {
                    println!(
                        "{}",
                        windows_registration::command(config, &location, operation)?
                    );
                    Ok(())
                }
                #[cfg(not(windows))]
                {
                    let _ = operation;
                    anyhow::bail!(
                        "desktop registration install/remove/status is available only on Windows; use the maintained platform installer"
                    )
                }
            }
            ["registration", "refresh-info", "--json"] => {
                #[cfg(windows)]
                { println!("{}", windows_registration::refresh_info(config,&location)?); Ok(()) }
                #[cfg(not(windows))]
                { anyhow::bail!("Original-instance refresh is available only on Windows.") }
            }
            ["registration", "refresh", "--json", "--request-id", id, "--scope", target] => {
                #[cfg(windows)]
                { println!("{}", windows_registration::refresh_command(config,&location,id,target)?); Ok(()) }
                #[cfg(not(windows))]
                { let _=(id,target); anyhow::bail!("Original-instance refresh is available only on Windows.") }
            }
            ["registration", op @ ("refresh-result" | "cancel-refresh"), "--json", "--request-id", id] => {
                #[cfg(windows)]
                { println!("{}", if *op == "refresh-result" { windows_registration::refresh_result(config,&location,id)? }
                    else { windows_registration::cancel_refresh(config,&location,id)? }); Ok(()) }
                #[cfg(not(windows))]
                { let _=(op,id); anyhow::bail!("Original-instance refresh is available only on Windows.") }
            }
            ["registration", "install", "--mode", mode, "--json"] => {
                #[cfg(windows)]
                {
                    println!(
                        "{}",
                        windows_registration::mode_command(config, &location, mode)?
                    );
                    Ok(())
                }
                #[cfg(not(windows))]
                {
                    let _ = mode;
                    anyhow::bail!("Use the maintained platform installer to select desktop mode.")
                }
            }
            ["registration", "cancel-rollback", "--json", "--request-id", id] => {
                #[cfg(windows)]
                {
                    println!(
                        "{}",
                        windows_registration::cancel_rollback(config, &location, id)?
                    );
                    Ok(())
                }
                #[cfg(not(windows))]
                {
                    let _ = id;
                    anyhow::bail!(
                        "Use the maintained platform installer to cancel rollback metadata."
                    )
                }
            }
            ["registration", "review-mode", "--json", "--request-id", id] => {
                #[cfg(windows)]
                { println!("{}", windows_registration::review_mode(config, &location, id)?); Ok(()) }
                #[cfg(not(windows))]
                { let _ = id; anyhow::bail!("Use the maintained Unix helper to review its original activation.") }
            }
            ["registration", "mode-result", "--json", "--request-id", id] => {
                #[cfg(windows)]
                { println!("{}", windows_registration::mode_result(config, &location, id)?); Ok(()) }
                #[cfg(not(windows))]
                { let _ = id; anyhow::bail!("Use the maintained Unix helper to read its original activation.") }
            }
            ["registration", "install", "--mode", "background", "--json", "--request-id", request] => {
                #[cfg(windows)]
                { println!("{}", windows_registration::first_choice(config, &location, request)?); Ok(()) }
                #[cfg(not(windows))]
                { let _ = request; anyhow::bail!("Use the maintained platform installer to select desktop mode.") }
            }
            ["registration", "install", "--mode", mode, "--json", "--after-review", reviewed, "--request-id", request] => {
                #[cfg(windows)]
                { println!("{}", windows_registration::mode_after_review(config, &location, mode, reviewed, request)?); Ok(()) }
                #[cfg(not(windows))]
                { let _ = (mode, reviewed, request); anyhow::bail!("Use the maintained Unix helper for an explicit mode choice.") }
            }
            ["notification-registration", rest @ ..] => {
                let (operation, request_id) = match rest {
                    [operation @ ("install" | "status"), "--json"] => (*operation, None),
                    ["remove", "--json", "--request-id", id] => ("remove", Some(*id)),
                    _ => anyhow::bail!(USAGE),
                };
                #[cfg(windows)]
                {
                    println!(
                        "{}",
                        notifications::windows::registration::command(
                            config, operation, request_id
                        )?
                    );
                    Ok(())
                }
                #[cfg(not(windows))]
                {
                    let _ = (operation, request_id);
                    anyhow::bail!("Windows Toast registration is available only on Windows")
                }
            }
            ["import-evidence", "--json"] => {
                println!("{}", import_evidence::cli(&location)?);
                Ok(())
            }
            ["import-status", "--json", "--import-id", id] => {
                println!("{}", import_evidence::status(&location, id)?);
                Ok(())
            }
            ["startup", "--json"] => startup_stdio::run(&location),
            ["prepare-state", "--json"] => {
                state_directory::prepare(config, &location)?;
                println!("{}", json!({"prepared":true}));
                Ok(())
            }
            ["owner-status", "--json"] => {
                println!("{}", owner::status(&location));
                Ok(())
            }
            ["ownership-status", "--json"] => {
                let value = match exchange(&location, "ownership_status", None)? {
                    Some(value) => value,
                    None => ownership::offline_read(&location, None)?,
                };
                println!("{value}");
                Ok(())
            }
            ["ownership-result", "--json"] => {
                let mut bytes = Vec::new();
                std::io::stdin()
                    .take(protocol::CONTROL_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                ensure!(
                    bytes.len() <= protocol::CONTROL_BYTES,
                    "ownership query limit"
                );
                let query: zerus_client_core::desktop::ownership::Query =
                    serde_json::from_slice(&bytes)?;
                ensure!(query.valid(), "invalid ownership query");
                let value =
                    match exchange(&location, "request", Some(serde_json::to_value(&query)?))? {
                        Some(value) => value,
                        None => ownership::offline_read(&location, Some(query))?,
                    };
                println!("{value}");
                Ok(())
            }
            ["status", "--json"] => {
                println!("{}", status(&location)?);
                Ok(())
            }
            ["status"] => {
                let status = status(&location)?;
                println!(
                    "Desktop service: {}",
                    if status["ownership"] == "background_service" {
                        "running (background fleet and presence)"
                    } else if status["ownership"] == "background_readonly" {
                        "running (background fleet, effect writers inactive)"
                    } else if status["ownership"] == "revoked" {
                        "running (background workers revoked)"
                    } else if status["running"] == true {
                        "running (passive, awaiting handoff)"
                    } else {
                        "not running"
                    }
                );
                let custody = &status["operation_queue"]["resource_custody"];
                if let Some(remediation) = custody["remediation"].as_str() {
                    println!(
                        "Operation attempt {}: helper {} is {}; revoke request {}.\n{}",
                        custody["attempt_id"].as_str().unwrap_or("unavailable"),
                        custody["helper_pid"],
                        custody["helper_state"].as_str().unwrap_or("unavailable"),
                        custody["revoke_request_id"]
                            .as_str()
                            .unwrap_or("unavailable"),
                        remediation
                    );
                }
                Ok(())
            }
            ["watch", "--json"] => watch(&location),
            ["feed", "--json"] => feed::proxy(&location),
            ["revoke", rest @ ..] => {
                let command = revoke::cli(rest)?;
                let result = match exchange(&location, "request", Some(revoke::payload(&command)?))
                    .with_context(|| {
                        format!(
                            "revoke request {} has no settled receipt; keep Qt stopped",
                            command.id()
                        )
                    })? {
                    Some(result) => result,
                    None => revoke::offline(&location, &command).with_context(|| {
                        format!(
                            "revoke request {} was not settled; keep Qt stopped",
                            command.id()
                        )
                    })?,
                };
                println!("{result}");
                let settled = result["accepted"] == true
                    && result["durable"] == true
                    && result["revoked"] == true
                    && result["checkpointed"] == true;
                let absent = result["applicable"] == false
                    && result["handoff"] == "none"
                    && result["revoked"] == true
                    && result["nothing_written"] == true;
                ensure!(
                    (settled || absent)
                        && result["request_id"] == command.id()
                        && result["configuration"] == location.configuration,
                    "desktop revocation is not settled; keep Qt stopped and inspect the same request_id"
                );
                Ok(())
            }
            ["catalog-relay", "--json"] => catalog_relay::run(&location),
            ["request", "--json"] | ["frontend-request", "--json"] => {
                let mut bytes = Vec::new();
                std::io::stdin()
                    .take(protocol::CONTROL_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                ensure!(
                    bytes.len() <= protocol::CONTROL_BYTES,
                    "desktop request exceeds its limit"
                );
                let request: Value =
                    serde_json::from_slice(&bytes).context("invalid desktop request")?;
                let value = match exchange(
                    &location,
                    if args[0] == "frontend-request" {
                        "frontend_request"
                    } else {
                        "request"
                    },
                    Some(request.clone()),
                )? {
                    Some(value) => value,
                    None if args[0] == "frontend-request" => {
                        ownership::offline_frontend(config, &location, &request)?
                    }
                    None => anyhow::bail!("desktop service is not running"),
                };
                println!("{value}");
                Ok(())
            }
            _ => anyhow::bail!(USAGE),
        }
    };
    run().map(|()| 0).map_err(|error| {
        cli::Error::new(
            if error.is::<StartupRefused>() {
                STARTUP_REFUSED
            } else {
                1
            },
            format!("{error:#}"),
        )
    })
}

fn frame(epoch: &str, seq: u64, kind: &str, value: Value) -> Value {
    json!({"abi":ABI,"epoch":epoch,"seq":seq,"t":kind,"value":value})
}
fn attention_counts(store: &Store, epoch: &str, seq: u64, feed: &feed::Broker) -> Value {
    let mut value = counts::projection(
        &store.db,
        epoch,
        |computer| feed.available(computer),
        feed.expected(),
    )
    .unwrap_or_else(|_| {
        json!({"epoch":epoch,"scope":"shared_host_state","display_only":true,
            "authority":false,"local_marks_included":false,
            "aggregate":{"complete":false,"attention":null},"error":"counts_unavailable"})
    });
    value["projection_generation"] = store.generation.into();
    value["revision"] = seq.into();
    value
}
fn projection(
    location: &Location,
    store: &Store,
    epoch: &str,
    subscribers: usize,
    migration: &Value,
    feed: &feed::Broker,
    reads: &catalog_read::Reader,
) -> Value {
    let mut value = json!({"abi":ABI,"version":env!("CARGO_PKG_VERSION"),"schema":store::SCHEMA,
        "configuration":location.configuration,"generation":store.generation,"epoch":epoch,
        "lifecycle":lifecycle::status(&location.configuration,epoch,store.generation),
        "running":true,"ownership":"passive","writers_enabled":false,"handoff":"required",
        "upstreams":[],"complete":false,"subscribers":subscribers,"automatic_approvals":false,
        "pending":{"reads":store.read_status()["pending"],"posting":0,"approvals":0},"errors":[],"migration":migration,"effect_journal":store.effect_status(),"read_queue":store.read_status(),
        "catalog_projection":store.catalog_status(),"operation_queue":store.operation_status(),
        "passive_feed":feed.status(),"catalog_reads":reads.status(),
        "limits":{"subscribers":protocol::SUBSCRIBERS,"queue_bytes":protocol::QUEUE_BYTES,
            "frame_bytes":protocol::FRAME_BYTES,"control_bytes":protocol::CONTROL_BYTES,"ingress_bytes":feed::INGRESS_BYTES,
            "feed_producers":1,"feed_frame_bytes":feed::LINE_BYTES,"connections":protocol::CONNECTIONS},
        "components":{"fleet":"awaiting_handoff","presence":"awaiting_handoff","reads":"awaiting_handoff","attention":"awaiting_handoff","approvals":"awaiting_handoff"}});
    delivery::decorate_passive(store, &mut value);
    question_route::decorate(&store.db, &mut value);
    task_controls::decorate(&mut value);
    value["desktop_ownership"] = ownership::status(store, &location.configuration)
        .and_then(|v| Ok(serde_json::to_value(v)?))
        .unwrap_or(Value::Null);
    value
}

enum Event {
    TaskControl(Box<task_controls::Incoming>),
    ApprovalAudit(Hello, mpsc::SyncSender<approval_audit::worker::Reply>),
    ImportOwner(String, mpsc::SyncSender<import_evidence::held::Loan>),
    Command(Hello, Arc<Queue>, Option<Box<ui_authority::Authorized>>),
    #[cfg(windows)]
    SwarmRestart(Arc<Queue>, zerus_client_core::desktop::windows::Peer),
    FeedOpen(Hello, mpsc::SyncSender<feed::Opened>),
    FeedData(feed::Data),
    ListenerFailed,
}
struct Permit(Arc<AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn serve(config: &Config, location: Location, background_mode: bool) -> Result<()> {
    state_directory::prepare(config, &location).context(StartupRefused)?;
    // A logon-task start on Windows before 24H2 gets a console window of its own.
    #[cfg(windows)]
    lifecycle::CONSOLE_RELEASED.store(
        platform::release_private_console(),
        std::sync::atomic::Ordering::Relaxed,
    );
    let mut upgrade = upgrade::Upgrade::new();
    #[cfg(all(test, target_os = "linux"))]
    upgrade::actor_tests::configure(&mut upgrade);
    let (_signals, owner, listener, epoch, mut store, ui_scope) = (|| -> Result<_> {
        #[cfg(windows)]
        crate::host::token::private_creation::prepare()
            .context("select exact-user desktop creation owner")?;
        let signals =
            platform::ShutdownGuard::install().context("install desktop shutdown handler")?;
        // This dedicated service process has no workers yet. Make socket creation
        // private too, so a concurrent status probe never sees the pre-chmod mode.
        #[cfg(unix)]
        unsafe {
            libc::umask(0o077);
        }
        location
            .prepare()
            .context("prepare private desktop folders")?;
        let lock = Arc::new(location.lock().context("acquire desktop owner lock")?);
        let epoch = uuid::Uuid::new_v4().to_string();
        let store = Store::open_owned(&location, &epoch, &lock).context("open desktop store")?;
        // A status deadline must not start while startup still owns the actor
        // for migrations/recovery. Publish only once that work has completed.
        #[allow(unused_mut)]
        let mut ui_scope = ui_authority::Scope::new(config, &location);
        #[cfg(target_os = "linux")]
        ui_scope.initialize_reference(&lock);
        let listener = Arc::new(Listener::bind(&location).context("bind desktop listener")?);
        Ok((signals, lock, listener, epoch, store, Arc::new(ui_scope)))
    })()
    .context(StartupRefused)?;
    let stopped = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicUsize::new(0));
    // The sealed negatives of this epoch (zerus-tlb9q): every background
    // runtime this epoch starts, the first and each later one, gets them.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let fences = Arc::new(not_admitted::Fences::default());
    let mut feed = feed::Broker::new(&epoch);
    let mut reads = catalog_read::Reader::default();
    let audit = Arc::new(approval_audit::worker::Worker::new(
        config,
        &location,
        &epoch,
        stopped.clone(),
    ));
    let mut notification_controls =
        notification_controls::Runtime::new(&store, &location.configuration, &epoch)?;
    let mut frontend = frontend::Cache::default();
    let mut background: Option<background::Runtime> = None;
    let mut frontend_order = ui_authority::Order::default();
    // At most16 queued controls/data, thirteen bounded parsers and one passive
    // producer. Background mode owns the only internal source producer.
    let (send, receive) = mpsc::sync_channel(protocol::PENDING);
    #[cfg(all(test, target_os = "linux"))]
    let fault_send = upgrade::actor_tests::fault_sender(&send);
    let acceptor = {
        let ui_scope = ui_scope.clone();
        let (listener, stopped, active, ingress) = (
            listener.clone(),
            stopped.clone(),
            active.clone(),
            feed.ingress.clone(),
        );
        thread::Builder::new()
            .name("desktop-listener".into())
            .spawn(move || {
                while !stopped.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok(stream) => {
                            if active
                                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                                    (n < protocol::CONNECTIONS).then_some(n + 1)
                                })
                                .is_err()
                            {
                                transport::shutdown(&stream);
                                continue;
                            }
                            let permit = Permit(active.clone());
                            let (send, stopped, ingress) =
                                (send.clone(), stopped.clone(), ingress.clone());
                            let ui_scope = ui_scope.clone();
                            let _ = thread::Builder::new().name("desktop-client".into()).spawn(
                                move || {
                                    let _permit = permit;
                                    let _ = client(stream, send, stopped, ingress, &ui_scope);
                                },
                            );
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(50))
                        }
                        Err(_) if stopped.load(Ordering::SeqCst) => break,
                        Err(_) => {
                            let _ = send.try_send(Event::ListenerFailed);
                            break;
                        }
                    }
                }
            })?
    };
    #[cfg(all(test, target_os = "linux"))]
    upgrade::actor_tests::started(&location, &owner);
    let mut fanout = Fanout::default();
    let mut migration = handoff::inspect(&location);
    let mut seq = 0;
    let mut heartbeat = Instant::now() + HEARTBEAT;
    let mut fleet_publication = fleet::Publication::default();
    let mut collect_at = Instant::now();
    let mut failed = false;
    let mut background_blocked = false;
    // Pure memory only; no task ledger/native work is opened on startup.
    let mut task_runtime = task_controls::enabled().then(task_controls::Runtime::default);
    #[cfg(any(windows, all(test, target_os = "linux")))]
    let mut retirement_drained = false;
    let run_result = (|| -> Result<()> {
        if background_mode {
            background_blocked = !revoke::background_permitted(&location, &store)?;
        }
        if background_mode && !background_blocked {
            background = Some(
                background::Runtime::start(
                    config,
                    &epoch,
                    &mut feed,
                    owner.clone(),
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    fences.clone(),
                )
                .context(StartupRefused)?,
            );
            // Store the runtime first: a writer startup error still drains the
            // fleet through the common shutdown path below.
            background
                .as_mut()
                .unwrap()
                .start_presence(&location)
                .context(StartupRefused)?;
        }
        while platform::shutdown_signal() == 0 {
            let was_replacing = upgrade.requested();
            if upgrade.poll(Instant::now()) {
                if !was_replacing {
                    seq += 1;
                    fanout.publish(
                        protocol::encode(&frame(
                            &epoch,
                            seq,
                            "error",
                            json!({"code":"hgs_replaced","fatal":true}),
                        ))?
                        .into(),
                    );
                }
                // Only stop admission here. Original jobs/SDK callbacks keep
                // their actual owners until the existing drain really finishes.
                let drained = background.as_mut().map_or(Ok(()), |runtime| {
                    runtime.drain(&mut store, Duration::from_millis(10))
                });
                if drained.is_ok() && feed.stop(&mut store).is_ok() {
                    #[cfg(any(windows, all(test, target_os = "linux")))]
                    {
                        retirement_drained = true;
                    }
                    background = None;
                    // Audit/finalizer I/O can retain the original serve owner
                    // outside the background runtime. Never abandon those loans.
                    if upgrade.restart_ready(&owner) {
                        break;
                    }
                }
            } else {
                // Prepared takeover reconciliation only reduces authority. Keep the
                // same runtime until its actual drain succeeds; no replacement yet.
                let handoff = background.as_ref().and_then(background::Runtime::handoff);
                ownership::settle(&location, &mut store, handoff.as_deref(), |store| {
                    background.as_mut().map_or(Ok(()), |runtime| {
                        runtime.drain(store, Duration::from_millis(10))
                    })
                })?;
                drop(handoff);
                if background::replacement_permitted(
                    background_mode,
                    background
                        .as_ref()
                        .is_some_and(background::Runtime::drained),
                    &location,
                    &store,
                )? {
                    feed.stop(&mut store)?;
                    background = None;
                }
                let ownership_status = ownership::status(&store, &location.configuration)?;
                if ownership_status.state == zerus_client_core::desktop::ownership::State::Native {
                    if background.is_some() {
                        // NativeReady was written only after this original drain.
                        feed.stop(&mut store)?;
                        background = None;
                    }
                    background_blocked = true;
                } else if background_mode
                    && background.is_none()
                    && revoke::background_permitted(&location, &store)?
                {
                    background = Some(background::Runtime::start(
                        config,
                        &epoch,
                        &mut feed,
                        owner.clone(),
                        #[cfg(any(target_os = "linux", target_os = "macos"))]
                        fences.clone(),
                    )?);
                    background.as_mut().unwrap().start_presence(&location)?;
                    background_blocked = false;
                }
            }
            let mut full_batch = false;
            if let Some(runtime) = background.as_mut().filter(|_| !upgrade.requested()) {
                let previous = seq;
                runtime.notification_controls(
                    notification_controls.preferences(),
                    notification_controls.foreground_until(),
                );
                let (changes, full) = runtime.poll(&mut store, &mut feed, &mut seq);
                full_batch = full || previous != seq;
                for mut change in changes {
                    seq += 1;
                    change["attention_counts"] = attention_counts(&store, &epoch, seq, &feed);
                    fanout.publish(
                        protocol::encode(&frame(&epoch, seq, "catalog_changed", change))?.into(),
                    );
                }
                // A changed connection status or restart schedule reaches the
                // desktop within the loop's wake, not only with the heartbeat.
                if fleet_publication.due(runtime.fleet_connections_revision(), Instant::now()) {
                    seq += 1;
                    fanout.publish(
                        protocol::encode(&frame(
                            &epoch,
                            seq,
                            "fleet_connections",
                            runtime.fleet_connections(),
                        ))?
                        .into(),
                    );
                }
            }
            frontend.tick(&location);
            store.recover_operation_custody(&location);
            if let Some(runtime) = task_runtime.as_mut() {
                runtime.poll(
                    config,
                    &location,
                    &mut store,
                    !upgrade.requested() && background.as_ref().is_some_and(|r| !r.drained()),
                    &epoch,
                    seq,
                );
            }
            #[cfg(all(test, target_os = "linux"))]
            upgrade::actor_tests::replacement_error(&upgrade, &fault_send, &active);
            match receive.recv_timeout(if full_batch {
                Duration::ZERO
            } else {
                Duration::from_millis(200)
            }) {
                Ok(Event::TaskControl(incoming)) => {
                    if let Some(runtime) = task_runtime.as_mut() {
                        let custody = if !upgrade.requested() {
                            background
                                .as_ref()
                                .filter(|r| !r.drained())
                                .and_then(background::Runtime::handoff)
                                .zip(
                                    ownership::grant(&store, &location.configuration)
                                        .ok()
                                        .flatten(),
                                )
                                .map(|(handoff, grant)| task_controls::Custody {
                                    service: owner.clone(),
                                    handoff,
                                    grant,
                                })
                        } else {
                            None
                        };
                        runtime.admit(
                            config, &location, &mut store, *incoming, custody, &epoch, seq,
                        );
                    }
                }
                Ok(Event::ImportOwner(configuration, reply)) => {
                    seq += 1;
                    let owner = if !upgrade.requested() && configuration == location.configuration {
                        background
                            .as_ref()
                            .map(|runtime| runtime.publication_owner(&store))
                            .unwrap_or(Ok(None))
                    } else {
                        Ok(None)
                    };
                    // The initially empty one-shot never blocks the actor. A
                    // dropped caller/shutdown drops the original owner loan.
                    // This actor never claims the stage. A failed owner query
                    // is a definite no-job outcome for this request, not an
                    // unknown filesystem publication.
                    let _ = reply.try_send(import_evidence::held::Loan {
                        owner: owner.unwrap_or(None),
                        epoch: epoch.clone(),
                        seq,
                    });
                }
                Ok(Event::ApprovalAudit(hello, reply)) => {
                    // Only bounded validation/admission occurs here. The same
                    // original client worker owns query I/O and transport cleanup.
                    if !upgrade.requested() {
                        let _ = reply.try_send(audit.admit(hello, owner.clone(), seq));
                    } // Dropping the one-shot refuses new audit admission.
                }
                #[cfg(windows)]
                Ok(Event::SwarmRestart(queue, peer)) => {
                    let result = if peer.current() {
                        background.as_ref().map(|runtime| runtime.restart_swarm())
                            .unwrap_or_else(|| json!({"schema":1,"accepted":false,"code":"background_service_required"}))
                    } else {
                        json!({"schema":1,"accepted":false,"code":"swarm_requester_exited"})
                    };
                    let _ = queue
                        .offer(protocol::encode(&frame(&epoch, seq, "response", result))?.into());
                    queue.close();
                }
                Ok(Event::Command(hello, queue, authority)) => {
                    let mut value = projection(
                        &location,
                        &store,
                        &epoch,
                        fanout.len(),
                        &migration,
                        &feed,
                        &reads,
                    );
                    value["frontend_read_delivery"] = frontend.status();
                    value["notification_controls"] = notification_controls.projection(&store);
                    value["lifecycle"]["mode"] = if background_mode {
                        "background"
                    } else {
                        "passive"
                    }
                    .into();
                    value["attention_counts"] = attention_counts(&store, &epoch, seq, &feed);
                    if let Some(runtime) = &background {
                        runtime.decorate(&mut value);
                    } else if background_blocked {
                        value["background_start"] = json!({"admitted":false,"code":"explicit_handoff_required","durably_blocked":true});
                    }
                    upgrade.decorate(&mut value);
                    #[cfg(any(windows, all(test, target_os = "linux")))]
                    if hello.op == "status" && hello.request.is_none() {
                        upgrade.decorate_refresh(
                            &mut value,
                            &owner,
                            retirement_drained,
                            active.load(Ordering::SeqCst),
                            store.operation_undrained()?,
                        );
                    }
                    let mut streaming = false;
                    let mut activation_waiting = false;
                    let reply = if hello.abi != ABI || hello.configuration != location.configuration
                    {
                        frame(
                            &epoch,
                            seq,
                            "error",
                            json!({"code":"incompatible_configuration"}),
                        )
                    } else if upgrade.requested()
                        && !upgrade::allowed_during_retirement(&hello.op, hello.request.as_ref())
                    {
                        frame(
                            &epoch,
                            seq,
                            "error",
                            json!({"code":"hgs_replaced","fatal":true}),
                        )
                    } else if hello.op == "watch" && hello.request.is_none() {
                        if fanout.subscribe(queue.clone()) {
                            streaming = true;
                            if let Some(runtime) = &mut background {
                                if let Some(details) = runtime.details_open(&queue) {
                                    value["details"] = details;
                                }
                            }
                            frame(&epoch, seq, "hello", value)
                        } else {
                            frame(&epoch, seq, "error", json!({"code":"subscriber_limit"}))
                        }
                    } else if hello.op == "ownership_status" && hello.request.is_none() {
                        frame(
                            &epoch,
                            seq,
                            "response",
                            serde_json::to_value(ownership::status(
                                &store,
                                &location.configuration,
                            )?)?,
                        )
                    } else if hello.op == "status" && hello.request.is_none() {
                        frame(&epoch, seq, "status", value)
                    } else if hello.op == "frontend_request" {
                        let result = match (authority, &mut background) {
                            (Some(authority), _) if authority.operation() == "desktop_ownership_observe" => {
                                match authority.into_request(&mut frontend_order).and_then(|permit|
                                    ownership::observation::frontend(&location, &store, &epoch, permit)) {
                                    Ok(observation) => serde_json::to_value(observation)?,
                                    Err(_) => frontend_refused(hello.request.as_ref(), "ownership_observation_unavailable"),
                                }
                            }
                            (Some(authority), _)
                                if authority.operation() == "termination_outcome" =>
                            {
                                match authority.into_request(&mut frontend_order) {
                                    Ok(request) => {
                                        termination_outcome::frontend(&mut store, request)
                                    }
                                    Err(_) => frontend_refused(
                                        hello.request.as_ref(),
                                        "frontend_identity_expired",
                                    ),
                                }
                            }
                            (Some(authority), _)
                                if authority.operation() == "desktop_ownership" =>
                            {
                                match authority.into_request(&mut frontend_order) {
                                    Ok(permit) => {
                                        let handoff = background
                                            .as_ref()
                                            .and_then(background::Runtime::handoff);
                                        let result = ownership::frontend(
                                            &location,
                                            &mut store,
                                            &epoch,
                                            permit,
                                            handoff.as_deref(),
                                            |store| {
                                                background.as_mut().map_or(Ok(()), |runtime| {
                                                    runtime.drain(store, Duration::from_millis(750))
                                                })
                                            },
                                        );
                                        drop(handoff);
                                        match result {
                                            Ok(receipt) => serde_json::to_value(receipt)?,
                                            Err(_) => frontend_refused(
                                                hello.request.as_ref(),
                                                "ownership_not_settled",
                                            ),
                                        }
                                    }
                                    Err(_) => frontend_refused(
                                        hello.request.as_ref(),
                                        "frontend_identity_expired",
                                    ),
                                }
                            }
                            (Some(authority), _)
                                if matches!(
                                    authority.operation(),
                                    "notification_preferences" | "notification_focus"
                                ) =>
                            {
                                match authority.into_request(&mut frontend_order) {
                                    Ok(permit) => {
                                        notification_controls.frontend(&mut store, permit)
                                    }
                                    Err(_) => frontend_refused(
                                        hello.request.as_ref(),
                                        "frontend_identity_expired",
                                    ),
                                }
                            }
                            (Some(authority), _)
                                if authority.operation() == "import_pending_reads" =>
                            {
                                match authority.into_request(&mut frontend_order) {
                                    Ok(request) => pending_reads::frontend(&mut store, request),
                                    Err(_) => frontend_refused(
                                        hello.request.as_ref(),
                                        "frontend_identity_expired",
                                    ),
                                }
                            }
                            (Some(authority), Some(runtime)) => {
                                match authority.into_request(&mut frontend_order) {
                                    Ok(request) if request.operation() == "activation_wait" => {
                                        match runtime.activation_wait(request, queue.clone()) {
                                            Ok(()) => {
                                                streaming = true;
                                                activation_waiting = true;
                                                Value::Null
                                            }
                                            Err(_) => frontend_refused(
                                                hello.request.as_ref(),
                                                "activation_receiver_unavailable",
                                            ),
                                        }
                                    }
                                    // The reference form prepares off the actor and
                                    // answers later on this same connection.
                                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                                    Ok(request)
                                        if request.operation() == "task_message"
                                            && request
                                                .task_message_preview()
                                                .is_ok_and(|intent| !intent.form.is_inline()) =>
                                    {
                                        runtime.task_message_request(
                                            &mut store,
                                            request,
                                            queue.clone(),
                                            seq,
                                        );
                                        streaming = true;
                                        activation_waiting = true;
                                        Value::Null
                                    }
                                    // A reviewed input release answers once its
                                    // worker joined, on this same connection.
                                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                                    Ok(request)
                                        if request.operation() == "task_message_input_release" =>
                                    {
                                        runtime.task_message_input_release(
                                            &mut store,
                                            request,
                                            queue.clone(),
                                            seq,
                                        );
                                        streaming = true;
                                        activation_waiting = true;
                                        Value::Null
                                    }
                                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                                    Ok(request)
                                        if request.operation() == "task_start_reconcile" =>
                                    {
                                        let request_id = request.id().to_owned();
                                        match runtime.task_start_request(
                                            &mut store,
                                            request,
                                            queue.clone(),
                                            seq,
                                        ) {
                                            Ok(()) => {
                                                streaming = true;
                                                activation_waiting = true;
                                                Value::Null
                                            }
                                            Err(_) => {
                                                json!({"request_id":request_id,"accepted":false,"durable":false,"code":"task_start_reconcile_unavailable"})
                                            }
                                        }
                                    }
                                    Ok(request) => runtime.frontend_request(&mut store, request),
                                    Err(_) => frontend_refused(
                                        hello.request.as_ref(),
                                        "frontend_identity_expired",
                                    ),
                                }
                            }
                            (None, _) => frontend_refused(
                                hello.request.as_ref(),
                                "frontend_authentication_refused",
                            ),
                            (Some(authority), None) => {
                                match authority.into_request(&mut frontend_order) {
                                    // The person's question route switch
                                    // needs no background worker.
                                    Ok(request) if request.operation() == "question_routes" => {
                                        question_route::frontend(&mut store.db, request)
                                    }
                                    Ok(request) => {
                                        delivery::passive_frontend_request(&mut store, request)
                                    }
                                    Err(_) => frontend_refused(
                                        hello.request.as_ref(),
                                        "frontend_identity_expired",
                                    ),
                                }
                            }
                        };
                        frame(&epoch, seq, "response", result)
                    } else if hello.op == "request" {
                        let result = if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "termination_outcome_status")
                        {
                            termination_outcome::query(
                                &store,
                                &location.configuration,
                                hello.request.as_ref().unwrap(),
                            )
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "termination_outcome")
                        {
                            frontend_refused(
                                hello.request.as_ref(),
                                "frontend_authentication_required",
                            )
                        } else if hello.request.as_ref().is_some_and(|v| {
                            matches!(
                                v["op"].as_str(),
                                Some("cache_maintenance_result" | "cache_maintenance_policy")
                            )
                        }) {
                            cache_maintenance::query(
                                &store,
                                &location.configuration,
                                hello.request.as_ref().unwrap(),
                            )
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "cache_maintenance")
                        {
                            frontend_refused(
                                hello.request.as_ref(),
                                "frontend_authentication_required",
                            )
                        } else if hello.request.as_ref().is_some_and(|v| {
                            matches!(
                                v["op"].as_str(),
                                Some("fleet_reconnect" | "fleet_retry_delays_reload")
                            )
                        }) {
                            // Connection observation only: the existing
                            // supervisors' waits and saved timings, never
                            // activation, ownership or a restart.
                            match &mut background {
                                Some(runtime) => {
                                    runtime.fleet_request(hello.request.as_ref().unwrap())
                                }
                                None => {
                                    json!({"request_id":hello.request.as_ref().unwrap()["request_id"],"accepted":false,"code":"fleet_unavailable"})
                                }
                            }
                        } else if hello.request.as_ref().is_some_and(|v| {
                            matches!(
                                v["op"].as_str(),
                                Some("detail_follow" | "detail_unfollow" | "detail_source")
                            )
                        }) {
                            match &mut background {
                                Some(runtime) => runtime.details_request(
                                    &store,
                                    hello.request.as_ref().unwrap(),
                                    &mut seq,
                                ),
                                None => {
                                    json!({"request_id":hello.request.as_ref().unwrap()["request_id"],"accepted":false,"code":"details_unavailable"})
                                }
                            }
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "creation_clone_result")
                        {
                            creation_clone::query(&store, hello.request.as_ref().unwrap())
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "creation_worktree_receipt")
                        {
                            match &mut background {
                                Some(runtime) => runtime.creation_worktree_receipt(
                                    &store,
                                    hello.request.as_ref().unwrap(),
                                ),
                                None => {
                                    json!({"accepted":false,"code":"creation_receipt_unavailable"})
                                }
                            }
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "creation_worktree_result")
                        {
                            creation_worktree::query(&store, hello.request.as_ref().unwrap())
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "creation_launch_result")
                        {
                            creation_launch::query(&store, hello.request.as_ref().unwrap())
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "task_message_result")
                        {
                            task_message::query(&store, hello.request.as_ref().unwrap())
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "deferred_send_result")
                        {
                            deferred_send::query(&store, hello.request.as_ref().unwrap())
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "deferred_answer_result")
                        {
                            deferred_answer::query(&store, hello.request.as_ref().unwrap())
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "compact_to_send_result")
                        {
                            compact_to_send::query(&store, hello.request.as_ref().unwrap())
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "session_resume_result")
                        {
                            session_resume::query(&store, hello.request.as_ref().unwrap())
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "task_message_staging")
                        {
                            task_message::staging_route(
                                &store,
                                &location,
                                hello.request.as_ref().unwrap(),
                            )
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "task_message_inputs")
                        {
                            // Read-only: one bounded page, observed off the
                            // actor, answered on this same connection.
                            #[cfg(any(target_os = "linux", target_os = "macos"))]
                            match &mut background {
                                Some(runtime) => {
                                    runtime.task_message_inputs(
                                        &store,
                                        hello.request.as_ref().unwrap(),
                                        queue.clone(),
                                        seq,
                                    );
                                    streaming = true;
                                    activation_waiting = true;
                                    Value::Null
                                }
                                None => {
                                    json!({"read_only":true,"code":"task_message_inputs_unavailable"})
                                }
                            }
                            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                            {
                                json!({"read_only":true,"code":"task_message_inputs_unavailable"})
                            }
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "task_start_reconcile_result")
                        {
                            #[cfg(any(target_os = "linux", target_os = "macos"))]
                            {
                                task_start_reconcile::query(&store, hello.request.as_ref().unwrap())
                            }
                            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                            {
                                json!({"read_only":true,"code":"task_start_result_unavailable"})
                            }
                        } else if hello.request.as_ref().is_some_and(|v| {
                            matches!(
                                v["op"].as_str(),
                                Some(
                                    "creation_clone"
                                        | "creation_worktree"
                                        | "creation_launch"
                                        | "task_message"
                                        | "task_message_input_release"
                                        | "deferred_send"
                                        | "deferred_answer"
                                        | "compact_to_send"
                                        | "session_resume"
                                        | "task_start_reconcile"
                                )
                            )
                        }) {
                            frontend_refused(
                                hello.request.as_ref(),
                                "frontend_authentication_required",
                            )
                        } else if hello.request.as_ref().is_some_and(|v| {
                            matches!(
                                v["op"].as_str(),
                                Some("catalog_page" | "catalog_changes" | "catalog_close")
                            )
                        }) {
                            reads.request(&store, &feed, &epoch, hello.request.as_ref().unwrap())
                        } else if hello.request.as_ref().is_some_and(|v| {
                            matches!(
                                v["op"].as_str(),
                                Some(
                                    "notification_preferences_status"
                                        | "notification_preferences_result"
                                )
                            )
                        }) {
                            notification_controls.request(&store, hello.request.as_ref().unwrap())
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "notification_control")
                        {
                            frontend_refused(
                                hello.request.as_ref(),
                                "frontend_authentication_required",
                            )
                        } else if hello.request.as_ref().is_some_and(|v| {
                            matches!(
                                v["op"].as_str(),
                                Some("approval_audit_list" | "approval_audit_detail")
                            )
                        }) {
                            // Native client dispatch routes these through the
                            // original bounded I/O worker before this generic lane.
                            audit.unavailable(hello.request.as_ref().unwrap())
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "desktop_ownership_result")
                        {
                            ownership::result_request(
                                &store,
                                &location.configuration,
                                hello.request.as_ref().unwrap(),
                            )
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "desktop_ownership")
                        {
                            frontend_refused(
                                hello.request.as_ref(),
                                "frontend_authentication_required",
                            )
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "pending_reads_import_result")
                        {
                            pending_reads::result_request(&store, hello.request.as_ref().unwrap())
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "import_pending_reads")
                        {
                            json!({"request_id":hello.request.as_ref().unwrap()["request_id"],
                                "accepted":false,"durable":false,"code":"frontend_authentication_required"})
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "maintenance_result")
                        {
                            store::maintenance::result_request(
                                &store,
                                hello.request.as_ref().unwrap(),
                            )
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "delivery_result")
                        {
                            delivery::result_request(&store, hello.request.as_ref().unwrap())
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "question_routes_result")
                        {
                            question_route::result_request(
                                &store.db,
                                hello.request.as_ref().unwrap(),
                            )
                        } else if hello
                            .request
                            .as_ref()
                            .is_some_and(|v| v["op"] == "question_route_activity")
                        {
                            question_route::activity_request(
                                &store.db,
                                hello.request.as_ref().unwrap(),
                            )
                        } else if let Some(runtime) = &mut background {
                            let request_value = hello.request.as_ref().unwrap_or(&Value::Null);
                            if matches!(
                                request_value["op"].as_str(),
                                Some("revoke" | "revoke_status")
                            ) {
                                let handoff = runtime.handoff();
                                let captured_writer = runtime.captured_writer_epoch();
                                let result = revoke::request_with_runtime(
                                    &location,
                                    &mut store,
                                    request_value,
                                    &epoch,
                                    captured_writer.as_deref(),
                                    handoff.as_deref(),
                                    |store| runtime.drain(store, Duration::from_millis(750)),
                                );
                                // Match the original writer incarnation first;
                                // a rearmed runtime must not lend its drain state
                                // to an earlier incarnation's settled receipt.
                                if request_value["op"] == "revoke_status"
                                    && result["revoked"] == true
                                    && !runtime.drained()
                                {
                                    json!({"request_id":request_value["request_id"],"accepted":false,"durable":false,
                                        "revoked":false,"checkpointed":false,"code":"drain_not_settled"})
                                } else {
                                    result
                                }
                            } else if matches!(request_value["op"].as_str(), Some("read_intent")) {
                                runtime.delivery_request(&mut store, request_value)
                            } else {
                                request(&location, &mut store, hello.request.as_ref())
                            }
                        } else {
                            request(&location, &mut store, hello.request.as_ref())
                        };
                        let held = background.as_ref().and_then(background::Runtime::handoff);
                        migration = handoff::inspect_held(&location, held.as_deref());
                        frame(&epoch, seq, "response", result)
                    } else {
                        frame(
                            &epoch,
                            seq,
                            "error",
                            json!({"code":"unsupported_operation"}),
                        )
                    };
                    if !activation_waiting {
                        queue.offer(protocol::encode(&reply)?.into());
                    }
                    if !streaming {
                        queue.close();
                    }
                }
                Ok(Event::FeedOpen(hello, reply)) => {
                    let producer = if hello.abi == ABI
                        && hello.configuration == location.configuration
                        && hello.op == "feed"
                        && !upgrade.requested()
                        && background.is_none()
                    {
                        feed.open(hello.request.as_ref()).ok()
                    } else {
                        None
                    };
                    let value = json!({"accepted":producer.is_some(),"durable":false,"feed_abi":feed::ABI,"display_only":true,
                    "incarnation":producer.as_ref().map(|p|p.incarnation),"code":if producer.is_some(){"display_only"}else{"feed_unavailable"}});
                    let _ = reply.send(feed::Opened {
                        producer,
                        reply: frame(&epoch, seq, "feed", value),
                    });
                }
                Ok(Event::FeedData(data)) => {
                    let outcome = feed.apply(&mut store, &data.producer, &data.packet);
                    let accepted = outcome.is_ok();
                    if let Ok(Some(change)) = outcome {
                        seq += 1;
                        let mut change = change;
                        change["attention_counts"] = attention_counts(&store, &epoch, seq, &feed);
                        fanout.publish(
                            protocol::encode(&frame(&epoch, seq, "catalog_changed", change))?
                                .into(),
                        );
                    }
                    if !accepted {
                        feed.failed(&data.producer);
                    }
                    let value = json!({"accepted":accepted,"durable":false,"feed_abi":feed::ABI,"display_only":true,
                    "incarnation":data.producer.incarnation,"code":if accepted{"observed"}else{"feed_item_refused"}});
                    let _ = data.reply.send(frame(&epoch, seq, "feed_receipt", value));
                }
                Ok(Event::ListenerFailed) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    failed = true;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
            }
            for mut change in feed.retire(&mut store).unwrap_or_default() {
                seq += 1;
                change["attention_counts"] = attention_counts(&store, &epoch, seq, &feed);
                fanout.publish(
                    protocol::encode(&frame(&epoch, seq, "catalog_changed", change))?.into(),
                );
            }
            if Instant::now() >= collect_at {
                let delay = if store.collect_catalog() {
                    Duration::from_millis(250)
                } else {
                    HEARTBEAT
                };
                collect_at = Instant::now() + delay;
            }
            if Instant::now() >= heartbeat {
                store.recover_effects(&epoch);
                seq += 1;
                let held = background.as_ref().and_then(background::Runtime::handoff);
                migration = handoff::inspect_held(&location, held.as_deref());
                let mut value = projection(
                    &location,
                    &store,
                    &epoch,
                    fanout.len(),
                    &migration,
                    &feed,
                    &reads,
                );
                value["frontend_read_delivery"] = frontend.status();
                value["notification_controls"] = notification_controls.projection(&store);
                value["lifecycle"]["mode"] = if background_mode {
                    "background"
                } else {
                    "passive"
                }
                .into();
                value["attention_counts"] = attention_counts(&store, &epoch, seq, &feed);
                if let Some(runtime) = &background {
                    runtime.decorate(&mut value);
                } else if background_blocked {
                    value["background_start"] = json!({"admitted":false,"code":"explicit_handoff_required","durably_blocked":true});
                }
                upgrade.decorate(&mut value);
                fanout.publish(protocol::encode(&frame(&epoch, seq, "heartbeat", value))?.into());
                heartbeat = Instant::now() + HEARTBEAT;
            }
        }
        ensure!(!failed, "desktop listener failed");
        Ok(())
    })();
    let replaced = upgrade.requested() && platform::shutdown_signal() == 0;
    stopped.store(true, Ordering::SeqCst);
    listener.close();
    let _ = acceptor.join();
    drop(fanout);
    for event in receive.try_iter() {
        match event {
            Event::Command(_, queue, _) => queue.close(),
            #[cfg(windows)]
            Event::SwarmRestart(queue, _) => queue.close(),
            _ => {}
        }
    }
    let deadline = Instant::now() + TIMEOUT + Duration::from_secs(1);
    while upgrade::connections_pending(
        active.load(Ordering::SeqCst),
        replaced,
        Instant::now() < deadline,
        platform::shutdown_signal() != 0,
    ) {
        #[cfg(all(test, target_os = "linux"))]
        upgrade::actor_tests::connection_wait(replaced, &active);
        thread::sleep(Duration::from_millis(10));
    }
    if let Some(runtime) = task_runtime.as_mut() {
        runtime.drain();
    }
    let drained = if replaced {
        // Even an unrelated listener/store error during replacement must not
        // turn the normal four-second shutdown wait into abandonment. An
        // explicit shutdown signal retains the existing termination behavior.
        loop {
            let result = background.as_mut().map_or(Ok(()), |runtime| {
                runtime.drain(&mut store, Duration::from_millis(100))
            });
            if result.is_ok() || platform::shutdown_signal() != 0 {
                break result;
            }
            thread::sleep(Duration::from_millis(10));
        }
    } else {
        background.as_mut().map_or(Ok(()), |runtime| {
            runtime.drain(&mut store, Duration::from_secs(4))
        })
    };
    let feed_stopped = feed.stop(&mut store);
    if replaced && drained.is_ok() {
        // A drained runtime can now drop its own serve-owner copies. Retain
        // the original kernel owner through any independent finalizer/audit
        // loan even if the actor ended on an unrelated listener/store error.
        drop(background.take());
        #[cfg(all(test, target_os = "linux"))]
        upgrade::actor_tests::owner_wait(&owner);
        while !upgrade::owner_loans_released(&owner) && platform::shutdown_signal() == 0 {
            thread::sleep(Duration::from_millis(10));
        }
    }
    let checkpointed = store.checkpoint();
    if let Err(error) = run_result {
        if drained.is_err() {
            eprintln!("desktop fleet cleanup is incomplete");
        }
        if feed_stopped.is_err() {
            eprintln!("desktop feed cleanup is incomplete");
        }
        if checkpointed.is_err() {
            eprintln!("desktop shutdown checkpoint is incomplete");
        }
        return Err(error);
    }
    drained?;
    feed_stopped?;
    checkpointed?;
    ensure!(
        active.load(Ordering::SeqCst) == 0,
        "desktop connections did not drain"
    );
    ensure!(
        !replaced,
        "hgs_replaced: original desktop resources drained; supervisor restart required"
    );
    Ok(())
}

fn client(
    mut stream: Stream,
    send: mpsc::SyncSender<Event>,
    stopped: Arc<AtomicBool>,
    ingress: Arc<feed::Ingress>,
    location: &ui_authority::Scope,
) -> Result<()> {
    let task_deadline =
        task_controls::enabled().then(|| Instant::now() + task_controls::server_total());
    // Unix peer credentials require no input. Windows pipe impersonation can
    // require a completed read, so that authentication remains after the hello.
    #[cfg(unix)]
    transport::authenticate(&stream)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let bytes = if let Some(until) = task_deadline {
        task_controls::read_until(&mut reader, protocol::CONTROL_BYTES, until)?
    } else {
        protocol::line(&mut reader, protocol::CONTROL_BYTES)?
    }
    .context("missing desktop hello")?;
    #[cfg(windows)]
    transport::authenticate(&stream)?;
    let hello: Hello = serde_json::from_slice(&bytes).context("invalid desktop hello")?;
    if task_controls::enabled() && hello.op == task_controls::OP {
        ensure!(!stopped.load(Ordering::SeqCst), "desktop is stopping");
        drop(reader);
        return task_controls::client(stream, &bytes, send, location, task_deadline.unwrap());
    }
    if hello.op == import_evidence::held::OP {
        // The finalizer's Session owns the only original connection handle;
        // its stream cleanup precedes release of the runtime Owner loan.
        drop(reader);
        return import_evidence::held::client(stream, hello, send, stopped, location);
    }
    ensure!(!stopped.load(Ordering::SeqCst), "desktop is stopping");
    if approval_audit::worker::recognizes(&hello) {
        drop(reader);
        return approval_audit::worker::client(stream, hello, send);
    }
    if hello.op == "feed" {
        return feed::client(stream, reader, hello, send, stopped, ingress);
    }
    let watching = hello.op == "watch"
        || (hello.op == "frontend_request"
            && hello
                .request
                .as_ref()
                .is_some_and(|v| v["request"]["op"] == "activation_wait"));
    let authority = if hello.op == "frontend_request"
        && hello.abi == ABI
        && hello.configuration == location.configuration
    {
        transport::peer_pid(&stream)
            .ok()
            .and_then(|pid| ui_authority::authenticate(location, pid, hello.request.as_ref()?).ok())
    } else {
        None
    };
    let queue = Arc::new(Queue::default());
    let event = Event::Command(hello, queue.clone(), authority.map(Box::new));
    #[cfg(windows)]
    let event = match event {
        Event::Command(hello, _, _) if hello.op == swarm_windows::OP => {
            use zerus_client_core::desktop::windows::{verify_own_client_until, Server};
            ensure!(
                hello.abi == ABI
                    && hello.configuration == location.configuration
                    && hello.request.is_none(),
                "invalid swarm restart scope"
            );
            let pipe = stream.verification_handle()?;
            let peer = match verify_own_client_until(&pipe, Instant::now() + TIMEOUT) {
                Server::Verified(peer) => peer,
                _ => anyhow::bail!("swarm requester could not be verified"),
            };
            Event::SwarmRestart(queue.clone(), peer)
        }
        event => event,
    };
    send.try_send(event)
        .map_err(|_| anyhow::anyhow!("desktop admission is full"))?;
    // Close even if a timeout change or thread creation fails after admission.
    struct CloseQueue(Arc<Queue>);
    impl Drop for CloseQueue {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    let _close = CloseQueue(queue.clone());
    let reader = if watching {
        stream.set_read_timeout(None)?;
        let queue = queue.clone();
        Some(
            thread::Builder::new()
                .name("desktop-detach".into())
                .spawn(move || {
                    // No control operations on the display stream. EOF or any input
                    // detaches only this subscriber; commands use the request lane.
                    let _ = reader.read(&mut [0]);
                    queue.close();
                })?,
        )
    } else {
        None
    };
    let written = (|| -> Result<()> {
        while let Some(frame) = queue.next() {
            stream.write_all(&frame)?;
        }
        Ok(())
    })();
    queue.close();
    transport::shutdown(&stream);
    if let Some(reader) = reader {
        let _ = reader.join();
    }
    written
}

fn frontend_refused(request: Option<&Value>, code: &str) -> Value {
    json!({"request_id":request.and_then(|v|v.get("request")).and_then(|v|v.get("request_id")),
        "accepted":false,"durable":false,"code":code})
}

fn request(location: &Location, store: &mut Store, value: Option<&Value>) -> Value {
    let id = value
        .and_then(|v| v["request_id"].as_str())
        .filter(|id| uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == *id));
    let Some(id) = id else {
        return json!({"accepted":false,"code":"invalid_request_id"});
    };
    let op = value.and_then(|v| v["op"].as_str()).unwrap_or("");
    // Revocation only reduces authority. Every positive effect remains fenced.
    if op == "ping" {
        json!({"request_id":id,"accepted":true,"durable":false,"op":"pong"})
    } else if matches!(op, "revoke" | "revoke_status") {
        revoke::request(location, store, value.expect("validated request"))
    } else {
        json!({"request_id":id,"accepted":false,"code":"handoff_required"})
    }
}

fn connection(location: &Location, op: &str, request: Option<Value>) -> Result<Option<Stream>> {
    location.check_existing()?;
    let Some(mut stream) = transport::connect(location)? else {
        return Ok(None);
    };
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let hello = json!({"abi":ABI,"configuration":location.configuration,"op":op,"request":request});
    let bytes = protocol::encode(&hello)?;
    ensure!(
        bytes.len() <= protocol::CONTROL_BYTES,
        "desktop request exceeds its limit"
    );
    stream.write_all(&bytes)?;
    Ok(Some(stream))
}
// A new private overload for the task-control end-to-end budget. Existing
// callers retain their exact connection/timeouts and wire bytes.
fn connection_until(
    location: &Location,
    op: &str,
    request: Option<Value>,
    until: Instant,
) -> Result<Option<Stream>> {
    ensure!(
        Instant::now() < until,
        "task-control request deadline expired"
    );
    location.check_existing()?;
    let Some(mut stream) = transport::connect_until(location, until)? else {
        return Ok(None);
    };
    let remaining = until.saturating_duration_since(Instant::now());
    ensure!(
        !remaining.is_zero(),
        "task-control request deadline expired"
    );
    stream.set_read_timeout(Some(remaining))?;
    stream.set_write_timeout(Some(remaining))?;
    let hello = json!({"abi":ABI,"configuration":location.configuration,"op":op,"request":request});
    let bytes = protocol::encode(&hello)?;
    ensure!(
        bytes.len() <= protocol::CONTROL_BYTES,
        "desktop request exceeds its limit"
    );
    task_controls::write_until(&mut stream, &bytes, until)?;
    Ok(Some(stream))
}
fn exchange(location: &Location, op: &str, request: Option<Value>) -> Result<Option<Value>> {
    let revoking = request
        .as_ref()
        .is_some_and(|value| matches!(value["op"].as_str(), Some("revoke" | "revoke_status")));
    let activation_wait = op == "frontend_request"
        && request
            .as_ref()
            .is_some_and(|v| v["request"]["op"] == "activation_wait");
    let Some(stream) = connection(location, op, request)? else {
        return Ok(None);
    };
    // Revocation includes a bounded SQLite busy wait and durable filesystem I/O.
    // A timeout remains an unknown result, never permission to start Qt.
    if op == import_evidence::held::OP {
        stream.set_read_timeout(Some(import_evidence::TOTAL))?;
    } else if revoking || op == "frontend_request" {
        stream.set_read_timeout(Some(if activation_wait {
            zerus_client_core::desktop::activation::CLIENT_WAIT
        } else {
            Duration::from_secs(10)
        }))?;
    }
    let bytes = protocol::line(&mut BufReader::new(stream), protocol::FRAME_BYTES)?
        .context("desktop service disconnected")?;
    let response: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        response["abi"] == ABI && response["t"] != "error",
        "desktop service refused the request"
    );
    Ok(Some(response["value"].clone()))
}
/// Restart only the worker retained by an already-running background service.
/// Offline/passive/revoked owners are refused; this never starts a service or GUI.
#[cfg(windows)]
pub(crate) fn restart_swarm(config: &Config) -> Result<Value> {
    let budget = TIMEOUT;
    #[cfg(test)]
    let budget = swarm_cli_test::budget(config).unwrap_or(budget);
    bounded_swarm_restart(config, Instant::now() + budget, restart_swarm_until)
}
#[cfg(all(test, not(windows)))]
fn restart_swarm(config: &Config) -> Result<Value> {
    let budget = swarm_cli_test::budget(config).context("no CLI lookup fixture selected")?;
    bounded_swarm_restart(config, Instant::now() + budget, |config, _| {
        let _ = swarm_cli_test::Location::new(config)?;
        anyhow::bail!("CLI lookup fixture finished")
    })
}
#[cfg(any(windows, test))]
pub(crate) static SWARM_RESTART_ACQUIRING: AtomicBool = AtomicBool::new(false);
#[cfg(any(windows, test))]
fn bounded_swarm_restart(
    config: &Config,
    until: Instant,
    request: impl FnOnce(&Config, Instant) -> Result<Value> + Send + 'static,
) -> Result<Value> {
    // Path/state discovery can involve filesystem I/O. Only one
    // acquisition/request worker may outlive this caller's total deadline.
    ensure!(
        SWARM_RESTART_ACQUIRING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok(),
        "desktop swarm restart acquisition is still owned"
    );
    let config = config.clone();
    let (send, receive) = mpsc::sync_channel(1);
    let spawned = thread::Builder::new()
        .name("swarm-restart-request".into())
        .spawn(move || {
            struct Admission;
            impl Drop for Admission {
                fn drop(&mut self) {
                    SWARM_RESTART_ACQUIRING.store(false, Ordering::Release);
                }
            }
            let _admission = Admission;
            let result = request(&config, until);
            drop(config);
            let _ = send.try_send(result);
        });
    if let Err(error) = spawned {
        SWARM_RESTART_ACQUIRING.store(false, Ordering::Release);
        return Err(error.into());
    }
    receive
        .recv_timeout(until.saturating_duration_since(Instant::now()))
        .context("desktop swarm restart outcome is unconfirmed; do not repeat this attempt")?
}
#[cfg(windows)]
pub(crate) fn restart_swarm_until(config: &Config, until: Instant) -> Result<Value> {
    #[cfg(test)]
    use swarm_cli_test::Location;
    use zerus_client_core::desktop::windows::{verify_own_server_until, Server};
    let location = Location::new(config)?;
    location.check_existing()?;
    let mut stream = transport::connect(&location)?
        .context("the Windows desktop background service is not running")?;
    let pipe = stream.verification_handle()?;
    let peer = match verify_own_server_until(&pipe, until) {
        Server::Verified(peer) => peer,
        _ => anyhow::bail!("the desktop swarm owner could not be verified"),
    };
    ensure!(peer.current(), "desktop swarm owner exited");
    let remaining = || {
        until
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .context("desktop swarm restart deadline expired")
    };
    stream.set_write_timeout(Some(remaining()?))?;
    stream.write_all(&protocol::encode(&json!({"abi":ABI,"configuration":location.configuration,"op":swarm_windows::OP,"request":null}))?)?;
    stream.set_read_timeout(Some(remaining()?))?;
    let bytes = protocol::line(&mut BufReader::new(stream), protocol::FRAME_BYTES)?
        .context("desktop swarm owner disconnected")?;
    ensure!(
        peer.current() && Instant::now() < until,
        "desktop swarm restart outcome is unconfirmed"
    );
    let response: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        response["abi"] == ABI && response["t"] == "response",
        "invalid desktop swarm restart response"
    );
    let value = response["value"].clone();
    ensure!(
        value["accepted"] == true,
        "desktop swarm restart refused: {}",
        value["code"]
    );
    Ok(value)
}
#[cfg(test)]
mod swarm_cli_test;
fn status(location: &Location) -> Result<Value> {
    Ok(exchange(location, "status", None)?.unwrap_or_else(|| json!({"abi":ABI,"running":false,"ownership":"unavailable","configuration":location.configuration,"writers_enabled":false,
        "lifecycle":lifecycle::unavailable(&location.configuration),
        "frontend_read_delivery":frontend::inspect(&location.configuration)})))
}
fn watch(location: &Location) -> Result<()> {
    let mut upgrade = upgrade::Upgrade::new();
    let stream = connection(location, "watch", None)?.context("desktop service is not running")?;
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    let detached = stream.try_clone()?;
    thread::Builder::new()
        .name("desktop-stdin".into())
        .spawn(move || {
            let _ = std::io::stdin().read(&mut [0]);
            transport::shutdown(&detached);
        })?;
    let mut reader = BufReader::new(stream);
    let mut output = std::io::stdout().lock();
    let mut line = protocol::WatchLine::default();
    let mut last: Option<(String, u64)> = None;
    loop {
        if upgrade.poll(Instant::now()) {
            if let Some((epoch, seq)) = &last {
                output.write_all(&protocol::encode(&frame(
                    epoch,
                    *seq,
                    "error",
                    json!({"code":"hgs_replaced","fatal":true}),
                ))?)?;
                output.flush()?;
            }
            break;
        }
        let bytes = match line.read(&mut reader)? {
            protocol::ReadTurn::Pending => continue,
            protocol::ReadTurn::End => break,
            protocol::ReadTurn::Frame(bytes) => bytes,
        };
        // Retain only the original stream identity for a local transport error.
        // It cannot construct a writer receipt or a service incarnation.
        #[derive(serde::Deserialize)]
        struct ErrorCode {
            code: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct Header {
            abi: String,
            epoch: String,
            seq: u64,
            t: String,
            value: ErrorCode,
        }
        let header = serde_json::from_slice::<Header>(&bytes).ok();
        if let Some(header) = &header {
            if header.abi == ABI
                && uuid::Uuid::parse_str(&header.epoch)
                    .is_ok_and(|id| !id.is_nil() && id.to_string() == header.epoch)
            {
                last = Some((header.epoch.clone(), header.seq));
            }
        }
        output.write_all(&bytes)?;
        output.flush()?;
        if header.is_some_and(|header| {
            header.abi == ABI
                && header.t == "error"
                && header.value.code.as_deref() == Some("hgs_replaced")
        }) {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod background_command_tests {
    use super::*;
    #[test]
    fn registered_mode_keeps_original_path_validation_and_launchd_boundary() {
        let old: Vec<String> = [
            "desktop",
            "serve",
            "--config-dir",
            "/private/config",
            "--state-dir",
            "/private/state",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert!(registered_command(&old));
        let mut background = old.clone();
        background.push("--background".into());
        assert!(registered_command(&background));
        background.push("--launchd".into());
        assert!(launchd_command(&background));
        let mut invalid = old;
        invalid[5] = "--background".into();
        assert!(
            registered_command(&invalid),
            "path value must not be swallowed as an option"
        );
        assert!(launch_configuration(&invalid).is_err());
    }
}
