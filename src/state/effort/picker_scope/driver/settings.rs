//! Existing Settings requests, one original owner and one immutable raw request.
//! Scheduling grants no key permission; manual pending binds both UUID receipts.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Liveness {
    Running,
    Stopped,
}

pub(super) fn liveness(record: &Value) -> Result<Liveness> {
    if process_alive(record) {
        if !matches(record, panes_of(&live()?, record)) {
            return Err(
                "the tracked terminal identity changed; refresh before changing settings".into(),
            );
        }
        Ok(Liveness::Running)
    } else {
        recipes::argv(record)?;
        Ok(Liveness::Stopped)
    }
}

/// Only this module constructs the closed schedule step. It never contains
/// a native Turn, reservation, key, callback or arbitrary write destination.
pub(super) struct Schedule {
    pub(super) class: Liveness,
    record: Option<Value>,
    receipt: Value,
}
impl Schedule {
    fn new(class: Liveness, record: Option<Value>, receipt: Value) -> Self {
        Self {
            class,
            record,
            receipt,
        }
    }
    pub(super) fn check(
        &self,
        owner: &Arc<owner::Owner>,
        staged: &publication::Staged,
    ) -> Result<(Value, Value)> {
        if owner.control.frame().operation != host::frame::Operation::Settings
            || !owner.native()?
            || owner
                .reservation
                .lock()
                .map_err(|_| "Settings reservation unavailable")?
                .is_some()
        {
            return Err("Schedule fence requires an unreserved original Settings scope".into());
        }
        let (record, version) = owner.snapshot()?;
        if liveness(&record)? != self.class {
            return Err("Settings scheduling liveness changed".into());
        }
        let (after, after_version) = owner.snapshot()?;
        if after != record || after_version != version {
            return Err("Settings source changed during schedule observation".into());
        }
        staged.schedule_content(owner, &record, &version, self.class)?;
        let value = staged.json()?;
        let mut receipt = self.receipt.clone();
        receipt["status"] = value["status"].clone();
        if value != receipt && self.record.as_ref() != Some(&value) {
            return Err("Schedule fence cannot publish content from another step".into());
        }
        owner.check()?;
        Ok((record, version))
    }
}

struct Confirmation {
    request: Request,
    model: ModelOption,
    transcript: Option<(PathBuf, u64)>,
}
impl Confirmation {
    fn observe(&self, owner: &Arc<owner::Owner>, applied: bool) -> Result<()> {
        let record = checked(
            string(&owner.control.frame().binding, "name"),
            &self.request,
            true,
        )?;
        if applied {
            let screen = capture(&record)?;
            if let Some((path, offset)) = &self.transcript {
                let effort = claude_receipt_since(
                    path,
                    &self.request.expected_conversation_id,
                    *offset,
                    &self.model.label,
                )?
                .ok_or("Claude's new session-only settings receipt is not confirmed")?;
                if (!effort.is_empty() && effort != self.request.effort)
                    || claude_footer(&screen).is_some_and(|shown| shown != self.request.effort)
                {
                    return Err("Claude's session settings confirmation changed".into());
                }
            } else if target_footer(&record, &self.model, &screen).as_deref()
                != Some(self.request.effort.as_str())
            {
                return Err("The requested native model and effort are no longer confirmed".into());
            }
        }
        owner.check()
    }
}

