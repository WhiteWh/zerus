//! In-memory Undo windows. This owns no source or writer stack: reservations
//! use Actions' existing lane and remain invisible to its send-block facts.
//! Only the main webview can register/cancel. Hiding keeps the deadline;
//! destruction discards it. Durable quit delivery is a separate prerequisite.
use crate::actions::{Actions, Claim};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{self, Write},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::Instant,
};
use tauri::{State, Webview};
use zerus_client_core::{
    creation,
    end_action::{Captured, Window, STATE_CHANGED},
    Action, Target,
};

const WINDOWS: usize = 5;
const TARGETS: usize = 50_000;
const CAPTURE_BYTES: usize = 16 * 1024 * 1024;
const REGISTRATIONS: usize = 16_384;
pub const FIVE_WAITING: &str = "Five actions are waiting. Undo one or wait a moment.";
const TOO_MUCH_DATA: &str = "Too much session data selected at once. Select fewer sessions.";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub id: String,
    pub index: usize,
}
struct Member {
    captured: Captured,
    used: bool,
}
struct Group {
    window: Window,
    waiting: bool,
    ready: bool,
    members: Vec<Member>,
    remaining: usize,
    bytes: usize,
    claims: BTreeMap<(String, String), Arc<Claim>>,
}
#[derive(Default)]
struct Model {
    closed: bool,
    seen: BTreeSet<String>,
    settled: VecDeque<String>,
    bytes: usize,
    groups: BTreeMap<String, Group>,
}

/// Count the exact serialized captures without allocating another JSON body.
struct ByteCount {
    bytes: usize,
    limit: usize,
}
impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self
            .bytes
            .checked_add(bytes.len())
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| io::Error::other(TOO_MUCH_DATA))?;
        self.bytes = count;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn capture_bytes(captures: &[Captured], limit: usize) -> Result<usize, String> {
    let mut count = ByteCount { bytes: 0, limit };
    serde_json::to_writer(&mut count, captures).map_err(|_| TOO_MUCH_DATA.to_owned())?;
    Ok(count.bytes)
}

impl Model {
    fn room_for_id(&mut self) -> bool {
        while self.seen.len() >= REGISTRATIONS {
            let Some(oldest) = self.settled.pop_front() else {
                return false;
            };
            self.seen.remove(&oldest);
        }
        true
    }
    fn remove(&mut self, id: &str) -> Option<Group> {
        let group = self.groups.remove(id)?;
        let Some(bytes) = self.bytes.checked_sub(group.bytes) else {
            self.closed = true;
            self.groups.clear();
            self.bytes = 0;
            return None;
        };
        self.bytes = bytes;
        self.settled.push_back(id.into());
        Some(group)
    }
    fn discard(&mut self) {
        for id in self.groups.keys().cloned().collect::<Vec<_>>() {
            drop(self.remove(&id));
        }
    }
}
#[derive(Default)]
struct Inner {
    model: Mutex<Model>,
    changed: Condvar,
}
#[derive(Clone, Default)]
pub struct Delays(Arc<Inner>);
fn lock(inner: &Inner) -> MutexGuard<'_, Model> {
    inner.model.lock().unwrap_or_else(|error| {
        let mut model = error.into_inner();
        model.closed = true;
        model.groups.clear();
        model.bytes = 0;
        inner.changed.notify_all();
        model
    })
}

impl Delays {
    fn begin(&self, actions: &Actions, id: String, captures: Vec<Captured>) -> Result<(), String> {
        if captures.len() > TARGETS {
            return Err(format!(
                "Too many sessions selected at once ({}). Select at most 50,000.",
                captures.len()
            ));
        }
        if !creation::canonical_uuid(&id)
            || captures.is_empty()
            || captures.iter().any(|c| !c.valid())
        {
            return Err(STATE_CHANGED.into());
        }
        let mut model = lock(&self.0);
        if model.closed || model.seen.contains(&id) {
            return Err("busy".into());
        }
        if model.groups.len() >= WINDOWS {
            return Err(FIVE_WAITING.into());
        }
        let bytes = capture_bytes(&captures, CAPTURE_BYTES.saturating_sub(model.bytes))?;
        if !model.room_for_id() {
            return Err("busy".into());
        }
        let mut claims = BTreeMap::new();
        for c in &captures {
            let key = (c.target.computer.clone(), c.target.name.clone());
            if let std::collections::btree_map::Entry::Vacant(entry) = claims.entry(key) {
                // A partial reservation drops all its claims before returning.
                entry.insert(actions.reserve_end(&c.target).ok_or("busy")?);
            }
        }
        // Retain bounded tombstones for this process: an old waiter/cancel
        // must never bind to a reused caller UUID after Undo or completion.
        model.seen.insert(id.clone());
        model.bytes += bytes;
        let remaining = captures.len();
        model.groups.insert(
            id,
            Group {
                window: Window::new(Instant::now()),
                waiting: false,
                ready: false,
                remaining,
                bytes,
                members: captures
                    .into_iter()
                    .map(|captured| Member {
                        captured,
                        used: false,
                    })
                    .collect(),
                claims,
            },
        );
        Ok(())
    }

