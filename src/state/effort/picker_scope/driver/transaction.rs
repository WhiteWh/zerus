//! The existing Effort transaction, with its original raw request/receipt and
//! one owned ID input scope. Settings and legacy controllers remain separate.
use super::*;

pub(super) struct Pending {
    pub(super) id: String,
    pub(super) receipt: Original,
    pub(super) value: Value,
    pub(super) pending: Value,
}

pub(super) struct Reservation {
    pub(super) id: String,
    pub(super) digest: String,
    pub(super) run: String,
    pub(super) conversation: String,
    pub(super) kind: InputKind,
    pub(super) marker: Value,
    pub(super) prior: Option<Value>,
    pub(super) record_version: Value,
    pub(super) receipt: Original,
    pub(super) pending: Option<Pending>,
}

fn confirm(owner: &Arc<owner::Owner>, request: &Request, expected: &Settings) -> Result<()> {
    let record = checked(
        string(&owner.control.frame().binding, "name"),
        request,
        true,
    )?;
    if visible_effort(&record, &expected.model, &capture(&record)?).as_deref()
        != Some(expected.effort.as_str())
    {
        return Err("The original provider effort is no longer confirmed; inspect Terminal".into());
    }
    owner.check()
}

fn record_write(
    owner: &Arc<owner::Owner>,
    request: &Request,
    record: &Value,
    expected: &Settings,
) -> Result<()> {
    let staged = owner.with_scope(|scope| {
        let staged = publication::Staged::prepare(
            &scope.sessions,
            &scope.record.name,
            format!("{record}\n").as_bytes(),
        )?;
        if staged.expected_version() != Some(&scope.record.version) {
            return Err("Effort record changed before staging".into());
        }
        Ok(staged)
    })?;
    confirm(owner, request, expected)?;
    let fence = publication::CommitFence::native(owner, &staged)?;
    let committed = staged.commit(&fence)?;
    owner.adopt_record(committed)
}

fn receipt_write(
    owner: &Arc<owner::Owner>,
    request: &Request,
    receipt: &Value,
    expected: &Settings,
    original: Option<&Original>,
) -> Result<Original> {
    let name = format!("{}.json", owner.control.frame().request_id);
    let staged = owner.with_scope(|scope| {
        let staged =
            publication::Staged::prepare(&scope.receipts, &name, receipt.to_string().as_bytes())?;
        if staged.expected_version() != original.map(|receipt| &receipt.version) {
            return Err("Effort request receipt changed before staging".into());
        }
        if let Some(original) = original {
            original.verify(&scope.receipts)?;
        }
        Ok(staged)
    })?;
    confirm(owner, request, expected)?;
    let fence = publication::CommitFence::native(owner, &staged)?;
    let committed = staged.commit(&fence)?;
    owner.with_scope(|scope| committed.receipt(owner, &scope.receipts, &name))
}

/// Only this exact in-memory reservation may bypass its own conservative
/// timestamp. A timestamp alone never names a request or blocks every UUID.
pub(super) fn own_marker(record: &Value) -> Result<bool> {
    let Some(owner) = context::owner()? else {
        return Ok(false);
    };
    owner.actor()?;
    if owner.control.frame().operation == host::frame::Operation::Send {
        return send::own_marker(&owner,record);
    }
    let reservation = owner
        .reservation
        .lock()
        .map_err(|_| "Effort reservation unavailable")?;
    let Some(reserved) = reservation.as_ref() else {
        return Ok(false);
    };
    owner.with_scope(|scope| {
        let raw = scope.current()?;
        let frame = owner.control.frame();
        if reserved.id != frame.request_id
            || reserved.digest != format!("{:x}", Sha256::digest(&frame.raw))
            || reserved.run != string(&raw, "run_id")
            || reserved.conversation != string(&raw, "conversation_id")
            || reserved.kind != scope.proof.kind()
            || reserved.record_version != scope.record.version
            || raw.get("settings_unconfirmed_at") != Some(&reserved.marker)
            || record.get("settings_unconfirmed_at") != Some(&reserved.marker)
            || !same_target(&raw, record)
        {
            return Err("Original Effort reservation no longer matches its own marker".into());
        }
        let receipt = reserved.receipt.record(&scope.receipts)?;
        if receipt["status"] != "in_progress"
            || receipt["request_id"] != reserved.id
            || receipt["digest"] != reserved.digest
            || receipt["name"] != raw["name"]
            || receipt["run_id"] != reserved.run
            || receipt["conversation_id"] != reserved.conversation
        {
            return Err("Original Effort reservation receipt changed".into());
        }
        if let Some(pending) = &reserved.pending {
            if raw["pending_settings"] != pending.pending
                || string(&pending.pending, "request_id") != pending.id
                || pending.receipt.record(&scope.receipts)? != pending.value
                || pending.value["status"] != "in_progress"
            {
                return Err("Original pending Settings reservation changed".into());
            }
        }
        Ok(true)
    })
}

