//! Task discussion outbox. Each request is durably reserved before the only
//! native submission; repeating it only reads its receipt, including after loss
//! of an SSH response. This is not an execution scheduler or a distributed lease.
use super::*;
use zerus_replica::TaskDelivery;

pub(super) fn execute(config: &Config, args: &[String]) -> Result<Value> {
    // A reopen's attempt has one budget from here (C3 A3).
    let deadline = std::time::Instant::now() + super::attempts::BUDGET;
    let content = stdin_content(args)?;
    let files = content.attachments.unwrap_or_default();
    let id =
        option(args, "--request-id")?.ok_or_else(|| fail("message needs --request-id UUID"))?;
    let mode = option(args, "--mode")?
        .ok_or_else(|| fail("message needs --mode queue|send|reopen|note"))?;
    let dispatch = option(args, "--expect-dispatch")?.unwrap_or_default();
    // A reopen reads the catalog and waits for the project within its one
    // budget, from here (#1519 review, zerus-9gzdp).
    let reopen = mode == "reopen";
    let (machines, projects) = if reopen {
        let (machines, projects) = super::entry_until(config, deadline)?;
        (machines, Some(projects))
    } else {
        (machines(config)?, None)
    };
    let who = actor(&machines.here, args, true)?;
    let r = super::reference_by(
        config,
        args,
        &who,
        projects.as_deref().map(|p| (p, deadline)),
    )?;
    let mut store = if reopen {
        super::store_until(config, &r.project.0, deadline)?
    } else {
        store(config, &r.project.0)?
    };
    let mut task = store.find(&r.task).map_err(fail)?;
    let mut before = task.view().map_err(fail)?;
    let existing = before
        .log
        .iter()
        .find(|e| e.kind == "message" && e.request_id.as_deref() == Some(id))
        .cloned();
    let recovering = existing.is_some();
    let missing_recovery = !recovering && flag(args, "--recover-only");
    let mut routing_error = None;
    let to = if matches!(mode, "send" | "reopen") {
        let key = existing
            .as_ref()
            .and_then(|e| e.session.clone())
            .or(before.session.clone());
        // An existing reservation retains the original name/run, including after
        // handoff. Recovery must read that receipt, never retarget the message.
        let resolved = key.and_then(|key| if recovering { Some(key) } else { machines.resolve(&key) });
        if resolved.is_none() { routing_error = Some("The task has no verified current executor. The comment is saved here; choose an executor and start the task to send feedback.".to_string()); }
        resolved
    } else {
        None
    };
    if let Some(to) = to.as_ref().filter(|to| to.machine != machines.here) {
        let uuid = task.uuid().to_owned();
        drop(store);
        let alias = reach(config, &machines, &r.project.0, to).map_err(fail)?;
        let mut remote = vec![
            "task".into(),
            "message".into(),
            uuid.clone(),
            "--project".into(),
            r.project.0.clone(),
            "--request-id".into(),
            id.into(),
            "--mode".into(),
            mode.into(),
            "--expect-dispatch".into(),
            dispatch.into(),
            "--content-json".into(),
            "--json".into(),
        ];
        if let Actor::Agent(own) = &who {
            remote.extend([
                "--agent".into(),
                format!("{}/{}", own.machine, own.session),
                "--agent-session-id".into(),
                own.session_id.clone(),
            ]);
        }
        if flag(args, "--recover-only") {
            remote.push("--recover-only".into());
        }
        let reply = crate::swarm::remote_hgs_input(
            config,
            &alias,
            &remote,
            &json!({"text":content.text,"attachments":files}),
        )
        .map_err(fail)?;
        let synced = crate::swarm::exchange_tasks(config, &alias, &r.project.0).is_ok();
        let mut out = json!({"task":reply["task"],"message":reply["message"],"synced":synced});
        // A managed reopen's outcome, as the executor's computer settled it
        // (zerus-10gmu).
        if let Some(attempt) = reply.get("attempt") {
            out["attempt"] = attempt.clone();
        }
        return Ok(out);
    }
    // C3 activation A3 (desktop-u's (b)): a reopen is an attempt. An ID that
    // names one is only recovered; a fresh reopen goes through the fence,
    // and only its explicit Legacy keeps the message path below.
    if mode == "reopen" {
        if task.attempt(id).map_err(fail)?.is_some() {
            // No lock is held into its settlement.
            drop(store);
            return super::attempts::reopen_again(
                config,
                &machines,
                &r.project,
                &task,
                id,
                &content.text,
                &files,
            );
        }
        let fresh = existing.is_none() && !missing_recovery && routing_error.is_none();
        // A request that cannot even be prepared sends nothing: the message
        // path below keeps the comment as not delivered, as before.
        let frozen = to.as_ref().filter(|_| fresh).and_then(|to| {
            super::attempts::reopen_request(
                &r.project.1,
                &before,
                id,
                &content.text,
                &files,
                to,
                dispatch,
            )
            .ok()
            .map(|frozen| (to, frozen))
        });
        if let Some((to, frozen)) = frozen {
            let uuid = task.uuid().to_owned();
            drop(store);
            let reopened = super::attempts::reopen(
                config,
                &machines,
                &r.project,
                &uuid,
                id,
                &content.text,
                &files,
                dispatch,
                to,
                &frozen,
                (&before.target, before.stage_at),
                &who,
                deadline,
            )?;
            if let Some(out) = reopened {
                return Ok(out);
            }
            // Legacy: read again; the message path below rechecks it all,
            // and refuses a task that is managed by now.
            store = super::store(config, &r.project.0)?;
            task = store.load(&uuid).map_err(fail)?;
            before = task.view().map_err(fail)?;
        }
    }
    let mut delivery = TaskDelivery {
        mode: mode.into(),
        stage: before.stage.as_str().into(),
        stage_at: before.stage_at,
        state: match mode {
            "queue" => "queued",
            "note" => "note",
            _ => "reserved",
        }
        .into(),
        dispatch_id: dispatch.into(),
        submitted_id: String::new(),
        run_id: String::new(),
        conversation_id: String::new(),
        prompt: String::new(),
        error: String::new(),
    };
    if missing_recovery {
        delivery.state = "uncertain".into();
        delivery.error = "The previous delivery could not be confirmed. Your comment is kept in this task and was not resent. Check the executor Activity before sending it again.".into();
    } else if let Some(error) = &routing_error {
        refuse_delivery(&mut delivery, error);
    }
    let request = if let Some(existing) = &existing {
        delivery = existing
            .delivery
            .clone()
            .ok_or_else(|| fail("message has no delivery reservation"))?;
        // Compare the caller's original intent even when the current stage moved.
        if delivery.mode != mode || delivery.dispatch_id != dispatch {
            return Err(fail("message identity already belongs to another intent"));
        }
        None
    } else if let Some(to) = to.as_ref().filter(|_| !missing_recovery && routing_error.is_none()) {
        // The project's agent rules, as a send and the managed reopen apply
        // them (zerus-anemc): an agent reopens a task only where it may
        // start one, within the hourly limit. A refusal saves nothing.
        if mode == "reopen" {
            let settings = store.project().map_err(fail)?.settings();
            let starts = if matches!(who, Actor::Agent(_)) && settings.agent_limit > 0 {
                super::agent_starts_since(&store, now() - 3_600_000, deadline)?
            } else {
                0
            };
            settings.check_agent(&who, to, true, starts).map_err(fail)?;
        }
        delivery.prompt = prompt(&r.project.1, &before.id, id, mode, &content.text, before.worker.as_ref());
        // Feedback addresses this captured execution; a reopen's new attempt
        // is handled by attempts::reopen_request before its request is hashed.
        if let Some(origin) = before.execution.as_ref().and_then(|e| e.current.as_ref()).filter(|a| !a.legacy) {
            super::reports::pin(&mut delivery.prompt, &r.project.1, &before.id, &origin.id);
        }
        let attachments = files
            .iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(fail)?;
        match crate::state::prepare_task_reply(to, dispatch, id, &delivery.prompt, &attachments) {
            Ok(request) => {
                delivery.run_id = request["expected_run_id"].as_str().unwrap_or_default().into();
                delivery.conversation_id = request["expected_conversation_id"].as_str().unwrap_or_default().into();
                Some(request)
            },
            Err(error) => { refuse_delivery(&mut delivery, &error); None },
        }
    } else {
        None
    };
    let reserved = task.reserve_message(
            id,
            &content.text,
            &files,
            &delivery,
            to.as_ref(),
            (&who, now()),);
    let fresh = match reserved {
        Ok(fresh) => fresh,
        Err(error) if !recovering && matches!(mode, "send" | "reopen") && delivery.state == "reserved" => {
            refuse_delivery(&mut delivery, &error.to_string());
            task.reserve_message(id, &content.text, &files, &delivery, to.as_ref(), (&who, now())).map_err(fail)?
        },
        Err(error) => return Err(fail(error)),
    };
    // The project lock serializes local reservations. A save failure stops before input.
    store.save(&mut task).map_err(fail)?;
    if let Some(to) = to.as_ref().filter(|_| !delivery.run_id.is_empty() && matches!(delivery.state.as_str(), "reserved" | "uncertain")) {
        let request = request.unwrap_or_else(|| {
            json!({"request_id":id,"text":delivery.prompt,"attachments":files,
            "expected_run_id":delivery.run_id,"expected_conversation_id":delivery.conversation_id})
        });
        if fresh {
            match crate::state::submit_task_reply(&to.session, &request) {
                Ok(_) => task
                    .finish_message(id, "submitted", "", &who, now())
                    .map_err(fail)?,
                Err(error) => task
                    .finish_message(
                        id,
                        if paste_uncertain(&error) {
                            "uncertain"
                        } else {
                            "failed"
                        },
                        &error,
                        &who,
                        now(),
                    )
                    .map_err(fail)?,
            }
        } else if matches!(delivery.state.as_str(), "reserved" | "uncertain") {
            let receipt = crate::state::task_reply_receipt(&to.session, &request).map_err(fail)?;
            if receipt.as_ref().is_some_and(|r| r["status"] == "submitted") {
                task.finish_message(id, "submitted", "", &who, now())
                    .map_err(fail)?;
            } else {
                task.finish_message(id, "uncertain", "No confirmed submission receipt. This message will not be resent automatically.", &who, now()).map_err(fail)?;
            }
        }
        store.save(&mut task).map_err(|e| {
            fail(format!(
                "message outcome could not be saved; check this same request_id: {e}"
            ))
        })?;
    }
    let current = task.view().map_err(fail)?;
    let message = current
        .log
        .iter()
        .find(|e| e.kind == "message" && e.request_id.as_deref() == Some(id));
    Ok(json!({"task":view(&task,&r.project.1,true,&machines)?,"message":message}))
}

fn refuse_delivery(delivery: &mut TaskDelivery, error: &str) {
    delivery.state = "failed".into();
    delivery.error = error.chars().take(1000).collect();
    delivery.run_id.clear(); delivery.conversation_id.clear(); delivery.prompt.clear(); delivery.submitted_id.clear();
}

/// The text a discussion message pastes into its task's session.
pub(super) fn prompt(
    project_name: &str,
    task_id: &str,
    id: &str,
    mode: &str,
    text: &str,
    worker: Option<&zerus_replica::TaskWorker>,
) -> String {
    let worker = worker.map(|w| format!("\nCoordinator: relay this feedback to the registered child {} only if its exact current identity still matches; otherwise explain why it cannot be delivered.", w.id)).unwrap_or_default();
    // The header is a label; the commands name the task as one safe word (zerus-sa9qn.38).
    let reference = zerus_replica::command_reference(project_name, task_id);
    format!("[Zerus task {project_name}/{task_id} message {id}]\n{}\n\n{text}{worker}\n\n{}",
        if mode == "reopen" { "The person requests changes. Continue this task." } else { "The person added task feedback." },
        format_args!("Read the task context with `hgs task get {reference}` and report the result with `hgs task report {reference}`."))
}
