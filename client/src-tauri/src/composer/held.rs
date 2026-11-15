//! The messages hgs holds for the open session until its agent takes the
//! previous one, and the text left in the agent's own Terminal input that
//! holds them:
//! - a send whose receipt says "queued" leaves the composer like a sent one,
//!   but shows as a held row at once instead of a sent card, until an
//!   inspection lists it or two inspections did not;
//! - the held rows are the inspection's `held_inputs` plus those just
//!   queued, oldest first; Cancel (`hgs send-cancel`) waits for its answer,
//!   and only a cancel hgs confirmed puts the text back, into an empty editor;
//! - a held message this composer queued that hgs lists in `held_settled`
//!   (failed, or uncertain) comes back to the editor once, with why;
//! - the Terminal draft is read right after a send hgs refused for it, and
//!   at most every 4 s while messages are held for an agent that is not
//!   busy. Send it presses Enter only while Terminal still holds exactly the
//!   draft shown (its hash); an Enter that may have gone is never sent again.
//!
//! Everything is bounded: at most 64 sessions, 32 held rows each.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tauri::State;
use zerus_client_core::held_input::{self, DraftError, DraftRead};
use zerus_client_core::text::{Arg, Text};
use zerus_client_core::{Exit, Invocation, Target};

use super::{lock, Composer, ComposerEvent, Composers, Hub};

/// One blocking `hgs` call: the app's runs the configured CLI.
pub type HgsRun = dyn Fn(&Invocation) -> Exit + Send + Sync;

/// Sessions whose held messages the hub remembers.
const SESSIONS: usize = 64;
/// Held rows per session (hgs holds at most this many too).
const PER_SESSION: usize = 32;
/// While messages are held, the draft is read at most this often.
const DRAFT_EVERY: f64 = 4.0;
/// A Send that did nothing while the previous message is not confirmed.
const UNSETTLED: &str = zerus_client_core::source!(
    "Not sent: the previous message is not confirmed yet. Check Terminal, then use Allow retry before sending again."
);
/// A Send that did nothing while the desktop service still delivers one.
const STILL_DELIVERING: &str = zerus_client_core::source!(
    "Not sent: the previous message is still being delivered. Wait for it, or check Terminal."
);
/// A Send that did nothing while another one is on its way.
const ON_ITS_WAY: &str =
    zerus_client_core::source!("Not sent: another message to this session is on its way.");

/// One held message as the bar shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct HeldInputView {
    pub request_id: String,
    pub text: String,
    /// Its Cancel runs: the button waits for the answer.
    pub cancelling: bool,
}

/// The Terminal draft bar, shown while a draft or its last error is known.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TerminalDraftView {
    pub draft: String,
    /// Send it: a hash is known and no read or Enter runs.
    pub can_send: bool,
    /// Send it shows "Sending…".
    pub submitting: bool,
    /// Why the last Send it did not go; shown instead of the draft.
    pub error: Option<Text>,
}

/// A message shown at once after its "queued" receipt.
#[derive(Clone, Debug, PartialEq)]
struct Just {
    request_id: String,
    text: String,
    /// Inspections since its receipt that did not list it.
    reads: u8,
}

/// A held message hgs could not deliver (`HeldSent::Lost`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Lost {
    pub(crate) text: String,
    pub(crate) error: String,
    pub(crate) uncertain: bool,
}

/// The Terminal draft of one exact run.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Draft {
    key: String,
    computer: String,
    name: String,
    run: String,
    conversation: String,
    draft: String,
    hash: String,
    error: Option<Text>,
    /// The read or Enter under way, by number: only its answer counts.
    request: Option<u64>,
    submitting: bool,
    /// When the last read started, on the hub clock.
    checked: Option<f64>,
}

impl Draft {
    fn of(key: &str, target: &Target) -> Self {
        Draft {
            key: key.to_owned(),
            computer: target.computer.clone(),
            name: target.name.clone(),
            run: target.run_id.clone(),
            conversation: target.conversation_id.clone(),
            ..Draft::default()
        }
    }

    /// The same session, run and conversation (Qt's `setSession`).
    fn same(&self, key: &str, target: &Target) -> bool {
        self.key == key
            && self.computer == target.computer
            && self.name == target.name
            && self.run == target.run_id
            && self.conversation == target.conversation_id
    }

