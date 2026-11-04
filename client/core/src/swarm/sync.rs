//! The write side of the organization, a port of tray/src/SwarmController.cpp:
//! one `hgs swarm apply {base, desired, versions}` at a time, base and
//! versions from one observed state, edits flushed 120 ms after the last
//! one, and only acknowledged fields advanced. Pure: the hub runs the
//! invocations and passes the clock in (milliseconds).
//!
//! A failed or timed-out apply keeps its edits, and the next tick sends the
//! same base, desired state and versions again, as Qt's `refresh` does
//! (67-73, 96-110). `swarm apply` is a declarative three-way merge guarded by
//! versions: a resend of a diff that already landed adds a head with the same
//! value, which is no conflict, and a stale base surfaces as a conflict
//! (src/swarm/model.rs:126-201, 298-312; the Decisions of
//! docs/superpowers/plans/2026-10-28-tauri-client-organization.md).
//!
//! Pending edits survive a restart as Qt's do: [`Controller::take_draft`]
//! gives the draft file's next content ([`super::draft`]) until
//! [`Controller::draft_written`] confirms it is on disk, and
//! [`Controller::restore`] takes it back at start. Nothing is sent before the
//! first state, and the draft then goes out from its saved base and versions
//! (SwarmController.cpp:76-89, 191-206).
//!
//! Edits wait until the catalog reports `initialized`. Production-switch
//! recovery separately reads Qt's retained organization and calls the existing
//! missing-field initializer (`super::qt_import`); it never treats pending
//! edits or an observed cache as an initialization source.
//!
//! Edits are refused while the controller is not ready, renames included: a
//! rename the list saw before the first state stays with the hub, which hands
//! it over once [`Controller::ready`] holds. `Intent::Renamed` changes nothing
//! when its old name is no member any more, so a late or repeated one is safe.
use super::draft::{Draft, DraftWrite};
use super::edit::{self, Intent};
use super::grouping::SyncView;
use super::members;
use crate::reads::{failure_text, ReadError, NOT_TAKEN};
use crate::views::projects::status_of;
use crate::{Exit, Invocation};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

mod external;
pub use external::{ExternalTicket, EXTERNAL_BUSY, EXTERNAL_CHANGED};

pub use super::STARTING;

/// SwarmController's flush delay (SwarmController.cpp:64).
pub const FLUSH_MS: u64 = 120;
/// SwarmController's poll interval (SwarmController.cpp:55): the tick that
/// sends pending edits again after a failed apply.
pub const REFRESH_MS: u64 = 5_000;
/// How long a state without this client's last write counts as older.
pub const GUARD_MS: u64 = 10_000;
/// HgsClient::requestSwarm's timeout for local catalog commands (HgsClient.cpp:711).
pub const APPLY_TIMEOUT: Duration = Duration::from_secs(10);
pub const NOT_SET_UP: &str =
    "Project sync is not set up on this computer yet. Start Zerus here once to import its projects.";
/// The organization topic ended, or the hub's worker is gone.
pub const STOPPED: &str = "Project sync stopped.";
/// SwarmController.cpp:195.
pub const OUTDATED: &str = "Project sync needs an updated HGS installation.";
/// HgsClient.cpp:713.
pub const INVALID: &str = "Invalid project catalog response";
/// How much of a failed apply's standard error its status keeps (HgsClient.cpp:475).
const MESSAGE_CHARS: usize = 4000;

/// SwarmController.cpp:69.
pub fn unavailable(detail: &str) -> String {
    format!("Project sync unavailable: {detail}")
}

/// One observed catalog: the `swarm get` object without `versions`, its
/// digest, the versions observed with it and its membership.
#[derive(Clone, Debug)]
pub struct Observation {
    pub digest: String,
    pub value: Arc<Value>,
    pub versions: Value,
    pub membership: Arc<BTreeMap<String, String>>,
}

impl Observation {
    /// A `swarm get` or `swarm apply` reply, with its versions taken out.
    pub fn from_reply(mut reply: Value) -> Option<Self> {
        if reply["schema"] != 1 || !reply["organization"].is_object() {
            return None;
        }
        let object = reply.as_object_mut()?;
        let versions = object.remove("versions").unwrap_or_else(|| json!({}));
        object.remove("written");
        let digest = reply["digest"].as_str().unwrap_or("").to_owned();
        let membership = Arc::new(members(&reply["organization"]));
        Some(Self {
            digest,
            value: Arc::new(reply),
            versions,
            membership,
        })
    }
}

pub type Ticket = u64;

/// How an edit ended, for the webview: the outcome of the first apply that carried it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum EditOutcome {
    /// Saved; `conflicts` counts the conflicts on the fields it wrote.
    Done { conflicts: usize },
    /// The apply failed or timed out. The edit stays pending, the list keeps
    /// showing it, and the next tick sends it again. `detail` is Qt's status
    /// line: "Project sync unavailable: …" or the schema text.
    Kept { detail: String },
    /// Not queued: the catalog is not ready, or the edit cannot be made.
    Refused { detail: String },
}

#[derive(Debug, PartialEq)]
pub enum Due {
    Nothing,
    Apply(Invocation),
    /// Edits that changed nothing shared: done without a call.
    Settled(Vec<(Ticket, EditOutcome)>),
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    Done(Value),
    /// A reply of another schema (SwarmController.cpp:194-196).
    Outdated,
    /// Qt's `swarmFailed` (HgsClient.cpp:713-715), with the error.
    Failed(String),
}

/// How an apply ended, read as Qt reads it: a JSON object, else "Invalid
/// project catalog response" (HgsClient.cpp:711-715), then the schema check
/// (SwarmController.cpp:194). Every failure is sent again, so whether a
/// failed apply landed does not change what happens next. The failure texts
/// are the reads' ([`crate::reads`]).
pub fn classify(exit: Exit) -> Reply {
    match exit {
        Exit::Code {
            code: 0, stdout, ..
        } => match serde_json::from_slice::<Value>(&stdout) {
            Ok(reply) if reply["schema"] == 1 && reply["organization"].is_object() => {
                Reply::Done(reply)
            }
            Ok(reply) if reply.is_object() => Reply::Outdated,
            _ => Reply::Failed(INVALID.into()),
        },
        Exit::Code { code, stderr, .. } => {
            Reply::Failed(failure_text(Some(code), &stderr, MESSAGE_CHARS))
        }
        Exit::StartFailed(error) => Reply::Failed(error),
        Exit::TimedOut => Reply::Failed(ReadError::TimedOut.to_string()),
        Exit::Abnormal { stderr } => Reply::Failed(failure_text(None, &stderr, MESSAGE_CHARS)),
        Exit::WriteFailed => Reply::Failed(NOT_TAKEN.into()),
    }
}

/// `hgs swarm apply` with compact JSON input, this computer only (HgsClient.cpp:706-719).
pub fn invocation(input: &Value) -> Invocation {
    Invocation {
        args: vec!["swarm".into(), "apply".into()],
        stdin: Some(serde_json::to_vec(input).expect("the request serializes")),
        timeout: APPLY_TIMEOUT,
        request_id: None,
    }
}

fn saved(tickets: Vec<Ticket>) -> Vec<(Ticket, EditOutcome)> {
    tickets
        .into_iter()
        .map(|ticket| (ticket, EditOutcome::Done { conflicts: 0 }))
        .collect()
}

#[derive(Debug)]
struct Sent {
    desired: Value,
    /// The edits this apply carries; each is answered by it.
    tickets: Vec<Ticket>,
    /// The state held when the apply went out. The source read it before
    /// hgs took the lock for this apply, so it is older than the reply and
    /// goes with it; a failed apply gives it back.
    held: Option<Observation>,
}

/// SwarmController's state (SwarmController.h:37-46).
#[derive(Debug, Default)]
pub struct Controller {
    /// m_base: the organization the edits start from.
    base: Option<Value>,
    value: Option<Arc<Value>>,
    membership: Arc<BTreeMap<String, String>>,
    /// m_versions.
    versions: Map<String, Value>,
    digest: String,
    /// m_desired, when it differs from the base.
    desired: Option<Value>,
    desired_membership: Option<Arc<BTreeMap<String, String>>>,
    /// Edits no apply has carried yet.
    queued: Vec<Ticket>,
    /// m_sent and the request (busy while Some).
    sent: Option<Sent>,
    /// A Join/Resolve reservation in this same controller; never a second writer.
    external: Option<Arc<()>>,
    /// Catalog identity accompanying the external result's causal-head guard.
    external_guard: Option<(String, String)>,
    /// m_flush (SwarmController.cpp:64-65).
    flush_at: Option<u64>,
    /// The next `refresh` that sends `apply` (m_poll, 55, 96-101); a failure arms it.
    refresh_at: Option<u64>,
    /// m_ready: an initialized catalog was observed. Once set, it stays.
    ready: bool,
    /// The organization topic ended: new edits wait for its next state.
    ended: bool,
    /// A draft of the last run waits for the first state (Qt's `start`, 76-89).
    restored: bool,
    /// Counts the changes of the pending edits that the draft file follows.
    draft_change: u64,
    /// The change the draft file holds: the hub confirmed its write.
    draft_saved: u64,
    /// The newest state not adopted: refused while edits were pending or an
    /// apply ran, or held back by the guard. The source sends a state only
    /// when the catalog changes (the topic sends changed rows only, and the
    /// core passes on an object only when it differs), so it does not come
    /// again. Edits that settle without a call take it, where Qt's next tick
    /// reads the catalog once base and desired match (100); a state the
    /// guard held back is adopted when the guard ends, unless a state that
    /// shows the write came first. The source re-reads well inside the
    /// guard, so by then the held state is the newest it has. An apply that
    /// goes out takes it along (`Sent::held`): its reply is newer.
    held: Option<Observation>,
    /// A state already moved this failure streak's tick to now; the next
    /// ones wait for the 5 s tick, as Qt resends at most once per tick.
    tick_pulled: bool,
    /// The operations the last acknowledged write made, per token, until a state shows them.
    guard: BTreeMap<String, Value>,
    guard_until: u64,
    status: Option<(String, bool)>,
    conflicts: usize,
    source_error: Option<String>,
    revision: u64,
    tickets: Ticket,
}