    /// Blocking only on a dedicated worker, with an absolute native deadline.
    /// Cancelling removes the group and wakes the worker immediately. Returning
    /// true is timing only: `take` must still find the current exact reservation.
    fn wait(&self, id: &str) -> bool {
        let mut model = lock(&self.0);
        let Some(group) = model.groups.get_mut(id) else {
            return false;
        };
        if group.waiting || group.ready {
            return false;
        }
        group.waiting = true;
        loop {
            let Some(group) = model.groups.get_mut(id) else {
                return false;
            };
            if group.ready {
                return false;
            }
            let now = Instant::now();
            if group.window.take_elapsed(now) {
                group.ready = true;
                return true;
            }
            let remaining = group.window.remaining(now);
            match self.0.changed.wait_timeout(model, remaining) {
                Ok((next, _)) => model = next,
                Err(error) => {
                    (model, _) = error.into_inner();
                    model.closed = true;
                    model.groups.clear();
                    model.bytes = 0;
                    self.0.changed.notify_all();
                }
            }
        }
    }

    pub(crate) fn take(
        &self,
        reference: &Reference,
        target: &Target,
        action: &Action,
    ) -> Result<(Captured, Arc<Claim>), String> {
        self.take_if(reference, |captured| {
            captured.target == *target && captured.action == *action
        })
    }

    pub(crate) fn take_clear(
        &self,
        reference: &Reference,
        run: &str,
        conversation: &str,
        dismiss: bool,
    ) -> Result<(Captured, Arc<Claim>), String> {
        self.take_if(reference, |captured| {
            captured.target.run_id == run
                && captured.target.conversation_id == conversation
                && captured.action
                    == (Action::ClearContext {
                        dismiss_prompt: dismiss,
                    })
        })
    }

    fn take_if(
        &self,
        reference: &Reference,
        matches: impl FnOnce(&Captured) -> bool,
    ) -> Result<(Captured, Arc<Claim>), String> {
        let mut model = lock(&self.0);
        let group = model.groups.get_mut(&reference.id).ok_or(STATE_CHANGED)?;
        let member = group
            .members
            .get_mut(reference.index)
            .ok_or(STATE_CHANGED)?;
        if !group.ready || member.used || !matches(&member.captured) {
            return Err(STATE_CHANGED.into());
        }
        let captured = member.captured.clone();
        let claim = Arc::clone(
            group
                .claims
                .get(&(
                    captured.target.computer.clone(),
                    captured.target.name.clone(),
                ))
                .ok_or(STATE_CHANGED)?,
        );
        member.used = true;
        group.remaining -= 1;
        if group.remaining == 0 {
            model.remove(&reference.id);
        }
        Ok((captured, claim))
    }

    fn cancel(&self, id: &str) {
        let mut model = lock(&self.0);
        // Undo can beat async command registration. Its UUID remains cancelled
        // even then; a late waiter must not recreate the action.
        if creation::canonical_uuid(id) && !model.seen.contains(id) && model.room_for_id() {
            model.seen.insert(id.into());
            model.settled.push_back(id.into());
        }
        if let Some(mut group) = model.remove(id) {
            group.window.cancel();
        }
        self.0.changed.notify_all();
    }

    /// Called by the current exit flush and on destruction. A permanent quit
    /// also refuses later IPC admission; hiding/minimizing does not call this.
    pub(crate) fn cancel_all(&self, quitting: bool) {
        let mut model = lock(&self.0);
        model.closed |= quitting;
        if quitting {
            for group in model.groups.values() {
                if let Some(line) = quit_line(group) {
                    eprintln!("{line}");
                }
            }
        }
        model.discard();
        self.0.changed.notify_all();
    }
}

// At most five bounded names per group, even for a 50,000-member batch.
fn quit_line(group: &Group) -> Option<String> {
    let mut unused = group.members.iter().filter(|member| !member.used);
    let first = unused.next()?;
    let action = match first.captured.action {
        Action::Archive => "Archive",
        Action::Terminate | Action::Kill => "Terminate",
        Action::ClearContext { .. } => "Clear",
        _ => "Forget",
    };
    let name = &first.captured.target.name;
    if group.remaining == 1 {
        return Some(format!(
            "zerus-client: dropped pending {action} for {name:?} on quit"
        ));
    }
    let names = std::iter::once(first)
        .chain(unused)
        .take(5)
        .map(|member| format!("{:?}", member.captured.target.name))
        .collect::<Vec<_>>()
        .join(", ");
    let more = group.remaining.saturating_sub(5);
    Some(format!(
        "zerus-client: dropped pending {action} of {} sessions on quit: {names}{}",
        group.remaining,
        if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        }
    ))
}

fn main_only(window: &Webview) -> Result<(), String> {
    if window.label() != crate::MAIN_WINDOW {
        return Err(STATE_CHANGED.into());
    }
    Ok(())
}

/// Synchronous admission orders the confirmation against main-window close.
/// No hgs, filesystem access, source read or automatic dispatch occurs here.
#[tauri::command]
pub fn action_delay_begin(
    window: Webview,
    state: State<'_, Delays>,
    actions: State<'_, Actions>,
    id: String,
    captures: Vec<Captured>,
) -> Result<(), String> {
    main_only(&window)?;
    if !window.window().is_visible().unwrap_or(false)
        && !window.window().is_minimized().unwrap_or(false)
    {
        return Err(STATE_CHANGED.into());
    }
    state.begin(&actions, id, captures)
}

#[tauri::command]
pub async fn action_delay_wait(
    window: Webview,
    state: State<'_, Delays>,
    id: String,
) -> Result<bool, String> {
    main_only(&window)?;
    let delays = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || delays.wait(&id))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn action_delay_cancel(
    window: Webview,
    state: State<'_, Delays>,
    id: String,
) -> Result<(), String> {
    main_only(&window)?;
    state.cancel(&id);
    Ok(())
}

#[cfg(test)]
mod tests;
