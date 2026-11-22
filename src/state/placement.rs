//! Durable, local initial placement. Only genuine launch writers prepare one;
//! the catalog owner drains a bounded pending-only queue, never a session scan.
use super::*;
use crate::config::Config;
use repositories::directory::Directory;
use std::{
    fs,
    io::Read,
    time::{Duration, Instant},
};

const FIELD: &str = "initial_placement";
const QUEUE: &str = "placement-intents";
const BYTES: u64 = 64 << 10;

/// Called under the launch's existing writer, before its first binding write.
/// Grouping is optional: a storage failure warns once, cleans up this attempt,
/// and leaves the ordinary launch and its first placement attempt unchanged.
pub(crate) fn prepare(record: &mut Value, directory: &Path) {
    if let Err(error) = prepare_inner(record, directory) {
        if let Some(record) = record.as_object_mut() {
            record.remove(FIELD);
        }
        eprintln!("hgs: initial placement unavailable; launch continues: {error}");
    }
}

fn prepare_inner(record: &mut Value, directory: &Path) -> Result<()> {
    let config = Config::load().map_err(|error| error.message)?;
    if !config.dir.join("swarm/catalog.json").is_file() {
        return Ok(());
    }
    ensure_session_id(record);
    let catalog = crate::platform::canonical(&config.dir).map_err(|e| e.to_string())?;
    let parent = record.get("parent").cloned().unwrap_or(Value::Null);
    let intent = json!({"schema":1,"session_id":record["session_id"],"run_id":record["run_id"],
        "name":record["name"],"backend":record.get("backend").cloned().unwrap_or(Value::Null),
        "id_layout":storage::in_id_layout(record),"catalog":catalog,"directory":directory,
        "parent":parent,"nonce":uuid::Uuid::new_v4().to_string()});
    let name = filename(&intent).ok_or("Invalid new placement identity")?;
    let root = Directory::open(&absolute_root()?)?;
    let queue = root.ensure_child(QUEUE)?;
    let bytes = serde_json::to_vec(&intent).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > BYTES {
        return Err("Initial placement intent is oversized".into());
    }
    publish_intent(&root, &queue, &name, &bytes)?;
    record[FIELD] = intent;
    Ok(())
}

fn publish_intent(root: &Directory, queue: &Directory, name: &str, bytes: &[u8]) -> Result<()> {
    let result = (|| {
        publish_file(queue, name, bytes)?;
        queue.verify()?;
        root.verify()
    })();
    if let Err(error) = result {
        // Publication can fail after rename (for example at directory sync).
        // Retire only this attempt's exact nonce-bearing bytes, through the
        // original parent even if its pathname has since been replaced.
        return Err(match discard(queue, name, bytes) {
            Ok(()) => error,
            Err(cleanup) => format!("{error}; placement cleanup: {cleanup}"),
        });
    }
    Ok(())
}

#[cfg(not(windows))]
fn publish_file(queue: &Directory, name: &str, bytes: &[u8]) -> Result<()> {
    queue.replace_file(name, bytes)
}

