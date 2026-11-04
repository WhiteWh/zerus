//! Projects' adapter for the existing core clone state machine. Only a fresh
//! explicit begin can clone. Reopening a pending checkpoint offers status only.
use super::folders::Place;
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use zerus_client_core::{
    paths::Style,
    repository as repo,
    settings::{repository::Checkpoint, Settings},
    ComputerId,
};

#[derive(Clone, Serialize)]
pub struct View {
    id: String,
    url: String,
    parent: String,
    name: String,
    destination: String,
    status: String,
    busy: bool,
    cloning: bool,
    uncertain: bool,
    ready: bool,
    locked: bool,
    can_submit: bool,
    #[serde(skip)]
    cancelled: bool,
}
struct Work {
    model: repo::Clone,
    checkpoint: Checkpoint,
}
struct Session {
    key: String,
    place: Place,
    work: Mutex<Work>,
    view: Mutex<View>,
    active: AtomicBool,
}
#[derive(Clone, Default)]
pub struct Clones(Arc<Mutex<VecDeque<Arc<Session>>>>);
fn lock<T>(value: &Mutex<T>) -> Result<MutexGuard<'_, T>, String> {
    value
        .lock()
        .map_err(|_| "Clone recovery is unavailable. Reopen Zerus before continuing.".into())
}
fn view(id: String, model: &repo::Clone) -> View {
    View {
        id,
        url: model.url().into(),
        parent: model.parent().into(),
        name: model.name().into(),
        destination: model.destination(),
        status: if model.created_path().is_empty() {
            model.status_text()
        } else {
            repo::READY
        }
        .into(),
        busy: model.busy(),
        cloning: model.cloning(),
        uncertain: model.uncertain(),
        ready: !model.created_path().is_empty(),
        locked: model.fields_locked(),
        can_submit: model.can_submit(),
        cancelled: false,
    }
}
fn proof(org: &Organization, place: &Place) -> Result<(), String> {
    if !org.0.allowed {
        return Err(REPLAY_NEEDS_HGS.into());
    }
    folders::machine(&org.0.pump, place)?;
    let meta = org
        .0
        .pump
        .machine(&ComputerId(place.computer.clone()))
        .ok_or("Computer unavailable.")?;
    if meta.ok != Some(true)
        || !place
            .machine_id
            .as_deref()
            .is_some_and(zerus_client_core::creation::canonical_uuid)
    {
        return Err("Waiting for this computer's identity.".into());
    }
    Ok(())
}
impl Clones {
    fn find(&self, id: &str) -> Result<Arc<Session>, String> {
        lock(&self.0)?
            .iter()
            .find(|s| lock(&s.view).is_ok_and(|v| v.id == id))
            .cloned()
            .ok_or_else(|| "This clone dialog expired. Open it again.".into())
    }
    fn open(&self, org: &Organization, settings: &Settings, place: Place) -> Result<View, String> {
        proof(org, &place)?;
        let mut hash = Sha256::new();
        for bytes in [
            org.0.hgs.as_os_str().as_encoded_bytes(),
            place.computer.as_bytes(),
            place.host.as_bytes(),
            place.machine_id.as_deref().unwrap_or("").as_bytes(),
        ] {
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        let key = format!("{:x}", hash.finalize());
        let mut sessions = lock(&self.0)?;
        if let Some(session) = sessions.iter().find(|s| s.key == key) {
            let mut current = lock(&session.view)?;
            if !session.active.load(Ordering::Acquire) {
                current.cancelled = false;
            }
            return Ok(current.clone());
        }
        if sessions.len() >= 8 {
            let at = sessions
                .iter()
                .position(|s| !s.active.load(Ordering::Acquire))
                .ok_or("Too many clone dialogs are busy.")?;
            sessions.remove(at);
        }
        let checkpoint = settings.repository_checkpoint(&key)?;
        let model = repo::Clone::new(
            Style::Posix,
            checkpoint.parent.as_deref(),
            checkpoint.pending.clone(),
        );
        let v = view(uuid::Uuid::new_v4().to_string(), &model);
        sessions.push_back(Arc::new(Session {
            key,
            place,
            work: Mutex::new(Work { model, checkpoint }),
            view: Mutex::new(v.clone()),
            active: AtomicBool::new(false),
        }));
        Ok(v)
    }
}
fn publish(session: &Session, model: &repo::Clone) -> Result<bool, String> {
    let mut old = lock(&session.view)?;
    if old.cancelled {
        return Ok(false);
    }
    *old = view(old.id.clone(), model);
    Ok(true)
}
struct Active<'a>(&'a Session);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::Release);
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Begin,
    Check,
    Retire,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    url: String,
    parent: String,
    name: String,
}
fn execute(
    session: &Session,
    org: &Organization,
    settings: &Settings,
    step: Step,
    source: Source,
) -> Result<View, String> {
    if session
        .active
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("This clone is already busy.".into());
    }
    let _active = Active(session);
    proof(org, &session.place)?;
    if source.url.len() > 4096
        || source.parent.len() > 32768
        || source.name.chars().count() > 80
        || [&source.url, &source.parent, &source.name]
            .iter()
            .any(|s| s.chars().any(char::is_control))
    {
        return Err("Invalid repository destination.".into());
    }
    let mut work = lock(&session.work)?;
    if lock(&session.view)?.cancelled {
        return Err("This clone dialog closed.".into());
    }
    work.model
        .set_source(&source.url, &source.parent, &source.name);
    let mut next = match step {
        Step::Begin => work.model.begin(),
        Step::Check => work.model.check_result(),
        Step::Retire => work.model.choose_another_destination(),
    };
    // One explicit request can only check, reserve, and clone once (or status
    // once). The core owns all transitions and ignores stale call tickets.
    loop {
        if !publish(session, &work.model)? {
            work.model = repo::Clone::new(
                Style::Posix,
                work.checkpoint.parent.as_deref(),
                work.checkpoint.pending.clone(),
            );
            break;
        }
        if let Some(effect) = next.store.take() {
            let mut changed = work.checkpoint.clone();
            changed.apply(&effect);
            let saved = settings.replace_repository_checkpoint(
                &session.key,
                &work.checkpoint,
                changed.clone(),
            );
            if let Err(error) = saved {
                work.model.saved(false);
                publish(session, &work.model)?;
                return Err(error);
            }
            work.checkpoint = changed;
            if matches!(effect, repo::Store::Pending(_)) {
                next = work.model.saved(true);
                continue;
            }
        }
        let Some((ticket, action)) = next.run.take() else {
            break;
        };
        let result = (|| {
            proof(org, &session.place)?;
            if !publish(session, &work.model)? {
                return Err("Clone cancelled.".into());
            }
            if action == repo::Action::Check {
                repo::supported(
                    repo::run(
                        &org.0.hgs,
                        &repo::help_invocation(
                            &ComputerId(session.place.computer.clone()),
                            session.place.machine_id.as_deref().unwrap_or(""),
                        ),
                    ),
                    &session.place.host,
                )?;
                proof(org, &session.place)?;
                if lock(&session.view)?.cancelled {
                    return Err("Clone cancelled.".into());
                }
            }
            let call = work.model.invocation(
                &ComputerId(session.place.computer.clone()),
                action,
                session.place.machine_id.as_deref().unwrap_or(""),
            );
            repo::reply(
                repo::run(&org.0.hgs, &call),
                &ComputerId(session.place.computer.clone()),
                action,
                work.model.name(),
                call.args
                    .windows(2)
                    .find(|a| a[0] == "--request-id")
                    .map(|a| a[1].as_str())
                    .unwrap_or(""),
            )
        })();
        next = work
            .model
            .finished(ticket, result, &uuid::Uuid::new_v4().to_string());
    }
    let mut current = view(lock(&session.view)?.id.clone(), &work.model);
    current.cancelled = lock(&session.view)?.cancelled;
    *lock(&session.view)? = current.clone();
    Ok(current)
}
#[tauri::command]
pub async fn project_clone_open(
    state: State<'_, Organization>,
    clones: State<'_, Clones>,
    settings: State<'_, crate::settings::Store>,
    place: Place,
) -> Result<View, String> {
    let (org, clones, settings) = (
        state.inner().clone(),
        clones.inner().clone(),
        settings.0.clone(),
    );
    tauri::async_runtime::spawn_blocking(move || clones.open(&org, &settings, place))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn project_clone_status(clones: State<'_, Clones>, id: String) -> Result<View, String> {
    Ok(lock(&clones.find(&id)?.view)?.clone())
}
#[tauri::command]
pub fn project_clone_close(clones: State<'_, Clones>, id: String) -> Result<(), String> {
    let session = clones.find(&id)?;
    let mut v = lock(&session.view)?;
    if v.cloning && session.active.load(Ordering::Acquire) {
        return Err("Cloning repository…".into());
    }
    v.cancelled = true;
    Ok(())
}
#[tauri::command]
pub async fn project_clone_step(
    state: State<'_, Organization>,
    clones: State<'_, Clones>,
    settings: State<'_, crate::settings::Store>,
    id: String,
    step: Step,
    source: Source,
) -> Result<View, String> {
    let (org, session, settings) = (state.inner().clone(), clones.find(&id)?, settings.0.clone());
    tauri::async_runtime::spawn_blocking(move || execute(&session, &org, &settings, step, source))
        .await
        .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub struct Placed {
    outcome: EditOutcome,
    project: String,
    folder: String,
}
#[tauri::command]
pub async fn project_clone_use(
    state: State<'_, Organization>,
    clones: State<'_, Clones>,
    id: String,
    project: Option<String>,
    name: String,
) -> Result<Placed, String> {
    let (org, session) = (state.inner().clone(), clones.find(&id)?);
    tauri::async_runtime::spawn_blocking(move || {
        if session
            .active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err("This clone is already busy.".into());
        }
        let _active = Active(&session);
        proof(&org, &session.place)?;
        if lock(&session.view)?.cancelled {
            return Err("This clone dialog closed.".into());
        }
        let mut work = lock(&session.work)?;
        if work.model.created_path().is_empty() || work.model.busy() || work.model.uncertain() {
            return Err(repo::PREVIOUS_UNCONFIRMED.into());
        }
        let folder = uuid::Uuid::new_v4().to_string();
        let target = project
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let (id, folder_id, path, place) = (
            target.clone(),
            folder.clone(),
            work.model.created_path().to_owned(),
            session.place.clone(),
        );
        let outcome = org.submit_checked(move |pump| {
            let change = zerus_client_core::swarm::folder_edit::Change::Set {
                original: None,
                id: folder_id,
                machine: folders::machine(pump, &place)?,
                path,
                name: String::new(),
            };
            Ok(if project.is_some() {
                Intent::Folder {
                    project: id,
                    change,
                }
            } else {
                Intent::CreateFolder {
                    id,
                    name,
                    folder: change,
                }
            })
        });
        // The exact created/link identity is returned, never found by its name.
        if !matches!(outcome, EditOutcome::Refused { .. }) {
            work.model = repo::Clone::new(Style::Posix, work.checkpoint.parent.as_deref(), None);
            let dialog_id = lock(&session.view)?.id.clone();
            *lock(&session.view)? = view(dialog_id, &work.model);
        }
        Ok(Placed {
            outcome,
            project: target,
            folder,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    struct Fixture {
        org: Organization,
        settings: Settings,
        dir: PathBuf,
        _tree: std::sync::RwLockReadGuard<'static, ()>,
    }
    impl Fixture {
        fn new(gate: bool) -> Self {
            let tree = crate::PROCESS_TREE
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            let dir =
                std::env::temp_dir().join(format!("zerus-clone-adapter-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let hgs = dir.join("hgs");
            std::os::unix::fs::symlink(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../core/tests/support/fake_hgs_repository.py"),
                &hgs,
            )
            .unwrap();
            std::fs::write(
                dir.join("config.json"),
                json!({"machine":MACHINE,"gate":gate,"result":"unconfirmed"}).to_string(),
            )
            .unwrap();
            let pump = Pump::default();
            pump.push(Change::Machine {
                computer: ComputerId::local(),
                machine: zerus_client_core::MachineMeta {
                    host: Some("workstation".into()),
                    machine_id: Some(MACHINE.into()),
                    platform: Some("linux".into()),
                    ok: Some(true),
                    ..Default::default()
                },
            });
            let org = Organization::new(
                pump,
                hgs,
                true,
                dir.join("layout"),
                dir.join("draft"),
                false,
            );
            let settings = Settings::open(dir.join("settings.json"), None);
            Self {
                org,
                settings,
                dir,
                _tree: tree,
            }
        }
        fn calls(&self) -> Vec<Vec<String>> {
            std::fs::read_to_string(self.dir.join("calls.jsonl"))
                .unwrap_or_default()
                .lines()
                .map(|s| serde_json::from_str(s).unwrap())
                .collect()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.org.flush(); /* worker owns no source/edits */
        }
    }
    const MACHINE: &str = "00000000-0000-4000-8000-000000000001";
    fn place() -> Place {
        Place {
            computer: String::new(),
            host: "workstation".into(),
            machine_id: Some(MACHINE.into()),
        }
    }
    fn source() -> Source {
        Source {
            url: "https://example.org/repo.git".into(),
            parent: "/srv/example".into(),
            name: "repo".into(),
        }
    }
    #[test]
    fn durable_reservation_unknown_reopen_status_only_and_changed_owner_refusal() {
        let f = Fixture::new(false);
        let clones = Clones::default();
        let v = clones.open(&f.org, &f.settings, place()).unwrap();
        assert!(f.calls().is_empty());
        let session = clones.find(&v.id).unwrap();
        let v = execute(&session, &f.org, &f.settings, Step::Begin, source()).unwrap();
        assert!(v.uncertain && !v.busy);
        assert_eq!(
            f.calls()
                .iter()
                .filter(|a| a.windows(2).any(|w| w == ["repository", "clone"]))
                .count(),
            1
        );
        let clones = Clones::default();
        let v = clones.open(&f.org, &f.settings, place()).unwrap();
        assert!(v.uncertain);
        let session = clones.find(&v.id).unwrap();
        execute(&session, &f.org, &f.settings, Step::Begin, source()).unwrap();
        assert_eq!(f.calls().len(), 3);
        execute(&session, &f.org, &f.settings, Step::Check, source()).unwrap();
        assert_eq!(f.calls().last().unwrap()[3], "status");
        f.org.0.pump.push(Change::Machine {
            computer: ComputerId::local(),
            machine: zerus_client_core::MachineMeta {
                host: Some("workstation".into()),
                machine_id: Some("00000000-0000-4000-8000-000000000002".into()),
                ok: Some(true),
                ..Default::default()
            },
        });
        assert!(execute(&session, &f.org, &f.settings, Step::Check, source()).is_err());
        assert_eq!(f.calls().len(), 4);
    }
    #[test]
    fn closing_during_check_never_reserves_or_clones() {
        let f = Fixture::new(true);
        let clones = Clones::default();
        let v = clones.open(&f.org, &f.settings, place()).unwrap();
        let session = clones.find(&v.id).unwrap();
        std::thread::scope(|scope| {
            let worker =
                scope.spawn(|| execute(&session, &f.org, &f.settings, Step::Begin, source()));
            let limit = Instant::now() + Duration::from_secs(5);
            while !f.dir.join("checking").exists() && Instant::now() < limit {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(f.dir.join("checking").exists());
            lock(&session.view).unwrap().cancelled = true;
            std::fs::write(f.dir.join("release"), b"").unwrap();
            worker.join().unwrap().unwrap();
        });
        assert_eq!(f.calls().len(), 2);
        assert!(!f.dir.join("settings.json").exists());
        assert!(!lock(&session.work).unwrap().model.busy());
    }
    #[test]
    fn closing_before_worker_admission_is_sticky() {
        let f = Fixture::new(false);
        let clones = Clones::default();
        let v = clones.open(&f.org, &f.settings, place()).unwrap();
        let session = clones.find(&v.id).unwrap();
        lock(&session.view).unwrap().cancelled = true;
        assert!(execute(&session, &f.org, &f.settings, Step::Begin, source()).is_err());
        assert!(f.calls().is_empty());
    }
    #[test]
    fn failed_durable_save_never_dispatches_clone() {
        let f = Fixture::new(false);
        let clones = Clones::default();
        let v = clones.open(&f.org, &f.settings, place()).unwrap();
        let session = clones.find(&v.id).unwrap();
        std::fs::create_dir(f.dir.join("settings.json")).unwrap();
        assert!(execute(&session, &f.org, &f.settings, Step::Begin, source()).is_err());
        assert_eq!(f.calls().len(), 2);
        assert!(!lock(&session.work).unwrap().model.busy());
    }
}
