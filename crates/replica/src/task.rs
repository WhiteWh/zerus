//! One task as one Loro document: `meta` (identity, set once), `fields`
//! (last writer wins), `text` (the prompt, merged by character) and `log`
//! (history, append only).
use crate::attachment::{
    check_attachments, TaskAttachment, BASE_TASK_SCHEMA, DISCUSSION_TASK_SCHEMA,
    MESSAGE_TASK_SCHEMA, NAMED_TASK_SCHEMA, TASK_SCHEMA,
};
use crate::execution::{
    self, check_attempt, check_event, Attempt, AttemptEvent, ReportDecision, ReportKind, Stale,
    MAX_ATTEMPTS, MAX_ATTEMPT_EVENTS,
};
use crate::schema::{
    check_text, Actor, LogEntry, SessionKey, Stage, Target, TaskView, TaskWorker, MAX_DOCUMENT,
    SCHEMA,
};
use crate::{ids, rank, Error, Result};
use loro::{ContainerTrait, ExportMode, LoroDoc, LoroMap, LoroValue, UpdateOptions, VersionVector};
use sha2::{Digest, Sha256};

#[path = "task_attempts.rs"]
mod attempts;
pub use attempts::{Settled, Settlement};

#[path = "task_cutover.rs"]
mod cutover;
pub use cutover::{Capture, CutoverSeal, NotCutOver};

#[cfg(test)]
#[path = "task_team_tests.rs"]
mod team_key_tests;

#[cfg(test)]
#[path = "task_team_finish_tests.rs"]
mod team_finish_tests;

#[cfg(test)]
#[path = "task_team_handover_tests.rs"]
mod team_handover_tests;

#[cfg(test)]
#[path = "task_ui_fixture_tests.rs"]
mod ui_fixture_tests;

#[cfg(not(feature = "test-without-team"))]
#[path = "task_team.rs"]
mod team;
#[cfg(not(feature = "test-without-team"))]
pub use team::TaskTeam;

pub(crate) fn cutover_schema() -> i64 {
    cutover::SCHEMA
}

pub struct NewTask<'a> {
    pub project: &'a str,
    pub id: &'a str,
    pub text: &'a str,
    pub stage: Stage,
    pub target: Target,
    pub author: Actor,
    pub rank: &'a str,
    pub now: i64,
}

pub struct TaskDoc {
    uuid: String,
    doc: LoroDoc,
    /// The version the store last read or wrote, for appending updates.
    pub(crate) saved: Option<VersionVector>,
    /// The length of the valid updates the store read or wrote after the snapshot.
    pub(crate) saved_len: u64,
    /// The stored updates were cut short or could not be applied: rewrite the snapshot.
    pub(crate) needs_fold: bool,
    /// The stage when the store last read or wrote the task.
    pub(crate) saved_stage: Option<Stage>,
    /// In-process creation witness. Importing bytes never manufactures it.
    pub(crate) created_here: bool,
    /// A phone cache can project state, never mutate a sealed native task.
    pub(crate) projection_only: bool,
}

fn string(map: &LoroMap, key: &str) -> Option<String> {
    match map.get(key)?.into_value().ok()? {
        LoroValue::String(value) => Some(value.to_string()),
        _ => None,
    }
}

fn integer(map: &LoroMap, key: &str) -> Option<i64> {
    match map.get(key)?.into_value().ok()? {
        LoroValue::I64(value) => Some(value),
        _ => None,
    }
}

fn decode<T: serde::de::DeserializeOwned>(map: &LoroMap, key: &str) -> Result<T> {
    let text =
        string(map, key).ok_or_else(|| Error::failed(format!("task field {key} is missing")))?;
    Ok(serde_json::from_str(&text)?)
}

/// Refuses a stage a new task cannot start in.
pub(crate) fn check_new_stage(stage: Stage) -> Result<()> {
    if !matches!(stage, Stage::Backlog | Stage::Todo) {
        return Err(Error::new("a new task goes to Backlog or To do"));
    }
    Ok(())
}

pub(crate) fn check_title(title: &str) -> Result<()> {
    // One shared rule: the nine bidirectional characters are refused as
    // control characters are (zerus-63rru.75), in this one place.
    if title.trim().is_empty() || title.len() > 512 || crate::forbidden_char(title).is_some() {
        return Err(Error::new(
            "task title must be nonempty, at most 512 UTF-8 bytes and contain no control or bidirectional characters",
        ));
    }
    Ok(())
}

/// Whether an edit only drops files from the current set: no new media is
/// added, so it may still run when the retained history is already full.
fn removal_only(current: &[TaskAttachment], files: &[TaskAttachment]) -> bool {
    files.len() < current.len() && files.iter().all(|file| current.contains(file))
}

impl TaskDoc {
    pub fn create(task: NewTask<'_>) -> Result<Self> {
        Self::create_inner(task, None, None)
    }

    pub(crate) fn create_named(
        task: NewTask<'_>,
        title: &str,
        identity: Option<(&str, &str)>,
    ) -> Result<Self> {
        Self::create_inner(task, Some(title), identity)
    }

    fn create_inner(
        task: NewTask<'_>,
        title: Option<&str>,
        identity: Option<(&str, &str)>,
    ) -> Result<Self> {
        check_new_stage(task.stage)?;
        if let Some(title) = title {
            check_title(title)?;
        }
        if title.is_none() || !task.text.trim().is_empty() {
            check_text(task.text, "prompt")?;
            // The plain card's title is the prompt's first line: it refuses
            // only the line separators and the nine bidirectional characters
            // (zerus-63rru.75 round 2). The prompt check already owns the
            // tabs and the other control characters, so a pasted table or
            // code keeps working; the explicit title keeps the full rule.
            if title.is_none()
                && task
                    .text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .any(|ch| {
                        let point = ch as u32;
                        matches!(point, 0x2028 | 0x2029 | 0x202A..=0x202E | 0x2066..=0x2069)
                    })
            {
                return Err(Error::new(
                    "task title must be nonempty, at most 512 UTF-8 bytes and contain no control or bidirectional characters",
                ));
            }
        } else if !task.text.is_empty() {
            return Err(Error::new(
                "an empty description must contain no whitespace",
            ));
        }
        if !ids::is_short_id(task.id) {
            return Err(Error::new(format!("invalid task ID {:?}", task.id)));
        }
        if !rank::is_rank(task.rank) {
            return Err(Error::new("invalid rank"));
        }
        if task.project.is_empty() || task.project.len() > 128 {
            return Err(Error::new("invalid project ID"));
        }
        let uuid = match identity {
            Some((uuid, _)) if ids::is_uuid(uuid) => uuid.to_owned(),
            Some(_) => return Err(Error::new("invalid task creation identity")),
            None => ids::new_uuid(),
        };
        let doc = LoroDoc::new();
        let meta = doc.get_map("meta");
        let planned = matches!(
            &task.target,
            Target::NewSession { .. } | Target::Subagent { .. }
        );
        if let Target::NewSession { parent } | Target::Subagent { parent } = &task.target {
            if parent.session_id.is_empty() || parent.machine.is_empty() {
                return Err(Error::new(
                    "choose a parent with a confirmed session identity",
                ));
            }
        }
        let identified = matches!(&task.target, Target::Session(key) if !key.session_id.is_empty())
            || matches!(&task.author, Actor::Agent(key) if !key.session_id.is_empty());
        meta.insert(
            "schema",
            if title.is_some() {
                NAMED_TASK_SCHEMA
            } else if planned {
                DISCUSSION_TASK_SCHEMA
            } else if identified {
                BASE_TASK_SCHEMA
            } else {
                SCHEMA
            },
        )?;
        meta.insert("uuid", uuid.as_str())?;
        meta.insert("id", task.id)?;
        meta.insert("project", task.project)?;
        meta.insert("created_at", task.now)?;
        meta.insert("author", serde_json::to_string(&task.author)?)?;
        if let Some((_, fingerprint)) = identity {
            meta.insert("creation_fingerprint", fingerprint)?;
        }
        let fields = doc.get_map("fields");
        fields.insert("stage", task.stage.as_str())?;
        fields.insert("stage_at", task.now)?;
        fields.insert("target", serde_json::to_string(&task.target)?)?;
        fields.insert("rank", task.rank)?;
        fields.insert("summary", "")?;
        if let Some(title) = title {
            fields.insert("title", title)?;
        }
        doc.get_text("text").insert(0, task.text)?;
        let this = Self {
            uuid,
            doc,
            saved: None,
            saved_len: 0,
            needs_fold: false,
            saved_stage: None,
            created_here: true,
            projection_only: false,
        };
        this.log("created", &task.author, task.now, "", None)?;
        this.doc.commit();
        Ok(this)
    }

