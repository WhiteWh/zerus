//! Replicating a project between replicas: what each holds (its inventory, the
//! version of every document), the updates the other lacks, and importing them.
//! The inventory is bookkeeping, not a log, so nothing here grows without bound.
use crate::schema::{SessionKey, Stage};
use crate::store::{write_atomic, Damaged, Keep, ProjectStore};
use crate::task::TaskDoc;
use crate::{ids, Error, Result};
use loro::VersionVector;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A document's version: each Loro peer that changed it (its ID as decimal
/// text) and how many changes it made. Sorted, so equal versions compare and
/// hash alike on every replica.
pub type Version = BTreeMap<String, i32>;

/// What a replica holds of one project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    /// The project document; empty while it was never saved.
    pub project: Version,
    /// Each task document by UUID.
    pub tasks: BTreeMap<String, Version>,
}

impl Inventory {
    /// Equal for replicas that hold the same versions of the same documents.
    pub fn digest(&self) -> String {
        let canonical = serde_json::to_vec(self).expect("an inventory serializes");
        format!("{:x}", Sha256::digest(canonical))
    }
}

/// The document an update belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Document {
    Project,
    Task(String),
}

/// Loro updates for one document: what the other replica lacks of it, or the
/// whole document when it has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub document: Document,
    pub bytes: Vec<u8>,
}

/// One Done task as the phone lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoneRow {
    pub uuid: String,
    pub id: String,
    /// The start of the prompt, at most 200 characters.
    pub text: String,
    pub session: Option<SessionKey>,
    /// When it entered Done (Unix milliseconds).
    pub accepted_at: i64,
}

/// A Done task as a page of the Done list is chosen from (`done_page_of`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoneEntry {
    /// When it entered Done (Unix milliseconds).
    pub at: i64,
    pub uuid: String,
    /// The session it was sent to.
    pub sent: Option<SessionKey>,
}

/// A page of the Done list; `next` is the cursor of the page after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DonePage {
    pub tasks: Vec<DoneRow>,
    pub next: Option<(i64, String)>,
    pub total: usize,
    pub recent: usize,
    /// The page's tasks that could not be read, left out of it.
    pub damaged: usize,
}

/// What an import changed, and the documents it refused.
#[derive(Debug, Default)]
pub struct Imported {
    pub changed: usize,
    pub refused: Vec<Damaged>,
    /// Healthy shallow copies that could not apply an update. Internal
    /// phone recovery bookkeeping, never a field in the exchange envelope.
    pub(crate) refresh: BTreeSet<String>,
}

const CACHE: &str = "inventory.json";
/// Beside the cache: the digest of the cache this build wrote. Only a build
/// that repairs the execution schema writes it, so rows an older binary
/// wrote or rewrote are never trusted and every task is read (and repaired)
/// again (zerus-cg5h.4 S2b.2).
const CACHE_MARK: &str = "inventory.repaired";

/// What the mark holds for the cache `bytes`.
fn cache_mark(bytes: &[u8]) -> String {
    // 3: retry any negative rows made by the former two-second proof policy.
    // A cut-over task's entry still follows its mark, which a load raises
    // before its version is acknowledged. The mark's format joins it: a
    // cache made under another rule for a readable mark (before the seal,
    // for one) is never trusted, so every task is read again.
    format!(
        "schema-repair-3 proof-budget-1 mark-{} {:x}\n",
        crate::store::MARK_FORMAT,
        Sha256::digest(bytes)
    )
}

/// What tells a file changed without reading it: its length, modification
/// time and, on Unix, its inode (an atomic rewrite gets a new one).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp(u64, u64, u32, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    stamp_of(fs::metadata(path).ok()?)
}

/// A mark's stamp as its entry has it: a dangling link changes it too.
fn mark_stamp(path: &Path) -> Option<Stamp> {
    stamp_of(fs::symlink_metadata(path).ok()?)
}

fn stamp_of(meta: fs::Metadata) -> Option<Stamp> {
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    #[cfg(unix)]
    let inode = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let inode = 0;
    Some(Stamp(
        meta.len(),
        modified.as_secs(),
        modified.subsec_nanos(),
        inode,
    ))
}

/// The versions read last time, by the stamps of the files they came from.
#[derive(Default, PartialEq, Serialize, Deserialize)]
struct Cache {
    project: Option<Cached>,
    tasks: BTreeMap<String, Cached>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Cached {
    files: Vec<Option<Stamp>>,
    #[serde(default)]
    version: Version,
    /// Why the task does not load; it is read again only when its files change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// The task's stage, for the phone's view; an entry without one is read again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stage: Option<Stage>,
    /// When it entered that stage, for the Done list; an entry without it is read again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stage_at: Option<i64>,
    /// The session it was last sent to, for the Done list's filter; an entry without it is read again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sent: Option<Sent>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Sent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<SessionKey>,
}

/// A rewrite within one timestamp tick can hand a file its old inode back, so
/// stamps are trusted only for files older than this.
const SETTLE: Duration = Duration::from_secs(2);

fn settled(files: &[Option<Stamp>]) -> bool {
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return false;
    };
    files
        .iter()
        .flatten()
        .all(|Stamp(_, secs, nanos, _)| Duration::new(*secs, *nanos) + SETTLE <= now)
}

impl TaskDoc {
    /// This task's version as an inventory holds it.
    pub fn inventory_version(&self) -> Version {
        version(&self.current_vv())
    }
}

impl ProjectStore {
    /// The project document's version as an inventory holds it; empty while
    /// it was never saved.
    pub fn project_version(&self) -> Result<Version> {
        if !self.dir().join("project.loro").exists() {
            return Ok(Version::new());
        }
        Ok(version(&VersionVector::decode(&self.project()?.version())?))
    }
}

pub(crate) fn version(vv: &VersionVector) -> Version {
    vv.iter()
        .map(|(peer, count)| (peer.to_string(), *count))
        .collect()
}

pub(crate) fn vector(version: &Version) -> Result<VersionVector> {
    version
        .iter()
        .map(|(peer, count)| {
            let peer = peer
                .parse::<u64>()
                .map_err(|_| Error::new(format!("invalid peer {peer:?} in a version")))?;
            Ok((peer, *count))
        })
        .collect()
}

/// A version received from another replica, in its one canonical form: each
/// peer a canonical decimal u64 with a positive count; an entry of 0 says
/// nothing and is dropped. None when any entry is not of that form, so two
/// versions compare as the changes they name, never as their encodings
/// (review-n on the C3 design: `TaskDoc::version` bytes are not canonical).
pub fn canonical_version(version: &Version) -> Option<Version> {
    version
        .iter()
        .filter(|(_, count)| **count != 0)
        .map(|(peer, count)| {
            let canonical = peer.parse::<u64>().ok()?.to_string() == *peer;
            (canonical && *count > 0).then(|| (peer.clone(), *count))
        })
        .collect()
}

/// Whether `theirs` holds every change of `ours` (both canonical).
pub fn version_includes(theirs: &Version, ours: &Version) -> bool {
    includes(theirs, ours)
}

/// Whether `theirs` holds every change of `ours`.
fn includes(theirs: &Version, ours: &Version) -> bool {
    ours.iter()
        .all(|(peer, count)| theirs.get(peer).is_some_and(|known| known >= count))
}

impl ProjectStore {
    /// The project IDs stored below `root`. A project is named by its
    /// folder's `project-id`, read without waiting or following a link and
    /// within its bound (zerus-iih7g), and only when that ID is valid and is
    /// the one the folder is named after. Any other folder's identity is
    /// unknown: it is not listed (never taken for another project) and
    /// nothing in it is changed.
    pub fn stored(root: &Path) -> Result<Vec<String>> {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(error.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let folder = entry?.path();
            let read = crate::store::read_identity(
                &folder.join("project-id"),
                crate::store::PROJECT_ID_BYTES,
            );
            let Ok(Some(bytes)) = read else { continue };
            let Ok(id) = String::from_utf8(bytes) else {
                continue;
            };
            let named = folder.file_name().and_then(|name| name.to_str())
                == Some(format!("{:x}", Sha256::digest(id.as_bytes())).as_str());
            if named && !id.is_empty() && !id.chars().any(char::is_control) {
                out.push(id);
            }
        }
        out.sort();
        Ok(out)
    }

    /// The version of every document of the project, and the tasks that do not
    /// load. A task is read again only when its files changed since last time.
    pub fn inventory(&self) -> Result<(Inventory, Vec<Damaged>)> {
        let (inventory, _, damaged) = self.inventory_with_stages()?;
        Ok((inventory, damaged))
    }

    /// What a phone holds of this project: the project document and every task
    /// that is not Done (docs/superpowers/specs/2026-10-18-task-replication-loro-design.md, "Phone").
    pub fn live_inventory(&self) -> Result<(Inventory, Vec<Damaged>)> {
        let (mut inventory, stages, damaged) = self.inventory_with_stages()?;
        inventory
            .tasks
            .retain(|uuid, _| stages.get(uuid).map(|s| s.0) != Some(Stage::Done));
        Ok((inventory, damaged))
    }

    /// What a phone holding `theirs` is answered from: the live documents and
    /// every task it already holds, so a task that became Done still reaches it.
    pub fn phone_inventory(&self, theirs: &Inventory) -> Result<Inventory> {
        let (mut inventory, stages, _) = self.inventory_with_stages()?;
        inventory.tasks.retain(|uuid, _| {
            stages.get(uuid).map(|s| s.0) != Some(Stage::Done) || theirs.tasks.contains_key(uuid)
        });
        Ok(inventory)
    }

    /// The Done tasks newest first (when they entered Done, then UUID, both
    /// descending): at most `limit` after `before`, with how many there are and
    /// how many entered Done at or after `since`; with `session`, only the
    /// tasks sent to it. Only the page's documents are read; one that cannot be
    /// read is left out of its page.
    pub fn done_page(
        &self,
        before: Option<(i64, String)>,
        limit: usize,
        since: i64,
        session: Option<&SessionKey>,
    ) -> Result<DonePage> {
        let (_, stages, _) = self.inventory_with_stages()?;
        let done = stages
            .into_iter()
            .filter(|(_, (stage, _, _))| *stage == Stage::Done)
            .map(|(uuid, (_, at, sent))| DoneEntry { at, uuid, sent })
            .collect();
        self.done_page_of(done, before, limit, since, session)
    }

    /// `done_page` over the Done tasks a caller already holds (hgs's task
    /// index, zerus-cg5h.23): each one's UUID, when it entered Done and the
    /// session it was sent to, which must be what the documents say. Only
    /// the page's documents are read.
    pub fn done_page_of(
        &self,
        done: Vec<DoneEntry>,
        before: Option<(i64, String)>,
        limit: usize,
        since: i64,
        session: Option<&SessionKey>,
    ) -> Result<DonePage> {
        let mut done: Vec<(i64, String)> = done
            .into_iter()
            .filter(|entry| {
                session.is_none_or(|session| {
                    entry.sent.as_ref().is_some_and(|sent| {
                        if session.session_id.is_empty() {
                            sent.machine == session.machine && sent.session == session.session
                        } else {
                            sent == session
                        }
                    })
                })
            })
            .map(|entry| (entry.at, entry.uuid))
            .collect();
        done.sort_unstable_by(|a, b| b.cmp(a));
        let total = done.len();
        let recent = done.iter().filter(|(at, _)| *at >= since).count();
        let start = before.map_or(0, |cursor| done.partition_point(|entry| *entry >= cursor));
        let end = (start + limit).min(total);
        let mut tasks = Vec::new();
        let mut damaged = 0;
        for (at, uuid) in &done[start..end] {
            let view = match self.load(uuid).and_then(|task| task.view()) {
                Ok(view) => view,
                Err(error) if error.kind() == crate::ErrorKind::Busy => return Err(error),
                Err(_) => {
                    damaged += 1;
                    continue;
                }
            };
            tasks.push(DoneRow {
                uuid: uuid.clone(),
                id: view.id,
                text: view.text.chars().take(200).collect(),
                session: view.session,
                accepted_at: *at,
            });
        }
        Ok(DonePage {
            tasks,
            next: (start < end && end < total).then(|| done[end - 1].clone()),
            total,
            recent,
            damaged,
        })
    }

