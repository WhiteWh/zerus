//! The Sessions list's grouping for the webview: each row's project and
//! rank, and the ordered project headers, sent only when they change
//! (`Batch.groups`, `Batch.grouping`). The Tauri pump feeds it every row
//! change and the hub the presented organization. Qt: SessionsWindowRows.cpp
//! 289-338 (headers and members), SessionOrganization.cpp:87-90 (groupFor)
//! and 114-131 (orderedMembers).
use super::edit::Intent;
use super::layout::Layout;
use super::runs::Runs;
use super::{
    default_project, project_id, projects, sessions, INBOX, INBOX_COLOR, INBOX_NAME, MAX_PROJECTS,
};
use crate::{Change, ComputerId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// The most renames kept for the hub, which takes them only while edits are
/// accepted ([`Grouping::take_renames`]): as many as the layout remembers
/// members. The oldest go first.
pub const MAX_RENAMES: usize = super::layout::MAX_MEMBERS;

/// One row's place: its project and its rank inside it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GroupOut {
    pub computer: ComputerId,
    pub key: String,
    pub group: String,
    pub rank: u64,
    /// The archive read whose reply owns this placement. Live rows omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_request: Option<u64>,
}

/// One checked archive placement identity, without session content. The caller
/// supplies its page key and the computer's machine identity, as for live rows.
#[derive(Clone, Debug, PartialEq)]
pub struct ArchivedRow {
    pub computer: ComputerId,
    pub key: String,
    pub machine: String,
    pub archive_id: String,
    pub run_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HeaderOut {
    pub id: String,
    pub name: String,
    /// The project's colour; None uses the palette slot.
    pub color: Option<String>,
    pub accessible: bool,
    pub collapsed: bool,
    pub is_default: bool,
    pub palette: usize,
}

/// Whether edits can run, and the catalog's status line (from `sync::Controller`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncView {
    pub ready: bool,
    /// Why edits cannot run, for the disabled entries.
    pub reason: Option<String>,
    pub busy: bool,
    pub status: String,
    pub status_error: bool,
    pub conflicts: usize,
    /// A one-time warning the hub raises, such as saved project changes it
    /// could not apply; cleared once a catalog state is adopted. Absent from
    /// the JSON when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// Presentation-only status: the saved local layout was refused and is
    /// retained. This carries no service writer/drain authority.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub layout_save_paused: bool,
}

