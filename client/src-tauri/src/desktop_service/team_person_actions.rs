//! Native confirmation for one exact team action, then receipt-only recovery.
use super::{creation_clone::Read, Service};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, PoisonError,
    },
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use zerus_client_core::{
    actions::Exit,
    desktop::{team_person_actions as wire, ui_requests::Request},
    upstream_teams::action::{classify, TeamCall, TeamOutcome},
};

#[derive(Default)]
pub(crate) struct Actions {
    originals: Mutex<BTreeMap<String, wire::Intent>>,
    reviewing: AtomicBool,
}
enum Path {
    Review,
    Read(Box<wire::Intent>),
    Unknown,
}
impl Actions {
    fn path(&self, plan: &wire::Plan, check_only: bool) -> Path {
        if check_only {
            return self
                .originals
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&plan.request_id)
                .filter(|intent| plan.reference().as_ref() == Some(&intent.plan))
                .cloned()
                .map_or(Path::Unknown, |intent| Path::Read(Box::new(intent)));
        }
        Path::Review
    }
    fn reserve(&self, intent: &wire::Intent) -> bool {
        let mut originals = self
            .originals
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if originals.contains_key(&intent.plan.request_id)
            || originals.len() >= zerus_client_core::upstream_teams::action::intents::INTENTS_KEPT
        {
            return false;
        }
        originals.insert(intent.plan.request_id.clone(), intent.clone());
        true
    }
    pub(crate) fn dismiss(&self, id: &str) {
        self.originals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }
}
struct Confirmed(wire::Intent);
impl Confirmed {
    /// Production supplies only the native dialog callback's result here.
    fn after_dialog(review: wire::Review, accepted: bool) -> Option<Self> {
        (accepted && review.valid()).then_some(Self(review.intent))
    }
}
struct ReviewSlot<'a>(&'a AtomicBool);
impl Drop for ReviewSlot<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
fn failed(call: &TeamCall, code: &str, detail: &str) -> TeamOutcome {
    TeamOutcome::Failed {
        request_id: call.request_id.clone(),
        code: code.into(),
        detail: detail.into(),
        field: None,
    }
}
fn unknown(call: &TeamCall) -> TeamOutcome {
    TeamOutcome::Uncertain { request_id:call.request_id.clone(),detail:"The original team action could not be confirmed. Check its original receipt; it will not be sent again.".into() }
}
fn outcome(call: &TeamCall, intent: &wire::Intent, bytes: &[u8]) -> TeamOutcome {
    let value: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
    if value["intent"] != json!(intent) || value["reply"]["request_id"] != call.request_id {
        return unknown(call);
    }
    let reply = &value["reply"];
    if reply["ok"] == true
        && (reply["revision"].as_u64() != intent.plan.expected_revision.checked_add(1)
            || reply["actor"] != json!({"kind":"person","person":"local","origin":"desktop_ui"}))
    {
        return unknown(call);
    }
    classify(
        call,
        Exit::Code {
            code: if reply["ok"] == true { 0 } else { 1 },
            stdout: serde_json::to_vec(reply).unwrap_or_default(),
            stderr: String::new(),
        },
    )
}
fn stage(read: &Arc<Read>, plan: &wire::Plan) -> bool {
    let Some(reference) = plan.reference() else {
        return false;
    };
    let Ok(body) = serde_json::to_vec(plan) else {
        return false;
    };
    for (ordinal, bytes) in body.chunks(wire::PIECE_BYTES).enumerate() {
        let Some(piece) = wire::Piece::from_bytes(
            uuid::Uuid::new_v4().to_string(),
            read.scope.clone(),
            reference.clone(),
            ordinal as u16,
            bytes,
        ) else {
            return false;
        };
        let id = piece.request_id.clone();
        let result = read
            .exchange(
                Some(Request::TeamPersonPlanPiece {
                    piece: Box::new(piece),
                }),
                Value::Null,
                false,
                None,
            )
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let last = (ordinal + 1) * wire::PIECE_BYTES >= body.len();
        if !result.is_some_and(|value| {
            value["ok"] == true && value["request_id"] == id && value["complete"] == last
        }) {
            return false;
        }
    }
    true
}