impl Controller {
    /// Whether edits are accepted.
    pub fn ready(&self) -> bool {
        self.ready && !self.ended && self.external.is_none()
    }

    pub fn busy(&self) -> bool {
        self.sent.is_some() || self.external.is_some()
    }

    pub fn pending(&self) -> bool {
        self.desired.is_some()
    }

    /// Changes whenever the presented organization does.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// When `due` next has something to do: the flush, the tick, or the end
    /// of the guard while it holds a state back.
    pub fn next_due(&self) -> Option<u64> {
        if self.external.is_some() {
            return None;
        }
        let guard =
            (self.held.is_some() && !self.busy() && !self.pending()).then_some(self.guard_until);
        [self.flush_at, self.refresh_at, guard]
            .into_iter()
            .flatten()
            .min()
    }

    /// What the list shows: the desired organization while edits are
    /// pending (Qt shows its m_organization), else the base.
    pub fn presented(&self) -> Option<(&Value, Arc<BTreeMap<String, String>>)> {
        match (&self.desired, &self.base) {
            (Some(desired), _) => {
                Some((desired, self.desired_membership.clone().unwrap_or_default()))
            }
            (None, Some(base)) => Some((base, Arc::clone(&self.membership))),
            (None, None) => None,
        }
    }

    /// The last `swarm get` as observed, without pending edits: shared, not
    /// rebuilt.
    pub fn observed_value(&self) -> Option<Arc<Value>> {
        self.value.clone()
    }

    /// The `swarm get` object of what the list shows, for the Projects
    /// screen. While edits are pending it carries the desired organization,
    /// whose `project_order` is the one the apply writes (`edit::shared`):
    /// the edits order `projects` only, and the screen follows
    /// `project_order` first, so a pending create, delete or reorder shows
    /// where hgs will place it.
    pub fn presented_value(&self) -> Option<Arc<Value>> {
        let value = self.value.as_ref()?;
        match &self.desired {
            Some(desired) => Some(Arc::new(showing(value, desired))),
            None => Some(Arc::clone(value)),
        }
    }

    fn reason(&self) -> String {
        if self.external.is_some() {
            return EXTERNAL_BUSY.into();
        }
        if let (None, Some(error)) = (&self.base, &self.source_error) {
            return unavailable(error);
        }
        match &self.value {
            Some(value) if value["initialized"] != true => NOT_SET_UP.into(),
            Some(_) if self.ended => STOPPED.into(),
            _ => STARTING.into(),
        }
    }

    pub fn view(&self) -> SyncView {
        let (status, status_error) = self.status.clone().unwrap_or_default();
        SyncView {
            ready: self.ready(),
            reason: (!self.ready()).then(|| self.reason()),
            busy: self.busy(),
            status,
            status_error,
            conflicts: self.conflicts,
            notice: None,
            layout_save_paused: false,
        }
    }

    fn adopt(&mut self, observation: Observation) {
        self.status = Some(status_of(&observation.value));
        self.conflicts = observation.value["conflicts"]
            .as_array()
            .map_or(0, Vec::len);
        self.ready |= observation.value["initialized"] == true;
        self.base = Some(observation.value["organization"].clone());
        self.versions = observation
            .versions
            .as_object()
            .cloned()
            .unwrap_or_default();
        self.digest = observation.digest;
        self.membership = observation.membership;
        self.value = Some(observation.value);
        self.desired = None;
        self.desired_membership = None;
        self.source_error = None;
        self.revision += 1;
    }

    /// A state from the source (the topic, or the core's `swarm get` poll).
    /// Not adopted while an apply runs or edits are pending
    /// (SwarmController.cpp:204-206), or while it lacks this client's last
    /// write. After a failed apply it is the tick that sends it again, where
    /// Qt's `refresh` would send `apply` instead of reading (96-101).
    ///
    /// A refused state is held ([`Controller`]'s `held`) for when the
    /// edits settle or the guard ends. The first state of a failure streak moves the tick to
    /// now; later ones leave it, so a refused apply goes out at most once
    /// per 5 s however often the catalog changes.
    pub fn observe(&mut self, observation: Observation, now: u64) -> bool {
        self.ended = false;
        if self.busy() {
            self.held = Some(observation);
            return false;
        }
        if self.pending() {
            if self.restored {
                // Qt's first reply after a start with a draft: ready (197), the
                // base kept since it was a read (203), and the flush sends the
                // draft from the saved base and versions (204-205).
                self.ready |= observation.value["initialized"] == true;
                if self.value.is_none() {
                    // Shown, not adopted: Projects gets a catalog (with the
                    // draft over it) and the status line, and `reason` reads
                    // whether the catalog is initialized.
                    self.value = Some(Arc::clone(&observation.value));
                    self.revision += 1;
                }
                if self.ready {
                    self.restored = false;
                    self.flush_at = Some(now + FLUSH_MS);
                }
            } else if self.refresh_at.is_some() && !self.tick_pulled {
                self.refresh_at = Some(now);
                self.tick_pulled = true;
            }
            self.held = Some(observation);
            return false;
        }
        self.offer(observation, now)
    }

    /// Adopts a state unless the guard holds it back: while it runs, a state
    /// without this client's last write may be older. Such a state is held,
    /// and `due` adopts it when the guard ends: a later writer may have
    /// superseded the write, and the source would not send it again.
    fn offer(&mut self, observation: Observation, now: u64) -> bool {
        let wrong_catalog = self.external_guard.as_ref().is_some_and(|(node, swarm)| {
            observation.value["node_id"].as_str() != Some(node.as_str())
                || observation.value["swarm_id"].as_str() != Some(swarm.as_str())
        });
        // Time cannot qualify a late pre-Join catalog. Wait for the source
        // to echo the completed identity; never queue an old one for later.
        if wrong_catalog {
            return false;
        }
        if now < self.guard_until && !shows(&observation, &self.guard) {
            self.held = Some(observation);
            return false;
        }
        self.guard.clear();
        self.external_guard = None;
        self.held = None;
        self.adopt(observation);
        true
    }

    pub fn source_failed(&mut self, message: &str) {
        self.source_error = Some(message.to_owned());
        self.status = Some((unavailable(message), true));
    }

    /// The topic ended (`OrganizationUpdate::Ended`). New edits wait for its
    /// next state; pending ones are still sent, since applies are local.
    pub fn source_ended(&mut self) {
        self.ended = true;
    }

    /// An edit (Qt's `edit`, SwarmController.cpp:90-95) on what the list shows.
    pub fn edit(&mut self, intent: &Intent, now: u64) -> Result<Ticket, String> {
        if !self.ready() {
            return Err(self.reason());
        }
        let current = self
            .desired
            .as_ref()
            .or(self.base.as_ref())
            .ok_or_else(|| self.reason())?;
        let next = edit::apply(current, intent)?;
        self.desired_membership = Some(Arc::new(members(&next)));
        self.desired = Some(next);
        self.draft_change += 1;
        self.revision += 1;
        self.tickets += 1;
        self.queued.push(self.tickets);
        if !self.busy() {
            // Restarted by each edit while idle (94), also after a failure.
            self.flush_at = Some(now + FLUSH_MS);
        }
        Ok(self.tickets)
    }

    /// The flush (SwarmController.cpp:65) or the tick (`refresh`, 96-101).
    /// With nothing pending, the end of the guard adopts the state it held
    /// back and answers `Settled` with no tickets, so the hub publishes the
    /// new revision.
    pub fn due(&mut self, now: u64) -> Due {
        debug_assert!(
            self.pending() || self.queued.is_empty(),
            "every queued edit has a desired state to answer it"
        );
        if !self.busy() && !self.pending() && now >= self.guard_until {
            if let Some(held) = self.held.take() {
                self.offer(held, now);
                return Due::Settled(Vec::new());
            }
        }
        let reached = |at: Option<u64>| at.is_some_and(|at| now >= at);
        if self.busy() || !self.ready || !(reached(self.flush_at) || reached(self.refresh_at)) {
            return Due::Nothing;
        }
        self.flush_at = None;
        self.refresh_at = None;
        let tickets = std::mem::take(&mut self.queued);
        let (Some(base), Some(desired)) = (&self.base, &self.desired) else {
            return Due::Settled(saved(tickets));
        };
        if !edit::changes_shared(base, desired) {
            // m_base == m_desired: nothing to send (65, 100). The list shows
            // the base again, which differs at most in the local order.
            self.desired = None;
            self.desired_membership = None;
            self.draft_change += 1;
            self.revision += 1;
            self.tick_pulled = false;
            // A failed apply's line goes with its edits: the newest state's
            // line again, or the source's failure, where Qt's next tick reads
            // the catalog (100). A held state adopted below replaces it.
            if let Some(error) = &self.source_error {
                self.status = Some((unavailable(error), true));
            } else if let Some(value) = &self.value {
                self.status = Some(status_of(value));
            }
            // Qt reads on its next tick (100); here the state refused under
            // the edits is the newest one, and the source sends no other.
            if let Some(held) = self.held.take() {
                self.offer(held, now);
            }
            return Due::Settled(saved(tickets));
        }
        // send (102-110): the held base, desired and versions. After a
        // failure they are unchanged, so the resend is the same request.
        let input = json!({"base": edit::shared(base), "desired": edit::shared(desired),
                           "versions": Value::Object(self.versions.clone())});
        self.sent = Some(Sent {
            desired: desired.clone(),
            tickets,
            held: self.held.take(),
        });
        Due::Apply(invocation(&input))
    }