impl Default for SyncView {
    fn default() -> Self {
        Self {
            ready: false,
            reason: Some(super::STARTING.into()),
            busy: false,
            status: String::new(),
            status_error: false,
            conflicts: 0,
            notice: None,
            layout_save_paused: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GroupingOut {
    /// A catalog is presented; before it, the list shows no headers.
    pub loaded: bool,
    pub default_project: String,
    pub headers: Vec<HeaderOut>,
    #[serde(flatten)]
    pub sync: SyncView,
}

#[derive(Debug)]
struct Slot {
    member: String,
    /// Only live, non-archived rows remember a run's project.
    run: Option<String>,
    title: String,
    group: String,
    rank: u64,
    /// Arrival order; a placement of many rows keeps it.
    seq: u64,
}

#[derive(Debug)]
struct ArchiveSlot {
    row: ArchivedRow,
    group: String,
    rank: u64,
    dirty: bool,
}

#[derive(Debug)]
struct ArchivePage {
    /// The shown page, including while an empty next reply raises Archive changed.
    request: u64,
    answered: u64,
    rows: Vec<ArchiveSlot>,
}

type At = (ComputerId, String);

#[derive(Debug)]
pub struct Grouping {
    pub(crate) layout: Layout,
    runs: Runs,
    loaded: bool,
    revision: Option<u64>,
    /// id, name, colour, accessible.
    projects: Vec<(String, String, Option<String>, bool)>,
    listed: HashSet<String>,
    default: String,
    membership: Arc<BTreeMap<String, String>>,
    rows: HashMap<At, Slot>,
    /// Only the last produced page's identities, at most 50 per configured computer.
    archives: BTreeMap<ComputerId, ArchivePage>,
    archive_reset_through: u64,
    dirty: BTreeSet<At>,
    sent: Option<GroupingOut>,
    sync: SyncView,
    /// Renames for the hub ([`Grouping::take_renames`]), at most [`MAX_RENAMES`].
    renames: VecDeque<(String, String)>,
    next_seq: u64,
    headers_dirty: bool,
    /// The layout size after the last prune from `observe`.
    prune_floor: usize,
}

impl Default for Grouping {
    fn default() -> Self {
        Self::new(Layout::default())
    }
}

/// Sort sessions by name (SessionsWindow.cpp:2603-2608): titles case-folded,
/// then as written, then by member. Qt compares with localeAwareCompare,
/// which has no equivalent here.
pub fn by_name(mut rows: Vec<(String, String)>) -> Vec<String> {
    rows.sort_by_cached_key(|(member, title)| {
        (title.to_lowercase(), title.clone(), member.clone())
    });
    rows.into_iter().map(|(member, _)| member).collect()
}

impl Grouping {
    pub fn new(layout: Layout) -> Self {
        Self {
            layout,
            runs: Runs::default(),
            loaded: false,
            revision: None,
            projects: Vec::new(),
            listed: HashSet::new(),
            default: INBOX.into(),
            membership: Arc::default(),
            rows: HashMap::new(),
            archives: BTreeMap::new(),
            archive_reset_through: 0,
            dirty: BTreeSet::new(),
            sent: None,
            sync: SyncView::default(),
            renames: VecDeque::new(),
            next_seq: 0,
            headers_dirty: true,
            prune_floor: 0,
        }
    }

    /// The current project of an identity, including one whose live row left.
    pub fn group_for(&self, member: &str) -> String {
        self.membership
            .get(member)
            .filter(|project| self.listed.contains(project.as_str()))
            .cloned()
            .unwrap_or_else(|| self.default.clone())
    }

    fn place(
        &mut self,
        at: At,
        member: String,
        run: Option<String>,
        title: String,
        seq: u64,
    ) -> bool {
        let group = self.group_for(&member);
        // Qt's run_groups is distinct from the rename token map. Unchanged
        // observations do not refresh the written age (SessionOrganization.cpp:239-243).
        let mut run_changed = false;
        if let Some((machine, run)) = member
            .split_once('\n')
            .and_then(|(machine, _)| run.as_deref().map(|run| (machine, run)))
        {
            let project = self
                .membership
                .get(&member)
                .map(String::as_str)
                .filter(|project| self.listed.contains(*project));
            if self.layout.run_project(machine, run) != project {
                run_changed = self.layout.remember_run(machine, run, project);
            }
        }
        let rank = self.layout.rank(&group, &member);
        let changed = self
            .rows
            .get(&at)
            .is_none_or(|slot| slot.group != group || slot.rank != rank);
        if changed {
            self.dirty.insert(at.clone());
        }
        self.rows.insert(
            at,
            Slot {
                member,
                run,
                title,
                group,
                rank,
                seq,
            },
        );
        run_changed
    }

    fn upsert(&mut self, computer: &ComputerId, key: &str, row: &crate::ViewRow) {
        let run = row
            .run_id
            .as_ref()
            .filter(|run| !run.is_empty() && row.state != "archived" && row.archive_id.is_none())
            .cloned();
        self.upsert_with_run(computer, key, &row.machine, &row.name, &row.title, run);
    }

    fn upsert_with_run(
        &mut self,
        computer: &ComputerId,
        key: &str,
        machine: &str,
        name: &str,
        title: &str,
        run: Option<String>,
    ) {
        let at = (computer.clone(), key.to_owned());
        let member = super::member(machine, name);
        let mut seq = None;
        if let Some(slot) = self.rows.get(&at) {
            seq = Some(slot.seq);
        }
        let remembered = key
            .strip_prefix("run:")
            .and_then(|run| self.runs.observe(machine, run, name));
        // A live upsert and a restart use the same machine-scoped token.
        if let Some(old) = remembered {
            self.renamed(old, &member);
        }
        let seq = seq.unwrap_or_else(|| {
            self.next_seq += 1;
            self.next_seq
        });
        if self.loaded {
            if self.place(at, member, run, title.to_owned(), seq) {
                // A run write can evict another computer's oldest remembered run.
                self.reposition_archives();
            }
        } else {
            // Before the first catalog the row is only recorded: no placement,
            // no rank, no group. A rename above has already moved the layout's
            // place, but cannot move membership (none is known yet); the hub's
            // Renamed intent settles it once edits are accepted.
            self.rows.insert(
                at,
                Slot {
                    member,
                    run,
                    title: title.to_owned(),
                    group: String::new(),
                    rank: 0,
                    seq,
                },
            );
        }
    }

    fn renamed(&mut self, old: String, member: &str) {
        self.layout.rename(&old, member);
        self.runs.rename(&old, member);
        let explicit = self.membership.contains_key(&old);
        if explicit {
            let membership = Arc::make_mut(&mut self.membership);
            if let Some(project) = membership.remove(&old) {
                membership.insert(member.to_owned(), project);
            }
        }
        // Only the hub can prove that the absence of an old member is
        // authoritative rather than the effect of its own unsaved draft.
        // Even fallback renames wait for that check before tokens advance.
        if self.renames.len() >= MAX_RENAMES {
            self.renames.pop_front();
        }
        self.renames.push_back((old, member.to_owned()));
    }

    /// Startup runs off the main thread and may follow the first row batch.
    pub fn replace_runs(&mut self, mut runs: Runs) {
        runs.merge_missing(&self.runs);
        self.runs = runs;
        let mut slots: Vec<_> = self
            .rows
            .iter()
            .map(|(at, slot)| {
                (
                    slot.seq,
                    at.clone(),
                    slot.member.clone(),
                    slot.run.clone(),
                    slot.title.clone(),
                )
            })
            .collect();
        slots.sort_unstable_by_key(|slot| slot.0);
        for (_, at, member, run, title) in slots {
            if let Some((machine, name)) = member.split_once('\n') {
                self.upsert_with_run(&at.0, &at.1, machine, name, &title, run);
            }
        }
    }

    pub fn take_runs(&mut self) -> Option<super::runs::Snapshot> {
        self.runs.take_dirty()
    }

    /// Projects-only copy. Never used by shared edits or session placement.
    pub fn local_windows_projects(
        &self,
        organization: &Value,
        node: &str,
    ) -> Result<Value, &'static str> {
        self.layout
            .overlay_local_windows_folders(organization, node)
    }

    /// A rename is recoverable (durable draft), or its old member was absent.
    pub fn acknowledge_rename(&mut self, from: &str, to: &str) {
        self.runs.acknowledge_rename(from, to);
    }

    /// A row change from the pump.
    pub fn observe(&mut self, change: &Change) {
        match change {
            Change::Upsert { computer, key, row } => self.upsert(computer, key, row),
            Change::Remove { computer, key, .. } => {
                let at = (computer.clone(), key.clone());
                self.rows.remove(&at);
                self.dirty.remove(&at);
            }
            Change::Replace { computer, rows } => {
                let keep: HashSet<&str> = rows.iter().map(|(key, _)| key.as_str()).collect();
                self.rows
                    .retain(|(owner, key), _| owner != computer || keep.contains(key.as_str()));
                self.dirty
                    .retain(|(owner, key)| owner != computer || keep.contains(key.as_str()));
                for (key, row) in rows {
                    self.upsert(computer, key, row);
                }
            }
            Change::Computer {
                computer,
                status: crate::ComputerStatus::Ended,
            } => {
                self.archives.remove(computer);
            }
            _ => {}
        }
        if self.loaded
            && self.layout.len() > super::layout::MAX_MEMBERS.max(self.prune_floor) + 1024
        {
            self.prune_layout();
            self.prune_floor = self.layout.len();
        }
    }

    fn prune_layout(&mut self) {
        self.prune_layout_with(&HashSet::new());
    }

    fn prune_layout_with(&mut self, page: &HashSet<String>) {
        let listed: HashSet<String> = self.rows.values().map(|slot| slot.member.clone()).collect();
        let retained: HashSet<_> = self
            .archives
            .values()
            .flat_map(|page| page.rows.iter())
            .map(|slot| super::archive_member(&slot.row.machine, &slot.row.archive_id))
            .collect();
        let membership = Arc::clone(&self.membership);
        self.layout.prune(|member| {
            listed.contains(member)
                || membership.contains_key(member)
                || page.contains(member)
                || retained.contains(member)
        });
    }

    /// Place one archive page: explicit archive membership, then the run's
    /// last project if it still exists, then the default (Qt observe,
    /// SessionOrganization.cpp:223-247). A page adds no live rows or stream
    /// changes. The normal archive reader rejects oversized pages; this
    /// boundary also caps work at [`crate::reads::ARCHIVE_PAGE_ROWS`].
    pub fn place_archived(&mut self, rows: &[ArchivedRow]) -> Vec<GroupOut> {
        self.place_archive_rows(rows.iter())
    }

    fn place_archive_rows<'a>(
        &mut self,
        rows: impl Iterator<Item = &'a ArchivedRow>,
    ) -> Vec<GroupOut> {
        let mut page = HashSet::new();
        let out = rows
            .take(crate::reads::ARCHIVE_PAGE_ROWS)
            .map(|row| {
                let (group, rank) = self.archive_place(row);
                page.insert(super::archive_member(&row.machine, &row.archive_id));
                GroupOut {
                    computer: row.computer.clone(),
                    key: row.key.clone(),
                    group,
                    rank,
                    archive_request: None,
                }
            })
            .collect();
        if self.layout.len() > super::layout::MAX_MEMBERS.max(self.prune_floor) + 1024 {
            self.prune_layout_with(&page);
            self.prune_floor = self.layout.len();
        }
        out
    }

    /// Read-only archive membership for bounded search hits; no placement cache.
    pub fn archive_project(&self, row: &ArchivedRow) -> String {
        let member = super::archive_member(&row.machine, &row.archive_id);
        self.membership
            .get(&member)
            .map(String::as_str)
            .filter(|project| self.listed.contains(*project))
            .or_else(|| {
                self.layout
                    .run_project(&row.machine, &row.run_id)
                    .filter(|project| self.listed.contains(*project))
            })
            .unwrap_or(&self.default)
            .to_owned()
    }

    fn archive_place(&mut self, row: &ArchivedRow) -> (String, u64) {
        let member = super::archive_member(&row.machine, &row.archive_id);
        let project = self.archive_project(row);
        let rank = self.layout.rank(&project, &member);
        (project, rank)
    }

    /// Produce the reply's placement before retaining its identities. The caller
    /// holds the pump lock across both: no delta for this request can exist before
    /// this snapshot. A later edit's delta may beat IPC delivery; the UI merges it
    /// by archive_request after applying that reply (SessionsWindowRows.cpp:266-300).
    /// The caller checks that this computer is still configured and its source current.
    pub fn retain_archive_page(
        &mut self,
        computer: &ComputerId,
        request: u64,
        rows: &[ArchivedRow],
    ) -> Option<Vec<GroupOut>> {
        if request <= self.archive_reset_through
            || self
                .archives
                .get(computer)
                .is_some_and(|page| request <= page.answered)
        {
            return None;
        }
        let rows = rows.iter().filter(|row| &row.computer == computer);
        // ArchiveBrowser.cpp:37 keeps the shown rows on an empty next reply.
        // Their deltas keep the shown request; the answered watermark still
        // rejects a late reply older than this empty answer.
        if rows.clone().next().is_none() {
            if let Some(page) = self
                .archives
                .get_mut(computer)
                .filter(|page| !page.rows.is_empty())
            {
                page.answered = request;
                return Some(Vec::new());
            }
        }
        let mut out = self.place_archive_rows(rows.clone());
        for place in &mut out {
            place.archive_request = Some(request);
        }
        let retained = rows
            .zip(&out)
            .map(|(row, place)| ArchiveSlot {
                row: row.clone(),
                group: place.group.clone(),
                rank: place.rank,
                dirty: false,
            })
            .collect();
        self.archives.insert(
            computer.clone(),
            ArchivePage {
                request,
                answered: request,
                rows: retained,
            },
        );
        Some(out)
    }

    /// Explicit page reset, separate from hiding/cancelling in-flight reads.
    /// The watermark also rejects a pre-reset reply that arrives late.
    pub fn reset_archive_pages(&mut self, through: u64) {
        self.archive_reset_through = self.archive_reset_through.max(through);
        self.archives.retain(|_, page| page.request > through);
    }

    fn reposition_archive_computer(&mut self, computer: &ComputerId) {
        let Some(mut page) = self.archives.remove(computer) else {
            return;
        };
        for slot in &mut page.rows {
            let (group, rank) = self.archive_place(&slot.row);
            if slot.group != group || slot.rank != rank {
                slot.group = group;
                slot.rank = rank;
                slot.dirty = true;
            }
        }
        self.archives.insert(computer.clone(), page);
    }

    fn reposition_archives(&mut self) {
        let computers: Vec<_> = self.archives.keys().cloned().collect();
        for computer in computers {
            self.reposition_archive_computer(&computer);
        }
    }

    /// The organization the list shows; `revision` changes whenever it does.
    pub fn present(
        &mut self,
        organization: &Value,
        membership: Arc<BTreeMap<String, String>>,
        revision: u64,
    ) {
        if self.revision == Some(revision) {
            return;
        }
        self.revision = Some(revision);
        self.loaded = true;
        self.headers_dirty = true;
        let listed_projects: Vec<&Value> = projects(organization)
            .iter()
            .filter(|project| !project_id(project).is_empty())
            .take(MAX_PROJECTS)
            .collect();
        self.projects = listed_projects
            .iter()
            .map(|project| {
                (
                    project_id(project).to_owned(),
                    project["name"].as_str().unwrap_or("").to_owned(),
                    project["color"]
                        .as_str()
                        .filter(|color| !color.is_empty())
                        .map(str::to_owned),
                    project["accessible"].as_bool().unwrap_or(true),
                )
            })
            .collect();
        if self.projects.is_empty() {
            self.projects.push((
                INBOX.into(),
                INBOX_NAME.into(),
                Some(INBOX_COLOR.into()),
                true,
            ));
        }
        self.listed = self.projects.iter().map(|(id, ..)| id.clone()).collect();
        self.default = default_project(organization, self.layout.default_project());
        if !self.listed.contains(&self.default) {
            self.default = self.projects[0].0.clone();
        }
        self.membership = membership;
        // Startup tokens may reveal a rename before any organization was
        // presented. Keep its project/rank while the hub writes that intent;
        // a second writer that already removed the old member stays untouched.
        if !self.renames.is_empty() {
            let membership = Arc::make_mut(&mut self.membership);
            for (old, new) in &self.renames {
                if let Some(project) = membership.remove(old) {
                    membership.insert(new.clone(), project);
                }
            }
        }
        for project in &listed_projects {
            for member in sessions(project) {
                if self.membership.contains_key(member) {
                    self.layout.rank(project_id(project), member);
                }
            }
        }
        if self.layout.len() > super::layout::MAX_MEMBERS {
            self.prune_layout();
            self.prune_floor = self.layout.len();
        }
        let mut slots: Vec<_> = self
            .rows
            .iter()
            .map(|(at, slot)| {
                (
                    slot.seq,
                    at.clone(),
                    slot.member.clone(),
                    slot.run.clone(),
                    slot.title.clone(),
                )
            })
            .collect();
        slots.sort_unstable_by_key(|slot| slot.0);
        for (seq, at, member, run, title) in slots {
            self.place(at, member, run, title, seq);
        }
        self.reposition_archives();
    }

    /// Replaces the saved layout, keeping the observed rows, membership and
    /// what was sent; the rows are placed again against the new layout.
    pub fn replace_layout(&mut self, layout: Layout) {
        self.layout = layout;
        self.prune_floor = 0;
        self.headers_dirty = true;
        if self.loaded {
            let mut slots: Vec<_> = self
                .rows
                .iter()
                .map(|(at, slot)| {
                    (
                        slot.seq,
                        at.clone(),
                        slot.member.clone(),
                        slot.run.clone(),
                        slot.title.clone(),
                    )
                })
                .collect();
            slots.sort_unstable_by_key(|slot| slot.0);
            for (seq, at, member, run, title) in slots {
                self.place(at, member, run, title, seq);
            }
        }
        self.reposition_archives();
    }

    pub fn runs_seeded(&self) -> bool {
        self.layout.runs_seeded()
    }

    pub fn presentation_seeded(&self) -> Result<bool, &'static str> {
        self.layout.presentation_seeded()
    }

