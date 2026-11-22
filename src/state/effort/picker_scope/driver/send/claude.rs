//! Shared Claude settle/proof under the original pending-Send owner.
use super::*;
use input::settle;

pub(in crate::state::effort::picker_scope::driver) fn owned_marker(
    owner: &owner::Owner,
    record: &Value,
) -> Result<()> {
    let h = header(owner)?;
    with(owner, |held| {
        if h.agent != "claude"
            || owner.control.frame().operation != host::frame::Operation::Send
            || !matches!(held.phase, Phase::Pasted | Phase::Entered)
            || held.child.is_some()
            || !held.progress.paste_success.load(Ordering::Acquire)
            || record["input_pending_request"] != h.request_id
            || held
                .marker
                .as_ref()
                .is_none_or(|(at, _)| record["input_pending_at"] != *at)
            || record["input_typing"]["request_id"] != h.request_id
            || record.get("pending_settings").is_some()
        {
            return Err("Original Claude Send request or typing marker changed".into());
        }
        // The prompt's own hook supersedes a completed continuation after
        // Enter. Before Enter it must still be the original idle continuation.
        if held.phase == Phase::Pasted {
            compact_context::check_continuation(record, &h.compaction)?;
        }
        terminating::check_mutation(record)
    })
}

pub(in crate::state::effort::picker_scope::driver) fn check_marker(
    owner: &owner::Owner,
    record: &Value,
) -> Result<()> {
    owned_marker(owner, record)?;
    if input::input_typing(record) != Some(header(owner)?.request_id.as_str()) {
        return Err("Original Claude Send typing marker expired or changed".into());
    }
    Ok(())
}

pub(in crate::state::effort::picker_scope::driver) fn adopt_version(
    owner: &owner::Owner,
    record: &Value,
    version: &Value,
) -> Result<()> {
    check_marker(owner, record)?;
    with(owner, |held| {
        held.marker = Some((record["input_pending_at"].clone(), version.clone()));
        Ok(())
    })
}

pub(in crate::state::effort::picker_scope::driver) fn check_publication(
    owner: &Arc<owner::Owner>,
    staged: &publication::Staged,
    directory: &Path,
    name: &str,
    record: &Value,
) -> Result<()> {
    let proposed = staged.json()?;
    let h = header(owner)?;
    owner.with_scope(|scope| {
        let (mut before, mut after, fields) =
            if directory == scope.sessions.path && name == scope.record.name {
                (
                    record.clone(),
                    proposed,
                    &["input_typing", "input_seen_at", "updated"][..],
                )
            } else if directory == scope.receipts.path && name == format!("{}.json", h.request_id) {
                if proposed["status"] != "in_progress" || proposed["delivery"] != "uncertain" {
                    return Err("Post-paste publication cannot claim submission".into());
                }
                (
                    with(owner, |held| Ok(held.value.clone()))?,
                    proposed,
                    &["delivery", "uncertain_reason", "composer"][..],
                )
            } else {
                return Err("Post-paste publication has no original Send destination".into());
            };
        for key in fields {
            before
                .as_object_mut()
                .ok_or("Invalid original Send JSON")?
                .remove(*key);
            after
                .as_object_mut()
                .ok_or("Invalid proposed Send JSON")?
                .remove(*key);
        }
        if before != after {
            return Err("Post-paste publication changed another field".into());
        }
        Ok(())
    })
}

fn record_write(owner: &Arc<owner::Owner>, record: Value) -> Result<()> {
    let (staged, directory, name) = owner.with_scope(|scope| {
        let staged = publication::Staged::prepare(
            &scope.sessions,
            &scope.record.name,
            format!("{record}\n").as_bytes(),
        )?;
        if staged.expected_version() != Some(&scope.record.version) {
            return Err("Original Send typing source changed".into());
        }
        Ok((
            staged,
            scope.sessions.path.clone(),
            scope.record.name.clone(),
        ))
    })?;
    let step = Publication::after_input(owner, &staged, directory, name)?;
    let fence = publication::CommitFence::send(owner, &staged, step)?;
    owner.adopt_record(staged.commit(&fence)?)?;
    let (record, version) = owner.snapshot()?;
    with(owner, |held| {
        held.marker = Some((record["input_pending_at"].clone(), version));
        Ok(())
    })
}

