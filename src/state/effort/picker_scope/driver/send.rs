//! One admitted pending Settings step followed by one ordinary message. The
//! parent retains the original Root, SID, frame and deadline throughout.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
pub(super) struct Progress {
    pub(super) stage_started: AtomicBool,
    pub(super) stage_done: AtomicBool,
    pub(super) paste_success: AtomicBool,
    pub(super) paste_possible: AtomicBool,
    pub(super) enter_settled: AtomicBool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Prepared,
    Child,
    Ready,
    Staged,
    Marked,
    Pasted,
    Entered,
}

pub(super) struct Child {
    nonce: uuid::Uuid,
    pending: Value,
    receipt: Original,
    value: Value,
    marker: Value,
    prior: Option<Value>,
    version: Value,
    kind: InputKind,
    closing: bool,
}

pub(super) struct Reservation {
    pub(super) phase: Phase,
    pub(super) progress: Arc<Progress>,
    receipt: Option<Original>,
    value: Value,
    pending: Value,
    pub(super) child: Option<Child>,
    pasted: Vec<u8>,
    marker: Option<(Value, Value)>,
}

fn header(owner: &owner::Owner) -> Result<&input::native_send::Header> {
    owner
        .control
        .frame()
        .send
        .as_ref()
        .ok_or("Original Send header missing".into())
}

fn with<T>(owner: &owner::Owner, body: impl FnOnce(&mut Reservation) -> Result<T>) -> Result<T> {
    let mut held = owner
        .send
        .lock()
        .map_err(|_| "Original Send reservation unavailable")?;
    body(held.as_mut().ok_or("Original Send reservation missing")?)
}

pub(super) fn settings_nonce(owner: &owner::Owner) -> Result<uuid::Uuid> {
    with(owner, |held| {
        held.child
            .as_ref()
            .map(|child| child.nonce)
            .ok_or("Send has no active pending Settings capability".into())
    })
}

pub(super) fn own_marker(owner: &Arc<owner::Owner>, record: &Value) -> Result<bool> {
    if owner
        .send
        .lock()
        .map_err(|_| "Original Send reservation unavailable")?
        .is_none()
    {
        return Ok(false);
    }
    let (raw, version) = owner.snapshot()?;
    let h = header(owner)?;
    let kind = owner.with_scope(|scope| Ok(scope.proof.kind()))?;
    with(owner, |held| {
        let Some(child) = &held.child else {
            return Ok(false);
        };
        let outer = held
            .receipt
            .as_ref()
            .ok_or("Pending Settings lacks its outer receipt")?;
        owner.with_scope(|scope| {
            if outer.record(&scope.receipts)? != held.value
                || held.value["status"] != "in_progress"
                || held.value["request_id"] != h.request_id
                || held.value["digest"] != h.digest
                || child.kind != kind
                || child.version != version
                || !same_target(&raw, record)
            {
                return Err("Original parent Send reservation changed".into());
            }
            let settings = &scope
                .send_directories
                .as_ref()
                .ok_or("Send directories missing")?
                .0;
            if child.receipt.record(settings)? != child.value {
                return Err("Original pending Settings receipt changed".into());
            }
            if child.closing {
                if raw.get("pending_settings").is_some()
                    || raw.get("settings_unconfirmed_at") != child.prior.as_ref()
                {
                    return Err("Completed pending Settings record changed".into());
                }
                return Ok(false);
            }
            if raw["pending_settings"] != child.pending
                || raw.get("settings_unconfirmed_at") != Some(&child.marker)
                || record.get("settings_unconfirmed_at") != Some(&child.marker)
                || child.value["status"] != "in_progress"
            {
                return Err("Original pending Settings marker changed".into());
            }
            Ok(true)
        })
    })
}

pub(super) fn settings_key_ready(owner: &Arc<owner::Owner>) -> Result<()> {
    let record = owner.snapshot()?.0;
    if !own_marker(owner, &record)? {
        return Err("Pending Settings key has no live child capability".into());
    }
    settings_nonce(owner).map(drop)
}

pub(super) fn checked_settings(
    owner: &Arc<owner::Owner>,
    name: &str,
    request: &Request,
) -> Result<Value> {
    let h = header(owner)?;
    settings_nonce(owner)?;
    if name != h.dispatch_name
        || request.request_id != h.request_id
        || request.expected_run_id != h.run
        || request.expected_conversation_id != h.conversation
    {
        return Err("Pending Settings observation belongs to another parent Send".into());
    }
    let record = input::checked_bound_record(
        owner.snapshot()?.0,
        &h.run,
        &h.conversation,
        h.conversation.is_empty(),
    )?;
    if h.conversation.is_empty() && !input::first_message_ready(&record) {
        return Err(
            "The new session is not ready for model settings; refresh or open Terminal".into(),
        );
    }
    Ok(record)
}

