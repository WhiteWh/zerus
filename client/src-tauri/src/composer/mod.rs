//! The composer hub, ported from `tray/src/MessageComposer.cpp`: the open
//! session's draft (A1's `Draft`, held only here, so attachment bytes never
//! cross the IPC as JSON), its durable copy in the app's one draft store (S5,
//! shared with voice), the attachment list, the status line and Send's
//! enablement. The webview gets a `ComposerView` through `next_batch`
//! whenever it changed.
//!
//! Text protocol: the webview owns the text while the user types and sends
//! `composer_edit` with a growing `rev` and the `text_epoch` its text is
//! based on; Rust applies only a higher `rev` of the current epoch. When
//! Rust changes the text itself (a load, an attach or remove, an Undo, a
//! delivery's end, a restore), it bumps `text_epoch` and the webview takes
//! `text`, so an edit typed over the older text cannot bring back or drop a
//! file mention. A locked composer (a rename under way) hands its text back
//! the same way (Qt's `restoreDraft`).
//!
//! Bounded: at most 64 composers and 128 MiB of their attachments in memory
//! (the open one and a busy one are never dropped, nor one whose save is
//! under way or failed; a dropped one reloads from disk); the draft keys the
//! cards mark come from the 200 newest drafts plus the composers held.
//!
//! The hub never holds the last `Arc<DraftStore>`: the app's `Drafts` keeps
//! one for the life of the process, and tests keep theirs, since a store
//! dropped on its own writer thread would wait for itself.

pub mod fetch;
pub mod files;
pub mod questions;
pub mod queue;
pub mod saved;
pub mod send;
#[cfg(feature = "media")]
mod voice;

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use zerus_client_core::attachments::{self, Attachment, Draft, FileRead, Selection};
use zerus_client_core::cache::Chip;
use zerus_client_core::drafts::{DraftStore, IndexPage, Loaded, StoredDraft, ALREADY_EXISTS};
use zerus_client_core::local_messages::{LocalMessage, LocalMessages};
use zerus_client_core::send::{CompactSend, StartSend};
use zerus_client_core::session_usage::ContextChip;
use zerus_client_core::{Action, Batch, Outcome, Target, ViewRow};

use crate::actions::WriteKind;

pub type Job = Box<dyn FnOnce() + Send>;
pub type Spawn = Arc<dyn Fn(Job) + Send + Sync>;
pub type Notify = Arc<dyn Fn() + Send + Sync>;
/// Seconds since the epoch.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
/// (computer, catalog key, name) -> (row, online); a new run is found by name.
pub type Rows = Arc<dyn Fn(&str, &str, &str) -> Option<(ViewRow, bool)> + Send + Sync>;
/// (computer, name) -> the session's write, for the send rules.
pub type Facts = Arc<dyn Fn(&str, &str) -> Option<WriteKind> + Send + Sync>;
/// One `hgs inspect` of the parent or exact child (archive id, then agent id); blocking.
/// The error is Qt's detail for "Could not refresh activity: %1" (`send::inspection`).
pub type Inspect =
    Arc<dyn Fn(&Target, Option<&str>, Option<&str>) -> Result<Value, String> + Send + Sync>;
/// Nudges the existing source's offline retry for one computer; nonblocking.
pub type Reconnect = Arc<dyn Fn(&str) + Send + Sync>;
/// One guarded call (`actions::dispatch_as`); blocking. The String is the
/// request id the delivery made for it.
pub type Dispatch = Arc<dyn Fn(Target, Action, String) -> Result<Outcome, String> + Send + Sync>;

const COMPOSERS: usize = 64;
/// Attachment bytes the idle composers in memory may hold; their drafts stay on disk.
const HELD_BYTES: usize = 128 * 1024 * 1024;
const DRAFT_KEYS: usize = 200;
/// Message text, in UTF-8 bytes (`MaxTextBytes`).
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
const EXCERPT_LINES: usize = 12;
const EXCERPT_CHARS: usize = 1200;
/// How long the exit waits for attach reads under way.
const EXIT_WAIT: Duration = Duration::from_secs(2);
/// A failed save is tried again after this long (Qt's `retrySave` timer).
const SAVE_RETRY: Duration = Duration::from_secs(2);
/// Events of the followed session kept for the local rows' reconcile, as
/// many as Qt keeps in `m_events` (`SessionsWindow.cpp:3651`).
const EVENTS: usize = 500;
/// Sessions whose draft key the hub remembers by catalog key, as S4 keeps
/// the local rows of at most 512 sessions.
const OPENED: usize = 512;
/// At most this many composer events wait for the next batch; older ones go
/// first.
const COMPOSER_EVENTS: usize = 16;

/// What Rust tells the webview besides the views: a notice (Qt's
/// `showNotice`), or the Terminal tab to open. Drained with each batch.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComposerEvent {
    Notice {
        text: String,
        error: bool,
        transient: bool,
    },
    OpenTerminal {
        key: String,
    },
}

pub struct Deps {
    pub spawn: Spawn,
    pub notify: Notify,
    pub clock: Clock,
    pub rows: Rows,
    pub facts: Facts,
    pub inspect: Inspect,
    pub reconnect: Reconnect,
    pub dispatch: Dispatch,
}

/// Where the hub keeps its files.
pub struct Config {
    /// The private attachment cache (`files::Cache`): the app's own cache
    /// folder, never Qt's.
    pub cache: PathBuf,
    /// Qt's draft store ([`qt_store`]), only ever read: Saved drafts' "From
    /// the previous Zerus app", its previews, Recover and Restore.
    pub qt_drafts: Option<PathBuf>,
    /// The dev app's draft stores, only ever read: the previews and Restore
    /// of the drafts the switch import left there (Saved drafts' conflicts).
    pub dev_drafts: Vec<PathBuf>,
}

/// Qt's draft store, read-only, unless it is, holds or sits inside the
/// client's own root: then it is left out, so nothing the client writes can
/// land in Qt's folder. The same rule as the switch import's sources
/// (`drafts::import_sources`), which also compares folders not created yet
/// by their longest existing ancestor.
pub fn qt_store(own: &Path, qt: Option<PathBuf>) -> Option<PathBuf> {
    zerus_client_core::drafts::import_sources(own, qt, Vec::new())
        .into_iter()
        .next()
        .map(|source| source.root)
}

/// The dev app's draft stores, without any that is, holds or sits inside
/// the client's own root.
pub fn dev_stores(own: &Path, dev: Vec<PathBuf>) -> Vec<PathBuf> {
    zerus_client_core::drafts::import_sources(own, None, dev)
        .into_iter()
        .map(|source| source.root)
        .collect()
}

/// One exact native child of a parent conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentRef {
    pub agent_id: String,
    pub parent_conversation_id: String,
}

impl SubagentRef {
    pub(crate) fn valid(&self) -> bool {
        !self.agent_id.is_empty()
            && self.agent_id.len() <= 160
            && !self.agent_id.contains(['/', '\\'])
            && !self.agent_id.chars().any(char::is_control)
            && !self.parent_conversation_id.is_empty()
            && self.parent_conversation_id.len() <= 1024
            && !self.parent_conversation_id.chars().any(char::is_control)
    }
}

/// The session or child a composer belongs to, as the webview opened it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    pub computer: String,
    /// The parent catalog key (`run:...` or `name:...`).
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub archive_id: Option<String>,
    #[serde(default)]
    pub subagent: Option<SubagentRef>,
}

impl SessionRef {
    /// Archive selection is an identity boundary even if an older caller omits its id.
    pub(crate) fn archived(&self) -> bool {
        self.key.starts_with("archive:")
            || self.archive_id.as_ref().is_some_and(|id| !id.is_empty())
    }
}

/// Archived rows are paged outside the live catalog. Give their composers
/// a stopped context without falling back to a live row of the same name.
/// The caller supplies the computer's current online state separately.
pub fn archived_row(key: &str, name: &str) -> Option<ViewRow> {
    let id = key.strip_prefix("archive:")?;
    Some(ViewRow {
        name: name.into(),
        state: "archived".into(),
        archive_id: Some(id.into()),
        ..ViewRow::default()
    })
}

/// Qt's main/archive draft key: `machine\nsession[\narchive]`.
pub fn draft_key(computer: &str, name: &str, archive: Option<&str>) -> String {
    match archive.filter(|id| !id.is_empty()) {
        Some(archive) => format!("{computer}\n{name}\n{archive}"),
        None => format!("{computer}\n{name}"),
    }
}