    /// The apply's exit (Qt's `received` and `swarmFailed`, SwarmController.cpp:191-219, 67-73):
    /// the answers of the edits it carried.
    pub fn finish(&mut self, exit: Exit, now: u64) -> Vec<(Ticket, EditOutcome)> {
        let Some(sent) = self.sent.take() else {
            return Vec::new();
        };
        match classify(exit) {
            Reply::Done(reply) => self.acknowledged(sent, reply, now),
            Reply::Outdated => self.kept(sent, OUTDATED.into(), now),
            Reply::Failed(error) => self.kept(sent, unavailable(&error), now),
        }
    }

    fn acknowledged(&mut self, sent: Sent, reply: Value, now: u64) -> Vec<(Ticket, EditOutcome)> {
        self.refresh_at = None;
        // A new base and versions for the edits made meanwhile, or none left (persist, 205, 209).
        self.draft_change += 1;
        let written = reply["written"].as_object().cloned().unwrap_or_default();
        for (token, ops) in &written {
            self.versions.insert(token.clone(), ops.clone());
            self.guard.insert(token.clone(), ops.clone());
        }
        if !written.is_empty() {
            self.guard_until = now + GUARD_MS;
        }
        let conflicts = reply["conflicts"].as_array().map_or(0, |list| {
            list.iter()
                .filter(|conflict| {
                    conflict["key"]
                        .as_str()
                        .is_some_and(|key| written.contains_key(key))
                })
                .count()
        });
        let outcome = EditOutcome::Done { conflicts };
        let mut settled: Vec<(Ticket, EditOutcome)> = sent
            .tickets
            .iter()
            .map(|ticket| (*ticket, outcome.clone()))
            .collect();
        self.tick_pulled = false;
        // hgs answers from the store it saved under the write's lock
        // (src/swarm/mod.rs:225-250). A state held when the apply went out
        // was read before that lock and goes with `sent`. One seen while it
        // ran may be older or newer than the reply: another writer may save
        // after the lock is released (an apply, a session assignment, a
        // peer's merge), and its heads need not list this write, since `shows`
        // checks heads, not history (src/swarm/catalog.rs:340-357). It stays
        // held: behind the guard after a write until the source's re-read or
        // the guard's end, and adopted at once without one, where Qt's next
        // `get` would read it (SwarmController.cpp:96-101).
        if self.desired.as_ref() == Some(&sent.desired) {
            self.desired = None;
            self.desired_membership = None;
            // Edits made while it ran that left the desired state as sent:
            // what was written is theirs too, and no later apply carries them.
            settled.extend(
                std::mem::take(&mut self.queued)
                    .into_iter()
                    .map(|ticket| (ticket, outcome.clone())),
            );
        }
        if self.desired.is_some() {
            // Edits made meanwhile go next, from what was sent (203-205).
            self.written(sent.desired);
            self.flush_at = Some(now + FLUSH_MS);
        } else if let Some(observation) = Observation::from_reply(reply) {
            let guard = std::mem::take(&mut self.guard);
            self.adopt(observation);
            self.guard = guard;
            if let Some(held) = self.held.take() {
                if held.digest != self.digest {
                    self.offer(held, now);
                }
            }
        } else {
            self.written(sent.desired);
        }
        settled
    }

    /// What an acknowledged apply wrote becomes the base (Qt's `m_base =
    /// m_sent`, 203) without a state adopted. The membership the list places
    /// rows by, and the catalog Projects shows, follow it: edits that later
    /// settle without a call show this base, and Delete and Sort read its
    /// members.
    fn written(&mut self, desired: Value) {
        self.membership = Arc::new(members(&desired));
        if let Some(value) = &self.value {
            self.value = Some(Arc::new(showing(value, &desired)));
        }
        self.base = Some(desired);
    }

    /// Qt's failure path (67-73) and schema check (194-196): only the status
    /// changes. Base, desired and versions stay, so the next tick sends the
    /// same apply (refresh, 96-101), and edits queued meanwhile ride on it.
    fn kept(&mut self, sent: Sent, status: String, now: u64) -> Vec<(Ticket, EditOutcome)> {
        self.status = Some((status.clone(), true));
        self.refresh_at = Some(now + REFRESH_MS);
        // Nothing was written: the state from before the apply is still the
        // newest unless one came while it ran. Edits that settle take it.
        if self.held.is_none() {
            self.held = sent.held;
        }
        sent.tickets
            .into_iter()
            .map(|ticket| {
                (
                    ticket,
                    EditOutcome::Kept {
                        detail: status.clone(),
                    },
                )
            })
            .collect()
    }

    /// The draft of the last run (Qt's `start`, SwarmController.cpp:76-89):
    /// the saved base, desired state and versions. The list shows the draft at
    /// once. The controller stays not ready, so nothing is sent and no edit is
    /// taken before the first state (`observe`). Ignored once a base is held.
    pub fn restore(&mut self, draft: Draft) {
        if self.base.is_some() {
            return;
        }
        self.membership = Arc::new(members(&draft.base));
        self.desired_membership = Some(Arc::new(members(&draft.desired)));
        self.base = Some(draft.base);
        self.desired = Some(draft.desired);
        self.versions = draft.versions;
        self.restored = true;
        self.revision += 1;
    }

    /// The draft file's next content while the file does not hold the
    /// pending edits' last change: the base, desired state and versions
    /// while edits are pending, else its removal. Qt saves its draft with
    /// every change (SessionsWindow.cpp:2496) and its base and versions with
    /// `persist` (SwarmController.cpp:111-130). It is given again until the
    /// hub confirms the write with [`Controller::draft_written`], as `persist`
    /// remembers only checkpoints that were saved (116-129).
    ///
    /// The number with it names the change it holds, for `draft_written`.
    pub fn take_draft(&mut self) -> Option<(u64, DraftWrite)> {
        if self.draft_change == self.draft_saved {
            return None;
        }
        let write = match (&self.base, &self.desired) {
            (Some(base), Some(desired)) => DraftWrite::Save(
                Draft::new(base.clone(), desired.clone(), self.versions.clone()).to_json(),
            ),
            _ => DraftWrite::Remove,
        };
        Some((self.draft_change, write))
    }

    /// The hub wrote the draft of `change`, as [`Controller::take_draft`]
    /// numbered it: the file holds that change. A later change is still
    /// given, and a stale confirmation never covers it.
    pub fn draft_written(&mut self, change: u64) {
        if change <= self.draft_change {
            self.draft_saved = change;
        }
    }
}

/// `value`, a `swarm get` object, with `organization` in place of its own,
/// whose `project_order` is the one an apply of it writes (`edit::shared`):
/// the edits order `projects` only, and Projects follows `project_order`
/// first.
fn showing(value: &Value, organization: &Value) -> Value {
    let order = edit::project_order(organization);
    let mut organization = organization.clone();
    if let Some(object) = organization.as_object_mut() {
        object.insert("project_order".into(), order);
    }
    let mut copy = value.clone();
    copy["organization"] = organization;
    copy
}