pub(super) fn observe(owner: &Arc<owner::Owner>, kind: context::Kind) -> Result<Value> {
    let observation =
        context::Observation::begin_kind(kind)?.ok_or("Original Send observation missing")?;
    let (record, _) = owner.snapshot()?;
    let h = header(owner)?;
    let record = match kind {
        context::Kind::SendReady => {
            let record = input::checked_bound_record(record, &h.run, &h.conversation, true)?;
            compact_context::check_continuation(&record, &h.compaction)?;
            if !input::native_send::ready(&record) {
                return Err("agent became busy before pending Settings input; refresh".into());
            }
            input::checked_send_terminal(&record)?;
            record
        }
        context::Kind::SendAfterPaste => {
            if !process_alive(&record) || !matches(&record, panes_of(&live()?, &record)) {
                return Err("agent changed after paste".into());
            }
            input::checked_terminal(&record, false)?;
            record
        }
        _ => return Err("Wrong Send observation kind".into()),
    };
    observation.complete(&record)?;
    Ok(record)
}

/// The witness is built only here from the closed send sequence. It binds the
/// exact prepared bytes/destination and current record; it grants no argv.
pub(super) struct Publication {
    record: Value,
    version: Value,
    directory: PathBuf,
    name: String,
    sha: String,
    final_receipt: bool,
    source: Option<crate::state::guarded_recovery::Original>,
}
impl Publication {
    fn new(
        owner: &Arc<owner::Owner>,
        staged: &publication::Staged,
        directory: PathBuf,
        name: String,
        final_receipt: bool,
        source: Option<crate::state::guarded_recovery::Original>,
    ) -> Result<Self> {
        if final_receipt {
            with(owner, |held| {
                if held.phase != Phase::Entered
                    || !held.progress.enter_settled.load(Ordering::Acquire)
                {
                    return Err(
                        "Submitted receipt lacks the original settled Enter exchange".into(),
                    );
                }
                Ok(())
            })?;
        } else {
            observe(owner, context::Kind::SendReady)?;
            context::take_kind(owner, context::Kind::SendReady)?;
        }
        let (record, version) = owner.snapshot()?;
        Ok(Self {
            record,
            version,
            directory,
            name,
            sha: staged.content_sha()?,
            final_receipt,
            source,
        })
    }
    pub(super) fn pending_receipt(
        owner: &Arc<owner::Owner>,
        staged: &publication::Staged,
        id: &str,
    ) -> Result<Self> {
        parent_ready(owner, Phase::Prepared)?;
        let h = header(owner)?;
        if id != h.pending_id {
            return Err("Pending publication belongs to another UUID".into());
        }
        let directory = owner.with_scope(|scope| {
            Ok(scope
                .send_directories
                .as_ref()
                .ok_or("Send directories missing")?
                .0
                .path
                .clone())
        })?;
        let value = staged.json()?;
        let pending = with(owner, |held| Ok(held.pending.clone()))?;
        if value["request_id"] != id
            || value["status"] != "in_progress"
            || value["model"] != pending["model"]
            || value["effort"] != pending["effort"]
            || value["name"] != pending["receipt_name"]
            || value["run_id"] != pending["receipt_run_id"]
            || !input::native_send::cid_matches(&value["conversation_id"], &h.conversation)
        {
            return Err("Pending publication content does not match its child".into());
        }
        Self::new(owner, staged, directory, format!("{id}.json"), false, None)
    }
    pub(super) fn pending_marker(
        owner: &Arc<owner::Owner>,
        staged: &publication::Staged,
    ) -> Result<Self> {
        parent_ready(owner, Phase::Prepared)?;
        let (record, version) = owner.snapshot()?;
        let mut original = record.clone();
        let mut proposed = staged.json()?;
        let marker = proposed["settings_unconfirmed_at"]
            .as_f64()
            .ok_or("Pending marker is not a timestamp")?;
        if !marker.is_finite() || marker <= 0.0 || staged.expected_version() != Some(&version) {
            return Err("Invalid pending marker source".into());
        }
        for key in ["settings_unconfirmed_at", "updated"] {
            original
                .as_object_mut()
                .ok_or("Invalid Send record")?
                .remove(key);
            proposed
                .as_object_mut()
                .ok_or("Invalid Send marker record")?
                .remove(key);
        }
        if original != proposed {
            return Err("Pending marker cannot change another record field".into());
        }
        let (directory, name) = owner
            .with_scope(|scope| Ok((scope.sessions.path.clone(), scope.record.name.clone())))?;
        Self::new(owner, staged, directory, name, false, None)
    }
    pub(super) fn check(
        &self,
        owner: &Arc<owner::Owner>,
        staged: &publication::Staged,
    ) -> Result<()> {
        if owner.control.frame().operation != host::frame::Operation::Send
            || owner.snapshot()? != (self.record.clone(), self.version.clone())
            || !staged.destination_is(&Directory::open(&self.directory)?, &self.name)?
            || staged.content_sha()? != self.sha
        {
            return Err("Original Send publication source or content changed".into());
        }
        if let Some(source) = &self.source {
            source.verify()?;
        }
        if self.final_receipt {
            with(owner, |held| {
                if held.phase == Phase::Entered
                    && held.progress.enter_settled.load(Ordering::Acquire)
                {
                    Ok(())
                } else {
                    Err("Original Enter no longer authorizes submitted receipt".into())
                }
            })?;
        }
        owner.check()
    }
}

