//! The fixed existing ID Effort and Settings lanes. Its production and real-CLI controls use
//! the same owner, raw request, recovery, native helpers and receipt commits.
use super::*;
use std::sync::Arc;

pub(super) mod context;
mod effects;
mod owner;
mod publication;
pub(super) mod read;
mod settings;
mod send;
mod send_helpers;
#[cfg(test)]
mod tests;
pub(super) mod transaction;

pub(super) fn entered() -> bool {
    context::entered()
}
pub(super) fn owned_send() -> bool {
    context::owner().is_ok_and(|owner|owner.is_some_and(|owner|
        owner.control.frame().operation==host::frame::Operation::Send))
}
pub(super) fn helper_read(
    call: read::Call,
    args: &[String],
) -> Option<Result<std::process::Output>> {
    read::route(call, args)
}
pub(super) fn close_claude_model(name: &str, request: &Request) -> Result<()> {
    let owner = context::owner()?.ok_or("Original picker owner missing during model cancellation")?;
    read::close_claude_model(&owner, name, request)
}
pub(super) fn refuse<T>(error: String) -> Result<T> {
    match context::owner()? {
        Some(owner) => owner.refuse(error),
        None => Err(error),
    }
}
pub(super) fn own_marker(record: &Value) -> Result<bool> {
    transaction::own_marker(record).or_else(refuse)
}
pub(super) fn deadline(until: Instant) -> Result<Instant> {
    match context::owner()? {
        Some(owner) => {
            owner.check()?;
            Ok(until.min(owner.until))
        }
        None => Ok(until),
    }
}

pub(super) fn checked(name: &str, request: &Request, empty: bool) -> Result<Value> {
    let observation =
        context::Observation::begin()?.ok_or("Original Effort observation context missing")?;
    let record = if let Some(owner) = context::owner()? {
        if owner.control.frame().operation == host::frame::Operation::Send {
            send::checked_settings(&owner, name, request)?
        } else { input::checked_settings(name,&request.expected_run_id,&request.expected_conversation_id)? }
    } else { input::checked_settings(
        name,
        &request.expected_run_id,
        &request.expected_conversation_id,
    )? };
    input::checked_terminal(&record, empty)?;
    observation.complete(&record)?;
    Ok(record)
}

pub(super) fn send_settings_ready(record: &Value) -> Option<Result<()>> {
    let owner = match context::owner() {
        Ok(Some(owner)) if owner.control.frame().operation==host::frame::Operation::Send => owner,
        Ok(_) => return None,
        Err(error) => return Some(Err(error)),
    };
    Some((|| {
        let (raw,_)=owner.snapshot()?;
        if !same_target(&raw,record) {return Err("Pending Settings target changed".into());}
        let h=owner.control.frame().send.as_ref().ok_or("Send header missing")?;
        let record=input::checked_bound_record(raw,&h.run,&h.conversation,true)?;
        input::checked_terminal(&record,true)
    })())
}

pub(super) fn front(name: &str, raw: &[u8]) -> Option<Result<i32>> {
    front_operation(host::frame::Operation::Effort, name, raw)
}

pub(super) fn front_settings(name: &str, raw: &[u8]) -> Option<Result<i32>> {
    front_operation(host::frame::Operation::Settings, name, raw)
}

fn front_operation(
    operation: host::frame::Operation,
    name: &str,
    raw: &[u8],
) -> Option<Result<i32>> {
    let record = if crate::state::session_id::has_deferred(name) {
        Err("The original label is awaiting its raw request guard".into())
    } else {
        crate::state::storage::read_metadata(name)
    };
    if record
        .as_ref()
        .is_ok_and(|record| !crate::state::storage::in_id_layout(record))
    {
        return None;
    }
    // A mixed label may still name the existing legacy first holder. Only a
    // unique metadata match of this exact raw request stays on that old lane;
    // unknown/ID/error cases retain cache-first keeper lookup below.
    if record.is_err() && crate::state::session_id::selected().is_none() {
        if let (Ok(request), Ok(records)) = (
            request_guard(operation, raw),
            crate::state::session_index::metadata_records_unrepaired(),
        ) {
            let matches: Vec<_> = records
                .iter()
                .filter(|record| {
                    string(record, "name") == name
                        && string(record, "run_id") == request.expected_run_id
                        && string(record, "conversation_id") == request.expected_conversation_id
                })
                .collect();
            if matches.as_slice().first().is_some_and(|record| {
                matches.len() == 1 && !crate::state::storage::in_id_layout(record)
            }) {
                return None;
            }
        }
    }
    Some((|| {
        let selector = record.unwrap_or_else(|_| json!({"name":name}));
        let frame = host::frame::Frame::new(
            operation,
            raw.to_vec(),
            &selector,
            host::frame::Deadline::after(operation_budget(operation))?,
        )?;
        let bytes = host::request(frame)?;
        println!("{}", String::from_utf8_lossy(&bytes));
        Ok(0)
    })())
}