/// Qt's child key is `machine\nsession\nparent conversation\nagent ID`.
pub fn session_draft_key(session: &SessionRef) -> String {
    session.subagent.as_ref().map_or_else(
        || {
            draft_key(
                &session.computer,
                &session.name,
                session.archive_id.as_deref(),
            )
        },
        |child| {
            format!(
                "{}\n{}\n{}\n{}",
                session.computer, session.name, child.parent_conversation_id, child.agent_id
            )
        },
    )
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AttachmentView {
    pub name: String,
    pub mime: String,
    pub reference: String,
    pub bytes: u64,
    /// The `zerus-file` path of its bytes (`draft/{handle}/{list}/{index}`,
    /// Task 3b); the webview turns it into a URL with `convertFileSrc`.
    pub path: String,
    /// The first lines of a text file, for the preview.
    pub excerpt: Option<String>,
    pub image_size: Option<attachments::image::Size>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SubagentView {
    pub agent_id: String,
    pub send_supported: bool,
    pub hint: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ComposerView {
    pub handle: u64,
    pub key: String,
    /// Present only for a child composer; its target is still the parent run/conversation plus this agent ID.
    pub subagent: Option<SubagentView>,
    pub loaded: bool,
    pub rev: u64,
    pub text_epoch: u64,
    pub text: String,
    pub position: i64,
    pub anchor: i64,
    pub attachments: Vec<AttachmentView>,
    /// The file the last removal took out, while Undo can restore it.
    pub removed: Option<String>,
    pub attaching: bool,
    pub sending: bool,
    pub recording: bool,
    pub waiting: Option<String>,
    pub uncertain: bool,
    pub status: String,
    pub status_error: bool,
    pub can_send: bool,
    /// Interface prose; the webview translates it (`client/src/text.ts`).
    pub send_label: zerus_client_core::text::Text,
    /// The session takes messages: no block reason (`setAvailability`).
    pub available: bool,
    pub unavailable_reason: String,
    /// `messageBlockReason`, None when a message may be sent.
    pub block_reason: Option<String>,
    /// The session's agent takes files (`setAttachmentAvailability`); the
    /// reason is Attach's tooltip and, with files attached, the status.
    pub attachments_available: bool,
    pub attachments_reason: String,
    /// Qt's session status in the composer (`setSessionStatus`): the hint
    /// shown when the computer is offline or the session cannot be read,
    /// whether it is an error, and whether it locks the editor and its
    /// attachments while a block reason stands.
    pub session_status: String,
    pub session_status_error: bool,
    pub session_blocks_input: bool,
    /// The open session's last read failed (Qt's `m_inspectError`): its
    /// Activity reads as unavailable, so the compaction indicator hides
    /// (`SessionsWindow.cpp:3975`).
    pub inspect_error: bool,
    /// The cache chip, its popover's warning and its detail lines.
    pub cache: Chip,
    pub cache_warning: String,
    pub cache_details: String,
    /// The Context chip (R1, Qt's `SessionUsage::ContextButton`).
    pub context: ContextChip,
    /// The session's local message rows (at most 50): what was sent until
    /// the agent's own history shows it, and what failed.
    pub local: Vec<LocalMessage>,
    /// "Cancel send": a start or a compaction waits to send this draft
    /// (`SessionsWindow.cpp:3763`).
    pub cancel_send: bool,
    /// A compaction waits to send this draft (compact-to-send).
    pub compacting: bool,
    /// Clear session and the cache popover's Clear context (`clearContext`).
    pub clear: send::ClearControl,
    /// The agent's question card shown in the composer (A2-1).
    pub question: Option<questions::QuestionView>,
    pub queue: Option<zerus_client_core::input_queue::QueueView>,
    /// The editor and the action row show: false while a required card
    /// stands in for them (`MessageComposer::setInputVisible`).
    pub input_visible: bool,
}

/// One composer's draft and state (`MessageComposer::Draft` plus its sets).
pub(crate) struct Composer {
    pub(crate) handle: u64,
    pub(crate) key: String,
    pub(crate) loaded: bool,
    pub(crate) draft: Draft,
    pub(crate) position: i64,
    pub(crate) anchor: i64,
    pub(crate) label: String,
    pub(crate) form_state: Map<String, Value>,
    pub(crate) rev: u64,
    pub(crate) text_epoch: u64,
    pub(crate) sending: bool,
    pub(crate) recording_owner: Option<String>,
    pub(crate) waiting: Option<String>,
    pub(crate) uncertain: bool,
    pub(crate) error: bool,
    pub(crate) notice: String,
    pub(crate) storage_error: Option<String>,
    pub(crate) removed: Option<(usize, Attachment)>,
    pub(crate) attaching: bool,
    /// A rename's store call runs: the composer is locked, as Qt's loading keys.
    pub(crate) renaming: bool,
    /// Bumps whenever the attachment list changes; the `zerus-file` paths name it.
    pub(crate) attachments_rev: u64,
    /// Bumps with every save (`m_saveRevision`); only the newest save's
    /// answer counts, since a later save replaces a queued one.
    pub(crate) saves: u64,
    /// The newest save has not answered yet (`m_savingKeys`).
    pub(crate) saving: bool,
    /// The newest save failed; it is tried again (`m_failedSaves`).
    pub(crate) save_failed: bool,
    /// A Retry that leaves this draft as it is (Qt's `m_preservedDrafts`):
    /// its error mark and notice, given back when the delivery ends.
    pub(crate) kept: Option<(bool, String)>,
    /// The delivery under way, by the revision of its "sending" save: only
    /// that delivery may call or end; an older one's late steps do nothing.
    /// A continuation's waiting draft holds the revision of its waiting save
    /// here, so its cancel never ends the delivery that followed it.
    pub(crate) delivering: Option<u64>,
    /// The SHA-256 digests of one attachment list (`attachments_rev`), for
    /// the continuations' draft marks: each list is hashed once.
    pub(crate) digests: Option<(u64, Vec<String>)>,
}

impl Composer {
    fn new(handle: u64, key: String) -> Self {
        Composer {
            handle,
            key,
            loaded: false,
            draft: Draft::default(),
            position: 0,
            anchor: 0,
            label: String::new(),
            form_state: Map::new(),
            rev: 0,
            text_epoch: 0,
            sending: false,
            recording_owner: None,
            waiting: None,
            uncertain: false,
            error: false,
            notice: String::new(),
            storage_error: None,
            removed: None,
            attaching: false,
            renaming: false,
            attachments_rev: 0,
            saves: 0,
            saving: false,
            save_failed: false,
            kept: None,
            delivering: None,
            digests: None,
        }
    }

    pub(crate) fn has_content(&self) -> bool {
        !self.draft.text.trim().is_empty() || !self.draft.attachments.is_empty()
    }

    pub(crate) fn busy(&self) -> bool {
        self.sending
            || self.recording_owner.is_some()
            || self.waiting.is_some()
            || self.attaching
            || self.renaming
    }

    /// Only the memory holds a change the disk may lack, so the composer
    /// stays in memory (`trimDraftCache`'s saving and failed keys).
    fn unsaved(&self) -> bool {
        self.saving || self.save_failed
    }

    pub(crate) fn stored(&self) -> StoredDraft {
        StoredDraft {
            draft: self.draft.clone(),
            position: self.position,
            anchor: self.anchor,
            label: self.label.clone(),
            form_state: self.form_state.clone(),
            sending: self.sending,
            uncertain: self.uncertain,
        }
    }

    /// Qt's `showError`: the status line shows it in the error colour.
    pub(crate) fn show_error(&mut self, text: impl Into<String>) {
        self.notice = text.into();
        self.error = true;
    }

    /// The attachment list changed: its files get new `zerus-file` paths.
    pub(crate) fn attachments_changed(&mut self) {
        self.attachments_rev += 1;
    }

    /// Attachment bytes this composer holds, an Undo's file included.
    fn held_bytes(&self) -> usize {
        self.draft
            .attachments
            .iter()
            .map(|a| a.bytes.len())
            .sum::<usize>()
            + self.removed.as_ref().map_or(0, |(_, a)| a.bytes.len())
    }
}

/// The followed session's Activity state, as the batches bring it.
#[derive(Default)]
pub(crate) struct Followed {
    pub(crate) computer: String,
    pub(crate) key: String,
    pub(crate) details: Option<Arc<Value>>,
    /// The cache expiry of `details` in local time, formatted before the
    /// hub lock was taken, since local time may read the time zone files.
    pub(crate) expiry: Option<(i64, String)>,
    /// Why the last inspect failed (`m_inspectError`, the whole text); a
    /// state clears it.
    pub(crate) inspect_error: Option<String>,
    pub(crate) opened_at: f64,
    /// When the last inspect started (Qt's `m_inspectionAge`).
    pub(crate) inspected_at: f64,
    /// The number of the inspect under way.
    pub(crate) inspecting: Option<u64>,
    /// A state frame of this follow arrived: the stream keeps the details
    /// current, and Qt's `inspect()` reads nothing more while it does.
    pub(crate) streamed: bool,
    /// The row's run, conversation and state when the last inspect started.
    pub(crate) inspected: Option<send::Basis>,
    /// Its last 500 Activity events (Qt's `m_events`), for the local rows'
    /// reconcile.
    pub(crate) events: VecDeque<Value>,
}

#[derive(Default)]
pub(crate) struct Hub {
    pub(crate) composers: HashMap<String, Composer>,
    /// Least recently opened first.
    order: VecDeque<String>,
    pub(crate) open: Option<SessionRef>,
    pub(crate) open_key: Option<String>,
    next_handle: u64,
    last_view: Option<ComposerView>,
    /// The change counts the last view and the last draft keys were taken
    /// at: nothing changed since, so `next_batch` builds neither.
    view_taken: Option<u64>,
    keys_taken: Option<u64>,
    /// Keys of the newest stored drafts, newest first, at most 200.
    stored_keys: VecDeque<String>,
    sent_keys: Option<Vec<String>>,
    /// The app quits: no edit, attach or send starts any more.
    pub(crate) closed: bool,
    pub(crate) followed: Followed,
    /// The session part of the open composer's view as the last tick saw
    /// it: the tick wakes the pump only when it changed.
    pub(crate) seen: Option<send::SessionView>,
    /// Numbers the inspects, so a late answer of another session's is dropped.
    next_inspect: u64,
    /// The local message rows of every session, by draft key (S4).
    pub(crate) local: LocalMessages,
    /// Draft keys whose Retry reads its cached files (`m_retryReads`).
    pub(crate) retry_reads: HashSet<String>,
    /// The questions waiting for an answer, by token.
    pub(crate) prompts: HashMap<u64, send::Prompt>,
    pub(crate) next_token: u64,
    /// Computer and catalog key to draft key of the sessions opened, so a
    /// session that leaves the catalog drops its rows; at most 512, the
    /// oldest go first.
    opened: HashMap<(String, String), String>,
    opened_order: VecDeque<(String, String)>,
    /// Resume-then-send: one at a time across all sessions (Qt's `m_startSend`).
    pub(crate) start: Option<(send::Pending, StartSend)>,
    /// Compact-to-send, by draft key (Qt's `m_compacts`).
    pub(crate) compacts: HashMap<String, (send::Pending, CompactSend)>,
    /// When the continuations were last polled (every 2.5 s).
    pub(crate) last_poll: f64,
    /// The draft key whose context reset runs: one at a time across all
    /// sessions (Qt's `m_clearKey` while `m_clearRequest`).
    pub(crate) clearing: Option<String>,
    /// The agent's question cards: their answer drafts and the shown card.
    pub(crate) answers: questions::Answers,
    /// How long a failed read of the open session waits before it is tried
    /// again while another detail tab shows: Qt's `backgroundMs` behind
    /// Terminal or Native UI, `activeMs` behind Processes
    /// (`SessionsWindow.cpp:2013-2016`); None is Activity's 2.5 s.
    pub(crate) poll: Option<f64>,
    pub(crate) queue: queue::Queue,
    /// Events for the next batch (`composer_events`), at most 16.
    pub(crate) events: VecDeque<ComposerEvent>,
}

impl Hub {
    /// Remembers which draft key the catalog row `entry` opened.
    fn remember(&mut self, entry: (String, String), key: &str) {
        self.opened_order.retain(|known| known != &entry);
        self.opened_order.push_back(entry.clone());
        self.opened.insert(entry, key.to_owned());
        while self.opened_order.len() > OPENED {
            if let Some(oldest) = self.opened_order.pop_front() {
                self.opened.remove(&oldest);
            }
        }
    }
}

pub(crate) struct Shared {
    pub(crate) hub: Mutex<Hub>,
    pub(crate) store: Arc<DraftStore>,
    /// The private attachment cache the `zerus-file` scheme serves.
    pub(crate) cache: Arc<files::Cache>,
    /// Qt's draft store, read-only (`Config::qt_drafts`, checked again by
    /// [`qt_store`]).
    pub(crate) qt: Option<PathBuf>,
    /// The dev app's draft stores, read-only.
    pub(crate) dev: Vec<PathBuf>,
    pub(crate) deps: Deps,
    /// Counts `changed` calls.
    changes: AtomicU64,
    /// Wakes the ticker, which sleeps while no composer is open.
    opened: Condvar,
    /// The ticker sleeps (tests wait for it).
    #[cfg(test)]
    parked: std::sync::atomic::AtomicBool,
}

/// The managed state of the composer commands; clones share it.
#[derive(Clone)]
pub struct Composers(pub(crate) Arc<Shared>);

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether `batch` changes the row of `open` or the status of its computer.
fn touches(batch: &Batch, open: &SessionRef) -> bool {
    let here = |computer: &zerus_client_core::ComputerId| computer.0 == open.computer;
    batch
        .upserts
        .iter()
        .any(|out| here(&out.computer) && (out.key == open.key || out.row.name == open.name))
        || batch
            .removes
            .iter()
            .any(|out| here(&out.computer) && out.key == open.key)
        || batch.replaces.iter().any(|out| here(&out.computer))
        || batch.computers.iter().any(|out| here(&out.computer))
}

fn excerpt(attachment: &Attachment) -> Option<String> {
    let text = attachment.mime.starts_with("text/") || attachment.mime == "application/json";
    text.then(|| {
        let head = String::from_utf8_lossy(
            &attachment.bytes[..attachment.bytes.len().min(EXCERPT_CHARS * 4)],
        )
        .into_owned();
        let lines: Vec<&str> = head.lines().take(EXCERPT_LINES).collect();
        lines.join("\n").chars().take(EXCERPT_CHARS).collect()
    })
}

impl Composers {
    /// `drafts` is the app's one draft store (`Drafts` in lib.rs), shared
    /// with voice, so every draft key has one writer.
    pub fn new(drafts: Arc<DraftStore>, config: Config, deps: Deps) -> Self {
        let composers = Composers(Arc::new(Shared {
            hub: Mutex::new(Hub::default()),
            qt: qt_store(drafts.root(), config.qt_drafts),
            dev: dev_stores(drafts.root(), config.dev_drafts),
            store: drafts,
            cache: files::Cache::new(config.cache),
            deps,
            changes: AtomicU64::new(0),
            opened: Condvar::new(),
            #[cfg(test)]
            parked: Default::default(),
        }));
        composers.load_stored_keys();
        composers
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Hub> {
        lock(&self.0.hub)
    }

    /// The private attachment cache.
    pub fn cache(&self) -> Arc<files::Cache> {
        Arc::clone(&self.0.cache)
    }

    /// Wakes the pump: the view or the draft keys changed.
    pub(crate) fn changed(&self) {
        self.0.changes.fetch_add(1, Ordering::SeqCst);
        (self.0.deps.notify)();
    }

    /// `changed` for another owner to call (the write marks' hook,
    /// `Actions::on_writes`). It takes no hub lock and holds the hub only
    /// weakly, so the two never keep each other alive.
    pub fn changed_hook(&self) -> Arc<dyn Fn() + Send + Sync> {
        let hub = Arc::downgrade(&self.0);
        Arc::new(move || {
            if let Some(shared) = hub.upgrade() {
                Composers(shared).changed();
            }
        })
    }

    /// Keys of the 200 newest nonempty drafts, for the cards' draft mark.
    fn load_stored_keys(&self) {
        let me = self.clone();
        self.0.store.index(
            IndexPage {
                limit: DRAFT_KEYS,
                before: None,
            },
            move |page| {
                me.lock().stored_keys = page.items.into_iter().map(|(key, _)| key).collect();
                me.changed();
            },
        );
    }

    /// Opens the selected session's composer (None closes it). The same
    /// catalog row under a new name is a rename: its draft moves (`renameDraft`).
    pub fn open(&self, session: Option<SessionRef>) -> Option<ComposerView> {
        let mut hub = self.lock();
        let next = session.as_ref().map(session_draft_key);
        self.push_selection(&mut hub, next.as_deref());
        let Some(session) = session else {
            hub.open = None;
            hub.open_key = None;
            hub.followed = Followed::default();
            // Qt clears the details with the selection (SessionsWindow.cpp:451):
            // the card shown on a later selection is built again.
            hub.answers.deselect();
            drop(hub);
            self.changed();
            return None;
        };
        let key = session_draft_key(&session);
        let child_open = session.subagent.is_some();
        let renamed = hub
            .open
            .as_ref()
            .filter(|old| {
                old.computer == session.computer
                    && old.key == session.key
                    && old.name != session.name
                    && old.subagent == session.subagent
            })
            .and_then(|_| hub.open_key.clone());
        let child_changed =
            hub.open.as_ref().and_then(|old| old.subagent.as_ref()) != session.subagent.as_ref();
        if hub.followed.computer != session.computer
            || hub.followed.key != session.key
            || child_changed
        {
            hub.followed = Followed {
                computer: session.computer.clone(),
                key: session.key.clone(),
                opened_at: self.now(),
                ..Followed::default()
            };
        }
        hub.remember((session.computer.clone(), session.key.clone()), &key);
        // A start reopened under its new run's catalog key: its continuation
        // follows (the webview hands the selection to the new run).
        if let Some((pending, _)) = hub.start.as_mut().filter(|(p, _)| p.key == key) {
            pending.session = session.clone();
        }
        if let Some((pending, _)) = hub.compacts.get_mut(&key) {
            pending.session = session.clone();
        }
        hub.open = Some(session);
        hub.open_key = Some(key.clone());
        self.0.opened.notify_all();
        if let Some(old) = renamed {
            self.rename(&mut hub, &old, &key);
        }
        self.ensure(&mut hub, &key);
        hub.order.retain(|k| k != &key);
        hub.order.push_back(key.clone());
        self.trim(&mut hub);
        let view = self.view_of(&hub, &key);
        drop(hub);
        self.sync_questions();
        self.changed();
        if child_open {
            self.refresh_details();
        }
        view
    }

    /// Loads a composer that is not in memory (`ensureDraftLoaded`).
    fn ensure(&self, hub: &mut Hub, key: &str) {
        if hub.composers.contains_key(key) {
            return;
        }
        hub.next_handle += 1;
        let composer = Composer::new(hub.next_handle, key.to_owned());
        let handle = composer.handle;
        hub.composers.insert(key.to_owned(), composer);
        let me = self.clone();
        let owned = key.to_owned();
        self.0
            .store
            .load(key, move |loaded| me.loaded(&owned, handle, loaded));
    }

    fn loaded(&self, key: &str, handle: u64, loaded: Loaded) {
        let mut hub = self.lock();
        let Some(composer) = hub
            .composers
            .get_mut(key)
            .filter(|c| c.handle == handle && !c.loaded)
        else {
            return;
        };
        let stored = loaded.draft;
        composer.loaded = true;
        composer.draft = stored.draft;
        composer.position = stored.position;
        composer.anchor = stored.anchor;
        composer.label = stored.label;
        composer.form_state = stored.form_state;
        composer.uncertain = stored.uncertain || stored.sending || loaded.notice.is_some();
        if let Some(notice) = loaded.notice {
            composer.show_error(notice);
        }
        composer.storage_error = loaded.storage_error;
        composer.text_epoch += 1;
        composer.attachments_changed();
        self.trim(&mut hub);
        drop(hub);
        self.changed();
    }

    /// `renameDraft`: the draft moves once the store has it at the new key,
    /// and the composer stays locked until then. A draft already at the new
    /// key is never overwritten (S5's `ALREADY_EXISTS`): it loads instead,
    /// and the moved draft stays under its old key for Saved drafts.
    fn rename(&self, hub: &mut Hub, old: &str, new: &str) {
        let Some(composer) = hub.composers.get(old) else {
            return;
        };
        if composer.busy() || !composer.loaded || composer.storage_error.is_some() {
            return;
        }
        let stored = composer.stored();
        if let Some(here) = hub.composers.get_mut(new) {
            if here.busy() || !here.loaded {
                return;
            }
            if here.has_content() || here.storage_error.is_some() {
                here.show_error(ALREADY_EXISTS);
                return;
            }
            hub.composers.remove(new);
        }
        let mut moved = hub.composers.remove(old).expect("checked above");
        moved.key = new.to_owned();
        moved.renaming = true;
        hub.composers.insert(new.to_owned(), moved);
        let me = self.clone();
        let (from, to) = (old.to_owned(), new.to_owned());
        self.0.store.rename(old, new, stored, move |result| {
            me.renamed(&from, &to, result)
        });
    }

    fn renamed(&self, old: &str, new: &str, result: Result<(), String>) {
        let mut hub = self.lock();
        match result {
            Ok(()) => {
                if let Some(composer) = hub.composers.get_mut(new) {
                    composer.renaming = false;
                    // The rename wrote this very draft: any older save is moot.
                    composer.saving = false;
                    composer.save_failed = false;
                }
                hub.stored_keys.retain(|k| k != old);
                // The session's local rows follow its draft (Qt moves
                // `m_localMessages` with a rename, 1946 and 3312).
                hub.local.rename(old, new);
                // Qt (`MessageComposer.cpp:803-806`): the composer open at the
                // old key (a rename back while this one was pending) follows
                // the draft to the new key. The old key's composer has not
                // loaded yet (its load waits behind the rename), so it goes,
                // as Qt drops the old key's entry.
                if hub.open_key.as_deref() == Some(old) && hub.composers.contains_key(new) {
                    hub.composers.remove(old);
                    hub.open_key = Some(new.to_owned());
                }
                // A composer still held at the old key keeps its place in the
                // eviction order, so it stays within the bounds.
                if !hub.composers.contains_key(old) {
                    hub.order.retain(|k| k != old);
                }
            }
            Err(error) => {
                // Qt (`MessageComposer.cpp:795-798`): the moved draft goes
                // back to its old key marked with the refusal, and the new
                // key reloads with no message.
                if let Some(mut moved) = hub.composers.remove(new) {
                    match hub.composers.get_mut(old) {
                        None => {
                            moved.key = old.to_owned();
                            moved.renaming = false;
                            moved.storage_error = Some(error);
                            // Its saves answered under the new key and were
                            // ignored: save it again. Only a failure replaces
                            // the refusal; a later save clears it, as in Qt.
                            self.save_with(&mut moved, true);
                            hub.composers.insert(old.to_owned(), moved);
                            hub.order.retain(|k| k != old);
                            hub.order.push_front(old.to_owned());
                        }
                        // A rename back while this one was pending opened a
                        // newer composer at the old key. Its load waits
                        // behind the rename, so it has not loaded: it takes
                        // the moved draft, whose last save may never have
                        // reached the disk, and keeps its handle. Its load
                        // answer comes later and is ignored.
                        Some(newer) if !newer.loaded => {
                            newer.draft = moved.draft;
                            newer.position = moved.position;
                            newer.anchor = moved.anchor;
                            newer.label = moved.label;
                            newer.form_state = moved.form_state;
                            // An uncertain delivery stays uncertain.
                            newer.uncertain = moved.uncertain;
                            newer.error = moved.error;
                            newer.notice = moved.notice;
                            newer.removed = moved.removed;
                            newer.loaded = true;
                            newer.storage_error = Some(error);
                            newer.text_epoch = newer.text_epoch.max(moved.text_epoch) + 1;
                            newer.attachments_changed();
                            self.save_with(newer, true);
                        }
                        // Not reached: a newer composer's load waits behind
                        // the rename on the in-order writer.
                        Some(_) => {}
                    }
                }
                if hub.open_key.as_deref() == Some(new) {
                    self.ensure(&mut hub, new);
                }
            }
        }
        drop(hub);
        self.changed();
    }

    /// Drops least recently opened composers past the bounds (`trimDraftCache`,
    /// plus a byte bound Qt's unbounded cache lacks); a dropped composer's key
    /// joins or leaves the stored keys, so its card's draft mark stays right.
    fn trim(&self, hub: &mut Hub) {
        let held = |hub: &Hub| -> usize { hub.composers.values().map(Composer::held_bytes).sum() };
        while hub.composers.len() > COMPOSERS || held(hub) > HELD_BYTES {
            let open = hub.open_key.clone();
            let Some(index) = hub.order.iter().position(|key| {
                Some(key) != open.as_ref()
                    && hub
                        .composers
                        .get(key)
                        .is_none_or(|c| c.loaded && !c.busy() && !c.unsaved())
            }) else {
                break;
            };
            let key = hub.order.remove(index).expect("found above");
            if let Some(gone) = hub.composers.remove(&key) {
                hub.stored_keys.retain(|k| k != &key);
                if gone.has_content() {
                    hub.stored_keys.push_front(key);
                    hub.stored_keys.truncate(DRAFT_KEYS);
                }
            }
        }
    }

    pub(crate) fn by_handle(hub: &mut Hub, handle: u64) -> Option<&mut Composer> {
        hub.composers.values_mut().find(|c| c.handle == handle)
    }

    /// The composer of `handle` when it takes a change: loaded, not busy, and
    /// the app not quitting.
    pub(crate) fn editable(hub: &mut Hub, handle: u64) -> Option<&mut Composer> {
        if hub.closed {
            return None;
        }
        Self::by_handle(hub, handle).filter(|c| c.loaded && !c.busy())
    }

    /// Whether `handle` is the open composer and takes files (`canAttachFiles`).
    pub fn can_attach(&self, handle: u64) -> bool {
        let mut hub = self.lock();
        let open = hub.open_key.clone();
        self.session_allows(&hub, handle, true)
            && Self::editable(&mut hub, handle).is_some_and(|c| Some(&c.key) == open.as_ref())
    }

    /// The session's part of Qt's `canEditAttachments` and `canAttachFiles`
    /// (`MessageComposer.h:108-109`) for the open composer: its session
    /// status does not lock input and, for new `files`, its agent takes
    /// them. Another composer has no session part.
    fn session_allows(&self, hub: &Hub, handle: u64, files: bool) -> bool {
        let Some(composer) = hub
            .open_key
            .as_ref()
            .and_then(|key| hub.composers.get(key))
            .filter(|c| c.handle == handle)
        else {
            return true;
        };
        let session = send::session_view(self, hub, composer);
        !session.session_blocks_input && (!files || session.attachments_available)
    }

    /// Saves as the user types; a later save of the same key replaces it.
    pub(crate) fn save(&self, composer: &mut Composer) {
        self.save_with(composer, false);
    }

    /// `keep_error`: a success leaves the storage error shown (a refused
    /// rename's mark survives the hub's own save of the moved draft).
    fn save_with(&self, composer: &mut Composer, keep_error: bool) {
        composer.saves += 1;
        composer.saving = true;
        let me = self.clone();
        let (key, handle, revision) = (composer.key.clone(), composer.handle, composer.saves);
        self.0
            .store
            .save(&composer.key, composer.stored(), move |result| {
                me.saved(&key, handle, revision, keep_error, result)
            });
    }

    fn saved(
        &self,
        key: &str,
        handle: u64,
        revision: u64,
        keep_error: bool,
        result: Result<(), String>,
    ) {
        let mut hub = self.lock();
        let Some(composer) = hub
            .composers
            .get_mut(key)
            .filter(|c| c.handle == handle && c.saves == revision)
        else {
            return;
        };
        composer.saving = false;
        composer.save_failed = result.is_err();
        if result.is_err() || !keep_error {
            composer.storage_error = result.err();
        }
        let retry = composer.save_failed;
        self.trim(&mut hub);
        drop(hub);
        if retry {
            self.retry_save(key.to_owned(), handle, revision);
        }
        self.changed();
    }

    /// Qt's `retrySave`: two seconds after a save failed, the same draft is
    /// saved again, unless a newer save went meanwhile or the app quits.
    fn retry_save(&self, key: String, handle: u64, revision: u64) {
        let me = self.clone();
        (self.0.deps.spawn)(Box::new(move || {
            std::thread::sleep(SAVE_RETRY);
            let mut hub = me.lock();
            if hub.closed {
                return;
            }
            if let Some(composer) = hub
                .composers
                .get_mut(&key)
                .filter(|c| c.handle == handle && c.saves == revision && c.save_failed)
            {
                me.save(composer);
            }
        }));
    }

    /// Qt's `restoreDraft`: a change the composer cannot take now (a rename
    /// under way, a remove or Undo it refuses) hands its text back under a
    /// new epoch, so the webview takes the text again and unlocks its editor.
    fn hand_back(&self, mut hub: MutexGuard<'_, Hub>, handle: u64) {
        let mut handed_back = false;
        if !hub.closed {
            if let Some(composer) = Self::by_handle(&mut hub, handle).filter(|c| c.loaded) {
                composer.text_epoch += 1;
                handed_back = true;
            }
        }
        drop(hub);
        if handed_back {
            self.changed();
        }
    }

    /// The editor changed (`MessageComposer.cpp:367-379`). `epoch` is the
    /// `text_epoch` the webview's text is based on; an edit over older text
    /// is ignored, and the webview takes the newer text from the view.
    pub fn edit(
        &self,
        handle: u64,
        rev: u64,
        epoch: u64,
        text: String,
        position: i64,
        anchor: i64,
    ) {
        let mut hub = self.lock();
        let Some(composer) = Self::editable(&mut hub, handle) else {
            return self.hand_back(hub, handle);
        };
        if epoch != composer.text_epoch || rev <= composer.rev {
            return;
        }
        composer.rev = rev;
        composer.draft.text = text;
        composer.position = position;
        composer.anchor = anchor;
        if !composer.error && !composer.uncertain {
            composer.notice.clear();
        }
        self.save(composer);
        drop(hub);
        self.changed();
    }

    /// Attaches chosen or dropped files, one after another, off the UI thread.
    /// A file that fails shows its error and the next one still attaches, as
    /// Qt's `attachFiles` loop does.
    pub fn attach_paths(&self, handle: u64, paths: Vec<PathBuf>, selection: Selection) {
        if !self.begin_attach(handle) {
            return;
        }
        let me = self.clone();
        (self.0.deps.spawn)(Box::new(move || {
            let mut at = selection;
            for path in paths {
                let read = attachments::read_file(&path);
                if !me.add(handle, read, &mut at) {
                    break;
                }
            }
            me.end_attach(handle);
        }));
    }

    /// A pasted image (`attachImage`): PNG bytes from a raw IPC body, checked in the core.
    pub fn paste_image(&self, handle: u64, png: Vec<u8>, selection: Selection) {
        if !self.begin_attach(handle) {
            return;
        }
        let me = self.clone();
        (self.0.deps.spawn)(Box::new(move || {
            let mut at = selection;
            me.add(handle, attachments::pasted_image(png), &mut at);
            me.end_attach(handle);
        }));
    }

    fn begin_attach(&self, handle: u64) -> bool {
        let mut hub = self.lock();
        if !self.session_allows(&hub, handle, true) {
            return false;
        }
        let Some(composer) = Self::editable(&mut hub, handle) else {
            return false;
        };
        composer.attaching = true;
        drop(hub);
        self.changed();
        true
    }

    /// `addAttachment`, or the read's error. False ends the batch: the
    /// composer is gone, or the app quits (the read under way still counts).
    fn add(&self, handle: u64, read: Result<FileRead, String>, at: &mut Selection) -> bool {
        let mut hub = self.lock();
        let more = !hub.closed;
        let Some(composer) = Self::by_handle(&mut hub, handle) else {
            return false;
        };
        let added =
            read.and_then(|file| composer.draft.add(&file.name, &file.mime, file.bytes, *at));
        match added {
            Ok(added) => {
                *at = Selection {
                    start: added.cursor,
                    end: added.cursor,
                };
                composer.position = added.cursor as i64;
                composer.anchor = added.cursor as i64;
                composer.text_epoch += 1;
                composer.attachments_changed();
                if !composer.uncertain {
                    composer.notice.clear();
                    composer.error = false;
                }
                self.save(composer);
            }
            Err(error) => composer.show_error(error),
        }
        drop(hub);
        self.changed();
        more
    }

    fn end_attach(&self, handle: u64) {
        let mut hub = self.lock();
        if let Some(composer) = Self::by_handle(&mut hub, handle) {
            composer.attaching = false;
        }
        self.trim(&mut hub);
        drop(hub);
        self.changed();
    }

    /// `removeAttachment`: the file and every mention of its reference. It
    /// always answers with a new text epoch, so the webview's editor, locked
    /// for it, unlocks.
    pub fn remove(&self, handle: u64, index: usize) {
        let mut hub = self.lock();
        if !self.session_allows(&hub, handle, false) {
            return self.hand_back(hub, handle);
        }
        let Some(composer) = Self::editable(&mut hub, handle) else {
            return self.hand_back(hub, handle);
        };
        composer.text_epoch += 1;
        if let Some(removed) = composer.draft.remove(index) {
            composer.removed = Some((index, removed));
            composer.attachments_changed();
            self.save(composer);
        }
        drop(hub);
        self.changed();
    }

    /// Undo in the attachment list (the approved mockup): the file returns
    /// to its place and its reference to the end of the text. Like a remove,
    /// it always answers with a new text epoch.
    pub fn undo_remove(&self, handle: u64) {
        let mut hub = self.lock();
        if !self.session_allows(&hub, handle, false) {
            return self.hand_back(hub, handle);
        }
        let Some(composer) = Self::editable(&mut hub, handle) else {
            return self.hand_back(hub, handle);
        };
        composer.text_epoch += 1;
        let Some((_, removed)) = composer.removed.as_ref() else {
            drop(hub);
            return self.changed();
        };
        // The file returns only within the message's limits (`addAttachment`);
        // a refused Undo stays available.
        let total = removed.bytes.len()
            + composer
                .draft
                .attachments
                .iter()
                .map(|a| a.bytes.len())
                .sum::<usize>();
        let refusal = if composer.draft.attachments.len() >= attachments::MAX_ATTACHMENTS {
            Some("Attach up to 8 files per message.")
        } else if total > attachments::MAX_TOTAL_BYTES {
            Some("Attachments: 10 MiB per file, 20 MiB per message.")
        } else {
            None
        };
        if let Some(refusal) = refusal {
            composer.show_error(refusal);
            drop(hub);
            self.changed();
            return;
        }
        let Some((index, attachment)) = composer.removed.take() else {
            return;
        };
        if !attachment.reference.is_empty() {
            let text = &mut composer.draft.text;
            if text.chars().next_back().is_some_and(|c| !c.is_whitespace()) {
                text.push(' ');
            }
            text.push_str(&attachment.reference);
            text.push(' ');
        }
        let at = index.min(composer.draft.attachments.len());
        composer.draft.attachments.insert(at, attachment);
        composer.attachments_changed();
        self.save(composer);
        drop(hub);
        self.changed();
    }

    /// Allow retry (`MessageComposer.cpp:412-417`): clears an uncertain mark.
    pub fn allow_retry(&self, handle: u64) {
        let mut hub = self.lock();
        let Some(composer) = Self::editable(&mut hub, handle) else {
            return;
        };
        composer.uncertain = false;
        composer.error = false;
        composer.notice.clear();
        self.save(composer);
        drop(hub);
        self.changed();
    }

    /// The open composer's view (tests).
    #[cfg(test)]
    pub(crate) fn current(&self) -> Option<ComposerView> {
        let hub = self.lock();
        let key = hub.open_key.clone()?;
        self.view_of(&hub, &key)
    }

    /// The open composer's view when it changed since the last take.
    pub fn take_view(&self) -> Option<ComposerView> {
        let mut hub = self.lock();
        let changes = self.0.changes.load(Ordering::SeqCst);
        if hub.view_taken.replace(changes) == Some(changes) {
            return None;
        }
        let key = hub.open_key.clone()?;
        let view = self.view_of(&hub, &key)?;
        if hub.last_view.as_ref() == Some(&view) {
            return None;
        }
        hub.last_view = Some(view.clone());
        Some(view)
    }

    /// The keys of nonempty drafts, when they changed since the last take:
    /// the 200 newest stored ones and the composers held (at most 264).
    pub fn take_draft_keys(&self) -> Option<Vec<String>> {
        let mut hub = self.lock();
        let changes = self.0.changes.load(Ordering::SeqCst);
        if hub.keys_taken.replace(changes) == Some(changes) {
            return None;
        }
        let mut keys: BTreeSet<String> = hub.stored_keys.iter().cloned().collect();
        for (key, composer) in &hub.composers {
            if composer.loaded && composer.has_content() && !composer.sending {
                keys.insert(key.clone());
            } else if composer.loaded {
                keys.remove(key);
            }
        }
        let keys: Vec<String> = keys.into_iter().collect();
        if hub.sent_keys.as_ref() == Some(&keys) {
            return None;
        }
        hub.sent_keys = Some(keys.clone());
        Some(keys)
    }

    /// Records the followed session's Activity state from a batch; a batch
    /// that changes the open session's row or its computer's status changes
    /// its block reason too. `next_batch` calls it before it takes the view.
    pub fn observe(&self, batch: &Batch) {
        if batch.is_empty() {
            return;
        }
        let (computer, key) = {
            let hub = self.lock();
            (hub.followed.computer.clone(), hub.followed.key.clone())
        };
        // The followed session's last state (or end) in this batch, and its
        // events in order: "replace" starts the window anew.
        let mut last = None;
        let mut replaced = false;
        let mut events: Vec<&Value> = Vec::new();
        for out in batch
            .activity
            .iter()
            .filter(|out| out.computer.0 == computer && out.key == key)
        {
            match out.kind.as_str() {
                "state" => last = Some(Some(&out.payload["value"])),
                "ended" => last = Some(None),
                kind @ ("events" | "replace") => {
                    if kind == "replace" {
                        replaced = true;
                        events.clear();
                    }
                    events.extend(out.payload["events"].as_array().into_iter().flatten());
                }
                _ => {}
            }
        }
        // Only the last 500 are kept, so only those are copied.
        let events: Vec<Value> = events[events.len().saturating_sub(EVENTS)..]
            .iter()
            .map(|&event| event.clone())
            .collect();
        let new_events = replaced || !events.is_empty();
        // Formatted before the lock is taken: local time may read files.
        let update =
            last.map(|state| state.map(|value| (Arc::new(value.clone()), send::expiry(value))));
        let mut hub = self.lock();
        // A session that left the catalog drops its local rows; a replaced
        // key (the same session under its run's key) keeps them.
        for out in batch
            .removes
            .iter()
            .filter(|out| out.reason.as_deref() != Some("replaced"))
        {
            let entry = (out.computer.0.clone(), out.key.clone());
            if let Some(gone) = hub.opened.remove(&entry) {
                hub.opened_order.retain(|known| known != &entry);
                hub.local.forget(&gone);
            }
        }
        let followed = hub.followed.computer == computer && hub.followed.key == key;
        let child = hub.open.as_ref().and_then(|open| open.subagent.clone());
        let allowed = |details: &Value| {
            child.as_ref().is_none_or(|child| {
                details["agent_id"].as_str() == Some(child.agent_id.as_str())
                    && details["parent_conversation_id"].as_str()
                        == Some(child.parent_conversation_id.as_str())
            })
        };
        if followed && child.is_none() && new_events {
            let kept = &mut hub.followed.events;
            if replaced {
                kept.clear();
            }
            kept.extend(events);
            while kept.len() > EVENTS {
                kept.pop_front();
            }
        }
        let accepted_details =
            followed && matches!(update, Some(Some((ref details, _))) if allowed(details));
        let state_changed = match update.filter(|_| followed) {
            Some(Some((details, expiry))) if allowed(&details) => {
                hub.followed.details = Some(details);
                hub.followed.expiry = expiry;
                hub.followed.inspect_error = None;
                hub.followed.streamed = true;
                true
            }
            Some(Some(_)) => false,
            // The follow ended: the details stay and an inspect reads them
            // anew at once (Qt's `activityEnded`, `SessionsWindow.cpp:1880-1883`).
            // An inspect still under way is dropped by its number, so it
            // cannot bring back details older than the last state.
            Some(None) => {
                hub.followed.streamed = false;
                hub.followed.inspected = None;
                hub.followed.inspecting = None;
                true
            }
            None => false,
        };
        let touched = hub.open.as_ref().is_some_and(|open| touches(batch, open));
        drop(hub);
        if state_changed {
            self.after_observe(accepted_details);
        } else if new_events {
            // New events alone change the view only when they reconcile a row.
            if self.reconcile() || touched {
                self.changed();
            }
        } else if touched {
            self.changed();
        }
    }

    /// What a change of the followed session's Activity sets off: the
    /// local rows it shows go (`reconcileMessages`), a continuation
    /// waiting for the open session reads the new details (Qt's
    /// `continueAfterCompact` and `continueAfterStart`, 3607), and the
    /// question cards follow (`renderQuestion`).
    fn after_observe(&self, accepted_details: bool) {
        self.reconcile();
        self.continue_open();
        self.sync_questions();
        // Qt's pushInspected runs only for an accepted details read (3627,
        // 3653), never for an ended frame or an inspection failure.
        if accepted_details {
            self.queue_observed();
        }
        self.changed();
    }

    /// A reloaded webview gets the view and the draft keys again. Its
    /// cold-cache question for an answer is gone with it, so nothing waits
    /// for that answer any more.
    pub fn resend(&self) {
        let mut hub = self.lock();
        hub.last_view = None;
        hub.sent_keys = None;
        hub.view_taken = None;
        hub.keys_taken = None;
        hub.answers.cold = None;
        drop(hub);
        self.changed();
    }

    /// Queues an event; the caller wakes the pump (`changed`) after it
    /// drops the hub.
    pub(crate) fn event(&self, hub: &mut Hub, event: ComposerEvent) {
        hub.events.push_back(event);
        while hub.events.len() > COMPOSER_EVENTS {
            hub.events.pop_front();
        }
    }

    /// The events since the last batch.
    pub fn take_events(&self) -> Vec<ComposerEvent> {
        self.lock().events.drain(..).collect()
    }

    /// The app quits (`exit` in lib.rs, before the store's own flush): no
    /// edit, attach or send starts any more, and attach reads under way end
    /// first (at most 2 s), so everything the hub accepted is queued in the
    /// store when the store flushes. A send whose "sending" save is still
    /// queued never starts; it reloads as uncertain.
    pub fn flush(&self) {
        self.lock().closed = true;
        self.0.opened.notify_all();
        let deadline = Instant::now() + EXIT_WAIT;
        while self.lock().composers.values().any(|c| c.attaching) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The status line and Send, by Qt's `updateControls` priority
    /// (`MessageComposer.cpp:1185, 1200-1204`), with the session's block
    /// reason, attachment rule and cache chip.
    pub(crate) fn view_of(&self, hub: &Hub, key: &str) -> Option<ComposerView> {
        let composer = hub.composers.get(key)?;
        let session = send::session_view(self, hub, composer);
        let (question, question_input) = self.question_part(hub);
        let input_visible = question_input
            && session
                .subagent
                .as_ref()
                .is_none_or(|child| child.send_supported);
        let attachments_blocked =
            !composer.draft.attachments.is_empty() && !session.attachments_available;
        let too_long = composer.draft.text.len() > MAX_TEXT_BYTES;
        let mut status = if composer.recording_owner.is_some() {
            "Recording voice. Space / Esc to stop.".into()
        } else if let Some(error) = &composer.storage_error {
            error.clone()
        } else if let Some(waiting) = &composer.waiting {
            waiting.clone()
        } else if composer.error || composer.uncertain {
            composer.notice.clone()
        } else {
            String::new()
        };
        if status.is_empty() && attachments_blocked {
            status = session.attachments_reason.clone();
        }
        if status.is_empty() {
            status = if too_long {
                "Message exceeds 64 KiB.".to_owned()
            } else if !session.session_status.is_empty() {
                session.session_status.clone()
            } else {
                session.unavailable_reason.clone()
            };
        }
        // Qt's `ownError` (`MessageComposer.cpp:1202`): an uncertain mark
        // alone is a caution, not an error. The session status is one when
        // it is shown and Qt marks it so (1225).
        let status_error = (composer.waiting.is_none() && composer.error)
            || too_long
            || composer.storage_error.is_some()
            || (status == session.session_status && session.session_status_error);
        // Qt's `locked` includes the session status's lock (1149).
        let locked = composer.busy() || !composer.loaded || session.session_blocks_input;
        Some(ComposerView {
            handle: composer.handle,
            key: composer.key.clone(),
            subagent: session.subagent.clone(),
            loaded: composer.loaded && !composer.renaming,
            rev: composer.rev,
            text_epoch: composer.text_epoch,
            text: if composer.sending {
                String::new()
            } else {
                composer.draft.text.clone()
            },
            position: composer.position,
            anchor: composer.anchor,
            attachments: if composer.sending {
                Vec::new()
            } else {
                composer
                    .draft
                    .attachments
                    .iter()
                    .enumerate()
                    .map(|(index, a)| AttachmentView {
                        name: a.name.clone(),
                        mime: a.mime.clone(),
                        reference: a.reference.clone(),
                        bytes: a.bytes.len() as u64,
                        path: format!(
                            "draft/{}/{}/{index}",
                            composer.handle, composer.attachments_rev
                        ),
                        excerpt: excerpt(a),
                        image_size: attachments::image::dimensions(&a.bytes),
                    })
                    .collect()
            },
            removed: composer.removed.as_ref().map(|(_, a)| a.name.clone()),
            attaching: composer.attaching,
            sending: composer.sending,
            recording: composer.recording_owner.is_some(),
            waiting: composer.waiting.clone(),
            uncertain: composer.uncertain,
            status,
            status_error,
            can_send: composer.loaded
                && session.available
                && composer.has_content()
                && !attachments_blocked
                && !too_long
                && !locked
                && !composer.uncertain,
            send_label: if composer.sending {
                zerus_client_core::text!("Sending…")
            } else if composer.waiting.is_some() {
                zerus_client_core::text!("Waiting to send…")
            } else {
                zerus_client_core::text!("Send")
            },
            available: session.available,
            unavailable_reason: session.unavailable_reason,
            block_reason: session.block_reason,
            attachments_available: session.attachments_available,
            attachments_reason: session.attachments_reason,
            session_status: session.session_status,
            session_status_error: session.session_status_error,
            session_blocks_input: session.session_blocks_input,
            inspect_error: session.inspect_error,
            cache: session.cache,
            cache_warning: session.cache_warning,
            cache_details: session.cache_details,
            context: session.context,
            local: hub.local.rows(&composer.key).to_vec(),
            cancel_send: hub.compacts.contains_key(key)
                || hub.start.as_ref().is_some_and(|(p, _)| p.key == key),
            compacting: hub.compacts.contains_key(key),
            clear: session.clear,
            question,
            queue: self.queue_part(hub),
            input_visible,
        })
    }
}

use tauri::ipc::{InvokeBody, Request};
use tauri::State;

#[tauri::command]
pub fn composer_open(
    state: State<'_, Composers>,
    session: Option<SessionRef>,
) -> Option<ComposerView> {
    state.open(session)
}

#[tauri::command(rename_all = "snake_case")]
pub fn composer_edit(
    state: State<'_, Composers>,
    handle: u64,
    rev: u64,
    epoch: u64,
    text: String,
    position: i64,
    anchor: i64,
) {
    state.edit(handle, rev, epoch, text, position, anchor);
}

/// Attach files: the native picker (`attachFiles`), then the reads. As in
/// Qt, the picker is modal to the main window, opens only for the open
/// composer that takes files, and its choice is dropped when another
/// session was opened meanwhile.
#[tauri::command(rename_all = "snake_case")]
pub async fn composer_pick(
    app: tauri::AppHandle,
    state: State<'_, Composers>,
    handle: u64,
    start: usize,
    end: usize,
) -> Result<(), String> {
    use tauri_plugin_dialog::DialogExt;
    let composers = state.inner().clone();
    if !composers.can_attach(handle) {
        return Ok(());
    }
    let window = crate::main_window(&app);
    let picked = tauri::async_runtime::spawn_blocking(move || {
        // The window handle is asked of the main thread: never block it here.
        let mut dialog = app.dialog().file().set_title("Attach files");
        if let Some(window) = &window {
            dialog = dialog.set_parent(window);
        }
        dialog.blocking_pick_files()
    })
    .await
    .map_err(|error| error.to_string())?;
    let paths: Vec<PathBuf> = picked
        .unwrap_or_default()
        .into_iter()
        .filter_map(|file| file.into_path().ok())
        .collect();
    if !paths.is_empty() && composers.can_attach(handle) {
        composers.attach_paths(handle, paths, Selection { start, end });
    }
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn composer_attach(
    state: State<'_, Composers>,
    handle: u64,
    paths: Vec<PathBuf>,
    start: usize,
    end: usize,
) {
    state.attach_paths(handle, paths, Selection { start, end });
}

/// A pasted image as a raw PNG body: `x-zerus-composer` is the handle,
/// `x-zerus-start` and `x-zerus-end` the selection in UTF-16 units.
#[tauri::command]
pub async fn composer_paste_image(
    state: State<'_, Composers>,
    request: Request<'_>,
) -> Result<(), String> {
    let number = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
    };
    let handle = number("x-zerus-composer").ok_or("missing x-zerus-composer")?;
    let start = number("x-zerus-start").unwrap_or(0) as usize;
    let end = number("x-zerus-end").unwrap_or(start as u64) as usize;
    let InvokeBody::Raw(png) = request.body() else {
        return Err("a pasted image must be a raw body".into());
    };
    state.paste_image(handle, pasted_bytes(png), Selection { start, end });
    Ok(())
}

/// The part of a pasted body the core needs: the IPC lends the body, so it
/// is copied, but never beyond 10 MiB and one byte. The core reads the size
/// from the PNG header and refuses a larger file with Qt's text, so the
/// outcome is the same as with the whole body.
fn pasted_bytes(body: &[u8]) -> Vec<u8> {
    body[..body.len().min(attachments::MAX_ATTACHMENT_BYTES + 1)].to_vec()
}

#[tauri::command]
pub fn composer_remove(state: State<'_, Composers>, handle: u64, index: usize) {
    state.remove(handle, index);
}

#[tauri::command]
pub fn composer_undo_remove(state: State<'_, Composers>, handle: u64) {
    state.undo_remove(handle);
}

#[tauri::command]
pub fn composer_allow_retry(state: State<'_, Composers>, handle: u64) {
    state.allow_retry(handle);
}

/// Refresh in the composer's status line (Qt's `sessionRefreshRequested`).
#[tauri::command]
pub fn composer_refresh(state: State<'_, Composers>, handle: u64) {
    state.refresh(handle);
}

/// Clear session for an unselected menu row, using its own inspection.
#[tauri::command]
pub async fn composer_clear_control(
    state: State<'_, Composers>,
    session: SessionRef,
) -> Result<Option<send::ClearControl>, String> {
    let composers = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || composers.clear_control_for(session))
        .await
        .map_err(|error| error.to_string())
}

/// The detail tab that shows. Terminal and Native UI retry a failed read at
/// Settings > Processes' background interval, Processes at its active one,
/// as Qt's poll timer does (`SessionsWindow.cpp:2013-2016`).
#[tauri::command]
pub fn composer_tab(
    state: State<'_, Composers>,
    settings: State<'_, crate::settings::Store>,
    tab: String,
) {
    state.set_poll(send::poll_seconds(&tab, |name| settings.0.number(name)));
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use zerus_client_core::drafts::{read_draft, StoredDraft};

    pub(crate) const KEY: &str = "studio\nclaude/sample-project/review";
    /// The status line of a composer whose session the catalog lacks.
    const NO_SESSION: &str = "Select a session.";

    /// A test's own folder, `zerus-composer-{name}-{pid}` in the temp
    /// folder, removed when the test ends. Declared first in a test, it ends
    /// last: the draft stores opened in it write what is still queued, then
    /// the folder goes.
    pub(crate) struct Scratch {
        path: PathBuf,
        stores: std::cell::RefCell<Vec<Arc<DraftStore>>>,
    }

    impl std::ops::Deref for Scratch {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            for store in self.stores.get_mut().drain(..) {
                store.flush();
            }
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    pub(crate) fn dir(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("zerus-composer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch {
            path: dir,
            stores: Default::default(),
        }
    }

    /// Holds the store's writer until the returned sender sends (at most
    /// five seconds): whatever is queued after this waits.
    pub(crate) fn hold(store: &DraftStore) -> std::sync::mpsc::Sender<()> {
        let (release, gate) = std::sync::mpsc::channel::<()>();
        store.index(
            IndexPage {
                limit: 1,
                before: None,
            },
            move |_| {
                let _ = gate.recv_timeout(Duration::from_secs(5));
            },
        );
        release
    }

    /// The test's draft store under `root/drafts`. The folder keeps it to
    /// the end of the test, as the app's `Drafts` keeps the app's: a store
    /// must never drop on its own writer thread.
    pub(crate) fn drafts(root: &Scratch) -> Arc<DraftStore> {
        let store = Arc::new(DraftStore::open(root.join("drafts")));
        root.stores.borrow_mut().push(Arc::clone(&store));
        store
    }

    pub(crate) fn hub_on(store: &Arc<DraftStore>) -> (Composers, Arc<AtomicUsize>) {
        hub_with_qt(store, None)
    }

    /// A hub whose Qt draft store is `qt` (read-only).
    pub(crate) fn hub_with_qt(
        store: &Arc<DraftStore>,
        qt: Option<PathBuf>,
    ) -> (Composers, Arc<AtomicUsize>) {
        hub_with_stores(store, qt, Vec::new())
    }

    /// A hub with Qt's and the dev app's draft stores (read-only).
    pub(crate) fn hub_with_stores(
        store: &Arc<DraftStore>,
        qt: Option<PathBuf>,
        dev: Vec<PathBuf>,
    ) -> (Composers, Arc<AtomicUsize>) {
        let notified = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&notified);
        let composers = Composers::new(
            Arc::clone(store),
            // The `cache` folder beside the test's `drafts`.
            Config {
                cache: store.root().with_file_name("cache"),
                qt_drafts: qt,
                dev_drafts: dev,
            },
            Deps {
                spawn: Arc::new(|job: Job| {
                    std::thread::spawn(job);
                }),
                notify: Arc::new(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                }),
                // No session: these views read "Select a session.".
                clock: Arc::new(|| 1_793_000_100.0),
                rows: Arc::new(|_, _, _| None),
                facts: Arc::new(|_, _| None),
                reconnect: Arc::new(|_| {}),
                inspect: Arc::new(|_, _, _| Err("timed out".into())),
                dispatch: Arc::new(|_, _, _| Err("no call in these tests".into())),
            },
        );
        (composers, notified)
    }

    pub(crate) fn hub(root: &Scratch) -> (Composers, Arc<AtomicUsize>) {
        hub_on(&drafts(root))
    }

    pub(crate) fn session() -> SessionRef {
        SessionRef {
            computer: "studio".into(),
            key: "run:run-1".into(),
            name: "claude/sample-project/review".into(),
            archive_id: None,
            subagent: None,
        }
    }

    /// The open composer's view once `done` holds, within five seconds.
    pub(crate) fn until(c: &Composers, done: impl Fn(&ComposerView) -> bool) -> ComposerView {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let view = c.current().expect("a composer is open");
            if done(&view) {
                return view;
            }
            assert!(Instant::now() < deadline, "timed out: {view:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn draft_keys_are_qts() {
        assert_eq!(draft_key("", "claude/p/a", None), "\nclaude/p/a");
        assert_eq!(
            draft_key("studio", "claude/p/a", Some("a-1")),
            "studio\nclaude/p/a\na-1"
        );
    }

    #[test]
    fn a_draft_survives_reopening() {
        let root = dir("reopen");
        let store = drafts(&root);
        let (c, _) = hub_on(&store);
        let view = c.open(Some(session())).unwrap();
        let view = until(&c, |v| v.loaded && v.handle == view.handle);
        c.edit(view.handle, 1, view.text_epoch, "hello".into(), 5, 5);
        c.flush();
        store.flush();
        drop(c);
        // A new hub reads the draft back from the disk.
        let (again, _) = hub_on(&store);
        again.open(Some(session()));
        let view = until(&again, |v| v.loaded);
        assert_eq!((view.text.as_str(), view.position), ("hello", 5));
    }

    #[test]
    fn quit_flushes_drafts() {
        let root = dir("flush");
        let store = drafts(&root);
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.edit(
            view.handle,
            1,
            view.text_epoch,
            "typed before quitting".into(),
            0,
            0,
        );
        // The exit hook's order: the hub closes, then the store writes everything queued.
        c.flush();
        store.flush();
        assert_eq!(
            read_draft(&root.join("drafts"), KEY).draft.draft.text,
            "typed before quitting"
        );
        c.edit(
            view.handle,
            2,
            view.text_epoch,
            "after the exit".into(),
            0,
            0,
        );
        assert_eq!(
            c.current().unwrap().text,
            "typed before quitting",
            "a closed hub takes no edits"
        );
    }

    #[test]
    fn a_sending_draft_reloads_uncertain() {
        let root = dir("sending");
        let store = DraftStore::open(root.join("drafts"));
        let mut stored = StoredDraft {
            sending: true,
            ..StoredDraft::default()
        };
        stored.draft.text = "maybe delivered".into();
        store.save_durable(KEY, stored, |_| {});
        store.flush();
        drop(store);
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        assert!(view.uncertain);
        assert_eq!(
            view.status,
            "Delivery was interrupted. Check Activity or Terminal before sending again."
        );
        assert!(view.status_error && !view.can_send);
        c.allow_retry(view.handle);
        let view = until(&c, |v| !v.uncertain);
        // Send's other conditions (the session's state) arrive with Task 4a.
        assert!(!view.status_error);
    }

    #[test]
    fn stale_edits_are_ignored() {
        let root = dir("stale");
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.edit(view.handle, 2, view.text_epoch, "newer".into(), 0, 0);
        c.edit(view.handle, 1, view.text_epoch, "older".into(), 0, 0);
        assert_eq!(c.current().unwrap().text, "newer");
        c.edit(
            view.handle,
            2,
            view.text_epoch,
            "the same revision".into(),
            0,
            0,
        );
        assert_eq!(c.current().unwrap().text, "newer");
    }

    #[test]
    fn over_64_kib_disables_send() {
        let root = dir("long");
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.edit(
            view.handle,
            1,
            view.text_epoch,
            "x".repeat(64 * 1024 + 1),
            0,
            0,
        );
        let view = c.current().unwrap();
        assert_eq!(view.status, "Message exceeds 64 KiB.");
        assert!(view.status_error && !view.can_send);
    }

    #[test]
    fn attaching_inserts_the_reference_and_saves_it() {
        let root = dir("attach");
        let file = root.join("notes.txt");
        std::fs::write(&file, "first line\nsecond line\n").unwrap();
        let store = drafts(&root);
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.edit(view.handle, 1, view.text_epoch, "see".into(), 3, 3);
        c.attach_paths(view.handle, vec![file], Selection { start: 3, end: 3 });
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        assert_eq!(view.text, "see [File #1] ");
        assert!(view.text_epoch > 0);
        let attachment = &view.attachments[0];
        assert_eq!(
            (attachment.name.as_str(), attachment.mime.as_str()),
            ("notes.txt", "text/plain")
        );
        assert_eq!(
            attachment.excerpt.as_deref(),
            Some("first line\nsecond line")
        );
        assert!(attachment
            .path
            .starts_with(&format!("draft/{}/", view.handle)));
        store.flush();
        assert_eq!(
            read_draft(&root.join("drafts"), KEY)
                .draft
                .draft
                .attachments
                .len(),
            1
        );
    }

    #[test]
    fn a_huge_pasted_image_is_refused() {
        let root = dir("paste");
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        // A PNG header of 8000 x 5000 pixels: 40 megapixels.
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(8000u32.to_be_bytes());
        png.extend(5000u32.to_be_bytes());
        png.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        c.paste_image(view.handle, png, Selection::default());
        let view = until(&c, |v| !v.attaching);
        assert!(view.attachments.is_empty());
        assert_eq!(
            view.status,
            "Image is too large. Use an image below 32 megapixels."
        );
    }

    #[test]
    fn remove_drops_the_reference_and_undo_restores_the_file() {
        let root = dir("remove");
        let file = root.join("a.txt");
        std::fs::write(&file, "a").unwrap();
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.attach_paths(view.handle, vec![file], Selection::default());
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        let first_path = view.attachments[0].path.clone();
        c.remove(view.handle, 0);
        let view = c.current().unwrap();
        assert!(view.attachments.is_empty() && !view.text.contains("[File #1]"));
        assert_eq!(view.removed.as_deref(), Some("a.txt"));
        c.undo_remove(view.handle);
        let view = c.current().unwrap();
        assert_eq!(view.attachments.len(), 1);
        assert!(view.text.contains("[File #1]") && view.removed.is_none());
        assert_ne!(
            view.attachments[0].path, first_path,
            "a new list gets new paths"
        );
    }

    #[test]
    fn a_rename_moves_the_draft() {
        let root = dir("rename");
        let store = drafts(&root);
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.edit(
            view.handle,
            1,
            view.text_epoch,
            "kept across a rename".into(),
            0,
            0,
        );
        let mut renamed = session();
        renamed.name = "claude/sample-project/review-2".into();
        c.open(Some(renamed));
        let view = until(&c, |v| {
            v.key == "studio\nclaude/sample-project/review-2" && v.loaded
        });
        assert_eq!(view.text, "kept across a rename");
        assert!(
            !c.lock().order.contains(&KEY.to_owned()),
            "the old key leaves the eviction order"
        );
        store.flush();
        assert_eq!(read_draft(&root.join("drafts"), KEY).draft.draft.text, "");
    }

    #[test]
    fn a_rename_onto_an_existing_draft_keeps_both() {
        let root = dir("rename-collision");
        let store = drafts(&root);
        let mut there = StoredDraft::default();
        there.draft.text = "already here".into();
        store.save_durable("studio\nclaude/sample-project/review-2", there, |_| {});
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.edit(view.handle, 1, view.text_epoch, "moving".into(), 0, 0);
        let mut renamed = session();
        renamed.name = "claude/sample-project/review-2".into();
        c.open(Some(renamed));
        let view = until(&c, |v| v.loaded && v.text == "already here");
        // Qt's placement: the new key reloads with no message, and the moved
        // draft is marked under its old key, past the hub's own save of it.
        // (No session here: the status line reads "Select a session.".)
        assert!(view.status == NO_SESSION && !view.status_error, "{view:?}");
        store.flush();
        assert_eq!(
            c.lock().composers[KEY].storage_error.as_deref(),
            Some(zerus_client_core::drafts::ALREADY_EXISTS)
        );
        assert_eq!(
            read_draft(&root.join("drafts"), KEY).draft.draft.text,
            "moving",
            "the moved draft stays for Saved drafts"
        );
        assert_eq!(
            read_draft(
                &root.join("drafts"),
                "studio\nclaude/sample-project/review-2"
            )
            .draft
            .draft
            .text,
            "already here"
        );
    }

    #[test]
    fn draft_keys_follow_nonempty_drafts() {
        let root = dir("keys");
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        let _ = c.take_draft_keys();
        c.edit(view.handle, 1, view.text_epoch, "x".into(), 1, 1);
        assert_eq!(c.take_draft_keys(), Some(vec![KEY.to_owned()]));
        assert_eq!(
            c.take_draft_keys(),
            None,
            "unchanged keys are not sent again"
        );
        c.edit(view.handle, 2, view.text_epoch, String::new(), 0, 0);
        assert_eq!(c.take_draft_keys(), Some(vec![]));
    }

    #[test]
    fn a_view_is_taken_once_per_change() {
        let root = dir("take");
        let (c, notified) = hub(&root);
        c.open(Some(session()));
        until(&c, |v| v.loaded);
        assert!(c.take_view().is_some());
        assert!(c.take_view().is_none());
        let before = notified.load(Ordering::SeqCst);
        let view = c.current().unwrap();
        c.edit(view.handle, 1, view.text_epoch, "y".into(), 1, 1);
        assert!(
            notified.load(Ordering::SeqCst) > before,
            "a change wakes the pump"
        );
        assert_eq!(c.take_view().unwrap().text, "y");
        c.resend();
        assert!(
            c.take_view().is_some(),
            "a reloaded webview gets the view again"
        );
    }

    #[test]
    fn the_hub_writes_through_the_apps_one_store() {
        let root = dir("one-store");
        let store = drafts(&root);
        let (c, _) = hub_on(&store);
        assert!(
            Arc::ptr_eq(&c.0.store, &store),
            "the hub opens no store of its own"
        );
        // Voice's save of the same key, held up on the shared writer until
        // the hub has asked for its load: one writer orders the two.
        let (release, gate) = std::sync::mpsc::channel::<()>();
        store.index(
            IndexPage {
                limit: 1,
                before: None,
            },
            move |_| {
                let _ = gate.recv_timeout(Duration::from_secs(5));
            },
        );
        let mut dictated = StoredDraft::default();
        dictated.draft.text = "dictated".into();
        store.save_durable(KEY, dictated, |_| {});
        c.open(Some(session()));
        release.send(()).unwrap();
        let view = until(&c, |v| v.loaded);
        assert_eq!(view.text, "dictated");
    }

    /// Every file under `root` but the app's store (`root/drafts`), with
    /// its bytes and modification time.
    fn outside_the_store(root: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
        fn walk(dir: &Path, skip: &Path, out: &mut Vec<(PathBuf, Vec<u8>, std::time::SystemTime)>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .flatten()
                .map(|e| e.path())
                .collect();
            entries.sort();
            for path in entries {
                if path == skip {
                    continue;
                }
                if path.is_dir() {
                    walk(&path, skip, out);
                } else {
                    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
                    out.push((path.clone(), std::fs::read(&path).unwrap(), modified));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, &root.join("drafts"), &mut out);
        out
    }

    #[test]
    fn qts_drafts_are_neither_read_nor_written() {
        let root = dir("qt-untouched");
        // Qt's store in its own format, with a draft under the same key.
        let qt = root.join("hgs-tray.conf.drafts");
        {
            let theirs = DraftStore::open(qt.clone());
            let mut stored = StoredDraft::default();
            stored.draft.text = "typed in the Qt app".into();
            stored
                .draft
                .add("qt.txt", "text/plain", b"qt".to_vec(), Selection::default())
                .unwrap();
            theirs.save_durable(KEY, stored, |_| {});
        }
        let file = root.join("notes.txt");
        std::fs::write(&file, "mine").unwrap();
        let before = outside_the_store(&root);
        assert!(before.iter().any(|(path, ..)| path.starts_with(&qt)));

        let store = drafts(&root);
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        assert_eq!(
            view.text, "",
            "a Qt draft comes over only through Saved drafts' recovery"
        );
        c.edit(view.handle, 1, view.text_epoch, "mine".into(), 4, 4);
        c.attach_paths(view.handle, vec![file], Selection { start: 4, end: 4 });
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        c.remove(view.handle, 0);
        c.undo_remove(view.handle);
        let mut renamed = session();
        renamed.name = "claude/sample-project/review-2".into();
        c.open(Some(renamed));
        until(&c, |v| v.loaded && v.key.ends_with("review-2"));
        c.flush();
        store.flush();

        assert!(
            outside_the_store(&root) == before,
            "only the app's store changed; Qt's folder is byte-for-byte the same"
        );
        let moved = read_draft(
            &root.join("drafts"),
            "studio\nclaude/sample-project/review-2",
        );
        assert!(
            moved.draft.draft.text.starts_with("mine") && moved.draft.draft.attachments.len() == 1
        );
    }

    #[test]
    fn draft_marks_are_bounded() {
        let root = dir("marks");
        let store = drafts(&root);
        for n in 0..205 {
            let mut stored = StoredDraft::default();
            stored.draft.text = format!("stored {n}");
            store.save_durable(&format!("studio\nclaude/p/stored-{n}"), stored, |_| {});
        }
        store.flush();
        let (c, _) = hub_on(&store);
        for n in 0..70 {
            let mut session = session();
            session.key = format!("run:held-{n}");
            session.name = format!("claude/p/held-{n}");
            c.open(Some(session));
            let view = until(&c, |v| v.loaded && v.key.ends_with(&format!("/held-{n}")));
            c.edit(view.handle, 1, view.text_epoch, "x".into(), 1, 1);
        }
        store.flush();
        assert_eq!(c.lock().composers.len(), COMPOSERS);
        let keys = c.take_draft_keys().unwrap();
        // The 200 newest stored drafts (six of them dropped composers) and
        // the 64 composers held.
        assert_eq!(keys.len(), DRAFT_KEYS + COMPOSERS);
        assert!(keys.contains(&"studio\nclaude/p/held-0".to_owned()));
    }

    #[test]
    fn idle_composers_hold_at_most_128_mib() {
        let root = dir("held");
        let (c, _) = hub(&root);
        let bytes: Arc<[u8]> = vec![1u8; 20 * 1024 * 1024].into();
        let mut hub = c.lock();
        for n in 0..10 {
            let key = format!("studio\nclaude/p/held-{n}");
            let mut composer = Composer::new(100 + n, key.clone());
            composer.loaded = true;
            composer.draft.attachments.push(Attachment {
                name: "a.bin".into(),
                mime: "application/octet-stream".into(),
                bytes: Arc::clone(&bytes),
                reference: String::new(),
            });
            // The oldest one's last save failed: only the memory holds it.
            composer.save_failed = n == 0;
            hub.composers.insert(key.clone(), composer);
            hub.order.push_back(key);
        }
        c.trim(&mut hub);
        let held: usize = hub.composers.values().map(Composer::held_bytes).sum();
        assert!(held <= HELD_BYTES, "{held}");
        assert_eq!(hub.composers.len(), 6);
        assert!(hub.composers.contains_key("studio\nclaude/p/held-0"));
        assert!(hub.composers.contains_key("studio\nclaude/p/held-9"));
    }

    #[test]
    fn a_failed_save_is_retried_until_it_succeeds() {
        let root = dir("retry");
        let blocker = root.join("blocked");
        std::fs::write(&blocker, "a file where the store's folder belongs").unwrap();
        let store = Arc::new(DraftStore::open(blocker.join("drafts")));
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.edit(
            view.handle,
            1,
            view.text_epoch,
            "kept in memory".into(),
            0,
            0,
        );
        let view = until(&c, |v| v.status_error);
        assert!(
            view.status.starts_with("Draft not saved"),
            "{}",
            view.status
        );
        std::fs::remove_file(&blocker).unwrap();
        until(&c, |v| !v.status_error);
        store.flush();
        assert_eq!(
            read_draft(&blocker.join("drafts"), KEY).draft.draft.text,
            "kept in memory"
        );
    }

    #[test]
    fn a_file_that_fails_does_not_stop_the_next_one() {
        let root = dir("attach-batch");
        let good = root.join("good.txt");
        std::fs::write(&good, "good").unwrap();
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.attach_paths(
            view.handle,
            vec![root.join("missing.txt"), good],
            Selection::default(),
        );
        let view = until(&c, |v| !v.attaching);
        assert_eq!(view.attachments.len(), 1);
        assert_eq!(view.attachments[0].name, "good.txt");
        assert!(
            !view.status_error,
            "a later success clears the error: {view:?}"
        );
    }

    #[test]
    fn files_attach_only_to_the_open_composer() {
        let root = dir("can-attach");
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let first = until(&c, |v| v.loaded).handle;
        assert!(c.can_attach(first));
        let mut other = session();
        other.key = "run:run-2".into();
        other.name = "claude/sample-project/other".into();
        c.open(Some(other));
        let second = until(&c, |v| v.loaded).handle;
        assert!(!c.can_attach(first) && c.can_attach(second));
        c.flush();
        assert!(!c.can_attach(second), "nothing attaches after the exit");
    }

    const RENAMED: &str = "studio\nclaude/sample-project/review-2";

    fn renamed() -> SessionRef {
        let mut renamed = session();
        renamed.name = "claude/sample-project/review-2".into();
        renamed
    }

    #[test]
    fn undo_stays_within_the_message_limits() {
        let root = dir("undo-limits");
        let files: Vec<PathBuf> = ["a", "b", "c"]
            .iter()
            .map(|name| {
                let file = root.join(format!("{name}.bin"));
                let bytes = vec![name.as_bytes()[0]; attachments::MAX_ATTACHMENT_BYTES];
                std::fs::write(&file, bytes).unwrap();
                file
            })
            .collect();
        let store = drafts(&root);
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.attach_paths(view.handle, vec![files[0].clone()], Selection::default());
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        c.remove(view.handle, 0);
        c.attach_paths(view.handle, files[1..].to_vec(), Selection::default());
        let before = until(&c, |v| v.attachments.len() == 2 && !v.attaching);
        c.undo_remove(before.handle);
        let view = c.current().unwrap();
        assert_eq!(
            view.attachments.len(),
            2,
            "a third 10 MiB file passes 20 MiB"
        );
        assert!(view.text_epoch > before.text_epoch, "the text comes back");
        assert_eq!(
            view.status,
            "Attachments: 10 MiB per file, 20 MiB per message."
        );
        assert!(view.status_error);
        assert_eq!(
            view.removed.as_deref(),
            Some("a.bin"),
            "Undo stays available"
        );
        c.flush();
        store.flush();
        drop(c);
        let (again, _) = hub_on(&store);
        again.open(Some(session()));
        let view = until(&again, |v| v.loaded);
        assert_eq!(view.status, NO_SESSION, "no restore error");
        let kept: Vec<(&str, u64)> = view
            .attachments
            .iter()
            .map(|a| (a.name.as_str(), a.bytes))
            .collect();
        let size = attachments::MAX_ATTACHMENT_BYTES as u64;
        assert_eq!(kept, [("b.bin", size), ("c.bin", size)], "no copy lost");
    }

    #[test]
    fn an_edit_over_the_text_before_a_remove_is_refused() {
        let root = dir("epoch-remove");
        let file = root.join("a.txt");
        std::fs::write(&file, "a").unwrap();
        let store = drafts(&root);
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.attach_paths(view.handle, vec![file], Selection::default());
        let before = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        c.remove(before.handle, 0);
        let after = c.current().unwrap();
        assert!(after.text_epoch > before.text_epoch);
        // Typed over the text that still names the file.
        let stale = format!("{}typed", before.text);
        c.edit(before.handle, 1, before.text_epoch, stale, 0, 0);
        assert_eq!(c.current().unwrap().text, after.text);
        store.flush();
        let disk = read_draft(&root.join("drafts"), KEY).draft.draft.text;
        assert_eq!(disk, after.text);
        assert!(!disk.contains("[File #1]"));
    }

    #[test]
    fn an_edit_over_the_text_before_an_undo_is_refused() {
        let root = dir("epoch-undo");
        let file = root.join("a.txt");
        std::fs::write(&file, "a").unwrap();
        let store = drafts(&root);
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.attach_paths(view.handle, vec![file], Selection::default());
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        c.remove(view.handle, 0);
        let removed = c.current().unwrap();
        c.undo_remove(view.handle);
        let restored = c.current().unwrap();
        assert!(restored.text_epoch > removed.text_epoch);
        // Typed over the text without the mention.
        let stale = format!("{}typed", removed.text);
        c.edit(view.handle, 1, removed.text_epoch, stale, 0, 0);
        let now = c.current().unwrap();
        assert_eq!(now.text, restored.text);
        assert!(now.text.contains("[File #1]"), "the restored mention stays");
        store.flush();
        let disk = read_draft(&root.join("drafts"), KEY).draft.draft.text;
        assert!(disk.contains("[File #1]"));
    }

    #[test]
    fn an_edit_during_a_rename_is_refused_and_handed_back() {
        let root = dir("rename-edit");
        let store = drafts(&root);
        let mut there = StoredDraft::default();
        there.draft.text = "already here".into();
        store.save_durable(RENAMED, there, |_| {});
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.edit(view.handle, 1, view.text_epoch, "moving".into(), 6, 6);
        let release = hold(&store);
        c.open(Some(renamed()));
        let during = c.current().unwrap();
        assert!(!during.loaded, "a rename locks the composer");
        let typed = "typed during the rename".to_owned();
        c.edit(during.handle, 2, during.text_epoch, typed, 0, 0);
        let handed = c.current().unwrap();
        assert!(
            handed.text_epoch > during.text_epoch,
            "the text is handed back"
        );
        assert_eq!(handed.text, "moving");
        release.send(()).unwrap();
        until(&c, |v| v.loaded && v.text == "already here");
        store.flush();
        assert_eq!(
            read_draft(&root.join("drafts"), RENAMED).draft.draft.text,
            "already here",
            "the draft already at the new name stays"
        );
        assert_eq!(
            read_draft(&root.join("drafts"), KEY).draft.draft.text,
            "moving"
        );
    }

    #[test]
    fn a_rename_back_keeps_the_newer_composer() {
        let root = dir("rename-back");
        let store = drafts(&root);
        let mut there = StoredDraft::default();
        there.draft.text = "already here".into();
        store.save_durable(RENAMED, there, |_| {});
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.edit(view.handle, 1, view.text_epoch, "first".into(), 5, 5);
        let release = hold(&store);
        c.open(Some(renamed()));
        // Renamed back while the store still has the first rename queued.
        let back = c.open(Some(session())).unwrap();
        release.send(()).unwrap();
        let view = until(&c, |v| v.loaded && v.key == KEY);
        assert_eq!(view.handle, back.handle, "the newer composer stays");
        assert_eq!(view.text, "first");
        store.flush();
        assert_eq!(
            read_draft(&root.join("drafts"), KEY).draft.draft.text,
            "first"
        );
        assert_eq!(
            read_draft(&root.join("drafts"), RENAMED).draft.draft.text,
            "already here"
        );
    }

    #[test]
    fn a_refused_remove_or_undo_hands_the_text_back() {
        let root = dir("hand-back");
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.remove(view.handle, 3);
        let after_remove = c.current().unwrap();
        assert!(
            after_remove.text_epoch > view.text_epoch,
            "no file 3: the text comes back"
        );
        c.undo_remove(view.handle);
        let after_undo = c.current().unwrap();
        assert!(
            after_undo.text_epoch > after_remove.text_epoch,
            "nothing to undo: likewise"
        );
    }

    #[test]
    fn undo_stays_within_eight_files() {
        let root = dir("undo-count");
        let files: Vec<PathBuf> = (0..=attachments::MAX_ATTACHMENTS)
            .map(|n| {
                let file = root.join(format!("{n}.txt"));
                std::fs::write(&file, format!("file {n}")).unwrap();
                file
            })
            .collect();
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.attach_paths(view.handle, vec![files[0].clone()], Selection::default());
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        c.remove(view.handle, 0);
        c.attach_paths(view.handle, files[1..].to_vec(), Selection::default());
        let view = until(&c, |v| v.attachments.len() == 8 && !v.attaching);
        c.undo_remove(view.handle);
        let view = c.current().unwrap();
        assert_eq!(view.status, "Attach up to 8 files per message.");
        assert_eq!(view.attachments.len(), attachments::MAX_ATTACHMENTS);
        assert_eq!(
            view.removed.as_deref(),
            Some("0.txt"),
            "Undo stays available"
        );
    }

    /// Keeps a folder from taking new entries until it drops, a panic
    /// included, so the test's own folder can still be removed.
    #[cfg(unix)]
    struct ReadOnly(PathBuf);

    #[cfg(unix)]
    impl ReadOnly {
        fn set(folder: &Path, bits: u32) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(bits)).unwrap();
        }

        fn new(folder: PathBuf) -> ReadOnly {
            Self::set(&folder, 0o500);
            ReadOnly(folder)
        }
    }

    #[cfg(unix)]
    impl Drop for ReadOnly {
        fn drop(&mut self) {
            Self::set(&self.0, 0o700);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_rename_gives_a_newer_composer_the_unsaved_draft() {
        let root = dir("rename-back-refused");
        let store = drafts(&root);
        let mut there = StoredDraft::default();
        there.draft.text = "already here".into();
        store.save_durable(RENAMED, there, |_| {});
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        let folder = root.join("drafts");
        let release = hold(&store);
        // The edit's save fails: the drafts folder takes no new draft.
        let read_only = ReadOnly::new(folder.clone());
        c.edit(
            view.handle,
            1,
            view.text_epoch,
            "not saved yet".into(),
            13,
            13,
        );
        // A delivery of it was interrupted: that must survive the move.
        c.lock().composers.get_mut(KEY).unwrap().uncertain = true;
        c.open(Some(renamed()));
        // Renamed back while the first rename, which the store refuses, waits.
        let back = c.open(Some(session())).unwrap();
        release.send(()).unwrap();
        let view = until(&c, |v| v.loaded && v.key == KEY);
        assert_eq!(
            view.handle, back.handle,
            "the newer composer keeps its handle"
        );
        assert_eq!(view.text, "not saved yet");
        assert!(view.text_epoch > back.text_epoch);
        assert!(view.uncertain && !view.can_send, "still uncertain");
        drop(read_only);
        // The failed save is retried and reaches the disk; no load replaced it.
        let view = until(&c, |v| !v.status_error);
        assert_eq!(view.text, "not saved yet");
        store.flush();
        assert_eq!(read_draft(&folder, KEY).draft.draft.text, "not saved yet");
        assert_eq!(
            read_draft(&folder, RENAMED).draft.draft.text,
            "already here"
        );
    }

    #[test]
    fn a_rename_that_succeeds_while_renamed_back_follows_the_draft() {
        let root = dir("rename-back-done");
        let store = drafts(&root);
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.edit(view.handle, 1, view.text_epoch, "moved".into(), 5, 5);
        let release = hold(&store);
        c.open(Some(renamed()));
        let back = c.open(Some(session())).unwrap();
        assert_ne!(back.handle, view.handle);
        release.send(()).unwrap();
        // Qt (`MessageComposer.cpp:806`): the open composer follows the draft.
        let view = until(&c, |v| v.loaded && v.key == RENAMED);
        assert_eq!(view.text, "moved");
        let hub = c.lock();
        assert!(!hub.composers.contains_key(KEY) && !hub.order.contains(&KEY.to_owned()));
        drop(hub);
        store.flush();
        assert_eq!(
            read_draft(&root.join("drafts"), RENAMED).draft.draft.text,
            "moved"
        );
        assert_eq!(read_draft(&root.join("drafts"), KEY).draft.draft.text, "");
    }

    #[test]
    fn a_held_composer_at_the_old_key_keeps_its_place_in_the_order() {
        let root = dir("rename-back-held");
        let store = drafts(&root);
        let (c, _) = hub_on(&store);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.edit(view.handle, 1, view.text_epoch, "moved".into(), 5, 5);
        let release = hold(&store);
        c.open(Some(renamed()));
        c.open(Some(session()));
        // Another session opens before the first rename answers.
        let mut other = session();
        other.key = "run:run-2".into();
        other.name = "claude/sample-project/other".into();
        c.open(Some(other));
        release.send(()).unwrap();
        until(&c, |v| v.loaded);
        store.flush();
        let hub = c.lock();
        assert!(
            hub.composers.contains_key(KEY),
            "the old key's composer is held"
        );
        assert!(
            hub.order.contains(&KEY.to_owned()),
            "and can still be trimmed"
        );
    }

    #[test]
    fn the_open_composer_is_never_trimmed() {
        let root = dir("open-kept");
        let (c, _) = hub(&root);
        {
            let mut hub = c.lock();
            for n in 0..COMPOSERS {
                let key = format!("studio\nclaude/p/failed-{n}");
                let mut composer = Composer::new(1000 + n as u64, key.clone());
                composer.loaded = true;
                composer.draft.text = format!("only in memory {n}");
                composer.save_failed = true;
                hub.composers.insert(key.clone(), composer);
                hub.order.push_back(key);
            }
        }
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.edit(view.handle, 1, view.text_epoch, "applied".into(), 7, 7);
        assert_eq!(c.current().unwrap().text, "applied");
        assert_eq!(c.lock().composers.len(), COMPOSERS + 1);
    }

    #[test]
    fn an_uncertain_mark_alone_is_a_caution() {
        let root = dir("caution");
        let (c, _) = hub(&root);
        c.open(Some(session()));
        until(&c, |v| v.loaded);
        {
            let mut hub = c.lock();
            let composer = hub.composers.get_mut(KEY).unwrap();
            composer.uncertain = true;
            composer.notice = zerus_client_core::drafts::INTERRUPTED.into();
        }
        let view = c.current().unwrap();
        assert_eq!(view.status, zerus_client_core::drafts::INTERRUPTED);
        assert!(!view.status_error && !view.can_send);
    }

    #[test]
    fn a_pasted_body_is_copied_up_to_the_limit() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(100u32.to_be_bytes());
        png.extend(100u32.to_be_bytes());
        png.resize(attachments::MAX_ATTACHMENT_BYTES + 4096, 0);
        let copied = pasted_bytes(&png);
        assert_eq!(copied.len(), attachments::MAX_ATTACHMENT_BYTES + 1);
        assert_eq!(copied[..24], png[..24]);
        let root = dir("paste-bound");
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.paste_image(view.handle, copied, Selection::default());
        let view = until(&c, |v| !v.attaching);
        assert!(view.attachments.is_empty());
        assert_eq!(
            view.status,
            "Attachments: 10 MiB per file, 20 MiB per message."
        );
    }
}