fn stage_record(owner: &Arc<owner::Owner>, record: &Value) -> Result<publication::Staged> {
    owner.with_scope(|scope| {
        let staged = publication::Staged::prepare(
            &scope.sessions,
            &scope.record.name,
            format!("{record}\n").as_bytes(),
        )?;
        if staged.expected_version() != Some(&scope.record.version) {
            return Err("Settings record changed before staging".into());
        }
        Ok(staged)
    })
}
fn stage_receipt(
    owner: &Arc<owner::Owner>,
    id: &str,
    value: &Value,
    original: Option<&Original>,
) -> Result<publication::Staged> {
    owner.with_scope(|scope| {
        let directory=if owner.control.frame().operation==host::frame::Operation::Send {
            &scope.send_directories.as_ref().ok_or("Send Settings directory missing")?.0
        } else {&scope.receipts};
        let staged = publication::Staged::prepare(
            directory,
            &format!("{id}.json"),
            value.to_string().as_bytes(),
        )?;
        if staged.expected_version() != original.map(|o| &o.version) {
            return Err("Settings receipt changed before staging".into());
        }
        if let Some(original) = original {
            original.verify(directory)?;
        }
        Ok(staged)
    })
}
fn receipt_commit(
    owner: &Arc<owner::Owner>,
    id: &str,
    staged: publication::Staged,
    fence: publication::CommitFence,
) -> Result<Original> {
    let committed = staged.commit(&fence)?;
    owner.with_scope(|scope| {
        let directory=if owner.control.frame().operation==host::frame::Operation::Send {
            &scope.send_directories.as_ref().ok_or("Send Settings directory missing")?.0
        } else {&scope.receipts};
        committed.receipt(owner,directory,&format!("{id}.json"))
    })
}
fn native_receipt(
    owner: &Arc<owner::Owner>,
    id: &str,
    value: &Value,
    original: Option<&Original>,
    confirmation: &Confirmation,
    applied: bool,
) -> Result<Original> {
    let staged = stage_receipt(owner, id, value, original)?;
    confirmation.observe(owner, applied)?;
    let fence = publication::CommitFence::native(owner, &staged)?;
    receipt_commit(owner, id, staged, fence)
}
fn native_record(
    owner: &Arc<owner::Owner>,
    record: &Value,
    confirmation: &Confirmation,
    applied: bool,
) -> Result<()> {
    let staged = stage_record(owner, record)?;
    confirmation.observe(owner, applied)?;
    let fence = publication::CommitFence::native(owner, &staged)?;
    owner.adopt_record(staged.commit(&fence)?)
}

fn scheduled(
    owner: &Arc<owner::Owner>,
    mut receipt: Value,
    mut record: Value,
    class: Liveness,
    replace: bool,
) -> Result<Vec<u8>> {
    let frame = owner.control.frame();
    receipt["status"] = json!("scheduled");
    receipt["apply_when"] = json!(if class == Liveness::Running {
        "ready"
    } else {
        "resume"
    });
    let changed = if replace {
        record["pending_settings"] = json!({"request_id":frame.request_id,"model":receipt["model"],"effort":receipt["effort"],
            "run_id":record["run_id"],"conversation_id":record["conversation_id"],"receipt_name":record["name"],"receipt_run_id":record["run_id"]});
        record["resume_settings"] = json!({"model":receipt["model"],"effort":receipt["effort"]});
        record["updated"] = json!(now());
        receipt["pending_settings_id"] = json!(frame.request_id);
        Some(stage_record(owner, &record)?)
    } else {
        receipt["pending_settings_id"] = record["pending_settings"]["request_id"].clone();
        None
    };
    // BOTH forward results are staged and fsynced before the first commit.
    // No failure path rolls back a record or overwrites another pending value.
    let witness = Schedule::new(class, replace.then_some(record), receipt.clone());
    let mut final_receipt = stage_receipt(owner, &frame.request_id, &receipt, None)?;
    let mut progress = receipt.clone();
    progress["status"] = json!("in_progress");
    let staged = stage_receipt(owner, &frame.request_id, &progress, None)?;
    let fence = publication::CommitFence::schedule(owner, &staged, &witness)?;
    let original = receipt_commit(owner, &frame.request_id, staged, fence)?;
    if let Some(staged) = changed {
        let fence = publication::CommitFence::schedule(owner, &staged, &witness)?;
        owner.adopt_record(staged.commit(&fence)?)?;
    }
    final_receipt.follow_own_settings_receipt(owner, &original)?;
    let fence = publication::CommitFence::schedule(owner, &final_receipt, &witness)?;
    receipt_commit(owner, &frame.request_id, final_receipt, fence)?;
    Ok(receipt.to_string().into_bytes())
}