pub(super) fn run(writer: Guard, control: Arc<host::Control>) -> Result<Vec<u8>> {
    let (owner, done) = owner::Owner::new(writer, control)?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _entered = context::Entered::install(owner.clone())?;
        let frame = owner.control.frame();
        let effects = effects::Recovery(owner.clone());
        crate::state::rename::recover_guarded(&effects)?;
        crate::state::rename::sync_legacy_guarded(&effects)?;
        crate::state::worktree_lock::recover_guarded(&effects)?;
        owner.check()?;
        if string(&frame.binding, "session_id").is_empty() || frame.resolve_send_label() {
            let request = frame.request_guard()?;
            crate::state::resolve_effort_label(
                string(&frame.binding, "name"),
                crate::state::LabelGuard::Given {
                    run: request.expected_run_id,
                    conversation: request.expected_conversation_id,
                },
            )
            .map_err(|error| match error.value {
                Some(value) => crate::state::session_id::refuse((error.text, value)),
                None => error.text,
            })?;
            if frame.resolve_send_label() && crate::state::session_id::selected().is_none_or(|selected|
                selected.session_id!=string(&frame.binding,"session_id")) {
                return Err("Original guarded Send label changed before Root admission".into());
            }
        }
        let record = host::original_record(frame)?;
        let receipt_path = root()
            .join(frame.operation.receipt_directory())
            .join(format!("{}.json", frame.request_id));
        let absent = crate::state::guarded_recovery::Original::read_absent(&receipt_path)?;
        effects::parent(&owner, &receipt_path, &[&absent])?;
        if frame.operation == host::frame::Operation::Send {
            for directory in ["settings_receipts","attachments"] {
                effects::parent(&owner,&root().join(directory).join(".send-parent"),&[&absent])?;
            }
        }
        crate::state::session_id::select_guarded_route(
            string(&record, "name"),
            string(&record, "session_id"),
            string(&record, "run_id"),
            string(&record, "conversation_id"),
            true,
        );
        owner.open_scope(&record)?;
        match frame.operation {
            host::frame::Operation::Effort => transaction::execute(&owner),
            host::frame::Operation::Settings => settings::execute(&owner),
            host::frame::Operation::Send => send::execute(&owner),
        }
        .or_else(|error| owner.refuse(error))
    }));
    drop(owner);
    // Body failure/unwind does not release Root/SID while ANY exact exchange
    // retains the original owner. Completion is its last field, after phase.
    loop {
        match done.recv_timeout(Duration::from_millis(20)) {
            Ok(()) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => loop {
                std::thread::park_timeout(Duration::from_secs(1));
            },
        }
    }
    match result {
        Ok(result) => result,
        Err(_) => Err("Original Effort operation panicked; no replay".into()),
    }
}

pub(super) fn request_guard(operation: host::frame::Operation, raw: &[u8]) -> Result<Request> {
    match operation {
        host::frame::Operation::Effort => serde_json::from_slice(raw).map_err(|e| e.to_string()),
        host::frame::Operation::Settings => {
            let request: SettingsRequest =
                serde_json::from_slice(raw).map_err(|e| e.to_string())?;
            Ok(Request {
                request_id: request.request_id,
                expected_run_id: request.expected_run_id,
                expected_conversation_id: request.expected_conversation_id,
                effort: request.effort.unwrap_or_default(),
            })
        }
        host::frame::Operation::Send => Err("Send request guard must use its verified envelope".into()),
    }
}