fn receipt_write(owner: &Arc<owner::Owner>, value: Value, final_receipt: bool) -> Result<()> {
    let h = header(owner)?;
    let (staged, parent, name) = owner.with_scope(|scope| {
        let name = format!("{}.json", h.request_id);
        let staged =
            publication::Staged::prepare(&scope.receipts, &name, value.to_string().as_bytes())?;
        with(owner, |held| {
            if staged.expected_version() != held.receipt.as_ref().map(|r| &r.version) {
                return Err("Original Send receipt changed before staging".into());
            }
            Ok(())
        })?;
        Ok((staged, scope.receipts.path.clone(), name))
    })?;
    let receipt_path = parent.join(&name);
    let step = Publication::new(owner, &staged, parent, name.clone(), final_receipt, None)?;
    let fence = publication::CommitFence::send(owner, &staged, step)?;
    let committed = staged.commit(&fence)?;
    let original = owner.with_scope(|scope| committed.receipt(owner, &scope.receipts, &name))?;
    // Cache registration follows the exact fenced original and its proof. It
    // cannot rewrite that original or turn an accepted send into a refusal.
    crate::state::receipt_index::remember_committed(&receipt_path, &value);
    with(owner, |held| {
        held.receipt = Some(original);
        held.value = value;
        Ok(())
    })
}