pub(super) fn execute(owner: &Arc<owner::Owner>) -> Result<Vec<u8>> {
    let frame = owner.control.frame();
    let request: SettingsRequest = serde_json::from_slice(&frame.raw).map_err(|e| e.to_string())?;
    let name = string(&frame.binding, "name");
    let mut record = read(name)?;
    owner.check()?;
    terminating::check_mutation(&record)?;
    let (model, effort) = requested_model(&record, &request)?;
    owner.check()?;
    if request
        .expected_pending_id
        .as_deref()
        .is_some_and(|id| id != string(&record["pending_settings"], "request_id"))
    {
        return Err("pending settings changed; refresh before applying".into());
    }
    if request.expected_pending_id.is_some()
        && (string(&record["pending_settings"], "model") != model.id
            || string(&record["pending_settings"], "effort") != effort)
    {
        return Err("pending settings target changed; refresh before applying".into());
    }
    let class = liveness(&record)?;
    let receipt = json!({"request_id":frame.request_id,"name":name,"digest":format!("{:x}",Sha256::digest(&frame.raw)),
        "status":"in_progress","run_id":request.expected_run_id,"conversation_id":request.expected_conversation_id,
        "model":model.id,"effort":effort,"scope":"session","at":now()});
    let mut pending = if let Some(id) = &request.expected_pending_id {
        let original =
            owner.with_scope(|scope| Original::open(&scope.receipts, format!("{id}.json")))?;
        let value = owner.with_scope(|scope| original.record(&scope.receipts))?;
        if value["request_id"] != *id
            || value["model"] != model.id
            || value["effort"] != effort
            || value["status"] != "scheduled"
            || string(&value, "name") != string(&record["pending_settings"], "receipt_name")
            || string(&value, "run_id") != string(&record["pending_settings"], "receipt_run_id")
            || string(&value, "conversation_id") != request.expected_conversation_id
            || string(&record["pending_settings"], "run_id") != request.expected_run_id
            || string(&record["pending_settings"], "conversation_id")
                != request.expected_conversation_id
        {
            return Err("pending settings are not safe to retry; inspect Terminal".into());
        }
        Some((
            id.clone(),
            original,
            value,
            record["pending_settings"].clone(),
        ))
    } else {
        None
    };
    if !applies_now(&record, class == Liveness::Running) {
        return scheduled(
            owner,
            receipt,
            record,
            class,
            request.expected_pending_id.is_none(),
        );
    }
    let native = Request {
        request_id: frame.request_id.clone(),
        expected_run_id: request.expected_run_id.clone(),
        expected_conversation_id: request.expected_conversation_id.clone(),
        effort,
    };
    // A fresh Claude /model command itself creates the first native transcript.
    // Never infer its current model/effort from the static startup banner.
    let transcript = if string(&record, "agent") == "claude" {
        let path = claude_transcript(&record)?;
        let offset = fs::metadata(&path).map_or(0, |m| m.len());
        Some((path, offset))
    } else {
        None
    };
    let confirmation = Confirmation {
        request: native,
        model,
        transcript,
    };
    let original = native_receipt(
        owner,
        &frame.request_id,
        &receipt,
        None,
        &confirmation,
        false,
    )?;
    let inner = if let Some((id, original, mut value, pending_record)) = pending.take() {
        value["status"] = json!("in_progress");
        let next = native_receipt(owner, &id, &value, Some(&original), &confirmation, false)?;
        Some(transaction::Pending {
            id,
            receipt: next,
            value,
            pending: pending_record,
        })
    } else {
        None
    };
    record = owner.snapshot()?.0;
    let prior = record.get("settings_unconfirmed_at").cloned();
    let marker = json!(now());
    record["settings_unconfirmed_at"] = marker.clone();
    record["updated"] = json!(now());
    native_record(owner, &record, &confirmation, false)?;
    let (_, record_version) = owner.snapshot()?;
    let kind = owner.with_scope(|scope| Ok(scope.proof.kind()))?;
    *owner
        .reservation
        .lock()
        .map_err(|_| "Settings reservation unavailable")? = Some(transaction::Reservation {
        id: frame.request_id.clone(),
        digest: format!("{:x}", Sha256::digest(&frame.raw)),
        run: request.expected_run_id,
        conversation: request.expected_conversation_id,
        kind,
        marker,
        prior,
        record_version,
        receipt: original,
        pending: inner,
    });
    start_attempt();
    // Use the SAME trusted native walkers. Every helper/key borrows this owner,
    // and each new key requires its own completed native observation turn.
    let result = apply_native(name, &record, &confirmation);
    if let Err(error) = result {
        return owner.refuse(format!("settings change not confirmed: {error}"));
    }
    record = owner.snapshot()?.0;
    let latest = history(&record);
    record["model"] = json!(confirmation.model.id);
    record["effort"] = json!(confirmation.request.effort);
    record["effort_source_path"] = json!(latest.path);
    record["effort_source_offset"] = json!(latest.offset);
    record["resume_settings"] =
        json!({"model":confirmation.model.id,"effort":confirmation.request.effort});
    record["updated"] = json!(now());
    {
        let reservation = owner
            .reservation
            .lock()
            .map_err(|_| "Settings reservation unavailable")?;
        let held = reservation
            .as_ref()
            .ok_or("Original Settings reservation missing")?;
        if record.get("settings_unconfirmed_at") != Some(&held.marker) {
            return Err("Original Settings marker changed".into());
        }
        if let Some(pending) = &held.pending {
            if record["pending_settings"] != pending.pending {
                return Err("Original pending Settings changed".into());
            }
        }
        // An explicit direct Apply supersedes prior pending settings, as the
        // legacy path does; it can recover a conservatively interrupted schedule.
        record.as_object_mut().unwrap().remove("pending_settings");
        match &held.prior {
            Some(value) => record["settings_unconfirmed_at"] = value.clone(),
            None => {
                record
                    .as_object_mut()
                    .unwrap()
                    .remove("settings_unconfirmed_at");
            }
        }
    }
    native_record(owner, &record, &confirmation, true)?;
    let held = owner
        .reservation
        .lock()
        .map_err(|_| "Settings reservation unavailable")?
        .take()
        .ok_or("Original Settings reservation missing")?;
    if let Some(mut inner) = held.pending {
        inner.value["status"] = json!("applied");
        native_receipt(
            owner,
            &inner.id,
            &inner.value,
            Some(&inner.receipt),
            &confirmation,
            true,
        )?;
    }
    let mut receipt = receipt;
    receipt["status"] = json!("applied");
    native_receipt(
        owner,
        &frame.request_id,
        &receipt,
        Some(&held.receipt),
        &confirmation,
        true,
    )?;
    Ok(receipt.to_string().into_bytes())
}