    /// The inventory with each loaded task's stage and when it entered it.
    #[allow(clippy::type_complexity)]
    fn inventory_with_stages(
        &self,
    ) -> Result<(
        Inventory,
        BTreeMap<String, (Stage, i64, Option<SessionKey>)>,
        Vec<Damaged>,
    )> {
        let path = self.dir().join(CACHE);
        let mark = self.dir().join(CACHE_MARK);
        let old: Cache = crate::store::read_regular(&path)
            .ok()
            .filter(|bytes| {
                crate::store::read_regular(&mark)
                    .ok()
                    .and_then(|mark| String::from_utf8(mark).ok())
                    == Some(cache_mark(bytes))
            })
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let mut cache = Cache::default();
        let mut inventory = Inventory::default();
        let mut stages = BTreeMap::new();
        let mut damaged = Vec::new();
        let project = vec![stamp(&self.dir().join("project.loro"))];
        if project[0].is_some() {
            let trusted = settled(&project);
            let cached = match old
                .project
                .as_ref()
                .filter(|c| trusted && c.files == project)
            {
                Some(cached) => cached.clone(),
                None => Cached {
                    files: project,
                    version: version(&VersionVector::decode(&self.project()?.version())?),
                    error: None,
                    stage: None,
                    stage_at: None,
                    sent: None,
                },
            };
            inventory.project = cached.version.clone();
            if trusted {
                cache.project = Some(cached);
            }
        }
        for uuid in self.uuids()? {
            let files = self.stamps(&uuid);
            let trusted = settled(&files);
            let remembered = old.tasks.get(&uuid).filter(|c| {
                trusted
                    && c.files == files
                    && (c.error.is_some()
                        || (c.stage.is_some() && c.stage_at.is_some() && c.sent.is_some()))
            });
            let cached = match remembered {
                Some(cached) => cached.clone(),
                // The version and its stamps are one observation: no save of
                // this store comes between them (a load may raise the mark,
                // a repair may save; the stamps follow both).
                None => match self.observe(&uuid, || self.stamps(&uuid)) {
                    (Ok(task), files) => Cached {
                        files,
                        version: version(&task.current_vv()),
                        error: None,
                        stage: Some(task.stage()?),
                        stage_at: Some(task.stage_at()),
                        sent: Some(Sent {
                            session: task.session(),
                        }),
                    },
                    (Err(error), _) if error.kind() == crate::ErrorKind::Busy => return Err(error),
                    (Err(error), files) => Cached {
                        files,
                        version: Version::new(),
                        error: Some(error.to_string()),
                        stage: None,
                        stage_at: None,
                        sent: None,
                    },
                },
            };
            match &cached.error {
                Some(error) => damaged.push(self.damaged(uuid.as_str(), error.clone())),
                None => {
                    inventory.tasks.insert(uuid.clone(), cached.version.clone());
                    if let (Some(stage), Some(at)) = (cached.stage, cached.stage_at) {
                        let sent = cached.sent.as_ref().and_then(|sent| sent.session.clone());
                        stages.insert(uuid.clone(), (stage, at, sent));
                    }
                }
            }
            if trusted {
                cache.tasks.insert(uuid, cached);
            }
        }
        if cache != old {
            // Only a speed-up: an unwritten cache means reading the tasks again.
            // The mark follows the cache; until it does, the cache is not trusted.
            let bytes = serde_json::to_vec(&cache)?;
            let _ = fs::remove_file(&mark);
            if write_atomic(&path, &bytes).is_ok() {
                let _ = write_atomic(&mark, cache_mark(&bytes).as_bytes());
            }
        }
        Ok((inventory, stages, damaged))
    }

    /// The updates a replica holding `theirs` lacks of `ours` (this store's
    /// inventory), in pages of at most `limit` bytes; a document larger than
    /// that comes alone. True when more remain after this page.
    pub fn updates_for(
        &self,
        ours: &Inventory,
        theirs: &Inventory,
        limit: usize,
    ) -> Result<(Vec<Update>, bool)> {
        self.updates_for_mode(ours, theirs, limit, false)
    }

    pub(crate) fn updates_for_phone(
        &self,
        ours: &Inventory,
        theirs: &Inventory,
        limit: usize,
    ) -> Result<(Vec<Update>, bool)> {
        self.updates_for_mode(ours, theirs, limit, true)
    }

    fn updates_for_mode(
        &self,
        ours: &Inventory,
        theirs: &Inventory,
        limit: usize,
        phone: bool,
    ) -> Result<(Vec<Update>, bool)> {
        let mut page = Vec::new();
        let mut size = 0;
        let mut add = |update: Update| {
            if !page.is_empty() && size + update.bytes.len() > limit {
                return false;
            }
            size += update.bytes.len();
            page.push(update);
            true
        };
        if !ours.project.is_empty() && !includes(&theirs.project, &ours.project) {
            let bytes = self
                .project()?
                .export_since(&vector(&theirs.project)?.encode())?;
            if !add(Update {
                document: Document::Project,
                bytes,
            }) {
                return Ok((page, true));
            }
        }
        for (uuid, mine) in &ours.tasks {
            let known = theirs.tasks.get(uuid);
            if known.is_some_and(|known| includes(known, mine)) {
                continue;
            }
            let task = self.load(uuid)?;
            let bytes = match known {
                Some(known) if phone && known.is_empty() && task.stage()? == Stage::Done => {
                    task.shallow_snapshot()?
                }
                Some(known) => task.export_since_vv(&vector(known)?)?,
                None => task.snapshot()?,
            };
            if !add(Update {
                document: Document::Task(uuid.clone()),
                bytes,
            }) {
                return Ok((page, true));
            }
        }
        Ok((page, false))
    }

    /// A task's files as the inventory cache compares them: its snapshot,
    /// its updates and its mark.
    fn stamps(&self, uuid: &str) -> Vec<Option<Stamp>> {
        vec![
            stamp(&self.snapshot_path(uuid)),
            stamp(&self.updates_path(uuid)),
            mark_stamp(&self.managed_path(uuid)),
        ]
    }

    /// Imports another replica's updates. A document that cannot take them is
    /// refused and named; it never stops the others.
    pub fn import_updates(&self, updates: Vec<Update>) -> Result<Imported> {
        self.import_phone_updates(updates, &BTreeSet::new())
    }

    /// Ordinary phone ingress cannot write a native sealed task. This source
    /// comes from the receiver entry point, never from serialized fields.
    pub(crate) fn import_mobile_updates(&self, updates: Vec<Update>) -> Result<Imported> {
        self.import_with_origin(updates, &BTreeSet::new(), true)
    }

    /// A fresh phone fetch may replace a healthy shallow copy only when
    /// the incoming whole state covers every locally saved operation.
    pub(crate) fn import_phone_updates(
        &self,
        updates: Vec<Update>,
        refresh: &BTreeSet<String>,
    ) -> Result<Imported> {
        self.import_with_origin(updates, refresh, false)
    }

    fn import_with_origin(
        &self,
        updates: Vec<Update>,
        refresh: &BTreeSet<String>,
        mobile: bool,
    ) -> Result<Imported> {
        let mut imported = Imported::default();
        for update in updates {
            let (name, result) = match &update.document {
                Document::Project => (
                    "project".to_owned(),
                    self.import_project(&update.bytes).map_err(Refusal::from),
                ),
                Document::Task(uuid) => (
                    uuid.clone(),
                    if refresh.contains(uuid) {
                        self.refresh_phone_task(uuid, &update.bytes)
                            .map_err(Refusal::from)
                    } else {
                        self.import_task(uuid, &update.bytes, mobile)
                    },
                ),
            };
            match result {
                Ok(changed) => imported.changed += usize::from(changed),
                Err(refusal) => {
                    if refusal.error.kind() == crate::ErrorKind::Busy {
                        return Err(refusal.error);
                    }
                    if refusal.refresh {
                        imported.refresh.insert(name.clone());
                    }
                    let refused = Damaged::new(name, refusal.error.to_string());
                    imported.refused.push(match refusal.kept {
                        Some(reason) => refused.kept_for(reason),
                        None => refused,
                    });
                }
            }
        }
        Ok(imported)
    }

    fn import_project(&self, bytes: &[u8]) -> Result<bool> {
        let mut doc = self.project()?;
        let before = VersionVector::decode(&doc.version())?;
        doc.import(bytes)?;
        if VersionVector::decode(&doc.version())? == before {
            return Ok(false);
        }
        self.save_project(&doc)?;
        Ok(true)
    }

    fn import_task(
        &self,
        uuid: &str,
        bytes: &[u8],
        mobile: bool,
    ) -> std::result::Result<bool, Refusal> {
        if !ids::is_uuid(uuid) {
            return Err(Error::new(format!("{uuid:?} is not a task UUID")).into());
        }
        let stored = self.snapshot_path(uuid).exists() || self.updates_path(uuid).exists();
        let (mut task, replacing) = if stored {
            match self.load(uuid) {
                Ok(mut task) => {
                    let before = task.current_vv();
                    if let Err(error) = task.apply_checked(bytes) {
                        if error.kind() == crate::ErrorKind::Busy {
                            return Err(error.into());
                        }
                        let shallow = task.is_shallow();
                        return Err(Refusal {
                            error: if shallow {
                                Error::new(format!("the updates could not be applied to the shallow history; \
                                 the local copy is kept and needs a fresh copy covering \
                                 all its changes: {error}"))
                            } else {
                                error
                            },
                            kept: None,
                            refresh: shallow,
                        });
                    }
                    // Every live import restores the execution schema a merged
                    // lower value replaced, before anything reads or exports it.
                    task.repair_schema()?;
                    if task.current_vv() == before {
                        return Ok(false);
                    }
                    (task, false)
                }
                // A copy here that does not load gives way to a whole document
                // from another replica only when local proof shows nothing of
                // the cutover to execution attempts can be lost (`recovered`);
                // its files are set aside, not removed. Otherwise it stays
                // damaged and closed for a person's review.
                Err(error) if error.kind() == crate::ErrorKind::Busy => return Err(error.into()),
                Err(damaged) => match self.recovered(uuid, bytes) {
                    Recovery::Whole(task) | Recovery::Replaced(task) => (task, true),
                    Recovery::Unreadable => return Err(damaged.into()),
                    Recovery::Busy(error) => return Err(error.into()),
                    Recovery::Kept(reason) => {
                        return Err(Refusal {
                            error: Error::new(format!(
                                "the copy here does not load ({damaged}) and may hold execution \
                                 attempts no other copy is shown to hold; it is kept for a \
                                 person's review and nothing is sent for it"
                            )),
                            kept: Some(reason),
                            refresh: false,
                        });
                    }
                },
            }
        } else if self.was_managed(uuid) {
            // Its files are gone, but the mark says this store cut it over:
            // nothing here can show that another replica's copy holds all of
            // it. Removing the mark is the person's decision to accept one.
            return Err(Refusal {
                error: Error::new(
                    "the copy here is missing and it recorded execution attempts; it is kept for \
                     a person's review and nothing is sent for it",
                ),
                kept: Some(Keep::Marked),
                refresh: false,
            });
        } else {
            (TaskDoc::from_bytes(bytes)?, false)
        };
        if self.phone && task.schema() >= crate::task::cutover_schema() {
            task.projection_only = true;
        }
        task.repair_schema()?;
        if task.uuid() != uuid {
            return Err(Error::new("the update is for another task").into());
        }
        let view = task.view()?;
        if view.project != self.project_id() {
            return Err(Error::new("the task belongs to another project").into());
        }
        // Preflight before moving even a damaged canonical copy aside. A
        // refused fresh seal or frozen update must preserve every local file.
        self.check_cutover_save(&mut task, true)?;
        if mobile {
            self.check_mobile_received(&task)?;
        }
        if replacing {
            self.set_aside(uuid)?;
        }
        if mobile {
            self.save_mobile_received(&mut task)?;
        } else {
            self.save_received(&mut task)?;
        }
        Ok(true)
    }

    fn refresh_phone_task(&self, uuid: &str, bytes: &[u8]) -> Result<bool> {
        // One admission covers the latest local version, the inclusion
        // proof and the save. A writer through the same store cannot add
        // an unsent edit between the check and the replacement.
        let mut admitted = self
            .admit_until(Instant::now() + Duration::from_secs(2))
            .ok_or_else(|| Error::failed("the task store is busy; the local copy is kept"))?;
        let current = admitted.load_complete(uuid)?;
        if !current.is_shallow() {
            return Err(Error::new(
                "the local task copy changed before its refresh; it is kept for a person's review",
            ));
        }
        let mut incoming = TaskDoc::from_bytes(bytes)?;
        incoming.repair_schema()?;
        if incoming.uuid() != uuid || incoming.view()?.project != self.project_id() {
            return Err(Error::new("the fresh copy is for another task or project"));
        }
        if incoming.applied_version().is_none()
            || !incoming.current_vv().includes_vv(&current.current_vv())
        {
            return Err(Error::new(
                "the fresh copy does not cover all local changes; the shallow \
                 copy and any unsent edits are kept for a person's review",
            ));
        }
        admitted.save_received(&mut incoming)?;
        Ok(true)
    }

    /// How a copy here that does not load meets another replica's whole
    /// document. The valid prefix of the update blocks saved here is always
    /// merged into that document before anything is decided.
    ///
    /// A copy this store cut over to the execution schema is rebuilt only
    /// from local proof: its mark is readable and fresh (the updates file
    /// still opens with the block it names, so no other writer folded or
    /// rewrote the files since), every block applies, and the result holds
    /// the version the mark says this store last saved. Everything this
    /// store saved is then inside the result, whatever was lost or cut from
    /// the files; an older build's later appends are blocks applied here.
    /// Anything less keeps it for a person's review.
    ///
    /// A copy saved without a mark gives way only when every local block
    /// applies and the result shows no attempts or floor; a block that does
    /// not read or apply may hold them, so it keeps the copy too.
    fn recovered(&self, uuid: &str, bytes: &[u8]) -> Recovery {
        let Ok(mut task) = TaskDoc::from_bytes(bytes) else {
            return Recovery::Unreadable;
        };
        let marked = self.was_managed(uuid);
        let local = match crate::store::read_regular(&self.updates_path(uuid)) {
            Ok(local) => local,
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(_) if marked => return Recovery::Kept(Keep::Marked),
            Err(_) => return Recovery::Kept(Keep::UnreadableLocalData),
        };
        let (found, intact) = crate::store::blocks(&local);
        let mut applied = true;
        for (payload, _) in found {
            match task.import_block(payload) {
                Ok(true) => {}
                Ok(false) => {
                    applied = false;
                    break;
                }
                Err(error) => return Recovery::Busy(error),
            }
        }
        if !marked {
            return if task.execution_managed() {
                Recovery::Kept(Keep::AttemptEvidence)
            } else if !intact {
                Recovery::Kept(Keep::UnreadableLocalData)
            } else if !applied {
                Recovery::Kept(Keep::Pending)
            } else {
                Recovery::Replaced(task)
            };
        }
        match self.fresh_version(uuid) {
            Some(saved) if intact && applied && task.current_vv().includes_vv(&saved) => {
                Recovery::Whole(task)
            }
            _ => Recovery::Kept(Keep::Marked),
        }
    }

