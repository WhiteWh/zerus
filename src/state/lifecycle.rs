use super::*;
use crate::platform::ExitInfo;
use std::time::{Duration, Instant};

fn required_env(name: &str) -> Result<String> {
    nonempty_env(name).ok_or_else(|| format!("missing {name} in tracked agent launch"))
}

/// Whether a tracked Codex launch passes `--no-daemon`. Older Codex already
/// runs in-process and rejects this newer flag, so the native CLI is asked.
/// A project launcher such as `run_codex.sh` runs only under supervision: it
/// may prompt, unlock secrets or exit before this run is recorded. It receives
/// the flag that reviewed Codex releases require; `--bare` selects the native
/// CLI for an older Codex.
fn codex_daemon_flag(executable: &str) -> Result<bool> {
    if executable != "codex" {
        return Ok(true);
    }
    codex_supports_no_daemon(executable)
}

fn codex_supports_no_daemon(executable: &str) -> Result<bool> {
    Ok(agent_help(executable)?.split_whitespace().any(|word| word == "--no-daemon"))
}

pub(super) fn agent_help(executable: &str) -> Result<String> {
    use std::process::Stdio;
    // Probe capabilities without starting a conversation or inheriting a hook identity.
    let mut output = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut command = crate::platform::agent_command(&[executable.into(), "--help".into()])
        .map_err(|error| format!("{executable}: {error}"))?;
    let mut child = crate::platform::spawn(
        command
            .env_remove("HGS_RUN_ID")
            .env_remove("HGS_SESSION")
            .env("KIMI_CODE_NO_AUTO_UPDATE", "1")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(output.try_clone().map_err(|e| e.to_string())?),
    )
    .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().map_err(|e|e.to_string())? {
            if !status.success() { return Err(format!("Cannot check {executable} launch capabilities (--help failed)")); }
            use std::io::{Read, Seek, SeekFrom};
            output.seek(SeekFrom::Start(0)).map_err(|e|e.to_string())?;
            let mut text=String::new();output.take(128*1024).read_to_string(&mut text).map_err(|e|e.to_string())?;
            return Ok(text);
        }
        if Instant::now()>=deadline {
            let _=child.kill();let _=child.wait();return Err(format!("{executable} capability check timed out"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(super) fn run(args: &[String]) -> Result<i32> {
    let first_count: usize = args
        .first()
        .ok_or("missing launch argument count")?
        .parse()
        .map_err(|_| "invalid launch argument count")?;
    if first_count >= args.len() {
        return Err("missing agent command".into());
    }
    let mut first = args[1..=first_count].to_vec();
    let mut base = args[1 + first_count..].to_vec();
    if base.first().map(|arg| arg.is_empty()).unwrap_or(true) {
        return Err("missing agent command".into());
    }
    let mut name = required_env("HGS_SESSION")?;
    let agent = required_env("HGS_AGENT")?;
    home_var(&agent)?;
    let codex_no_daemon = agent == "codex" && codex_daemon_flag(&base[0])?;
    for key in ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "KIMI_CODE_HOME"] {
        if nonempty_env(key).is_none() {
            std::env::remove_var(key);
        }
    }
    // Strip only a capability-verified interactive launch prompt. Never save it
    // in the native resume recipe, where a later resume could replay it.
    let native_kimi = ["kimi", "kimi.exe", "kimi.cmd"].contains(&Path::new(&base[0])
        .file_name().and_then(|name| name.to_str()).unwrap_or(""));
    let mut launch_prompt = if agent == "kimi" && native_kimi && base.len() > 1 {
        // Resolve after the account's login shell and environment assignments.
        // Probe and spawn the same absolute path, including PATH-local installs
        // and Windows npm shims. A project run_kimi.sh owns its own argv contract
        // and must never run twice just to probe help; --bare selects native Kimi.
        let resolved = crate::platform::which(&base[0]).ok_or("Kimi is not found on the launch PATH")?;
        let resolved = std::path::absolute(resolved).map_err(|e|e.to_string())?.to_string_lossy().into_owned();
        let help = agent_help(&resolved)?;
        let prompt = kimi_launch::take_prompt(&mut base, &help)?;
        if !first.is_empty() {
            let other = kimi_launch::take_prompt(&mut first, &help)?;
            if other != prompt { return Err("Kimi launch prompts do not match".into()); }
        }
        if let Some(text) = &prompt {
            input::validate_launch_prompt(text)?;
            base[0] = resolved.clone();
            if !first.is_empty() { first[0] = resolved; }
        }
        prompt.map(|text| kimi_launch::Prompt::new(text,
            nonempty_env("HGS_EXPECTED_ID").or_else(|| nonempty_env("HGS_REQUESTED_ID")).unwrap_or_default()))
    } else { None };
    if nonempty_env("HGS_ACCOUNT_ID").is_some_and(|id| !id.starts_with("native-")) {
        for key in crate::accounts::AUTH_ENV {
            std::env::remove_var(key);
        }
    }
    // U60 (#658 N3): the model server's secret environment comes from the
    // private file the launch left; it is applied after the account's variables
    // so a managed account cannot strip it, and the file is removed here.
    crate::state::model_servers::apply_session_secrets().map_err(|error| error.message)?;
    if agent == "claude" {
        // The tmux server may predate this SSH connection and still belong to
        // another security session. Join immediately before the native launch.
        let _ = crate::macos_session::join();
        crate::accounts::prepare_claude_settings().map_err(|e|e.message)?;
    }
    crate::accounts::prepare_mcp_servers(&agent).map_err(|e|e.message)?;
    hooks::install_hooks(&agent, &base[0])?;
    let requested = nonempty_env("HGS_REQUESTED_ID");
    let expected = nonempty_env("HGS_EXPECTED_ID");
    let fresh = nonempty_env("HGS_FRESH").as_deref() == Some("1");
    // `resume --fresh` names the exact stopped run it replaces.
    let replaces = nonempty_env("HGS_FRESH_REPLACES");
    let requested_permission = nonempty_env("HGS_PERMISSION_MODE");
    std::env::remove_var("HGS_PERMISSION_MODE");
    std::env::remove_var("HGS_REQUESTED_ID");
    std::env::remove_var("HGS_FRESH");
    std::env::remove_var("HGS_FRESH_REPLACES");
    if requested.is_some() && expected.is_some() {
        return Err("requested and saved resume identities cannot be combined".into());
    }
    if replaces.is_some() && (!fresh || requested.is_some() || expected.is_some()) {
        return Err("a fresh start cannot resume a conversation".into());
    }
    if requested
        .as_ref()
        .is_some_and(|id| id.chars().any(char::is_whitespace))
    {
        return Err("requested conversation ID cannot contain whitespace".into());
    }
    let archive_id = nonempty_env("HGS_ARCHIVE_ID");
    std::env::remove_var("HGS_ARCHIVE_ID");
    let fork_hint_revision = nonempty_env(agent_hint::FORK_REVISION_ENV);
    std::env::remove_var(agent_hint::FORK_REVISION_ENV);
    let _signals = crate::platform::ShutdownGuard::install().map_err(|e| e.to_string())?;
    // The session host's channel for pauses and input checks (spec D10, D11).
    #[cfg(windows)]
    crate::host::supervisor::open();
    let token = nonempty_env("HGS_RUN_ID").unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let pane = backend::current().own_pane()?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let permission_override: Option<String>;
    {
        let _guard = lock(None)?;
        let actual_name = backend::current()
            .pane_info(
                backend::Target::Pane(&pane),
                &[backend::PaneField::SessionName],
            )?
            .value()
            .to_owned();
        if actual_name.is_empty() {
            return Err("launch pane has no session name".into());
        }
        if name != actual_name {
            name = actual_name;
        }
        let mut replaced = None;
        if expected.is_none() && record_path(&name).exists() {
            let previous = read(&name)?;
            if replaces.as_deref().is_some_and(|run| run != string(&previous, "run_id")) {
                return Err(format!("{name}: saved binding changed before a fresh start"));
            }
            refuse_unsettled_clear(&previous)?;
            attempts::prepare_replacement(&previous, &pane, &token, fresh, requested.as_deref())?;
            replaced = Some(previous);
        } else if replaces.is_some() {
            return Err(format!("{name}: saved binding disappeared before a fresh start"));
        }
        let startup_kind = if expected.is_some() || requested.is_some() || !first.is_empty() {
            "resume"
        } else {
            "new"
        };
        let infer_project = startup_kind == "new" && replaced.is_none()
            && nonempty_env("HGS_FORK_PARENT_ID").is_none();
        let mut record = json!({"version": 1, "run_identity_version": 1, "name": name, "agent": agent, "run_id": token, "pane": pane,
            "cwd": cwd, "launch_dir": cwd, "base": base, "hgs": required_env("HGS_EXECUTABLE")?,
            "shell": nonempty_env("SHELL").unwrap_or_else(|| "/bin/bash".into()), "created": now() as i64,
            "activity": "unknown", "phase": "unknown", "conversation_state": "unknown", "conversation_id": null,
            "transcript": null, "startup_kind":startup_kind,"launch_id":nonempty_env("HGS_LAUNCH_ID"), "account_id": nonempty_env("HGS_ACCOUNT_ID"), "agent_home": nonempty_env(home_var(&agent)?), "pid": std::process::id(),
            "process_start": process_start(std::process::id()),
            "supervisor": {"pid": std::process::id(), "start": process_start(std::process::id())}});
        // U60 (#658 N4): the binding names the model server and model it runs
        // on, so a later resume applies the same provider or refuses.
        if let Some(model_server) = model_server_record() {
            record["model_server"] = model_server;
        }
        if let Some(parent) = nonempty_env("HGS_FORK_PARENT_ID") {
            record["fork_parent_id"] = json!(parent);
            record.as_object_mut().unwrap().remove("launch_id");
            record["fork_source_name"] =
                json!(nonempty_env("HGS_FORK_SOURCE_NAME").unwrap_or_default());
        }
        std::env::remove_var("HGS_FORK_PARENT_ID");
        std::env::remove_var("HGS_FORK_SOURCE_NAME");
        agent_hint::inherit_fork(&mut record, fork_hint_revision.as_deref());
        if let Some(expected) = expected {
            let previous = read(&name)?;
            refuse_unsettled_clear(&previous)?;
            if string(&previous, "conversation_id") != expected
                || string(&previous, "agent") != agent
            {
                return Err(format!("{name}: saved binding changed before agent launch"));
            }
            for key in ["base", "created", "launch_dir"] {
                record[key] = previous
                    .get(key)
                    .ok_or_else(|| format!("{name}: missing saved {key}"))?
                    .clone();
            }
            // Optional on old bindings; never make an existing resume depend
            // on fields introduced by the rules feature.
            for key in ["iron_rules", "iron_rules_revision"] {
                if let Some(value) = previous.get(key) { record[key] = value.clone(); }
            }
            // U60 (#658 N4): a resumed session keeps its model server.
            if let Some(model_server) = previous.get("model_server") {
                record["model_server"] = model_server.clone();
            }
            let restore_id = archive_id
                .clone()
                .or_else(|| nonempty_value(&previous, "restore_archive_id"));
            if let Some(id) = restore_id {
                let archived = archive::read_archive(&name, &id)?;
                if string(&archived, "conversation_id") != expected {
                    return Err("archive resume ID does not match binding".into());
                }
                record["restore_archive_id"] = json!(id);
            }
            record["expected_id"] = json!(expected);
            record["conversation_id"] = json!(expected);
            record["transcript"] = previous["transcript"].clone();
            for key in [
                "prompt",
                "launch_id",
                "last_message",
                "model",
                "effort",
                "effort_source_path",
                "effort_source_offset",
                "resume_settings",
                "pending_settings",
                "fork_parent_id",
                "fork_source_name",
                "cleared_conversations",
                "permission_override",
            ] {
                if let Some(value) = previous.get(key) {
                    record[key] = value.clone();
                }
            }
            if record["pending_settings"].is_object() {
                record["pending_settings"]["run_id"] = json!(token);
            }
            effort::restore_pending(&mut record)?;
            if let Some(anchor) = previous.get("journal_name") {
                record["journal_name"] = anchor.clone();
            }
            agent_hint::carry_resume(&previous, &mut record);
        } else if let Some(requested) = requested {
            // The ID is an explicit request, not proof that the provider has
            // opened it. Only a matching SessionStart confirms the binding.
            record["expected_id"] = json!(requested);
            record["requested_id"] = json!(requested);
        } else if let Some(previous) = replaced.as_ref().filter(|_| replaces.is_some()) {
            clear_context::carry_fresh_start(previous, &mut record);
        }
        // A per-launch choice is symbolic and durable: resume revalidates this
        // mode instead of trusting stale concrete flags. Account defaults stay
        // dynamic because they never create this field.
        if let Some(mode) = requested_permission.as_deref() {
            let normalized = crate::accounts::normalize_permission_mode(mode).map_err(|e| e.message)?;
            crate::accounts::validate_permission_mode(&agent, &normalized).map_err(|e| e.message)?;
            record["permission_override"] = json!(normalized);
        }
        permission_override = record["permission_override"]
            .as_str()
            .filter(|mode| !mode.is_empty())
            .map(str::to_owned);
        backend::current().set_run_marker(&name, &token)?;
        if let Some(owner)=super::worktree_lifecycle::for_folder(std::path::Path::new(string(&record,"launch_dir"))) {
            record["worktree_owner"]=owner;
        }
        write(&mut record)?;
        // Keep this exact newly-created binding pinned during optional initial
        // placement. A removal/replacement must not inherit the old folder's
        // inference. The swarm lock is nonblocking and Git shares one 3s cap.
        if infer_project {
            if let Ok(config) = crate::config::Config::load() {
                crate::swarm::infer_new_session(&config, &name, &cwd);
            }
        }
    }
    std::env::set_var("HGS_RUN_ID", &token);
    std::env::set_var("HGS_SESSION", &name);
    std::env::set_var("HGS_STATE_DIR", absolute_root()?);
    std::env::remove_var("HGS_EXPECTED_ID");
    let config = crate::config::Config::load().map_err(|e|e.message)?;
    let permission_mode = match permission_override.as_deref() {
        Some(mode) => mode.to_owned(),
        None => crate::accounts::permission_mode(&config,
            &nonempty_env("HGS_ACCOUNT_ID").unwrap_or_default(), &agent).map_err(|e|e.message)?,
    };
    // Explicit native flags in the agent's own argv always win. Do not resolve
    // an account default (or a possibly unavailable saved native mode) at all
    // when every argv already decides its own permissions.
    let needs_account_mode = [&base, &first]
        .iter()
        .any(|argv| !argv.is_empty() && !crate::accounts::explicit_permissions(&agent, argv));
    let permission_values = if needs_account_mode {
        crate::accounts::permission_args(&agent, &permission_mode).map_err(|e|e.message)?
    } else {
        Vec::new()
    };
    for argv in [&mut base, &mut first] {
        crate::accounts::apply_permission_values(&agent, &permission_values, argv);
    }
    // Claude Code reports its context window only to a status line command.
    // The saved base stays as launched; resume adds the settings again.
    if agent == "claude" {
        if let Some(settings) = claude_statusline::launch_settings(&base[0], &cwd, &[&base, &first]) {
            for argv in [&mut base, &mut first] {
                claude_statusline::add_settings(argv, &settings);
            }
        }
    }
    if agent == "codex" {
        for argv in [&mut base, &mut first] {
            let delimiter = argv
                .iter()
                .position(|arg| arg == "--")
                .unwrap_or(argv.len());
            if codex_no_daemon && !argv.is_empty() && !argv[..delimiter].iter().any(|arg| arg == "--no-daemon") {
                // Anything after -- is prompt text, never a CLI option.
                argv.insert(delimiter, "--no-daemon".into());
            }
        }
        eprintln!("hgs: tracking needs trusted hooks (review /hooks if prompted)");
    }
    if !first.is_empty() {
        let started = Instant::now();
        let mut child = spawn_agent(&first, &name, &token)?;
        let child_id = child.id();
        let status = kimi_launch::wait(&mut child, &name, &token, launch_prompt.as_mut())?;
        let confirmed = {
            let _guard = lock(None)?;
            let current =
                read_run(&name, &token)?.ok_or("session changed while launching agent")?;
            !string(&current, "conversation_id").is_empty() || current.get("pausing").is_some()
        };
        let fallback = nonempty_env("HGS_FALLBACK_SECS")
            .unwrap_or_else(|| "5".into())
            .parse::<f64>()
            .map_err(|_| "invalid HGS_FALLBACK_SECS")?;
        if status.success()
            || supervisor::shutdown_requested()
            || ExitInfo::of(status).signal.is_some()
            || confirmed
            || started.elapsed().as_secs_f64() >= fallback
        {
            supervisor::finish(&name, &token, child_id, status)?;
            return Ok(ExitInfo::of(status).exit_code());
        }
        eprintln!("hgs: nothing to resume — starting fresh");
        {
            let _guard = lock(None)?;
            let mut current =
                read_run(&name, &token)?.ok_or("session changed while launching agent")?;
            current["pid"] = json!(std::process::id());
            current["process_start"] = json!(process_start(std::process::id()));
            current["activity"] = json!("unknown");
            current["conversation_id"] = Value::Null;
            current["transcript"] = Value::Null;
            current.as_object_mut().unwrap().remove("cleared_conversations");
            current["startup_kind"] = json!("new");
            write(&mut current)?;
        }
    }
    let mut child = spawn_agent(&base, &name, &token)?;
    let child_id = child.id();
    // The same in-memory ticket survives a definite early resume failure. Once
    // a submission is attempted, even a refusal can never authorize a replay.
    let status = kimi_launch::wait(&mut child, &name, &token, launch_prompt.as_mut())?;
    supervisor::finish(&name, &token, child_id, status)?;
    Ok(ExitInfo::of(status).exit_code())
}

fn nonempty_value(record: &Value, key: &str) -> Option<String> {
    record[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn spawn_agent(argv: &[String], name: &str, run_id: &str) -> Result<std::process::Child> {
    match supervisor::spawn(argv, name, run_id) {
        Ok(child) => Ok(child),
        Err(error) => {
            let _guard = lock(None)?;
            if let Some(mut record) = read_run(name, run_id)? {
                record["error"] = json!(error);
                write(&mut record)?;
                archive::failed_restore(&mut record)?;
            }
            Err(error)
        }
    }
}

pub(super) fn safe_to_pause(record: &Value) -> bool {
    pausable(record, || resume_input::idle(record))
}

/// `safe_to_pause` with the verification of a pending resume supplied by the
/// caller; it is asked only when no confirmed idle conversation qualifies.
pub(super) fn pausable(record: &Value, resumed_idle: impl FnOnce() -> bool) -> bool {
    !telemetry::work_remains(record)
        && ((string(record, "activity") == "idle" && string(record, "expected_id").is_empty())
            // Codex defers SessionStart after resume until the next turn; a
            // verified resume waits at its restored conversation's empty composer.
            || resumed_idle())
}

fn pause_check(name: &str, snapshot: &Snapshot) -> Result<Value> {
    let record = read(name)?;
    if !matches(&record, snapshot.get(name)) {
        return Err(format!(
            "{name}: live pane does not match its saved binding"
        ));
    }
    if pause_active(&record) {
        return Err(format!("{name}: another pause is already in progress"));
    }
    if !safe_to_pause(&record) {
        return Err(format!(
            "{name}: agent is busy or unconfirmed; wait for its answer before pausing"
        ));
    }
    if !process_alive(&record) {
        return Err(format!("{name}: tracked process is no longer present"));
    }
    recipes::verify_history(&record, false)?;
    recipes::resume_argv(
        string(&record, "agent"),
        &recipes::argv(&record)?,
        string(&record, "conversation_id"),
    )?;
    Ok(record)
}

pub(super) fn pause_one(record: &Value, scope: Option<&session_action::Scope>) -> Result<()> {
    pause_one_quiet(record, scope, false)
}

/// `pause_one` without its report, for a caller that prints its own receipt.
pub(super) fn pause_one_quiet(record: &Value, scope: Option<&session_action::Scope>, quiet: bool) -> Result<()> {
    let name = string(record, "name");
    {
        let _guard = lock(None)?;
        let current = read(name)?;
        if let Some(scope)=scope {scope.check(&current)?;}
        if current["run_id"] != record["run_id"]
            || !safe_to_pause(&current)
            || !matches(&current, live()?.get(name))
            || !process_alive(&current)
        {
            return Err("session changed while pausing".into());
        }
        let pid = current["pid"]
            .as_u64()
            .and_then(|pid| i32::try_from(pid).ok())
            .filter(|pid| *pid > 0)
            .ok_or("invalid tracked process ID")?;
        // Only signal the exact PID whose start identity was just checked.
        // Never target a process group, terminal input or the tmux server. On
        // Windows the pane's supervisor raises Ctrl+Break for the group it
        // shares with this agent, as spec D11 decides.
        if let Some(scope)=scope {scope.changing();}
        backend::current().request_exit(string(&current, "pane"), pid as u32)?;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let remaining = loop {
        let snapshot = live()?;
        let panes = snapshot.get(name);
        if panes.is_none()
            || panes
                .map(|panes| {
                    panes.len() == 1 && panes[0].0 == string(record, "pane") && panes[0].1 == "1"
                })
                .unwrap_or(false)
        {
            break panes.is_some();
        }
        if !matches(record, panes) {
            return Err("session changed while pausing".into());
        }
        if Instant::now() >= deadline {
            return Err("agent did not exit after SIGTERM (no forced kill was sent)".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    recipes::verify_history(&read(name)?, false)?;
    {
        let _guard = lock(None)?;
        let mut current = read(name)?;
        if let Some(scope)=scope {scope.check(&current)?;}
        if current["run_id"] != record["run_id"] {
            return Err("session changed while pausing".into());
        }
        current["paused"] = json!(true);
        current.as_object_mut().unwrap().remove("pausing");
        write(&mut current)?;
    }
    if remaining {
        let _guard=lock(None)?;
        if let Some(scope)=scope {scope.check(&read(name)?)?;}
        // Revalidate the dead pane before deleting it: tmux may reuse IDs after
        // a server restart while a delayed pause command is still finishing.
        let snapshot = live()?;
        if snapshot
            .get(name)
            .map(|panes| {
                panes.len() == 1
                    && panes[0].0 == string(record, "pane")
                    && panes[0].1 == "1"
                    && panes[0].2 == string(record, "run_id")
            })
            .unwrap_or(false)
        {
            backend::current().kill_pane(string(record, "pane"))?;
        }
    }
    if let Some(scope)=scope {scope.result(name,string(record,"run_id"),string(record,"conversation_id"),None);} else if !quiet {println!("hgs: paused {name}");}
    Ok(())
}

pub(super) fn pause(names: &[String], dry: bool) -> Result<i32> { pause_scoped(names,dry,None) }

pub(super) fn pause_scoped(names: &[String], dry: bool, scope: Option<&session_action::Scope>) -> Result<i32> {
    let snapshot = live()?;
    let names: Vec<String> = if names == ["--all"] {
        snapshot.keys().cloned().collect()
    } else {
        names.to_vec()
    };
    let mut saved: Vec<Value> = names
        .iter()
        .map(|name| pause_check(name, &snapshot))
        .collect::<Result<_>>()?;
    if dry {
        for record in &saved {
            println!(
                "hgs: would pause {} ({})",
                string(record, "name"),
                string(record, "conversation_id")
            );
        }
        return Ok(0);
    }
    let owner = json!({"pid": std::process::id(), "start": process_start(std::process::id())});
    {
        let _guard = lock(None)?;
        for record in &saved {
            let current = read(string(record, "name"))?;
            if let Some(scope)=scope {scope.check(&current)?;}
            if current["run_id"] != record["run_id"] || current["updated"] != record["updated"] {
                return Err(format!(
                    "{}: activity changed while checking; retry pause",
                    string(record, "name")
                ));
            }
        }
        for record in &mut saved {
            if let Some(scope)=scope {scope.changing();}
            record["pausing"] = owner.clone();
            write(record)?;
        }
    }
    let mut failures = Vec::new();
    for record in &saved {
        let name = string(record, "name");
        if let Err(error) = pause_one(record, scope) {
            failures.push(format!("{name}: {error}"));
        }
        let cleanup: Result<()> = (|| {
            let _guard = lock(None)?;
            if record_path(name).exists() {
                let mut current = read(name)?;
                if current["run_id"] == record["run_id"] {
                    if let Some(scope)=scope {scope.check(&current)?;}
                    current.as_object_mut().unwrap().remove("pausing");
                    write(&mut current)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = cleanup {
            failures.push(format!("{name}: {error}"));
        }
    }
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }
    Ok(0)
}

pub(super) fn resume_spec(
    record: &Value,
    run_id: &str,
    archive_id: Option<&str>,
) -> Result<(backend::LaunchSpec, Vec<String>)> {
    let argv = recipes::resume_argv(
        string(record, "agent"),
        &recipes::argv(record)?,
        string(record, "conversation_id"),
    )?;
    session_spec(
        record,
        effort::resume_argv(record, argv),
        run_id,
        archive_id,
        None,
    )
}

/// U60 (#709 L1): the saved recipe's spec and its model-server secret lines.
/// The spec names no key file: one is written right before the backend launch
/// (`start_with_secrets`) and removed when that launch fails; `hgs --run`
/// removes the one it reads on success. A dry-run or an already-live session
/// therefore leaves no file behind.
fn session_spec(
    record: &Value,
    argv: Vec<String>,
    run_id: &str,
    archive_id: Option<&str>,
    replaces: Option<&str>,
) -> Result<(backend::LaunchSpec, Vec<String>)> {
    let name = string(record, "name");
    if !(Path::new(string(record, "launch_dir")).is_dir() || archive_id.is_some() && super::worktree_lifecycle::restorable(record)) {
        return Err(format!(
            "{name}: directory is missing: {}",
            string(record, "launch_dir")
        ));
    }
    let executable = runner_executable()?;
    let hint_environment = crate::config::agent_hint_environment(string(record, "agent"));
    // The owner, 2026-11-02: a Claude session behind a gateway started before
    // its launch carried the /model picker; its resume adds it.
    let mut argv = argv;
    if !argv.iter().any(|arg| arg == "--settings") {
        argv.extend(super::model_servers::resume_picker_args(record));
    }
    let launch = supervisor_command(record, &executable, argv, &hint_environment);
    let mut environment = BTreeMap::from([
        ("HGS_SESSION", name.to_owned()),
        ("HGS_AGENT", string(record, "agent").to_owned()),
        (
            "HGS_STATE_DIR",
            absolute_root()?.to_string_lossy().into_owned(),
        ),
        ("HGS_EXECUTABLE", executable),
        (
            "HGS_EXPECTED_ID",
            if replaces.is_some() { "" } else { string(record, "conversation_id") }.to_owned(),
        ),
        ("HGS_RUN_ID", run_id.into()),
        ("HGS_ARCHIVE_ID", archive_id.unwrap_or("").into()),
        ("HGS_REQUESTED_ID", "".into()),
        ("HGS_FRESH", if replaces.is_some() { "1" } else { "0" }.into()),
        ("HGS_FRESH_REPLACES", replaces.unwrap_or("").into()),
        ("HGS_TRACKING", "1".into()),
        (
            "HGS_CLIENT",
            nonempty_env("HGS_CLIENT").unwrap_or_else(|| "local".into()),
        ),
    ]);
    environment.insert(
        home_var(string(record, "agent"))?,
        string(record, "agent_home").to_owned(),
    );
    // U60 (#658 N4): a session started on a model server resumes with the same
    // provider and model; the secret lines come with the spec and are written
    // only right before the backend launch (#709 L1). A server that is gone
    // refuses the resume instead of silently falling back to the cloud account.
    let mut model_server_vars: Vec<String> = Vec::new();
    let mut model_server_secrets: Vec<String> = Vec::new();
    if let Some(model_server) = record["model_server"].as_object() {
        let server = model_server["server"].as_str().unwrap_or_default();
        let model = model_server["model"].as_str().unwrap_or_default();
        let config = crate::config::Config::load().map_err(|error| error.message)?;
        let (env, secrets) = super::model_servers::resume_environment(&config, server, string(record, "agent"), model)
            .map_err(|error| format!("{name}: model server {server} cannot start this session again: {}", error.message))?;
        model_server_vars.extend(env);
        model_server_secrets.extend(secrets);
        environment.insert("HGS_MODEL_SERVER", server.to_owned());
        environment.insert("HGS_MODEL_SERVER_MODEL", model.to_owned());
    }
    Ok((
        backend::LaunchSpec {
            name: name.into(),
            cwd: string(record, "launch_dir").to_owned(),
            env: environment
                .into_iter()
                .map(|(key, value)| format!("{key}={value}"))
                .chain(account_variables(record))
                .chain(hint_environment)
                .chain(model_server_vars)
                .collect(),
            command: launch,
            detached: true,
        },
        model_server_secrets,
    ))
}

/// Writes the session's model-server secret file right before the backend
/// launch and removes it if that launch fails (#709 L1); on success the pane's
/// `hgs --run` reads and removes it. No secrets mean no file at all.
pub(super) fn start_with_secrets(spec: &backend::LaunchSpec, secrets: &[String]) -> Result<()> {
    launch_with_secrets(spec, secrets, |spec| backend::current().start(spec))
}

/// The shared launch step: writes the key file right before `start`, removes it
/// when `start` fails, and leaves it for `hgs --run` on success (#709 L1).
fn launch_with_secrets<T>(
    spec: &backend::LaunchSpec,
    secrets: &[String],
    start: impl FnOnce(&backend::LaunchSpec) -> Result<T>,
) -> Result<T> {
    if secrets.is_empty() {
        return start(spec);
    }
    let path = super::model_servers::write_secret_file(&absolute_root()?, secrets).map_err(|error| error.message)?;
    let mut launch = backend::LaunchSpec {
        name: spec.name.clone(),
        cwd: spec.cwd.clone(),
        env: spec.env.clone(),
        command: spec.command.clone(),
        detached: spec.detached,
    };
    launch.env.push(format!("HGS_MODEL_SERVER_ENV={}", path.display()));
    match start(&launch) {
        Ok(value) => Ok(value),
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            Err(error)
        }
    }
}

fn print_resume(spec: &backend::LaunchSpec) {
    println!(
        "{}",
        backend::current().describe(&backend::DryRun::Start(spec))
    );
}

fn resume_archive(name: &str, id: &str, dry: bool, scope: Option<&session_action::Scope>) -> Result<i32> {
    let archived = archive::read_archive(name, id)?;
    refuse_unsettled_clear(&archived)?;
    recipes::verify_history(&archived, true)?;
    if let Some(scope)=scope {scope.check(&archived)?;}
    {
        let _guard = lock(None)?;
        if live()?.contains_key(name) || record_path(name).exists() { return Err(format!("{name}: a live or saved session already uses this name; finish or archive it before restoring")); }
    }
    if !dry { super::worktree_lifecycle::restore(&archived)?; }
    let run_id = uuid::Uuid::new_v4().to_string();
    let (spec, secrets) = resume_spec(&archived, &run_id, Some(id))?;
    let _guard = lock(None)?;
    if live()?.contains_key(name) || record_path(name).exists() {
        return Err(format!("{name}: a live or saved session already uses this name; finish or archive it before restoring"));
    }
    let current_archive = archive::read_archive(name, id)?;
    refuse_unsettled_clear(&current_archive)?;
    if let Some(scope)=scope {scope.check(&current_archive)?;}
    if current_archive["run_id"] != archived["run_id"]
        || current_archive["conversation_id"] != archived["conversation_id"]
    {
        return Err("archived binding changed; retry resume".into());
    }
    if dry {
        print_resume(&spec);
        return Ok(0);
    }
    // Reserve the name while holding the same lock used by hooks and ordinary
    // launch. The original archive remains durable until exact confirmation.
    let mut pending = archived.clone();
    for key in [
        "archive_id",
        "archived_at",
        "state",
        "completion_source",
        "completion_reason",
        "exit_code",
        "termination_signal",
        "exited_at",
        "session_end",
        "paused",
        "error",
        "last_restore_error",
        "pausing",
    ] {
        pending.as_object_mut().unwrap().remove(key);
    }
    pending["run_id"] = json!(run_id);
    pending["restore_archive_id"] = json!(id);
    pending["resume_pending"] = json!(true);
    pending["expected_id"] = pending["conversation_id"].clone();
    pending["activity"] = json!("unknown");
    pending["phase"] = json!("unknown");
    pending["pid"] = json!(0);
    pending["process_start"] = json!("");
    if let Some(scope)=scope {scope.changing();}
    write(&mut pending)?;
    if let Err(error) = start_with_secrets(&spec, &secrets) {
        let mut failed = read(name)?;
        if string(&failed, "run_id") == run_id {
            failed["error"] = json!(error);
            archive::failed_restore(&mut failed)?;
        }
        return Err(error);
    }
    if let Some(scope)=scope {scope.result(name,&run_id,string(&pending,"conversation_id"),None);} else {println!("hgs: restoring {name} from archive {id}");}
    Ok(0)
}

/// U3: the newest user or assistant entry time in the last part of a native
/// transcript, or None when it has none.
pub(super) fn transcript_last_turn(path: &std::path::Path) -> Option<f64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(256 * 1024))).ok()?;
    let mut text = String::new();
    file.take(256 * 1024).read_to_string(&mut text).ok()?;
    let mut newest = 0.0_f64;
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if !matches!(event["type"].as_str(), Some("user") | Some("assistant")) {
            continue;
        }
        if let Some(at) = event["timestamp"]
            .as_str()
            .map(|text| super::search::timestamp(&serde_json::json!(text)))
            .filter(|at| *at > 0.0)
            .or_else(|| event["timestamp"].as_f64())
        {
            newest = newest.max(at);
        }
    }
    (newest > 0.0).then_some(newest)
}

/// U3: how far the transcript is behind the events Zerus recorded; behind by
/// more than two minutes is a silent resume that would drop dialogue.
pub(super) fn transcript_lag(transcript_at: f64, events_at: f64) -> Option<f64> {
    (transcript_at > 0.0 && events_at > 0.0 && events_at - transcript_at > 120.0).then_some(events_at - transcript_at)
}

/// U3: the dialogue Zerus recorded after the transcript's last turn, oldest
/// first, from a sessions events index (read-only, bounded to the newest 4000
/// rows; task notifications are not dialogue). N2: only the session's own
/// conversation counts. N3: the newest 400 entries are kept; the number of
/// older ones left out is returned so the note tells the truth.
pub(super) fn read_events(path: &std::path::Path, journal: &str, conversation: &str, since: f64) -> (Vec<serde_json::Value>, usize) {
    if journal.is_empty() || !path.exists() {
        return (Vec::new(), 0);
    }
    let Ok(db) = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return (Vec::new(), 0);
    };
    // N2: the conversation column narrows the rows when the record names one;
    // an old record without a conversation id keeps the name-only read.
    let mut found: Vec<serde_json::Value> = if conversation.is_empty() {
        let Ok(mut statement) = db.prepare("SELECT payload FROM events WHERE name = ?1 ORDER BY seq DESC LIMIT 4000") else {
            return (Vec::new(), 0);
        };
        let Ok(rows) = statement.query_map([journal], |row| row.get::<_, String>(0)) else {
            return (Vec::new(), 0);
        };
        rows.flatten().filter_map(|text| serde_json::from_str::<serde_json::Value>(&text).ok()).collect()
    } else {
        let Ok(mut statement) =
            db.prepare("SELECT payload FROM events WHERE name = ?1 AND conversation = ?2 ORDER BY seq DESC LIMIT 4000")
        else {
            return (Vec::new(), 0);
        };
        let Ok(rows) = statement.query_map([journal, conversation], |row| row.get::<_, String>(0)) else {
            return (Vec::new(), 0);
        };
        rows.flatten().filter_map(|text| serde_json::from_str::<serde_json::Value>(&text).ok()).collect()
    };
    found.retain(|payload| {
        matches!(payload["type"].as_str(), Some("UserPromptSubmit") | Some("Stop"))
            && payload["at"].as_f64().unwrap_or(0.0) > since
            && payload["origin"] != "task_notification"
    });
    found.sort_by(|a, b| {
        a["at"].as_f64().unwrap_or(0.0).total_cmp(&b["at"].as_f64().unwrap_or(0.0))
    });
    // N3: keep the newest entries, not the oldest.
    let dropped = found.len().saturating_sub(400);
    if dropped > 0 {
        found.drain(..dropped);
    }
    (found, dropped)
}

/// U3: the state's sessions events index.
fn events_index() -> std::path::PathBuf {
    super::root().join("sessions").join("events.sqlite3")
}

/// U3: writes the lost dialogue to the recovered folder; None when Zerus
/// recorded none. The newest 400 entries / 400 KiB are kept, the oldest cut
/// with a note (N3); the file goes through the state's private storage (N4).
pub(super) fn write_recovery(
    dir: &std::path::Path,
    conversation: &str,
    events: &[serde_json::Value],
    older_cut: usize,
) -> Option<std::path::PathBuf> {
    if events.is_empty() {
        return None;
    }
    let entry_of = |event: &serde_json::Value| {
        format!("## {} {}\n{}\n\n", event["type"].as_str().unwrap_or_default(), event["at"], event["detail"].as_str().unwrap_or_default())
    };
    // N3: fill the 400 KiB budget from the newest entry backwards.
    let mut kept: Vec<&serde_json::Value> = Vec::new();
    let mut used = 0usize;
    for event in events.iter().rev() {
        let size = entry_of(event).len();
        if used + size > 400 * 1024 {
            break;
        }
        used += size;
        kept.push(event);
    }
    kept.reverse();
    let cut = older_cut + (events.len() - kept.len());
    let mut text = String::from("# Dialogue Zerus recorded after the transcript stopped\n\n");
    for event in &kept {
        text.push_str(&entry_of(event));
    }
    if cut > 0 {
        text.push_str(&format!("[cut: the oldest {cut} entries are not shown]\n"));
    }
    let path = dir.join(format!("{conversation}-{}.md", super::now().round() as i64));
    super::atomic(&path, &text).ok()?;
    Some(path)
}

/// N4: the resumed session was not told about the recovered dialogue; the
/// owner gets a warning that names the file and the reason.
pub(super) fn recovery_send_failure(name: &str, path: &std::path::Path, reason: &str) -> String {
    format!(
        "hgs: warning: {name} was resumed but the recovered-dialogue message was not sent ({reason}); \
         read {} and tell the session yourself",
        path.display()
    )
}

/// U3: the message the resumed session reads first.
pub(super) fn recovery_message(transcript_at: f64, path: &std::path::Path) -> String {
    format!(
        "Your transcript stops at {transcript_at:.0} (unix time); Zerus recorded the dialogue after it: {}. Read it before you go on.",
        path.display()
    )
}

pub(super) fn resume(names: &[String], dry: bool) -> Result<i32> { resume_scoped(names,dry,None) }

pub(super) fn resume_scoped(names: &[String], dry: bool, scope: Option<&session_action::Scope>) -> Result<i32> {
    // U3: the owner may accept a stale transcript explicitly.
    let mut names = names.to_vec();
    let mut accept_stale = false;
    let mut with_lost = false;
    names.retain(|name| {
        let flag = matches!(name.as_str(), "--accept-stale" | "--with-lost-dialogue");
        accept_stale |= flag;
        with_lost |= name == "--with-lost-dialogue";
        !flag
    });
    let names = names.as_slice();
    if scope.is_none() {archive::reconcile()?;}
    if names.len() == 3 && names[1] == "--archive" {
        return resume_archive(&names[0], &names[2], dry, scope);
    }
    if names.iter().any(|name| name == "--archive") {
        return Err("usage: hgs resume <session> --archive ID".into());
    }
    let snapshot = live()?;
    let requested = if scope.is_none() {nonempty_env("HGS_REQUESTED_ID")} else {None};
    let archived = archive::records()?;
    let names = if names == ["--all"] {
        records()?
            .iter()
            .filter(|record| {
                !snapshot.contains_key(string(record, "name"))
                    && !string(record, "conversation_id").is_empty()
                    && !archive::equivalent(record, &archived)
            })
            .map(|record| string(record, "name").to_owned())
            .collect::<Vec<_>>()
    } else {
        names.to_vec()
    };
    let mut recipes = Vec::new();
    let mut recovered: std::collections::BTreeMap<String, (std::path::PathBuf, f64)> = std::collections::BTreeMap::new();
    for name in names {
        if let Some(id) = &requested {
            verify_requested(&read(&name)?, &snapshot, id)?;
        }
        if snapshot.contains_key(&name) {
            if scope.is_some() {return Err("Session is already live; no resume was launched".into());}
            continue;
        }
        let record = read(&name)?;
        // U3: compare the native transcript's last turn with the events Zerus
        // recorded; a silent resume would drop the dialogue after it.
        if string(&record, "agent") == "claude" {
            let transcript = string(&record, "transcript");
            let transcript_at = (!transcript.is_empty())
                .then(|| transcript_last_turn(std::path::Path::new(transcript)))
                .flatten();
            if let Some(transcript_at) = transcript_at {
                let journal = string(&record, "journal_name").to_owned();
                let conversation = string(&record, "conversation_id").to_owned();
                let (events, older_cut) = read_events(&events_index(), &journal, &conversation, transcript_at);
                let anchor = events
                    .last()
                    .and_then(|event| event["at"].as_f64())
                    .unwrap_or(record["last_event_at"].as_f64().unwrap_or(0.0));
                if let Some(lag) = transcript_lag(transcript_at, anchor) {
                    if with_lost {
                        if let Some(path) = write_recovery(
                            &super::root().join("sessions").join("recovered"),
                            &conversation,
                            &events,
                            older_cut,
                        ) {
                            recovered.insert(name.clone(), (path, transcript_at));
                        }
                    }
                    if !accept_stale && !with_lost {
                        // The desktop reads the times and the count from the JSON tail.
                        let details = serde_json::json!({"error":"transcript_behind","transcript_at":transcript_at,
                            "events_at":anchor,"missing":events.len() + older_cut});
                        return Err(format!(
                            "{name}: the native transcript stops {lag:.0} s before the events Zerus recorded (transcript_behind); resume with --accept-stale or --with-lost-dialogue {details}"
                        ));
                    }
                }
            }
        }
        if let Some(scope)=scope {scope.check(&record)?;}
        if archive::equivalent(&record, &archived) {
            return Err(format!(
                "{name}: this session is archived; restore it using --archive ID"
            ));
        }
        refuse_unsettled_clear(&record)?;
        recipes::verify_history(&record, true)?;
        let restore_id = nonempty_value(&record, "restore_archive_id");
        let new_run=uuid::Uuid::new_v4().to_string();
        let (spec, secrets) = resume_spec(&record, &new_run, restore_id.as_deref())?;
        recipes.push((name, spec, secrets, record["run_id"].clone(), record["conversation_id"].clone(),new_run));
    }
    let _guard = lock(None)?;
    for (name, spec, secrets, original_run, original_conversation,new_run) in recipes {
        let current_snapshot = live()?;
        let current = read(&name)?;
        refuse_unsettled_clear(&current)?;
        if let Some(scope)=scope {scope.check(&current)?;}
        if let Some(id) = &requested {
            verify_requested(&current, &current_snapshot, id)?;
        }
        if current_snapshot.contains_key(&name) {
            if scope.is_some() {return Err("Session became live before resume; no replacement was launched".into());}
            continue;
        }
        if current["run_id"] != original_run || current["conversation_id"] != original_conversation {
            return Err(format!("{name}: saved binding changed; retry resume"));
        }
        if dry {
            print_resume(&spec);
        } else {
            if let Some(scope)=scope {scope.changing();}
            start_with_secrets(&spec, &secrets)?;
            // U3: the resumed session reads the recovered dialogue first.
            if let Some((path, transcript_at)) = recovered.get(&name) {
                let message = recovery_message(*transcript_at, path);
                if let Ok(executable) = std::env::current_exe() {
                    let outcome = std::process::Command::new(executable)
                        .args(["send", &name, &message])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                    // N4: a failed send must not leave the file unannounced.
                    match outcome {
                        Ok(status) if status.success() => {}
                        Ok(status) => eprintln!(
                            "{}",
                            recovery_send_failure(&name, path, &format!("hgs send exited with {status}"))
                        ),
                        Err(error) => eprintln!("{}", recovery_send_failure(&name, path, &error.to_string())),
                    }
                } else {
                    eprintln!("{}", recovery_send_failure(&name, path, "the hgs executable is not known"));
                }
            }
            if let Some(scope)=scope {scope.result(&name,&new_run,string(&current,"conversation_id"),None);} else {println!("hgs: resuming {name}");}
        }
    }
    Ok(0)
}

/// U60 (#658 N4): the model server a launch names, for the binding.
fn model_server_record() -> Option<Value> {
    nonempty_env("HGS_MODEL_SERVER").map(|server| {
        json!({"server": server, "model": nonempty_env("HGS_MODEL_SERVER_MODEL").unwrap_or_default()})
    })
}

/// `hgs resume <session> --fresh`: the stopped session's own launch recipe,
/// account and model with a new conversation, like /clear in a live agent.
/// The earlier conversation stays in native history and above the boundary.
pub(super) fn fresh(args: &[String], dry: bool) -> Result<i32> {
    let (name, expected_run) = match args {
        [name] => (name, None),
        [name, flag, run] if flag == "--expected-run-id" && !run.is_empty() => (name, Some(run)),
        _ => return Err("usage: hgs resume <session> --fresh [--expected-run-id ID] [-d]".into()),
    };
    archive::reconcile()?;
    let record = read(name)?;
    refuse_unsettled_clear(&record)?;
    if !AGENTS.contains(&string(&record, "agent")) {
        return Err(format!("{name}: only Claude, Codex and Kimi sessions can start fresh"));
    }
    let argv = recipes::fresh_argv(string(&record, "agent"), &recipes::argv(&record)?)?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let (spec, secrets) = session_spec(
        &record,
        effort::resume_argv(&record, argv),
        &run_id,
        None,
        Some(string(&record, "run_id")),
    )?;
    let _guard = lock(None)?;
    let current = read(name)?;
    refuse_unsettled_clear(&current)?;
    if live()?.contains_key(name) {
        return Err(format!("{name}: the session is running; clear its context instead"));
    }
    if expected_run.is_some_and(|run| run != string(&current, "run_id"))
        || current["run_id"] != record["run_id"]
        || current["conversation_id"] != record["conversation_id"]
    {
        return Err(format!("{name}: saved binding changed; refresh before starting fresh"));
    }
    if archive::equivalent(&current, &archive::records()?) {
        return Err(format!("{name}: this session is archived; restore it using --archive ID"));
    }
    if pause_active(&current) || current["resume_pending"] == true {
        return Err(format!("{name}: wait for the current pause or restore to finish"));
    }
    if dry {
        print_resume(&spec);
        return Ok(0);
    }
    start_with_secrets(&spec, &secrets)?;
    println!("hgs: starting {name} with a fresh conversation");
    Ok(0)
}

fn verify_requested(record: &Value, snapshot: &Snapshot, requested: &str) -> Result<()> {
    let name = string(record, "name");
    let agent = string(record, "agent");
    if string(record, "conversation_id") != requested
        || name.split('/').next() != Some(agent)
        || !attempts::agent_home_matches(record, agent)?
        || (snapshot.contains_key(name)
            && (!matches(record, snapshot.get(name)) || !string(record, "error").is_empty()))
    {
        return Err(format!(
            "{name}: saved or live binding no longer matches requested conversation {requested}"
        ));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn capability_detection_never_runs_a_project_launcher() {
        use std::os::unix::fs::PermissionsExt;
        let folder = tempfile::tempdir().unwrap();
        let marker = folder.path().join("launcher-ran");
        let launcher = folder.path().join("run_codex.sh");
        std::fs::write(&launcher, format!("#!/bin/sh\n: > '{}'\nexit 7\n", marker.display())).unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(codex_daemon_flag(&launcher.to_string_lossy()), Ok(true));
        assert!(!marker.exists());
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    /// U60 (#658 N4): the binding names the model server and model the launch
    /// chose, so a resume can apply them again.
    #[test]
    fn the_record_names_the_model_server_and_model() {
        std::env::set_var("HGS_MODEL_SERVER", "lab");
        std::env::set_var("HGS_MODEL_SERVER_MODEL", "small");
        assert_eq!(model_server_record(), Some(json!({"server":"lab","model":"small"})));
        std::env::remove_var("HGS_MODEL_SERVER");
        std::env::remove_var("HGS_MODEL_SERVER_MODEL");
        assert_eq!(model_server_record(), None);
    }

    /// U60 (#709 L1): the key file is written right before the backend start
    /// and removed when that start fails; a success leaves it for `hgs --run`.
    #[test]
    fn a_launch_writes_its_key_file_right_before_the_start_and_cleans_up_a_failure() {
        let root = tempfile::tempdir().unwrap();
        with_test_state_root(root.path(), || {
            let spec = backend::LaunchSpec {
                name: "claude/p/t".into(),
                cwd: root.path().display().to_string(),
                env: vec!["HGS_SESSION=claude/p/t".into()],
                command: vec!["claude".into()],
                detached: true,
            };
            let seen = std::cell::RefCell::new(None);
            let failed: Result<()> = launch_with_secrets(&spec, &["ANTHROPIC_AUTH_TOKEN=sk-test".into()], |launch| {
                let line = launch
                    .env
                    .iter()
                    .find(|line| line.starts_with("HGS_MODEL_SERVER_ENV="))
                    .expect("the key file path is in the launch environment");
                let path = PathBuf::from(line.trim_start_matches("HGS_MODEL_SERVER_ENV="));
                assert!(path.is_file(), "the key file exists when the backend starts");
                assert!(std::fs::read_to_string(&path).unwrap().contains("sk-test"));
                *seen.borrow_mut() = Some(path);
                Err("the backend refused".to_string())
            });
            assert!(failed.is_err());
            assert!(!seen.borrow().as_ref().unwrap().exists(), "a failed launch removes its key file");
            // A successful start leaves the file for the pane's `hgs --run`.
            let kept: Result<()> = launch_with_secrets(&spec, &["HGS_MODEL_SERVER_KEY=sk".into()], |launch| {
                let line = launch.env.iter().find(|line| line.starts_with("HGS_MODEL_SERVER_ENV=")).unwrap();
                *seen.borrow_mut() = Some(PathBuf::from(line.trim_start_matches("HGS_MODEL_SERVER_ENV=")));
                Ok(())
            });
            assert!(kept.is_ok());
            assert!(seen.borrow().as_ref().unwrap().is_file(), "a successful start leaves the file for hgs --run");
            // No secrets mean no file and the original spec goes through.
            let untouched: Result<()> = launch_with_secrets(&spec, &[], |launch| {
                assert!(!launch.env.iter().any(|line| line.starts_with("HGS_MODEL_SERVER_ENV=")));
                Ok(())
            });
            assert!(untouched.is_ok());
        });
    }

    #[test]
    fn a_transcript_behind_the_events_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"user\",\"timestamp\":\"2026-10-26T00:10:00Z\"}\n",
                "{\"type\":\"assistant\",\"timestamp\":\"2026-10-26T00:11:00Z\"}\n",
                "{\"type\":\"system\",\"timestamp\":\"2026-10-26T04:11:00Z\"}\n"
            ),
        )
        .unwrap();
        let at = transcript_last_turn(&path).unwrap();
        let expected = super::super::search::timestamp(&serde_json::json!("2026-10-26T00:11:00Z"));
        assert!((at - expected).abs() < 1.0, "{at} {expected}");
        assert!(transcript_lag(at, expected + 3600.0).is_some());
        assert!(transcript_lag(at, expected + 60.0).is_none());
        assert!(transcript_lag(0.0, expected + 3600.0).is_none());
        assert!(transcript_last_turn(&dir.path().join("missing.jsonl")).is_none());
    }

    #[test]
    fn the_lost_dialogue_comes_from_the_events_index() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("events.sqlite3");
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection
            .execute_batch("CREATE TABLE events (seq INTEGER PRIMARY KEY, name TEXT, conversation TEXT, payload TEXT);")
            .unwrap();
        let insert = |seq: i64, at: f64, kind: &str, detail: &str, origin: &str| {
            let payload = serde_json::json!({"type":kind,"at":at,"detail":detail,"origin":origin}).to_string();
            connection
                .execute(
                    "INSERT INTO events (seq,name,conversation,payload) VALUES (?1,'claude/x/dev','c',?2)",
                    rusqlite::params![seq, payload],
                )
                .unwrap();
        };
        insert(1, 50.0, "UserPromptSubmit", "old", "");
        insert(2, 200.0, "UserPromptSubmit", "keep going", "");
        insert(3, 210.0, "Stop", "done", "");
        insert(4, 220.0, "UserPromptSubmit", "noise", "task_notification");
        insert(5, 230.0, "PreToolUse", "tool", "");
        let (events, cut) = read_events(&db, "claude/x/dev", "c", 100.0);
        assert_eq!((events.len(), cut), (2, 0), "{events:?}");
        assert_eq!((events[0]["detail"].as_str(), events[1]["detail"].as_str()), (Some("keep going"), Some("done")));
        let path = write_recovery(dir.path(), "c", &events, cut).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("keep going") && text.contains("# Dialogue Zerus recorded"), "{text}");
        assert!(recovery_message(120.0, &path).contains("Your transcript stops at 120"), "{}", recovery_message(120.0, &path));
        assert!(read_events(&db, "claude/x/other", "c", 100.0).0.is_empty());
        assert!(read_events(&dir.path().join("missing.sqlite3"), "claude/x/dev", "c", 0.0).0.is_empty());
        // N4: a failed recovery message names the file and the reason.
        let warning = recovery_send_failure("claude/x/dev", &path, "hgs send exited with exit status: 1");
        assert!(warning.contains("claude/x/dev") && warning.contains(&path.display().to_string()), "{warning}");
        assert!(warning.contains("read") && warning.contains("hgs send exited"), "{warning}");
    }

    #[test]
    fn the_lost_dialogue_keeps_the_newest_entries_of_the_right_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("events.sqlite3");
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection
            .execute_batch("CREATE TABLE events (seq INTEGER PRIMARY KEY, name TEXT, conversation TEXT, payload TEXT);")
            .unwrap();
        let insert = |seq: i64, conversation: &str, at: f64, detail: &str| {
            let payload = serde_json::json!({"type":"UserPromptSubmit","at":at,"detail":detail}).to_string();
            connection
                .execute(
                    "INSERT INTO events (seq,name,conversation,payload) VALUES (?1,'claude/x/dev',?2,?3)",
                    rusqlite::params![seq, conversation, payload],
                )
                .unwrap();
        };
        // N2: the same session name ran another conversation later; its rows
        // must not leak into this conversation's recovery.
        insert(1, "old", 100.0, "ancient");
        insert(2, "current", 200.0, "first");
        insert(3, "other", 300.0, "not mine");
        insert(4, "current", 400.0, "second");
        let (events, cut) = read_events(&db, "claude/x/dev", "current", 50.0);
        assert_eq!(events.iter().map(|event| event["detail"].as_str().unwrap()).collect::<Vec<_>>(), vec!["first", "second"]);
        assert_eq!(cut, 0);
        // An old record without a conversation id keeps the name-only read.
        assert_eq!(read_events(&db, "claude/x/dev", "", 50.0).0.len(), 4);
        // N3: more than 400 entries keep the newest ones, and the note counts
        // the oldest that were left out.
        let db = dir.path().join("many.sqlite3");
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection
            .execute_batch("CREATE TABLE events (seq INTEGER PRIMARY KEY, name TEXT, conversation TEXT, payload TEXT);")
            .unwrap();
        for index in 0..450 {
            let payload = serde_json::json!({"type":"UserPromptSubmit","at":index as f64 + 1.0,"detail":format!("entry {index}")}).to_string();
            connection
                .execute(
                    "INSERT INTO events (seq,name,conversation,payload) VALUES (?1,'claude/x/dev','c',?2)",
                    rusqlite::params![index + 1, payload],
                )
                .unwrap();
        }
        let (events, cut) = read_events(&db, "claude/x/dev", "c", 0.0);
        assert_eq!((events.len(), cut), (400, 50));
        assert_eq!(events.first().map(|event| event["detail"].as_str().unwrap()), Some("entry 50"));
        assert_eq!(events.last().map(|event| event["detail"].as_str().unwrap()), Some("entry 449"));
        let path = write_recovery(dir.path(), "c", &events, cut).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("entry 449") && !text.contains("entry 49\n"), "{text}");
        assert!(text.contains("[cut: the oldest 50 entries are not shown]"), "{text}");
        // The 400 KiB budget also keeps the newest entries and says how many
        // were cut.
        let large: Vec<serde_json::Value> = (0..40)
            .map(|index| serde_json::json!({"type":"Stop","at":index,"detail":"x".repeat(20_000)}))
            .collect();
        let path = write_recovery(&dir.path().join("big"), "c", &large, 0).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.len() <= 400 * 1024 + 200, "{}", text.len());
        assert!(text.contains("[cut: the oldest "), "{text}");
    }
}

/// Replacing a binding cannot drop an unfinished archive/Clear transaction.
/// A resident old finalizer may have archived the successor with both anchors;
/// current restore/resume/fresh must retain that record until exact settlement.
fn refuse_unsettled_clear(record: &Value) -> Result<()> {
    if clear_context::cleanup_pending(record) {
        return Err("Restored Clear archive cleanup is unfinished; retain the binding for review".into());
    }
    Ok(())
}
