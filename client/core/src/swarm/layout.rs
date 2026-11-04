//! This device's layout of the Sessions list: collapsed projects, the order
//! of sessions inside each project and the default project. Qt keeps them in
//! its local organization (`workspace/organization`, SessionsWindow.cpp:
//! 2486-2500; collapsed, SwarmController.cpp:141; order, 148-150; default,
//! 188) and never shares them. The Tauri side saves the JSON.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, Read};
use std::path::Path;

mod presentation;
pub mod windows_folders;
mod windows_import;
pub use presentation::{qt_presentation, PresentationSeed};
pub use windows_import::{qt_windows_folders, WindowsFoldersContext, WindowsFoldersSeed};

/// The space between neighbouring ranks; an insert takes the middle.
pub const GAP: u64 = 1 << 20;
/// The most members the layout remembers.
pub const MAX_MEMBERS: usize = 100_000;
/// Run-to-project memory (Qt's `run_groups`, SessionOrganization.cpp:234-243).
/// Qt has no eviction; the client keeps the newest 20,000 writes.
pub const MAX_RUNS: usize = 20_000;
/// The most bytes read from the layout file, as from the draft
/// (`super::draft::MAX_BYTES`): far above what [`MAX_MEMBERS`] members take.
pub const MAX_BYTES: u64 = super::draft::MAX_BYTES;

/// The saved layout file's bytes, read at most [`MAX_BYTES`] as
/// `super::draft::load` reads the draft; `None` when there is none. A larger
/// file is an error and is never read whole.
pub fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    read_at_most(path, MAX_BYTES)
}

fn read_at_most(path: &Path, limit: u64) -> io::Result<Option<Vec<u8>>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the layout file is larger than {limit} bytes"),
        ));
    }
    Ok(Some(bytes))
}

/// Seeds only Qt's run-to-project memory, never its session-name tokens or
/// shared catalog (SessionOrganization.cpp:60,336). Call on a startup worker
/// for the switch identity only. An unreadable store or malformed JSON leaves
/// the seed pending; neither this function nor QtStore writes Qt's settings.
pub fn seed_runs_from_qt(
    layout: &mut Layout,
    store: &crate::settings::QtStore,
) -> Result<usize, String> {
    if layout.runs_seeded() {
        return Ok(0);
    }
    Ok(layout.seed_runs(&qt_run_groups(store)?))
}

/// Reads and parses Qt's run memory off the main thread and outside the
/// grouping lock. Only the newest [`MAX_RUNS`] valid entries cross that lock.
/// The order is Qt's object order (Qt stores no per-entry write timestamps).
pub fn qt_run_groups(
    store: &crate::settings::QtStore,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let values = store.read()?;
    let organization = values.and_then(|(mut values, _)| values.remove("workspace/organization"));
    let bytes = match organization {
        Some(crate::settings::qt::QtValue::Bytes(bytes)) => Some(bytes),
        Some(value) => Some(value.to_text().into_bytes()),
        // Qt moved it out of its store into a file of its own (owner,
        // 2026-10-30, tray/src/SettingsJson.cpp).
        None => read_at_most(&store.json_file("organization"), MAX_BYTES).map_err(|error| error.to_string())?,
    };
    let mut organization: serde_json::Map<String, serde_json::Value> = match bytes {
        Some(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string())?,
        None => serde_json::Map::new(),
    };
    let groups = match organization.remove("run_groups") {
        Some(serde_json::Value::Object(groups)) => groups,
        _ => serde_json::Map::new(),
    };
    let mut newest: Vec<_> = groups
        .into_iter()
        .rev()
        .filter(|(token, project)| {
            let Some((machine, run)) = token.split_once('\n') else {
                return false;
            };
            valid_part(machine) && valid_part(run) && project.as_str().is_some_and(valid_part)
        })
        .take(MAX_RUNS)
        .collect();
    // Preserve iteration order even when serde_json uses preserve_order.
    newest.reverse();
    Ok(newest.into_iter().collect())
}