fn attachments(owner: &Arc<owner::Owner>) -> Result<Vec<Value>> {
    use std::ffi::CString;
    let h = header(owner)?;
    let leaf = format!("message-{}", uuid::Uuid::new_v4());
    let path = owner.with_scope(|scope| {
        let parent = &scope
            .send_directories
            .as_ref()
            .ok_or("Send directories missing")?
            .1;
        parent.verify()?;
        let name = CString::new(leaf.as_str()).map_err(|e| e.to_string())?;
        owner.control.commit_boundary()?;
        if unsafe { libc::mkdirat(parent.descriptor(), name.as_ptr(), 0o700) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        parent.sync()?;
        Ok(parent.child(&leaf)?.path)
    })?;
    let directory = Directory::open(&path)?;
    let mut paths = Vec::new();
    for (index, (name, mime, reference, bytes)) in owner.control.payload()?.files().enumerate() {
        let filename = input::safe_filename(name, index);
        let staged = publication::Staged::prepare(&directory, &filename, bytes)?;
        if staged.expected_version().is_some() {
            return Err("Send attachment destination appeared".into());
        }
        let step = Publication::new(owner, &staged, path.clone(), filename.clone(), false, None)?;
        let fence = publication::CommitFence::send(owner, &staged, step)?;
        staged.commit(&fence)?;
        // Receipt and terminal keep absolute_root's original spelling.
        let public = Path::new(&owner.control.frame().root)
            .join("attachments")
            .join(&leaf)
            .join(filename);
        paths.push(json!({"name":name,"mime":mime,"reference":reference,"path":public,"bytes":bytes.len()}));
    }
    h.validate()?;
    Ok(paths)
}

fn cancel_recovery(owner: &Arc<owner::Owner>, record: &Value) -> Result<()> {
    let Some((source, value)) = crate::state::recovery::input_cancellation(record)? else {
        return Ok(());
    };
    let parent = Directory::open(
        source
            .path()
            .parent()
            .ok_or("Recovery cancellation has no parent")?,
    )?;
    let name = source
        .path()
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("Invalid recovery job filename")?;
    let staged = publication::Staged::prepare(&parent, name, value.to_string().as_bytes())?;
    if staged.expected_version() != source.version() {
        return Err("Recovery job changed before cancellation".into());
    }
    let step = Publication::new(
        owner,
        &staged,
        parent.path.clone(),
        name.into(),
        false,
        Some(source),
    )?;
    let fence = publication::CommitFence::send(owner, &staged, step)?;
    staged.commit(&fence).map(drop)
}

fn mark_input(owner: &Arc<owner::Owner>, at: f64) -> Result<()> {
    let mut record = owner.snapshot()?.0;
    record["input_pending_at"] = json!(at);
    record["updated"] = json!(now());
    let (staged, parent, name) = owner.with_scope(|scope| {
        let staged = publication::Staged::prepare(
            &scope.sessions,
            &scope.record.name,
            format!("{record}\n").as_bytes(),
        )?;
        if staged.expected_version() != Some(&scope.record.version) {
            return Err("Send record changed before input marker".into());
        }
        Ok((
            staged,
            scope.sessions.path.clone(),
            scope.record.name.clone(),
        ))
    })?;
    let step = Publication::new(owner, &staged, parent, name, false, None)?;
    let fence = publication::CommitFence::send(owner, &staged, step)?;
    owner.adopt_record(staged.commit(&fence)?)?;
    let (_, version) = owner.snapshot()?;
    with(owner, |held| {
        held.marker = Some((json!(at), version));
        held.phase = Phase::Marked;
        Ok(())
    })
}

pub(super) fn parent_ready(owner: &Arc<owner::Owner>, phase: Phase) -> Result<()> {
    owner.actor()?;
    owner.check()?;
    let (record, version) = owner.snapshot()?;
    with(owner, |held| {
        if held.phase != phase || held.child.is_some() {
            return Err("Send effect belongs to another phase".into());
        }
        owner.with_scope(|scope| {
            if held
                .receipt
                .as_ref()
                .ok_or("Send effect lacks its outer receipt")?
                .record(&scope.receipts)?
                != held.value
                || held.value["status"] != "in_progress"
            {
                return Err("Original Send receipt changed before effect".into());
            }
            Ok(())
        })?;
        if matches!(phase, Phase::Marked | Phase::Pasted)
            && held.marker.as_ref() != Some(&(record["input_pending_at"].clone(), version))
        {
            return Err("Original Send input marker changed".into());
        }
        Ok(())
    })
}

pub(super) fn pasted(owner: &owner::Owner) -> Result<Vec<u8>> {
    with(owner, |held| Ok(held.pasted.clone()))
}
pub(super) fn advance(owner: &owner::Owner, from: Phase, to: Phase) -> Result<()> {
    with(owner, |held| {
        if held.phase != from {
            return Err("Original Send phase changed".into());
        }
        held.phase = to;
        Ok(())
    })
}

fn check_replies(record: &Value, value: &Value) -> Result<()> {
    let mut max = value.clone();
    max["status"] = json!("submitted");
    let preview = input::native_send::Reply::Submitted {
        receipt: max,
        analytics: crate::analytics::adapters::describe_delivery(
            record,
            value,
            crate::analytics::adapters::Delivery::Submitted,
        ),
    };
    input::native_send::check_reply_size(&preview)?;
    let error_preview = input::native_send::Reply::Failed {
        message: "\u{1}".repeat(input::native_send::ERROR_BYTES),
        analytics: crate::analytics::adapters::describe_delivery(
            record,
            value,
            crate::analytics::adapters::Delivery::Uncertain,
        ),
    };
    input::native_send::check_reply_size(&error_preview)?;
    Ok(())
}

pub(super) fn execute(owner: &Arc<owner::Owner>) -> Result<Vec<u8>> {
    let h = header(owner)?;
    let record = owner.snapshot()?.0;
    if string(&record, "agent") != h.agent
        || !matches!(string(&record, "backend"), "" | "tmux")
        || input::native_send::pending_digest(&record["pending_settings"]) != h.pending_digest
    {
        return Err("Original pending Settings target changed before Send".into());
    }
    // Runtime policy/platform checks are repeated under the actual Root.
    crate::state::session_operations()?;
    observe(owner, context::Kind::SendReady)?;
    let paths = attachments(owner)?;
    let (text, pasted) = owner.control.payload()?.terminal_text(&paths, &h.agent);
    if pasted.len() > 1 << 20 {
        return Err("Terminal input exceeds 1 MiB".into());
    }
    let value = owner
        .control
        .payload()?
        .receipt(h, &record, &paths, &text, now());
    let progress = Arc::new(Progress::default());
    check_replies(&record, &value)?;
    *owner
        .send
        .lock()
        .map_err(|_| "Send reservation unavailable")? = Some(Reservation {
        phase: Phase::Prepared,
        progress: progress.clone(),
        receipt: None,
        value: value.clone(),
        pending: record["pending_settings"].clone(),
        child: None,
        pasted: pasted.into_bytes(),
        marker: None,
    });
    // Reserve cleanup capacity before any Settings key/Stage. The lease lives
    // outside Owner, avoiding a source-charge/Owner reference cycle.
    let cleanup = send_helpers::Cleanup::reserve(owner, progress.clone())?;
    let result = (|| {
        receipt_write(owner, value, false)?;
        let step = begin_pending(owner)?;
        settings::apply_pending_step(owner, step)?;
        advance(owner, Phase::Child, Phase::Ready)?;
        // Recovery publication obtains its own fresh SendReady turn after
        // staging/fsync. Its optional read grants no effect by itself.
        let ready = owner.snapshot()?.0;
        cancel_recovery(owner, &ready)?;
        observe(owner, context::Kind::SendReady)?;
        send_helpers::stage(owner, progress.clone())?;
        // receipt_write stages first, then obtains and consumes a fresh
        // SendReady turn. An earlier, unconsumed observation adds no fence.
        let at = now();
        let mut receipt = with(owner, |held| Ok(held.value.clone()))?;
        receipt["submitted_at"] = json!(at);
        check_replies(&owner.snapshot()?.0, &receipt)?;
        receipt_write(owner, receipt, false)?;
        mark_input(owner, at)?;
        send_helpers::paste(owner, progress.clone())?;
        std::thread::sleep(Duration::from_millis(180));
        observe(owner, context::Kind::SendAfterPaste)?;
        send_helpers::enter(owner, progress.clone())?;
        let mut receipt = with(owner, |held| Ok(held.value.clone()))?;
        receipt["status"] = json!("submitted");
        receipt_write(owner, receipt.clone(), true)?;
        let record = owner.snapshot()?.0;
        serde_json::to_vec(&input::native_send::Reply::Submitted {
            analytics: crate::analytics::adapters::describe_delivery(
                &record,
                &receipt,
                crate::analytics::adapters::Delivery::Submitted,
            ),
            receipt,
        })
        .map_err(|e| e.to_string())
    })();
    let discarded = cleanup.finish();
    let result = match (result, discarded) {
        (Ok(bytes), Ok(())) => Ok(bytes),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(format!("{error}; {cleanup}")),
    };
    result.or_else(|error| {
        let receipt = with(owner, |held| Ok(held.value.clone()))?;
        let analytics = if progress.paste_possible.load(Ordering::Acquire) {
            crate::analytics::adapters::describe_delivery(
                &record,
                &receipt,
                crate::analytics::adapters::Delivery::Uncertain,
            )
        } else {
            None
        };
        serde_json::to_vec(&input::native_send::Reply::Failed {
            message: input::native_send::bounded_error(error),
            analytics,
        })
        .map_err(|e| e.to_string())
    })
}

/// By-value, non-Clone capability. Its nonce is never serialized; the only
/// consumer is Settings::apply_pending_step in this same owner/thread.
pub(super) struct PendingStep {
    nonce: uuid::Uuid,
    pending: Value,
    receipt: Original,
    value: Value,
}

fn begin_pending(owner: &Arc<owner::Owner>) -> Result<PendingStep> {
    parent_ready(owner, Phase::Prepared)?;
    let h = header(owner)?;
    let pending = with(owner, |held| Ok(held.pending.clone()))?;
    let (receipt, value) = owner.with_scope(|scope| {
        let directory = &scope
            .send_directories
            .as_ref()
            .ok_or("Send directories missing")?
            .0;
        let receipt = Original::open(directory, format!("{}.json", h.pending_id))?;
        let value = receipt.record(directory)?;
        if value["status"] != "scheduled"
            || value["request_id"] != h.pending_id
            || value["model"] != pending["model"]
            || value["effort"] != pending["effort"]
            || value["name"] != pending["receipt_name"]
            || value["run_id"] != pending["receipt_run_id"]
            || !input::native_send::cid_matches(&value["conversation_id"], &h.conversation)
            || string(&pending, "run_id") != h.run
            || !input::native_send::cid_matches(&pending["conversation_id"], &h.conversation)
        {
            return Err("Pending Settings receipt is not safe to apply".into());
        }
        Ok((receipt, value))
    })?;
    Ok(PendingStep {
        nonce: uuid::Uuid::new_v4(),
        pending,
        receipt,
        value,
    })
}

impl PendingStep {
    pub(super) fn original<'a>(&'a self, owner: &Arc<owner::Owner>) -> Result<&'a Original> {
        owner.with_scope(|scope| {
            let directory = &scope
                .send_directories
                .as_ref()
                .ok_or("Send directories missing")?
                .0;
            if self.receipt.record(directory)? != self.value {
                return Err("Pending capability source changed".into());
            }
            Ok(())
        })?;
        Ok(&self.receipt)
    }
    pub(super) fn value(&self) -> Value {
        self.value.clone()
    }
    pub(super) fn request(&self, owner: &Arc<owner::Owner>) -> Result<SettingsRequest> {
        parent_ready(owner, Phase::Prepared)?;
        let h = header(owner)?;
        if input::native_send::pending_digest(&self.pending) != h.pending_digest {
            return Err("Pending capability target changed".into());
        }
        Ok(SettingsRequest {
            request_id: h.request_id.clone(),
            expected_run_id: h.run.clone(),
            expected_conversation_id: h.conversation.clone(),
            model: Some(string(&self.pending, "model").into()),
            effort: Some(string(&self.pending, "effort").into()),
            expected_pending_id: Some(h.pending_id.clone()),
        })
    }
    pub(super) fn activate(
        self,
        owner: &Arc<owner::Owner>,
        receipt: Original,
        value: Value,
        marker: Value,
        prior: Option<Value>,
    ) -> Result<()> {
        let (_, version) = owner.snapshot()?;
        let kind = owner.with_scope(|scope| Ok(scope.proof.kind()))?;
        let mut expected = self.value.clone();
        expected["status"] = json!("in_progress");
        with(owner, |held| {
            if held.phase != Phase::Prepared
                || held.child.is_some()
                || self.pending != held.pending
                || self.value["status"] != "scheduled"
                || self.receipt.name != receipt.name
                || value != expected
            {
                return Err("Pending capability cannot be activated".into());
            }
            held.child = Some(Child {
                nonce: self.nonce,
                pending: self.pending,
                receipt,
                value,
                marker,
                prior,
                version,
                kind,
                closing: false,
            });
            held.phase = Phase::Child;
            Ok(())
        })
    }
}