pub(super) fn key_ready(owner: &Arc<owner::Owner>) -> Result<()> {
    if owner.control.frame().operation == host::frame::Operation::Send {
        return send::settings_key_ready(owner);
    }
    let raw = owner.snapshot()?.0;
    if !own_marker(&raw)? {
        return owner.refuse("Effort key has no original settled reservation".into());
    }
    Ok(())
}

pub(super) fn execute(owner: &Arc<owner::Owner>) -> Result<Vec<u8>> {
    let frame = owner.control.frame();
    let request: Request = serde_json::from_slice(&frame.raw).map_err(|e| e.to_string())?;
    let name = string(&frame.binding, "name");
    let initial = checked(name, &request, true)?;
    let current = settings(&initial, true);
    if visible_effort(&initial, &current.model, &capture(&initial)?).as_deref()
        != Some(current.effort.as_str())
    {
        return Err("the current provider effort is not visible; open Terminal".into());
    }
    let mut levels = options(&initial, &current.model);
    if string(&initial, "agent") == "codex" && request.effort == "ultra" {
        return Err("Ultra requires the explicit advanced reasoning picker in Terminal".into());
    }
    if levels.len() > 16 || !levels.contains(&request.effort) || current.effort.is_empty() {
        return Err("effort is unavailable for the active model; refresh or use Terminal".into());
    }
    if string(&initial, "agent") == "codex" {
        levels.sort_by_key(|value| match value.as_str() {
            "max" => 1,
            "ultra" => 2,
            _ => 0,
        });
    }
    let digest = format!("{:x}", Sha256::digest(&frame.raw));
    let mut receipt = json!({"request_id":frame.request_id,"name":name,"digest":digest,
        "status":"in_progress","run_id":request.expected_run_id,
        "conversation_id":request.expected_conversation_id,"effort":request.effort,
        "model":current.model,"scope":"session","at":now()});
    let original_receipt = receipt_write(owner, &request, &receipt, &current, None)?;
    let (mut marked, _) = owner.snapshot()?;
    let prior = marked.get("settings_unconfirmed_at").cloned();
    let marker = json!(now());
    marked["settings_unconfirmed_at"] = marker.clone();
    marked["updated"] = json!(now());
    record_write(owner, &request, &marked, &current)?;
    let (_, record_version) = owner.snapshot()?;
    let kind = owner.with_scope(|scope| Ok(scope.proof.kind()))?;
    *owner
        .reservation
        .lock()
        .map_err(|_| "Effort reservation unavailable")? = Some(Reservation {
        id: frame.request_id.clone(),
        digest,
        run: request.expected_run_id.clone(),
        conversation: request.expected_conversation_id.clone(),
        kind,
        marker,
        prior,
        record_version,
        receipt: original_receipt,
        pending: None,
    });
    start_attempt();
    let result = if current.effort == request.effort {
        Ok(())
    } else {
        match string(&initial, "agent") {
            "codex" => codex_change(name, &request, &current, &levels),
            "kimi" => kimi_change(name, &request, &current),
            "claude" => claude_change(name, &request, &current),
            _ => Err("change this provider's effort in Terminal".into()),
        }
    };
    if let Err(error) = result {
        // The durable original UUID/marker remain conservative. No terminal
        // probe, rollback, Escape or failure write is authorized by this error.
        return owner.refuse(format!("effort change not confirmed: {error}"));
    }
    let expected = Settings {
        effort: request.effort.clone(),
        ..current.clone()
    };
    let mut record = owner.snapshot()?.0;
    let latest = history(&record);
    record["effort"] = json!(request.effort);
    record["model"] = json!(current.model);
    record["effort_source_path"] = json!(latest.path);
    record["effort_source_offset"] = json!(latest.offset);
    record["updated"] = json!(now());
    {
        let reservation = owner
            .reservation
            .lock()
            .map_err(|_| "Effort reservation unavailable")?;
        let reserved = reservation
            .as_ref()
            .ok_or("Original Effort reservation missing")?;
        if record.get("settings_unconfirmed_at") != Some(&reserved.marker) {
            return Err("Original Effort marker changed before confirmation".into());
        }
        match &reserved.prior {
            Some(value) => record["settings_unconfirmed_at"] = value.clone(),
            None => {
                record
                    .as_object_mut()
                    .ok_or("Invalid Effort record")?
                    .remove("settings_unconfirmed_at");
            }
        }
    }
    record_write(owner, &request, &record, &expected)?;
    receipt["status"] = json!("confirmed");
    // Detach only the original data proof to avoid holding the reservation
    // mutex during fresh checked_settings/checked_terminal native callbacks.
    let original = owner
        .reservation
        .lock()
        .map_err(|_| "Effort reservation unavailable")?
        .take()
        .ok_or("Original Effort reservation missing")?
        .receipt;
    receipt_write(owner, &request, &receipt, &expected, Some(&original))?;
    Ok(receipt.to_string().into_bytes())
}