/// Whether `observation`'s heads hold every operation of `written`, per token.
fn shows<'a>(
    observation: &Observation,
    written: impl IntoIterator<Item = (&'a String, &'a Value)>,
) -> bool {
    written.into_iter().all(|(token, ops)| {
        let heads = observation.versions.get(token).and_then(Value::as_array);
        ops.as_array()
            .into_iter()
            .flatten()
            .all(|op| heads.is_some_and(|heads| heads.contains(op)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::edit::Intent;
    use serde_json::json;

    const R: &str = "workstation\nclaude/sample-project/review";
    const LOCAL: &str = "Local catalog. Connect another computer to sync projects.";

    fn review() -> String {
        json!({"kind": "session", "machine": "m-1", "session": "claude/sample-project/review"})
            .to_string()
    }

    fn alpha_name() -> String {
        json!({"kind": "project", "id": "alpha", "field": "name"}).to_string()
    }

    /// `swarm get` with the review session in `project` and its heads; alpha's name has heads `op-a1`.
    fn state(digest: &str, project: &str, heads: &[&str]) -> Value {
        let list = |id: &str| if id == project { json!([R]) } else { json!([]) };
        let mut versions = Map::new();
        versions.insert(review(), json!(heads));
        versions.insert(alpha_name(), json!(["op-a1"]));
        json!({"schema": 1, "node_id": "node-1", "digest": digest, "initialized": true, "machines": [], "peers": {}, "conflicts": [],
               "organization": {"version": 2, "default_project": "ungrouped", "projects": [
                   {"id": "alpha", "name": "Alpha", "color": "#3fb950", "accessible": true, "folders": [], "sessions": list("alpha")},
                   {"id": "beta", "name": "Beta", "color": "#64b5f6", "accessible": true, "folders": [], "sessions": list("beta")},
                   {"id": "ungrouped", "name": "Inbox", "color": "#8d9baa", "accessible": true, "folders": [], "sessions": list("ungrouped")}]},
               "versions": versions})
    }

    fn observation(value: Value) -> Observation {
        Observation::from_reply(value).unwrap()
    }

    fn to(project: &str) -> Intent {
        Intent::Move {
            members: vec![R.into()],
            project: project.into(),
            before: None,
        }
    }

    fn ready() -> Controller {
        let mut sync = Controller::default();
        assert!(sync.observe(observation(state("d1", "alpha", &["op-1"])), 0));
        sync
    }

    fn input(due: Due) -> Value {
        match due {
            Due::Apply(invocation) => {
                assert_eq!(invocation.args, ["swarm", "apply"]);
                assert_eq!(invocation.timeout, APPLY_TIMEOUT);
                serde_json::from_slice(invocation.stdin.as_ref().unwrap()).unwrap()
            }
            _ => panic!("no apply"),
        }
    }

    fn reply(mut value: Value, written: &[&str]) -> Exit {
        let mut map = Map::new();
        if !written.is_empty() {
            map.insert(review(), json!(written));
        }
        value["written"] = Value::Object(map);
        Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(&value).unwrap(),
            stderr: String::new(),
        }
    }

    fn place(sync: &Controller) -> String {
        crate::swarm::members(sync.presented().unwrap().0)[R].clone()
    }

    /// What the hub does after each change: the draft file follows, and the
    /// write succeeds.
    fn written(sync: &mut Controller) -> Option<DraftWrite> {
        let (change, write) = sync.take_draft()?;
        sync.draft_written(change);
        Some(write)
    }

    #[test]
    fn edits_wait_for_an_initialized_catalog_and_nothing_initializes_it() {
        let mut sync = Controller::default();
        assert_eq!(sync.edit(&to("beta"), 0), Err(STARTING.into()));
        let mut fresh = state("d0", "alpha", &["op-1"]);
        fresh["initialized"] = json!(false);
        assert!(sync.observe(observation(fresh), 0));
        assert_eq!(sync.edit(&to("beta"), 0), Err(NOT_SET_UP.into()));
        assert!(matches!(sync.due(10_000), Due::Nothing));
        assert_eq!(sync.view().reason.as_deref(), Some(NOT_SET_UP));
        assert!(sync.presented().is_some(), "the list still groups by it");
    }

    #[test]
    fn edits_flush_together_120_ms_after_the_last_one() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        sync.edit(
            &Intent::Rename {
                project: "beta".into(),
                name: "Beta two".into(),
            },
            100,
        )
        .unwrap();
        assert!(matches!(sync.due(219), Due::Nothing));
        let sent = input(sync.due(220));
        let seen = state("d1", "alpha", &["op-1"]);
        assert_eq!(
            sent["base"],
            crate::swarm::edit::shared(&seen["organization"])
        );
        assert_eq!(sent["versions"], seen["versions"]);
        assert_eq!(crate::swarm::members(&sent["desired"])[R], "beta");
        assert_eq!(
            crate::swarm::projects(&sent["desired"])[1]["name"],
            "Beta two"
        );
        assert!(sync.busy() && sync.view().busy);
        assert!(
            matches!(sync.due(10_000), Due::Nothing),
            "one apply at a time"
        );
    }

    // A folder without an id is keyed by its computer and path on this side
    // only (swarm::folders). The apply carries the catalog's folders as they
    // are: no id is added, and the key never reaches hgs.
    #[test]
    fn a_folder_key_made_here_never_reaches_the_apply() {
        let raw = json!([
            {"machine": "workstation", "path": "/home/example/src/sample-project", "name": "", "machine_id": "m-1"},
            {"id": "f2", "machine": "studio", "path": "/srv/docs", "name": "Docs"}]);
        let mut value = state("d1", "alpha", &["op-1"]);
        value["organization"]["projects"][0]["folders"] = raw.clone();
        let mut sync = Controller::default();
        assert!(sync.observe(observation(value), 0));
        let read = crate::swarm::folders(&crate::swarm::projects(sync.presented().unwrap().0)[0]);
        let key = read[0].key.clone();
        assert_eq!(
            (read[0].id.as_str(), key.as_str()),
            ("", "workstation\n/home/example/src/sample-project")
        );
        sync.edit(
            &Intent::Rename {
                project: "alpha".into(),
                name: "Alpha two".into(),
            },
            0,
        )
        .unwrap();
        sync.edit(&to("beta"), 0).unwrap();
        let Due::Apply(invocation) = sync.due(FLUSH_MS) else {
            panic!("no apply");
        };
        let stdin = invocation.stdin.unwrap();
        let sent: Value = serde_json::from_slice(&stdin).unwrap();
        for side in ["base", "desired"] {
            assert_eq!(sent[side]["projects"][0]["folders"], raw, "{side}");
        }
        assert_eq!(sent["desired"]["projects"][0]["name"], "Alpha two");
        assert_eq!(crate::swarm::members(&sent["desired"])[R], "beta");
        let text = String::from_utf8(stdin).unwrap();
        let escaped = serde_json::to_string(&key).unwrap();
        assert!(!text.contains(&key) && !text.contains(escaped.trim_matches('"')));
    }

    #[test]
    fn edits_during_an_apply_follow_from_what_was_sent_with_only_written_fields_advanced() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let first = input(sync.due(120));
        let second_ticket = sync.edit(&to("ungrouped"), 500).unwrap();
        assert_eq!(sync.next_due(), None, "no timer while busy");
        // Another writer renamed alpha meanwhile: the reply shows it; the user has not seen it.
        let mut answer = state("d2", "beta", &["op-2"]);
        answer["versions"][alpha_name().as_str()] = json!(["op-a2"]);
        assert_eq!(
            sync.finish(reply(answer, &["op-2"]), 900),
            [(1, EditOutcome::Done { conflicts: 0 })]
        );
        assert_eq!(sync.next_due(), Some(1020));
        assert_eq!(
            place(&sync),
            "ungrouped",
            "the list keeps showing the pending edit"
        );
        let second = input(sync.due(1020));
        assert_eq!(second["base"], first["desired"]);
        assert_eq!(second["versions"][review().as_str()], json!(["op-2"]));
        assert_eq!(
            second["versions"][alpha_name().as_str()],
            json!(["op-a1"]),
            "an unseen field keeps the version the user saw"
        );
        let settled = sync.finish(reply(state("d3", "ungrouped", &["op-4"]), &["op-4"]), 1500);
        assert_eq!(
            settled,
            [(second_ticket, EditOutcome::Done { conflicts: 0 })]
        );
        assert_eq!(
            sync.digest(),
            "d3",
            "nothing pending: the reply is the base"
        );
    }

    #[test]
    fn states_wait_for_pending_edits_and_the_reply_is_adopted() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        assert!(
            !sync.observe(observation(state("d2", "ungrouped", &["op-9"])), 50),
            "never merged under an edit"
        );
        assert_eq!(place(&sync), "beta");
        let _ = input(sync.due(120));
        assert!(!sync.observe(observation(state("d2", "ungrouped", &["op-9"])), 200));
        sync.finish(reply(state("d3", "beta", &["op-2"]), &["op-2"]), 300);
        assert_eq!(sync.digest(), "d3");
        assert_eq!(place(&sync), "beta");
    }

    #[test]
    fn a_state_without_our_write_is_older_and_waits_for_one_that_shows_it() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        sync.finish(reply(state("d3", "beta", &["op-2"]), &["op-2"]), 300);
        assert!(
            !sync.observe(observation(state("d1", "alpha", &["op-1"])), 400),
            "read before the write"
        );
        assert!(
            !sync.observe(observation(state("d2", "ungrouped", &["op-9"])), 500),
            "another writer's state without it"
        );
        assert_eq!(sync.digest(), "d3");
        assert!(sync.observe(observation(state("d4", "beta", &["op-2"])), 600));
        assert!(
            sync.observe(observation(state("d5", "alpha", &["op-5"])), 700),
            "the guard ended with the state that showed the write"
        );
    }

    #[test]
    fn the_guard_ends_after_ten_seconds() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        sync.finish(reply(state("d3", "beta", &["op-2"]), &["op-2"]), 300);
        assert!(!sync.observe(
            observation(state("d6", "alpha", &["op-6"])),
            300 + GUARD_MS - 1
        ));
        assert!(
            sync.observe(observation(state("d6", "alpha", &["op-6"])), 300 + GUARD_MS),
            "a later writer superseded ours"
        );
    }

    // SwarmController.cpp:67-73 (failure), 96-101 (refresh), 102-110 (send).
    #[test]
    fn a_failed_apply_keeps_its_edits_and_the_next_tick_sends_them_again() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let first = input(sync.due(120));
        let queued = sync.edit(&to("ungrouped"), 130).unwrap();
        let exit = Exit::Code {
            code: 1,
            stdout: vec![],
            stderr: " hgs: session belongs to more than one project \n".into(),
        };
        let status = "Project sync unavailable: hgs: session belongs to more than one project";
        assert_eq!(
            sync.finish(exit, 200),
            [(
                1,
                EditOutcome::Kept {
                    detail: status.into()
                }
            )],
            "the queued edit rides on the resend"
        );
        assert!(
            sync.pending() && sync.ready(),
            "nothing was dropped and the base stays valid"
        );
        assert_eq!(
            place(&sync),
            "ungrouped",
            "the list keeps showing the edits"
        );
        let view = sync.view();
        assert_eq!((view.status.as_str(), view.status_error), (status, true));
        let tick = 200 + REFRESH_MS;
        assert_eq!(sync.next_due(), Some(tick));
        for now in (200..tick).step_by(100) {
            assert!(
                matches!(sync.due(now), Due::Nothing),
                "nothing before the tick, at {now}"
            );
        }
        let again = input(sync.due(tick));
        assert_eq!(
            again["base"], first["base"],
            "the base the edits started from"
        );
        assert_eq!(
            again["versions"], first["versions"],
            "a failure advances no version"
        );
        assert_eq!(
            crate::swarm::members(&again["desired"])[R],
            "ungrouped",
            "the desired state now"
        );
        assert!(
            matches!(sync.due(tick + 100), Due::Nothing),
            "one apply at a time"
        );
        let settled = sync.finish(
            reply(state("d3", "ungrouped", &["op-3"]), &["op-3"]),
            tick + 200,
        );
        assert_eq!(settled, [(queued, EditOutcome::Done { conflicts: 0 })]);
        assert!(!sync.pending());
        assert_eq!(sync.next_due(), None, "the tick has nothing left to send");
        assert_eq!(sync.view().status, LOCAL);
    }

    // SwarmController.cpp:90-95: an edit restarts the flush, also after a failure.
    #[test]
    fn an_edit_after_a_failure_flushes_120_ms_later_from_the_same_base() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let first = input(sync.due(120));
        sync.finish(Exit::TimedOut, 10_120);
        let ticket = sync
            .edit(
                &Intent::Rename {
                    project: "beta".into(),
                    name: "Beta two".into(),
                },
                10_200,
            )
            .unwrap();
        assert!(matches!(sync.due(10_319), Due::Nothing));
        let again = input(sync.due(10_320));
        assert_eq!(
            (&again["base"], &again["versions"]),
            (&first["base"], &first["versions"])
        );
        assert_eq!(
            crate::swarm::members(&again["desired"])[R],
            "beta",
            "the kept edit"
        );
        assert_eq!(
            crate::swarm::projects(&again["desired"])[1]["name"],
            "Beta two",
            "and the new one"
        );
        let settled = sync.finish(reply(state("d2", "beta", &["op-2"]), &["op-2"]), 10_500);
        assert_eq!(settled, [(ticket, EditOutcome::Done { conflicts: 0 })]);
    }

    // The topic's state is this client's read tick, where Qt's refresh would send the apply.
    #[test]
    fn a_state_after_a_failed_apply_is_the_tick_that_sends_it_again() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let first = input(sync.due(120));
        sync.finish(Exit::TimedOut, 10_120);
        // The topic shows that the first apply landed; it is not adopted under the pending edit.
        assert!(!sync.observe(observation(state("d2", "beta", &["op-2"])), 10_300));
        assert_eq!(
            input(sync.due(10_300)),
            first,
            "sent again at once, unchanged"
        );
        assert!(
            !sync.observe(observation(state("d2", "beta", &["op-2"])), 10_400),
            "nothing while it runs"
        );
        assert!(matches!(sync.due(10_400), Due::Nothing));
        // hgs adds a head with the same value: no conflict (src/swarm/model.rs:298-312).
        let settled = sync.finish(
            reply(state("d3", "beta", &["op-2", "op-3"]), &["op-3"]),
            10_600,
        );
        assert!(
            settled.is_empty(),
            "the edit was answered by the first apply"
        );
        assert_eq!((sync.digest(), sync.view().conflicts), ("d3", 0));
    }

    // SwarmController.cpp:194-196.
    #[test]
    fn an_outdated_reply_keeps_the_edits_with_qt_s_schema_text() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        let exit = Exit::Code {
            code: 0,
            stdout: br#"{"schema":2}"#.to_vec(),
            stderr: String::new(),
        };
        assert_eq!(
            sync.finish(exit, 300),
            [(
                1,
                EditOutcome::Kept {
                    detail: OUTDATED.into()
                }
            )]
        );
        let view = sync.view();
        assert_eq!(
            (view.status.as_str(), view.status_error),
            ("Project sync needs an updated HGS installation.", true)
        );
        assert!(sync.pending());
        assert_eq!(sync.next_due(), Some(300 + REFRESH_MS));
    }

    // Qt's m_base == m_desired (SwarmController.cpp:65, 100): nothing is left to send.
    #[test]
    fn an_edit_back_to_the_base_after_a_failure_ends_the_resend() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        sync.finish(Exit::TimedOut, 10_120);
        let back = sync.edit(&to("alpha"), 10_200).unwrap();
        match sync.due(10_320) {
            Due::Settled(settled) => {
                assert_eq!(settled, [(back, EditOutcome::Done { conflicts: 0 })])
            }
            _ => panic!("nothing left to send"),
        }
        assert!(!sync.pending());
        assert_eq!(sync.next_due(), None);
        assert!(
            sync.observe(observation(state("d2", "alpha", &["op-1"])), 10_400),
            "states are adopted again"
        );
    }

    #[test]
    fn the_end_of_the_source_refuses_new_edits_and_still_sends_pending_ones() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        sync.source_ended();
        assert_eq!(sync.edit(&to("ungrouped"), 50), Err(STOPPED.into()));
        assert_eq!(sync.view().reason.as_deref(), Some(STOPPED));
        let _ = input(sync.due(120));
        sync.finish(reply(state("d2", "beta", &["op-2"]), &["op-2"]), 300);
        assert_eq!(place(&sync), "beta");
        assert!(
            sync.observe(observation(state("d3", "beta", &["op-2"])), 400),
            "a state: the source is back"
        );
        assert!(sync.ready());
    }

    // Qt saves its draft with every change (SessionsWindow.cpp:2496) and its base
    // and versions with persist (SwarmController.cpp:111-130, called at 205 and 209).
    #[test]
    fn the_draft_follows_pending_edits_and_is_removed_when_they_settle() {
        let saved = |change: Option<DraftWrite>| match change {
            Some(DraftWrite::Save(bytes)) => Draft::from_json(&bytes).unwrap(),
            other => panic!("no draft saved: {other:?}"),
        };
        let mut sync = ready();
        assert_eq!(written(&mut sync), None, "nothing pending");
        sync.edit(&to("beta"), 0).unwrap();
        let seen = state("d1", "alpha", &["op-1"]);
        let draft = saved(written(&mut sync));
        assert_eq!(
            draft.base, seen["organization"],
            "the base the edit started from"
        );
        assert_eq!(Value::Object(draft.versions), seen["versions"]);
        assert_eq!(crate::swarm::members(&draft.desired)[R], "beta");
        assert_eq!(written(&mut sync), None, "written once");
        let _ = input(sync.due(120));
        sync.edit(&to("ungrouped"), 200).unwrap();
        assert_eq!(
            crate::swarm::members(&saved(written(&mut sync)).desired)[R],
            "ungrouped",
            "an edit during the apply"
        );
        sync.finish(reply(state("d2", "beta", &["op-2"]), &["op-2"]), 300);
        let draft = saved(written(&mut sync));
        assert_eq!(
            crate::swarm::members(&draft.base)[R],
            "beta",
            "the acknowledged write is the new base"
        );
        assert_eq!(
            draft.versions[review().as_str()],
            json!(["op-2"]),
            "with its written version"
        );
        let _ = input(sync.due(420));
        sync.finish(Exit::TimedOut, 10_420);
        assert_eq!(
            written(&mut sync),
            None,
            "a failure changes nothing pending"
        );
        let _ = input(sync.due(10_420 + REFRESH_MS));
        sync.finish(
            reply(state("d3", "ungrouped", &["op-3"]), &["op-3"]),
            15_600,
        );
        assert_eq!(written(&mut sync), Some(DraftWrite::Remove), "settled");
    }

    // Qt's start with a saved draft (SwarmController.cpp:76-89) and its first reply (191-206).
    #[test]
    fn a_restored_draft_is_shown_and_waits_for_the_first_state() {
        let seen = state("d1", "alpha", &["op-1"]);
        let mut desired = seen["organization"].clone();
        desired["projects"][0]["sessions"] = json!([]);
        desired["projects"][1]["sessions"] = json!([R]);
        let versions = seen["versions"].as_object().unwrap().clone();
        let mut sync = Controller::default();
        sync.restore(Draft::new(seen["organization"].clone(), desired, versions));
        assert_eq!(place(&sync), "beta", "the list shows the draft");
        assert_eq!(sync.edit(&to("ungrouped"), 0), Err(STARTING.into()));
        for now in (0..30_000).step_by(500) {
            assert!(
                matches!(sync.due(now), Due::Nothing),
                "nothing before the first state, at {now}"
            );
        }
        assert_eq!(sync.take_draft(), None, "the file already holds it");
        assert!(
            !sync.observe(observation(state("d2", "alpha", &["op-1"])), 30_000),
            "not adopted under the draft"
        );
        assert!(sync.ready() && sync.pending());
        assert_eq!(sync.next_due(), Some(30_000 + FLUSH_MS));
        let sent = input(sync.due(30_000 + FLUSH_MS));
        assert_eq!(
            sent["base"],
            crate::swarm::edit::shared(&seen["organization"])
        );
        assert_eq!(sent["versions"], seen["versions"]);
        assert_eq!(crate::swarm::members(&sent["desired"])[R], "beta");
    }

    #[test]
    fn conflicts_on_written_fields_are_the_outcome_and_the_status_follows_qt() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        let mut answer = state("d3", "beta", &["op-2", "op-3"]);
        answer["conflicts"] = json!([
            {"key": review(), "field": {"kind": "session"}, "selected": "op-3", "variants": []},
            {"key": alpha_name(), "field": {"kind": "project"}, "selected": null, "variants": []}]);
        assert_eq!(
            sync.finish(reply(answer, &["op-3"]), 300),
            [(1, EditOutcome::Done { conflicts: 1 })]
        );
        let view = sync.view();
        assert_eq!(
            (view.status.as_str(), view.status_error, view.conflicts),
            ("2 project conflict(s) need review in Swarm.", true, 2)
        );
    }

    #[test]
    fn an_edit_that_changes_nothing_shared_sends_nothing() {
        let mut sync = ready();
        let ticket = sync
            .edit(
                &Intent::Move {
                    members: vec![R.into()],
                    project: "alpha".into(),
                    before: None,
                },
                0,
            )
            .unwrap();
        match sync.due(120) {
            Due::Settled(settled) => {
                assert_eq!(settled, [(ticket, EditOutcome::Done { conflicts: 0 })])
            }
            _ => panic!("nothing to send"),
        }
        assert!(!sync.busy());
    }

    #[test]
    fn a_source_failure_keeps_the_base_and_shows_qt_s_text() {
        let mut sync = ready();
        sync.source_failed("catalog unreadable");
        assert_eq!(
            sync.view().status,
            "Project sync unavailable: catalog unreadable"
        );
        assert!(sync.ready());
    }

    #[test]
    fn exits_classify_as_qt_s_reply_schema_check_or_failure() {
        let code = |code: i32, stdout: &[u8], stderr: &str| Exit::Code {
            code,
            stdout: stdout.to_vec(),
            stderr: stderr.into(),
        };
        assert!(matches!(
            classify(code(0, br#"{"schema":1,"organization":{}}"#, "")),
            Reply::Done(_)
        ));
        assert_eq!(classify(code(0, br#"{"schema":2}"#, "")), Reply::Outdated);
        assert_eq!(classify(code(0, br#"{"schema":1}"#, "")), Reply::Outdated);
        assert_eq!(classify(code(0, b"", "")), Reply::Failed(INVALID.into()));
        assert_eq!(classify(code(0, b"[]", "")), Reply::Failed(INVALID.into()));
        assert_eq!(
            classify(code(3, b"", "")),
            Reply::Failed("hgs exited with code 3".into())
        );
        assert_eq!(
            classify(Exit::StartFailed("No such file or directory".into())),
            Reply::Failed("No such file or directory".into())
        );
        assert_eq!(
            classify(Exit::TimedOut),
            Reply::Failed("hgs did not answer in time".into())
        );
        assert_eq!(
            classify(Exit::Abnormal {
                stderr: String::new()
            }),
            Reply::Failed("hgs ended abnormally".into())
        );
        assert_eq!(
            classify(Exit::WriteFailed),
            Reply::Failed("hgs did not take the request".into())
        );
    }

    // Qt's failure detail keeps 4000 characters of standard error (HgsClient.cpp:475).
    #[test]
    fn a_failure_keeps_4000_trimmed_characters_of_standard_error() {
        let long = format!("  {}  ", "e".repeat(5000));
        assert_eq!(
            classify(Exit::Code {
                code: 1,
                stdout: vec![],
                stderr: long.clone()
            }),
            Reply::Failed("e".repeat(4000))
        );
        assert_eq!(
            classify(Exit::Abnormal { stderr: long }),
            Reply::Failed("e".repeat(4000))
        );
    }

    // The Projects screen orders by `project_order` first, so the
    // presented value's order is the desired one while edits are pending.
    #[test]
    fn the_presented_value_orders_projects_as_the_pending_edits_do() {
        let mut seen = state("d1", "alpha", &["op-1"]);
        seen["organization"]["project_order"] = json!(["alpha", "beta", "ungrouped"]);
        let mut sync = Controller::default();
        assert!(sync.observe(observation(seen.clone()), 0));
        assert_eq!(
            sync.presented_value().unwrap()["organization"],
            seen["organization"],
            "nothing pending: the state as observed"
        );
        sync.edit(
            &Intent::Create {
                id: "gamma".into(),
                name: "Gamma".into(),
                members: vec![],
            },
            0,
        )
        .unwrap();
        sync.edit(
            &Intent::Delete {
                project: "beta".into(),
                destination: "ungrouped".into(),
                members: vec![],
            },
            10,
        )
        .unwrap();
        sync.edit(
            &Intent::Reorder {
                project: "alpha".into(),
                before: None,
            },
            20,
        )
        .unwrap();
        let presented = sync.presented_value().unwrap();
        let organization = &presented["organization"];
        let ids: Vec<&str> = crate::swarm::projects(organization)
            .iter()
            .map(crate::swarm::project_id)
            .collect();
        assert_eq!(
            ids,
            ["gamma", "ungrouped", "alpha"],
            "created before the Inbox, beta gone, alpha last"
        );
        assert_eq!(
            organization["project_order"],
            json!(ids),
            "the order the apply will write"
        );
        assert_eq!(
            presented["digest"], "d1",
            "the rest of the state is the observed one"
        );
        // The Projects screen shows that order.
        let mut screen = crate::views::projects::SwarmState::default();
        screen.finish(Ok((*presented).clone()));
        let shown: Vec<String> = screen
            .view(&[])
            .projects
            .into_iter()
            .map(|project| project.id)
            .collect();
        assert_eq!(shown, ["gamma", "ungrouped", "alpha"]);
        let sent = input(sync.due(140));
        assert_eq!(
            sent["desired"]["project_order"],
            organization["project_order"]
        );
    }

    // The hub hands the list's renames over while the controller is ready;
    // before the first state it refuses them like any edit and
    // keeps nothing, so a rename seen then waits with the hub. Handed over
    // later, it migrates the member; once more, it changes nothing.
    #[test]
    fn a_rename_refused_before_the_first_state_migrates_when_handed_over_after_it() {
        const R2: &str = "workstation\nclaude/sample-project/review-2";
        let renamed = Intent::Renamed {
            from: R.into(),
            to: R2.into(),
        };
        let mut sync = Controller::default();
        assert_eq!(sync.edit(&renamed, 0), Err(STARTING.into()));
        assert!(
            !sync.pending() && sync.take_draft().is_none(),
            "nothing kept"
        );
        assert!(sync.observe(observation(state("d1", "alpha", &["op-1"])), 100));
        let ticket = sync.edit(&renamed, 100).unwrap();
        let sent = input(sync.due(220));
        assert_eq!(
            crate::swarm::members(&sent["desired"]),
            BTreeMap::from([(R2.to_owned(), "alpha".to_owned())])
        );
        let mut landed = state("d2", "alpha", &["op-2"]);
        landed["organization"]["projects"][0]["sessions"] = json!([R2]);
        assert_eq!(
            sync.finish(reply(landed, &["op-2"]), 300),
            [(ticket, EditOutcome::Done { conflicts: 0 })]
        );
        let again = sync.edit(&renamed, 400).unwrap();
        match sync.due(520) {
            Due::Settled(settled) => {
                assert_eq!(settled, [(again, EditOutcome::Done { conflicts: 0 })])
            }
            _ => panic!("the old name is no member any more: nothing to send"),
        }
    }

    // The same with a restored draft: the rename waits for the first state,
    // then migrates the member in the draft, which goes out with it.
    #[test]
    fn a_rename_during_a_restored_draft_waits_for_the_first_state_and_rides_on_the_draft() {
        const R2: &str = "workstation\nclaude/sample-project/review-2";
        let renamed = Intent::Renamed {
            from: R.into(),
            to: R2.into(),
        };
        let seen = state("d1", "alpha", &["op-1"]);
        let mut desired = seen["organization"].clone();
        desired["projects"][0]["sessions"] = json!([]);
        desired["projects"][1]["sessions"] = json!([R]);
        let versions = seen["versions"].as_object().unwrap().clone();
        let mut sync = Controller::default();
        sync.restore(Draft::new(seen["organization"].clone(), desired, versions));
        assert_eq!(sync.edit(&renamed, 0), Err(STARTING.into()));
        assert!(!sync.observe(observation(state("d2", "alpha", &["op-1"])), 1_000));
        sync.edit(&renamed, 1_000).unwrap();
        let sent = input(sync.due(1_000 + FLUSH_MS));
        assert_eq!(
            sent["base"],
            crate::swarm::edit::shared(&seen["organization"]),
            "still the saved base"
        );
        assert_eq!(
            crate::swarm::members(&sent["desired"]),
            BTreeMap::from([(R2.to_owned(), "beta".to_owned())])
        );
    }

    /// `state` with alpha under another name, as another writer renamed it.
    fn renamed_alpha(digest: &str, heads: &[&str]) -> Value {
        let mut value = state(digest, "alpha", heads);
        value["organization"]["projects"][0]["name"] = json!("Alpha 2");
        value
    }

    fn alpha(sync: &Controller) -> String {
        crate::swarm::projects(sync.presented().unwrap().0)[0]["name"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    // The source sends a state only when the catalog
    // changes, so one refused under the edits must be taken when they settle
    // without a call, where Qt's next tick reads (SwarmController.cpp:100).
    #[test]
    fn a_state_refused_under_edits_is_adopted_when_they_settle_without_a_call() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        assert!(!sync.observe(observation(renamed_alpha("d2", &["op-1"])), 50));
        let back = sync.edit(&to("alpha"), 60).unwrap();
        match sync.due(180) {
            Due::Settled(settled) => assert_eq!(
                settled,
                [
                    (1, EditOutcome::Done { conflicts: 0 }),
                    (back, EditOutcome::Done { conflicts: 0 })
                ]
            ),
            _ => panic!("nothing to send"),
        }
        assert_eq!(sync.digest(), "d2", "the newest state");
        assert_eq!(alpha(&sync), "Alpha 2");
        assert_eq!(
            sync.presented_value().unwrap()["digest"],
            "d2",
            "and the Projects screen"
        );
        assert_eq!(sync.next_due(), None);
    }

    #[test]
    fn a_restored_draft_that_settles_without_a_call_adopts_the_first_state() {
        let seen = state("d1", "alpha", &["op-1"]);
        let mut sync = Controller::default();
        sync.restore(Draft::new(
            seen["organization"].clone(),
            seen["organization"].clone(),
            Map::new(),
        ));
        assert!(!sync.observe(observation(renamed_alpha("d2", &["op-1"])), 1_000));
        assert!(matches!(sync.due(1_000 + FLUSH_MS), Due::Settled(_)));
        assert_eq!(sync.digest(), "d2");
        assert_eq!(alpha(&sync), "Alpha 2", "the catalog, not the saved base");
        assert!(sync.presented_value().is_some());
        assert_eq!(sync.view().status, LOCAL);
        assert_eq!(written(&mut sync), Some(DraftWrite::Remove));
    }

    // A held state older than this client's write stays out: within the
    // guard, a state without the write is older (the guard's rule).
    #[test]
    fn a_held_state_without_the_acknowledged_write_is_not_adopted_when_edits_settle() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        sync.edit(&to("ungrouped"), 200).unwrap();
        sync.finish(reply(state("d2", "beta", &["op-2"]), &["op-2"]), 300);
        // Read before the write, delivered while the next edit is pending.
        assert!(!sync.observe(observation(state("d1", "alpha", &["op-1"])), 350));
        let back = sync.edit(&to("beta"), 360).unwrap();
        assert!(
            matches!(sync.due(480), Due::Settled(settled) if settled.len() == 2 && settled[1].0 == back)
        );
        assert_eq!(place(&sync), "beta", "the acknowledged write stays");
        assert!(
            sync.observe(observation(state("d3", "beta", &["op-2"])), 500),
            "the next state with the write is adopted"
        );
    }

    // hgs answers from the store it saved under the write's lock
    // (src/swarm/mod.rs:225-250): a state seen during the apply that shows
    // the write is at least as new as the reply.
    #[test]
    fn a_newer_state_seen_during_the_apply_is_adopted_after_its_reply() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        let mut newer = renamed_alpha("d3", &["op-2"]);
        newer["organization"]["projects"][0]["sessions"] = json!([]);
        newer["organization"]["projects"][1]["sessions"] = json!([R]);
        assert!(!sync.observe(observation(newer), 200), "an apply runs");
        sync.finish(reply(state("d2", "beta", &["op-2"]), &["op-2"]), 300);
        assert_eq!(sync.digest(), "d3");
        assert_eq!(
            (place(&sync), alpha(&sync)),
            ("beta".to_owned(), "Alpha 2".to_owned())
        );
        // A state without the write may be older: the reply stays while the
        // guard runs, and the source's re-read, which shows the write, ends it.
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        assert!(!sync.observe(observation(renamed_alpha("d0", &["op-1"])), 200));
        sync.finish(reply(state("d2", "beta", &["op-2"]), &["op-2"]), 300);
        assert_eq!((sync.digest(), alpha(&sync)), ("d2", "Alpha".to_owned()));
        assert_eq!(
            sync.next_due(),
            Some(300 + GUARD_MS),
            "held back by the guard"
        );
        assert!(sync.observe(observation(state("d2", "beta", &["op-2"])), 1_000));
        assert_eq!(sync.next_due(), None);
        assert!(matches!(sync.due(300 + GUARD_MS), Due::Nothing));
        assert_eq!((sync.digest(), alpha(&sync)), ("d2", "Alpha".to_owned()));
    }

    // A state read before an acknowledged write gives way to the source's
    // re-read after it, so edits that settle after the guard ended take the
    // newer one.
    #[test]
    fn a_state_older_than_an_acknowledged_write_gives_way_to_the_source_s_re_read() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        assert!(!sync.observe(observation(state("d0", "alpha", &["op-1"])), 200));
        sync.edit(&to("ungrouped"), 250).unwrap();
        sync.finish(reply(state("d2", "beta", &["op-2"]), &["op-2"]), 300);
        assert!(
            !sync.observe(observation(state("d2", "beta", &["op-2"])), 400),
            "edits are pending"
        );
        let _ = input(sync.due(420));
        sync.finish(Exit::TimedOut, 10_500);
        sync.edit(&to("beta"), 11_000).unwrap();
        assert!(matches!(sync.due(11_120), Due::Settled(_)));
        const { assert!(11_120 > 300 + GUARD_MS, "the guard has ended") };
        assert_eq!((sync.digest(), place(&sync).as_str()), ("d2", "beta"));
    }

    // The source does not send a state again, so
    // one the guard held back is adopted when the guard ends.
    #[test]
    fn a_state_the_guard_held_back_is_adopted_when_it_ends() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        sync.finish(reply(state("d2", "beta", &["op-2"]), &["op-2"]), 300);
        // Another writer superseded op-2 (op-3 has it as parent): heads [op-3].
        assert!(!sync.observe(observation(state("d3", "ungrouped", &["op-3"])), 500));
        assert_eq!(place(&sync), "beta");
        let end = 300 + GUARD_MS;
        assert_eq!(sync.next_due(), Some(end));
        assert!(matches!(sync.due(end - 1), Due::Nothing));
        let revision = sync.revision();
        assert_eq!(sync.due(end), Due::Settled(Vec::new()), "the hub publishes");
        assert!(sync.revision() > revision);
        assert_eq!((sync.digest(), place(&sync).as_str()), ("d3", "ungrouped"));
        assert_eq!(sync.next_due(), None);
    }

    #[test]
    fn an_older_state_the_guard_held_back_gives_way_to_the_re_read_with_the_write() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        sync.finish(reply(state("d2", "beta", &["op-2"]), &["op-2"]), 300);
        // A read started before the write, delivered after the reply.
        assert!(!sync.observe(observation(renamed_alpha("d1b", &["op-1"])), 320));
        assert!(sync.observe(observation(state("d2", "beta", &["op-2"])), 1_400));
        assert_eq!(sync.next_due(), None, "nothing held");
        assert!(matches!(sync.due(300 + GUARD_MS), Due::Nothing));
        assert_eq!((sync.digest(), place(&sync).as_str()), ("d2", "beta"));
        assert_eq!(alpha(&sync), "Alpha");
    }

    // The same for a state seen during the apply: one that superseded the
    // write before the reply came is held back by the guard, not dropped.
    #[test]
    fn a_state_that_superseded_the_write_during_the_apply_is_adopted_when_the_guard_ends() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        assert!(!sync.observe(observation(state("d3", "ungrouped", &["op-3"])), 200));
        sync.finish(reply(state("d2", "beta", &["op-2"]), &["op-2"]), 300);
        assert_eq!(sync.digest(), "d2");
        assert_eq!(sync.due(300 + GUARD_MS), Due::Settled(Vec::new()));
        assert_eq!((sync.digest(), place(&sync).as_str()), ("d3", "ungrouped"));
    }

    // Qt's persist remembers only checkpoints that
    // were saved (SwarmController.cpp:116-129).
    #[test]
    fn a_draft_whose_write_failed_is_given_again_until_one_succeeds() {
        let desired = |offer: Option<(u64, DraftWrite)>| match offer {
            Some((_, DraftWrite::Save(bytes))) => {
                crate::swarm::members(&Draft::from_json(&bytes).unwrap().desired)[R].clone()
            }
            other => panic!("no draft: {other:?}"),
        };
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let first = sync.take_draft();
        assert_eq!(desired(first.clone()), "beta");
        // The hub's write failed: no draft_written.
        assert_eq!(sync.take_draft(), first, "given again on the next tick");
        let (change, _) = first.unwrap();
        sync.draft_written(change);
        assert_eq!(sync.take_draft(), None);
        // The removal is retried the same way.
        sync.edit(&to("alpha"), 50).unwrap();
        assert!(matches!(sync.due(170), Due::Settled(_)));
        let removal = sync.take_draft().unwrap();
        assert_eq!(removal.1, DraftWrite::Remove);
        assert_eq!(
            sync.take_draft(),
            Some((removal.0, DraftWrite::Remove)),
            "the removal failed"
        );
        sync.draft_written(removal.0);
        assert_eq!(sync.take_draft(), None);
    }

    // A confirmation covers only the change it
    // names, so a stale one never hides a newer draft.
    #[test]
    fn a_confirmation_of_an_older_draft_leaves_the_newer_one_given() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let (first, _) = sync.take_draft().unwrap();
        sync.edit(&to("ungrouped"), 10).unwrap();
        let (second, _) = sync.take_draft().unwrap();
        assert!(second > first);
        // The first write succeeds, the second fails.
        sync.draft_written(first);
        let Some((again, DraftWrite::Save(bytes))) = sync.take_draft() else {
            panic!("the newer change is given")
        };
        assert_eq!(again, second);
        assert_eq!(
            crate::swarm::members(&Draft::from_json(&bytes).unwrap().desired)[R],
            "ungrouped"
        );
        sync.draft_written(second);
        assert_eq!(sync.take_draft(), None);
        sync.draft_written(first);
        assert!(
            sync.take_draft().is_some(),
            "the file holds the older change again"
        );
    }

    // Qt resends at most once per 5 s tick.
    #[test]
    fn a_burst_of_states_after_a_failure_moves_the_tick_once_per_streak() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let first = input(sync.due(120));
        sync.finish(Exit::TimedOut, 1_000);
        assert!(!sync.observe(observation(state("d2", "alpha", &["op-1"])), 1_100));
        assert_eq!(input(sync.due(1_100)), first, "the first state is the tick");
        sync.finish(Exit::TimedOut, 1_200);
        let tick = 1_200 + REFRESH_MS;
        for now in (1_300..tick).step_by(100) {
            assert!(!sync.observe(
                observation(state(&format!("d{now}"), "alpha", &["op-1"])),
                now
            ));
            assert!(
                matches!(sync.due(now), Due::Nothing),
                "no resend before the tick, at {now}"
            );
        }
        assert_eq!(sync.next_due(), Some(tick));
        assert_eq!(input(sync.due(tick)), first);
        // An acknowledgement ends the streak: the next failure's first state moves the tick again.
        sync.finish(reply(state("d3", "beta", &["op-2"]), &["op-2"]), tick + 100);
        sync.edit(&to("ungrouped"), tick + 200).unwrap();
        let _ = input(sync.due(tick + 320));
        sync.finish(Exit::TimedOut, tick + 400);
        assert!(!sync.observe(observation(state("d4", "beta", &["op-2"])), tick + 500));
        assert!(matches!(sync.due(tick + 500), Due::Apply(_)));
    }

    // Edits that settle end the failure streak
    // too, so the next failure's first state moves the tick again.
    #[test]
    fn edits_that_settle_end_the_failure_streak() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        sync.finish(Exit::TimedOut, 1_000);
        assert!(!sync.observe(observation(state("d2", "alpha", &["op-1"])), 1_100));
        let _ = input(sync.due(1_100));
        sync.finish(Exit::TimedOut, 1_200);
        sync.edit(&to("alpha"), 1_300).unwrap();
        assert!(
            matches!(sync.due(1_420), Due::Settled(_)),
            "back to the base"
        );
        sync.edit(&to("ungrouped"), 1_500).unwrap();
        let _ = input(sync.due(1_620));
        sync.finish(Exit::TimedOut, 1_700);
        assert!(!sync.observe(observation(state("d3", "alpha", &["op-1"])), 1_800));
        assert!(
            matches!(sync.due(1_800), Due::Apply(_)),
            "a new streak: the state is the tick"
        );
    }

    /// `value` with alpha renamed to "Alpha 2" by another writer, whose name
    /// field has heads `[op]`.
    fn with_alpha2(mut value: Value, op: &str) -> Value {
        value["organization"]["projects"][0]["name"] = json!("Alpha 2");
        value["versions"][alpha_name().as_str()] = json!([op]);
        value
    }

    // The source read a state held when the apply went out
    // before hgs took the apply's lock, so the reply is newer and the state
    // goes with it, even when the source then ends.
    #[test]
    fn a_state_read_before_the_apply_never_outlives_its_reply() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let older = with_alpha2(state("d1b", "alpha", &["op-1"]), "op-a2");
        assert!(!sync.observe(observation(older), 50), "edits are pending");
        let _ = input(sync.due(120));
        let answer = with_alpha2(state("d2", "beta", &["op-2"]), "op-a2");
        sync.finish(reply(answer.clone(), &["op-2"]), 300);
        sync.source_ended();
        assert_eq!(sync.next_due(), None, "nothing held");
        assert!(matches!(sync.due(300 + GUARD_MS), Due::Nothing));
        assert_eq!((sync.digest(), place(&sync).as_str()), ("d2", "beta"));
        assert!(
            sync.observe(observation(answer), 20_000),
            "the source is back"
        );
        assert_eq!((sync.digest(), place(&sync).as_str()), ("d2", "beta"));
    }

    // A state seen during the first apply, with edits made
    // meanwhile, goes with the second apply, whose reply is newer.
    #[test]
    fn a_state_seen_during_an_apply_gives_way_to_the_next_apply_s_reply() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        assert!(!sync.observe(observation(state("d3", "ungrouped", &["op-3"])), 200));
        sync.edit(&to("alpha"), 250).unwrap();
        sync.finish(reply(state("d2", "beta", &["op-2"]), &["op-2"]), 300);
        let _ = input(sync.due(420));
        let settled = sync.finish(
            reply(state("d4", "alpha", &["op-3", "op-4"]), &["op-4"]),
            500,
        );
        assert_eq!(settled, [(2, EditOutcome::Done { conflicts: 0 })]);
        assert_eq!(sync.next_due(), None, "nothing held");
        assert!(matches!(sync.due(500 + GUARD_MS), Due::Nothing));
        assert_eq!((sync.digest(), place(&sync).as_str()), ("d4", "alpha"));
    }

    // Without a write there is no guard; a state seen while
    // the apply ran may be newer (another writer saved after hgs released the
    // lock), and the source does not send it again, so it is adopted, where
    // Qt's next `get` would read it (SwarmController.cpp:96-101).
    #[test]
    fn a_newer_state_seen_during_an_apply_that_wrote_nothing_is_adopted() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        let newer = with_alpha2(state("d3", "beta", &["op-x"]), "op-a2");
        assert!(!sync.observe(observation(newer), 200), "an apply runs");
        // Another device had moved the session to beta already: nothing written.
        sync.finish(reply(state("d2", "beta", &["op-x"]), &[]), 300);
        assert_eq!((sync.digest(), alpha(&sync)), ("d3", "Alpha 2".to_owned()));
        assert_eq!(sync.next_due(), None);
    }

    // A failed apply gives back the state held when it went out, so edits
    // that settle afterwards still take it (the source sends no state
    // again); one seen while the failed apply ran is newer and stays instead.
    #[test]
    fn a_failed_apply_gives_back_the_state_it_took() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let renamed = with_alpha2(state("d2", "alpha", &["op-1"]), "op-a2");
        assert!(!sync.observe(observation(renamed), 50));
        let _ = input(sync.due(120));
        sync.finish(Exit::TimedOut, 1_000);
        sync.edit(&to("alpha"), 1_100).unwrap();
        assert!(matches!(sync.due(1_220), Due::Settled(_)));
        assert_eq!((sync.digest(), alpha(&sync)), ("d2", "Alpha 2".to_owned()));

        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        assert!(!sync.observe(observation(state("d2", "alpha", &["op-1"])), 50));
        let _ = input(sync.due(120));
        let newer = with_alpha2(state("d3", "alpha", &["op-1"]), "op-a3");
        assert!(!sync.observe(observation(newer), 500), "an apply runs");
        sync.finish(Exit::TimedOut, 1_000);
        sync.edit(&to("alpha"), 1_100).unwrap();
        assert!(matches!(sync.due(1_220), Due::Settled(_)));
        assert_eq!((sync.digest(), alpha(&sync)), ("d3", "Alpha 2".to_owned()));
    }

    /// An edit made while an apply runs that leaves the desired state as
    /// sent: moving the session again to the project the apply moves it to.
    #[test]
    fn an_edit_during_an_apply_that_leaves_the_desired_state_is_answered_by_it() {
        let mut sync = ready();
        let first = sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        let second = sync.edit(&to("beta"), 130).unwrap();
        let answered = sync.finish(reply(state("d2", "beta", &["op-2"]), &["op-2"]), 200);
        assert_eq!(
            answered,
            [
                (first, EditOutcome::Done { conflicts: 0 }),
                (second, EditOutcome::Done { conflicts: 0 })
            ],
            "what was written is the second edit's desired state too"
        );
        assert!(!sync.pending());
        assert_eq!(sync.next_due(), None, "nothing left to send");
        assert!(matches!(sync.due(20_000), Due::Nothing));
    }

    const Y: &str = "workstation\nclaude/sample-project/other";

    /// `swarm get` with the review session in `r_in`, and the other session in beta.
    fn two(digest: &str, r_in: &str) -> Value {
        let list = |id: &str| -> Value {
            let mut sessions = vec![];
            if id == r_in {
                sessions.push(json!(R));
            }
            if id == "beta" {
                sessions.push(json!(Y));
            }
            Value::Array(sessions)
        };
        let mut versions = Map::new();
        versions.insert(review(), json!(["op-1"]));
        json!({"schema": 1, "node_id": "node-1", "digest": digest, "initialized": true, "machines": [], "peers": {}, "conflicts": [],
               "organization": {"version": 2, "default_project": "ungrouped", "projects": [
                   {"id": "alpha", "name": "Alpha", "color": "#3fb950", "accessible": true, "folders": [], "sessions": list("alpha")},
                   {"id": "beta", "name": "Beta", "color": "#64b5f6", "accessible": true, "folders": [], "sessions": list("beta")},
                   {"id": "ungrouped", "name": "Inbox", "color": "#8d9baa", "accessible": true, "folders": [], "sessions": []}]},
               "versions": versions})
    }

    /// An order-only edit made while an apply runs settles without a call
    /// once the apply landed: the list then shows what was written, and the
    /// membership the grouping places rows by agrees with it.
    #[test]
    fn an_order_only_follow_up_settles_with_the_membership_of_what_was_written() {
        let mut sync = Controller::default();
        assert!(sync.observe(observation(two("d1", "alpha")), 0));
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        // While the apply runs, the other session moves to the end of its own project.
        let order = Intent::Move {
            members: vec![Y.into()],
            project: "beta".into(),
            before: None,
        };
        sync.edit(&order, 130).unwrap();
        sync.finish(reply(two("d2", "beta"), &["op-2"]), 200);
        assert_eq!(place(&sync), "beta", "while pending");
        assert!(matches!(sync.due(400), Due::Settled(_)));
        let (organization, membership) = sync.presented().unwrap();
        assert_eq!(crate::swarm::members(organization)[R], "beta");
        assert_eq!(
            membership.get(R).map(String::as_str),
            Some("beta"),
            "the membership the grouping uses matches it"
        );
        let value = sync.presented_value().unwrap();
        assert_eq!(
            crate::swarm::members(&value["organization"])[R],
            "beta",
            "Projects shows what was written too"
        );
    }

    /// A kept apply's failure line goes once nothing is pending any more:
    /// the edit moved back before the tick settles without a call.
    #[test]
    fn a_reverted_kept_edit_clears_the_failure_line() {
        let mut sync = ready();
        sync.edit(&to("beta"), 0).unwrap();
        let _ = input(sync.due(120));
        sync.finish(Exit::TimedOut, 10_120);
        assert!(sync.view().status_error);
        sync.edit(&to("alpha"), 11_000).unwrap();
        assert!(matches!(sync.due(11_200), Due::Settled(_)));
        assert!(!sync.pending());
        assert_eq!(sync.view().status, LOCAL, "the newest state's line again");
        assert!(!sync.view().status_error);
    }

    /// A restored draft that fails on every tick still gives Projects the
    /// first state's catalog with the failure line, and an uninitialized
    /// first state is reported as such.
    #[test]
    fn a_restored_draft_that_keeps_failing_reaches_projects() {
        let mut first = ready();
        first.edit(&to("beta"), 0).unwrap();
        let (_, write) = first.take_draft().unwrap();
        let DraftWrite::Save(bytes) = write else {
            panic!("a draft")
        };

        let mut waiting = Controller::default();
        waiting.restore(Draft::from_json(&bytes).unwrap());
        let mut fresh = state("d0", "alpha", &["op-1"]);
        fresh["initialized"] = json!(false);
        assert!(!waiting.observe(observation(fresh), 0));
        assert_eq!(waiting.view().reason.as_deref(), Some(NOT_SET_UP));

        let mut sync = Controller::default();
        sync.restore(Draft::from_json(&bytes).unwrap());
        assert!(sync.presented_value().is_none(), "no state yet");
        let revision = sync.revision();
        assert!(!sync.observe(observation(state("d5", "alpha", &["op-9"])), 1_000));
        assert!(sync.revision() > revision, "Projects gets the catalog");
        let _ = input(sync.due(1_200));
        sync.finish(Exit::TimedOut, 11_200);
        assert!(sync.view().status_error, "the status line is the failure");
        let value = sync.presented_value().expect("a catalog for Projects");
        assert_eq!(value["digest"], "d5");
        assert_eq!(
            crate::swarm::members(&value["organization"])[R],
            "beta",
            "with the draft over it"
        );
    }
}