    /// A document from a snapshot, possibly followed by updates.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let doc = LoroDoc::new();
        doc.import(bytes)?;
        let uuid = string(&doc.get_map("meta"), "uuid")
            .ok_or_else(|| Error::failed("not a task document"))?;
        if !ids::is_uuid(&uuid) {
            return Err(Error::failed("not a task document"));
        }
        Ok(Self {
            uuid,
            doc,
            saved: None,
            saved_len: 0,
            needs_fold: false,
            saved_stage: None,
            created_here: false,
            projection_only: false,
        })
    }

    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// Imports another replica's changes, restoring the execution schema when
    /// a merged lower value replaced it.
    pub fn import(&mut self, bytes: &[u8]) -> Result<()> {
        self.apply_checked(bytes)?;
        self.repair_schema()?;
        Ok(())
    }

    pub(crate) fn apply_checked(&mut self, bytes: &[u8]) -> Result<()> {
        let doc = self.doc.fork();
        if doc.import(bytes)?.pending.is_some() {
            return Err(Error::new(
                "the task update has incomplete causal history; the saved copy is kept",
            ));
        }
        let candidate = Self {
            uuid: self.uuid.clone(),
            doc,
            saved: self.saved.clone(),
            saved_len: self.saved_len,
            needs_fold: self.needs_fold,
            saved_stage: self.saved_stage,
            created_here: false,
            projection_only: self.projection_only,
        };
        if string(&candidate.meta(), "uuid") != Some(self.uuid.clone())
            || string(&candidate.meta(), "project") != string(&self.meta(), "project")
        {
            return Err(Error::new(
                "the update changes the original task or project identity",
            ));
        }
        // Legacy unrooted DTOs still decode/merge for projection. They cannot
        // construct store authority: every authoritative save/import calls
        // validate_cutover_history and the native pin gate independently.
        // Never skip a malformed, replaced or newly received actual root.
        if !self.projection_only
            && (self.meta().get("floor").is_some()
                || self.meta().get("cutover").is_some()
                || candidate.meta().get("floor").is_some()
                || candidate.meta().get("cutover").is_some())
        {
            candidate.validate_cutover_history()?;
        }
        if self
            .cutover_seal()?
            .is_some_and(|seal| candidate.cutover_seal().ok().flatten().as_ref() != Some(&seal))
        {
            return Err(Error::new(
                "an incoming update cannot replace the original task seal",
            ));
        }
        self.doc = candidate.doc;
        self.created_here = false;
        Ok(())
    }

    pub fn snapshot(&self) -> Result<Vec<u8>> {
        Ok(self.doc.export(ExportMode::Snapshot)?)
    }

    /// Current state without the operations before these frontiers. The
    /// logical task history and execution facts are state, so remain intact.
    pub(crate) fn shallow_snapshot(&self) -> Result<Vec<u8>> {
        self.doc.commit();
        if self.doc.state_vv() != self.doc.oplog_vv() {
            return Err(Error::new("the task has unapplied changes"));
        }
        Ok(self
            .doc
            .export(ExportMode::shallow_snapshot(&self.doc.oplog_frontiers()))?)
    }

    pub(crate) fn is_shallow(&self) -> bool {
        self.doc.is_shallow()
    }

    pub fn version(&self) -> Vec<u8> {
        self.doc.oplog_vv().encode()
    }

    /// A stable, bounded token for list readers to notice journal-only edits.
    /// Sorting the clock keeps it identical after reload and replica exchange.
    pub fn revision(&self) -> String {
        let clock: std::collections::BTreeMap<_, _> = self
            .doc
            .oplog_vv()
            .iter()
            .map(|(peer, count)| (peer.to_string(), *count))
            .collect();
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&clock).expect("document clock"))
        )
    }

    pub fn export_since(&self, version: &[u8]) -> Result<Vec<u8>> {
        let version = VersionVector::decode(version)?;
        Ok(self.doc.export(ExportMode::updates(&version))?)
    }

    pub(crate) fn export_since_vv(&self, version: &VersionVector) -> Result<Vec<u8>> {
        Ok(self.doc.export(ExportMode::updates(version))?)
    }

    pub(crate) fn current_vv(&self) -> VersionVector {
        self.doc.oplog_vv()
    }

    /// The document's version as a canonical map, when its state is every
    /// change it holds (attached, nothing pending): what a fence compares.
    pub fn applied_version(&self) -> Option<crate::Version> {
        let oplog = self.doc.oplog_vv();
        (oplog == self.doc.state_vv()).then(|| crate::sync::version(&oplog))
    }

    /// Why this copy cannot be a fence's common version, or none when it
    /// can: it holds a valid floor, its RAW stored schema is at least the
    /// execution schema (never `schema()`, which raises a cut-over task over
    /// a lower merged value an older build still reads), and its state is
    /// every change it holds (zerus-cg5h.4 S2b.2 C3c2).
    pub fn fence_ready(&self) -> Option<&'static str> {
        if self.floor_record().is_none() {
            Some("no_floor")
        } else if self.stored_schema() < execution::EXECUTION_TASK_SCHEMA {
            Some("schema")
        } else if self.applied_version().is_none() {
            Some("damaged")
        } else {
            None
        }
    }

    /// The updates a replica at `version` (canonical) lacks of this task.
    pub fn export_since_version(&self, version: &crate::Version) -> Result<Vec<u8>> {
        let canonical = crate::sync::canonical_version(version)
            .ok_or_else(|| Error::new("the version is not in its canonical form"))?;
        self.export_since_vv(&crate::sync::vector(&canonical)?)
    }

    /// The operations at the document's frontiers, as updates every copy
    /// that holds this version already has: they import as nothing, add no
    /// change, and differ whenever the version does.
    pub(crate) fn export_frontier(&self) -> Result<Vec<u8>> {
        let spans: Vec<loro::IdSpan> = self
            .doc
            .oplog_frontiers()
            .iter()
            .map(|id| loro::IdSpan::new(id.peer, id.counter, id.counter + 1))
            .collect();
        Ok(self.doc.export(ExportMode::updates_in_range(spans))?)
    }

    /// Applies stored updates. Invalid/incomplete bytes retain the existing
    /// fold/recovery behavior; temporary proof exhaustion is never corruption.
    pub(crate) fn import_block(&mut self, bytes: &[u8]) -> Result<bool> {
        match self.apply_checked(bytes) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == crate::ErrorKind::Busy => Err(error),
            Err(_) => Ok(false),
        }
    }

    fn meta(&self) -> LoroMap {
        self.doc.get_map("meta")
    }

    fn fields(&self) -> LoroMap {
        self.doc.get_map("fields")
    }

    /// The bytes of text the task holds: its prompt and every history entry.
    /// Snapshots compress, so their size does not bound the history.
    fn content_len(&self) -> usize {
        let list = self.doc.get_list("log");
        let entries: usize = (0..list.len())
            .filter_map(|index| list.get(index)?.into_container().ok()?.into_map().ok())
            .map(|entry| {
                string(&entry, "text").map_or(0, |text| text.len())
                    + string(&entry, "delivery").map_or(0, |text| text.len())
            })
            .sum();
        self.doc.get_text("text").len_utf8() + entries
    }

    /// Writes a field as is, bypassing the rules, to build damaged documents.
    #[cfg(test)]
    pub(crate) fn set_raw_field(&mut self, key: &str, value: &str) {
        self.fields().insert(key, value).unwrap();
        self.doc.commit();
    }

    /// Writes the cancelled mark and its reason exactly as an older build's
    /// set_stage left them, for tests of the stale-mark intake and the
    /// binding write's clear (zerus-63rru.36 round 3).
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_cancelled_for_test(&mut self, at: i64, reason: &str) {
        self.fields().insert("cancelled_at", at).unwrap();
        self.fields().insert("cancel_reason", reason).unwrap();
        self.doc.commit();
    }

    pub fn id(&self) -> String {
        string(&self.meta(), "id").unwrap_or_default()
    }

    pub(crate) fn creation_fingerprint(&self) -> Option<String> {
        string(&self.meta(), "creation_fingerprint")
    }

    pub fn title(&self) -> String {
        string(&self.fields(), "title").unwrap_or_default()
    }

    pub fn set_title(&mut self, title: &str, actor: &Actor, now: i64) -> Result<()> {
        self.writable()?;
        check_title(title)?;
        if self.title() == title {
            return Ok(());
        }
        if self.content_len() + title.len() > MAX_DOCUMENT {
            return Err(Error::new("task history is full"));
        }
        self.upgrade_schema(NAMED_TASK_SCHEMA)?;
        self.fields().insert("title", title)?;
        self.log("title", actor, now, title, None)?;
        self.commit()?;
        Ok(())
    }

    /// A task that records attempts keeps their schema even when a lower
    /// upgrade by an older replica won the merged `meta.schema` value.
    pub fn schema(&self) -> i64 {
        let stored = self.stored_schema();
        if self.meta().get("cutover").is_some() {
            stored.max(TASK_SCHEMA)
        } else if self.execution_managed() {
            stored.max(execution::EXECUTION_TASK_SCHEMA)
        } else {
            stored
        }
    }

    /// The schema the document stores, as merged: what an older build reads
    /// when it decides whether it may write, before any rule of this build
    /// (`schema` raises it for a cut-over task).
    pub fn stored_schema(&self) -> i64 {
        integer(&self.meta(), "schema").unwrap_or(0)
    }

    /// Whether the task was cut over to the execution schema: it records
    /// attempts, or took its floor before the first one. A floor this build
    /// cannot read still counts, so it never reopens the legacy path.
    pub fn execution_managed(&self) -> bool {
        self.executes_attempts() || self.meta().get("floor").is_some()
    }

    /// The task's cutover floor, when it took one (`floor`).
    pub fn floor_record(&self) -> Option<execution::Floor> {
        string(&self.meta(), "floor")
            .and_then(|raw| serde_json::from_str::<execution::Floor>(&raw).ok())
            .filter(|floor| {
                floor.schema >= execution::EXECUTION_TASK_SCHEMA
                    && floor.counter >= 0
                    && floor.peer.parse::<u64>().is_ok()
            })
    }

    /// Cuts the task over to the execution schema before its first attempt:
    /// a floor record, then the stored schema raised to it, in one commit.
    /// It changes no stage, history, attempt or receipt; true when it wrote.
    /// An older replica refuses every guarded write once its copy holds it.
    pub fn floor(&mut self) -> Result<bool> {
        self.writable()?;
        if self.floor_record().is_some() {
            self.repair_schema()?;
            return Ok(false);
        }
        let peer = self.doc.peer_id();
        let counter = self.doc.oplog_vv().get(&peer).copied().unwrap_or(0);
        // The floor record is this commit's first operation: its counter is
        // the one a replica must hold to hold the floor.
        let floor = execution::Floor {
            schema: execution::EXECUTION_TASK_SCHEMA,
            peer: peer.to_string(),
            counter,
        };
        self.meta()
            .insert("floor", serde_json::to_string(&floor)?)?;
        self.meta().insert(
            "schema",
            execution::EXECUTION_TASK_SCHEMA.max(self.stored_schema()),
        )?;
        self.doc.commit();
        Ok(true)
    }

    /// Whether the task records execution attempts (`execution` module).
    pub fn executes_attempts(&self) -> bool {
        !self.doc.get_map("attempts").is_empty()
    }

    pub fn stage(&self) -> Result<Stage> {
        Stage::parse(&string(&self.fields(), "stage").unwrap_or_default())
    }

    /// When the task entered its current stage (Unix milliseconds).
    pub fn stage_at(&self) -> i64 {
        integer(&self.fields(), "stage_at")
            .or_else(|| integer(&self.meta(), "created_at"))
            .unwrap_or(0)
    }

    pub fn rank(&self) -> String {
        string(&self.fields(), "rank").unwrap_or_default()
    }

    /// The session it was last sent to.
    pub fn session(&self) -> Option<SessionKey> {
        string(&self.fields(), "session").and_then(|s| serde_json::from_str(&s).ok())
    }

    /// How `session` was bound from a name-only key to its verified run,
    /// kept as the current execution's evidence until its next boundary.
    fn binding(&self) -> Option<execution::SessionBinding> {
        string(&self.fields(), "session_binding")
            .and_then(|s| serde_json::from_str::<execution::SessionBinding>(&s).ok())
            .filter(execution::SessionBinding::valid)
    }

    /// SHA-256 (lowercase hex) of the requirement as it is now: the prompt
    /// text and its attachments, exactly. Titles and comments are not part
    /// of it.
    pub fn requirement_digest(&self) -> Result<String> {
        let text = self.doc.get_text("text").to_string();
        let bytes = serde_json::to_vec(&(text, self.attachments()?))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    /// An edit of the requirement, its description or its attachments,
    /// counts the next revision. A receipt for input reserved under an
    /// earlier revision never advances the edited task.
    fn count_requirement(&self) -> Result<()> {
        let next = self.requirement().saturating_add(1);
        self.fields()
            .insert("requirement", i64::try_from(next).unwrap_or(i64::MAX))?;
        Ok(())
    }

    /// The requirement revision: 1 until an edit of the requirement counts one.
    pub fn requirement(&self) -> u64 {
        integer(&self.fields(), "requirement")
            .and_then(|n| u64::try_from(n).ok())
            .filter(|n| *n > 0)
            .unwrap_or(1)
    }

    /// The attempt records and attempt events (`execution` module), checked
    /// and bounded. Reading them never reads the history.
    pub(crate) fn execution_records(&self) -> Result<(Vec<Attempt>, Vec<AttemptEvent>)> {
        let map = self.doc.get_map("attempts");
        let list = self.doc.get_list("attempt_events");
        if map.len() > MAX_ATTEMPTS || list.len() > MAX_ATTEMPT_EVENTS {
            return Err(Error::failed("too many task attempts"));
        }
        let mut attempts = Vec::with_capacity(map.len());
        let mut failure = None;
        map.for_each(|key, value| {
            let attempt = match value.into_value() {
                Ok(LoroValue::String(raw)) => serde_json::from_str::<Attempt>(&raw)
                    .map_err(Error::from)
                    .and_then(|attempt| check_attempt(key, &attempt).map(|()| attempt)),
                _ => Err(Error::failed("invalid task attempt")),
            };
            match attempt {
                Ok(attempt) => attempts.push(attempt),
                Err(error) => failure = failure.take().or(Some(error)),
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        let mut events = Vec::with_capacity(list.len());
        for index in 0..list.len() {
            let event = match list.get(index).map(|item| item.into_value()) {
                Some(Ok(LoroValue::String(raw))) => serde_json::from_str::<AttemptEvent>(&raw)?,
                _ => return Err(Error::failed("invalid task attempt event")),
            };
            check_event(&event)?;
            events.push(event);
        }
        Ok((attempts, events))
    }

    pub fn view(&self) -> Result<TaskView> {
        self.view_with_history_ids().map(|(view, _)| view)
    }

    /// The canonical view plus immutable Loro identities in the same order as
    /// `view.log`. A derived history index can keep a cursor across appends,
    /// receipt refinements and merges without exposing Loro to its caller.
    pub fn view_with_history_ids(&self) -> Result<(TaskView, Vec<String>)> {
        let meta = self.meta();
        let fields = self.fields();
        let writable = !self.projection_only && self.schema() <= TASK_SCHEMA;
        let mut log = Vec::new();
        let list = self.doc.get_list("log");
        for index in 0..list.len() {
            let Some(item) = list.get(index) else {
                continue;
            };
            let Ok(container) = item.into_container() else {
                continue;
            };
            let Ok(entry) = container.into_map() else {
                continue;
            };
            log.push((
                entry.id().to_string(),
                LogEntry {
                    kind: string(&entry, "kind").unwrap_or_default(),
                    actor: decode(&entry, "actor").unwrap_or(Actor::Person),
                    at: integer(&entry, "at").unwrap_or(0),
                    text: string(&entry, "text").unwrap_or_default(),
                    session: string(&entry, "session").and_then(|s| serde_json::from_str(&s).ok()),
                    request_id: string(&entry, "request_id"),
                    delivery: match string(&entry, "delivery")
                        .map(|raw| serde_json::from_str(&raw))
                        .transpose()
                    {
                        Err(_) if !writable => None,
                        delivery => delivery?,
                    },
                    attachments: match string(&entry, "attachments") {
                        Some(raw) => match serde_json::from_str::<Vec<TaskAttachment>>(&raw)
                            .map_err(Error::from)
                            .and_then(|files| {
                                check_attachments(&files)?;
                                Ok(files)
                            }) {
                            Err(_) if !writable => Vec::new(),
                            files => files?,
                        },
                        None => Vec::new(),
                    },
                    stale: string(&entry, "stale").and_then(|s| Stale::parse(&s)),
                    reported_attempt: string(&entry, "reported_attempt"),
                    considered: integer(&entry, "considered").and_then(|n| u64::try_from(n).ok()),
                    reviews: string(&entry, "reviews"),
                    of: string(&entry, "of"),
                },
            ));
        }
        // A merge orders entries appended on two replicas by peer, not by time;
        // the history reads oldest first, keeping the order of equal times.
        log.sort_by_key(|(_, entry)| entry.at);
        // Receipt events refine the original message without rewriting its text.
        let mut receipts: std::collections::BTreeMap<String, crate::TaskDelivery> =
            std::collections::BTreeMap::new();
        let strength = |state: &str| match state {
            "submitted" => 3,
            "failed" => 2,
            "uncertain" => 1,
            _ => 0,
        };
        for (_, entry) in log.iter().filter(|(_, e)| e.kind == "message_delivery") {
            if let (Some(id), Some(delivery)) = (&entry.request_id, &entry.delivery) {
                if receipts
                    .get(id)
                    .is_none_or(|old| strength(&delivery.state) >= strength(&old.state))
                {
                    receipts.insert(id.clone(), delivery.clone());
                }
            }
        }
        for (_, message) in log.iter_mut().filter(|(_, e)| e.kind == "message") {
            if let Some(receipt) = message.request_id.as_ref().and_then(|id| receipts.get(id)) {
                let prompt = message
                    .delivery
                    .as_ref()
                    .map(|d| d.prompt.clone())
                    .unwrap_or_default();
                message.delivery = Some(crate::TaskDelivery {
                    prompt,
                    ..receipt.clone()
                });
            }
        }
        // Concurrent replicas may append the same queued request. It appears and
        // dispatches once; a conflicting identity fails closed.
        let mut messages = std::collections::BTreeMap::new();
        let mut unique = Vec::with_capacity(log.len());
        for (id, entry) in log {
            if entry.kind == "message" {
                if let Some(id) = &entry.request_id {
                    if let Some(old) = messages.get(id) {
                        let old: &LogEntry = old;
                        if old.text != entry.text
                            || old.actor != entry.actor
                            || old.attachments != entry.attachments
                            || old.delivery != entry.delivery
                        {
                            return Err(Error::new("conflicting task message identity"));
                        }
                        continue;
                    }
                    messages.insert(id.clone(), entry.clone());
                }
            }
            unique.push((id, entry));
        }
        let (ids, log): (Vec<_>, Vec<_>) = unique.into_iter().unzip();
        // A newer schema may encode a field differently: such a task is shown
        // read-only with defaults. In this schema a field that does not decode
        // is damage, and the store sets the task aside.
        let author = match decode(&meta, "author") {
            Err(_) if !writable => Actor::Person,
            author => author?,
        };
        let target = match decode(&fields, "target") {
            Err(_) if !writable => Target::Any,
            target => target?,
        };
        let attachments = match self.attachments() {
            Err(_) if !writable => Vec::new(),
            files => files?,
        };
        let stage = self.stage()?;
        let session = self.session();
        let execution = match self.execution_records() {
            Ok((attempts, events)) => {
                let mut execution = execution::project(
                    stage,
                    session.as_ref(),
                    self.binding().as_ref(),
                    &log,
                    &attempts,
                    &events,
                    self.requirement(),
                );
                // Execution entries no attempt explains make the task a
                // conflict, whatever its attempts project.
                if let Some(execution) = execution.as_mut() {
                    execution.unexplained = execution::unexplained(&log, &ids, &attempts);
                    execution.conflict |= !execution.unexplained.is_empty();
                }
                execution
            }
            Err(_) if !writable => None,
            Err(error) => return Err(error),
        };
        Ok((
            TaskView {
                uuid: self.uuid.clone(),
                id: self.id(),
                project: string(&meta, "project").unwrap_or_default(),
                schema: self.schema(),
                writable,
                created_at: integer(&meta, "created_at").unwrap_or(0),
                // Documents written before this field existed report their creation.
                stage_at: integer(&fields, "stage_at")
                    .or_else(|| integer(&meta, "created_at"))
                    .unwrap_or(0),
                author,
                stage,
                target,
                #[cfg(not(feature = "test-without-team"))]
                team: self.team()?,
                #[cfg(not(feature = "test-without-team"))]
                team_invalid: self.team_invalid(),
                rank: self.rank(),
                session,
                worker: match string(&fields, "worker")
                    .map(|raw| serde_json::from_str(&raw))
                    .transpose()
                {
                    Err(_) if !writable => None,
                    worker => worker?,
                },
                summary: string(&fields, "summary").unwrap_or_default(),
                cancelled_at: integer(&fields, "cancelled_at"),
                cancel_reason: string(&fields, "cancel_reason").unwrap_or_default(),
                title: self.title(),
                text: self.doc.get_text("text").to_string(),
                attachments,
                log,
                execution,
            },
            ids,
        ))
    }

    fn log(
        &self,
        kind: &str,
        actor: &Actor,
        at: i64,
        text: &str,
        session: Option<&SessionKey>,
    ) -> Result<LoroMap> {
        let list = self.doc.get_list("log");
        let entry = list.insert_container(list.len(), LoroMap::new())?;
        entry.insert("kind", kind)?;
        entry.insert("actor", serde_json::to_string(actor)?)?;
        entry.insert("at", at)?;
        if !text.is_empty() {
            entry.insert("text", text)?;
        }
        if let Some(session) = session {
            entry.insert("session", serde_json::to_string(session)?)?;
        }
        Ok(entry)
    }

    /// Commits a change. A task that records attempts keeps the execution
    /// schema in every commit: a lower value an older replica's concurrent
    /// upgrade left in the merge is raised again here, never lowered.
    fn commit(&self) -> Result<()> {
        if self.meta().get("cutover").is_some() {
            self.upgrade_schema(TASK_SCHEMA)?;
        } else if self.execution_managed() {
            self.upgrade_schema(execution::EXECUTION_TASK_SCHEMA)?;
        }
        self.doc.commit();
        Ok(())
    }

    /// Whether the stored schema is below the execution schema its attempts
    /// need: a lower upgrade by an older replica won the merge.
    pub fn needs_schema_repair(&self) -> bool {
        (self.meta().get("cutover").is_some() && self.stored_schema() < TASK_SCHEMA)
            || (self.execution_managed() && self.stored_schema() < execution::EXECUTION_TASK_SCHEMA)
    }

    /// Raises the stored schema of a task with attempts to the execution
    /// schema in a commit of its own; true when it did. Save the task before
    /// using or exporting it.
    pub fn repair_schema(&mut self) -> Result<bool> {
        if !self.needs_schema_repair() {
            return Ok(false);
        }
        self.commit()?;
        Ok(true)
    }

    /// Raises the stored schema, never lowering it. It compares the stored
    /// value, so an attempt write also restores the execution schema that a
    /// concurrent lower upgrade replaced.
    fn upgrade_schema(&self, required: i64) -> Result<()> {
        if self.stored_schema() < required {
            self.meta().insert("schema", required)?;
        }
        Ok(())
    }

    fn check_media_budget(
        &self,
        description: &[TaskAttachment],
        extra: &[TaskAttachment],
    ) -> Result<()> {
        let mut total = 0usize;
        for file in description.iter().chain(extra) {
            total += file.bytes()?.len();
        }
        for entry in self.view()?.log {
            for file in entry.attachments {
                total += file.bytes()?.len();
            }
        }
        if total > crate::MAX_ATTACHMENT_TOTAL {
            return Err(Error::new(
                "task attachments are full: 20 MiB across description and comments",
            ));
        }
        Ok(())
    }

    pub(crate) fn writable(&self) -> Result<()> {
        if self.projection_only {
            return Err(Error::new("This sealed task is read-only on the phone; use its computer to change or start it"));
        }
        if self.schema() > TASK_SCHEMA {
            return Err(Error::new("Update Zerus to edit this task"));
        }
        Ok(())
    }

    pub fn attachments(&self) -> Result<Vec<TaskAttachment>> {
        let files: Vec<TaskAttachment> = match string(&self.fields(), "attachments") {
            Some(text) => serde_json::from_str(&text)?,
            None => Vec::new(),
        };
        check_attachments(&files)?;
        Ok(files)
    }

    pub fn set_attachments(
        &mut self,
        files: &[TaskAttachment],
        actor: &Actor,
        now: i64,
    ) -> Result<()> {
        self.require(
            &[Stage::Backlog, Stage::Todo],
            "attachments can be edited only in Backlog or To do",
        )?;
        check_attachments(files)?;
        self.check_media_budget(files, &[])?;
        let current = self.attachments()?;
        if files == current {
            return Ok(());
        }
        let encoded = serde_json::to_string(files)?;
        // Every replacement appends the whole set to the operation history.
        // Stop before the document outgrows what a phone can assemble, but
        // keep removals available so an oversized legacy task can be reduced.
        let retained = self.snapshot()?.len();
        if retained
            .saturating_add(encoded.len())
            .saturating_add(64 * 1024)
            > crate::MAX_TASK_DOCUMENT
            && !removal_only(&current, files)
        {
            return Err(Error::new(
                "this task's attachment history is full; remove attachments or start a new task",
            ));
        }
        // Never lower this after removal: old binaries must not mutate a task
        // whose media history they cannot preserve or deliver.
        self.upgrade_schema(BASE_TASK_SCHEMA)?;
        self.fields().insert("attachments", encoded)?;
        self.count_requirement()?;
        self.log("attachments", actor, now, "Updated attachments", None)?;
        self.commit()?;
        Ok(())
    }

    /// Enrich a legacy name-only key after the host resolved it uniquely. Names
    /// remain hints; subsequent operations and replicas retain the same ID.
    pub fn bind_session_identity(&mut self, old: &SessionKey, current: &SessionKey) -> Result<()> {
        self.writable()?;
        if old.machine != current.machine
            || current.session_id.is_empty()
            || (!old.session_id.is_empty() && old != current)
        {
            return Err(Error::new("task session identity changed"));
        }
        self.upgrade_schema(BASE_TASK_SCHEMA)?;
        let target = match decode::<Target>(&self.fields(), "target")? {
            Target::Session(key) if &key == old => Some(Target::Session(current.clone())),
            Target::NewSession { parent } if &parent == old => Some(Target::NewSession {
                parent: current.clone(),
            }),
            Target::Subagent { parent } if &parent == old => Some(Target::Subagent {
                parent: current.clone(),
            }),
            _ => None,
        };
        if let Some(target) = target {
            self.fields()
                .insert("target", serde_json::to_string(&target)?)?;
        }
        if self.session().as_ref() == Some(old) {
            self.fields()
                .insert("session", serde_json::to_string(current)?)?;
            // The history keeps the name-only boundary; the execution reads
            // this binding, to this verified run alone, as the same executor.
            if old.session_id.is_empty() {
                let binding = execution::SessionBinding {
                    from: old.clone(),
                    to: current.clone(),
                };
                self.fields()
                    .insert("session_binding", serde_json::to_string(&binding)?)?;
            }
        }
        self.commit()?;
        Ok(())
    }

    fn require(&self, stages: &[Stage], message: &str) -> Result<Stage> {
        self.writable()?;
        let stage = self.stage()?;
        if !stages.contains(&stage) {
            return Err(Error::new(message));
        }
        Ok(stage)
    }

    pub fn edit_text(&mut self, text: &str, actor: &Actor, now: i64) -> Result<()> {
        self.require(
            &[Stage::Backlog, Stage::Todo],
            "the prompt can be edited only in Backlog or To do",
        )?;
        if !text.is_empty() || self.title().is_empty() {
            check_text(text, "prompt")?;
        }
        let current = self.doc.get_text("text");
        let quick = UpdateOptions {
            timeout_ms: Some(200.0),
            ..UpdateOptions::default()
        };
        // A character diff merges best; a rewrite too large to diff in time is
        // replaced as a whole, with the same result.
        let changed = current.to_string() != text;
        if current.update(text, quick).is_err() {
            current.delete(0, current.len_unicode())?;
            current.insert(0, text)?;
        }
        if changed {
            self.count_requirement()?;
        }
        self.log("edited", actor, now, "", None)?;
        self.commit()?;
        Ok(())
    }

    pub fn set_rank(&mut self, value: &str) -> Result<()> {
        // Board order is presentation priority, independent of execution state.
        // In particular, moving an active card never interrupts its worker.
        self.writable()?;
        if !rank::is_rank(value) {
            return Err(Error::new("invalid rank"));
        }
        self.fields().insert("rank", value)?;
        self.commit()?;
        Ok(())
    }

    /// Backlog ⇄ To do, Accept (In review → Done), Back to To do (from In review or Done).
    pub fn set_stage(&mut self, to: Stage, actor: &Actor, now: i64) -> Result<()> {
        self.writable()?;
        let from = self.stage()?;
        if from == to {
            return Err(Error::new(format!(
                "{} is already in {}",
                self.id(),
                to.label()
            )));
        }
        let allowed = matches!(
            (from, to),
            (Stage::Backlog, Stage::Todo)
                | (Stage::Todo, Stage::Backlog)
                | (Stage::Review, Stage::Done)
                | (Stage::Review, Stage::Todo)
                | (Stage::Done, Stage::Todo)
        );
        if !allowed {
            return Err(Error::new(format!(
                "a task cannot go from {} to {}",
                from.label(),
                to.label()
            )));
        }
        self.fields().insert("stage", to.as_str())?;
        self.fields().insert("stage_at", now)?;
        // A task that leaves Review or Done for a stage where no work has
        // started is no longer cancelled: the mark and its reason go with
        // the stage, in the same commit (zerus-63rru.36). Every other move
        // keeps them.
        if matches!(from, Stage::Review | Stage::Done) && matches!(to, Stage::Todo | Stage::Backlog)
        {
            self.fields().delete("cancelled_at")?;
            self.fields().delete("cancel_reason")?;
        }
        self.log("stage", actor, now, to.as_str(), None)?;
        self.commit()?;
        Ok(())
    }

    pub fn set_target(&mut self, target: &Target, actor: &Actor, now: i64) -> Result<()> {
        self.require(
            &[Stage::Backlog, Stage::Todo],
            "the session can be changed only in Backlog or To do",
        )?;
        if matches!(target, Target::Session(key) if !key.session_id.is_empty()) {
            self.upgrade_schema(BASE_TASK_SCHEMA)?;
        }
        if let Target::NewSession { parent } | Target::Subagent { parent } = target {
            if parent.session_id.is_empty() || parent.machine.is_empty() {
                return Err(Error::new(
                    "choose a parent with a confirmed session identity",
                ));
            }
            self.upgrade_schema(DISCUSSION_TASK_SCHEMA)?;
        }
        self.fields()
            .insert("target", serde_json::to_string(target)?)?;
        let label = match target {
            Target::Any => "any".to_owned(),
            Target::Session(key) => key.session.clone(),
            Target::NewSession { parent } => format!("new session from {}", parent.session),
            Target::Subagent { parent } => format!("subagent of {}", parent.session),
        };
        self.log("target", actor, now, &label, None)?;
        self.commit()?;
        Ok(())
    }

    /// Whether a send could be recorded; check it before pasting anything.
    /// A task that records attempts refuses a send that would not reserve
    /// one, before any input.
    pub fn check_send(&self) -> Result<()> {
        self.sendable()?;
        self.legacy_start()
    }

    /// Whether the task can begin an execution: its stage, and no attempt
    /// whose delivery is unknown.
    fn sendable(&self) -> Result<()> {
        self.writable()?;
        self.not_held()?;
        match self.stage()? {
            Stage::Backlog | Stage::Todo => Ok(()),
            Stage::Active => Err(self.already_running()),
            stage => Err(Error::new(format!(
                "a task in {} is not sent; move it back to To do first",
                stage.label()
            ))),
        }
    }

    /// The send that Zerus pasted into `session`; the task becomes In progress.
    pub fn record_send(
        &mut self,
        session: &SessionKey,
        request_id: &str,
        actor: &Actor,
        now: i64,
    ) -> Result<()> {
        self.check_send()?;
        if !session.session_id.is_empty() {
            self.upgrade_schema(BASE_TASK_SCHEMA)?;
        }
        let fields = self.fields();
        fields.insert("stage", Stage::Active.as_str())?;
        fields.insert("stage_at", now)?;
        fields.insert("session", serde_json::to_string(session)?)?;
        fields.delete("session_binding")?;
        fields.delete("worker")?;
        fields.delete("cancelled_at")?;
        fields.delete("cancel_reason")?;
        self.log("sent", actor, now, "", Some(session))?
            .insert("request_id", request_id)?;
        self.commit()?;
        Ok(())
    }

    /// The refusal for a second send of a task this copy shows In progress.
    pub fn already_running(&self) -> Error {
        let session = string(&self.fields(), "session")
            .and_then(|s| serde_json::from_str::<SessionKey>(&s).ok())
            .map_or_else(|| "a session".to_owned(), |key| key.session);
        Error::new(format!("{} is already In progress in {session}", self.id()))
    }

    /// Whether a handoff could be recorded, checked before any input; a task
    /// that records attempts refuses one that would not reserve its attempt.
    pub fn check_handoff(&self, actor: &Actor) -> Result<()> {
        self.handoffable(actor)?;
        self.legacy_start()
    }

    fn handoffable(&self, actor: &Actor) -> Result<()> {
        self.require(
            &[Stage::Active],
            "only a task In progress can be handed off",
        )?;
        self.not_held()?;
        if let Actor::Agent(own) = actor {
            if self.session().as_ref() != Some(own) {
                return Err(Error::new(
                    "only the task's current session can hand it off",
                ));
            }
        }
        Ok(())
    }

    /// A confirmed delivery transfers both the working session and its pin.
    pub fn record_handoff(
        &mut self,
        session: &SessionKey,
        request_id: &str,
        actor: &Actor,
        now: i64,
    ) -> Result<()> {
        self.check_handoff(actor)?;
        if self.session().as_ref() == Some(session) {
            return Err(Error::new("choose another session for the handoff"));
        }
        if !session.session_id.is_empty() {
            self.upgrade_schema(BASE_TASK_SCHEMA)?;
        }
        self.fields().insert(
            "target",
            serde_json::to_string(&Target::Session(session.clone()))?,
        )?;
        self.fields()
            .insert("session", serde_json::to_string(session)?)?;
        self.fields().delete("session_binding")?;
        self.fields().insert("stage_at", now)?;
        self.fields().insert("summary", "")?;
        self.fields().delete("worker")?;
        self.log("sent", actor, now, "handoff", Some(session))?
            .insert("request_id", request_id)?;
        self.commit()?;
        Ok(())
    }

    /// A result report, as `report` with `ReportKind::Ready`: returns whether
    /// it moved the task to In review.
    pub fn record_report(&mut self, summary: &str, actor: &Actor, now: i64) -> Result<bool> {
        Ok(self
            .report(summary, ReportKind::Ready, None, actor, now)?
            .moves)
    }

    /// Records a report. The reducer decides it (`execution::decide_report`):
    /// only the current execution's executor moves its task to In review with
    /// a result, a person's result is an explicit human action, and progress
    /// never moves the task. A report that cannot move it is still recorded,
    /// with the reason.
    pub fn report(
        &mut self,
        summary: &str,
        kind: ReportKind,
        considered: Option<u64>,
        actor: &Actor,
        now: i64,
    ) -> Result<ReportDecision> {
        self.report_scoped(summary, kind, considered, None, actor, now)
    }

    /// Report for the attempt pinned in this execution's original context.
    /// The caller never substitutes whatever attempt happens to be current.
    pub fn report_attempt(
        &mut self,
        attempt: &str,
        summary: &str,
        kind: ReportKind,
        considered: Option<u64>,
        actor: &Actor,
        now: i64,
    ) -> Result<ReportDecision> {
        if !ids::is_uuid(attempt) {
            return Err(Error::new("a report attempt is a canonical UUID"));
        }
        self.report_scoped(summary, kind, considered, Some(attempt), actor, now)
    }

    fn report_scoped(
        &mut self,
        summary: &str,
        kind: ReportKind,
        considered: Option<u64>,
        origin: Option<&str>,
        actor: &Actor,
        now: i64,
    ) -> Result<ReportDecision> {
        self.writable()?;
        check_text(summary, "report")?;
        let considered = considered
            .map(|n| i64::try_from(n).ok().filter(|n| *n > 0))
            .map(|n| n.ok_or_else(|| Error::new("the considered revision counts from 1")))
            .transpose()?;
        // Preserve the documented direct-work rule for a pinned agent's
        // unsent queued legacy task. A cut-over task never starts implicitly
        // and never supplies its latest attempt to an unscoped report.
        if origin.is_none()
            && !self.execution_managed()
            && matches!(self.stage()?, Stage::Todo | Stage::Backlog)
            && matches!((&self.view()?.target, actor), (Target::Session(target), Actor::Agent(own)) if target == own)
        {
            self.record_start(actor, now)?;
        }
        let view = self.view()?;
        let current = view.execution.as_ref().and_then(|e| e.current.as_ref());
        let named_current = match origin {
            Some(id) => self.attempt(id)?.is_some_and(|named| {
                current.is_some_and(|current| {
                    !current.legacy
                        && current.id == named.id
                        && current.generation == named.generation
                })
            }),
            None => !self.execution_managed() || matches!(actor, Actor::Person),
        };
        let decision = if named_current {
            execution::decide_report(view.stage, view.execution.as_ref(), actor, kind, origin)
        } else {
            ReportDecision {
                moves: false,
                stale: Some(execution::Stale::OtherAttempt),
            }
        };
        if decision.moves {
            let fields = self.fields();
            fields.insert("stage", Stage::Review.as_str())?;
            fields.insert("stage_at", now)?;
            fields.insert("summary", summary)?;
        }
        let entry_kind = match kind {
            ReportKind::Progress => "progress",
            ReportKind::Ready => "report",
        };
        let entry = self.log(entry_kind, actor, now, summary, None)?;
        if let Some(origin) = origin {
            entry.insert("reported_attempt", origin)?;
        }
        if let Some(stale) = decision.stale {
            entry.insert("stale", stale.as_str())?;
        }
        if let Some(considered) = considered {
            entry.insert("considered", considered)?;
        }
        self.record_report_fact(&view, &decision, kind, actor, now)?;
        self.commit()?;
        Ok(decision)
    }

    /// The calling agent explicitly takes an existing task without pasting a
    /// second copy of the instruction into its own active conversation.
    pub fn record_start(&mut self, actor: &Actor, now: i64) -> Result<()> {
        self.writable()?;
        let Actor::Agent(own) = actor else {
            return Err(Error::new(
                "start is for an agent working in its own session",
            ));
        };
        let view = self.view()?;
        if view.stage == Stage::Active {
            if matches!(view.target, Target::Subagent { .. }) && view.worker.is_none() {
                return Err(Error::new(
                    "register the native subagent worker before starting",
                ));
            }
            // A recorded attempt is acknowledged as one; history without a
            // started attempt is acknowledged as it always was.
            if self.current_attempt()?.is_some() {
                return self.acknowledge_attempt(&view, own, actor, now);
            }
            if view.session.as_ref() != Some(own) {
                return Err(Error::new("the task is active in another session"));
            }
            let current = view
                .log
                .iter()
                .rev()
                .take_while(|entry| entry.kind != "sent");
            if current.clone().any(|entry| entry.kind == "started") {
                return Ok(());
            }
            self.log("started", actor, now, "", Some(own))?;
            self.commit()?;
            return Ok(());
        }
        self.sendable()?;
        if matches!(
            view.target,
            Target::NewSession { .. } | Target::Subagent { .. }
        ) {
            return Err(Error::new(
                "start this task through its selected executor plan",
            ));
        }
        if let Target::Session(target) = &view.target {
            if target != own {
                return Err(Error::new("the task is pinned to another session"));
            }
        }
        if !own.session_id.is_empty() {
            self.upgrade_schema(BASE_TASK_SCHEMA)?;
        }
        // A task that records attempts begins a direct-start attempt in the
        // same commit as its history entry.
        if self.execution_managed() {
            self.begin_start_attempt(&view, own, actor, now)?;
        }
        let fields = self.fields();
        fields.insert("stage", Stage::Active.as_str())?;
        fields.insert("stage_at", now)?;
        fields.insert("session", serde_json::to_string(own)?)?;
        fields.delete("session_binding")?;
        fields.delete("worker")?;
        fields.delete("cancelled_at")?;
        fields.delete("cancel_reason")?;
        self.log("started", actor, now, "", Some(own))?;
        self.commit()?;
        Ok(())
    }

    pub fn record_cancel(&mut self, reason: &str, actor: &Actor, now: i64) -> Result<()> {
        self.require(&[Stage::Active], "only a task In progress can be cancelled")?;
        // An attempt whose delivery is unknown is cancelled as that attempt,
        // never through the session that held the task before it.
        self.not_held()?;
        // The reason becomes a comment, under the same rules.
        if !reason.trim().is_empty() {
            check_text(reason, "reason")?;
        }
        let fields = self.fields();
        fields.insert("stage", Stage::Review.as_str())?;
        fields.insert("stage_at", now)?;
        fields.insert("cancelled_at", now)?;
        fields.insert("cancel_reason", reason.trim())?;
        if let Some(current) = self.current_attempt()? {
            self.push_event(&current.id, execution::EventKind::Cancelled, actor, now)?;
        }
        self.log("cancelled", actor, now, "", None)?;
        // The reason is a comment for people and agents; it is never sent.
        if !reason.trim().is_empty() {
            self.log("comment", actor, now, reason.trim(), None)?;
        }
        self.commit()?;
        Ok(())
    }

    pub fn record_worker(&mut self, worker: &TaskWorker, actor: &Actor, now: i64) -> Result<()> {
        self.writable()?;
        let view = self.view()?;
        if view.stage != Stage::Active
            || actor != &Actor::Agent(worker.parent.clone())
            || view.session.as_ref() != Some(&worker.parent)
        {
            return Err(Error::new(
                "only the task's current coordinator can bind its subagent",
            ));
        }
        // Any attempt whose delivery is unknown holds worker binding too,
        // whichever claim the projection selected.
        self.not_held()?;
        let sent = view.log.iter().rev().find(|entry| entry.kind == "sent");
        let attempt = self.current_attempt()?;
        if sent.and_then(|entry| entry.request_id.as_deref()) != Some(worker.request_id.as_str())
            || attempt
                .as_ref()
                .is_some_and(|current| current.request_id != worker.request_id)
        {
            return Err(Error::new(
                "task dispatch changed; read the current request_id",
            ));
        }
        if worker.id.is_empty() || worker.conversation_id.is_empty() || worker.provider.is_empty() {
            return Err(Error::new(
                "a worker needs its native child and parent conversation identities",
            ));
        }
        if view.worker.as_ref() == Some(worker) {
            return Ok(());
        }
        self.upgrade_schema(DISCUSSION_TASK_SCHEMA)?;
        self.fields()
            .insert("worker", serde_json::to_string(worker)?)?;
        self.log(
            "worker",
            actor,
            now,
            &format!("Delegated to {} ({})", worker.name, worker.id),
            Some(&worker.parent),
        )?;
        self.commit()?;
        Ok(())
    }

    pub fn comment(&mut self, text: &str, actor: &Actor, now: i64) -> Result<()> {
        self.comment_with_attachments(text, &[], actor, now)
    }

    pub fn comment_with_attachments(
        &mut self,
        text: &str,
        files: &[TaskAttachment],
        actor: &Actor,
        now: i64,
    ) -> Result<()> {
        self.writable()?;
        check_attachments(files)?;
        if !files.is_empty() {
            self.check_media_budget(&self.attachments()?, files)?;
        }
        check_text(text, "comment")?;
        if self.content_len() + text.len() > MAX_DOCUMENT {
            return Err(Error::new(
                "this task's history is full; it accepts no more comments",
            ));
        }
        let entry = self.log("comment", actor, now, text, None)?;
        if !files.is_empty() {
            self.upgrade_schema(DISCUSSION_TASK_SCHEMA)?;
            entry.insert("attachments", serde_json::to_string(files)?)?;
        }
        self.commit()?;
        Ok(())
    }

    /// Reserve before transport. Existing identities are read/recovered, never sent again.
    pub fn reserve_message(
        &mut self,
        id: &str,
        text: &str,
        files: &[TaskAttachment],
        delivery: &crate::TaskDelivery,
        session: Option<&SessionKey>,
        (actor, now): (&Actor, i64),
    ) -> Result<bool> {
        self.writable()?;
        if uuid::Uuid::parse_str(id)
            .map(|id| id.to_string())
            .ok()
            .as_deref()
            != Some(id)
        {
            return Err(Error::new("message request_id must be a canonical UUID"));
        }
        let view = self.view()?;
        if let Some(old) = view
            .log
            .iter()
            .find(|e| e.kind == "message" && e.request_id.as_deref() == Some(id))
        {
            if old.text != text
                || old.attachments != files
                || old.actor != *actor
                || old.delivery.as_ref().map(|d| (&d.mode, &d.dispatch_id))
                    != Some((&delivery.mode, &delivery.dispatch_id))
            {
                return Err(Error::new(
                    "message request_id already identifies different content or execution",
                ));
            }
            return Ok(false);
        }
        match delivery.mode.as_str() {
            "queue"
                if matches!(view.stage, Stage::Todo | Stage::Backlog)
                    && delivery.state == "queued" => {}
            "note" if delivery.state == "note" => {}
            // A refused delivery still belongs in the task discussion. It
            // carries no native submission authority and cannot reopen work.
            "queue" | "note" | "send" | "reopen"
                if matches!(delivery.state.as_str(), "failed" | "uncertain")
                    && !delivery.error.is_empty()
                    && delivery.run_id.is_empty()
                    && delivery.conversation_id.is_empty()
                    && delivery.prompt.is_empty()
                    && delivery.submitted_id.is_empty() => {}
            "send" | "reopen" if delivery.state == "reserved" => {
                // A reopen begins an attempt (S2b.3), and nothing more goes to
                // an attempt whose delivery is unknown.
                if delivery.mode == "reopen" && self.execution_managed() {
                    return Err(Error::new(
                        "this task records execution attempts; reopen it through its next attempt",
                    ));
                }
                self.not_held()?;
                let expected_stage = if delivery.mode == "send" {
                    view.stage == Stage::Active
                } else {
                    matches!(view.stage, Stage::Review | Stage::Done)
                };
                if !expected_stage
                    || delivery.stage != view.stage.as_str()
                    || delivery.stage_at != view.stage_at
                    || session.is_none()
                    || session != view.session.as_ref()
                    || session.is_some_and(|s| s.session_id.is_empty())
                    || delivery.run_id.is_empty()
                    || delivery.dispatch_id.is_empty()
                    || view.execution_id().as_deref() != Some(delivery.dispatch_id.as_str())
                {
                    return Err(Error::new("task execution changed; refresh before sending"));
                }
            }
            _ => return Err(Error::new("message mode does not match the task state")),
        }
        if !text.is_empty() || files.is_empty() {
            check_text(text, "message")?;
        }
        check_attachments(files)?;
        self.check_media_budget(&view.attachments, files)?;
        if self.content_len() + text.len() + serde_json::to_string(delivery)?.len() + 8192
            > MAX_DOCUMENT
        {
            return Err(Error::new("task discussion is full"));
        }
        self.upgrade_schema(MESSAGE_TASK_SCHEMA)?;
        let entry = self.log("message", actor, now, text, session)?;
        entry.insert("request_id", id)?;
        entry.insert("delivery", serde_json::to_string(delivery)?)?;
        if !files.is_empty() {
            entry.insert("attachments", serde_json::to_string(files)?)?;
        }
        self.commit()?;
        Ok(true)
    }

    pub fn finish_message(
        &mut self,
        id: &str,
        state: &str,
        error: &str,
        actor: &Actor,
        now: i64,
    ) -> Result<()> {
        self.writable()?;
        if !matches!(state, "submitted" | "failed" | "uncertain") {
            return Err(Error::new("invalid message receipt"));
        }
        let view = self.view()?;
        let message = view
            .log
            .iter()
            .find(|e| e.kind == "message" && e.request_id.as_deref() == Some(id))
            .ok_or_else(|| Error::new("message reservation is missing"))?;
        let mut delivery = message
            .delivery
            .clone()
            .ok_or_else(|| Error::new("message has no reservation"))?;
        if state == "submitted" && delivery.run_id.is_empty() {
            return Err(Error::new("message has no native execution reservation"));
        }
        if delivery.state == "submitted" || delivery.state == state && delivery.error == error {
            return Ok(());
        }
        if !matches!(delivery.state.as_str(), "reserved" | "uncertain") {
            return Err(Error::new("message is not awaiting a receipt"));
        }
        delivery.state = state.into();
        delivery.error = error.chars().take(1000).collect();
        if state == "submitted" {
            delivery.submitted_id = id.into();
        }
        delivery.prompt.clear();
        let entry = self.log("message_delivery", actor, now, "", message.session.as_ref())?;
        entry.insert("request_id", id)?;
        entry.insert("delivery", serde_json::to_string(&delivery)?)?;
        if state == "submitted"
            && delivery.mode == "reopen"
            && matches!(view.stage, Stage::Review | Stage::Done)
            && delivery.stage == view.stage.as_str()
            && delivery.stage_at == view.stage_at
            && view.session == message.session
            && view.execution_id().as_deref() == Some(delivery.dispatch_id.as_str())
        {
            self.fields().insert("stage", Stage::Active.as_str())?;
            self.fields().insert("stage_at", now)?;
            self.fields().delete("cancelled_at")?;
            self.fields().insert("cancel_reason", "")?;
            self.log(
                "resumed",
                actor,
                now,
                "Changes requested and submitted",
                message.session.as_ref(),
            )?;
        }
        self.commit()?;
        Ok(())
    }

    pub fn record_queued_messages(
        &mut self,
        dispatch: &str,
        actor: &Actor,
        now: i64,
    ) -> Result<()> {
        self.writable()?;
        self.queued_submitted(dispatch, actor, now)?;
        self.commit()?;
        Ok(())
    }

    /// A line Zerus adds to the history, such as a refused cancel message.
    pub fn note(&mut self, text: &str, actor: &Actor, now: i64) -> Result<()> {
        self.writable()?;
        self.log("note", actor, now, text, None)?;
        self.commit()?;
        Ok(())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl TaskDoc {
    /// Writes the stored schema as an older replica's merged upgrade leaves
    /// it, below what the task's execution needs (tests of other crates).
    #[doc(hidden)]
    pub fn lower_stored_schema_for_test(&mut self, value: i64) {
        self.meta().insert("schema", value).unwrap();
        self.doc.commit();
    }
}

#[cfg(test)]
impl TaskDoc {
    /// Records an attempt and its events as a writer of the execution schema
    /// does; this build reads them and does not write them yet.
    /// Writes the stored schema as an older replica's upgrade does.
    pub(crate) fn set_stored_schema_for_test(&mut self, value: i64) {
        self.meta().insert("schema", value).unwrap();
        self.doc.commit();
    }

    /// Stores a floor value as written, readable or not.
    pub(crate) fn set_floor_value_for_test(&mut self, value: impl Into<LoroValue>) {
        self.meta().insert("floor", value).unwrap();
        self.doc.commit();
    }

    /// Imports as an importer without the schema repair does.
    pub(crate) fn import_unrepaired_for_test(&mut self, bytes: &[u8]) {
        self.doc.import(bytes).unwrap();
    }

    pub(crate) fn record_attempt_for_test(&mut self, attempt: &Attempt, events: &[AttemptEvent]) {
        self.upgrade_schema(execution::EXECUTION_TASK_SCHEMA)
            .unwrap();
        self.doc
            .get_map("attempts")
            .insert(&attempt.id, serde_json::to_string(attempt).unwrap())
            .unwrap();
        let list = self.doc.get_list("attempt_events");
        for event in events {
            list.push(serde_json::to_string(event).unwrap()).unwrap();
        }
        self.doc.commit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// review-n on the C3 design: a copy that never saw the floor, written
    /// with a later clock, wins the merged schema; `schema()` still reports
    /// the execution schema for the cut-over task, but the RAW stored value
    /// an older build reads is lower, so the fence refuses it until a repair.
    #[test]
    fn the_fence_reads_the_raw_stored_schema_not_the_raised_one() {
        let mut task = TaskDoc::create(crate::NewTask {
            project: "p",
            id: "T-000001",
            text: "Add rate limiting",
            stage: crate::Stage::Todo,
            target: crate::Target::Any,
            author: Actor::Person,
            rank: "V",
            now: 1,
        })
        .unwrap();
        let before = task.snapshot().unwrap();
        assert_eq!(task.fence_ready(), Some("no_floor"));
        assert!(task.floor().unwrap());
        assert_eq!(task.fence_ready(), None);
        // A copy from before the floor, edited further, then named: its
        // schema write comes later than the floor's.
        let mut stale = TaskDoc::from_bytes(&before).unwrap();
        for n in 0..20 {
            stale
                .comment(&format!("note {n}"), &Actor::Person, 2 + n)
                .unwrap();
        }
        stale
            .set_title("Rate limiting", &Actor::Person, 30)
            .unwrap();
        let mut merged = TaskDoc::from_bytes(&task.snapshot().unwrap()).unwrap();
        let old = stale.export_since_vv(&merged.current_vv()).unwrap();
        assert!(
            !merged.import_block(&old).unwrap(),
            "a live import must refuse the pre-floor branch"
        );
        // Decode a literal older importer's result to retain the raw-schema
        // reader control; this does not qualify it for a current store save.
        merged.import_unrepaired_for_test(&old);
        assert_eq!(
            merged.schema(),
            execution::EXECUTION_TASK_SCHEMA,
            "a cut-over task reports the execution schema"
        );
        assert!(
            merged.stored_schema() < execution::EXECUTION_TASK_SCHEMA,
            "the lower write won the merge"
        );
        assert_eq!(merged.fence_ready(), Some("schema"));
        assert!(merged.repair_schema().unwrap());
        assert_eq!(merged.fence_ready(), None);
    }

    /// Versions compare as the changes they name: canonical peers with
    /// positive counts, zero entries dropped, anything else refused.
    #[test]
    fn versions_compare_in_their_one_canonical_form() {
        let version = |pairs: &[(&str, i32)]| -> crate::Version {
            pairs
                .iter()
                .map(|(peer, count)| (peer.to_string(), *count))
                .collect()
        };
        assert_eq!(
            crate::canonical_version(&version(&[("12", 3), ("7", 0)])),
            Some(version(&[("12", 3)]))
        );
        for invalid in [
            &[("012", 3)][..],
            &[("+12", 3)],
            &[("peer", 3)],
            &[("12", -1)],
            &[("18446744073709551616", 1)],
        ] {
            assert_eq!(
                crate::canonical_version(&version(invalid)),
                None,
                "{invalid:?}"
            );
        }
        assert!(crate::version_includes(
            &version(&[("1", 4), ("2", 1)]),
            &version(&[("1", 3)])
        ));
        assert!(!crate::version_includes(
            &version(&[("1", 2)]),
            &version(&[("1", 3)])
        ));
    }

    #[test]
    fn an_identified_session_keeps_ownership_across_renames() {
        let one = SessionKey {
            machine: "m1".into(),
            session: "codex/old".into(),
            session_id: "launch_id:one".into(),
        };
        let renamed = SessionKey {
            session: "codex/new".into(),
            ..one.clone()
        };
        assert_eq!(one, renamed);
        assert_ne!(
            one,
            SessionKey {
                session_id: "launch_id:replacement".into(),
                ..one.clone()
            }
        );
        assert_ne!(
            one,
            SessionKey {
                machine: "m2".into(),
                ..one.clone()
            }
        );
        assert_ne!(
            one,
            SessionKey {
                session_id: String::new(),
                ..one.clone()
            }
        );
    }

    #[test]
    fn handoff_records_the_new_owner_and_preserves_both_receipts() {
        let mut task = task(Stage::Todo);
        let old = api();
        let new = SessionKey {
            session: "codex/worker".into(),
            ..old.clone()
        };
        task.record_send(&old, "original-request", &Actor::Person, 2)
            .unwrap();
        let before = task.view().unwrap();
        assert!(task
            .record_handoff(&new, "foreign-request", &Actor::Agent(new.clone()), 3)
            .is_err());
        assert_eq!(task.view().unwrap(), before);
        task.record_handoff(&new, "handoff-request", &Actor::Agent(old), 4)
            .unwrap();
        let view = task.view().unwrap();
        assert_eq!(view.stage, Stage::Active);
        assert_eq!(view.session, Some(new.clone()));
        assert_eq!(view.target, Target::Session(new));
        assert_eq!(
            view.log
                .iter()
                .filter_map(|entry| entry.request_id.as_deref())
                .collect::<Vec<_>>(),
            ["original-request", "handoff-request"]
        );
    }

    fn agent(session: &str) -> Actor {
        Actor::Agent(SessionKey {
            machine: "m1".into(),
            session: session.into(),
            ..Default::default()
        })
    }

    fn task(stage: Stage) -> TaskDoc {
        TaskDoc::create(NewTask {
            project: "p1",
            id: "T-a1b2c3",
            text: "Add rate limiting",
            stage,
            target: Target::Any,
            author: Actor::Person,
            rank: "V",
            now: 1,
        })
        .unwrap()
    }

    fn api() -> SessionKey {
        SessionKey {
            machine: "m1".into(),
            session: "codex/api".into(),
            ..Default::default()
        }
    }

    /// zerus-63rru.36 rule 1: leaving Review or Done for To do clears the
    /// cancelled mark and its reason in the same commit; every other move
    /// keeps them.
    #[test]
    fn moving_a_cancelled_task_to_a_stage_without_work_clears_the_mark() {
        // Review -> To do, after a real cancel.
        let mut cancelled = task(Stage::Todo);
        cancelled
            .record_send(&api(), "request-1", &Actor::Person, 2)
            .unwrap();
        cancelled
            .record_cancel("Superseded", &Actor::Person, 3)
            .unwrap();
        assert!(cancelled.view().unwrap().cancelled_at.is_some());
        cancelled.set_stage(Stage::Todo, &Actor::Person, 4).unwrap();
        let view = cancelled.view().unwrap();
        assert_eq!(view.cancelled_at, None);
        assert_eq!(view.cancel_reason, "");

        // Done -> To do: the mark survives Done (a report does not clear it),
        // and the departure from Done clears it.
        let mut done = task(Stage::Todo);
        done.record_send(&api(), "request-1", &Actor::Person, 2)
            .unwrap();
        done.record_cancel("Wrong approach", &Actor::Person, 3)
            .unwrap();
        done.set_stage(Stage::Done, &Actor::Person, 4).unwrap();
        assert!(done.view().unwrap().cancelled_at.is_some());
        done.set_stage(Stage::Todo, &Actor::Person, 5).unwrap();
        assert_eq!(done.view().unwrap().cancelled_at, None);
        assert_eq!(done.view().unwrap().cancel_reason, "");

        // Review -> Done keeps the mark: work has not restarted.
        let mut kept = task(Stage::Todo);
        kept.record_send(&api(), "request-1", &Actor::Person, 2)
            .unwrap();
        kept.record_cancel("Superseded", &Actor::Person, 3).unwrap();
        kept.set_stage(Stage::Done, &Actor::Person, 4).unwrap();
        assert!(kept.view().unwrap().cancelled_at.is_some());
    }

    /// zerus-63rru.36: a cancel and a move-back merged in either order leave
    /// one of the four safe combinations; the two concurrent move-backs both
    /// land on To do with the mark cleared.
    #[test]
    fn a_cancel_and_a_move_back_merge_into_a_safe_state_in_either_order() {
        // Two copies at In progress: A cancels; B cancels and then moves the
        // task back. The syncs happen in either order; every merged state is
        // one of the safe combinations (the stage To do or Review, the mark
        // set or cleared), and a set mark always refuses the intake.
        let mut base = task(Stage::Todo);
        base.record_send(&api(), "request-1", &Actor::Person, 2)
            .unwrap();
        let base = base.snapshot().unwrap();
        for order in 0..2 {
            let mut cancelling = TaskDoc::from_bytes(&base).unwrap();
            let mut reviving = TaskDoc::from_bytes(&base).unwrap();
            cancelling
                .record_cancel("Superseded", &Actor::Person, 3)
                .unwrap();
            reviving
                .record_cancel("Superseded", &Actor::Person, 3)
                .unwrap();
            reviving.set_stage(Stage::Todo, &Actor::Person, 4).unwrap();
            let (mut first, second) = if order == 0 {
                (reviving, cancelling)
            } else {
                (cancelling, reviving)
            };
            assert!(first
                .import_block(&second.export_since_vv(&first.current_vv()).unwrap())
                .unwrap());
            let view = first.view().unwrap();
            assert!(
                matches!(view.stage, Stage::Todo | Stage::Review),
                "{view:?}"
            );
            match (view.stage, view.cancelled_at.is_some()) {
                (Stage::Todo, false) | (Stage::Todo, true) | (Stage::Review, true) => {}
                (Stage::Review, false) => {
                    // The move-back's clear beat the cancel's mark; the task
                    // is In review without a mark: the intake refuses by
                    // stage, so this is still conservative.
                }
                other => panic!("unsafe merged state: {other:?}"),
            }
        }

        // Two concurrent move-backs land on To do, the mark cleared.
        let mut shared = TaskDoc::from_bytes(&base).unwrap();
        shared
            .record_cancel("Superseded", &Actor::Person, 3)
            .unwrap();
        let marked = shared.snapshot().unwrap();
        let mut one = TaskDoc::from_bytes(&marked).unwrap();
        let mut two = TaskDoc::from_bytes(&marked).unwrap();
        one.set_stage(Stage::Todo, &Actor::Person, 4).unwrap();
        two.set_stage(Stage::Todo, &Actor::Person, 5).unwrap();
        assert!(one
            .import_block(&two.export_since_vv(&one.current_vv()).unwrap())
            .unwrap());
        let view = one.view().unwrap();
        assert_eq!(view.stage, Stage::Todo);
        assert_eq!(view.cancelled_at, None);
        assert_eq!(view.cancel_reason, "");
    }

    /// zerus-63rru.36: an older build's document — To do with the mark the
    /// older set_stage left — is not cleared by this build's set_stage (it
    /// clears only on the departure from Review or Done); the send, start
    /// and reopen paths still clear it. Stated in the report as the
    /// remaining older-document gap.
    #[test]
    fn an_older_builds_stale_mark_on_a_todo_task_is_not_cleared_by_set_stage() {
        let mut doc = task(Stage::Todo);
        // The old writer's leftover, reconstructed with the fields API: the
        // new set_stage cannot produce it.
        doc.fields().insert("cancelled_at", 2).unwrap();
        doc.fields().insert("cancel_reason", "Superseded").unwrap();
        // No move from To do departs Review or Done: the mark survives.
        doc.set_stage(Stage::Backlog, &Actor::Person, 3).unwrap();
        doc.set_stage(Stage::Todo, &Actor::Person, 4).unwrap();
        let view = doc.view().unwrap();
        assert!(view.cancelled_at.is_some(), "{view:?}");
        // The paths that still clear it: a send.
        doc.record_send(&api(), "request-1", &Actor::Person, 5)
            .unwrap();
        assert_eq!(doc.view().unwrap().cancelled_at, None);
    }

    /// zerus-63rru.36 round 2: the team-binding write clears the cancelled
    /// mark and its reason in the same commit, whatever the stage; the
    /// other fields of the document are untouched.
    #[test]
    #[cfg(not(feature = "test-without-team"))]
    fn the_team_binding_write_clears_the_mark() {
        let mut doc = task(Stage::Todo);
        doc.fields().insert("cancelled_at", 2).unwrap();
        doc.fields().insert("cancel_reason", "Superseded").unwrap();
        let before = doc.view().unwrap();
        doc.set_team_named(
            Some(&crate::TaskTeam {
                team: "00000000-0000-4000-8000-00000000beef".into(),
                home: "00000000-0000-4000-8000-00000000beef".into(),
            }),
            "Example team",
            &Actor::Person,
            3,
        )
        .unwrap();
        let view = doc.view().unwrap();
        assert_eq!(view.cancelled_at, None, "{view:?}");
        assert_eq!(view.cancel_reason, "", "{view:?}");
        assert!(view.team.is_some());
        // Nothing else new: the stage, the title and the author stay.
        assert_eq!(view.stage, before.stage);
        assert_eq!(view.title, before.title);
        assert_eq!(view.author, before.author);
    }

    /// zerus-63rru.75: each of the nine bidirectional characters is refused
    /// in a title as a control character is, through the one title check.
    #[test]
    fn each_bidirectional_character_is_refused_in_a_title() {
        for ch in [
            '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}',
            '\u{2068}', '\u{2069}',
        ] {
            let mut task = task(Stage::Todo);
            let error = task
                .set_title(&format!("Title{ch}name"), &Actor::Person, 2)
                .unwrap_err();
            assert!(
                error.to_string().contains("no control or bidirectional"),
                "{ch:?}: {error:?}"
            );
        }
    }

    /// zerus-63rru.75 round 2: the plain card's first line refuses only the
    /// line separators and the nine bidirectional characters; a tab (a
    /// pasted table or code) stays accepted there, while the explicit title
    /// keeps the full rule.
    #[test]
    fn a_plain_cards_first_line_refuses_only_the_separators_and_bidirectionals() {
        let plain = TaskDoc::create(NewTask {
            project: "p1",
            id: "T-tabbed",
            text: "Column\twith\ta\ttab",
            stage: Stage::Todo,
            target: Target::Any,
            author: Actor::Person,
            rank: "V",
            now: 1,
        })
        .unwrap();
        assert_eq!(plain.title(), "");
        let error = TaskDoc::create(NewTask {
            project: "p1",
            id: "T-202e01",
            text: "First\u{202E}line",
            stage: Stage::Todo,
            target: Target::Any,
            author: Actor::Person,
            rank: "V",
            now: 1,
        })
        .err()
        .unwrap();
        assert!(
            error.to_string().contains("no control or bidirectional"),
            "{error:?}"
        );
        // The explicit title keeps the full rule: the tab is refused there.
        let error = TaskDoc::create_named(
            NewTask {
                project: "p1",
                id: "T-ttab01",
                text: "",
                stage: Stage::Todo,
                target: Target::Any,
                author: Actor::Person,
                rank: "V",
                now: 1,
            },
            "Explicit\u{0009}title",
            None,
        )
        .err()
        .unwrap();
        assert!(
            error.to_string().contains("no control or bidirectional"),
            "{error:?}"
        );
    }

    /// zerus-63rru.75: a title with non-ASCII letters is accepted; a title
    /// that already holds a forbidden character still loads and moves.
    #[test]
    fn a_non_ascii_title_is_accepted_and_an_old_one_still_moves() {
        let mut named = task(Stage::Todo);
        named
            .set_title("Заголовок Ångström 東京", &Actor::Person, 2)
            .unwrap();
        assert_eq!(named.title(), "Заголовок Ångström 東京");
        // The value an older build stored: it still loads and moves.
        let mut old = task(Stage::Todo);
        old.fields().insert("title", "Old\u{202D}title").unwrap();
        old.doc.commit();
        assert_eq!(old.view().unwrap().title, "Old\u{202D}title");
        old.set_stage(Stage::Backlog, &Actor::Person, 2).unwrap();
        assert_eq!(old.view().unwrap().title, "Old\u{202D}title");
    }

    #[test]
    fn message_intent_survives_dispatch_receipts_and_changed_stages() {
        let mut task = task(Stage::Todo);
        let id = uuid::Uuid::new_v4().to_string();
        let delivery = crate::TaskDelivery {
            mode: "queue".into(),
            state: "queued".into(),
            stage: "todo".into(),
            stage_at: 1,
            dispatch_id: String::new(),
            submitted_id: String::new(),
            run_id: String::new(),
            conversation_id: String::new(),
            prompt: String::new(),
            error: String::new(),
        };
        assert!(task
            .reserve_message(
                &id,
                "Clarification",
                &[],
                &delivery,
                None,
                (&Actor::Person, 2)
            )
            .unwrap());
        let (before, before_ids) = task.view_with_history_ids().unwrap();
        let revision = task.revision();
        assert_eq!(
            TaskDoc::from_bytes(&task.snapshot().unwrap())
                .unwrap()
                .revision(),
            revision
        );
        let message_index = before.log.iter().position(|e| e.kind == "message").unwrap();
        let message_id = before_ids[message_index].clone();
        task.record_send(&api(), "dispatch", &Actor::Person, 3)
            .unwrap();
        task.record_queued_messages("dispatch", &Actor::Person, 4)
            .unwrap();
        assert!(!task
            .reserve_message(
                &id,
                "Clarification",
                &[],
                &delivery,
                None,
                (&Actor::Person, 5)
            )
            .unwrap());
        assert!(task
            .reserve_message(&id, "Changed", &[], &delivery, None, (&Actor::Person, 6))
            .is_err());
        let view = task.view().unwrap();
        assert_ne!(
            task.revision(),
            revision,
            "receipt-only edits change the revision"
        );
        let message = view.log.iter().find(|e| e.kind == "message").unwrap();
        assert_eq!(message.delivery.as_ref().unwrap().state, "submitted");
        assert_eq!(message.delivery.as_ref().unwrap().submitted_id, "dispatch");
        let (after, after_ids) = task.view_with_history_ids().unwrap();
        let message_index = after.log.iter().position(|e| e.kind == "message").unwrap();
        assert_eq!(
            after_ids[message_index], message_id,
            "receipt refinement keeps the history cursor"
        );
        assert_eq!(
            serde_json::to_value(&after).unwrap(),
            serde_json::to_value(&view).unwrap()
        );
        assert_eq!(after_ids.len(), after.log.len());
        assert_eq!(
            after_ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            after_ids.len()
        );
    }

    #[test]
    fn unconfirmed_feedback_cannot_reopen_or_change_owner() {
        let mut task = task(Stage::Todo);
        let owner = SessionKey {
            session_id: "launch_id:one".into(),
            ..api()
        };
        task.record_send(&owner, "dispatch", &Actor::Person, 2)
            .unwrap();
        task.record_report("Review", &Actor::Agent(owner.clone()), 3)
            .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let delivery = crate::TaskDelivery {
            mode: "reopen".into(),
            state: "reserved".into(),
            stage: "review".into(),
            stage_at: 3,
            dispatch_id: "dispatch".into(),
            submitted_id: String::new(),
            run_id: "run-one".into(),
            conversation_id: "conversation-one".into(),
            prompt: "[Task feedback]".into(),
            error: String::new(),
        };
        task.reserve_message(
            &id,
            "Revise",
            &[],
            &delivery,
            Some(&owner),
            (&Actor::Person, 4),
        )
        .unwrap();
        task.finish_message(&id, "uncertain", "Check receipt", &Actor::Person, 50)
            .unwrap();
        assert_eq!(task.stage().unwrap(), Stage::Review);
        let snapshot = task.snapshot().unwrap();
        let mut accepted = TaskDoc::from_bytes(&snapshot).unwrap();
        accepted.set_stage(Stage::Done, &Actor::Person, 6).unwrap();
        accepted
            .finish_message(&id, "submitted", "", &Actor::Person, 7)
            .unwrap();
        assert_eq!(
            accepted.stage().unwrap(),
            Stage::Done,
            "a late receipt cannot undo acceptance"
        );
        task.finish_message(&id, "submitted", "", &Actor::Person, 6)
            .unwrap();
        assert_eq!(task.stage().unwrap(), Stage::Active);
        assert_eq!(task.session(), Some(owner));
        assert_eq!(
            task.view()
                .unwrap()
                .log
                .iter()
                .find(|e| e.kind == "message")
                .unwrap()
                .delivery
                .as_ref()
                .unwrap()
                .state,
            "submitted",
            "clock changes cannot downgrade a native receipt"
        );
        assert_eq!(
            task.view()
                .unwrap()
                .log
                .iter()
                .find(|e| e.kind == "message")
                .unwrap()
                .delivery
                .as_ref()
                .unwrap()
                .prompt,
            "[Task feedback]"
        );
    }

    #[test]
    fn saved_refused_and_receiptless_comments_never_authorize_native_delivery() {
        let mut task = task(Stage::Todo);
        let owner = SessionKey {
            session_id: "launch_id:one".into(),
            ..api()
        };
        task.record_send(&owner, "dispatch", &Actor::Person, 2)
            .unwrap();
        task.record_report("Review", &Actor::Agent(owner.clone()), 3)
            .unwrap();
        for state in ["failed", "uncertain"] {
            let id = uuid::Uuid::new_v4().to_string();
            let delivery = crate::TaskDelivery {
                mode: "reopen".into(),
                state: state.into(),
                stage: "review".into(),
                stage_at: 3,
                dispatch_id: "dispatch".into(),
                submitted_id: String::new(),
                run_id: String::new(),
                conversation_id: String::new(),
                prompt: String::new(),
                error: "Executor could not be verified".into(),
            };
            assert!(task
                .reserve_message(
                    &id,
                    "Keep this comment",
                    &[],
                    &delivery,
                    Some(&owner),
                    (&Actor::Person, 4)
                )
                .unwrap());
            assert!(!task
                .reserve_message(
                    &id,
                    "Keep this comment",
                    &[],
                    &delivery,
                    Some(&owner),
                    (&Actor::Person, 5)
                )
                .unwrap());
            assert!(task
                .finish_message(&id, "submitted", "", &Actor::Person, 6)
                .is_err());
            assert_eq!(task.stage().unwrap(), Stage::Review);
            assert_eq!(task.session(), Some(owner.clone()));
        }
    }

    #[test]
    fn a_new_task_has_its_fields_and_a_history() {
        let view = task(Stage::Todo).view().unwrap();
        assert_eq!(
            (view.id.as_str(), view.stage, view.text.as_str()),
            ("T-a1b2c3", Stage::Todo, "Add rate limiting")
        );
        assert_eq!(
            (
                view.author,
                view.target,
                view.rank.as_str(),
                view.schema,
                view.writable
            ),
            (Actor::Person, Target::Any, "V", SCHEMA, true)
        );
        assert_eq!(view.log.len(), 1);
        assert_eq!(view.log[0].kind, "created");
        assert!(TaskDoc::create(NewTask {
            project: "p1",
            id: "T-a1b2c3",
            text: "x",
            stage: Stage::Active,
            target: Target::Any,
            author: Actor::Person,
            rank: "V",
            now: 1
        })
        .is_err());
        assert!(TaskDoc::create(NewTask {
            project: "p1",
            id: "T-a1b2c3",
            text: " ",
            stage: Stage::Todo,
            target: Target::Any,
            author: Actor::Person,
            rank: "V",
            now: 1
        })
        .is_err());
    }

    #[test]
    fn the_lifecycle_follows_the_task_spec() {
        let mut t = task(Stage::Backlog);
        t.set_stage(Stage::Todo, &Actor::Person, 2).unwrap();
        assert!(t.set_stage(Stage::Done, &Actor::Person, 3).is_err());
        // A stage the task is already in is named as such.
        assert_eq!(
            t.set_stage(Stage::Todo, &Actor::Person, 3)
                .unwrap_err()
                .to_string(),
            "T-a1b2c3 is already in To do"
        );
        t.record_send(&api(), "r1", &Actor::Person, 4).unwrap();
        assert_eq!(t.stage().unwrap(), Stage::Active);
        assert_eq!(
            t.record_send(&api(), "r2", &Actor::Person, 5)
                .unwrap_err()
                .to_string(),
            "T-a1b2c3 is already In progress in codex/api"
        );
        assert!(t.edit_text("other", &Actor::Person, 5).is_err());
        assert!(t
            .record_report("Done; 6 tests.", &agent("codex/api"), 6)
            .unwrap());
        let view = t.view().unwrap();
        assert_eq!(
            (view.stage, view.summary.as_str()),
            (Stage::Review, "Done; 6 tests.")
        );
        // A further report is history only.
        assert!(!t
            .record_report("Also updated the README.", &agent("codex/api"), 7)
            .unwrap());
        assert_eq!(t.view().unwrap().summary, "Done; 6 tests.");
        t.set_stage(Stage::Done, &Actor::Person, 8).unwrap();
        t.set_stage(Stage::Todo, &Actor::Person, 9).unwrap();
        assert_eq!(t.view().unwrap().session, Some(api()));
    }

    #[test]
    fn cancel_moves_to_review_with_the_reason() {
        let mut t = task(Stage::Todo);
        assert!(t.record_cancel("", &Actor::Person, 2).is_err());
        t.record_send(&api(), "r1", &Actor::Person, 3).unwrap();
        t.record_cancel(" Wrong approach ", &Actor::Person, 4)
            .unwrap();
        let view = t.view().unwrap();
        assert_eq!(
            (view.stage, view.cancelled_at, view.cancel_reason.as_str()),
            (Stage::Review, Some(4), "Wrong approach")
        );
        let kinds: Vec<_> = view.log.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, ["created", "sent", "cancelled", "comment"]);
        assert_eq!(view.log[3].text, "Wrong approach");
        // A late report after the cancel stays in the history.
        assert!(!t
            .record_report("Stopped; changed one file.", &agent("codex/api"), 5)
            .unwrap());
        t.set_stage(Stage::Todo, &Actor::Person, 6).unwrap();
        t.record_send(&api(), "r2", &Actor::Person, 7).unwrap();
        assert_eq!(t.view().unwrap().cancelled_at, None);
    }

    #[test]
    fn executor_plans_only_store_intent_and_follow_identity() {
        let parent = SessionKey {
            session_id: "launch_id:parent".into(),
            ..api()
        };
        for target in [
            Target::NewSession {
                parent: parent.clone(),
            },
            Target::Subagent {
                parent: parent.clone(),
            },
        ] {
            let mut task = task(Stage::Todo);
            task.set_target(&target, &Actor::Person, 2).unwrap();
            let before = task.view().unwrap();
            assert_eq!(before.schema, DISCUSSION_TASK_SCHEMA);
            assert_eq!(before.stage, Stage::Todo);
            assert!(before.session.is_none() && before.worker.is_none());
            assert_eq!(before.log.last().unwrap().kind, "target");
            assert!(task.record_start(&Actor::Agent(parent.clone()), 3).is_err());
            let renamed = SessionKey {
                session: "codex/renamed".into(),
                ..parent.clone()
            };
            task.bind_session_identity(&parent, &renamed).unwrap();
            let current = match task.view().unwrap().target {
                Target::NewSession { parent } | Target::Subagent { parent } => parent,
                _ => panic!("lost executor plan"),
            };
            assert_eq!(current.session, renamed.session);
            assert_eq!(task.schema(), DISCUSSION_TASK_SCHEMA);
        }
    }

    #[test]
    fn current_dispatch_start_is_acknowledged_once_without_resending() {
        let mut task = task(Stage::Todo);
        task.record_send(&api(), "dispatch", &Actor::Person, 2)
            .unwrap();
        assert!(task.record_start(&agent("codex/other"), 3).is_err());
        task.record_start(&agent("codex/api"), 4).unwrap();
        task.record_start(&agent("codex/api"), 5).unwrap();
        let view = task.view().unwrap();
        assert_eq!(view.stage_at, 2);
        assert_eq!(
            view.log
                .iter()
                .filter(|entry| entry.kind == "started")
                .count(),
            1
        );
        assert_eq!(
            view.log.iter().filter(|entry| entry.kind == "sent").count(),
            1
        );
    }

    #[test]
    fn worker_binding_is_scoped_to_the_coordinator_and_dispatch() {
        let mut task = task(Stage::Todo);
        let parent = SessionKey {
            session_id: "launch_id:parent".into(),
            ..api()
        };
        task.set_target(
            &Target::Subagent {
                parent: parent.clone(),
            },
            &Actor::Person,
            2,
        )
        .unwrap();
        task.record_send(&parent, "dispatch", &Actor::Person, 3)
            .unwrap();
        let actor = Actor::Agent(parent.clone());
        assert!(task.record_start(&actor, 4).is_err());
        let worker = TaskWorker {
            parent: parent.clone(),
            conversation_id: "conversation".into(),
            provider: "codex".into(),
            id: "child".into(),
            name: "Builder".into(),
            request_id: "dispatch".into(),
        };
        assert!(task.record_worker(&worker, &Actor::Person, 5).is_err());
        assert!(task
            .record_worker(
                &TaskWorker {
                    request_id: "old".into(),
                    ..worker.clone()
                },
                &actor,
                5
            )
            .is_err());
        task.record_worker(&worker, &actor, 5).unwrap();
        task.record_worker(&worker, &actor, 6).unwrap();
        task.record_start(&actor, 7).unwrap();
        assert_eq!(task.view().unwrap().worker.as_ref(), Some(&worker));
        assert_eq!(
            task.view()
                .unwrap()
                .log
                .iter()
                .filter(|entry| entry.kind == "worker")
                .count(),
            1
        );
        let other = SessionKey {
            session: "codex/other".into(),
            session_id: "launch_id:other".into(),
            ..api()
        };
        task.record_handoff(&other, "next", &actor, 8).unwrap();
        assert!(task.view().unwrap().worker.is_none());
        assert!(task.record_worker(&worker, &actor, 9).is_err());
    }

    #[test]
    fn comment_media_and_description_share_one_storage_budget() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let mut task = task(Stage::Todo);
        let file = TaskAttachment {
            name: "image.bin".into(),
            mime: "application/octet-stream".into(),
            data_base64: STANDARD.encode(vec![0; crate::attachment::MAX_ATTACHMENT]),
            reference: String::new(),
        };
        task.set_attachments(std::slice::from_ref(&file), &Actor::Person, 2)
            .unwrap();
        task.comment_with_attachments(
            "First image",
            std::slice::from_ref(&file),
            &Actor::Person,
            3,
        )
        .unwrap();
        let before = task.view().unwrap().log;
        assert!(task
            .comment_with_attachments("Too much", std::slice::from_ref(&file), &Actor::Person, 4)
            .is_err());
        assert_eq!(task.view().unwrap().log, before);
        assert!(task
            .set_attachments(&[file.clone(), file], &Actor::Person, 5)
            .is_err());
        assert_eq!(task.attachments().unwrap().len(), 1);
    }

    #[test]
    fn comment_media_survives_replication_and_schema_never_downgrades() {
        let mut task = task(Stage::Todo);
        let file = TaskAttachment {
            name: "image.png".into(),
            mime: "image/png".into(),
            data_base64: "cG5n".into(),
            reference: "[Image #1]".into(),
        };
        task.comment_with_attachments(
            "Inspect [Image #1]",
            std::slice::from_ref(&file),
            &Actor::Person,
            2,
        )
        .unwrap();
        task.set_target(
            &Target::Session(SessionKey {
                session_id: "launch_id:parent".into(),
                ..api()
            }),
            &Actor::Person,
            3,
        )
        .unwrap();
        task.set_attachments(std::slice::from_ref(&file), &Actor::Person, 4)
            .unwrap();
        assert_eq!(task.schema(), DISCUSSION_TASK_SCHEMA);
        let log = task.view().unwrap().log;
        assert_eq!(log[1].attachments, [file]);
        let bytes = task.snapshot().unwrap();
        let restored = TaskDoc::from_bytes(&bytes).unwrap();
        assert_eq!(restored.view().unwrap().log, log);
    }

    #[test]
    fn edits_targets_ranks_and_comments() {
        let mut t = task(Stage::Todo);
        t.edit_text("Add strict rate limiting", &Actor::Person, 2)
            .unwrap();
        t.set_target(&Target::Session(api()), &Actor::Person, 3)
            .unwrap();
        t.set_rank("W").unwrap();
        t.comment("Use Redis", &agent("codex/web"), 4).unwrap();
        assert!(t
            .comment(&"x".repeat(64 * 1024 + 1), &Actor::Person, 5)
            .is_err());
        assert!(t.set_rank("W0").is_err());
        let view = t.view().unwrap();
        assert_eq!(
            (view.text.as_str(), view.target, view.rank.as_str()),
            ("Add strict rate limiting", Target::Session(api()), "W")
        );
        assert_eq!(
            view.log.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
            ["created", "edited", "target", "comment"]
        );
    }

    #[test]
    fn a_large_rewrite_is_replaced_quickly() {
        let mut t = task(Stage::Todo);
        let long = "x".repeat(60_000);
        let started = std::time::Instant::now();
        t.edit_text(&long, &Actor::Person, 2).unwrap();
        t.edit_text("Short again", &Actor::Person, 3).unwrap();
        assert_eq!(t.view().unwrap().text, "Short again");
        assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
    }

    #[test]
    fn terminal_control_characters_are_refused() {
        let mut t = task(Stage::Todo);
        for text in ["\x1b[31mred", "line one\r\n", "bell\x07"] {
            let error = t.edit_text(text, &Actor::Person, 2).unwrap_err();
            assert!(error.to_string().contains("control characters"), "{error}");
            assert!(t.comment(text, &Actor::Person, 2).is_err());
        }
        t.edit_text("tabs\tand\nnewlines", &Actor::Person, 3)
            .unwrap();
    }

    #[test]
    fn concurrent_edits_merge_the_same_way_everywhere() {
        let mut a = task(Stage::Todo);
        a.record_send(&api(), "r1", &Actor::Person, 1).unwrap();
        a.record_report("Done.", &agent("codex/api"), 1).unwrap();
        let mut b = TaskDoc::from_bytes(&a.snapshot().unwrap()).unwrap();
        let (va, vb) = (a.version(), b.version());
        a.set_stage(Stage::Done, &Actor::Person, 2).unwrap();
        b.set_stage(Stage::Todo, &agent("codex/api"), 2).unwrap();
        b.edit_text("Add strict rate limiting", &Actor::Person, 3)
            .unwrap();
        a.comment("Looks good", &Actor::Person, 3).unwrap();
        let from_a = a.export_since(&vb).unwrap();
        let from_b = b.export_since(&va).unwrap();
        a.import(&from_b).unwrap();
        b.import(&from_a).unwrap();
        let (va, vb) = (a.view().unwrap(), b.view().unwrap());
        assert_eq!(va, vb);
        // Both stage changes stay in the history, whichever won.
        assert_eq!(va.log.iter().filter(|e| e.kind == "stage").count(), 2);
        assert_eq!(va.text, "Add strict rate limiting");
    }

    #[test]
    fn a_newer_schema_is_read_only() {
        let t = task(Stage::Todo);
        t.meta().insert("schema", TASK_SCHEMA + 1).unwrap();
        t.doc.commit();
        let mut t = TaskDoc::from_bytes(&t.snapshot().unwrap()).unwrap();
        assert!(!t.view().unwrap().writable);
        assert_eq!(
            t.comment("x", &Actor::Person, 2).unwrap_err().to_string(),
            "Update Zerus to edit this task"
        );
    }

    #[test]
    fn a_cancel_reason_follows_the_comment_rules() {
        let mut t = task(Stage::Todo);
        t.record_send(&api(), "r", &Actor::Person, 2).unwrap();
        let error = t
            .record_cancel("stop\x1b[2J", &Actor::Person, 3)
            .unwrap_err();
        assert!(error.to_string().contains("control characters"), "{error}");
        assert!(t
            .record_cancel(&"x".repeat(70_000), &Actor::Person, 3)
            .is_err());
        assert_eq!(t.stage().unwrap(), Stage::Active);
        t.record_cancel("", &Actor::Person, 4).unwrap();
    }

    #[test]
    fn a_send_keeps_its_request_apart_from_the_text() {
        let mut t = task(Stage::Todo);
        t.record_send(&api(), "req-1", &Actor::Person, 2).unwrap();
        let log = t.view().unwrap().log;
        let sent = log.iter().find(|e| e.kind == "sent").unwrap();
        assert_eq!(
            (sent.text.as_str(), sent.request_id.as_deref()),
            ("", Some("req-1"))
        );
    }

    #[test]
    fn a_full_history_accepts_no_more_comments() {
        let mut t = task(Stage::Todo);
        let long = "x".repeat(crate::schema::MAX_TEXT - 1);
        let mut accepted = 0;
        let error = loop {
            match t.comment(&long, &Actor::Person, 2) {
                Ok(()) => accepted += 1,
                Err(error) => break error,
            }
            assert!(accepted < 100, "the history never filled");
        };
        assert!((60..=64).contains(&accepted), "{accepted}");
        assert!(error.to_string().contains("history is full"), "{error}");
    }

    #[test]
    fn a_send_is_checked_before_anything_is_pasted() {
        let mut t = task(Stage::Todo);
        t.check_send().unwrap();
        t.record_send(&api(), "r", &Actor::Person, 2).unwrap();
        let running = t.check_send().unwrap_err().to_string();
        assert!(running.contains("already In progress"), "{running}");
        let newer = task(Stage::Todo);
        newer.meta().insert("schema", TASK_SCHEMA + 1).unwrap();
        newer.doc.commit();
        assert_eq!(
            newer.check_send().unwrap_err().to_string(),
            "Update Zerus to edit this task"
        );
    }

    #[test]
    fn the_time_a_task_entered_its_stage_is_kept() {
        let mut t = task(Stage::Todo);
        assert_eq!(t.view().unwrap().stage_at, 1);
        t.comment("no stage change", &Actor::Person, 3).unwrap();
        assert_eq!(t.view().unwrap().stage_at, 1);
        t.record_send(&api(), "r", &Actor::Person, 5).unwrap();
        assert_eq!(t.view().unwrap().stage_at, 5);
        t.record_report("Done.", &agent("codex/api"), 9).unwrap();
        assert_eq!(t.view().unwrap().stage_at, 9);
        t.set_stage(Stage::Done, &Actor::Person, 12).unwrap();
        assert_eq!(t.view().unwrap().stage_at, 12);
        // Documents written before the field existed fall back to creation.
        t.fields().delete("stage_at").unwrap();
        t.doc.commit();
        assert_eq!(t.view().unwrap().stage_at, 1);
    }

    #[test]
    fn a_newer_task_shows_read_only_with_fields_it_cannot_read() {
        let t = task(Stage::Todo);
        t.meta().insert("schema", TASK_SCHEMA + 1).unwrap();
        t.meta().insert("author", "{\"kind\":\"robot\"}").unwrap();
        t.fields().insert("target", "{\"kind\":\"team\"}").unwrap();
        t.doc.commit();
        let t = TaskDoc::from_bytes(&t.snapshot().unwrap()).unwrap();
        let view = t.view().unwrap();
        assert!(!view.writable);
        assert_eq!((view.author, view.target), (Actor::Person, Target::Any));
    }

    mod report_guard {
        use super::*;
        use crate::execution::{ReportKind, Stale};

        fn web() -> SessionKey {
            SessionKey {
                session: "codex/web".into(),
                ..api()
            }
        }

        fn last(task: &TaskDoc) -> LogEntry {
            task.view().unwrap().log.pop().unwrap()
        }

        #[test]
        fn after_a_handoff_only_the_new_executor_reports_a_result() {
            let mut task = task(Stage::Todo);
            task.record_send(&api(), "first", &Actor::Person, 2)
                .unwrap();
            task.record_handoff(&web(), "second", &Actor::Agent(api()), 3)
                .unwrap();
            // The previous executor's late result is history, not a move.
            let late = task
                .report(
                    "Finished earlier",
                    ReportKind::Ready,
                    None,
                    &Actor::Agent(api()),
                    4,
                )
                .unwrap();
            assert_eq!(
                (late.moves, late.stale),
                (false, Some(Stale::OtherExecutor))
            );
            assert_eq!(task.stage().unwrap(), Stage::Active);
            let entry = last(&task);
            assert_eq!(
                (entry.kind.as_str(), entry.stale),
                ("report", Some(Stale::OtherExecutor))
            );
            assert!(!task
                .record_report("Same again", &Actor::Agent(api()), 5)
                .unwrap());
            // An unrelated session is stale as well.
            let other = Actor::Agent(SessionKey {
                session: "codex/ops".into(),
                ..api()
            });
            assert!(!task.record_report("Not mine", &other, 6).unwrap());
            assert_eq!(task.stage().unwrap(), Stage::Active);
            // The current executor's result moves it, with its summary.
            assert!(task
                .record_report("Done; 3 tests", &Actor::Agent(web()), 7)
                .unwrap());
            let view = task.view().unwrap();
            assert_eq!(
                (view.stage, view.summary.as_str()),
                (Stage::Review, "Done; 3 tests")
            );
            assert_eq!(last(&task).stale, None);
        }

        #[test]
        fn progress_is_recorded_and_never_moves_the_task() {
            let mut task = task(Stage::Todo);
            task.record_send(&api(), "first", &Actor::Person, 2)
                .unwrap();
            let progress = task
                .report(
                    "Half way",
                    ReportKind::Progress,
                    Some(1),
                    &Actor::Agent(api()),
                    3,
                )
                .unwrap();
            assert_eq!((progress.moves, progress.stale), (false, None));
            let view = task.view().unwrap();
            assert_eq!((view.stage, view.summary.as_str()), (Stage::Active, ""));
            let entry = last(&task);
            assert_eq!(
                (entry.kind.as_str(), entry.text.as_str(), entry.considered),
                ("progress", "Half way", Some(1))
            );
            // A person's progress is history too; a person's result is an explicit move.
            assert!(
                !task
                    .report("Note", ReportKind::Progress, None, &Actor::Person, 4)
                    .unwrap()
                    .moves
            );
            assert!(
                task.report("Accepting it", ReportKind::Ready, None, &Actor::Person, 5)
                    .unwrap()
                    .moves
            );
            assert_eq!(task.stage().unwrap(), Stage::Review);
            // Outside In progress a report only adds to the history, with no reason.
            let after = task
                .report("More", ReportKind::Ready, None, &Actor::Agent(api()), 6)
                .unwrap();
            assert_eq!((after.moves, after.stale), (false, None));
            assert!(task
                .report("Bad", ReportKind::Ready, Some(0), &Actor::Person, 7)
                .is_err());
        }

        #[test]
        fn the_pinned_agent_s_report_still_starts_and_finishes_its_own_task() {
            let mut task = task(Stage::Todo);
            task.set_target(&Target::Session(api()), &Actor::Person, 2)
                .unwrap();
            // Progress from the pinned agent starts the task and keeps it In progress.
            assert!(
                !task
                    .report(
                        "Started",
                        ReportKind::Progress,
                        None,
                        &Actor::Agent(api()),
                        3
                    )
                    .unwrap()
                    .moves
            );
            assert_eq!(task.stage().unwrap(), Stage::Active);
            assert!(task.record_report("Done", &Actor::Agent(api()), 4).unwrap());
            assert_eq!(task.stage().unwrap(), Stage::Review);
            // Another agent's report does not start a task pinned elsewhere.
            let mut pinned = super::task(Stage::Todo);
            pinned
                .set_target(&Target::Session(api()), &Actor::Person, 2)
                .unwrap();
            assert!(!pinned
                .record_report("Mine?", &Actor::Agent(web()), 3)
                .unwrap());
            assert_eq!(pinned.stage().unwrap(), Stage::Todo);
        }

        fn run(session: &str, id: &str) -> SessionKey {
            SessionKey {
                session: session.into(),
                session_id: format!("launch_id:{id}"),
                ..api()
            }
        }

        #[test]
        fn a_legacy_name_only_executor_bound_to_its_verified_run_reports_its_result() {
            // review-n on #1389: a send to a name-only executor, later bound to
            // its verified run (as every change does before it is made).
            let sent = || {
                let mut task = task(Stage::Todo);
                task.record_send(&api(), "first", &Actor::Person, 2)
                    .unwrap();
                task
            };
            let mut task = sent();
            task.bind_session_identity(&api(), &run("codex/api", "x"))
                .unwrap();
            let view = task.view().unwrap();
            let execution = view.execution.as_ref().unwrap();
            assert!(!execution.conflict);
            assert_eq!(
                execution.current.as_ref().unwrap().executor,
                Some(run("codex/api", "x"))
            );
            // Another run that holds the same name later is not that executor.
            let other = task
                .report(
                    "Not mine",
                    ReportKind::Ready,
                    None,
                    &Actor::Agent(run("codex/api", "y")),
                    3,
                )
                .unwrap();
            assert_eq!(
                (other.moves, other.stale),
                (false, Some(Stale::OtherExecutor))
            );
            assert!(task
                .record_report("Done", &Actor::Agent(run("codex/api", "x")), 4)
                .unwrap());
            assert_eq!(task.stage().unwrap(), Stage::Review);
            // A binding that followed a rename (a retained original label)
            // names the same executor, and a later rename keeps it.
            let mut renamed = sent();
            renamed
                .bind_session_identity(&api(), &run("codex/renamed", "x"))
                .unwrap();
            renamed
                .bind_session_identity(&run("codex/renamed", "x"), &run("codex/later", "x"))
                .unwrap();
            let view = renamed.view().unwrap();
            assert!(!view.execution.as_ref().unwrap().conflict);
            assert!(!renamed
                .record_report("Not mine", &Actor::Agent(run("codex/api", "y")), 3)
                .unwrap());
            assert!(renamed
                .record_report("Done", &Actor::Agent(run("codex/later", "x")), 4)
                .unwrap());
            // A holder an older build enriched without recording the binding is
            // not proven to be that run: a conflict only the person resolves.
            let mut older = sent();
            older
                .fields()
                .insert(
                    "session",
                    serde_json::to_string(&run("codex/api", "x")).unwrap(),
                )
                .unwrap();
            older.doc.commit();
            assert!(older.view().unwrap().execution.unwrap().conflict);
            let refused = older
                .report(
                    "Done",
                    ReportKind::Ready,
                    None,
                    &Actor::Agent(run("codex/api", "x")),
                    3,
                )
                .unwrap();
            assert_eq!(refused.stale, Some(Stale::Conflict));
            assert!(older.record_report("Accepted", &Actor::Person, 4).unwrap());
            // The next execution's own boundary replaces the binding.
            renamed.set_stage(Stage::Todo, &Actor::Person, 5).unwrap();
            renamed
                .record_send(&run("codex/later", "x"), "second", &Actor::Person, 6)
                .unwrap();
            assert_eq!(renamed.binding(), None);
        }

        #[test]
        fn a_stored_reason_this_build_does_not_know_reads_as_none() {
            let task = task(Stage::Todo);
            let entry = task
                .log("report", &Actor::Person, 2, "From a newer writer", None)
                .unwrap();
            entry.insert("stale", "a_future_reason").unwrap();
            task.doc.commit();
            assert_eq!(last(&task).stale, None);
        }
    }

    mod execution_records {
        use super::*;
        use crate::execution::{EventKind, Origin, Phase};

        const A1: &str = "11111111-1111-4111-8111-111111111111";
        const A2: &str = "22222222-2222-4222-8222-222222222222";

        fn worker() -> SessionKey {
            SessionKey {
                session_id: "launch_id:worker".into(),
                ..api()
            }
        }

        fn attempt(id: &str, generation: u64) -> Attempt {
            Attempt {
                id: id.into(),
                generation,
                origin: Origin::Send,
                executor: worker(),
                request_id: format!("request-{generation}"),
                requirement: 1,
                actor: Actor::Person,
                at: 50,
                delivery: None,
                frontier: None,
            }
        }

        fn began(n: u128, attempt: &str) -> AttemptEvent {
            AttemptEvent {
                id: uuid::Uuid::from_u128(n).to_string(),
                attempt: attempt.into(),
                kind: EventKind::Began,
                actor: Actor::Agent(worker()),
                at: 60,
                settles: Vec::new(),
                of: String::new(),
            }
        }

        /// The attempt's own submitted receipt.
        fn delivered(n: u128, attempt: &str) -> AttemptEvent {
            AttemptEvent {
                kind: EventKind::Submitted,
                at: 55,
                ..began(n, attempt)
            }
        }

        #[test]
        fn history_before_attempts_projects_an_assigned_execution() {
            let mut task = task(Stage::Todo);
            assert_eq!(task.view().unwrap().execution, None);
            assert_eq!(task.requirement(), 1);
            task.record_send(&worker(), "first-send", &Actor::Person, 10)
                .unwrap();
            let execution = task.view().unwrap().execution.unwrap();
            let current = execution.current.unwrap();
            assert!(current.legacy);
            assert_eq!(
                (current.phase, current.request_id.as_str(), current.executor),
                (Phase::Submitted, "first-send", Some(worker()))
            );
            task.record_start(&Actor::Agent(worker()), 20).unwrap();
            let current = task.view().unwrap().execution.unwrap().current.unwrap();
            assert_eq!(
                (current.phase, current.began_at),
                (Phase::Running, Some(20))
            );
            // Serialized only when there is one.
            let json = serde_json::to_value(task.view().unwrap()).unwrap();
            assert_eq!(json["execution"]["current"]["phase"], "running");
            assert!(
                serde_json::to_value(super::task(Stage::Todo).view().unwrap())
                    .unwrap()
                    .get("execution")
                    .is_none()
            );
        }

        #[test]
        fn a_direct_start_after_a_finished_send_is_its_own_execution() {
            // review-n's sequence, through public schema-5 operations only.
            let one = api();
            let two = SessionKey {
                session: "codex/web".into(),
                ..api()
            };
            let mut task = task(Stage::Todo);
            task.record_send(&one, "old-send-to-one", &Actor::Person, 10)
                .unwrap();
            assert!(task.record_report("Done", &Actor::Agent(one), 20).unwrap());
            task.set_stage(Stage::Todo, &Actor::Person, 30).unwrap();
            task.record_start(&Actor::Agent(two.clone()), 40).unwrap();
            let execution = task.view().unwrap().execution.unwrap();
            assert!(!execution.conflict);
            let current = execution.current.unwrap();
            assert_eq!(
                (
                    current.origin,
                    current.at,
                    current.request_id.as_str(),
                    current.began_at
                ),
                (Origin::Start, 40, "", Some(40))
            );
            assert_eq!(current.executor, Some(two));
        }

        #[test]
        fn a_task_with_attempts_is_read_and_written_under_their_rules() {
            let mut task = task(Stage::Todo);
            task.record_send(&worker(), "request-1", &Actor::Person, 10)
                .unwrap();
            let before = task.view().unwrap();
            task.record_attempt_for_test(&attempt(A1, 1), &[delivered(2, A1), began(1, A1)]);
            let view = task.view().unwrap();
            assert_eq!(view.schema, execution::EXECUTION_TASK_SCHEMA);
            assert!(view.writable);
            let current = view.execution.as_ref().unwrap().current.clone().unwrap();
            assert_eq!(
                (
                    current.id.as_str(),
                    current.generation,
                    current.phase,
                    current.legacy
                ),
                (A1, 1, Phase::Running, false)
            );
            // Attempts add no history and change no stage.
            assert_eq!(
                (view.stage, view.log.len()),
                (before.stage, before.log.len())
            );
            // The records survive a snapshot.
            let restored = TaskDoc::from_bytes(&task.snapshot().unwrap()).unwrap();
            assert_eq!(restored.view().unwrap().execution, view.execution);
            // The current executor's result finishes the attempt in its commit.
            assert!(
                task.report_attempt(
                    A1,
                    "Done",
                    ReportKind::Ready,
                    None,
                    &Actor::Agent(worker()),
                    70
                )
                .unwrap()
                .moves
            );
            let current = task.view().unwrap().execution.unwrap().current.unwrap();
            assert_eq!((current.id.as_str(), current.phase), (A1, Phase::Ready));
            // A newer schema is read-only: writes fail closed rather than
            // bypassing rules this build lacks.
            let mut newer = TaskDoc::from_bytes(&bytes_of(&restored)).unwrap();
            newer.meta().insert("schema", TASK_SCHEMA + 1).unwrap();
            newer.doc.commit();
            let refused = newer.record_report("Done", &Actor::Agent(worker()), 70);
            assert_eq!(
                refused.unwrap_err().to_string(),
                "Update Zerus to edit this task"
            );
            assert_eq!(newer.stage().unwrap(), Stage::Active);
        }

        fn bytes_of(task: &TaskDoc) -> Vec<u8> {
            task.snapshot().unwrap()
        }

        #[test]
        fn attempts_merged_from_two_replicas_project_the_same_conflict() {
            let mut base = task(Stage::Todo);
            base.record_send(&worker(), "request-1", &Actor::Person, 10)
                .unwrap();
            base.floor().unwrap();
            let bytes = base.snapshot().unwrap();
            let mut one = TaskDoc::from_bytes(&bytes).unwrap();
            let mut two = TaskDoc::from_bytes(&bytes).unwrap();
            one.record_attempt_for_test(&attempt(A1, 1), &[]);
            two.record_attempt_for_test(&attempt(A2, 1), &[began(1, A2)]);
            one.import(&two.snapshot().unwrap()).unwrap();
            two.import(&one.snapshot().unwrap()).unwrap();
            let (left, right) = (one.view().unwrap(), two.view().unwrap());
            assert_eq!(left.execution, right.execution);
            let execution = left.execution.unwrap();
            assert!(execution.conflict);
            assert_eq!(
                (execution.attempts, execution.current.unwrap().id.as_str()),
                (2, A1)
            );
            // The same event merged from both replicas counts once.
            let mut three = TaskDoc::from_bytes(&bytes).unwrap();
            three.record_attempt_for_test(&attempt(A1, 2), &[delivered(6, A1), began(7, A1)]);
            let mut four = TaskDoc::from_bytes(&three.snapshot().unwrap()).unwrap();
            four.record_attempt_for_test(&attempt(A1, 2), &[delivered(6, A1), began(7, A1)]);
            three.import(&four.snapshot().unwrap()).unwrap();
            let current = three.view().unwrap().execution.unwrap().current.unwrap();
            assert_eq!(
                (current.phase, current.began_at),
                (Phase::Running, Some(60))
            );
        }

        #[test]
        fn damaged_records_are_damage_in_this_schema_and_ignored_in_a_newer_one() {
            let task = task(Stage::Todo);
            task.doc
                .get_map("attempts")
                .insert(A1, "not an attempt")
                .unwrap();
            task.doc.commit();
            assert!(task.view().is_err());
            task.upgrade_schema(execution::EXECUTION_TASK_SCHEMA)
                .unwrap();
            task.doc.commit();
            assert!(task.view().is_err());
            task.upgrade_schema(TASK_SCHEMA + 1).unwrap();
            task.doc.commit();
            let view = task.view().unwrap();
            assert!(!view.writable && view.execution.is_none());
            // A record must sit under its own identity.
            let other = super::task(Stage::Todo);
            other
                .doc
                .get_map("attempts")
                .insert(A2, serde_json::to_string(&attempt(A1, 1)).unwrap())
                .unwrap();
            other.doc.commit();
            assert!(other.view().is_err());
            // Too many events is damage, not an unbounded read.
            let crowded = super::task(Stage::Todo);
            let list = crowded.doc.get_list("attempt_events");
            for n in 0..=execution::MAX_ATTEMPT_EVENTS as u128 {
                list.push(serde_json::to_string(&began(n + 1, A1)).unwrap())
                    .unwrap();
            }
            crowded.doc.commit();
            assert!(crowded.view().unwrap_err().to_string().contains("too many"));
        }
    }

    #[test]
    fn replacing_a_full_media_set_beyond_the_task_document_cap_is_refused() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let size = crate::attachment::MAX_ATTACHMENT;
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut media = |name: &str| {
            let mut bytes = Vec::with_capacity(size);
            while bytes.len() < size {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                bytes.extend_from_slice(&state.to_le_bytes());
            }
            bytes.truncate(size);
            TaskAttachment {
                name: name.into(),
                mime: "application/octet-stream".into(),
                data_base64: STANDARD.encode(bytes),
                reference: String::new(),
            }
        };
        let first = media("first.bin");
        let second = media("second.bin");
        let mut task = task(Stage::Todo);
        task.set_attachments(&[first.clone(), second.clone()], &Actor::Person, 2)
            .unwrap();
        let retained = task.snapshot().unwrap().len();
        assert!(
            retained <= crate::MAX_TASK_DOCUMENT,
            "the current document must fit the cap: {retained}"
        );
        let error = task
            .set_attachments(
                &[media("third.bin"), media("fourth.bin")],
                &Actor::Person,
                3,
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("attachment history is full"),
            "{error}"
        );
        assert_eq!(task.attachments().unwrap(), [first.clone(), second]);
        assert_eq!(task.snapshot().unwrap().len(), retained);
        // Removing attached files stays possible even when their retained
        // history no longer fits another full set.
        task.set_attachments(std::slice::from_ref(&first), &Actor::Person, 4)
            .unwrap();
        assert_eq!(task.attachments().unwrap(), [first]);
    }
}
