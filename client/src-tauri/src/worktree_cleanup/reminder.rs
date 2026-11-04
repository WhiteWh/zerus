//! Bounded read-only reminder for the mockup's Projects worktree panel.
//! Counts never supply rows, removal eligibility, a confirmation or authority.
use super::*;
use serde::Deserialize;
use std::time::{Duration, Instant};

const EVERY: Duration = Duration::from_secs(300);
const REPOSITORIES: usize = 64;
type Key = (String, String, String);

#[derive(Clone, Serialize)]
pub struct Reminder {
    count: Option<usize>,
    retry_ms: u64,
}
struct Entry {
    attempted: Instant,
    count: Option<usize>,
}
#[derive(Default)]
pub(super) struct Cache {
    entries: BTreeMap<Key, Entry>,
    running: bool,
    generation: u64,
}
impl Cache {
    fn begin(&mut self, key: &Key, now: Instant) -> Result<Option<Reminder>, String> {
        if let Some(entry) = self.entries.get(key) {
            let age = now.saturating_duration_since(entry.attempted);
            if age < EVERY {
                return Ok(Some(Reminder {
                    count: entry.count,
                    retry_ms: (EVERY - age).as_millis() as u64,
                }));
            }
        }
        // An outer await, hidden panel or changed selection cannot release
        // the actual blocking reader's slot. No waiting native queue grows.
        if self.running {
            return Err("A worktree reminder read is already running.".into());
        }
        self.entries
            .retain(|_, entry| now.saturating_duration_since(entry.attempted) < EVERY);
        if self.entries.len() >= REPOSITORIES {
            return Err("Worktree reminders are at capacity. Review the folder directly.".into());
        }
        // Failed reads also spend this interval. Never evict a fresh attempt
        // to turn rapid selection or repeated failures into a new CLI call.
        self.entries.insert(
            key.clone(),
            Entry {
                attempted: now,
                count: None,
            },
        );
        self.running = true;
        Ok(None)
    }
    // A verified removal changes this exact repository before the frontend
    // closes its review. Re-arm only its advisory read; the actual reader
    // still owns its slot until Drop, including any invalidated late reply.
    pub(super) fn removed(&mut self, computer: &str, machine: &str, common: &str) {
        self.entries
            .remove(&(computer.into(), machine.into(), common.into()));
        self.generation = self.generation.saturating_add(1);
    }
    pub(super) fn invalidate(&mut self) {
        self.generation = self.generation.saturating_add(1);
        for entry in self.entries.values_mut() {
            entry.count = None;
        }
    }
}
struct Reader {
    state: WorktreeCleanup,
    key: Key,
    generation: u64,
}
impl Reader {
    fn finish(&self, count: Option<usize>) -> Reminder {
        let mut model = lock(&self.state.0);
        let cache = &mut model.reminders;
        let count = count.filter(|_| cache.generation == self.generation);
        if let Some(entry) = cache.entries.get_mut(&self.key) {
            entry.count = count;
        }
        Reminder {
            count,
            retry_ms: EVERY.as_millis() as u64,
        }
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        lock(&self.state.0).reminders.running = false;
    }
}
fn count(value: &Value, unknown: impl Fn(&str) -> bool) -> usize {
    value["worktrees"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            row["kind"] == "linked"
                && !unknown(text(row, "path"))
                && ((row["verdict"] == "ready" && !review::local_data(row))
                    || row["verdict"] == "missing")
        })
        .count()
}