/// Runs off the UI thread. A renderer asks for review; only the native dialog
/// acceptance can register the separate original which carries Person consent.
pub(crate) fn run(
    app: tauri::AppHandle,
    service: Option<Arc<Service>>,
    actions: Arc<Actions>,
    call: TeamCall,
    check_only: bool,
) -> TeamOutcome {
    if crate::actions::blocked() {
        return failed(
            &call,
            "person_gesture_required",
            "Team actions require the live Zerus window.",
        );
    }
    let Some(plan) = wire::Plan::from_call(&call) else {
        return failed(
            &call,
            "invalid_request",
            "Read the exact team action before confirming it.",
        );
    };
    let Ok(read) = Read::new(service) else {
        return if check_only {
            unknown(&call)
        } else {
            failed(
                &call,
                "person_gesture_required",
                "Connect this window to its desktop service before changing a team.",
            )
        };
    };
    for marker in wire::required_markers(&plan) {
        if !read.supports(marker).unwrap_or(false) {
            return if check_only {
                unknown(&call)
            } else {
                failed(
                    &call,
                    "person_gesture_required",
                    if marker == wire::PLACEMENT_MARKER {
                        // An older service refuses a window Route's kept
                        // fields; say what to update (zerus-3u5yu 3).
                        "Update the selected desktop service to route or reassign tasks from this window."
                    } else {
                        "Update the selected desktop service to use native team confirmation."
                    },
                )
            };
        }
    }
    match actions.path(&plan, check_only) {
        Path::Read(intent) => {
            return read
                .exchange(
                    None,
                    json!({"op":"team_person_action_result","intent":intent}),
                    false,
                    None,
                )
                .map_or_else(|_| unknown(&call), |bytes| outcome(&call, &intent, &bytes))
        }
        Path::Unknown => return unknown(&call),
        Path::Review => (),
    }
    if actions.reviewing.swap(true, Ordering::AcqRel) {
        return failed(
            &call,
            "person_gesture_busy",
            "Finish the open team confirmation first.",
        );
    }
    let _slot = ReviewSlot(&actions.reviewing);
    {
        let originals = actions
            .originals
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if originals.contains_key(&call.request_id) {
            return unknown(&call);
        }
        if originals.len() >= zerus_client_core::upstream_teams::action::intents::INTENTS_KEPT {
            return failed(
                &call,
                "person_gesture_busy",
                "Check the unresolved team actions first.",
            );
        }
    }
    if !stage(&read, &plan) {
        return failed(
            &call,
            "team_person_plan_unavailable",
            "The complete team plan could not be prepared. Nothing was confirmed.",
        );
    }
    let review = read
        .exchange(
            None,
            json!({"op":"team_person_action_prepare","scope":read.scope,"plan":plan.reference()}),
            false,
            None,
        )
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| serde_json::from_value::<wire::Review>(value["review"].clone()).ok())
        .filter(|review| {
            review.valid()
                && Some(&review.intent.plan) == plan.reference().as_ref()
                && review.intent.scope == read.scope
        });
    let Some(review) = review else {
        return failed(
            &call,
            "team_action_changed",
            "The exact team action is unavailable or changed. Read it again.",
        );
    };
    let confirmed = match confirm(&app, &plan, review) {
        Ok(confirmed) => confirmed,
        Err(message) => return failed(&call, "person_gesture_cancelled", &message),
    };
    if !read.current() {
        return failed(
            &call,
            "person_gesture_required",
            "The selected desktop service changed during confirmation.",
        );
    }
    let intent = confirmed.0;
    // Retain the immutable original before its authority-bearing RPC. Lost
    // replies always take the read path, never registration or CLI replay.
    if !actions.reserve(&intent) {
        return unknown(&call);
    }
    let result = read
        .exchange(
            Some(Request::TeamPersonAction {
                intent: Box::new(intent.clone()),
            }),
            Value::Null,
            false,
            None,
        )
        .map_or_else(|_| unknown(&call), |bytes| outcome(&call, &intent, &bytes));
    if matches!(
        result,
        TeamOutcome::Done { .. } | TeamOutcome::Failed { .. }
    ) {
        actions.dismiss(&call.request_id);
    }
    result
}

fn confirm(
    app: &tauri::AppHandle,
    plan: &wire::Plan,
    review: wire::Review,
) -> Result<Confirmed, String> {
    let window = crate::main_window(app).ok_or("The main window is unavailable.")?;
    let title = crate::native_text::render(&zerus_client_core::text!("Confirm team action"));
    let details = review
        .details
        .iter()
        .map(|(key, value)| format!("{key}:\n{value}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let body=format!("{}\n\n{}\n\nTeam: {}\nComputer: {}\nRevision: {}\n\nReviewed values begin\n{}\nReviewed values end",
        serde_json::to_string(&review.team_name).unwrap_or_default(),review.subject,plan.team,review.intent.machine,plan.expected_revision,details);
    let accepted = app
        .dialog()
        .message(body)
        .title(title.clone())
        .parent(&window)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            title,
            crate::native_text::render(&zerus_client_core::text!("Cancel")),
        ))
        .blocking_show();
    Confirmed::after_dialog(review, accepted)
        .ok_or_else(|| "The team action was cancelled. Nothing was sent.".into())
}

mod tasks;
pub(crate) use tasks::run_task;

#[cfg(test)]
mod tests;