    pub fn windows_folders_import_layout(
        &self,
    ) -> Result<Option<super::layout::Layout>, &'static str> {
        self.layout
            .windows_folders_import_pending()
            .map(|pending| pending.then(|| self.layout.clone()))
    }

    pub fn seed_windows_folders(
        &mut self,
        seed: super::layout::WindowsFoldersSeed,
        context: &super::layout::WindowsFoldersContext<'_>,
    ) -> Result<bool, &'static str> {
        let changed = self.layout.seed_windows_folders(seed, context)?;
        if changed {
            let layout = std::mem::take(&mut self.layout);
            self.replace_layout(layout);
        }
        Ok(changed)
    }

    pub fn seed_presentation(
        &mut self,
        seed: super::layout::PresentationSeed,
    ) -> Result<(), &'static str> {
        if self.layout.seed_presentation(seed)? {
            if let Some(default) = self
                .layout
                .default_project()
                .filter(|id| self.listed.contains(*id))
            {
                self.default = default.into();
            }
            let layout = std::mem::take(&mut self.layout);
            self.replace_layout(layout);
        }
        Ok(())
    }

    /// Merge a seed read on a worker into the current layout, retaining newer
    /// native assignments. Reobserve live rows: implicit assignments made
    /// while the read was pending must remove their old Qt run memory again.
    pub fn seed_runs(&mut self, groups: &serde_json::Map<String, Value>) -> usize {
        if self.layout.runs_seeded() {
            return 0;
        }
        let imported = self.layout.seed_runs(groups);
        let layout = std::mem::take(&mut self.layout);
        self.replace_layout(layout);
        imported
    }

    pub fn set_sync(&mut self, sync: SyncView) {
        self.sync = sync;
        self.headers_dirty = true;
    }

    /// A device-local default, as Qt's presentation overlay. Existing explicit
    /// assignments stay put; implicit rows follow the selected default.
    pub fn set_default_project(&mut self, project: &str) -> Result<(), String> {
        if !self.loaded
            || !self
                .projects
                .iter()
                .any(|(id, _, _, accessible)| id == project && *accessible)
        {
            return Err(super::edit::GONE.into());
        }
        self.layout.set_default_project(Some(project.into()));
        self.default = project.into();
        let layout = std::mem::take(&mut self.layout);
        self.replace_layout(layout);
        Ok(())
    }

    pub fn set_collapsed(&mut self, project: &str, collapsed: bool) {
        self.layout.set_collapsed(project, collapsed);
        self.headers_dirty = true;
    }

    /// The local side of an edit; the hub presents the edited organization next.
    pub fn edited(&mut self, intent: &Intent) {
        match intent {
            Intent::Move {
                members,
                project,
                before,
            } => self.layout.place(project, members, before.as_deref()),
            Intent::Create { id, members, .. } => self.layout.place(id, members, None),
            Intent::Sort { project, members } => self.layout.sort(project, members),
            Intent::Delete {
                project,
                destination,
                ..
            } => {
                if self.default == *project {
                    self.layout.set_default_project(Some(destination.clone()));
                }
                self.layout.forget(project);
            }
            Intent::Color { .. }
            | Intent::Rename { .. }
            | Intent::Reorder { .. }
            | Intent::Renamed { .. }
            | Intent::AddFolder { .. }
            | Intent::Folder { .. } => {}
            Intent::CreateFolder { .. } => {}
        }
        self.revision = None;
        self.headers_dirty = true;
    }

    /// A project's listed rows in list order, with their titles.
    pub fn members_of(&self, project: &str) -> Vec<(String, String)> {
        let mut rows: Vec<&Slot> = self
            .rows
            .values()
            .filter(|slot| slot.group == project)
            .collect();
        rows.sort_by_key(|slot| slot.rank);
        rows.into_iter()
            .map(|slot| (slot.member.clone(), slot.title.clone()))
            .collect()
    }

    pub fn default_project(&self) -> &str {
        &self.default
    }

    fn out(&self) -> GroupingOut {
        let headers = if self.loaded {
            self.projects
                .iter()
                .enumerate()
                .map(|(at, (id, name, color, accessible))| HeaderOut {
                    id: id.clone(),
                    name: name.clone(),
                    color: color.clone(),
                    accessible: *accessible,
                    collapsed: self.layout.collapsed(id),
                    is_default: *id == self.default,
                    palette: at % 8,
                })
                .collect()
        } else {
            Vec::new()
        };
        GroupingOut {
            loaded: self.loaded,
            default_project: self.default.clone(),
            headers,
            sync: self.sync.clone(),
        }
    }

    /// What changed since the last take.
    pub fn take(&mut self) -> (Vec<GroupOut>, Option<GroupingOut>) {
        let mut groups: Vec<_> = std::mem::take(&mut self.dirty)
            .into_iter()
            .filter_map(|at| {
                let slot = self.rows.get(&at)?;
                Some(GroupOut {
                    computer: at.0,
                    key: at.1,
                    group: slot.group.clone(),
                    rank: slot.rank,
                    archive_request: None,
                })
            })
            .collect();
        for (computer, page) in &mut self.archives {
            for slot in &mut page.rows {
                if std::mem::take(&mut slot.dirty) {
                    groups.push(GroupOut {
                        computer: computer.clone(),
                        key: slot.row.key.clone(),
                        group: slot.group.clone(),
                        rank: slot.rank,
                        archive_request: Some(page.request),
                    });
                }
            }
        }
        let headers = if std::mem::take(&mut self.headers_dirty) {
            let out = self.out();
            (self.sent.as_ref() != Some(&out)).then(|| {
                self.sent = Some(out.clone());
                out
            })
        } else {
            None
        };
        (groups, headers)
    }

    /// A reloaded webview: everything again.
    pub fn resend(&mut self) {
        // Before the first catalog rows have no group yet; present sends them.
        self.dirty = if self.loaded {
            self.rows.keys().cloned().collect()
        } else {
            BTreeSet::new()
        };
        self.sent = None;
        self.headers_dirty = true;
    }

    /// The renames seen since the last call, oldest first.
    pub fn take_renames(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.renames).into()
    }

    pub fn take_layout(&mut self) -> Option<Vec<u8>> {
        self.layout.take_dirty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::members;
    use crate::ViewRow;
    use serde_json::json;
    use std::time::{Duration, Instant};

    fn row(machine: &str, name: &str) -> ViewRow {
        ViewRow {
            machine: machine.into(),
            name: name.into(),
            title: name.rsplit('/').next().unwrap_or(name).into(),
            ..ViewRow::default()
        }
    }

    fn upsert(key: &str, machine: &str, name: &str) -> Change {
        Change::Upsert {
            computer: ComputerId::local(),
            key: key.into(),
            row: row(machine, name),
        }
    }

    fn organization(alpha: &[&str], beta: &[&str]) -> Value {
        json!({"default_project": "ungrouped", "projects": [
            {"id": "alpha", "name": "Alpha", "color": "#3fb950", "sessions": alpha},
            {"id": "beta", "name": "Beta", "color": "", "accessible": false, "sessions": beta},
            {"id": "ungrouped", "name": "Inbox", "color": "#8d9baa", "sessions": []}]})
    }

    fn present(grouping: &mut Grouping, org: &Value, revision: u64) {
        grouping.present(org, Arc::new(members(org)), revision);
    }

    #[test]
    fn choosing_a_local_default_moves_only_implicit_rows_and_survives_restart() {
        let mut grouping = Grouping::default();
        let org = organization(&["workstation\nclaude/p/explicit"], &[]);
        for name in ["explicit", "implicit"] {
            grouping.observe(&upsert(name, "workstation", &format!("claude/p/{name}")));
        }
        present(&mut grouping, &org, 1);
        grouping.take();
        assert!(grouping.set_default_project("gone").is_err());
        assert!(
            grouping.set_default_project("beta").is_err(),
            "inaccessible project"
        );
        grouping.set_default_project("alpha").unwrap();
        let (rows, view) = grouping.take();
        assert_eq!(view.unwrap().default_project, "alpha");
        assert!(rows
            .iter()
            .any(|r| r.key == "implicit" && r.group == "alpha"));
        grouping.set_default_project("ungrouped").unwrap();
        let (rows, _) = grouping.take();
        assert!(rows
            .iter()
            .any(|r| r.key == "implicit" && r.group == "ungrouped"));
        assert!(!rows
            .iter()
            .any(|r| r.key == "explicit" && r.group != "alpha"));
        grouping.set_default_project("alpha").unwrap();
        let mut restored = Grouping::new(Layout::from_json(&grouping.take_layout().unwrap()));
        present(&mut restored, &org, 1);
        assert_eq!(restored.default_project(), "alpha");
        assert_eq!(
            org["default_project"], "ungrouped",
            "shared catalog untouched"
        );
    }

    #[test]
    fn a_notice_reaches_the_headers_json_only_while_set() {
        let mut grouping = Grouping::default();
        let (_, out) = grouping.take();
        let json = serde_json::to_value(out.unwrap()).unwrap();
        assert!(json.get("notice").is_none(), "absent when there is none");
        assert!(json.get("layout_save_paused").is_none());
        let legacy: GroupingOut = serde_json::from_value(json).unwrap();
        assert!(!legacy.sync.layout_save_paused);
        grouping.set_sync(SyncView {
            notice: Some("The saved project changes were not applied.".into()),
            layout_save_paused: true,
            ..SyncView::default()
        });
        let (_, out) = grouping.take();
        let json = serde_json::to_value(out.unwrap()).unwrap();
        assert_eq!(
            json["notice"],
            "The saved project changes were not applied."
        );
        assert_eq!(json["layout_save_paused"], true);
        let back: GroupingOut = serde_json::from_value(json).unwrap();
        assert!(back.sync.layout_save_paused);
        assert_eq!(
            back.sync.notice.as_deref(),
            Some("The saved project changes were not applied.")
        );
    }

    fn assigned(groups: &[GroupOut]) -> Vec<(&str, &str)> {
        groups
            .iter()
            .map(|g| (g.key.as_str(), g.group.as_str()))
            .collect()
    }

    #[test]
    fn rows_join_their_project_or_the_default_and_are_sent_when_that_changes() {
        let mut grouping = Grouping::new(Layout::default());
        grouping.observe(&upsert(
            "run:1",
            "workstation",
            "claude/sample-project/review",
        ));
        grouping.observe(&upsert("run:2", "workstation", "kimi/docs-site/landing"));
        let (groups, out) = grouping.take();
        assert!(groups.is_empty(), "before the catalog nothing is placed");
        assert!(!out.unwrap().loaded);
        present(
            &mut grouping,
            &organization(&["workstation\nclaude/sample-project/review"], &[]),
            1,
        );
        let (groups, out) = grouping.take();
        assert_eq!(
            assigned(&groups),
            [("run:1", "alpha"), ("run:2", "ungrouped")],
            "the first placement of both rows"
        );
        let out = out.unwrap();
        assert!(out.loaded);
        assert_eq!(
            out.headers
                .iter()
                .map(|h| h.id.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta", "ungrouped"]
        );
        assert_eq!(
            (
                out.headers[1].color.clone(),
                out.headers[1].accessible,
                out.headers[1].palette
            ),
            (None, false, 1)
        );
        assert!(out.headers[2].is_default);
        grouping.observe(&upsert("run:2", "workstation", "kimi/docs-site/landing"));
        assert_eq!(
            grouping.take(),
            (vec![], None),
            "an unchanged row sends nothing"
        );
        grouping.observe(&Change::Remove {
            computer: ComputerId::local(),
            key: "run:2".into(),
            reason: None,
        });
        grouping.observe(&upsert("run:2", "workstation", "kimi/docs-site/landing"));
        assert_eq!(
            assigned(&grouping.take().0),
            [("run:2", "ungrouped")],
            "a returning row is sent again"
        );
    }

    #[test]
    fn ranks_keep_the_catalog_order_then_arrivals_and_collapsing_sends_the_headers() {
        let mut grouping = Grouping::new(Layout::default());
        for (key, name) in [
            ("run:1", "claude/p/one"),
            ("run:2", "claude/p/two"),
            ("run:3", "claude/p/three"),
            ("run:4", "claude/p/four"),
            ("run:5", "claude/p/five"),
        ] {
            grouping.observe(&upsert(key, "workstation", name));
        }
        present(
            &mut grouping,
            &organization(
                &["workstation\nclaude/p/three", "workstation\nclaude/p/one"],
                &[],
            ),
            1,
        );
        let (mut groups, _) = grouping.take();
        groups.sort_by_key(|g| g.rank);
        let keys = |project: &str| {
            groups
                .iter()
                .filter(|g| g.group == project)
                .map(|g| g.key.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(keys("alpha"), ["run:3", "run:1"], "the catalog's order");
        assert_eq!(
            keys("ungrouped"),
            ["run:2", "run:4", "run:5"],
            "the rows no project lists stay in the Inbox, in arrival order"
        );
        grouping.set_collapsed("alpha", true);
        let (groups, out) = grouping.take();
        assert!(groups.is_empty());
        assert!(out.unwrap().headers[0].collapsed);
        assert!(grouping.take_layout().is_some());
    }

    #[test]
    fn no_projects_give_qt_s_general_project() {
        let mut grouping = Grouping::new(Layout::default());
        grouping.present(
            &json!({"default_project": "ungrouped", "projects": []}),
            Arc::default(),
            1,
        );
        let out = grouping.take().1.unwrap();
        assert_eq!(out.headers.len(), 1);
        assert_eq!(
            (out.headers[0].id.as_str(), out.headers[0].name.as_str()),
            ("ungrouped", "General")
        );
        assert_eq!(out.default_project, "ungrouped");
    }

    // SessionOrganization::observe, 223-231: a run last seen before exit is
    // still the same session after an unseen rename.
    #[test]
    fn a_persisted_run_follows_an_unseen_rename_once_and_keeps_its_rank() {
        let old = "workstation\nclaude/p/old";
        let new = "workstation\nclaude/p/new";
        let org = organization(&["workstation\nclaude/p/first", old], &[]);
        let mut first = Grouping::default();
        present(&mut first, &org, 1);
        first.observe(&upsert("run:one", "workstation", "claude/p/old"));
        let rank = first.layout.rank("alpha", old);
        let layout = first.take_layout().unwrap();
        let runs = first
            .take_runs()
            .expect("a live run is remembered separately")
            .to_json();
        assert!(first.take_runs().is_none(), "only a change needs saving");

        for rows_first in [false, true] {
            let mut next = Grouping::new(Layout::from_json(&layout));
            if rows_first {
                next.observe(&upsert("run:one", "workstation", "claude/p/new"));
            }
            next.replace_runs(super::super::runs::Runs::from_json(&runs).unwrap());
            present(&mut next, &org, 1);
            next.observe(&upsert("run:one", "workstation", "claude/p/new"));
            assert_eq!(next.take_renames(), [(old.to_owned(), new.to_owned())]);
            assert!(next.take_renames().is_empty());
            let (groups, _) = next.take();
            assert_eq!(groups[0].group, "alpha");
            assert_eq!(groups[0].rank, rank);
            next.observe(&upsert("run:one", "workstation", "claude/p/new"));
            assert!(
                next.take_renames().is_empty(),
                "never migrate the same token twice"
            );
        }
    }

    #[test]
    fn presenting_a_pending_rename_does_not_reintroduce_the_old_rank() {
        let old = "workstation\nclaude/p/old";
        let new = "workstation\nclaude/p/new";
        let mut grouping = Grouping::default();
        grouping.observe(&upsert("run:one", "workstation", "claude/p/old"));
        grouping.observe(&upsert("run:two", "workstation", "claude/p/old"));
        let org = organization(&[old], &[]);
        present(&mut grouping, &org, 1);
        grouping.observe(&upsert("run:one", "workstation", "claude/p/new"));
        // A historical token must not leave an old-name live row that later
        // moves the mistakenly recreated rank into the fallback project.
        grouping.observe(&Change::Remove {
            computer: ComputerId::local(),
            key: "run:two".into(),
            reason: None,
        });
        present(&mut grouping, &org, 2);
        let saved: Value = serde_json::from_slice(&grouping.take_layout().unwrap()).unwrap();
        assert_eq!(saved["order"]["alpha"], serde_json::json!([new]));
        assert_eq!(grouping.take_renames(), [(old.to_owned(), new.to_owned())]);
        // Qt moves every historical run of the old member. The grouping's
        // runs.rename call must also suppress an intent from the second run.
        grouping.observe(&upsert("run:two", "workstation", "claude/p/new"));
        assert!(grouping.take_renames().is_empty());
        grouping.acknowledge_rename(old, new);
        let mut loaded =
            super::super::runs::Runs::from_json(&grouping.take_runs().unwrap().to_json()).unwrap();
        assert_eq!(
            loaded.observe("workstation", "two", "later"),
            Some(new.into())
        );
    }

    #[test]
    fn persisted_runs_exclude_archives_empty_identities_and_other_machines() {
        let mut first = Grouping::default();
        for (key, machine, name) in [
            ("archive:one", "workstation", "archived"),
            ("run:", "workstation", "empty-run"),
            ("run:unknown", "", "unknown-machine"),
            ("run:empty-name", "workstation", ""),
            ("name:plain", "workstation", "plain"),
        ] {
            first.observe(&upsert(key, machine, name));
        }
        assert!(
            first.take_runs().is_none(),
            "only named live runs belong here"
        );
        first.observe(&upsert("run:one", "workstation", "claude/p/old"));
        let bytes = first.take_runs().unwrap().to_json();
        let mut next = Grouping::default();
        next.replace_runs(super::super::runs::Runs::from_json(&bytes).unwrap());
        present(&mut next, &organization(&[], &[]), 1);
        next.observe(&upsert("run:one", "studio", "claude/p/new"));
        next.observe(&upsert("archive:one", "workstation", "claude/p/new"));
        assert!(
            next.take_renames().is_empty(),
            "tokens are machine scoped and live only"
        );
    }

    #[test]
    fn a_layout_toggle_never_dirties_the_run_tokens() {
        let mut grouping = Grouping::default();
        present(&mut grouping, &organization(&[], &[]), 1);
        grouping.observe(&upsert("run:one", "workstation", "claude/p/old"));
        assert!(grouping.take_runs().is_some());
        grouping.take_layout();
        grouping.set_collapsed("alpha", true);
        grouping
            .layout
            .place("alpha", &["workstation\nclaude/p/old".into()], None);
        assert!(grouping.take_layout().is_some());
        assert!(grouping.take_runs().is_none());
    }

    #[test]
    fn a_rename_under_the_same_run_is_reported_once_and_keeps_its_place() {
        let mut grouping = Grouping::new(Layout::default());
        present(&mut grouping, &organization(&[], &[]), 1);
        grouping.observe(&upsert("run:0", "workstation", "claude/p/first"));
        grouping.observe(&upsert("run:1", "workstation", "claude/p/old"));
        grouping.observe(&upsert("run:9", "workstation", "claude/p/last"));
        let rank_of =
            |groups: &[GroupOut], key: &str| groups.iter().find(|g| g.key == key).map(|g| g.rank);
        let (before, _) = grouping.take();
        let (first, old, last) = (
            rank_of(&before, "run:0").unwrap(),
            rank_of(&before, "run:1").unwrap(),
            rank_of(&before, "run:9").unwrap(),
        );
        assert!(first < old && old < last);
        grouping.observe(&upsert("name:x", "workstation", "sh/p"));
        grouping.observe(&upsert("run:1", "workstation", "claude/p/new"));
        grouping.observe(&upsert("name:x", "workstation", "sh/q"));
        assert_eq!(
            grouping.take_renames(),
            [(
                "workstation\nclaude/p/old".into(),
                "workstation\nclaude/p/new".into()
            )],
            "the hub checks that the fallback needs no edit before acknowledging"
        );
        let (after, _) = grouping.take();
        assert_eq!(
            rank_of(&after, "run:1"),
            None,
            "the renamed row keeps its place: nothing to send"
        );
        grouping.resend();
        let (all, _) = grouping.take();
        assert_eq!(
            rank_of(&all, "run:1"),
            Some(old),
            "the new member holds the old member's rank"
        );
        assert_eq!(
            grouping
                .layout
                .rank("ungrouped", "workstation\nclaude/p/new"),
            old
        );
        grouping.observe(&upsert("run:2", "", "claude/p/early"));
        grouping.observe(&upsert("run:2", "workstation", "claude/p/early"));
        assert!(
            grouping.take_renames().is_empty(),
            "no machine yet: no rename"
        );
    }

    #[test]
    fn renames_for_the_hub_include_fallbacks_for_the_authoritative_check() {
        let mut grouping = Grouping::new(Layout::default());
        // Before the first catalog no member is known: each rename waits for the hub.
        grouping.observe(&upsert("run:1", "workstation", "claude/p/a"));
        grouping.observe(&upsert("run:1", "workstation", "claude/p/a2"));
        assert_eq!(
            grouping.take_renames(),
            [(
                "workstation\nclaude/p/a".to_owned(),
                "workstation\nclaude/p/a2".to_owned()
            )]
        );
        present(
            &mut grouping,
            &organization(&["workstation\nclaude/p/b"], &[]),
            1,
        );
        grouping.observe(&upsert("run:2", "workstation", "claude/p/b"));
        grouping.observe(&upsert("run:3", "workstation", "claude/p/c"));
        grouping.observe(&upsert("run:2", "workstation", "claude/p/b2"));
        grouping.observe(&upsert("run:3", "workstation", "claude/p/c2"));
        assert_eq!(
            grouping.take_renames(),
            [
                (
                    "workstation\nclaude/p/b".to_owned(),
                    "workstation\nclaude/p/b2".to_owned()
                ),
                (
                    "workstation\nclaude/p/c".to_owned(),
                    "workstation\nclaude/p/c2".to_owned()
                )
            ],
            "the hub decides which rename needs an edit"
        );
    }

    // Never drained while edits are refused (an uninitialized catalog, an
    // ended topic, a stopped worker): the oldest go first.
    #[test]
    fn renames_waiting_for_the_hub_keep_at_most_the_newest_max_renames() {
        let mut grouping = Grouping::new(Layout::default());
        for n in 0..=MAX_RENAMES {
            let key = format!("run:{n}");
            grouping.observe(&upsert(&key, "workstation", &format!("claude/p/{n}")));
            grouping.observe(&upsert(&key, "workstation", &format!("claude/p/{n}-2")));
        }
        let renames = grouping.take_renames();
        assert_eq!(renames.len(), MAX_RENAMES);
        assert_eq!(renames[0].0, "workstation\nclaude/p/1", "the oldest went");
        assert_eq!(
            renames.last().unwrap().1,
            format!("workstation\nclaude/p/{MAX_RENAMES}-2")
        );
    }

    #[test]
    fn edits_reorder_locally_and_delete_moves_the_default() {
        let mut grouping = Grouping::new(Layout::default());
        for (key, name) in [("run:1", "claude/p/b"), ("run:2", "claude/p/a")] {
            grouping.observe(&upsert(key, "workstation", name));
        }
        let org = organization(&[], &[]);
        present(&mut grouping, &org, 1);
        let _ = grouping.take();
        let members = by_name(grouping.members_of("ungrouped"));
        assert_eq!(
            members,
            ["workstation\nclaude/p/a", "workstation\nclaude/p/b"]
        );
        grouping.edited(&Intent::Sort {
            project: "ungrouped".into(),
            members: members.clone(),
        });
        present(&mut grouping, &org, 2);
        let mut groups = grouping.take().0;
        groups.sort_by_key(|g| g.rank);
        assert_eq!(
            groups.iter().map(|g| g.key.as_str()).collect::<Vec<_>>(),
            ["run:2", "run:1"]
        );
        grouping.edited(&Intent::Delete {
            project: "ungrouped".into(),
            destination: "alpha".into(),
            members: vec![],
        });
        assert_eq!(
            grouping.layout.default_project(),
            Some("alpha"),
            "removeGroup: the receiver becomes the default"
        );
    }

    #[test]
    fn the_saved_order_survives_rows_arriving_before_the_catalog() {
        let (a, b) = ("workstation\nclaude/p/a", "workstation\nclaude/p/b");
        let mut saved = Layout::default();
        saved.place("alpha", &[b.to_owned(), a.to_owned()], None);
        let mut grouping = Grouping::new(saved);
        grouping.observe(&upsert("run:1", "workstation", "claude/p/a"));
        grouping.observe(&upsert("run:2", "workstation", "claude/p/b"));
        let (groups, out) = grouping.take();
        assert!(groups.is_empty());
        assert!(!out.unwrap().loaded);
        present(&mut grouping, &organization(&[a, b], &[]), 1);
        let (mut groups, _) = grouping.take();
        groups.sort_by_key(|g| g.rank);
        assert_eq!(
            groups
                .iter()
                .map(|g| (g.key.as_str(), g.group.as_str()))
                .collect::<Vec<_>>(),
            [("run:2", "alpha"), ("run:1", "alpha")]
        );
        assert_eq!(
            grouping
                .members_of("alpha")
                .iter()
                .map(|m| m.0.as_str())
                .collect::<Vec<_>>(),
            [b, a]
        );
    }

    #[test]
    fn a_rename_inside_a_listed_project_keeps_the_project_and_the_place() {
        let member = |name: &str| format!("workstation\nclaude/p/{name}");
        let mut grouping = Grouping::new(Layout::default());
        for (key, name) in [("run:1", "x"), ("run:2", "old"), ("run:3", "z")] {
            grouping.observe(&upsert(key, "workstation", &format!("claude/p/{name}")));
        }
        let org = organization(&[&member("x"), &member("old"), &member("z")], &[]);
        present(&mut grouping, &org, 1);
        let (before, _) = grouping.take();
        let rank =
            |groups: &[GroupOut], key: &str| groups.iter().find(|g| g.key == key).unwrap().rank;
        let old = rank(&before, "run:2");
        grouping.observe(&upsert("run:2", "workstation", "claude/p/new"));
        let (after, _) = grouping.take();
        assert!(after.is_empty(), "no hop to the default project: {after:?}");
        let members = grouping.members_of("alpha");
        assert_eq!(
            members.iter().map(|m| m.0.clone()).collect::<Vec<_>>(),
            [member("x"), member("new"), member("z")]
        );
        grouping.resend();
        let (all, _) = grouping.take();
        assert_eq!(rank(&all, "run:2"), old);
        assert_eq!(
            all.iter().find(|g| g.key == "run:2").unwrap().group,
            "alpha"
        );
    }

    #[test]
    fn rows_that_move_together_keep_their_arrival_order() {
        let mut grouping = Grouping::new(Layout::default());
        for (key, name) in [("run:c", "c"), ("run:a", "a"), ("run:b", "b")] {
            grouping.observe(&upsert(key, "workstation", &format!("claude/p/{name}")));
        }
        let org = |default: &str| {
            json!({"default_project": default, "projects": [
                {"id": "one", "name": "One", "sessions": []},
                {"id": "two", "name": "Two", "sessions": []}]})
        };
        present(&mut grouping, &org("one"), 1);
        let _ = grouping.take();
        present(&mut grouping, &org("two"), 2);
        let (mut groups, _) = grouping.take();
        groups.sort_by_key(|g| g.rank);
        assert_eq!(
            groups
                .iter()
                .map(|g| (g.key.as_str(), g.group.as_str()))
                .collect::<Vec<_>>(),
            [("run:c", "two"), ("run:a", "two"), ("run:b", "two")]
        );
    }

    #[test]
    fn a_layout_grown_past_the_bound_is_pruned_by_row_changes() {
        let rows = |tag: &str| Change::Replace {
            computer: ComputerId::local(),
            rows: (0..super::super::layout::MAX_MEMBERS / 2 + 600)
                .map(|n| {
                    (
                        format!("run:{tag}{n}"),
                        row("workstation", &format!("claude/{tag}/s{n}")),
                    )
                })
                .collect(),
        };
        let mut grouping = Grouping::new(Layout::default());
        present(&mut grouping, &organization(&[], &[]), 1);
        grouping.observe(&rows("a"));
        grouping.observe(&Change::Replace {
            computer: ComputerId::local(),
            rows: vec![],
        });
        let kept = super::super::layout::MAX_MEMBERS / 2 + 600;
        assert_eq!(
            grouping.layout.len(),
            kept,
            "below the bound: nothing pruned yet"
        );
        grouping.observe(&rows("b"));
        assert_eq!(
            grouping.layout.len(),
            kept,
            "only the observe path can have dropped the first rows"
        );
    }

    #[test]
    fn a_resend_before_the_catalog_sends_no_placeholders() {
        let mut grouping = Grouping::new(Layout::default());
        for n in 0..3 {
            grouping.observe(&upsert(
                &format!("run:{n}"),
                "workstation",
                &format!("claude/p/{n}"),
            ));
        }
        grouping.resend();
        assert!(grouping.take().0.is_empty());
        present(&mut grouping, &organization(&[], &[]), 1);
        let (groups, _) = grouping.take();
        assert_eq!(groups.len(), 3);
        assert!(groups.iter().all(|g| g.group == "ungrouped"));
        assert!(grouping.take().0.is_empty(), "each row once");
    }

    #[test]
    fn a_rename_onto_a_stale_member_takes_the_old_project_and_place() {
        let member = |name: &str| format!("workstation\nclaude/p/{name}");
        let mut grouping = Grouping::new(Layout::default());
        for (key, name) in [("run:1", "x"), ("run:2", "old"), ("run:3", "z")] {
            grouping.observe(&upsert(key, "workstation", &format!("claude/p/{name}")));
        }
        let org = organization(
            &[&member("x"), &member("old"), &member("z")],
            &[&member("new")],
        );
        present(&mut grouping, &org, 1);
        let _ = grouping.take();
        grouping.observe(&upsert("run:2", "workstation", "claude/p/new"));
        assert_eq!(
            grouping
                .members_of("alpha")
                .iter()
                .map(|m| m.0.clone())
                .collect::<Vec<_>>(),
            [member("x"), member("new"), member("z")]
        );
        assert!(grouping.members_of("beta").is_empty());
    }

    #[test]
    fn headers_are_compared_only_after_something_could_change_them() {
        let mut grouping = Grouping::new(Layout::default());
        present(&mut grouping, &organization(&[], &[]), 1);
        assert!(grouping.take().1.is_some());
        grouping.observe(&upsert("run:1", "workstation", "claude/p/a"));
        assert!(grouping.take().1.is_none());
        grouping.set_collapsed("alpha", true);
        assert!(grouping.take().1.is_some());
        grouping.resend();
        assert!(grouping.take().1.is_some());
    }

    #[test]
    fn sort_by_name_folds_case_then_keeps_the_written_form_then_the_member() {
        let rows = vec![
            ("m3".into(), "beta".into()),
            ("m1".into(), "Alpha".into()),
            ("m2".into(), "alpha".into()),
            ("m0".into(), "alpha".into()),
        ];
        assert_eq!(by_name(rows), ["m1", "m0", "m2", "m3"]);
    }

    // Session counts reach tens of thousands: one pass groups them, and a
    // move sends only the row it moved.
    #[test]
    fn twenty_thousand_sessions_group_in_one_pass_and_a_move_sends_one_row() {
        let names: Vec<String> = (0..20_000)
            .map(|n| format!("claude/sample-project/s{n:05}"))
            .collect();
        let mut grouping = Grouping::new(Layout::default());
        let started = Instant::now();
        grouping.observe(&Change::Replace {
            computer: ComputerId::local(),
            rows: names
                .iter()
                .enumerate()
                .map(|(n, name)| (format!("run:{n}"), row("workstation", name)))
                .collect(),
        });
        let catalog = |moved: bool| {
            let mut alpha: Vec<String> = names[..10_000]
                .iter()
                .map(|n| crate::swarm::member("workstation", n))
                .collect();
            let mut beta: Vec<String> = names[10_000..]
                .iter()
                .map(|n| crate::swarm::member("workstation", n))
                .collect();
            if moved {
                beta.push(alpha.remove(0));
            }
            json!({"default_project": "ungrouped", "projects": [
                {"id": "alpha", "name": "Alpha", "sessions": alpha},
                {"id": "beta", "name": "Beta", "sessions": beta},
                {"id": "ungrouped", "name": "Inbox", "sessions": []}]})
        };
        let first = catalog(false);
        present(&mut grouping, &first, 1);
        let (groups, headers) = grouping.take();
        assert_eq!(groups.len(), 20_000);
        assert!(headers.is_some());
        let bytes = serde_json::to_vec(&groups).unwrap().len();
        assert!(bytes < 3 << 20, "{bytes} bytes for 20,000 rows");
        let moved = catalog(true);
        present(&mut grouping, &moved, 2);
        let (groups, headers) = grouping.take();
        assert_eq!(groups.len(), 1, "only the moved row");
        assert_eq!(groups[0].group, "beta");
        assert!(headers.is_none(), "the headers did not change");
        eprintln!(
            "grouping: 20,000 rows, two catalogs in {:?}",
            started.elapsed()
        );
    }

    /// Timing check for the plan's measurement task: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn twenty_thousand_sessions_group_within_five_seconds() {
        let names: Vec<String> = (0..20_000)
            .map(|n| format!("claude/sample-project/s{n:05}"))
            .collect();
        let mut grouping = Grouping::new(Layout::default());
        let started = Instant::now();
        grouping.observe(&Change::Replace {
            computer: ComputerId::local(),
            rows: names
                .iter()
                .enumerate()
                .map(|(n, name)| (format!("run:{n}"), row("workstation", name)))
                .collect(),
        });
        let sessions: Vec<String> = names
            .iter()
            .map(|n| crate::swarm::member("workstation", n))
            .collect();
        let first = json!({"default_project": "ungrouped", "projects": [
            {"id": "alpha", "name": "Alpha", "sessions": &sessions[..10_000]},
            {"id": "beta", "name": "Beta", "sessions": &sessions[10_000..]},
            {"id": "ungrouped", "name": "Inbox", "sessions": []}]});
        present(&mut grouping, &first, 1);
        assert_eq!(grouping.take().0.len(), 20_000);
        let took = started.elapsed();
        eprintln!("grouping: 20,000 rows in {took:?}");
        assert!(
            took < Duration::from_secs(5),
            "one bounded pass per catalog"
        );
    }
}