pub(super) fn child_prior(owner: &owner::Owner) -> Result<Option<Value>> {
    with(owner, |held| {
        held.child
            .as_ref()
            .map(|c| c.prior.clone())
            .ok_or("Pending child missing".into())
    })
}
pub(super) fn child_record_committed(owner: &Arc<owner::Owner>) -> Result<()> {
    let (_, version) = owner.snapshot()?;
    with(owner, |held| {
        let child = held.child.as_mut().ok_or("Pending child missing")?;
        child.version = version;
        child.closing = true;
        Ok(())
    })
}
pub(super) fn finish_pending(
    owner: &Arc<owner::Owner>,
    applied: Original,
    value: Value,
) -> Result<()> {
    let (record, version) = owner.snapshot()?;
    with(owner, |held| {
        let child = held.child.as_ref().ok_or("Pending child missing")?;
        let mut expected = child.value.clone();
        expected["status"] = json!("applied");
        if !child.closing
            || child.version != version
            || value != expected
            || applied.name != child.receipt.name
            || record.get("pending_settings").is_some()
            || record.get("settings_unconfirmed_at") != child.prior.as_ref()
            || record["model"] != value["model"]
            || record["effort"] != value["effort"]
        {
            return Err("Pending Settings is not committed".into());
        }
        owner.with_scope(|scope| {
            let directory = &scope
                .send_directories
                .as_ref()
                .ok_or("Send Settings directory missing")?
                .0;
            if child.kind != scope.proof.kind() || applied.record(directory)? != value {
                return Err("Applied pending Settings receipt changed".into());
            }
            Ok(())
        })?;
        held.child = None;
        Ok(())
    })
}

#[cfg(test)]
mod tests;