#[tauri::command]
pub async fn worktree_cleanup_reminder(
    webview: tauri::Webview,
    app: State<'_, crate::commands::AppState>,
    runtime: AppHandle,
    state: State<'_, WorktreeCleanup>,
    catalogs: State<'_, crate::newsession::NewSession>,
    input: Input,
) -> Result<Reminder, String> {
    let Input {
        computer,
        expected_machine,
        path,
        common,
    } = input;
    if webview.label() != crate::MAIN_WINDOW
        || computer.len() > 320
        || computer.chars().any(char::is_control)
        || !identity(&path, 8192)
        || !identity(&common, 8192)
    {
        return Err(CHANGED.into());
    }
    let machine = machine(&app, &computer)?;
    if machine != expected_machine {
        return Err(CHANGED.into());
    }
    let own = state.inner().clone();
    let key = (computer.clone(), machine.clone(), common.clone());
    let generation = {
        let mut model = lock(&own.0);
        if let Some(cached) = model.reminders.begin(&key, Instant::now())? {
            return Ok(cached);
        }
        model.reminders.generation
    };
    let reader = Reader {
        state: own,
        key,
        generation,
    };
    let pump = app.pump();
    let catalogs = catalogs.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = (|| -> Result<usize, String> {
            let data = catalogs.catalog(&ComputerId(computer.clone()), &path, false)?;
            if data["state"] != "ok" || data["common_dir"] != common || data["stale"] == true {
                return Err(CHANGED.into());
            }
            let value = read(&computer, &machine, &common, "")?;
            if crate::actions::blocked()
                || !pump.online(&computer)
                || pump
                    .machine(&ComputerId(computer.clone()))
                    .and_then(|m| m.machine_id)
                    .as_deref()
                    != Some(&machine)
            {
                return Err(CHANGED.into());
            }
            let cleanup = runtime.state::<crate::termination_cleanup::TerminationCleanup>();
            Ok(count(&value, |path| {
                reader.state.unknown(&machine, &common, path)
                    || cleanup.unknown(&machine, &common, path)
            }))
        })();
        // A reminder failure quietly removes the count; the ordinary explicit
        // Review path still owns diagnostics and fresh destructive checks.
        reader.finish(result.ok())
    })
    .await
    .map_err(|_| CHANGED.to_owned())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    computer: String,
    expected_machine: String,
    path: String,
    common: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn key(n: usize) -> Key {
        (
            "peer".into(),
            format!("machine-{n}"),
            "/srv/example/repo/.git".into(),
        )
    }
    #[test]
    fn attempt_interval_capacity_and_actual_reader_slot_are_bounded() {
        let mut cache = Cache::default();
        let now = Instant::now();
        assert!(cache.begin(&key(0), now).unwrap().is_none());
        assert!(cache.begin(&key(1), now).is_err());
        assert!(cache.begin(&key(0), now).unwrap().unwrap().count.is_none());
        cache.running = false;
        assert!(cache
            .begin(&key(0), now + Duration::from_secs(299))
            .unwrap()
            .is_some());
        for n in 1..REPOSITORIES {
            assert!(cache.begin(&key(n), now).unwrap().is_none());
            cache.running = false;
        }
        assert!(cache.begin(&key(REPOSITORIES), now).is_err());
        assert_eq!(cache.entries.len(), REPOSITORIES);
        assert!(cache
            .begin(&key(REPOSITORIES), now + EVERY)
            .unwrap()
            .is_none());
    }
    #[test]
    fn reader_lifetime_and_invalidation_never_publish_a_stale_count() {
        let state = WorktreeCleanup::default();
        let now = Instant::now();
        lock(&state.0).reminders.begin(&key(0), now).unwrap();
        let reader = Reader {
            state: state.clone(),
            key: key(0),
            generation: 0,
        };
        assert_eq!(reader.finish(Some(4)).count, Some(4));
        lock(&state.0).reminders.invalidate();
        assert!(reader.finish(Some(4)).count.is_none());
        assert!(lock(&state.0).reminders.running);
        drop(reader);
        assert!(!lock(&state.0).reminders.running);
        assert!(lock(&state.0)
            .reminders
            .begin(&key(0), now)
            .unwrap()
            .unwrap()
            .count
            .is_none());
    }
    #[test]
    fn verified_removal_rearms_only_its_repository_without_releasing_a_live_reader() {
        let state = WorktreeCleanup::default();
        let now = Instant::now();
        {
            let mut model = lock(&state.0);
            model.reminders.begin(&key(1), now).unwrap();
            model.reminders.running = false;
            model.reminders.begin(&key(0), now).unwrap();
        }
        let reader = Reader {
            state: state.clone(),
            key: key(0),
            generation: 0,
        };
        let (computer, machine, common) = key(0);
        {
            let mut model = lock(&state.0);
            model.reminders.removed(&computer, &machine, &common);
            assert!(model.reminders.begin(&key(0), now).is_err());
            assert!(model.reminders.begin(&key(1), now).unwrap().is_some());
        }
        assert!(reader.finish(Some(99)).count.is_none());
        assert!(!lock(&state.0).reminders.entries.contains_key(&key(0)));
        drop(reader);
        let mut model = lock(&state.0);
        assert!(model.reminders.begin(&key(0), now).unwrap().is_none());
        model.reminders.running = false;
        // Closing or refreshing again without another removal keeps the TTL.
        assert!(model.reminders.begin(&key(0), now).unwrap().is_some());
        assert!(model.reminders.begin(&key(1), now).unwrap().is_some());
    }
    #[test]
    fn count_follows_qt_local_data_rules_and_excludes_main_and_unknown_outcomes() {
        let rows = json!({"worktrees":[
            {"path":"clean","kind":"linked","verdict":"ready"},
            {"path":"missing","kind":"linked","verdict":"missing"},
            {"path":"main","kind":"main","verdict":"ready"},
            {"path":"unknown","kind":"linked","verdict":"ready"},
            {"path":"ignored","kind":"linked","verdict":"ready","ignored_count":1},
            {"path":"ignored-list","kind":"linked","verdict":"ready","ignored":[{}]},
            {"path":"ignored-bytes","kind":"linked","verdict":"ready","ignored_bytes":1},
            {"path":"blocked","kind":"linked","verdict":"blocked"}]});
        assert_eq!(count(&rows, |p| p == "unknown"), 2);
    }
}
