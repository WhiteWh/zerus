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
mod owner;
mod ownership;
mod pending_reads;
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
mod protocol;
mod registration;
mod revoke;
#[allow(dead_code)]
mod startup_exclusion;
mod startup_stdio;
mod store;
mod transport;
mod ui_authority;
#[cfg(windows)]
mod windows_registration;
#[cfg(any(windows, test))]
mod windows_registration_policy;

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

const USAGE: &str = "usage: hgs desktop serve [--config-dir DIR --state-dir DIR] [--background] | status [--json] | startup --json | owner-status --json | ownership-status --json | ownership-result --json | watch --json | feed --json | request --json | catalog-relay --json | registration --json | registration install|remove|status --json (Windows) | notification-registration install|status --json | notification-registration remove --json --request-id UUID (Windows) | revoke --json [--request-id UUID] [--reason rollback] | import-evidence --json | import-status --json --import-id UUID";
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
    let run = || -> Result<()> {
        ensure!(
            !dry && (target.is_empty() || config.is_self(target)),
            "desktop manages only this computer; remote desktop IPC is refused"
        );
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
                    cfg!(any(target_os = "linux", target_os = "macos")),
                    "background registration switching is currently supported on Linux and macOS"
                );
                println!("{}", registration::preview_mode(config, &location, true)?);
                Ok(())
            }
            ["registration", "mode-check", "--json"] => {
                #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                {
                    let scope = ui_authority::Scope::new(config, &location);
                    let source = ui_authority::registration::caller(&scope)?;
                    println!(
                        "{}",
                        json!({"schema":1,"configuration":location.configuration,"admitted":true,"source":source})
                    );
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
                    serde_json::from_slice(crate::state::without_bom(&bytes))?;
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
                    serde_json::from_slice(crate::state::without_bom(&bytes)).context("invalid desktop request")?;
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
    value["desktop_ownership"] = ownership::status(store, &location.configuration)
        .and_then(|v| Ok(serde_json::to_value(v)?))
        .unwrap_or(Value::Null);
    value
}