/// A nested step consumes a closed parent capability. It never executes or
/// schedules a Settings frame and never obtains another writer or deadline.
pub(super) fn apply_pending_step(owner: &Arc<owner::Owner>, step: send::PendingStep) -> Result<()> {
    let request=step.request(owner)?;
    let mut record=owner.snapshot()?.0;
    let (model,effort)=requested_model(&record,&request)?;
    if !applies_now(&record,liveness(&record)?==Liveness::Running) {
        return Err("Pending Settings is no longer eligible before this message".into());
    }
    let id=request.expected_pending_id.as_deref().ok_or("Pending Settings identity missing")?;
    let original=step.original(owner)?;
    let mut value=step.value();
    value["status"]=json!("in_progress");
    let staged=stage_receipt(owner,id,&value,Some(original))?;
    let witness=send::Publication::pending_receipt(owner,&staged,id)?;
    let fence=publication::CommitFence::send(owner,&staged,witness)?;
    let original=receipt_commit(owner,id,staged,fence)?;
    let prior=record.get("settings_unconfirmed_at").cloned();
    let marker=json!(now());
    record["settings_unconfirmed_at"]=marker.clone();record["updated"]=json!(now());
    let staged=stage_record(owner,&record)?;
    let witness=send::Publication::pending_marker(owner,&staged)?;
    let fence=publication::CommitFence::send(owner,&staged,witness)?;
    owner.adopt_record(staged.commit(&fence)?)?;
    step.activate(owner,original,value.clone(),marker,prior)?;
    let native=Request {request_id:request.request_id.clone(),expected_run_id:request.expected_run_id.clone(),expected_conversation_id:request.expected_conversation_id.clone(),effort};
    let transcript=if string(&record,"agent")=="claude" {
        let path=claude_transcript(&record)?;let offset=fs::metadata(&path).map_or(0,|m|m.len());Some((path,offset))
    } else {None};
    let confirmation=Confirmation {request:native,model,transcript};
    start_attempt();
    let name=string(&owner.control.frame().binding,"name");
    apply_native(name,&record,&confirmation).map_err(|e|format!("settings change not confirmed: {e}"))?;
    record=owner.snapshot()?.0;
    let latest=history(&record);
    record["model"]=json!(confirmation.model.id);record["effort"]=json!(confirmation.request.effort);
    record["effort_source_path"]=json!(latest.path);record["effort_source_offset"]=json!(latest.offset);
    record["resume_settings"]=json!({"model":confirmation.model.id,"effort":confirmation.request.effort});
    record["updated"]=json!(now());
    send::settings_key_ready(owner)?;
    record.as_object_mut().ok_or("Invalid pending Settings record")?.remove("pending_settings");
    match send::child_prior(owner)? {
        Some(prior)=>record["settings_unconfirmed_at"]=prior,
        None=>{record.as_object_mut().ok_or("Invalid pending Settings record")?.remove("settings_unconfirmed_at");},
    }
    native_record(owner,&record,&confirmation,true)?;
    send::child_record_committed(owner)?;
    value["status"]=json!("applied");
    let original=owner.with_scope(|scope| {
        Original::open(&scope.send_directories.as_ref().ok_or("Send Settings directory missing")?.0,format!("{id}.json"))
    })?;
    let applied=native_receipt(owner,id,&value,Some(&original),&confirmation,true)?;
    send::finish_pending(owner,applied,value)
}