#[cfg(windows)]
fn publish_file(queue: &Directory, name: &str, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    // Directory pins every Windows ancestor against rename/reparse changes.
    // Unlike the shared publication primitive (which deliberately retains
    // failed native-effect evidence), this optional metadata owns a temporary
    // file whose failure path removes its staging name as well.
    queue.verify()?;
    let mut file = tempfile::NamedTempFile::new_in(&queue.path).map_err(|e| e.to_string())?;
    if !crate::platform::verify_private_file(file.as_file()).map_err(|e| e.to_string())? {
        return Err("The placement file must be private and regular".into());
    }
    file.write_all(bytes).map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    let file = file
        .persist_noclobber(queue.path.join(name))
        .map_err(|e| e.error.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

fn discard(queue: &Directory, name: &str, bytes: &[u8]) -> Result<()> {
    let Some(file) = queue.read_private_optional(name)? else {
        return Ok(());
    };
    let mut actual = Vec::new();
    file.take(BYTES + 1)
        .read_to_end(&mut actual)
        .map_err(|e| e.to_string())?;
    if actual != bytes {
        return Err("Placement file changed; unrelated file retained".into());
    }
    #[cfg(unix)]
    {
        let name = std::ffi::CString::new(name).map_err(|e| e.to_string())?;
        // SAFETY: the original directory remains open; this single component
        // was just read without following links and matched this attempt.
        if unsafe { libc::unlinkat(queue.descriptor(), name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    #[cfg(windows)]
    {
        queue.verify()?;
        fs::remove_file(queue.path.join(name)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn filename(intent: &Value) -> Option<String> {
    let id = string(intent, "session_id");
    let run = string(intent, "run_id");
    (intent["schema"] == 1
        && session_id::canonical(id)
        && session_id::canonical(run)
        && session_id::canonical(string(intent, "nonce")))
    .then(|| format!("{id}.{run}.json"))
}

#[cfg(all(test, unix))]
mod tests;

/// Retains just one directory iterator. At most sixteen pending entries and
/// one second are attempted per worker round; busy entries stay for a later
/// pass, and cannot starve later entries in that same bounded cursor.
#[derive(Default)]
pub(crate) struct Scan {
    entries: Option<fs::ReadDir>,
}

impl Scan {
    pub(crate) fn advance(
        &mut self,
        config: &Config,
        mut place: impl FnMut(&Value, &Value, Instant) -> anyhow::Result<bool>,
    ) -> anyhow::Result<()> {
        if !root().join(QUEUE).exists() {
            self.entries = None;
            return Ok(());
        }
        let root =
            Directory::open(&crate::platform::canonical(root())?).map_err(anyhow::Error::msg)?;
        let queue = root.child(QUEUE).map_err(anyhow::Error::msg)?;
        if self.entries.is_none() {
            self.entries = Some(fs::read_dir(&queue.path)?);
        }
        let end = Instant::now() + Duration::from_secs(1);
        for _ in 0..16 {
            if Instant::now() >= end {
                break;
            }
            let Some(entry) = self.entries.as_mut().and_then(Iterator::next) else {
                self.entries = None;
                break;
            };
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !name.ends_with(".json") {
                continue;
            }
            // Unknown/corrupt entries are retained, but the cursor advances.
            let attempt = self.visit(config, &queue, &name, end, &mut place);
            if let Err(error) = attempt {
                eprintln!("hgs swarm: initial placement remains pending: {error:#}");
            }
        }
        queue.verify().map_err(anyhow::Error::msg)?;
        root.verify().map_err(anyhow::Error::msg)
    }

    fn visit(
        &self,
        config: &Config,
        queue: &Directory,
        name: &str,
        end: Instant,
        place: &mut impl FnMut(&Value, &Value, Instant) -> anyhow::Result<bool>,
    ) -> anyhow::Result<()> {
        let mut bytes = Vec::new();
        queue
            .read_private(name)
            .map_err(anyhow::Error::msg)?
            .take(BYTES + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= BYTES,
            "Initial placement intent is oversized"
        );
        let intent: Value = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            filename(&intent).as_deref() == Some(name),
            "Initial placement identity changed"
        );
        if intent["catalog"].as_str() != crate::platform::canonical(&config.dir)?.to_str() {
            return Ok(());
        }
        let Some(_writer) = storage::writer_until(Instant::now()).map_err(anyhow::Error::msg)?
        else {
            return Ok(());
        };
        let provider = match intent["backend"].as_str() {
            Some("dsh") => Some(provider_binding::Provider::DeepSeek),
            Some("opencode") => Some(provider_binding::Provider::OpenCode),
            None if intent["backend"].is_null() => None,
            _ => anyhow::bail!("Unknown initial placement provider"),
        };
        let _provider = if let Some(provider) = provider {
            let backend = if matches!(provider, provider_binding::Provider::DeepSeek) {
                "dsh"
            } else {
                "opencode"
            };
            let Some(lock) =
                storage::lock_until(&root().join(backend).join("bindings.lock"), Instant::now())
                    .map_err(anyhow::Error::msg)?
            else {
                return Ok(());
            };
            Some(lock)
        } else {
            None
        };
        let record = self.binding(&intent, provider)?;
        let complete = match record {
            None => true,
            Some(record)
                if record["session_id"] != intent["session_id"]
                    || record.get("backend").unwrap_or(&Value::Null) != &intent["backend"]
                    || record["run_id"] != intent["run_id"]
                    || record[FIELD] != intent
                    || record.get("parent").unwrap_or(&Value::Null) != &intent["parent"]
                    || (record["completion_source"] == "supervisor"
                        && record["exited_at"].as_f64().is_some_and(|at| at > 0.0)) =>
            {
                true
            }
            Some(record) if record["creation_pending"] == true => false,
            Some(record) => place(&record, &intent, end)?,
        };
        if complete {
            // Only launch writers create this exact ID/run key; the same
            // state/provider locks exclude a replacement through this unlink.
            queue.verify().map_err(anyhow::Error::msg)?;
            fs::remove_file(queue.path.join(name))?;
            crate::platform::sync_dir(&queue.path)?;
            queue.verify().map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }

    fn binding(
        &self,
        intent: &Value,
        provider: Option<provider_binding::Provider>,
    ) -> anyhow::Result<Option<Value>> {
        let id = string(intent, "session_id");
        // The advisory index supplies only a fixed route. The authoritative
        // record is re-read under the matching provider lock on every platform.
        if let Some((name, kind)) = session_index::indexed(id) {
            use session_index::Kind;
            let candidate = match (provider, kind) {
                (None, Kind::Record) => Some(storage::label_record_path(&name)),
                (None, Kind::IdRecord) => storage::id_record_path(id),
                (Some(provider_binding::Provider::DeepSeek), Kind::DeepSeek)
                | (Some(provider_binding::Provider::OpenCode), Kind::OpenCode) => {
                    provider.map(|p| p.label_path(&name))
                }
                (Some(provider_binding::Provider::DeepSeek), Kind::DeepSeekId)
                | (Some(provider_binding::Provider::OpenCode), Kind::OpenCodeId) => {
                    provider.and_then(|p| p.id_path(id))
                }
                _ => None,
            };
            if let Some(path) = candidate {
                if let Ok(record) = self.read_binding(&path, &name, provider) {
                    if record
                        .as_ref()
                        .is_some_and(|record| record["session_id"] == id)
                    {
                        return Ok(record);
                    }
                }
            }
        }
        let name = string(intent, "name");
        let path = match (provider, intent["id_layout"] == true) {
            (Some(provider), true) => provider
                .id_path(id)
                .ok_or_else(|| anyhow::anyhow!("Invalid placement ID"))?,
            (Some(provider), false) => provider.label_path(name),
            (None, true) => storage::id_record_path(id)
                .ok_or_else(|| anyhow::anyhow!("Invalid placement ID"))?,
            (None, false) => storage::label_record_path(name),
        };
        self.read_binding(&path, name, provider)
    }

    fn read_binding(
        &self,
        path: &Path,
        name: &str,
        provider: Option<provider_binding::Provider>,
    ) -> anyhow::Result<Option<Value>> {
        let Some(bytes) = labels::read_file(path, 1 << 20)? else {
            return Ok(None);
        };
        let raw: Value = serde_json::from_slice(&bytes)?;
        if raw["state"] == "archived" || !string(&raw, "archive_id").is_empty() {
            return Ok(None);
        }
        anyhow::ensure!(
            raw["rename_shadow"] != true,
            "Initial placement is waiting for its verified ID route"
        );
        let backend = provider.map(|p| {
            if matches!(p, provider_binding::Provider::DeepSeek) {
                "dsh"
            } else {
                "opencode"
            }
        });
        super::session_project::binding_metadata(path, name, backend).map(Some)
    }
}