enum Event {
    ApprovalAudit(Hello, mpsc::SyncSender<approval_audit::worker::Reply>),
    ImportOwner(String, mpsc::SyncSender<import_evidence::held::Loan>),
    Command(Hello, Arc<Queue>, Option<Box<ui_authority::Authorized>>),
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
    let (_signals, owner, listener, epoch, mut store) = (|| -> Result<_> {
        let signals = platform::ShutdownGuard::install()?;
        // This dedicated service process has no workers yet. Make socket creation
        // private too, so a concurrent status probe never sees the pre-chmod mode.
        #[cfg(unix)]
        unsafe {
            libc::umask(0o077);
        }
        location.prepare()?;
        let lock = Arc::new(location.lock()?);
        let epoch = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&location, &epoch)?;
        // A status deadline must not start while startup still owns the actor
        // for migrations/recovery. Publish only once that work has completed.
        let listener = Arc::new(Listener::bind(&location)?);
        Ok((signals, lock, listener, epoch, store))
    })()
    .context(StartupRefused)?;
    let stopped = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicUsize::new(0));
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
    let ui_scope = Arc::new(ui_authority::Scope::new(config, &location));
    // At most16 queued controls/data, thirteen bounded parsers and one passive
    // producer. Background mode owns the only internal source producer.
    let (send, receive) = mpsc::sync_channel(protocol::PENDING);
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
    let mut fanout = Fanout::default();
    let mut migration = handoff::inspect(&location);
    let mut seq = 0;
    let mut heartbeat = Instant::now() + HEARTBEAT;
    let mut collect_at = Instant::now();
    let mut failed = false;
    let mut background_blocked = false;
    let run_result = (|| -> Result<()> {
        if background_mode {
            background_blocked = !revoke::background_permitted(&location, &store)?;
        }
        if background_mode && !background_blocked {
            background = Some(
                background::Runtime::start(config, &epoch, &mut feed, owner.clone())
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
                )?);
                background.as_mut().unwrap().start_presence(&location)?;
                background_blocked = false;
            }
            let mut full_batch = false;
            if let Some(runtime) = &mut background {
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
            }
            frontend.tick(&location);
            store.recover_operation_custody(&location);
            match receive.recv_timeout(if full_batch {
                Duration::ZERO
            } else {
                Duration::from_millis(200)
            }) {
                Ok(Event::ImportOwner(configuration, reply)) => {
                    seq += 1;
                    let owner = if configuration == location.configuration {
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
                    let _ = reply.try_send(audit.admit(hello, owner.clone(), seq));
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
                        let result = if hello.request.as_ref().is_some_and(|v| {
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
                            .is_some_and(|v| v["op"] == "creation_clone")
                        {
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
                        } else if let Some(runtime) = &mut background {
                            let request_value = hello.request.as_ref().unwrap_or(&Value::Null);
                            if matches!(
                                request_value["op"].as_str(),
                                Some("revoke" | "revoke_status")
                            ) {
                                let handoff = runtime.handoff();
                                let revoke_epoch = runtime.revocation_epoch(&epoch);
                                let result = revoke::request_with_runtime(
                                    &location,
                                    &mut store,
                                    request_value,
                                    &revoke_epoch,
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
            if feed.retire(&mut store).unwrap_or(false) {
                seq += 1;
                fanout.publish(
                    protocol::encode(&frame(
                        &epoch,
                        seq,
                        "catalog_changed",
                        json!({"display_only":true,"retired":true,"reset_required":true,
                        "changes_abi":catalog_read::CHANGES_ABI,
                        "attention_counts":attention_counts(&store,&epoch,seq,&feed)}),
                    ))?
                    .into(),
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
                fanout.publish(protocol::encode(&frame(&epoch, seq, "heartbeat", value))?.into());
                heartbeat = Instant::now() + HEARTBEAT;
            }
        }
        ensure!(!failed, "desktop listener failed");
        Ok(())
    })();
    stopped.store(true, Ordering::SeqCst);
    listener.close();
    let _ = acceptor.join();
    drop(fanout);
    for event in receive.try_iter() {
        if let Event::Command(_, queue, _) = event {
            queue.close();
        }
    }
    let deadline = Instant::now() + TIMEOUT + Duration::from_secs(1);
    while active.load(Ordering::SeqCst) != 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let drained = background.as_mut().map_or(Ok(()), |runtime| {
        runtime.drain(&mut store, Duration::from_secs(4))
    });
    let feed_stopped = feed.stop(&mut store);
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
    Ok(())
}

fn client(
    mut stream: Stream,
    send: mpsc::SyncSender<Event>,
    stopped: Arc<AtomicBool>,
    ingress: Arc<feed::Ingress>,
    location: &ui_authority::Scope,
) -> Result<()> {
    // Unix peer credentials require no input. Windows pipe impersonation can
    // require a completed read, so that authentication remains after the hello.
    #[cfg(unix)]
    transport::authenticate(&stream)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let bytes =
        protocol::line(&mut reader, protocol::CONTROL_BYTES)?.context("missing desktop hello")?;
    #[cfg(windows)]
    transport::authenticate(&stream)?;
    let hello: Hello = serde_json::from_slice(&bytes).context("invalid desktop hello")?;
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
    send.try_send(Event::Command(
        hello,
        queue.clone(),
        authority.map(Box::new),
    ))
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
fn status(location: &Location) -> Result<Value> {
    Ok(exchange(location, "status", None)?.unwrap_or_else(|| json!({"abi":ABI,"running":false,"ownership":"unavailable","configuration":location.configuration,"writers_enabled":false,
        "lifecycle":lifecycle::unavailable(&location.configuration),
        "frontend_read_delivery":frontend::inspect(&location.configuration)})))
}
fn watch(location: &Location) -> Result<()> {
    let stream = connection(location, "watch", None)?.context("desktop service is not running")?;
    stream.set_read_timeout(None)?;
    let detached = stream.try_clone()?;
    thread::Builder::new()
        .name("desktop-stdin".into())
        .spawn(move || {
            let _ = std::io::stdin().read(&mut [0]);
            transport::shutdown(&detached);
        })?;
    let mut reader = BufReader::new(stream);
    let mut output = std::io::stdout().lock();
    while let Some(bytes) = protocol::line(&mut reader, protocol::FRAME_BYTES)? {
        output.write_all(&bytes)?;
        output.flush()?;
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