    /// No draft is shown .
    fn clear(&mut self) {
        self.draft.clear();
        self.hash.clear();
        self.error = None;
    }

    fn view(&self) -> Option<TerminalDraftView> {
        (!self.draft.is_empty() || self.error.is_some()).then(|| TerminalDraftView {
            draft: self.draft.clone(),
            can_send: !self.hash.is_empty() && !self.draft.is_empty() && self.request.is_none(),
            submitting: self.submitting,
            error: self.error.clone(),
        })
    }
}

/// The hub's part: held messages by draft key, and the open run's draft.
#[derive(Debug, Default)]
pub(crate) struct Holds {
    just: HashMap<String, Vec<Just>>,
    /// The messages each composer queued, as written (`HeldSent`).
    sent: HashMap<String, VecDeque<(String, String)>>,
    /// Lost messages waiting for their composer to be free.
    returning: HashMap<String, Vec<Lost>>,
    /// Cancels under way, and cancels hgs confirmed that an inspection
    /// may still list, by (key, request).
    cancelling: HashSet<(String, String)>,
    cancelled: HashSet<(String, String)>,
    /// Keys least recently touched first, at most [`SESSIONS`].
    order: VecDeque<String>,
    pub(crate) draft: Option<Draft>,
    next: u64,
}

fn listed(details: &Value) -> HashSet<&str> {
    details["held_inputs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["request_id"].as_str())
        .collect()
}

impl Holds {
    fn touch(&mut self, key: &str) {
        self.order.retain(|known| known != key);
        self.order.push_back(key.to_owned());
        while self.order.len() > SESSIONS {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            self.just.remove(&old);
            self.sent.remove(&old);
            self.returning.remove(&old);
            self.cancelling.retain(|(key, _)| key != &old);
            self.cancelled.retain(|(key, _)| key != &old);
        }
    }

    /// A send of `key` came back "queued": it shows at once, and it is this
    /// composer's, so a failure brings `written` back (`finishMessage`).
    pub(crate) fn queued(&mut self, key: &str, request_id: &str, text: &str, written: &str) {
        if request_id.is_empty() {
            return;
        }
        self.touch(key);
        let shown = if text.is_empty() { written } else { text };
        let kept = if written.is_empty() { text } else { written };
        let just = self.just.entry(key.to_owned()).or_default();
        just.retain(|held| held.request_id != request_id);
        just.push(Just {
            request_id: request_id.to_owned(),
            text: shown.to_owned(),
            reads: 0,
        });
        let excess = just.len().saturating_sub(PER_SESSION);
        just.drain(..excess);
        let sent = self.sent.entry(key.to_owned()).or_default();
        sent.retain(|(id, _)| id != request_id);
        sent.push_back((request_id.to_owned(), kept.to_owned()));
        while sent.len() > PER_SESSION {
            sent.pop_front();
        }
    }

    /// One accepted inspection of `key`: a just-queued message goes once it
    /// is listed or two inspections did not list it, and the messages this
    /// composer queued that hgs settled as failed or uncertain come back,
    /// each once, oldest first.
    pub(crate) fn observed(&mut self, key: &str, details: &Value) -> Vec<Lost> {
        let listed = listed(details);
        if let Some(just) = self.just.get_mut(key) {
            for held in just.iter_mut() {
                held.reads = held.reads.saturating_add(1);
            }
            just.retain(|held| !listed.contains(held.request_id.as_str()) && held.reads < 2);
            if just.is_empty() {
                self.just.remove(key);
            }
        }
        self.cancelled
            .retain(|(cancelled, id)| cancelled != key || listed.contains(id.as_str()));
        let mut lost = Vec::new();
        let Some(sent) = self.sent.get_mut(key) else {
            return lost;
        };
        for item in details["held_settled"].as_array().into_iter().flatten() {
            let id = item["request_id"].as_str().unwrap_or("");
            let Some(at) = sent.iter().position(|(sent, _)| !id.is_empty() && sent == id) else {
                continue;
            };
            let written = sent.remove(at).map(|(_, text)| text).unwrap_or_default();
            lost.push(Lost {
                text: if written.is_empty() {
                    item["text"].as_str().unwrap_or("").to_owned()
                } else {
                    written
                },
                error: item["error"].as_str().unwrap_or("").to_owned(),
                uncertain: item["status"].as_str() == Some("uncertain"),
            });
        }
        if sent.is_empty() {
            self.sent.remove(key);
        }
        lost
    }

    /// The held rows of `key`, oldest first: the inspection's, then those
    /// just queued that it does not list yet (`renderHeld`).
    pub(crate) fn rows(&self, key: &str, details: Option<&Value>) -> Vec<HeldInputView> {
        let mut rows: Vec<HeldInputView> = details
            .and_then(|details| details["held_inputs"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|item| {
                let id = item["request_id"].as_str().filter(|id| !id.is_empty())?;
                Some(HeldInputView {
                    request_id: id.to_owned(),
                    text: item["text"].as_str().unwrap_or("").to_owned(),
                    cancelling: false,
                })
            })
            .take(PER_SESSION)
            .collect();
        for held in self.just.get(key).into_iter().flatten() {
            if !rows.iter().any(|row| row.request_id == held.request_id) {
                rows.push(HeldInputView {
                    request_id: held.request_id.clone(),
                    text: held.text.clone(),
                    cancelling: false,
                });
            }
        }
        let mark = |row: &HeldInputView| (key.to_owned(), row.request_id.clone());
        rows.retain(|row| !self.cancelled.contains(&mark(row)));
        for row in &mut rows {
            row.cancelling = self.cancelling.contains(&mark(&*row));
        }
        rows
    }

    /// Cancel pressed: false while its cancel already runs.
    pub(crate) fn begin_cancel(&mut self, key: &str, request_id: &str) -> bool {
        self.touch(key);
        self.cancelling
            .insert((key.to_owned(), request_id.to_owned()))
    }

    /// A cancel answered: a confirmed one leaves the bar and is never
    /// brought back; a failed one enables its button again.
    pub(crate) fn end_cancel(&mut self, key: &str, request_id: &str, cancelled: bool) {
        let mark = (key.to_owned(), request_id.to_owned());
        self.cancelling.remove(&mark);
        if !cancelled {
            return;
        }
        if let Some(just) = self.just.get_mut(key) {
            just.retain(|held| held.request_id != request_id);
        }
        if let Some(sent) = self.sent.get_mut(key) {
            sent.retain(|(id, _)| id != request_id);
        }
        self.cancelled.insert(mark);
    }
}

/// an empty editor takes the text; one with a draft
/// keeps the draft and gets the text below it; a draft that already holds
/// it stays as it is.
pub(crate) fn restored(current: &str, lost: &str) -> String {
    if lost.is_empty() || current.contains(lost) {
        current.to_owned()
    } else if current.trim().is_empty() {
        lost.to_owned()
    } else {
        format!("{current}\n\n{lost}")
    }
}

/// `HeldSent::notice`: an uncertain message may have arrived.
pub(crate) fn notice(lost: &Lost) -> Text {
    let error = lost.error.trim();
    if lost.uncertain {
        zerus_client_core::text!(
            "Delivery unconfirmed: check the conversation before sending again. %1",
            error
        )
    } else {
        let error = if error.is_empty() || error.ends_with('.') {
            error.to_owned()
        } else {
            format!("{error}.")
        };
        zerus_client_core::text!("Not delivered: %1 The text is back in the composer.", error)
    }
}

/// hgs's own error as shown: its fixed texts are translated, the rest is
/// shown as it is.
fn prose(detail: String) -> Text {
    for known in [
        held_input::NOT_SENT,
        held_input::INVALID,
        held_input::NOT_CANCELLED,
    ] {
        if detail == known {
            return Text::new(known);
        }
    }
    Text::verbatim(detail)
}

/// Why a failed Enter did not go, or that it may have gone.
pub(crate) fn submit_error(error: DraftError) -> Text {
    if error.uncertain {
        Text::with(held_input::MAYBE_SENT, vec![Arg::from(error.detail)])
    } else {
        prose(error.detail)
    }
}

/// Why a Send that did nothing did nothing, when the composer itself says
/// so: its previous message is unconfirmed, still delivered by the desktop
/// service, or on its way. None otherwise (another refusal shows its own).
pub(crate) fn busy_reason(composer: &Composer) -> Option<Text> {
    if composer.handed.is_some() {
        Some(Text::new(STILL_DELIVERING))
    } else if composer.uncertain {
        Some(Text::new(UNSETTLED))
    } else if composer.sending || composer.waiting.is_some() {
        Some(Text::new(ON_ITS_WAY))
    } else {
        None
    }
}

/// The open session when it is a live main session the details follow: its
/// key, the target its calls name and the details.
fn open_main(hub: &Hub) -> Option<(String, Target, Arc<Value>)> {
    let open = hub.open.as_ref()?;
    let key = hub.open_key.clone()?;
    if open.subagent.is_some()
        || open.archived()
        || hub.followed.computer != open.computer
        || hub.followed.key != open.key
    {
        return None;
    }
    let details = hub.followed.details.clone()?;
    let text = |field: &str| details[field].as_str().unwrap_or("").to_owned();
    let target = Target {
        computer: open.computer.clone(),
        name: open.name.clone(),
        run_id: text("run_id"),
        conversation_id: text("conversation_id"),
    };
    Some((key, target, details))
}

/// The held rows `key`'s view shows: only the open main session's.
pub(crate) fn inputs_view(hub: &Hub, key: &str) -> Vec<HeldInputView> {
    open_main(hub)
        .filter(|(open, _, _)| open == key)
        .map(|(open, _, details)| hub.holds.rows(&open, Some(&*details)))
        .unwrap_or_default()
}

/// The Terminal draft bar `key`'s view shows: only for the open run.
pub(crate) fn draft_view(hub: &Hub, key: &str) -> Option<TerminalDraftView> {
    let (open, target, _) = open_main(hub).filter(|(open, _, _)| open == key)?;
    hub.holds
        .draft
        .as_ref()
        .filter(|draft| draft.same(&open, &target))
        .and_then(Draft::view)
}

/// Hands `text` to the editor under a new epoch, its cursor at the end.
fn give_back(composers: &Composers, composer: &mut Composer, text: String) {
    composer.draft.text = text;
    let end = composer.draft.text.encode_utf16().count() as i64;
    composer.position = end;
    composer.anchor = end;
    composer.text_epoch += 1;
    composers.save(composer);
}

impl Composers {
    /// Sets the runner of the held-message and Terminal draft calls (the
    /// app, at start). Without one those calls fail and change nothing.
    pub fn set_hgs(&self, run: Arc<HgsRun>) {
        *lock(&self.0.hgs) = Some(run);
    }

    /// One blocking call; a missing runner or a panic is a failed start or
    /// an abnormal end, never a success.
    fn call(&self, invocation: &Invocation) -> Exit {
        let Some(run) = lock(&self.0.hgs).clone() else {
            return Exit::StartFailed("hgs is unavailable".into());
        };
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(invocation))).unwrap_or(
            Exit::Abnormal {
                stderr: "The call ended abnormally.".into(),
            },
        )
    }

    /// A send of `key` came back "queued": its local row goes, and the
    /// held bar shows it until an inspection lists it.
    pub(crate) fn held_queued(&self, key: &str, request_id: &str, receipt: &Value, written: &str) {
        let mut hub = self.lock();
        let _ = hub.local.take(key, request_id);
        let text = receipt["text"].as_str().unwrap_or("");
        hub.holds.queued(key, request_id, text, written);
    }

    /// Queues a notice whose prose the webview translates.
    pub(crate) fn said(&self, hub: &mut Hub, text: Text, sticky: bool) {
        self.event(
            hub,
            ComposerEvent::Said {
                text,
                error: true,
                sticky,
            },
        );
    }

    /// A Send that did nothing says why, when its composer knows (the
    /// audit's silent Send after an unconfirmed delivery).
    pub(crate) fn busy_notice(&self, hub: &mut Hub, handle: u64) {
        let reason = hub
            .composers
            .values()
            .find(|composer| composer.handle == handle)
            .and_then(busy_reason);
        if let Some(reason) = reason {
            self.said(hub, reason, false);
        }
    }

    /// An accepted inspection of the open session (`after_observe`): the
    /// just-queued rows age, a lost message comes back to its composer with
    /// why, and while messages wait for an agent that is not busy the
    /// Terminal draft is read, at most every 4 s.
    pub(crate) fn held_observed(&self) {
        let mut hub = self.lock();
        let Some((key, _, details)) = open_main(&hub) else {
            return;
        };
        let lost = hub.holds.observed(&key, &details);
        let mut notices = Vec::new();
        {
            let hub = &mut *hub;
            let returning = hub.holds.returning.entry(key.clone()).or_default();
            returning.extend(lost);
            let free = hub
                .composers
                .get(&key)
                .is_some_and(|composer| composer.loaded && !composer.busy());
            if free && !returning.is_empty() {
                let back = std::mem::take(returning);
                if let Some(composer) = hub.composers.get_mut(&key) {
                    let mut text = composer.draft.text.clone();
                    for item in &back {
                        text = restored(&text, &item.text);
                        notices.push(notice(item));
                    }
                    if text != composer.draft.text {
                        give_back(self, composer, text);
                    }
                }
            }
            if hub.holds.returning.get(&key).is_some_and(Vec::is_empty) {
                hub.holds.returning.remove(&key);
            }
        }
        for text in notices {
            self.said(&mut hub, text, true);
        }
        // A shown draft is read again too: the person may have sent or cleared
        // it in Terminal, and the bar must not outlive the text.
        let shown = hub
            .holds
            .draft
            .as_ref()
            .is_some_and(|draft| draft.key == key && !draft.draft.is_empty());
        let waiting = shown
            || (!hub.holds.rows(&key, Some(&*details)).is_empty()
                && details["activity"].as_str() != Some("busy"));
        drop(hub);
        if waiting {
            self.draft_check(true);
        }
        self.changed();
    }

    /// Reads the open run's Terminal draft: at
    /// once, or with `waiting` at most every 4 s. One call at a time.
    pub(crate) fn draft_check(&self, waiting: bool) {
        let now = self.now();
        let mut hub = self.lock();
        if hub.closed {
            return;
        }
        let Some((key, target, _)) = open_main(&hub) else {
            return;
        };
        if target.name.is_empty() || target.run_id.is_empty() {
            return;
        }
        hub.holds.next += 1;
        let number = hub.holds.next;
        let holds = &mut hub.holds;
        if !holds.draft.as_ref().is_some_and(|draft| draft.same(&key, &target)) {
            holds.draft = Some(Draft::of(&key, &target));
        }
        let Some(draft) = holds.draft.as_mut() else {
            return;
        };
        if draft.request.is_some()
            || (waiting && draft.checked.is_some_and(|at| now - at < DRAFT_EVERY))
        {
            return;
        }
        draft.checked = Some(now);
        draft.request = Some(number);
        drop(hub);
        let request_id = uuid::Uuid::new_v4().to_string();
        let me = self.clone();
        (self.0.deps.spawn)(Box::new(move || {
            let read = held_input::draft_invocation(&target, &request_id, None).and_then(
                |invocation| held_input::classify_draft_read(&request_id, me.call(&invocation)),
            );
            me.draft_read(&key, number, read);
        }));
    }

    /// A draft read answered: an empty draft hides the bar, a draft shows
    /// with its hash, and a failed read keeps the bar as it was.
    fn draft_read(&self, key: &str, number: u64, read: Result<DraftRead, String>) {
        let mut hub = self.lock();
        let Some(draft) = hub
            .holds
            .draft
            .as_mut()
            .filter(|draft| draft.key == key && draft.request == Some(number))
        else {
            return;
        };
        draft.request = None;
        match read {
            Ok(read) if read.draft.is_empty() => draft.clear(),
            Ok(read) => {
                draft.draft = read.draft;
                draft.hash = read.hash;
                draft.error = None;
            }
            Err(_) => {}
        }
        drop(hub);
        self.changed();
    }

    /// Send it (`TerminalDraftBar`'s button): Enter in Terminal for the
    /// draft shown, once. False when nothing started.
    pub fn draft_submit(&self, handle: u64) -> bool {
        let mut hub = self.lock();
        if hub.closed {
            return false;
        }
        let Some((key, target, _)) = open_main(&hub) else {
            return false;
        };
        if !hub.composers.get(&key).is_some_and(|c| c.handle == handle) {
            return false;
        }
        hub.holds.next += 1;
        let number = hub.holds.next;
        let Some(draft) = hub
            .holds
            .draft
            .as_mut()
            .filter(|draft| draft.same(&key, &target))
        else {
            return false;
        };
        if draft.hash.is_empty() || draft.draft.is_empty() || draft.request.is_some() {
            return false;
        }
        draft.request = Some(number);
        draft.submitting = true;
        draft.error = None;
        let hash = draft.hash.clone();
        drop(hub);
        self.changed();
        let request_id = uuid::Uuid::new_v4().to_string();
        let me = self.clone();
        (self.0.deps.spawn)(Box::new(move || {
            let submitted = held_input::draft_invocation(&target, &request_id, Some(&hash))
                .map_err(|detail| DraftError {
                    detail,
                    uncertain: false,
                })
                .and_then(|invocation| {
                    held_input::classify_draft_submit(&request_id, me.call(&invocation))
                });
            me.draft_submitted(&key, number, submitted);
        }));
        true
    }

    /// Send it answered: success hides the bar and reads the session again;
    /// a failure shows why, forgets the hash and reads the draft again.
    fn draft_submitted(&self, key: &str, number: u64, submitted: Result<(), DraftError>) {
        let mut hub = self.lock();
        let Some(draft) = hub
            .holds
            .draft
            .as_mut()
            .filter(|draft| draft.key == key && draft.request == Some(number))
        else {
            return;
        };
        draft.request = None;
        draft.submitting = false;
        let failed = match submitted {
            Ok(()) => {
                draft.clear();
                None
            }
            Err(error) => {
                let text = submit_error(error);
                draft.hash.clear();
                draft.error = Some(text.clone());
                Some(text)
            }
        };
        if let Some(text) = &failed {
            self.said(&mut hub, text.clone(), true);
        }
        drop(hub);
        self.changed();
        if failed.is_some() {
            self.draft_check(false);
        } else {
            self.refresh_details();
        }
    }

    /// Cancel on a held row (`HeldInputBar::cancel`): `hgs send-cancel` for
    /// that one request. False when nothing started: another composer is
    /// open, the row is not shown, or its cancel already runs.
    pub fn held_cancel(&self, handle: u64, request_id: &str) -> bool {
        let mut hub = self.lock();
        if hub.closed {
            return false;
        }
        let Some((key, target, details)) = open_main(&hub) else {
            return false;
        };
        if !hub.composers.get(&key).is_some_and(|c| c.handle == handle) {
            return false;
        }
        let Some(row) = hub
            .holds
            .rows(&key, Some(&*details))
            .into_iter()
            .find(|row| row.request_id == request_id && !row.cancelling)
        else {
            return false;
        };
        if !hub.holds.begin_cancel(&key, request_id) {
            return false;
        }
        drop(hub);
        self.changed();
        let me = self.clone();
        let id = request_id.to_owned();
        (self.0.deps.spawn)(Box::new(move || {
            let cancelled = held_input::cancel_invocation(&target, &id)
                .and_then(|invocation| held_input::classify_cancel(&id, me.call(&invocation)))
                .map(|_| ());
            me.held_cancelled(&key, handle, &id, row.text, cancelled);
        }));
        true
    }

    /// A cancel answered (`heldCancelFinished`): a failure enables the row
    /// again and says why; a cancel puts the text back into an empty
    /// editor. Either way the session is read again.
    fn held_cancelled(
        &self,
        key: &str,
        handle: u64,
        request_id: &str,
        text: String,
        cancelled: Result<(), String>,
    ) {
        let mut hub = self.lock();
        hub.holds.end_cancel(key, request_id, cancelled.is_ok());
        match cancelled {
            Err(error) => {
                let notice = if error == held_input::NOT_CANCELLED {
                    Text::new(held_input::NOT_CANCELLED)
                } else {
                    zerus_client_core::text!("The message could not be cancelled. %1", error)
                };
                self.said(&mut hub, notice, true);
            }
            Ok(()) => {
                if let Some(composer) = hub.composers.get_mut(key).filter(|composer| {
                    composer.handle == handle
                        && composer.loaded
                        && !composer.busy()
                        && composer.draft.text.trim().is_empty()
                }) {
                    give_back(self, composer, text);
                }
            }
        }
        let open = hub.open_key.as_deref() == Some(key);
        drop(hub);
        self.changed();
        if open {
            self.refresh_details();
        }
    }
}

/// Cancel on a held row; false when nothing started.
#[tauri::command(rename_all = "snake_case")]
pub fn held_cancel(state: State<'_, Composers>, handle: u64, request_id: String) -> bool {
    state.held_cancel(handle, &request_id)
}

/// Send it on the Terminal draft bar; false when nothing started.
#[tauri::command]
pub fn terminal_draft_submit(state: State<'_, Composers>, handle: u64) -> bool {
    state.draft_submit(handle)
}

#[cfg(test)]
mod tests;