fn retire(owner: &Arc<owner::Owner>, recorded: bool) -> Result<()> {
    let mut record = owner.snapshot()?.0;
    check_marker(owner, &record)?;
    // Exact request AND timestamp: another message's proof cannot clear it.
    if recorded {
        record["input_seen_at"] = json!(now());
    }
    record
        .as_object_mut()
        .ok_or("Invalid original Send record")?
        .remove("input_typing");
    record["updated"] = json!(now());
    record_write(owner, record)
}

pub(in crate::state::effort::picker_scope::driver) fn submit(
    owner: &Arc<owner::Owner>,
    progress: &Arc<Progress>,
    transcript: Option<&mut settle::Transcript>,
    text: &str,
    pasted_at: f64,
) -> Result<()> {
    let deadline = (Instant::now() + settle::WINDOW).min(owner.until);
    with(owner, |held| {
        held.enter_deadline = Some(deadline);
        Ok(())
    })?;
    let mut record = owner.snapshot()?.0;
    owned_marker(owner, &record)?;
    // Root has remained held since marking. Start the typing term at the end
    // of the paste, without renewing either the window or original owner.
    record["input_typing"]["until"] = json!(now() + input::TYPING_SECONDS);
    record_write(owner, record.clone())?;
    let claim = backend::InputClaim {
        pane: string(&record, "pane"),
        name: terminal_name(&record),
        run_id: string(&record, "run_id"),
        agent: "claude",
        pid: record["pid"].as_u64(),
    };
    let terminal = backend::current();
    // The exact bytes pasted: a composer taller than its view is proved
    // against them (zerus-33caz).
    let pasted = String::from_utf8(super::pasted(owner)?).map_err(|_| "Original Send text is not UTF-8")?;
    owner.release_send_root()?;
    loop {
        let shown = settle::settle_complete(terminal, &claim, &pasted, deadline)
            .map_err(|_| "Claude's composer never settled; no Enter was pressed")?;
        owner.retake_send_root(deadline)?;
        check_marker(owner, &owner.snapshot()?.0)?;
        observe(owner, context::Kind::SendAfterPaste)?;
        if settle::still_complete(terminal, &claim, &pasted, &shown) {
            check_time(deadline)?;
            send_helpers::enter(owner, progress.clone())?;
            break;
        }
        owner.release_send_root()?;
    }
    owner.release_send_root()?;
    let proof = settle::confirm(terminal, &claim, transcript, text, pasted_at, deadline)
        .map_err(|_| "Claude recorded no submission of this message")?;
    if !proof.recorded() {
        return Err("Claude recorded no typed message or new enqueue for this send".into());
    }
    owner.retake_send_root(deadline)?;
    check_marker(owner, &owner.snapshot()?.0)?;
    progress.confirmed.store(true, Ordering::Release);
    retire(owner, true)?;
    with(owner, |held| {
        held.value["native"] = json!(proof.name());
        Ok(())
    })
}

/// Preserve the original UUID/payload as uncertain; never retry a paste or key.
/// A refused/expired/replaced source keeps its original in-progress receipt.
pub(in crate::state::effort::picker_scope::driver) fn uncertain(
    owner: &Arc<owner::Owner>,
    reason: &str,
) {
    let _ = (|| -> Result<()> {
        if !owner.root_held()? {
            owner.retake_send_root(owner.until)?;
        }
        let record = owner.snapshot()?.0;
        check_marker(owner, &record)?;
        let claim = backend::InputClaim {
            pane: string(&record, "pane"),
            name: terminal_name(&record),
            run_id: string(&record, "run_id"),
            agent: "claude",
            pid: record["pid"].as_u64(),
        };
        let composer = settle::residue(backend::current(), &claim);
        let mut receipt = with(owner, |held| Ok(held.value.clone()))?;
        receipt["delivery"] = json!("uncertain");
        receipt["uncertain_reason"] = json!(input::native_send::bounded_error(reason.into()));
        receipt["composer"] = json!(composer);
        receipt_write(owner, receipt, false)?;
        retire(owner, false)
    })();
}