    /// Copies a task's files that do not load to `<name>.damaged-<ms>`, kept
    /// on disk. The files themselves stay until the save that follows
    /// replaces them, so a crash before it leaves them to be read again.
    fn set_aside(&self, uuid: &str) -> Result<()> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        for path in [self.snapshot_path(uuid), self.updates_path(uuid)] {
            match crate::store::read_regular(&path) {
                Ok(bytes) => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    write_atomic(
                        &path.with_file_name(format!("{name}.damaged-{stamp}")),
                        &bytes,
                    )?;
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.note_change(uuid);
        Ok(())
    }
}

enum Recovery {
    /// Temporary incomplete proof never authorizes replacement or damage.
    Busy(Error),
    /// This copy's whole state with the other document merged in.
    Whole(TaskDoc),
    /// The other document, with every block saved here, replaces this copy.
    Replaced(TaskDoc),
    /// The other document does not load either.
    Unreadable,
    /// This copy stays damaged for a person's review, and why.
    Kept(Keep),
}

/// A document an import refused, and, for a copy here kept for a person's
/// review, why.
struct Refusal {
    error: Error,
    kept: Option<Keep>,
    refresh: bool,
}

impl From<Error> for Refusal {
    fn from(error: Error) -> Self {
        Self {
            error,
            kept: None,
            refresh: false,
        }
    }
}

#[cfg(test)]
mod tests {
    //! The live-store repair of the execution schema (zerus-cg5h.4 S2b.2):
    //! a lower value an older replica's concurrent upgrade left in a merge is
    //! raised again under the project lock before the task is saved, read for
    //! an inventory or exported.
    use super::*;
    use crate::execution::{NativeRequest, Origin, EXECUTION_TASK_SCHEMA};
    use crate::store::{Mark, DEPENDS_ON_INCOMING_COPY};
    use crate::{Actor, NewTask, Target};

    const ID: &str = "11111111-1111-4111-8111-111111111111";

    fn created() -> TaskDoc {
        TaskDoc::create(NewTask {
            project: "project-1",
            id: "T-a1b2c3",
            text: "Add rate limiting",
            stage: Stage::Todo,
            target: Target::Any,
            author: Actor::Person,
            rank: "V",
            now: 1,
        })
        .unwrap()
    }

    fn reserve(task: &mut TaskDoc) {
        let executor = SessionKey {
            machine: "m1".into(),
            session: "codex/api".into(),
            session_id: "launch_id:api".into(),
        };
        let native = NativeRequest {
            machine: "m1".into(),
            name: "codex/api".into(),
            run_id: "run-api".into(),
            conversation_id: "conversation-api".into(),
            digest: "ab".repeat(32),
        };
        task.reserve_attempt(ID, Origin::Send, &executor, native, &Actor::Person, 10)
            .unwrap();
    }

    /// A newer replica reserves the first attempt while an older one, forked
    /// from the same task, makes enough changes ending in its own schema
    /// upgrade that the merge keeps that lower value. Returns the newer copy
    /// and the older one's changes since the fork.
    fn race() -> (TaskDoc, Vec<u8>) {
        let base = created();
        let bytes = base.snapshot().unwrap();
        let version = base.version();
        let mut newer = TaskDoc::from_bytes(&bytes).unwrap();
        reserve(&mut newer);
        let mut older = TaskDoc::from_bytes(&bytes).unwrap();
        for n in 0..8 {
            older
                .comment(&format!("older {n}"), &Actor::Person, 2)
                .unwrap();
        }
        older.set_stored_schema_for_test(5);
        (newer, older.export_since(&version).unwrap())
    }

    /// The merge as an importer without the repair leaves it.
    fn merged_unrepaired() -> TaskDoc {
        let (newer, older) = race();
        let mut merged = TaskDoc::from_bytes(&newer.snapshot().unwrap()).unwrap();
        merged.import_unrepaired_for_test(&older);
        assert_eq!(merged.stored_schema(), 5, "the older value wins this merge");
        assert!(merged.needs_schema_repair());
        merged
    }

    fn update(task: &TaskDoc, bytes: Vec<u8>) -> Update {
        Update {
            document: Document::Task(task.uuid().to_owned()),
            bytes,
        }
    }

    #[test]
    fn conflicting_cutover_roots_are_explained_and_both_saved_copies_are_kept() {
        let base = created().snapshot().unwrap();
        let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let stores = roots.each_ref().map(|root| ProjectStore::open(root.path(), "project-1").unwrap());
        let nodes = [ID, "22222222-2222-4222-8222-222222222222"];
        for (store, node) in stores.iter().zip(nodes) {
            // Two offline migrations from the same legacy task each write
            // their own floor, then admit their own immutable seal.
            let mut task = TaskDoc::from_bytes(&base).unwrap();
            task.floor().unwrap();
            store.save_legacy_for_test(&mut task).unwrap();
            let (capture, bytes) = store.prepare_cutover(task.uuid(), node).unwrap();
            let mut proposed = TaskDoc::from_bytes(&bytes).unwrap();
            proposed.seal_cutover(BTreeMap::from([(node.into(), capture.clone())]), BTreeMap::new()).unwrap();
            store.adopt_cutover(&proposed.snapshot().unwrap(), Some(&capture)).unwrap();
        }
        let uuid = TaskDoc::from_bytes(&base).unwrap().uuid().to_owned();
        let tasks = stores.each_ref().map(|store| store.load(&uuid).unwrap());
        assert_ne!(tasks[0].cutover_seal().unwrap(), tasks[1].cutover_seal().unwrap());
        let before = stores.each_ref().map(|store| {
            ["loro", "updates", "managed", "cutover"].map(|ext| {
                fs::read(store.dir().join("tasks").join(format!("{uuid}.{ext}"))).ok()
            })
        });
        for (dest, source) in [(0, 1), (1, 0)] {
            let imported = stores[dest].import_updates(vec![update(&tasks[source], tasks[source].snapshot().unwrap())]).unwrap();
            assert_eq!(imported.changed, 0);
            assert_eq!(imported.refused.len(), 1);
            assert_eq!(imported.refused[0].uuid, uuid);
            let mut candidate = TaskDoc::from_bytes(&tasks[dest].snapshot().unwrap()).unwrap();
            let cause = candidate.import(&tasks[source].snapshot().unwrap()).unwrap_err().to_string();
            assert_eq!(imported.refused[0].error, cause, "keep the historical validation cause in the wire refusal");
            assert_ne!(cause, "the updates could not be applied");
            let after = ["loro", "updates", "managed", "cutover"].map(|ext| {
                fs::read(stores[dest].dir().join("tasks").join(format!("{uuid}.{ext}"))).ok()
            });
            assert_eq!(after, before[dest], "a refusal must preserve document and native custody");
        }
    }

    #[test]
    fn a_live_import_saves_the_execution_schema_a_merge_lowered() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        let (mut newer, older) = race();
        store.save_legacy_for_test(&mut newer).unwrap();
        let imported = store.import_updates(vec![update(&newer, older)]).unwrap();
        assert_eq!((imported.changed, imported.refused.len()), (1, 0));
        let saved = store.load(newer.uuid()).unwrap();
        assert_eq!(saved.stored_schema(), EXECUTION_TASK_SCHEMA);
        // Attempts and their facts are unchanged by the repair.
        assert_eq!(
            saved.execution_records().unwrap(),
            newer.execution_records().unwrap()
        );
        // An import that changes nothing still repairs a copy that needs it.
        let mut stale = merged_unrepaired();
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        store.save_legacy_for_test(&mut stale).unwrap();
        let again = store
            .import_updates(vec![update(&stale, stale.snapshot().unwrap())])
            .unwrap();
        assert_eq!(again.refused.len(), 0);
        assert_eq!(
            store.load(stale.uuid()).unwrap().stored_schema(),
            EXECUTION_TASK_SCHEMA
        );
    }