fn apply_native(name: &str, record: &Value, confirmation: &Confirmation) -> Result<()> {
    let current = settings(record, true);
    match string(record, "agent") {
        "claude" => claude_model(name, &confirmation.request, &confirmation.model),
        "codex" | "kimi" => {
            let screen = capture(record)?;
            let visible = target_footer(record, &confirmation.model, &screen);
            if visible.as_deref() == Some(confirmation.request.effort.as_str()) {
                Ok(())
            } else if current.model != confirmation.model.id
                || visible.is_none()
                || (string(record, "agent") == "codex" && confirmation.request.effort == "ultra")
            {
                if string(record, "agent") == "codex" {
                    codex_model(name, &confirmation.request, &confirmation.model)
                } else {
                    kimi_model(name, &confirmation.request, &confirmation.model)
                }
            } else {
                let current = Settings {
                    model: confirmation.model.label.clone(),
                    effort: visible.unwrap_or(current.effort),
                    ..current
                };
                if string(record, "agent") == "codex" {
                    codex_change(
                        name,
                        &confirmation.request,
                        &current,
                        &confirmation.model.efforts,
                    )
                } else {
                    kimi_change(name, &confirmation.request, &current)
                }
            }
        }
        _ => Err("unsupported model adapter".into()),
    }
}

#[cfg(test)]
mod tests;