fn valid_part(part: &str) -> bool {
    !part.trim().is_empty() && !part.chars().any(char::is_control)
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    version: u32,
    collapsed: Vec<String>,
    default_project: Option<String>,
    /// Each project's members in list order.
    order: BTreeMap<String, Vec<String>>,
    /// Oldest written first, so eviction order survives a restart.
    run_groups: Vec<(String, String)>,
    runs_seeded: bool,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    collapsed: BTreeSet<String>,
    default_project: Option<String>,
    order: HashMap<String, BTreeMap<u64, String>>,
    index: HashMap<String, (String, u64)>,
    dirty: bool,
    /// Counts renumberings, so `place` can tell a no-op from a move.
    renumbers: u64,
    runs: HashMap<String, (String, u64)>,
    run_order: BTreeMap<u64, String>,
    run_written: u64,
    runs_seeded: bool,
    extra: serde_json::Map<String, serde_json::Value>,
}

impl Layout {
    /// Compatibility projection only. File owners must use `try_from_json`
    /// and retain the original on refusal before enabling their writer.
    pub fn from_json(bytes: &[u8]) -> Self {
        let saved: Saved = serde_json::from_slice(bytes).unwrap_or_default();
        Self::from_saved(saved)
    }

    /// Bounded supported layout, without silently dropping malformed/newer
    /// fields or entries beyond the store's limits. Unknown v1 extensions
    /// round-trip unchanged. A refusal never authorizes an empty replacement.
    pub fn try_from_json(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err("the project layout exceeds its byte limit");
        }
        if bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
            return Err("the project layout is not an object");
        }
        let saved: Saved =
            serde_json::from_slice(bytes).map_err(|_| "the project layout is malformed")?;
        // Version zero is the legacy unversioned shape's serde default.
        if ![0, 1].contains(&saved.version) {
            return Err("the project layout has an unsupported version");
        }
        if saved.collapsed.len() > super::MAX_PROJECTS
            || saved.order.len() > MAX_MEMBERS
            || saved.order.values().map(Vec::len).sum::<usize>() > MAX_MEMBERS
            || saved.run_groups.len() > MAX_RUNS
        {
            return Err("the project layout exceeds its entry limits");
        }
        let mut members = BTreeSet::new();
        if saved
            .order
            .values()
            .flatten()
            .any(|member| !members.insert(member))
        {
            return Err("the project layout repeats a member identity");
        }
        let mut runs = BTreeSet::new();
        for (token, project) in &saved.run_groups {
            let Some((machine, run)) = token.split_once('\n') else {
                return Err("the project layout has an invalid run identity");
            };
            if !valid_part(machine)
                || !valid_part(run)
                || !valid_part(project)
                || !runs.insert(token)
            {
                return Err("the project layout has an invalid run identity");
            }
        }
        if let Some(family) = saved.extra.get(windows_folders::KEY) {
            windows_folders::validate(family)?;
        }
        Ok(Self::from_saved(saved))
    }

    fn from_saved(saved: Saved) -> Self {
        let mut layout = Self {
            collapsed: saved
                .collapsed
                .into_iter()
                .take(super::MAX_PROJECTS)
                .collect(),
            default_project: saved.default_project,
            runs_seeded: saved.runs_seeded,
            extra: saved.extra,
            ..Self::default()
        };
        'load: for (project, members) in saved.order {
            for member in members {
                if layout.index.len() >= MAX_MEMBERS {
                    break 'load;
                }
                layout.append(&project, &member);
            }
        }
        for (token, project) in saved.run_groups {
            if let Some((machine, run)) = token.split_once('\n') {
                layout.remember_run(machine, run, Some(&project));
            }
        }
        layout.dirty = false;
        layout
    }

    pub fn to_json(&self) -> Vec<u8> {
        let order = self
            .order
            .iter()
            .map(|(project, ranks)| (project.clone(), ranks.values().cloned().collect()))
            .collect();
        let saved = Saved {
            version: 1,
            collapsed: self.collapsed.iter().cloned().collect(),
            default_project: self.default_project.clone(),
            order,
            run_groups: self
                .run_order
                .values()
                .filter_map(|token| {
                    let (project, _) = self.runs.get(token)?;
                    Some((token.clone(), project.clone()))
                })
                .collect(),
            runs_seeded: self.runs_seeded,
            extra: self.extra.clone(),
        };
        serde_json::to_vec(&saved).expect("the layout serializes")
    }

    /// The layout to save, once after each change.
    pub fn take_dirty(&mut self) -> Option<Vec<u8>> {
        std::mem::take(&mut self.dirty).then(|| self.to_json())
    }

    /// The last explicit project of this run (SessionOrganization.cpp:234).
    pub fn run_project(&self, machine: &str, run: &str) -> Option<&str> {
        self.runs
            .get(&format!("{machine}\n{run}"))
            .map(|(project, _)| project.as_str())
    }

    /// Some records an explicit assignment as the newest write; None forgets
    /// an implicit assignment (Qt 239-243). Observers compare the old project
    /// first, as Qt does, so unchanged live rows do not refresh its age.
    pub fn remember_run(&mut self, machine: &str, run: &str, project: Option<&str>) -> bool {
        if !valid_part(machine)
            || !valid_part(run)
            || project.is_some_and(|project| !valid_part(project))
        {
            return false;
        }
        let token = format!("{machine}\n{run}");
        if let Some(project) = project {
            if self.run_project(machine, run) == Some(project)
                && self
                    .run_order
                    .last_key_value()
                    .is_some_and(|(_, last)| last == &token)
            {
                return false;
            }
            if let Some((_, written)) = self.runs.remove(&token) {
                self.run_order.remove(&written);
            }
            if self.run_written == u64::MAX {
                // A renumber keeps the bounded map's relative write order.
                let tokens = std::mem::take(&mut self.run_order).into_values();
                for (at, token) in tokens.enumerate() {
                    let written = at as u64 + 1;
                    self.runs.get_mut(&token).unwrap().1 = written;
                    self.run_order.insert(written, token);
                }
                self.run_written = self.run_order.len() as u64;
            }
            self.run_written += 1;
            self.run_order.insert(self.run_written, token.clone());
            self.runs.insert(token, (project.into(), self.run_written));
            while self.runs.len() > MAX_RUNS {
                let (_, oldest) = self.run_order.pop_first().unwrap();
                self.runs.remove(&oldest);
            }
        } else if let Some((_, written)) = self.runs.remove(&token) {
            self.run_order.remove(&written);
        } else {
            return false;
        }
        self.dirty = true;
        true
    }

    pub fn runs_seeded(&self) -> bool {
        self.runs_seeded
    }

    /// Imports Qt's machine\nrun -> project object once, read-only (Qt 60,
    /// 336). Qt has no write timestamps: later object entries are imported
    /// last. Native writes already made by this client remain newer and win.
    /// Returns the number of imported entries retained after the fixed cap.
    pub fn seed_runs(
        &mut self,
        qt_run_groups: &serde_json::Map<String, serde_json::Value>,
    ) -> usize {
        if self.runs_seeded {
            return 0;
        }
        let native = std::mem::take(&mut self.runs);
        let order = std::mem::take(&mut self.run_order);
        self.run_written = 0;
        for (token, project) in qt_run_groups {
            if let Some((machine, run)) = token.split_once('\n') {
                if let Some(project) = project.as_str() {
                    self.remember_run(machine, run, Some(project));
                }
            }
        }
        for token in order.into_values() {
            let (project, _) = &native[&token];
            let (machine, run) = token.split_once('\n').unwrap();
            self.remember_run(machine, run, Some(project));
        }
        self.runs_seeded = true;
        self.dirty = true;
        self.runs
            .keys()
            .filter(|token| !native.contains_key(*token))
            .count()
    }

    pub fn collapsed(&self, project: &str) -> bool {
        self.collapsed.contains(project)
    }

    pub fn set_collapsed(&mut self, project: &str, collapsed: bool) -> bool {
        let changed = if collapsed {
            self.collapsed.insert(project.to_owned())
        } else {
            self.collapsed.remove(project)
        };
        self.dirty |= changed;
        changed
    }

    pub fn default_project(&self) -> Option<&str> {
        self.default_project.as_deref()
    }

    pub fn set_default_project(&mut self, project: Option<String>) {
        if self.default_project != project {
            self.default_project = project;
            self.dirty = true;
        }
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// The member's rank in `project`: where it is, else last.
    pub fn rank(&mut self, project: &str, member: &str) -> u64 {
        if let Some((owner, rank)) = self.index.get(member) {
            if owner == project {
                return *rank;
            }
        }
        self.append(project, member)
    }

    fn remove(&mut self, member: &str) {
        if let Some((project, rank)) = self.index.remove(member) {
            if let Some(ranks) = self.order.get_mut(&project) {
                ranks.remove(&rank);
                if ranks.is_empty() {
                    self.order.remove(&project);
                }
            }
            self.dirty = true;
        }
    }

    fn append(&mut self, project: &str, member: &str) -> u64 {
        self.remove(member);
        let ranks = self.order.entry(project.to_owned()).or_default();
        let rank = ranks.keys().next_back().map_or(GAP, |last| last + GAP);
        ranks.insert(rank, member.to_owned());
        self.index
            .insert(member.to_owned(), (project.to_owned(), rank));
        self.dirty = true;
        rank
    }

    fn rank_in(&self, project: &str, member: &str) -> Option<u64> {
        self.index
            .get(member)
            .filter(|(owner, _)| owner == project)
            .map(|(_, rank)| *rank)
    }

    fn renumber(&mut self, project: &str) {
        self.renumbers += 1;
        let members: Vec<String> = self
            .order
            .remove(project)
            .map(|ranks| ranks.into_values().collect())
            .unwrap_or_default();
        let ranks = self.order.entry(project.to_owned()).or_default();
        for (at, member) in members.into_iter().enumerate() {
            let rank = (at as u64 + 1) * GAP;
            ranks.insert(rank, member.clone());
            self.index.insert(member, (project.to_owned(), rank));
        }
    }

    fn insert_before(&mut self, project: &str, member: &str, anchor: &str) {
        let Some(next) = self.rank_in(project, anchor) else {
            self.append(project, member);
            return;
        };
        let previous = self
            .order
            .get(project)
            .and_then(|ranks| ranks.range(..next).next_back().map(|(rank, _)| *rank))
            .unwrap_or(0);
        if next - previous < 2 {
            self.renumber(project);
            return self.insert_before(project, member, anchor);
        }
        let rank = previous + (next - previous) / 2;
        self.order
            .entry(project.to_owned())
            .or_default()
            .insert(rank, member.to_owned());
        self.index
            .insert(member.to_owned(), (project.to_owned(), rank));
    }

    /// A drop or Move to project: the members, in order, before `before`
    /// when it is in `project` and not moving itself, else last
    /// (moveSession, SessionOrganization.cpp:196-199).
    pub fn place(&mut self, project: &str, members: &[String], before: Option<&str>) {
        if members.is_empty() {
            return;
        }
        let anchor = before.filter(|anchor| !members.iter().any(|m| m == anchor));
        let was_dirty = self.dirty;
        let renumbers = self.renumbers;
        let ranks_before: Vec<Option<(String, u64)>> =
            members.iter().map(|m| self.index.get(m).cloned()).collect();
        for member in members {
            self.remove(member);
        }
        for member in members {
            match anchor {
                Some(anchor) => self.insert_before(project, member, anchor),
                None => {
                    self.append(project, member);
                }
            }
        }
        let ranks_after: Vec<Option<(String, u64)>> =
            members.iter().map(|m| self.index.get(m).cloned()).collect();
        self.dirty = was_dirty || self.renumbers != renumbers || ranks_before != ranks_after;
    }

    /// Sort sessions by name: the members leave their places and follow in
    /// this order, last (moveSession for each, SessionsWindow.cpp:2609).
    pub fn sort(&mut self, project: &str, members: &[String]) {
        self.place(project, members, None);
    }

    /// The new name takes the old one's place (renameSession, 214).
    pub fn rename(&mut self, from: &str, to: &str) {
        let Some((project, rank)) = self.index.get(from).cloned() else {
            return;
        };
        self.remove(to);
        self.remove(from);
        self.order
            .entry(project.clone())
            .or_default()
            .insert(rank, to.to_owned());
        self.index.insert(to.to_owned(), (project, rank));
        self.dirty = true;
    }

    /// A deleted project's order and collapsed state go.
    pub fn forget(&mut self, project: &str) {
        if let Some(ranks) = self.order.remove(project) {
            for member in ranks.into_values() {
                self.index.remove(&member);
            }
            self.dirty = true;
        }
        self.dirty |= self.collapsed.remove(project);
        if self.default_project.as_deref() == Some(project) {
            self.default_project = None;
            self.dirty = true;
        }
    }

    /// Past [`MAX_MEMBERS`], forgets every member `keep` refuses.
    pub fn prune(&mut self, keep: impl Fn(&str) -> bool) {
        if self.index.len() <= MAX_MEMBERS {
            return;
        }
        let gone: Vec<String> = self
            .index
            .keys()
            .filter(|member| !keep(member))
            .cloned()
            .collect();
        for member in gone {
            self.remove(&member);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_is_read_up_to_its_limit_and_no_further() {
        let dir = std::env::temp_dir().join(format!("zerus-layout-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("organization-layout.json");
        assert!(read_at_most(&path, 8).unwrap().is_none(), "a missing file");
        std::fs::write(&path, b"12345678").unwrap();
        assert_eq!(
            read_at_most(&path, 8).unwrap().as_deref(),
            Some(&b"12345678"[..])
        );
        std::fs::write(&path, b"123456789").unwrap();
        let error = read_at_most(&path, 8).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "the layout file is larger than 8 bytes");
        assert_eq!(MAX_BYTES, crate::swarm::draft::MAX_BYTES);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Qt keeps its organization in a file of its own beside the INI once it
    /// moved it out (tray/src/SettingsJson.cpp); the run memory is read there.
    #[test]
    fn run_groups_come_from_qts_own_organization_file() {
        let dir = std::env::temp_dir().join(format!("zerus-layout-qt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = crate::settings::QtStore::Ini(dir.join("hgs-tray.ini"));
        std::fs::write(store.path(), b"[workspace]\nskin=whitewh\n").unwrap();
        assert!(qt_run_groups(&store).unwrap().is_empty(), "no organization anywhere");
        assert_eq!(store.json_file("organization"), dir.join("hgs-tray.ini.organization.json"));
        std::fs::write(store.json_file("organization"), br#"{"run_groups":{"arch\nr1":"web"}}"#).unwrap();
        let groups = qt_run_groups(&store).unwrap();
        assert_eq!(groups.get("arch\nr1").and_then(|project| project.as_str()), Some("web"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|m| (*m).to_owned()).collect()
    }

    fn order(layout: &mut Layout, project: &str, members: &[&str]) -> Vec<String> {
        let mut ranked: Vec<(u64, String)> = members
            .iter()
            .map(|m| (layout.rank(project, m), (*m).to_owned()))
            .collect();
        ranked.sort();
        ranked.into_iter().map(|(_, m)| m).collect()
    }

    #[test]
    fn new_members_go_last_and_keep_their_rank() {
        let mut layout = Layout::default();
        assert_eq!(layout.rank("alpha", "a"), GAP);
        assert_eq!(layout.rank("alpha", "b"), 2 * GAP);
        assert_eq!(layout.rank("alpha", "a"), GAP, "a member keeps its place");
        assert_eq!(
            layout.rank("beta", "a"),
            GAP,
            "moved by the catalog: last in its new project"
        );
        assert_eq!(
            layout.rank("alpha", "c"),
            3 * GAP,
            "gaps stay; nothing is renumbered"
        );
    }

    #[test]
    fn a_drop_takes_the_middle_and_renumbers_only_without_room() {
        let mut layout = Layout::default();
        for m in ["a", "b", "c"] {
            layout.rank("alpha", m);
        }
        layout.place("alpha", &names(&["c"]), Some("b"));
        assert_eq!(
            order(&mut layout, "alpha", &["a", "b", "c"]),
            ["a", "c", "b"]
        );
        assert_eq!(
            layout.rank("alpha", "a"),
            GAP,
            "with room nobody else moves"
        );
        assert_eq!(
            layout.rank("alpha", "b"),
            2 * GAP,
            "with room nobody else moves"
        );
        assert_eq!(layout.rank("alpha", "c"), GAP + GAP / 2);
        assert_eq!(layout.renumbers, 0, "no renumber with room");
        for n in 0..40 {
            layout.place("alpha", &[format!("x{n}")], Some("c"));
        }
        assert!(layout.renumbers > 0, "a renumber happened without room");
        assert_eq!(
            order(&mut layout, "alpha", &["a", "x39", "c", "b"]),
            ["a", "x39", "c", "b"]
        );
        layout.place("beta", &names(&["a"]), Some("missing"));
        assert_eq!(
            layout.rank("beta", "a"),
            GAP,
            "an anchor outside the project places last"
        );
    }

    #[test]
    fn sort_appends_in_the_given_order() {
        let mut layout = Layout::default();
        for m in ["c", "a", "b", "kept"] {
            layout.rank("alpha", m);
        }
        layout.sort("alpha", &names(&["a", "b", "c"]));
        assert_eq!(
            order(&mut layout, "alpha", &["a", "b", "c", "kept"]),
            ["kept", "a", "b", "c"]
        );
    }

    #[test]
    fn a_rename_keeps_the_old_place() {
        let mut layout = Layout::default();
        for m in ["a", "b"] {
            layout.rank("alpha", m);
        }
        layout.rename("a", "a2");
        assert_eq!(order(&mut layout, "alpha", &["b", "a2"]), ["a2", "b"]);
    }

    #[test]
    fn the_layout_survives_a_restart_and_an_unreadable_file_starts_empty() {
        let mut layout = Layout::default();
        layout.rank("alpha", "b");
        layout.rank("alpha", "a");
        assert!(layout.set_collapsed("alpha", true));
        assert!(
            !layout.set_collapsed("alpha", true),
            "no change, nothing to save"
        );
        layout.set_default_project(Some("alpha".into()));
        let saved = layout.take_dirty().unwrap();
        assert!(layout.take_dirty().is_none(), "saved once");
        let mut again = Layout::from_json(&saved);
        assert!(again.collapsed("alpha"));
        assert_eq!(again.default_project(), Some("alpha"));
        assert_eq!(order(&mut again, "alpha", &["a", "b"]), ["b", "a"]);
        assert!(again.take_dirty().is_none(), "loading is not a change");
        assert_eq!(Layout::from_json(b"not json").len(), 0);
        assert_eq!(Layout::from_json(b"").len(), 0, "an empty file");
        for wrong in [
            &br#"{"order": 5}"#[..],
            br#"{"order": {"alpha": "a"}}"#,
            br#"{"collapsed": {"x": 1}, "version": "one"}"#,
            br#"[1, 2]"#,
            br#"null"#,
        ] {
            let loaded = Layout::from_json(wrong);
            assert_eq!(loaded.len(), 0);
            assert!(!loaded.collapsed("x"));
        }
        let mut future = Layout::from_json(
            br#"{"version": 99, "collapsed": ["p"], "order": {"p": ["a"]}, "extra": true}"#,
        );
        assert!(
            future.collapsed("p"),
            "an unknown version loads what it can"
        );
        assert_eq!(future.rank("p", "a"), GAP);
    }

    #[test]
    fn the_current_writer_round_trips_at_every_entry_limit() {
        let mut layout = Layout::default();
        for n in 0..MAX_MEMBERS {
            layout.rank("alpha", &format!("workstation\nmember-{n}"));
        }
        for n in 0..MAX_RUNS {
            layout.remember_run("workstation", &format!("run-{n}"), Some("alpha"));
        }
        for n in 0..super::super::MAX_PROJECTS {
            layout.set_collapsed(&format!("project-{n}"), true);
        }
        layout.set_default_project(Some("alpha".into()));
        let bytes = layout.to_json();
        let mut loaded = Layout::try_from_json(&bytes).expect("our own bounded writer must load");
        assert_eq!(loaded.len(), MAX_MEMBERS);
        assert_eq!(loaded.runs.len(), MAX_RUNS);
        assert_eq!(loaded.collapsed.len(), super::super::MAX_PROJECTS);
        assert_eq!(loaded.default_project(), Some("alpha"));
        assert_eq!(
            loaded.to_json(),
            bytes,
            "no eviction, reordering or lost fields"
        );
        assert!(loaded.take_dirty().is_none());
    }

    #[test]
    fn the_pre_510_installed_writer_round_trips_without_loss() {
        // Generated by executing Layout::to_json at 8d65bcc633363e3084fe9feeda9e26f6654c9050,
        // the owner-restored build. That writer is unchanged through pre-#510 0135e437.
        let bytes = include_bytes!("../../tests/fixtures/layout/pre-510-writer.json");
        let mut loaded = Layout::try_from_json(bytes).unwrap();
        assert_eq!(loaded.len(), 3);
        assert!(loaded.collapsed("alpha"));
        assert_eq!(loaded.default_project(), Some("beta"));
        assert!(loaded.runs_seeded());
        assert_eq!(loaded.run_project("workstation", "run-one"), Some("alpha"));
        assert_eq!(loaded.run_project("studio", "run-two"), Some("beta"));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&loaded.to_json()).unwrap(),
            serde_json::from_slice::<serde_json::Value>(bytes).unwrap()
        );
        assert!(loaded.take_dirty().is_none());
    }

    #[test]
    fn strict_loading_preserves_unknown_fields_when_known_values_change() {
        let original = serde_json::json!({"version":1,"collapsed":["alpha"],
            "future":{"nested":[1,2,3]},"g3":{"version":99,"opaque":"retain"}});
        let mut loaded = Layout::try_from_json(&serde_json::to_vec(&original).unwrap()).unwrap();
        loaded.set_collapsed("beta", true);
        let saved: serde_json::Value = serde_json::from_slice(&loaded.to_json()).unwrap();
        assert_eq!(saved["future"], original["future"]);
        assert_eq!(saved["g3"], original["g3"]);
        assert!(loaded.collapsed("alpha"));
        assert!(Layout::try_from_json(br#"{"collapsed":["legacy"]}"#)
            .unwrap()
            .collapsed("legacy"));
    }

    #[test]
    fn strict_loading_refuses_future_malformed_and_lossy_records() {
        for bytes in [
            b"not json".as_slice(),
            b"[]",
            b"{}trailing",
            br#"{"version":99}"#,
            br#"{"version":1,"order":{"alpha":["same"],"beta":["same"]}}"#,
            br#"{"run_groups":[["missing separator","alpha"]]}"#,
            br#"{"run_groups":[["host\nrun","alpha"],["host\nrun","beta"]]}"#,
            br#"{"run_groups":[["host\nrun\nextra","alpha"]]}"#,
        ] {
            assert!(
                Layout::try_from_json(bytes).is_err(),
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
        let oversized = serde_json::json!({"collapsed":(0..super::super::MAX_PROJECTS+1).map(|n|format!("p{n}")).collect::<Vec<_>>()});
        assert!(Layout::try_from_json(&serde_json::to_vec(&oversized).unwrap()).is_err());
        let members = serde_json::json!({"order":{"alpha":(0..MAX_MEMBERS+1).map(|n|format!("m{n}")).collect::<Vec<_>>()}});
        assert!(Layout::try_from_json(&serde_json::to_vec(&members).unwrap()).is_err());
    }

    #[test]
    fn loading_caps_members_and_collapsed_projects() {
        let members: Vec<String> = (0..MAX_MEMBERS + 5).map(|n| format!("m{n}")).collect();
        let collapsed: Vec<String> = (0..crate::swarm::MAX_PROJECTS + 5)
            .map(|n| format!("p{n}"))
            .collect();
        let json = serde_json::json!({"collapsed": collapsed, "order": {"alpha": members}});
        let loaded = Layout::from_json(&serde_json::to_vec(&json).unwrap());
        assert_eq!(loaded.len(), MAX_MEMBERS);
        assert!(loaded.index.contains_key("m0"));
        assert!(
            !loaded.index.contains_key(&format!("m{MAX_MEMBERS}")),
            "the excess is dropped"
        );
        assert_eq!(loaded.collapsed.len(), crate::swarm::MAX_PROJECTS);
    }

    #[test]
    fn forget_clears_the_default_project_it_names() {
        let mut layout = Layout::default();
        layout.set_default_project(Some("gone".into()));
        layout.take_dirty();
        layout.forget("other");
        assert_eq!(layout.default_project(), Some("gone"));
        assert!(layout.take_dirty().is_none());
        layout.forget("gone");
        assert_eq!(layout.default_project(), None);
        assert!(
            layout.take_dirty().is_some(),
            "the cleared default is saved"
        );
    }

    #[test]
    fn a_place_that_moves_nothing_is_not_a_change() {
        let mut layout = Layout::default();
        for m in ["a", "b"] {
            layout.rank("alpha", m);
        }
        layout.take_dirty();
        layout.place("alpha", &[], Some("a"));
        assert!(layout.take_dirty().is_none(), "no members");
        layout.place("alpha", &names(&["a"]), Some("b"));
        assert!(layout.take_dirty().is_none(), "a keeps its rank before b");
        layout.place("alpha", &names(&["b"]), Some("a"));
        assert!(layout.take_dirty().is_some(), "a real move is saved");
    }

    #[test]
    fn forget_and_prune_bound_the_layout() {
        let mut layout = Layout::default();
        layout.rank("gone", "a");
        layout.set_collapsed("gone", true);
        layout.forget("gone");
        assert!(!layout.collapsed("gone"));
        assert_eq!(layout.len(), 0);
        for n in 0..MAX_MEMBERS + 10 {
            layout.rank("alpha", &format!("m{n}"));
        }
        layout.prune(|m| m == "m0");
        assert_eq!(layout.len(), 1);
        let mut small = Layout::default();
        small.rank("alpha", "a");
        small.prune(|_| false);
        assert_eq!(small.len(), 1, "under the cap nothing is pruned");
    }
}
