//! Fixed native-provider binding routes. The ID layout is read and changed in
//! place; it is never inferred from a label, recreated after loss, or published
//! by this module. Duplicate-label creation remains a separate gated writer.
use super::*;
mod native_identity;
#[cfg(test)]
pub(in crate::state) use native_identity::{cache_path_for, complete_scan_count};

const BYTES: u64 = 1 << 20;
const RECORDS: usize = 5_000;
#[cfg(test)]
pub(in crate::state) const RECORDS_FOR_TEST: usize = RECORDS;

#[derive(Clone, Copy)]
pub(super) enum Provider {
    DeepSeek,
    OpenCode,
}

impl Provider {
    pub(super) fn native_caller(self, native: &str, home: &Path) -> Result<Option<Value>> {
        native_identity::find(self, native, home)
    }
    pub(super) fn named(name: &str) -> Option<Self> {
        match name.split('/').next() {
            Some("dsh") => Some(Self::DeepSeek),
            Some("opencode") => Some(Self::OpenCode),
            _ => None,
        }
    }
    fn backend(self) -> &'static str {
        match self {
            Self::DeepSeek => "dsh",
            Self::OpenCode => "opencode",
        }
    }
    pub(super) fn directory(self) -> PathBuf {
        root().join(self.backend()).join("sessions")
    }
    pub(super) fn id_path(self, id: &str) -> Option<PathBuf> {
        session_id::canonical(id).then(|| self.directory().join(format!("{id}.json")))
    }
    pub(super) fn label_path(self, name: &str) -> PathBuf {
        root()
            .join(self.backend())
            .join("bindings")
            .join(legacy_record_path(name).file_name().unwrap())
    }
    pub(super) fn kind(self, record: &Value) -> session_index::Kind {
        match (self, storage::in_id_layout(record)) {
            (Self::DeepSeek, false) => session_index::Kind::DeepSeek,
            (Self::DeepSeek, true) => session_index::Kind::DeepSeekId,
            (Self::OpenCode, false) => session_index::Kind::OpenCode,
            (Self::OpenCode, true) => session_index::Kind::OpenCodeId,
        }
    }
    fn read_at(self, path: &Path, id: Option<&str>) -> Result<Value> {
        let bytes = labels::read_file(path, BYTES)
            .map_err(|e| e.to_string())?
            .ok_or("Native binding is absent")?;
        let record: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let name = string(&record, "name");
        let valid_conversation = match self {
            Self::DeepSeek => {
                string(&record, "conversation_id").starts_with("session-")
                    && string(&record, "conversation_id").len() <= 2048
            }
            Self::OpenCode => opencode::native_id(string(&record, "conversation_id"), "ses_"),
        };
        if !record.is_object()
            || record["backend"] != self.backend()
            || !name.starts_with(&format!("{}/", self.backend()))
            || !valid_conversation
            || id.map_or_else(
                || storage::in_id_layout(&record),
                |id| !storage::is_id_layout_record(&record, id),
            )
        {
            return Err("Native binding identity, layout or location changed".into());
        }
        Ok(record)
    }
    /// Unselected metadata read, for verified indexes and complete candidates.
    pub(super) fn read_id(self, id: &str) -> Result<Value> {
        self.read_at(&self.id_path(id).ok_or("Invalid session ID")?, Some(id))
    }
    pub(super) fn read_label(self, name: &str) -> Result<Value> {
        let record = self.read_at(&self.label_path(name), None)?;
        if string(&record, "name") != name {
            return Err("Native binding label changed".into());
        }
        Ok(record)
    }
    /// A selection keeps its original namespace even after a missing/replaced
    /// file. Without a selection, only a proven sole holder supplies a route.
    pub(super) fn path(self, name: &str) -> PathBuf {
        if let Some(selection) = session_id::selected().filter(|s| s.name == name && s.id_layout) {
            return self
                .id_path(&selection.session_id)
                .unwrap_or_else(|| self.directory().join(".invalid"));
        }
        let legacy = self.label_path(name);
        if std::fs::symlink_metadata(&legacy)
            .map_or_else(|e| e.kind() != io::ErrorKind::NotFound, |_| true)
        {
            return legacy;
        }
        if let Ok(candidates) = labels::candidates(name) {
            if let (false, [only]) = (candidates.first, candidates.by_id.as_slice()) {
                if let Some(path) = self.id_path(string(only, "session_id")) {
                    return path;
                }
            }
        }
        legacy
    }
    pub(super) fn read(self, name: &str) -> Result<Value> {
        let path = self.path(name);
        let id = (path.parent() == Some(self.directory().as_path())).then(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
        });
        let record = self.read_at(&path, id)?;
        if string(&record, "name") != name {
            return Err("Native binding label changed".into());
        }
        session_id::verify(&record)?;
        Ok(record)
    }
    /// A uniqueness guard never interprets ambiguity or unreadable coverage as
    /// absence. This does not choose a holder or authorize creation.
    pub(super) fn exists(self, name: &str) -> bool {
        if !name.starts_with(&format!("{}/", self.backend())) {
            return false;
        }
        session_id::selected().is_some_and(|s| s.name == name)
            || labels::candidates(name).map_or(true, |c| c.first || !c.by_id.is_empty())
    }
    /// Native launch commands do not pass through the ordinary command
    /// resolver. Resolve once, under the provider lock, BEFORE starting or
    /// contacting a host/service. A stale explicit selection never becomes a
    /// new launch, and a name with several holders never gains a third one.
    pub(super) fn existing_for_launch(self, name: &str) -> Result<Option<Value>> {
        if !name.starts_with(&format!("{}/", self.backend())) {
            return Err("The native launch provider changed".into());
        }
        if session_id::selected().is_some_and(|s| s.name == name) {
            return self.read(name).map(Some);
        }
        let candidates = labels::candidates(name)?;
        match (candidates.first, candidates.by_id.as_slice()) {
            (false, []) => Ok(None),
            (true, []) => self.read(name).map(Some),
            (false, [only]) => {
                session_id::select(name, string(only, "session_id"));
                self.read(name).map(Some)
            }
            _ => Err(session_id::refuse(ambiguity_report(name, &candidates))),
        }
    }
    pub(super) fn vacant(self, name: &str) -> Result<bool> {
        if !name.starts_with(&format!("{}/", self.backend())) {
            return Err("The native target provider changed".into());
        }
        let candidates = labels::candidates(name)?;
        Ok(!candidates.first && candidates.by_id.is_empty())
    }
    /// Listings are bounded metadata, never a native admission or absence
    /// proof. Malformed entries do not become another session's binding.
    pub(super) fn records(self) -> Vec<Value> {
        let mut records = Vec::new();
        let mut count = 0;
        for (directory, by_id) in [
            (root().join(self.backend()).join("bindings"), false),
            (self.directory(), true),
        ] {
            let Ok(entries) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                if count == RECORDS {
                    return records;
                }
                count += 1;
                let path = entry.path();
                if path.extension().is_none_or(|s| s != "json") {
                    continue;
                }
                let id = by_id.then(|| {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default()
                });
                let Ok(record) = self.read_at(&path, id) else {
                    continue;
                };
                if !by_id && self.label_path(string(&record, "name")) != path {
                    continue;
                }
                if string(&record, "archive_id").is_empty() && record["rename_shadow"] != true {
                    records.push(record);
                }
            }
        }
        records
    }
    /// Durable index superset before changing an ID record's label. Provider
    /// mutators already hold their own lock: never wait for the state writer in
    /// the reverse order. A busy writer refuses; it is not a new lock ordering.
    pub(super) fn prepare_label(self, record: &Value, name: &str) -> Result<()> {
        if !storage::in_id_layout(record) {
            return Ok(());
        }
        let id = string(record, "session_id");
        self.read_id(id)?;
        if labels::listed_in(&root(), name).is_some_and(|ids| ids.iter().any(|i| i == id)) {
            return Ok(());
        }
        let _writer = storage::writer_until(std::time::Instant::now())?
            .ok_or("Session metadata is busy; nothing was changed")?;
        labels::add(name, id)
    }
    pub(super) fn save(self, record: &mut Value) -> Result<()> {
        ensure_session_id(record);
        let name = string(record, "name");
        let path = if storage::in_id_layout(record) {
            self.prepare_label(record, name)?;
            self.id_path(string(record, "session_id"))
                .ok_or("Invalid session ID")?
        } else {
            if self.id_path(string(record, "session_id")).is_some_and(|p| {
                std::fs::symlink_metadata(p)
                    .map_or_else(|e| e.kind() != io::ErrorKind::NotFound, |_| true)
            }) {
                return Err("Native binding layout changed; refusing to write".into());
            }
            self.label_path(name)
        };
        let before = native_identity::before_write(self);
        atomic(&path, &record.to_string())?;
        // A write the index could not record stays unaccounted: the next
        // lookup sees an unaccounted change instead of a clean ledger
        // (zerus-8g1je).
        if native_identity::note(self, record) {
            native_identity::account(self, &before);
        }
        session_index::note(record, self.kind(record));
        if matches!(self, Self::OpenCode) {
            // Keep the optional context route with every authoritative write,
            // including generic rename; a failed note leaves no valid digest.
            opencode::note_hint(record);
        }
        Ok(())
    }
    pub(super) fn remove(self, record: &Value) -> Result<()> {
        let path = if storage::in_id_layout(record) {
            let id = string(record, "session_id");
            let current = self.read_id(id)?;
            if current["name"] != record["name"]
                || current["run_id"] != record["run_id"]
                || current["conversation_id"] != record["conversation_id"]
            {
                return Err("Native binding changed; nothing was removed".into());
            }
            self.id_path(id).ok_or("Invalid session ID")?
        } else {
            self.label_path(string(record, "name"))
        };
        let before = native_identity::before_write(self);
        archive::remove_synced(&path)?;
        if native_identity::retire(self, record) {
            native_identity::account(self, &before);
        }
        session_index::retire_provider(record, self.kind(record));
        // Stale supersets are safe. Do not wait in reverse lock order.
        if storage::in_id_layout(record) {
            if let Ok(Some(_writer)) = storage::writer_until(std::time::Instant::now()) {
                let _ = labels::remove(string(record, "name"), string(record, "session_id"));
            }
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn native_provider_ids_never_fall_back_or_recreate_a_lost_binding() {
        let state = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::provider_binding::tests::provider_id_fixture",
                "--ignored",
                "--test-threads=1",
            ])
            .env("HGS_STATE_DIR", state.path().join("state"))
            .env("HGS_CONFIG_DIR", state.path().join("config"))
            .env("TMUX_TMPDIR", state.path().join("tmux"))
            .env("ZERUS_PROVIDER_ID_FIXTURE", "1")
            .env_remove("TMUX")
            .env_remove("HGS_SESSION_ID")
            .env_remove("HGS_SESSION")
            .env_remove("HGS_RUN_ID")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn provider_id_fixture() {
        if std::env::var("ZERUS_PROVIDER_ID_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        for provider in [Provider::DeepSeek, Provider::OpenCode] {
            session_id::clear();
            let name = format!("{}/example/shared", provider.backend());
            let mut records = Vec::new();
            for index in 0..3 {
                let id = uuid::Uuid::new_v4().to_string();
                let conversation = match provider {
                    Provider::DeepSeek => format!("session-{index}"),
                    Provider::OpenCode => format!("ses_{index}"),
                };
                let mut record = json!({"name":name,"backend":provider.backend(),"agent":provider.backend(),"session_id":id,
                    "run_id":format!("run-{index}"),"conversation_id":conversation,"journal_name":id,"agent_home":"/tmp/example"});
                let path = if index == 0 {
                    provider.label_path(&name)
                } else {
                    record["layout"] = json!("id");
                    provider.id_path(&id).unwrap()
                };
                if index != 0 {
                    let _writer = lock(None).unwrap();
                    labels::add(&name, &id).unwrap();
                }
                atomic(&path, &record.to_string()).unwrap();
                records.push(record);
            }
            let _provider =
                lock(Some(&root().join(provider.backend()).join("bindings.lock"))).unwrap();
            {
                let _writer = lock(None).unwrap();
                labels::ensure_manifest().unwrap();
            }
            assert_eq!(labels::candidates(&name).unwrap().by_id.len(), 2);
            assert!(
                !storage::label_taken(&name),
                "own provider bindings cannot block launch reconciliation as terminal holders"
            );
            let terminal = storage::label_record_path(&name);
            atomic(&terminal, "unreadable foreign terminal binding").unwrap();
            assert!(
                storage::label_taken(&name),
                "a foreign terminal holder still fences the provider"
            );
            std::fs::remove_file(terminal).unwrap();
            let foreign_id = uuid::Uuid::new_v4().to_string();
            let foreign = json!({"version":1,"agent":"codex","name":name,"layout":"id","session_id":foreign_id,
                "run_id":"foreign-run","conversation_id":uuid::Uuid::new_v4().to_string()});
            let foreign_path = storage::id_record_path(&foreign_id).unwrap();
            {
                let _writer = lock(None).unwrap();
                labels::add(&name, &foreign_id).unwrap();
            }
            atomic(&foreign_path, &foreign.to_string()).unwrap();
            assert!(
                storage::label_taken(&name),
                "a terminal ID record still fences the provider"
            );
            atomic(&foreign_path, "{bad").unwrap();
            assert!(
                storage::label_taken(&name),
                "unreadable terminal ID evidence is never absence"
            );
            std::fs::remove_file(foreign_path).unwrap();
            {
                let _writer = lock(None).unwrap();
                labels::remove(&name, &foreign_id).unwrap();
            }
            assert!(!storage::label_taken(&name));
            assert!(resolve_label(&name, LabelGuard::None).is_err());
            for record in &records {
                assert_eq!(
                    session_id::names_of(string(record, "session_id")).unwrap(),
                    vec![name.clone()]
                );
            }
            let mut selected = records[1].clone();
            let id = string(&selected, "session_id").to_owned();
            session_id::select(&name, &id);
            let other = match provider {
                Provider::DeepSeek => Provider::OpenCode,
                Provider::OpenCode => Provider::DeepSeek,
            };
            assert!(
                !other.exists(&name),
                "a selected route never changes its provider"
            );
            assert_eq!(provider.read(&name).unwrap(), selected);
            selected["paused"] = json!(true);
            provider.save(&mut selected).unwrap();
            assert_eq!(provider.read_id(&id).unwrap(), selected);
            assert_eq!(
                provider.read_id(string(&records[2], "session_id")).unwrap(),
                records[2]
            );
            assert_eq!(provider.read_label(&name).unwrap(), records[0]);
            let new = format!("{}/example/renamed", provider.backend());
            provider.prepare_label(&selected, &new).unwrap();
            selected["name"] = json!(new);
            provider.save(&mut selected).unwrap();
            assert_eq!(session_id::names_of(&id).unwrap(), vec![new.clone()]);
            session_id::clear();
            session_id::select(&new, &id);
            assert_eq!(provider.read(&new).unwrap(), selected);
            assert_eq!(labels::candidates(&name).unwrap().by_id.len(), 1);
            provider.remove(&selected).unwrap();
            assert!(provider.read(&new).is_err());
            assert!(provider.save(&mut selected).is_err());
            assert!(!provider.id_path(&id).unwrap().exists());
            session_id::clear();
            session_id::select(&name, string(&records[2], "session_id"));
            let other_path = provider.id_path(string(&records[2], "session_id")).unwrap();
            let original = std::fs::read(&other_path).unwrap();
            std::fs::remove_file(&other_path).unwrap();
            assert!(provider.read(&name).is_err());
            // A link or wrong layout at the fixed route never reaches the first holder.
            std::os::unix::fs::symlink(provider.label_path(&name), &other_path).unwrap();
            assert!(provider.read(&name).is_err());
            std::fs::remove_file(&other_path).unwrap();
            let mut bad = records[2].clone();
            bad.as_object_mut().unwrap().remove("layout");
            atomic(&other_path, &bad.to_string()).unwrap();
            assert!(provider.read(&name).is_err());
            atomic(&other_path, std::str::from_utf8(&original).unwrap()).unwrap();
            assert_eq!(provider.read(&name).unwrap(), records[2]);
            assert_eq!(provider.read_label(&name).unwrap(), records[0]);
        }
        session_id::clear();
    }
}