    #[test]
    fn an_inventory_repairs_a_copy_an_older_importer_saved_and_exports_the_repair() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        let mut stale = merged_unrepaired();
        store.save_legacy_for_test(&mut stale).unwrap();
        let stale_version = version(&stale.current_vv());
        let (inventory, damaged) = store.inventory().unwrap();
        assert!(damaged.is_empty());
        let saved = store.load(stale.uuid()).unwrap();
        assert_eq!(saved.stored_schema(), EXECUTION_TASK_SCHEMA);
        let listed = &inventory.tasks[stale.uuid()];
        assert_eq!(listed, &version(&saved.current_vv()));
        assert_ne!(listed, &stale_version);
        // A replica that holds the stale copy receives the repair.
        let theirs = Inventory {
            project: Version::new(),
            tasks: BTreeMap::from([(stale.uuid().to_owned(), stale_version)]),
        };
        let (page, _) = store.updates_for(&inventory, &theirs, 1 << 20).unwrap();
        let mut peer = TaskDoc::from_bytes(&stale.snapshot().unwrap()).unwrap();
        let bytes = page
            .into_iter()
            .find(|u| u.document == Document::Task(stale.uuid().to_owned()))
            .unwrap()
            .bytes;
        peer.import_unrepaired_for_test(&bytes);
        assert_eq!(peer.stored_schema(), EXECUTION_TASK_SCHEMA);
    }

    /// desktop-u G1 on #1415 (R7): an inventory cache made before the seal
    /// rule (marked `schema-repair-2`, as R5 wrote it) remembers a task with
    /// an unsealed mark as healthy. This build never trusts it: the task is
    /// read, kept as damaged, and its unsealed mark stays exactly as it was.
    #[test]
    fn a_cache_from_before_the_seal_never_serves_an_unsealed_mark_as_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        let mut task = created();
        assert!(task.floor().unwrap());
        store.save(&mut task).unwrap();
        let uuid = task.uuid().to_owned();
        // The mark as R5 wrote it: its fields, no seal.
        let sealed = store.mark(&uuid).unwrap();
        let unsealed = serde_json::to_vec(&sealed).unwrap();
        fs::write(store.managed_path(&uuid), &unsealed).unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(60);
        for path in [
            store.snapshot_path(&uuid),
            store.updates_path(&uuid),
            store.managed_path(&uuid),
        ] {
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(old_time))
                .unwrap();
        }
        assert!(store.load(&uuid).is_err());
        // R5's warm cache: the task healthy, under the stamps it has now.
        let cache = Cache {
            project: None,
            tasks: BTreeMap::from([(
                uuid.clone(),
                Cached {
                    files: store.stamps(&uuid),
                    version: version(&task.current_vv()),
                    error: None,
                    stage: Some(task.stage().unwrap()),
                    stage_at: Some(task.stage_at()),
                    sent: Some(Sent { session: None }),
                },
            )]),
        };
        let bytes = serde_json::to_vec(&cache).unwrap();
        let write_cache = |marker: String| {
            fs::write(store.dir().join(CACHE), &bytes).unwrap();
            fs::write(store.dir().join(CACHE_MARK), marker).unwrap();
        };
        write_cache(format!("schema-repair-2 {:x}\n", Sha256::digest(&bytes)));
        let (inventory, damaged) = store.inventory().unwrap();
        assert!(!inventory.tasks.contains_key(&uuid));
        assert!(damaged.iter().any(|d| d.uuid == uuid));
        assert_eq!(fs::read(store.managed_path(&uuid)).unwrap(), unsealed);
        // The marker is what decides: the same bytes under this build's
        // marker would be trusted, which this build never writes for such a
        // task, since its own reading keeps it damaged.
        write_cache(cache_mark(&bytes));
        assert!(store.inventory().unwrap().0.tasks.contains_key(&uuid));
        assert_eq!(fs::read(store.managed_path(&uuid)).unwrap(), unsealed);
    }

    /// review-n on #1415 (R5): a save that fails after its mark leaves the
    /// copy kept; a warm inventory cache entry never serves it as healthy,
    /// since the entry stamps the mark with the task's files.
    #[test]
    fn a_mark_only_change_reaches_a_warm_inventory() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        let mut task = created();
        assert!(task.floor().unwrap());
        store.save(&mut task).unwrap();
        let uuid = task.uuid().to_owned();
        let old_time = SystemTime::now() - Duration::from_secs(60);
        let age = |path: &Path, modified| {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(modified))
                .unwrap();
        };
        for path in [
            store.snapshot_path(&uuid),
            store.updates_path(&uuid),
            store.managed_path(&uuid),
        ] {
            age(&path, old_time);
        }
        let (warm, damaged) = store.inventory().unwrap();
        assert!(damaged.is_empty() && warm.tasks.contains_key(&uuid));
        let before = store.stamps(&uuid);
        // The mark moves ahead and the save fails before its append.
        task.comment("Never appended", &Actor::Person, 5).unwrap();
        let first = fs::read(store.updates_path(&uuid)).unwrap();
        store
            .write_mark(&uuid, &Mark::new(&task.current_vv(), &first))
            .unwrap();
        #[cfg(unix)]
        age(&store.managed_path(&uuid), old_time);
        #[cfg(windows)]
        // Windows stamps have no file identity: restoring the same mtime
        // and length is a known product limitation. Use a different settled
        // time to test mark-only invalidation without relying on Unix inodes.
        age(&store.managed_path(&uuid), old_time + Duration::from_secs(1));
        let after = store.stamps(&uuid);
        assert_eq!(&after[..2], &before[..2]);
        assert_ne!(after[2], before[2]);
        assert!(store.load(&uuid).is_err());
        let (inventory, damaged) = store.inventory().unwrap();
        assert!(!inventory.tasks.contains_key(&uuid));
        assert!(damaged.iter().any(|d| d.uuid == uuid));
    }

    #[test]
    fn an_older_binary_s_inventory_cache_never_bypasses_the_repair() {
        // review-n on #1402: an older importer persisted a lower-won copy and
        // its own valid cache row for it (same format, matching stamps).
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        let mut stale = merged_unrepaired();
        store.save_legacy_for_test(&mut stale).unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(60);
        for path in [
            store.snapshot_path(stale.uuid()),
            store.updates_path(stale.uuid()),
            store.managed_path(stale.uuid()),
        ] {
            if path.exists() {
                fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(old_time))
                    .unwrap();
            }
        }
        let files = store.stamps(stale.uuid());
        assert!(settled(&files));
        let before = version(&stale.current_vv());
        let older = Cache {
            project: None,
            tasks: BTreeMap::from([(
                stale.uuid().to_owned(),
                Cached {
                    files,
                    version: before.clone(),
                    error: None,
                    stage: Some(Stage::Todo),
                    stage_at: Some(1),
                    sent: Some(Sent { session: None }),
                },
            )]),
        };
        fs::write(store.dir().join(CACHE), serde_json::to_vec(&older).unwrap()).unwrap();
        let (ours, damaged) = store.inventory().unwrap();
        assert!(damaged.is_empty());
        assert_ne!(ours.tasks[stale.uuid()], before);
        assert_eq!(
            store.load(stale.uuid()).unwrap().stored_schema(),
            EXECUTION_TASK_SCHEMA
        );
        let (page, _) = store
            .updates_for(&ours, &Inventory::default(), 1 << 20)
            .unwrap();
        let exported = page
            .iter()
            .find(|u| u.document == Document::Task(stale.uuid().to_owned()))
            .unwrap();
        assert_eq!(
            TaskDoc::from_bytes(&exported.bytes)
                .unwrap()
                .stored_schema(),
            EXECUTION_TASK_SCHEMA
        );
        // The cache this build wrote is trusted again.
        let (again, _) = store.inventory().unwrap();
        assert_eq!(again, ours);
        let mark = fs::read_to_string(store.dir().join(CACHE_MARK)).unwrap();
        assert_eq!(
            mark,
            cache_mark(&fs::read(store.dir().join(CACHE)).unwrap())
        );
    }

    #[test]
    fn a_concurrent_prefloor_write_cannot_be_repaired_into_a_qualified_floor() {
        let base = created();
        let bytes = base.snapshot().unwrap();
        let mut floored = TaskDoc::from_bytes(&bytes).unwrap();
        floored.floor().unwrap();
        let mut older = TaskDoc::from_bytes(&bytes).unwrap();
        for n in 0..8 {
            older
                .comment(&format!("older {n}"), &Actor::Person, 2)
                .unwrap();
        }
        older.set_stored_schema_for_test(5);
        let older_update = older.export_since(&base.version()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        store.save_legacy_for_test(&mut floored).unwrap();
        let original = fs::read(store.snapshot_path(floored.uuid())).unwrap();
        let imported = store
            .import_updates(vec![update(&floored, older_update.clone())])
            .unwrap();
        assert_eq!(imported.refused.len(), 1);
        assert_eq!(
            fs::read(store.snapshot_path(floored.uuid())).unwrap(),
            original
        );
        assert_eq!(
            store.load(floored.uuid()).unwrap().stored_schema(),
            EXECUTION_TASK_SCHEMA
        );
        // A pre-upgrade importer may already have laundered the branch. A
        // schema repair cannot establish its missing causal dependency.
        let mut stale = TaskDoc::from_bytes(&floored.snapshot().unwrap()).unwrap();
        stale.import_unrepaired_for_test(&older_update);
        assert_eq!(stale.stored_schema(), 5);
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        store.save_legacy_for_test(&mut stale).unwrap();
        let original = fs::read(store.snapshot_path(stale.uuid())).unwrap();
        let (inventory, damaged) = store.inventory().unwrap();
        assert!(!inventory.tasks.contains_key(stale.uuid()));
        assert_eq!(damaged.len(), 1);
        assert_eq!(
            fs::read(store.snapshot_path(stale.uuid())).unwrap(),
            original
        );
    }

    #[test]
    fn every_write_keeps_the_schema_and_a_future_one_is_never_lowered() {
        let mut stale = merged_unrepaired();
        stale
            .comment("A newer comment", &Actor::Person, 30)
            .unwrap();
        assert_eq!(stale.stored_schema(), EXECUTION_TASK_SCHEMA);
        // A document of a newer schema is left as it is: read-only here.
        let mut newer = created();
        reserve(&mut newer);
        newer.set_stored_schema_for_test(crate::TASK_SCHEMA + 1);
        assert!(!newer.repair_schema().unwrap());
        let other = TaskDoc::from_bytes(&newer.snapshot().unwrap()).unwrap();
        newer.import(&other.snapshot().unwrap()).unwrap();
        assert_eq!(newer.stored_schema(), crate::TASK_SCHEMA + 1);
        assert!(!newer.view().unwrap().writable);
        // A task without attempts is never raised.
        let mut plain = created();
        assert!(!plain.repair_schema().unwrap());
        assert!(plain.stored_schema() < EXECUTION_TASK_SCHEMA);
    }

    /// A store copy of `created()` beside a peer's copy forked before the
    /// first attempt (after a saved floor when `floored`): the store's copy
    /// reserves it as an update block after its snapshot. Returns the store,
    /// its directory, the task's UUID and the peer's whole document, with
    /// an offline comment unless floored (a phone cannot edit a floored task).
    fn beside_a_peer(floored: bool) -> (tempfile::TempDir, ProjectStore, String, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        let mut task = created();
        store.save_legacy_for_test(&mut task).unwrap();
        if floored {
            assert!(task.floor().unwrap());
            store.save_legacy_for_test(&mut task).unwrap();
        }
        let mut peer = TaskDoc::from_bytes(&task.snapshot().unwrap()).unwrap();
        if !floored {
            peer.comment("Offline on the peer", &Actor::Person, 5)
                .unwrap();
        }
        let mut here = store.load(task.uuid()).unwrap();
        reserve(&mut here);
        store.save_legacy_for_test(&mut here).unwrap();
        assert!(store.updates_path(task.uuid()).exists());
        let uuid = task.uuid().to_owned();
        (dir, store, uuid, peer.snapshot().unwrap())
    }

    /// zerus-r75ru: an import or an inventory beside an irregular file (a
    /// FIFO) in a task's or the cache's place never waits on it. The copy is
    /// kept for review, or its import refused; a marked copy's freshness is
    /// unknown. Each runs under a watchdog, so a regression fails, not hangs.
    #[cfg(unix)]
    #[test]
    fn an_import_or_inventory_beside_an_irregular_file_never_waits() {
        fn fifo(path: &std::path::Path) {
            let _ = fs::remove_file(path);
            let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        }
        fn soon<T: Send + 'static>(
            root: &std::path::Path,
            read: impl FnOnce(ProjectStore) -> T + Send + 'static,
        ) -> T {
            let root = root.to_owned();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(read(ProjectStore::open(&root, "project-1").unwrap()));
            });
            rx.recv_timeout(Duration::from_secs(20))
                .expect("waited on an irregular file")
        }
        // A marked copy's updates log that is a FIFO: its freshness is
        // unknown and the copy is kept, never read through.
        let (dir, store, uuid, peer) = beside_a_peer(true);
        fifo(&store.updates_path(&uuid));
        drop(store);
        let (fresh, kept) = soon(dir.path(), move |store| {
            let fresh = store.fresh_version(&uuid).is_some();
            (
                fresh,
                store.import_task(&uuid, &peer, false).unwrap_err().kept,
            )
        });
        assert!(!fresh);
        assert_eq!(kept, Some(Keep::Marked));
        // An unmarked copy: a FIFO as its updates log is local data that
        // cannot be read (kept); as its snapshot, beside updates that would
        // give way, the replacement refuses when it sets the files aside.
        for snapshot in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let store = ProjectStore::open(dir.path(), "project-1").unwrap();
            let mut task = created();
            store.save_legacy_for_test(&mut task).unwrap();
            task.comment("Local", &Actor::Person, 2).unwrap();
            store.save_legacy_for_test(&mut task).unwrap();
            assert!(!store.was_managed(task.uuid()));
            let (uuid, peer) = (task.uuid().to_owned(), task.snapshot().unwrap());
            fifo(&if snapshot {
                store.snapshot_path(&uuid)
            } else {
                store.updates_path(&uuid)
            });
            drop(store);
            let refused = soon(dir.path(), move |store| {
                store.import_task(&uuid, &peer, false).err()
            });
            let refused = refused.expect("refused");
            if !snapshot {
                assert_eq!(refused.kept, Some(Keep::UnreadableLocalData));
            }
        }
        // The inventory cache and its mark, each written by an inventory of
        // a task and then replaced by a FIFO.
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        let mut task = created();
        store.save_legacy_for_test(&mut task).unwrap();
        // Only settled files are cached.
        let old_time = SystemTime::now() - Duration::from_secs(60);
        for path in [
            store.snapshot_path(task.uuid()),
            store.updates_path(task.uuid()),
        ] {
            if let Ok(file) = fs::File::options().write(true).open(&path) {
                file.set_times(fs::FileTimes::new().set_modified(old_time))
                    .unwrap();
            }
        }
        drop(store);
        for name in [CACHE, CACHE_MARK] {
            let store = ProjectStore::open(dir.path(), "project-1").unwrap();
            for stale in [CACHE, CACHE_MARK] {
                let _ = fs::remove_file(store.dir().join(stale));
            }
            store.inventory().unwrap();
            for written in [CACHE, CACHE_MARK] {
                let meta = fs::symlink_metadata(store.dir().join(written)).unwrap();
                assert!(meta.is_file(), "{written}");
            }
            fifo(&store.dir().join(name));
            drop(store);
            assert!(
                soon(dir.path(), |store| store.inventory().is_ok()),
                "{name}"
            );
        }
    }

    /// zerus-iih7g: a project is listed only by a `project-id` that is a
    /// regular file within its bound, readable without waiting, holding a
    /// valid ID its folder is named after. Any other folder stays unknown:
    /// it is not listed and nothing in it changes.
    #[cfg(unix)]
    #[test]
    fn only_a_folder_s_own_project_id_lists_it() {
        let root = tempfile::tempdir().unwrap();
        for id in ["kept", "fifo", "linked", "other", "large"] {
            drop(ProjectStore::open(root.path(), id).unwrap());
        }
        let folder = |id: &str| {
            root.path()
                .join(format!("{:x}", Sha256::digest(id.as_bytes())))
        };
        let place = |id: &str| folder(id).join("project-id");
        fs::remove_file(place("fifo")).unwrap();
        let name = std::ffi::CString::new(place("fifo").as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        fs::remove_file(place("linked")).unwrap();
        std::os::unix::fs::symlink(place("kept"), place("linked")).unwrap();
        fs::write(place("other"), b"kept").unwrap();
        let bound = crate::store::PROJECT_ID_BYTES as usize;
        fs::write(place("large"), "x".repeat(bound + 1)).unwrap();
        // Folders named after what they hold, which is still not a project:
        // past the bound, empty, or with a control character.
        for id in ["y".repeat(bound + 1), String::new(), "a\nb".to_owned()] {
            fs::create_dir(folder(&id)).unwrap();
            fs::write(place(&id), &id).unwrap();
        }
        let listed = {
            let root = root.path().to_owned();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(ProjectStore::stored(&root).unwrap());
            });
            rx.recv_timeout(Duration::from_secs(20))
                .expect("waited on an irregular project-id")
        };
        assert_eq!(listed, ["kept"]);
        // Nothing was rewritten.
        assert_eq!(fs::read(place("other")).unwrap(), b"kept");
        assert!(fs::symlink_metadata(place("linked")).unwrap().is_symlink());
        assert!(!fs::symlink_metadata(place("fifo")).unwrap().is_file());
    }

    fn damaged_beside_a_pre_attempt_peer() -> (tempfile::TempDir, ProjectStore, String, Vec<u8>) {
        beside_a_peer(false)
    }

    fn damage(store: &ProjectStore, uuid: &str) {
        fs::write(store.snapshot_path(uuid), b"unreadable snapshot fixture").unwrap();
    }

    /// The files beside a task's name, as bytes (a directory or link by kind).
    fn files(store: &ProjectStore, uuid: &str) -> BTreeMap<String, String> {
        fs::read_dir(store.snapshot_path(uuid).parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(uuid))
            .map(|p| {
                let meta = fs::symlink_metadata(&p).unwrap();
                let shown = if meta.is_file() {
                    format!("{:?}", fs::read(&p).map_err(|e| e.kind()))
                } else {
                    format!("{:?}", meta.file_type())
                };
                (p.file_name().unwrap().to_string_lossy().into_owned(), shown)
            })
            .collect()
    }

    /// Two imports of `peer` are refused for a person's review, and neither
    /// renames, removes or copies a file.
    fn kept(store: &ProjectStore, uuid: &str, peer: &[u8]) -> String {
        let before = files(store, uuid);
        let mut reasons = Vec::new();
        for _ in 0..2 {
            let refusals = refused(store, uuid, peer.to_vec());
            assert_eq!(refusals.len(), 1);
            assert!(
                refusals[0].error.contains("person's review"),
                "{}",
                refusals[0].error
            );
            assert!(store.load(uuid).is_err());
            assert_eq!(files(store, uuid), before);
            // zerus-cg5h.4.2: every refusal names the decision and its
            // reason; a copy marked here is always kept as `marked`.
            assert!(refusals[0].kept && refusals[0].recovery.is_none());
            let reason = refusals[0].kept_reason.as_deref().unwrap();
            assert_eq!(reason == "marked", store.was_managed(uuid), "{reason}");
            reasons.push(reason.to_owned());
        }
        assert_eq!(reasons[0], reasons[1]);
        reasons.remove(0)
    }

    /// A cut-over task's updates file after a fold: one block of no changes.
    fn only_the_opening_block(store: &ProjectStore, uuid: &str) {
        let bytes = fs::read(store.updates_path(uuid)).unwrap();
        let (found, intact) = crate::store::blocks(&bytes);
        assert!(intact && found.len() == 1, "{} blocks", found.len());
        let mut task = store.load(uuid).unwrap();
        let before = task.current_vv();
        assert!(task.import_block(found[0].0).unwrap());
        assert_eq!(task.current_vv(), before);
    }

    /// What a build before the mark does when its importer folds: the
    /// snapshot rewritten from its merged copy, the updates file removed,
    /// the mark left as it was.
    fn fold_as_an_older_build(store: &ProjectStore, uuid: &str, peer: &[u8]) {
        let mut task = store.load(uuid).unwrap();
        let mut other = TaskDoc::from_bytes(peer).unwrap();
        for n in 0..6 {
            let version = other.version();
            other
                .comment(&format!("Peer comment {n}"), &Actor::Person, 20 + n)
                .unwrap();
            task.import_unrepaired_for_test(&other.export_since(&version).unwrap());
        }
        write_atomic(&store.snapshot_path(uuid), &task.snapshot().unwrap()).unwrap();
        fs::remove_file(store.updates_path(uuid)).unwrap();
    }

    fn set_aside_files(store: &ProjectStore, uuid: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(store.snapshot_path(uuid).parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(uuid) && name.contains(".damaged-"))
            .collect();
        names.sort();
        names
    }

    fn refused(store: &ProjectStore, uuid: &str, bytes: Vec<u8>) -> Vec<Damaged> {
        let update = Update {
            document: Document::Task(uuid.to_owned()),
            bytes,
        };
        store.import_updates(vec![update]).unwrap().refused
    }

    fn mark_version(store: &ProjectStore, uuid: &str) -> Option<VersionVector> {
        store.mark(uuid)?.version()
    }

    #[test]
    fn the_managed_mark_names_the_saved_version_and_the_block_that_opens_the_updates() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        let mut task = created();
        store.save_received(&mut task).unwrap();
        assert!(!store.was_managed(task.uuid()));
        let snapshot = task.current_vv();
        // The cutover saved as an update block: the mark names everything
        // saved, and that block, the first of the updates file.
        reserve(&mut task);
        store.save_received(&mut task).unwrap();
        assert_eq!(mark_version(&store, task.uuid()), Some(task.current_vv()));
        assert_eq!(store.fresh_version(task.uuid()), Some(task.current_vv()));
        let opening = store.mark(task.uuid()).unwrap().opening;
        // Every later save moves the version; the opening stays.
        task.comment("Appended", &Actor::Person, 11).unwrap();
        store.save_received(&mut task).unwrap();
        assert_eq!(store.fresh_version(task.uuid()), Some(task.current_vv()));
        assert_eq!(store.mark(task.uuid()).unwrap().opening, opening);
        // A fold moves the mark with the snapshot and opens a new updates
        // file with a block of no changes, which this build and older ones
        // read as nothing.
        task.needs_fold = true;
        task.comment("Folded", &Actor::Person, 12).unwrap();
        store.save_received(&mut task).unwrap();
        only_the_opening_block(&store, task.uuid());
        assert_eq!(store.fresh_version(task.uuid()), Some(task.current_vv()));
        assert_ne!(mark_version(&store, task.uuid()), Some(snapshot.clone()));
        // A stale mark beside readable files is corrected by the next save;
        // a mark naming changes the copy lacks refuses it instead.
        let mut foreign = Mark::new(&created().current_vv(), b"foreign");
        store.write_mark(task.uuid(), &foreign).unwrap();
        task.comment("Refused", &Actor::Person, 13).unwrap();
        assert!(store.save_received(&mut task).is_err());
        foreign = Mark::new(&snapshot, b"stale");
        store.write_mark(task.uuid(), &foreign).unwrap();
        assert!(store.fresh_version(task.uuid()).is_none());
        task.comment("Corrected", &Actor::Person, 13).unwrap();
        store.save_received(&mut task).unwrap();
        let reloaded = store.load(task.uuid()).unwrap();
        assert_eq!(
            store.fresh_version(task.uuid()),
            Some(reloaded.current_vv())
        );
        // A task that never cut over still folds to no updates file.
        let mut plain = created();
        store.save_received(&mut plain).unwrap();
        plain.needs_fold = true;
        plain.comment("Plain", &Actor::Person, 2).unwrap();
        store.save_received(&mut plain).unwrap();
        assert!(!store.updates_path(plain.uuid()).exists());
        assert!(!store.was_managed(plain.uuid()));
    }

    /// review-n's control on #1409: a damaged snapshot beside valid update
    /// blocks is rebuilt from the peer's document and those blocks, keeping
    /// every attempt fact, and the damaged files are set aside.
    #[test]
    fn a_damaged_managed_copy_is_rebuilt_from_its_own_updates() {
        let (_dir, store, uuid, peer) = damaged_beside_a_pre_attempt_peer();
        let before = store.load(&uuid).unwrap();
        let facts = before.execution_records().unwrap();
        damage(&store, &uuid);
        assert!(store.load(&uuid).is_err());
        assert!(refused(&store, &uuid, peer).is_empty());
        let rebuilt = store.load(&uuid).unwrap();
        assert_eq!(rebuilt.execution_records().unwrap(), facts);
        assert!(rebuilt.held().unwrap().is_some());
        assert_eq!(rebuilt.stored_schema(), EXECUTION_TASK_SCHEMA);
        assert!(rebuilt.check_send().is_err());
        let comments = rebuilt
            .view()
            .unwrap()
            .log
            .iter()
            .filter(|e| e.kind == "comment")
            .count();
        assert_eq!(comments, 1, "the peer's comment is merged in");
        assert_eq!(set_aside_files(&store, &uuid).len(), 2);
        assert_eq!(store.fresh_version(&uuid), Some(rebuilt.current_vv()));
        only_the_opening_block(&store, &uuid);
    }

    #[test]
    fn a_damaged_managed_copy_the_peer_cannot_cover_is_kept_for_review() {
        // The attempt only the lost snapshot held: this build's fold took it in.
        let (_dir, store, uuid, peer) = damaged_beside_a_pre_attempt_peer();
        let mut folded = store.load(&uuid).unwrap();
        folded.needs_fold = true;
        store.save_received(&mut folded).unwrap();
        only_the_opening_block(&store, &uuid);
        damage(&store, &uuid);
        kept(&store, &uuid, &peer);
        assert!(store.tasks().unwrap().1.iter().any(|d| d.uuid == uuid));
        // zerus-cg5h.4.2: every listing tells the kept copy apart.
        assert!(store
            .tasks()
            .unwrap()
            .1
            .iter()
            .any(|d| d.uuid == uuid && d.kept && d.kept_reason.as_deref() == Some("marked")));
        assert!(store
            .inventory()
            .unwrap()
            .1
            .iter()
            .any(|d| d.uuid == uuid && d.kept && d.kept_reason.as_deref() == Some("marked")));
        assert_eq!(store.locally_kept(&uuid), Some(Keep::Marked));

        // A peer that holds the attempt but not what a later fold took in
        // (here the settlement of its delivery) cannot cover it either.
        let (_dir, store, uuid, _) = damaged_beside_a_pre_attempt_peer();
        let mut here = store.load(&uuid).unwrap();
        let cut_over = here.snapshot().unwrap();
        here.cancel_attempt(ID, "Settled only here", &Actor::Person, 11)
            .unwrap();
        here.needs_fold = true;
        store.save_received(&mut here).unwrap();
        damage(&store, &uuid);
        kept(&store, &uuid, &cut_over);

        // Update blocks whose base the peer lacks cannot rebuild it either.
        let (_dir, store, uuid, peer) = damaged_beside_a_pre_attempt_peer();
        let mut here = store.load(&uuid).unwrap();
        here.comment("Only here", &Actor::Person, 11).unwrap();
        here.needs_fold = true;
        store.save_received(&mut here).unwrap();
        here.comment("After the fold", &Actor::Person, 12).unwrap();
        store.save_received(&mut here).unwrap();
        damage(&store, &uuid);
        kept(&store, &uuid, &peer);

        // A torn last block, a torn first block, unreadable updates or an
        // unreadable mark keeps the copy too.
        for spoil in 0..4 {
            let (_dir, store, uuid, peer) = damaged_beside_a_pre_attempt_peer();
            let path = store.updates_path(&uuid);
            let mut updates = fs::read(&path).unwrap();
            match spoil {
                0 => {
                    updates.push(7);
                    fs::write(&path, &updates).unwrap();
                }
                1 => {
                    updates.truncate(updates.len() / 2);
                    fs::write(&path, &updates).unwrap();
                }
                2 => {
                    fs::remove_file(&path).unwrap();
                    fs::create_dir(&path).unwrap();
                }
                _ => fs::write(store.managed_path(&uuid), b"unreadable mark").unwrap(),
            }
            damage(&store, &uuid);
            kept(&store, &uuid, &peer);
        }
    }

    /// review-n on #1415 (R2): a build before the mark folds the copy here
    /// after the attempt was saved as an update block, leaving the mark as
    /// it was. That mark no longer describes the files, so no other copy is
    /// accepted, floored or not. The fold is reproduced as that build's save
    /// does it; the literal older build does the same in the S2b.2 harness.
    #[test]
    fn a_stale_mark_after_an_older_build_s_fold_never_accepts_another_copy() {
        for floored in [false, true] {
            let (_dir, store, uuid, peer) = beside_a_peer(floored);
            let mark = fs::read(store.managed_path(&uuid)).unwrap();
            fold_as_an_older_build(&store, &uuid, &peer);
            assert_eq!(fs::read(store.managed_path(&uuid)).unwrap(), mark);
            let folded = store.load(&uuid).unwrap();
            assert!(folded.held().unwrap().is_some(), "floored={floored}");
            assert!(store.fresh_version(&uuid).is_none());
            damage(&store, &uuid);
            kept(&store, &uuid, &peer);
            // And after that build appends again.
            let (_dir, store, uuid, peer) = beside_a_peer(floored);
            fold_as_an_older_build(&store, &uuid, &peer);
            let mut task = store.load(&uuid).unwrap();
            let version = task.current_vv();
            task.import_unrepaired_for_test(&peer);
            task.comment("Older build", &Actor::Person, 40).unwrap();
            let mut file = fs::File::options()
                .create(true)
                .append(true)
                .open(store.updates_path(&uuid))
                .unwrap();
            std::io::Write::write_all(
                &mut file,
                &crate::store::block_for_test(&task.export_since_vv(&version).unwrap()),
            )
            .unwrap();
            damage(&store, &uuid);
            kept(&store, &uuid, &peer);
        }
        // A mark restored from before this build's own fold is stale too.
        let (_dir, store, uuid, peer) = damaged_beside_a_pre_attempt_peer();
        let mark = fs::read(store.managed_path(&uuid)).unwrap();
        let mut task = store.load(&uuid).unwrap();
        task.needs_fold = true;
        store.save_received(&mut task).unwrap();
        fs::write(store.managed_path(&uuid), mark).unwrap();
        damage(&store, &uuid);
        kept(&store, &uuid, &peer);
    }

    /// review-n on #1415 (R3): every fold opens the updates file with a
    /// block of its own, so a mark restored from before any later fold no
    /// longer describes the files, while the current mark still rebuilds a
    /// copy a peer covers.
    #[test]
    fn a_mark_restored_from_before_a_later_fold_never_accepts_another_copy() {
        let (_dir, store, uuid, _) = damaged_beside_a_pre_attempt_peer();
        let second = "22222222-2222-4222-8222-222222222222";
        let (mut marks, mut copies, mut openings) = (Vec::new(), Vec::new(), Vec::new());
        for fold in 0..6 {
            let mut task = store.load(&uuid).unwrap();
            match fold {
                0 => task
                    .cancel_attempt(ID, "Settled before the backup", &Actor::Person, 11)
                    .unwrap(),
                1 => {
                    let executor = SessionKey {
                        machine: "m1".into(),
                        session: "codex/api".into(),
                        session_id: "launch_id:api".into(),
                    };
                    let native = NativeRequest {
                        machine: "m1".into(),
                        name: "codex/api".into(),
                        run_id: "run-api".into(),
                        conversation_id: "conversation-api".into(),
                        digest: "cd".repeat(32),
                    };
                    task.reserve_attempt(
                        second,
                        Origin::Send,
                        &executor,
                        native,
                        &Actor::Person,
                        12,
                    )
                    .unwrap();
                }
                n => task
                    .comment(&format!("Fold {n}"), &Actor::Person, 12 + n)
                    .unwrap(),
            }
            task.needs_fold = true;
            store.save_received(&mut task).unwrap();
            only_the_opening_block(&store, &uuid);
            marks.push(fs::read(store.managed_path(&uuid)).unwrap());
            copies.push(store.load(&uuid).unwrap().snapshot().unwrap());
            openings.push(fs::read(store.updates_path(&uuid)).unwrap());
        }
        // review-n's case among them: the second Unknown came after mark 0.
        let distinct: std::collections::BTreeSet<_> = openings.iter().collect();
        assert_eq!(
            distinct.len(),
            openings.len(),
            "each fold opens with its own block"
        );
        let held = store.load(&uuid).unwrap();
        assert_eq!(held.held().unwrap().map(|a| a.id), Some(second.to_owned()));
        damage(&store, &uuid);
        for (earlier, copy) in marks.iter().zip(&copies).take(marks.len() - 1) {
            fs::write(store.managed_path(&uuid), earlier).unwrap();
            assert!(store.fresh_version(&uuid).is_none());
            kept(&store, &uuid, copy);
        }
        // The current mark is fresh: an earlier copy is refused by the
        // version, the latest rebuilds the copy with the second Unknown.
        fs::write(store.managed_path(&uuid), marks.last().unwrap()).unwrap();
        kept(&store, &uuid, &copies[0]);
        assert!(refused(&store, &uuid, copies.last().unwrap().clone()).is_empty());
        let rebuilt = store.load(&uuid).unwrap();
        assert_eq!(
            rebuilt.held().unwrap().map(|a| a.id),
            Some(second.to_owned())
        );
        // The mark names the whole first block: another digest, or none, no
        // longer describes it.
        let mut forged = store.mark(&uuid).unwrap();
        assert!(store.fresh_version(&uuid).is_some());
        forged.opening = Some("00".repeat(32));
        store.write_mark(&uuid, &forged).unwrap();
        assert!(store.fresh_version(&uuid).is_none());
        forged.opening = None;
        store.write_mark(&uuid, &forged).unwrap();
        assert!(store.fresh_version(&uuid).is_none());
        let mut task = store.load(&uuid).unwrap();
        task.comment("The next save corrects the mark", &Actor::Person, 30)
            .unwrap();
        store.save_received(&mut task).unwrap();
        assert!(store.fresh_version(&uuid).is_some());
        // A fold with no change since the last one opens with the same block
        // and writes the same mark: the two describe the same version.
        let mut folded = store.load(&uuid).unwrap();
        folded.needs_fold = true;
        store.save_received(&mut folded).unwrap();
        let mark = fs::read(store.managed_path(&uuid)).unwrap();
        let opening = fs::read(store.updates_path(&uuid)).unwrap();
        let mut again = store.load(&uuid).unwrap();
        again.needs_fold = true;
        store.save_received(&mut again).unwrap();
        assert_eq!(fs::read(store.updates_path(&uuid)).unwrap(), opening);
        assert_eq!(fs::read(store.managed_path(&uuid)).unwrap(), mark);
    }

    /// The end of each block of the task's updates file.
    fn block_ends(store: &ProjectStore, uuid: &str) -> Vec<u64> {
        let bytes = fs::read(store.updates_path(uuid)).unwrap();
        let (found, intact) = crate::store::blocks(&bytes);
        assert!(intact);
        found.iter().map(|(_, end)| *end as u64).collect()
    }

    fn second_attempt(task: &mut TaskDoc) {
        let executor = SessionKey {
            machine: "m1".into(),
            session: "codex/api".into(),
            session_id: "launch_id:api".into(),
        };
        let native = NativeRequest {
            machine: "m1".into(),
            name: "codex/api".into(),
            run_id: "run-api".into(),
            conversation_id: "conversation-api".into(),
            digest: "cd".repeat(32),
        };
        task.reserve_attempt(
            "22222222-2222-4222-8222-222222222222",
            Origin::Send,
            &executor,
            native,
            &Actor::Person,
            12,
        )
        .unwrap();
    }

    /// review-n on #1415 (R4): every save of a cut-over task moves the mark
    /// to the version it saves, so updates cut short at any block boundary
    /// leave the files holding less than the mark records and keep the copy.
    #[test]
    fn updates_cut_short_at_any_block_keep_what_was_saved() {
        let (_dir, store, uuid, _) = damaged_beside_a_pre_attempt_peer();
        let mut task = store.load(&uuid).unwrap();
        task.cancel_attempt(ID, "Settled before the fold", &Actor::Person, 11)
            .unwrap();
        task.needs_fold = true;
        store.save_received(&mut task).unwrap();
        let peer = store.load(&uuid).unwrap().snapshot().unwrap();
        // review-n's case: the second Unknown is saved as an append.
        let mut task = store.load(&uuid).unwrap();
        second_attempt(&mut task);
        store.save_received(&mut task).unwrap();
        for n in 0..3 {
            task.comment(&format!("Append {n}"), &Actor::Person, 20 + n)
                .unwrap();
            store.save_received(&mut task).unwrap();
        }
        let full = fs::read(store.updates_path(&uuid)).unwrap();
        let ends = block_ends(&store, &uuid);
        assert_eq!(ends.len(), 5, "the opening block and four appends");
        // Cut after the opening block (review-n's case) and after each later
        // block: complete blocks, so the file still reads as whole. Even
        // with its snapshot intact the copy no longer loads without them.
        let snapshot = fs::read(store.snapshot_path(&uuid)).unwrap();
        for end in &ends[..ends.len() - 1] {
            let file = fs::File::options()
                .write(true)
                .open(store.updates_path(&uuid))
                .unwrap();
            file.set_len(*end).unwrap();
            let error = store.load(&uuid).err().unwrap().to_string();
            assert!(
                error.contains("holds less than this computer saved"),
                "{error}"
            );
            assert!(store.tasks().unwrap().1.iter().any(|d| d.uuid == uuid));
            damage(&store, &uuid);
            kept(&store, &uuid, &peer);
            fs::write(store.updates_path(&uuid), &full).unwrap();
            fs::write(store.snapshot_path(&uuid), &snapshot).unwrap();
            assert!(store.load(&uuid).is_ok());
        }
        damage(&store, &uuid);
        // Whole, the same peer copy rebuilds it with the second Unknown held.
        assert!(refused(&store, &uuid, peer).is_empty());
        let rebuilt = store.load(&uuid).unwrap();
        assert!(rebuilt.held().unwrap().is_some());
        assert!(rebuilt
            .view()
            .unwrap()
            .log
            .iter()
            .any(|e| e.text == "Append 2"));
    }

    /// review-n on #1415 (R4): an ordinary load is fenced by the same mark.
    /// A readable copy that holds less than the mark records is damaged, not
    /// loaded without those changes, whoever wrote its files; a mark that
    /// cannot be read keeps it too. A task never cut over is untouched.
    #[test]
    fn a_readable_copy_is_fenced_by_its_mark() {
        // review-n's case: the second Unknown's block cut away, snapshot intact.
        let (_dir, store, uuid, _) = damaged_beside_a_pre_attempt_peer();
        let mut task = store.load(&uuid).unwrap();
        task.cancel_attempt(ID, "Settled before the fold", &Actor::Person, 11)
            .unwrap();
        task.needs_fold = true;
        store.save_legacy_for_test(&mut task).unwrap();
        let opening = block_ends(&store, &uuid)[0];
        let mut task = store.load(&uuid).unwrap();
        second_attempt(&mut task);
        store.save_legacy_for_test(&mut task).unwrap();
        let full = fs::read(store.updates_path(&uuid)).unwrap();
        fs::File::options()
            .write(true)
            .open(store.updates_path(&uuid))
            .unwrap()
            .set_len(opening)
            .unwrap();
        let error = store.load(&uuid).err().unwrap().to_string();
        assert!(
            error.contains("holds less than this computer saved"),
            "{error}"
        );
        assert!(store.find(&uuid).is_err());
        assert!(store.tasks().unwrap().1.iter().any(|d| d.uuid == uuid));
        // An older build that read the cut files and folded them leaves a
        // snapshot without the Unknown and no opening the mark names.
        let mut older =
            TaskDoc::from_bytes(&fs::read(store.snapshot_path(&uuid)).unwrap()).unwrap();
        let cut = fs::read(store.updates_path(&uuid)).unwrap();
        for (payload, _) in crate::store::blocks(&cut).0 {
            assert!(older.import_block(payload).unwrap());
        }
        assert!(older.held().unwrap().is_none());
        write_atomic(&store.snapshot_path(&uuid), &older.snapshot().unwrap()).unwrap();
        fs::remove_file(store.updates_path(&uuid)).unwrap();
        assert!(store.fresh_version(&uuid).is_none());
        assert!(store.load(&uuid).is_err());
        // Whole again, it loads; a mark that cannot be read keeps it.
        let (_dir, store, uuid, _) = damaged_beside_a_pre_attempt_peer();
        fs::write(store.updates_path(&uuid), &full[..0]).unwrap();
        assert!(store.load(&uuid).is_err());
        let (_dir, store, uuid, _) = damaged_beside_a_pre_attempt_peer();
        assert!(store.load(&uuid).is_ok());
        let mark = fs::read(store.managed_path(&uuid)).unwrap();
        fs::write(store.managed_path(&uuid), b"unreadable mark").unwrap();
        assert!(store.load(&uuid).is_err());
        #[cfg(unix)]
        {
            fs::remove_file(store.managed_path(&uuid)).unwrap();
            std::os::unix::fs::symlink("absent-mark-target", store.managed_path(&uuid)).unwrap();
            assert!(store.load(&uuid).is_err());
            fs::remove_file(store.managed_path(&uuid)).unwrap();
        }
        fs::write(store.managed_path(&uuid), mark).unwrap();
        assert!(store.load(&uuid).is_ok());
        // A task that never cut over loads as before, with no mark read.
        let mut plain = created();
        store.save_legacy_for_test(&mut plain).unwrap();
        assert!(!store.was_managed(plain.uuid()));
        assert!(store.load(plain.uuid()).is_ok());
    }

    /// review-n on #1415 (R5): two documents of one task read from the same
    /// store. The one read first saves after the other saved an Unknown:
    /// its own changes are merged into what is saved, folding or not, the
    /// Unknown survives and the mark never moves back.
    #[test]
    fn a_stale_document_from_the_same_store_merges_and_never_lowers_the_mark() {
        for fold in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let store = ProjectStore::open(dir.path(), "project-1").unwrap();
            let mut task = created();
            assert!(task.floor().unwrap());
            store.save(&mut task).unwrap();
            let uuid = task.uuid().to_owned();
            let mut stale = store.load(&uuid).unwrap();
            let mut fresh = store.load(&uuid).unwrap();
            reserve(&mut fresh);
            store.save(&mut fresh).unwrap();
            let saved = mark_version(&store, &uuid).unwrap();
            stale
                .comment("Written from the older copy", &Actor::Person, 20)
                .unwrap();
            stale.needs_fold = fold;
            store.save(&mut stale).unwrap();
            let merged = store.load(&uuid).unwrap();
            assert!(merged.held().unwrap().is_some(), "fold={fold}");
            assert!(merged
                .view()
                .unwrap()
                .log
                .iter()
                .any(|e| e.text == "Written from the older copy"));
            let after = mark_version(&store, &uuid).unwrap();
            assert!(after.includes_vv(&saved) && after == merged.current_vv());
            // The saving document now holds the merged state.
            assert_eq!(stale.current_vv(), merged.current_vv());
        }
    }

    fn age_files(store: &ProjectStore, uuid: &str) {
        let old_time = SystemTime::now() - Duration::from_secs(60);
        for path in [
            store.snapshot_path(uuid),
            store.updates_path(uuid),
            store.managed_path(uuid),
        ] {
            if let Ok(file) = fs::File::options().write(true).open(&path) {
                file.set_times(fs::FileTimes::new().set_modified(old_time))
                    .unwrap();
            }
        }
    }

    /// review-n on #1415 (R6): an inventory's version of a task and the
    /// stamps it caches with it are one observation. A same-store save that
    /// tries to come between them waits, so the cache never pairs an older
    /// version with the newer files' stamps.
    #[test]
    fn an_inventory_version_and_its_stamps_are_one_observation() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        let mut task = created();
        assert!(task.floor().unwrap());
        store.save(&mut task).unwrap();
        let uuid = task.uuid().to_owned();
        age_files(&store, &uuid);
        let mut other = store.load(&uuid).unwrap();
        let started = std::sync::Arc::new(std::sync::Barrier::new(2));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        std::thread::scope(|scope| {
            let (started_there, done_there) = (started.clone(), done.clone());
            let (store, uuid, other) = (&store, &uuid, &mut other);
            scope.spawn(move || {
                started_there.wait();
                reserve(other);
                store.save(other).unwrap();
                age_files(store, uuid);
                done_there.store(true, std::sync::atomic::Ordering::SeqCst);
            });
            let (started_here, done_here) = (started.clone(), done.clone());
            crate::store::OBSERVING.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    started_here.wait();
                    // The save either waits for this observation or, were it
                    // let in, would finish here.
                    let until = std::time::Instant::now() + Duration::from_millis(300);
                    while !done_here.load(std::sync::atomic::Ordering::SeqCst)
                        && std::time::Instant::now() < until
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }))
            });
            let (first, _) = store.inventory().unwrap();
            crate::store::OBSERVING.with(|hook| *hook.borrow_mut() = None);
            assert_eq!(first.tasks[uuid], version(&task.current_vv()));
        });
        assert!(done.load(std::sync::atomic::Ordering::SeqCst));
        let held = store.load(&uuid).unwrap();
        assert!(held.held().unwrap().is_some());
        let (warm, _) = store.inventory().unwrap();
        assert_eq!(warm.tasks[&uuid], version(&held.current_vv()));
    }

    /// review-n on #1415 (R6): a document read through an earlier store,
    /// saved through a cold one after a newer save and with the mark spoiled,
    /// is refused: nothing writes over an unreadable mark, and a readable
    /// one is never lowered.
    #[test]
    fn a_cold_store_does_not_write_over_an_unreadable_mark() {
        let dir = tempfile::tempdir().unwrap();
        let (uuid, mut older) = {
            let store = ProjectStore::open(dir.path(), "project-1").unwrap();
            let mut task = created();
            assert!(task.floor().unwrap());
            store.save(&mut task).unwrap();
            (task.uuid().to_owned(), store.load(task.uuid()).unwrap())
        };
        {
            let store = ProjectStore::open(dir.path(), "project-1").unwrap();
            let mut newer = store.load(&uuid).unwrap();
            reserve(&mut newer);
            store.save(&mut newer).unwrap();
        }
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        let mark = fs::read(store.managed_path(&uuid)).unwrap();
        older.note("A harmless note", &Actor::Person, 30).unwrap();
        for unreadable in [true, false] {
            if unreadable {
                fs::write(store.managed_path(&uuid), b"unreadable mark").unwrap();
            } else {
                fs::write(store.managed_path(&uuid), &mark).unwrap();
            }
            let before = files(&store, &uuid);
            assert!(store.save(&mut older).is_err(), "unreadable={unreadable}");
            assert_eq!(files(&store, &uuid), before, "unreadable={unreadable}");
        }
        assert!(store.load(&uuid).unwrap().held().unwrap().is_some());
    }

    /// review-n on #1415 (R6): the mark is sealed over its whole content. One
    /// document across the floor, its fold and the reservation; after the
    /// Unknown is saved, one bit flips in place in a field of the mark and
    /// the saved tail is cut. The copy is kept by a load and by a recovery.
    #[test]
    fn a_flipped_bit_in_any_field_of_the_mark_keeps_the_copy() {
        for field in ["counter", "peer", "opening", "seal"] {
            let dir = tempfile::tempdir().unwrap();
            let store = ProjectStore::open(dir.path(), "project-1").unwrap();
            let mut task = created();
            assert!(task.floor().unwrap());
            store.save(&mut task).unwrap();
            let peer = task.snapshot().unwrap();
            let uuid = task.uuid().to_owned();
            let before = task.current_vv();
            reserve(&mut task);
            store.save(&mut task).unwrap();
            assert!(store.load(&uuid).unwrap().held().unwrap().is_some());
            let path = store.managed_path(&uuid);
            let mut bytes = fs::read(&path).unwrap();
            let text = String::from_utf8(bytes.clone()).unwrap();
            let json: serde_json::Value = serde_json::from_str(&text).unwrap();
            let (peer_id, counter) = json["saved"].as_object().unwrap().iter().next().unwrap();
            let entry = format!("\"{peer_id}\":{counter}");
            let at = text.find(&entry).unwrap();
            // A digit whose lowest bit flips stays a digit: the JSON stays valid.
            let digit_in = |bytes: &[u8], from: usize, to: usize| {
                (from..to).find(|&i| bytes[i].is_ascii_digit()).unwrap()
            };
            // The counter is lowered to exactly what the cut files will hold
            // (review-n's 56 -> 54), so only the seal can tell.
            if field == "counter" {
                let lowered = before
                    .get(&peer_id.parse::<u64>().unwrap())
                    .copied()
                    .unwrap();
                let text = text.replace(&entry, &format!("\"{peer_id}\":{lowered}"));
                assert_ne!(text.as_bytes(), bytes.as_slice());
                bytes = text.into_bytes();
            }
            let flip = match field {
                "counter" => None,
                "peer" => Some(digit_in(&bytes, at + 1, at + 1 + peer_id.len())),
                name => {
                    let value = json[name].as_str().unwrap();
                    let start = text.find(value).unwrap();
                    Some(digit_in(&bytes, start, start + value.len()))
                }
            };
            if let Some(flip) = flip {
                bytes[flip] ^= 1;
            }
            fs::write(&path, &bytes).unwrap();
            assert!(
                serde_json::from_slice::<serde_json::Value>(&bytes).is_ok(),
                "{field}"
            );
            // The complete saved tail is cut: only the opening block remains.
            let opening = block_ends(&store, &uuid)[0];
            fs::File::options()
                .write(true)
                .open(store.updates_path(&uuid))
                .unwrap()
                .set_len(opening)
                .unwrap();
            assert!(store.mark(&uuid).is_none(), "{field}");
            assert!(
                store.load(&uuid).is_err(),
                "{field}: the load dropped the Unknown"
            );
            assert!(store.tasks().unwrap().1.iter().any(|d| d.uuid == uuid));
            damage(&store, &uuid);
            kept(&store, &uuid, &peer);
        }
    }

    /// desktop-u G1 on #1415 (R6): a document read before its mark became
    /// unreadable (corrupt, a directory, a dangling link, no permission) is
    /// refused when it is saved, its base current or not, and every file and
    /// the mark stay exactly as they were.
    #[test]
    fn a_cached_document_never_writes_over_an_unreadable_mark() {
        type Spoiled = (&'static str, Box<dyn Fn(&Path)>);
        let spoils: Vec<Spoiled> = vec![
            (
                "corrupt",
                Box::new(|p| fs::write(p, b"unreadable mark").unwrap()),
            ),
            (
                "impossible",
                Box::new(|p| fs::write(p, br#"{"saved":{}}"#).unwrap()),
            ),
            (
                "directory",
                Box::new(|p| {
                    fs::remove_file(p).unwrap();
                    fs::create_dir(p).unwrap();
                }),
            ),
            #[cfg(unix)]
            (
                "dangling",
                Box::new(|p| {
                    fs::remove_file(p).unwrap();
                    std::os::unix::fs::symlink("absent-mark-target", p).unwrap();
                }),
            ),
            #[cfg(unix)]
            (
                "no permission",
                Box::new(|p| {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(p, fs::Permissions::from_mode(0o000)).unwrap();
                }),
            ),
        ];
        for (name, spoil) in &spoils {
            for stale in [false, true] {
                let (_dir, store, uuid, _) = damaged_beside_a_pre_attempt_peer();
                let mut cached = store.load(&uuid).unwrap();
                if stale {
                    // Another document's save moved the base on first.
                    let mut other = store.load(&uuid).unwrap();
                    other.comment("Saved first", &Actor::Person, 11).unwrap();
                    store.save_received(&mut other).unwrap();
                }
                cached.note("A harmless note", &Actor::Person, 12).unwrap();
                let path = store.managed_path(&uuid);
                spoil(&path);
                // A reader that ignores permissions cannot show that case.
                if *name == "no permission" && fs::read(&path).is_ok() {
                    continue;
                }
                assert!(store.load(&uuid).is_err(), "{name}");
                let before = files(&store, &uuid);
                let error = store
                    .save_received(&mut cached)
                    .err()
                    .map(|e| e.to_string());
                assert!(
                    error.is_some(),
                    "{name} stale={stale}: a cached document overwrote it"
                );
                assert_eq!(files(&store, &uuid), before, "{name} stale={stale}");
                #[cfg(unix)]
                if *name == "no permission" {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                }
            }
        }
        // A task that never cut over, and one whose mark reads, save as before.
        let (_dir, store, uuid, _) = damaged_beside_a_pre_attempt_peer();
        let mut cached = store.load(&uuid).unwrap();
        cached.note("A harmless note", &Actor::Person, 12).unwrap();
        store.save_received(&mut cached).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let plain = ProjectStore::open(dir.path(), "project-1").unwrap();
        let mut task = created();
        plain.save(&mut task).unwrap();
        task.note("Plain", &Actor::Person, 2).unwrap();
        plain.save(&mut task).unwrap();
    }

    /// review-n's barrier control: two documents of one floored task each
    /// reserve their own Unknown, and two threads save them at once. After
    /// both, the task's files and its mark hold every save that succeeded.
    #[test]
    fn shared_store_saves_keep_every_successful_version_and_never_lower_the_mark() {
        for round in 0..16 {
            let dir = tempfile::tempdir().unwrap();
            let store = ProjectStore::open(dir.path(), "project-1").unwrap();
            let mut task = created();
            assert!(task.floor().unwrap());
            store.save(&mut task).unwrap();
            let uuid = task.uuid().to_owned();
            let mut one = store.load(&uuid).unwrap();
            let mut two = store.load(&uuid).unwrap();
            reserve(&mut one);
            second_attempt(&mut two);
            let barrier = std::sync::Barrier::new(2);
            let (a, b) = std::thread::scope(|scope| {
                let a = scope.spawn(|| {
                    barrier.wait();
                    store.save(&mut one).map(|()| one.current_vv())
                });
                let b = scope.spawn(|| {
                    barrier.wait();
                    store.save(&mut two).map(|()| two.current_vv())
                });
                (a.join().unwrap(), b.join().unwrap())
            });
            let saved: Vec<VersionVector> = [a, b].into_iter().filter_map(|r| r.ok()).collect();
            assert!(!saved.is_empty(), "round {round}");
            let mut raw =
                TaskDoc::from_bytes(&fs::read(store.snapshot_path(&uuid)).unwrap()).unwrap();
            for (payload, _) in
                crate::store::blocks(&fs::read(store.updates_path(&uuid)).unwrap()).0
            {
                assert!(raw.import_block(payload).unwrap());
            }
            let mark = mark_version(&store, &uuid).unwrap();
            for version in &saved {
                assert!(raw.current_vv().includes_vv(version), "round {round}");
                assert!(mark.includes_vv(version), "round {round}");
            }
            assert_eq!(saved.len(), 2, "round {round}: both saves succeed");
            assert_eq!(
                store
                    .load(&uuid)
                    .unwrap()
                    .execution_records()
                    .unwrap()
                    .0
                    .len(),
                2
            );
        }
    }

    /// desktop-u G1 on #1415 (R5): an older build's fold crashed after its
    /// snapshot (holding a second Unknown) but before it removed the updates
    /// file. This build then inventories and exports that version; before it
    /// does, the load raises the mark to it, so after damage an older peer's
    /// copy cannot drop the acknowledged Unknown.
    #[test]
    fn what_this_build_acknowledges_is_inside_the_mark_first() {
        let (_dir, store, uuid, _) = damaged_beside_a_pre_attempt_peer();
        let mut base = store.load(&uuid).unwrap();
        base.cancel_attempt(ID, "first disposed", &Actor::Person, 12)
            .unwrap();
        base.needs_fold = true;
        store.save_received(&mut base).unwrap();
        let peer = base.snapshot().unwrap();
        let mark_before = fs::read(store.managed_path(&uuid)).unwrap();
        let updates_before = fs::read(store.updates_path(&uuid)).unwrap();
        let mut next = TaskDoc::from_bytes(&peer).unwrap();
        second_attempt(&mut next);
        write_atomic(&store.snapshot_path(&uuid), &next.snapshot().unwrap()).unwrap();
        assert_eq!(fs::read(store.updates_path(&uuid)).unwrap(), updates_before);
        assert_eq!(fs::read(store.managed_path(&uuid)).unwrap(), mark_before);
        let (inventory, damaged) = store.inventory().unwrap();
        assert!(damaged.is_empty());
        assert_eq!(
            inventory.tasks.get(&uuid),
            Some(&version(&next.current_vv()))
        );
        // The inventory's load raised the mark to what it acknowledged.
        assert_eq!(mark_version(&store, &uuid), Some(next.current_vv()));
        assert_ne!(fs::read(store.managed_path(&uuid)).unwrap(), mark_before);
        let (page, more) = store
            .updates_for(&inventory, &Inventory::default(), 1 << 20)
            .unwrap();
        assert!(!more);
        let sent = page
            .iter()
            .find(|u| u.document == Document::Task(uuid.clone()))
            .unwrap();
        assert!(TaskDoc::from_bytes(&sent.bytes)
            .unwrap()
            .held()
            .unwrap()
            .is_some());
        // A warm inventory serves the same version and the same mark.
        assert_eq!(
            store.inventory().unwrap().0.tasks.get(&uuid),
            Some(&version(&next.current_vv()))
        );
        damage(&store, &uuid);
        kept(&store, &uuid, &peer);
        // The copy that holds the second Unknown still rebuilds it.
        assert!(refused(&store, &uuid, next.snapshot().unwrap()).is_empty());
        assert_eq!(
            store.load(&uuid).unwrap().held().unwrap().map(|a| a.id),
            Some("22222222-2222-4222-8222-222222222222".to_owned())
        );
    }

    /// The mark is written before the append it describes: a crash between
    /// them leaves it ahead, which keeps the copy unless a peer holds the
    /// change that never landed. An older build's appends after a rollback
    /// only make the file longer than the mark records: still accepted.
    #[test]
    fn a_mark_moved_before_its_append_and_an_older_append_after_it() {
        let (_dir, store, uuid, peer) = damaged_beside_a_pre_attempt_peer();
        let mut task = store.load(&uuid).unwrap();
        task.comment("Saved", &Actor::Person, 11).unwrap();
        store.save_received(&mut task).unwrap();
        let saved = task.snapshot().unwrap();
        task.comment("Never appended", &Actor::Person, 12).unwrap();
        let first = fs::read(store.updates_path(&uuid)).unwrap();
        let first = &first[..block_ends(&store, &uuid)[0] as usize];
        store
            .write_mark(&uuid, &Mark::new(&task.current_vv(), first))
            .unwrap();
        // The copy is kept even before its snapshot is damaged.
        assert!(store.load(&uuid).is_err());
        // A fold that crashed after its mark is ahead the same way, whatever
        // opening it names: the readable copy is kept too.
        let landed = fs::read(store.managed_path(&uuid)).unwrap();
        store
            .write_mark(
                &uuid,
                &Mark::new(&task.current_vv(), b"a fold that never landed"),
            )
            .unwrap();
        assert!(store.load(&uuid).is_err());
        // A mark that cannot be read, or a dangling one, keeps it as well.
        fs::write(store.managed_path(&uuid), b"unreadable mark").unwrap();
        assert!(store.load(&uuid).is_err());
        // The mark of what was saved lets it load again.
        let mut saved_mark = Mark::new(&task.current_vv(), first);
        saved_mark.saved =
            Mark::new(&TaskDoc::from_bytes(&saved).unwrap().current_vv(), first).saved;
        store.write_mark(&uuid, &saved_mark).unwrap();
        assert!(store.load(&uuid).is_ok());
        fs::write(store.managed_path(&uuid), landed).unwrap();
        damage(&store, &uuid);
        kept(&store, &uuid, &saved);
        assert!(refused(&store, &uuid, task.snapshot().unwrap()).is_empty());
        assert!(store
            .load(&uuid)
            .unwrap()
            .view()
            .unwrap()
            .log
            .iter()
            .any(|e| e.text == "Never appended"));

        // An older build appends a peer's change after this store's save.
        let (_dir, store, uuid, peer_bytes) = damaged_beside_a_pre_attempt_peer();
        let _ = peer;
        let task = store.load(&uuid).unwrap();
        let version = task.current_vv();
        let mut older = TaskDoc::from_bytes(&task.snapshot().unwrap()).unwrap();
        older.import_unrepaired_for_test(&peer_bytes);
        older
            .note("Appended by an older build", &Actor::Person, 30)
            .unwrap();
        let mut file = fs::File::options()
            .append(true)
            .open(store.updates_path(&uuid))
            .unwrap();
        std::io::Write::write_all(
            &mut file,
            &crate::store::block_for_test(&older.export_since_vv(&version).unwrap()),
        )
        .unwrap();
        drop(file);
        damage(&store, &uuid);
        let covering = task.snapshot().unwrap();
        assert!(refused(&store, &uuid, covering).is_empty());
        let rebuilt = store.load(&uuid).unwrap();
        assert!(rebuilt.held().unwrap().is_some());
        assert!(rebuilt
            .view()
            .unwrap()
            .log
            .iter()
            .any(|e| e.text == "Appended by an older build"));
    }

    /// review-n on #1415 (R2): without a mark, the valid prefix of the local
    /// blocks is read before anything is decided; attempts or a floor in it
    /// keep the copy, and so does a block that cannot be read or applied.
    #[test]
    fn an_unmarked_copy_keeps_whatever_its_blocks_may_hold() {
        for torn in [false, true] {
            let (_dir, store, uuid, peer) = damaged_beside_a_pre_attempt_peer();
            fs::remove_file(store.managed_path(&uuid)).unwrap();
            if torn {
                let mut updates = fs::read(store.updates_path(&uuid)).unwrap();
                updates.push(9);
                fs::write(store.updates_path(&uuid), updates).unwrap();
            }
            damage(&store, &uuid);
            // The prefix holds the Unknown: the refusal names that evidence.
            assert_eq!(kept(&store, &uuid, &peer), "attempt_evidence");
            // The listing knows only what is local: a torn tail keeps the
            // copy for every incoming copy; an intact one depends on it.
            let listed = store.damaged(&uuid, "listed");
            if torn {
                assert_eq!(listed.kept_reason.as_deref(), Some("unreadable_local_data"));
            } else {
                assert!(!listed.kept);
                assert_eq!(listed.recovery.as_deref(), Some(DEPENDS_ON_INCOMING_COPY));
            }
        }
        let plain = |spoil: bool| {
            let dir = tempfile::tempdir().unwrap();
            let store = ProjectStore::open(dir.path(), "project-1").unwrap();
            let mut task = created();
            store.save_legacy_for_test(&mut task).unwrap();
            let other = task.snapshot().unwrap();
            task.comment("Only here", &Actor::Person, 11).unwrap();
            store.save_legacy_for_test(&mut task).unwrap();
            if spoil {
                let mut updates = fs::read(store.updates_path(task.uuid())).unwrap();
                updates.push(9);
                fs::write(store.updates_path(task.uuid()), updates).unwrap();
            }
            damage(&store, task.uuid());
            (dir, store, task.uuid().to_owned(), other)
        };
        // A torn tail may have held the cutover, and so may updates that
        // cannot be read at all.
        let (_dir, store, uuid, other) = plain(true);
        assert_eq!(kept(&store, &uuid, &other), "unreadable_local_data");
        assert_eq!(store.locally_kept(&uuid), Some(Keep::UnreadableLocalData));
        let (_dir, store, uuid, other) = plain(false);
        fs::remove_file(store.updates_path(&uuid)).unwrap();
        fs::create_dir(store.updates_path(&uuid)).unwrap();
        assert_eq!(kept(&store, &uuid, &other), "unreadable_local_data");
        assert_eq!(store.locally_kept(&uuid), Some(Keep::UnreadableLocalData));
        // A block whose base the incoming copy lacks is pending: kept against
        // that copy, while the listing says it depends on the copy.
        let (_dir, store, uuid, older) = plain(false);
        let mut here = TaskDoc::from_bytes(&older).unwrap();
        here.import(&fs::read(store.updates_path(&uuid)).unwrap()[8..])
            .unwrap();
        here.comment("Folded here", &Actor::Person, 12).unwrap();
        let base = here.version();
        here.comment("After the fold", &Actor::Person, 13).unwrap();
        write_atomic(&store.snapshot_path(&uuid), b"unreadable snapshot fixture").unwrap();
        fs::write(
            store.updates_path(&uuid),
            crate::store::block_for_test(&here.export_since(&base).unwrap()),
        )
        .unwrap();
        assert_eq!(kept(&store, &uuid, &older), "pending");
        assert!(store.locally_kept(&uuid).is_none());
        // A copy whose every block applies and shows none gives way, its
        // own blocks merged in.
        let (_dir, store, uuid, other) = plain(false);
        assert!(refused(&store, &uuid, other).is_empty());
        assert_eq!(set_aside_files(&store, &uuid).len(), 2);
        let replaced = store.load(&uuid).unwrap();
        assert!(replaced.execution_records().unwrap().0.is_empty());
        assert!(replaced
            .view()
            .unwrap()
            .log
            .iter()
            .any(|e| e.text == "Only here"));
        assert!(!store.was_managed(&uuid));
        // With no blocks at all it gives way as before.
        let (_dir, store, uuid, other) = plain(false);
        fs::remove_file(store.updates_path(&uuid)).unwrap();
        assert!(refused(&store, &uuid, other).is_empty());
        // zerus-cg5h.4.2: ordinary damage is never listed as kept, and it
        // reads and serializes as before.
        damage(&store, &uuid);
        let ordinary = store.tasks().unwrap().1;
        assert!(ordinary.len() == 1 && !ordinary[0].kept && store.locally_kept(&uuid).is_none());
        assert!(store.inventory().unwrap().1.iter().all(|d| !d.kept));
        // The listing never says a replacement is safe: what happens is
        // decided against each copy that arrives.
        assert_eq!(
            serde_json::to_value(&ordinary[0]).unwrap(),
            serde_json::json!({"uuid": ordinary[0].uuid, "error": ordinary[0].error,
                               "recovery": "depends_on_incoming_copy"})
        );
        let read: Damaged = serde_json::from_value(
            serde_json::json!({"uuid": ordinary[0].uuid, "error": "older answer"}),
        )
        .unwrap();
        assert!(!read.kept);
    }

    /// review-n on #1415 (R2): a mark that is impossible, unreadable or only
    /// a dangling link keeps the copy; only a verified absence is unmarked.
    #[test]
    fn an_impossible_or_unreadable_mark_keeps_the_copy() {
        type Spoil = Box<dyn Fn(&Path)>;
        let spoils: Vec<Spoil> = vec![
            Box::new(|p| fs::write(p, br#"{"saved":{}}"#).unwrap()),
            Box::new(|p| fs::write(p, br#"{"saved":{"1":0}}"#).unwrap()),
            Box::new(|p| fs::write(p, br#"{"saved":{"1":-1}}"#).unwrap()),
            Box::new(|p| fs::write(p, br#"{"saved":{"peer":3}}"#).unwrap()),
            Box::new(|p| {
                fs::remove_file(p).unwrap();
                fs::create_dir(p).unwrap();
            }),
        ];
        for spoil in &spoils {
            let (_dir, store, uuid, peer) = damaged_beside_a_pre_attempt_peer();
            let mut fresh = store.mark(&uuid).unwrap();
            spoil(&store.managed_path(&uuid));
            // An impossible version beside the right block is still refused.
            if let Some(mark) = store.mark(&uuid) {
                fresh.saved = mark.saved;
                store.write_mark(&uuid, &fresh).unwrap();
            }
            assert!(store.was_managed(&uuid));
            assert!(store.fresh_version(&uuid).is_none());
            damage(&store, &uuid);
            kept(&store, &uuid, &peer);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            let (_dir, store, uuid, peer) = damaged_beside_a_pre_attempt_peer();
            fs::remove_file(store.managed_path(&uuid)).unwrap();
            symlink("absent-mark-target", store.managed_path(&uuid)).unwrap();
            assert!(store.was_managed(&uuid));
            damage(&store, &uuid);
            kept(&store, &uuid, &peer);
            let (_dir, store, uuid, peer) = damaged_beside_a_pre_attempt_peer();
            let path = store.managed_path(&uuid);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
            // A reader that ignores permissions cannot show this case.
            if fs::read(&path).is_err() {
                assert!(store.was_managed(&uuid));
                damage(&store, &uuid);
                kept(&store, &uuid, &peer);
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn only_a_person_who_also_removes_the_mark_accepts_another_copy() {
        let (_dir, store, uuid, peer) = damaged_beside_a_pre_attempt_peer();
        damage(&store, &uuid);
        assert_eq!(
            refused(&store, &uuid, created().snapshot().unwrap()).len(),
            1
        );
        // Moving the damaged files away is not enough: the mark keeps it.
        for path in [store.snapshot_path(&uuid), store.updates_path(&uuid)] {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            fs::rename(&path, path.with_file_name(format!("{name}.reviewed"))).unwrap();
        }
        let refusals = refused(&store, &uuid, peer.clone());
        assert_eq!(refusals.len(), 1);
        assert!(
            refusals[0].error.contains("missing"),
            "{}",
            refusals[0].error
        );
        // Its mark alone keeps it listed, so a person can find it.
        assert!(store.uuids().unwrap().contains(&uuid));
        assert!(store.tasks().unwrap().1.iter().any(|d| d.uuid == uuid));
        assert!(store.inventory().unwrap().1.iter().any(|d| d.uuid == uuid));
        assert!(store
            .find(&uuid)
            .err()
            .unwrap()
            .to_string()
            .contains("damaged"));
        assert!(refusals[0].kept && refusals[0].kept_reason.as_deref() == Some("marked"));
        assert_eq!(store.locally_kept(&uuid), Some(Keep::Marked));
        assert!(!store.snapshot_path(&uuid).exists());
        // Removing the mark too is the person's decision to accept it.
        fs::remove_file(store.managed_path(&uuid)).unwrap();
        assert!(refused(&store, &uuid, peer).is_empty());
        let accepted = store.load(&uuid).unwrap();
        assert!(accepted.execution_records().unwrap().0.is_empty());
        assert!(!store.was_managed(&uuid));
    }

    /// The mark is written before the files it describes. A crash between
    /// them leaves a mark ahead of the snapshot, or a first mark naming a
    /// block that never landed: the first only asks more of a recovery, the
    /// second keeps the copy. Neither loses what the copy held.
    #[test]
    fn a_mark_ahead_of_its_files_never_loses_the_copy() {
        // Ahead: the mark names a version the snapshot never reached.
        let (_dir, store, uuid, peer) = damaged_beside_a_pre_attempt_peer();
        let mut ahead = store.load(&uuid).unwrap();
        ahead.comment("Never saved", &Actor::Person, 11).unwrap();
        let first = fs::read(store.updates_path(&uuid)).unwrap();
        store
            .write_mark(&uuid, &Mark::new(&ahead.current_vv(), &first))
            .unwrap();
        damage(&store, &uuid);
        kept(&store, &uuid, &peer);
        // The peer that holds it too rebuilds the copy.
        let mut covering = TaskDoc::from_bytes(&peer).unwrap();
        covering.import(&ahead.snapshot().unwrap()).unwrap();
        assert!(refused(&store, &uuid, covering.snapshot().unwrap()).is_empty());
        assert!(store.load(&uuid).unwrap().held().unwrap().is_some());

        // First mark, then a crash before the block that cut the task over.
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "project-1").unwrap();
        let mut task = created();
        store.save(&mut task).unwrap();
        store
            .write_mark(task.uuid(), &Mark::new(&task.current_vv(), b"never landed"))
            .unwrap();
        damage(&store, task.uuid());
        kept(&store, task.uuid(), &task.snapshot().unwrap());
    }

    #[test]
    fn a_recovery_keeps_the_damaged_files_in_place_until_its_save_replaces_them() {
        let (_dir, store, uuid, peer) = damaged_beside_a_pre_attempt_peer();
        let updates = fs::read(store.updates_path(&uuid)).unwrap();
        damage(&store, &uuid);
        // The copies come first; the files stay until the save replaces them.
        store.set_aside(&uuid).unwrap();
        assert_eq!(
            fs::read(store.snapshot_path(&uuid)).unwrap(),
            b"unreadable snapshot fixture"
        );
        assert_eq!(fs::read(store.updates_path(&uuid)).unwrap(), updates);
        // A crash here leaves the same recovery to run again.
        assert!(refused(&store, &uuid, peer).is_empty());
        only_the_opening_block(&store, &uuid);
        let kept = set_aside_files(&store, &uuid);
        assert!(kept.len() >= 2, "{kept:?}");
        assert!(store.load(&uuid).unwrap().held().unwrap().is_some());
    }
}
