//! New session's commands, over the core's launch, repository and worktree
//! rules: the form's derived state, Start and the background plan, the local
//! Windows-session warning, the repository fields, folder catalogs, manual
//! worktrees, folder listings and creation, the local folder picker and
//! OpenCode's models, favorites and service start.
//!
//! Opening or inspecting the dialog only reads: `worktrees --path`, `dirs`
//! and `account models`. The calls that create are each named by a click and
//! send one request identity: Create worktree (`create_worktree`), Create in
//! the folder browser (`create_directory`), a star (`opencode_favorite`) and
//! Start OpenCode on {computer} (`opencode_start`). Nothing here retries them,
//! and a generated worktree is created only by an admitted creation.
//!
//! Every `hgs` call blocks on Tauri's blocking pool; the commands are async.
//! A replay runs no call unless `ZERUS_CLIENT_HGS` names one, as the actions.
//!
//! Reference: `tray/src/NewSessionDialog.cpp`, `NewWorktreeDialog.h`,
//! `DirectoryDialog.cpp`, `LocalSessionHost.cpp` and `tray/src/HgsClient.cpp`
//! (`requestWorktrees`, `createWorktree`, `requestDirectories`,
//! `createDirectory`, `requestOpenCodeModels`, `requestOpenCodeFavorite`,
//! `requestAccounts`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, State};
use zerus_client_core::actions::{Exit, Invocation};
use zerus_client_core::launch::{self, Choices, Form, Plan, Project, Shown, Start};
use zerus_client_core::machine::MachineMeta;
use zerus_client_core::paths::{self, Style};
use zerus_client_core::repository;
use zerus_client_core::worktrees::{self, Catalogs, Stamp};
use zerus_client_core::ComputerId;

/// What a replay without an explicit `hgs` answers instead of calling it.
pub const REPLAY_NEEDS_HGS: &str = "New session in replay mode needs ZERUS_CLIENT_HGS.";
/// The local folder picker's title (`DirectoryDialog::chooseDirectory`).
pub const CHOOSE_A_FOLDER: &str = "Choose a folder";
/// The largest reply of a creating call or the service start.
const REPLY_LIMIT: usize = zerus_client_core::reads::READ_LIMIT;
/// The largest help text the folder creation's probe reads.
const HELP_LIMIT: usize = 128 * 1024;

/// Seconds since the Unix epoch, the clock folder catalogs are dated by.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One catalog read in flight; reads of the same computer and folder that
/// start meanwhile, from the same cache generations, wait for its answer.
struct Flight {
    stamp: Stamp,
    answer: Mutex<Option<Result<Value, String>>>,
    done: Condvar,
}

impl Flight {
    fn finish(&self, answer: Result<Value, String>) {
        *lock(&self.answer) = Some(answer);
        self.done.notify_all();
    }

    fn wait(&self) -> Result<Value, String> {
        let mut answer = lock(&self.answer);
        loop {
            if let Some(answer) = answer.as_ref() {
                return answer.clone();
            }
            answer = self
                .done
                .wait(answer)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// The running read's flight: it leaves the map and answers its waiters,
/// also when the read panics.
struct Finish<'a> {
    flights: &'a Mutex<HashMap<(String, String), Arc<Flight>>>,
    key: (String, String),
    flight: Arc<Flight>,
}

impl Finish<'_> {
    /// Leaves the map (when still the folder's flight), then answers.
    fn answer(&self, answer: Result<Value, String>) {
        let mut flights = lock(self.flights);
        if flights
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.flight))
        {
            flights.remove(&self.key);
        }
        drop(flights);
        if lock(&self.flight.answer).is_none() {
            self.flight.finish(answer);
        }
    }
}

impl Drop for Finish<'_> {
    fn drop(&mut self) {
        self.answer(Err("The worktree catalog read stopped.".into()));
    }
}

struct Shared {
    hgs: PathBuf,
    allowed: bool,
    catalogs: Mutex<Catalogs>,
    flights: Mutex<HashMap<(String, String), Arc<Flight>>>,
    /// Where the snapshots are kept; None keeps them in memory only.
    file: Option<PathBuf>,
    /// Snapshot states in order, and the last one written.
    sequence: AtomicU64,
    written: Mutex<u64>,
    clock: Clock,
}

/// The managed state of New session's commands; clones share it.
#[derive(Clone)]
pub struct NewSession(Arc<Shared>);

/// Reads the saved snapshots, at most twice their bound.
fn load(file: &Path) -> Option<Value> {
    use std::io::Read;
    let limit = 2 * worktrees::SNAPSHOT_BYTES as u64;
    let mut bytes = Vec::new();
    std::fs::File::open(file)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > limit {
        return None;
    }
    worktrees::decode_saved(&bytes)
}

/// Replaces `file` through a sibling, so a crash never leaves half of it.
fn write(file: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = file.with_extension("json.tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, file)
}

/// Runs a call bounded at `limit`; a larger reply reads as empty, which the
/// receipt checks refuse.
fn run_bounded(hgs: &Path, invocation: &Invocation, limit: usize) -> Exit {
    zerus_client_core::run::run_limited(hgs, invocation, &[], limit).unwrap_or(Exit::Code {
        code: 0,
        stdout: Vec::new(),
        stderr: String::new(),
    })
}

/// The name people know a computer by, for the update text: the one the view
/// passes, else the alias, else "this computer".
fn computer_name<'a>(computer: &'a ComputerId, name: &'a str) -> &'a str {
    if name.is_empty() {
        computer.alias().unwrap_or("this computer")
    } else {
        name
    }
}

impl NewSession {
    /// `file` keeps the folder catalogs' snapshots across restarts (None
    /// keeps them in memory only); `clock` dates them.
    pub fn new(hgs: PathBuf, allowed: bool, file: Option<PathBuf>, clock: Clock) -> Self {
        let saved = file.as_deref().and_then(load);
        let catalogs = Catalogs::new(&hgs.to_string_lossy(), saved);
        Self(Arc::new(Shared {
            hgs,
            allowed,
            catalogs: Mutex::new(catalogs),
            flights: Mutex::new(HashMap::new()),
            file,
            sequence: AtomicU64::new(0),
            written: Mutex::new(0),
            clock,
        }))
    }

    /// The app's: `default_hgs()`, the replay rule of the actions and the
    /// system clock.
    pub fn tauri(file: Option<PathBuf>) -> Self {
        let var = |name: &str| std::env::var(name).ok();
        let allowed = crate::actions::actions_allowed(
            var("ZERUS_CLIENT_REPLAY").as_deref(),
            var("ZERUS_CLIENT_HGS").as_deref(),
        );
        Self::new(
            zerus_client_core::default_hgs(),
            allowed,
            file,
            Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0.0, |elapsed| elapsed.as_secs_f64())
            }),
        )
    }

    fn allowed(&self) -> Result<(), String> {
        if self.0.allowed {
            Ok(())
        } else {
            Err(REPLAY_NEEDS_HGS.into())
        }
    }

    fn now(&self) -> f64 {
        (self.0.clock)()
    }

    /// Writes the snapshots when a file keeps them; a newer state already
    /// written makes an older one unnecessary.
    fn save(&self) {
        let Some(file) = &self.0.file else {
            return;
        };
        let (sequence, saved) = {
            let catalogs = lock(&self.0.catalogs);
            (
                self.0.sequence.fetch_add(1, Ordering::SeqCst) + 1,
                catalogs.saved(),
            )
        };
        let mut written = lock(&self.0.written);
        if *written >= sequence {
            return;
        }
        if let Err(error) = write(file, &saved.to_string()) {
            eprintln!(
                "zerus-client: could not save the worktree snapshots to {}: {error}",
                file.display()
            );
        }
        *written = sequence;
    }

    /// The saved snapshot of a folder, fresh or not, or `{}` (1087).
    pub fn snapshot(&self, computer: &ComputerId, path: &str) -> Value {
        lock(&self.0.catalogs)
            .snapshot(&computer.0, path)
            .cloned()
            .unwrap_or_else(|| json!({}))
    }

    /// A folder's worktree catalog (`requestWorktrees`, 1515-1563): a
    /// snapshot under 30 s old and not stale answers without a call unless
    /// `refresh`; reads of the same computer and folder in flight share one
    /// call; the answer is kept only while neither its repository nor its
    /// computer changed meanwhile. Blocks.
    pub fn catalog(
        &self,
        computer: &ComputerId,
        path: &str,
        refresh: bool,
    ) -> Result<Value, String> {
        self.allowed()?;
        let host = computer.0.clone();
        let key = (host.clone(), path.to_owned());
        let (flight, runs) = {
            let catalogs = lock(&self.0.catalogs);
            if !refresh {
                if let Some(fresh) = catalogs.fresh(&host, path, self.now()) {
                    return Ok(fresh.clone());
                }
            }
            let stamp = catalogs.stamp(&host, path);
            let mut flights = lock(&self.0.flights);
            match flights.get(&key) {
                Some(flight) if flight.stamp == stamp => (Arc::clone(flight), false),
                _ => {
                    let flight = Arc::new(Flight {
                        stamp,
                        answer: Mutex::new(None),
                        done: Condvar::new(),
                    });
                    flights.insert(key.clone(), Arc::clone(&flight));
                    (flight, true)
                }
            }
        };
        if !runs {
            return flight.wait();
        }
        let finish = Finish {
            flights: &self.0.flights,
            key,
            flight: Arc::clone(&flight),
        };
        let invocation = worktrees::catalog_invocation(computer, path, refresh);
        let exit = worktrees::run_catalog(&self.0.hgs, &invocation);
        let answer = lock(&self.0.catalogs).accept(&host, path, &flight.stamp, exit, self.now());
        finish.answer(answer.clone());
        if answer.is_ok() {
            self.save();
        }
        answer
    }

    pub(crate) fn invalidate_worktrees(&self, computer: &ComputerId, common_dir: &str) {
        lock(&self.0.catalogs).invalidate(&computer.0, common_dir);
        self.save();
    }

    /// Create worktree in the manual dialog (`NewWorktreeDialog`, with
    /// `createWorktree` 1565-1590): one fresh request identity, no expected
    /// machine and no receipt, as Qt sends it. `catalog` is the folder's
    /// catalog the dialog opened with: its `common_dir` names the repository
    /// (`NewWorktreeDialog.h:22`). The repository's catalogs are invalidated on
    /// every outcome. The new checkout, with `catalog` listing it as verified
    /// (`launch::created_catalog`, NewSessionDialog.cpp 1137-1141), or Qt's text.
    pub fn create_worktree(
        &self,
        computer: &ComputerId,
        path: &str,
        catalog: &Value,
        branch: &str,
        base: &str,
        destination: &str,
    ) -> Result<CreatedWorktree, String> {
        self.allowed()?;
        let common_dir = catalog["common_dir"].as_str().unwrap_or_default();
        let request_id = uuid::Uuid::new_v4().to_string();
        let invocation = worktrees::create_invocation(
            computer,
            path,
            common_dir,
            branch,
            base,
            destination,
            &request_id,
            false,
            "",
        );
        let exit = run_bounded(&self.0.hgs, &invocation, worktrees::CATALOG_LIMIT);
        lock(&self.0.catalogs).invalidate(&computer.0, common_dir);
        self.save();
        let data = worktrees::created(exit, &request_id, branch, common_dir, false, destination)?;
        let path = data["path"].as_str().unwrap_or_default().to_owned();
        Ok(CreatedWorktree {
            catalog: launch::created_catalog(catalog, &path),
            path,
        })
    }

    /// A folder listing (`requestDirectories`): 5 s here, 15 s on a peer.
    pub fn directories(
        &self,
        computer: &ComputerId,
        path: &str,
        hidden: bool,
    ) -> Result<Value, String> {
        self.allowed()?;
        let invocation = launch::directories_invocation(computer, path, hidden);
        launch::directories(launch::run_directories(&self.0.hgs, &invocation))
    }

    /// New folder in the folder browser (`createDirectory`, 1810-1870): the
    /// selected CLI must offer `dirs --create`, the canonical parent is pinned
    /// first, then the folder is created under one fresh identity and its
    /// receipt checked. Folders take this computer's form here and POSIX on a
    /// peer, as Qt's.
    pub fn create_directory(&self, computer: &ComputerId, path: &str) -> Result<Value, String> {
        self.allowed()?;
        let hgs = &self.0.hgs;
        let style = paths::of(None, computer.is_local());
        let probe = launch::directory_create_probe(computer);
        launch::directory_create_supported(run_bounded(hgs, &probe, HELP_LIMIT), computer)?;
        let (parent, leaf) = launch::directory_parent(path, style);
        let listing = launch::directories_invocation(computer, &parent, false);
        let parent =
            launch::directory_parent_confirmed(launch::run_directories(hgs, &listing), style)?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let create = launch::directory_create_invocation(computer, path, &request_id);
        let exit = run_bounded(hgs, &create, launch::DIRECTORIES_LIMIT);
        launch::directory_created(exit, &request_id, path, &parent, &leaf, style)
    }

    /// The folder's OpenCode model catalog (`requestOpenCodeModels`), its
    /// failure worded as `OpenCodeModels::errorText` for `name`.
    pub fn opencode_models(
        &self,
        computer: &ComputerId,
        folder: &str,
        name: &str,
    ) -> Result<Value, String> {
        self.allowed()?;
        let invocation = launch::opencode_models_invocation(computer, folder);
        launch::run_opencode_models(&self.0.hgs, &invocation)
            .map_err(|error| launch::opencode_error_text(&error, computer_name(computer, name)))
    }

    /// A star (`requestOpenCodeFavorite`): one fresh request identity; the
    /// receipt must name this change. Never sent again on its own.
    pub fn opencode_favorite(
        &self,
        computer: &ComputerId,
        value: &str,
        favorite: bool,
        name: &str,
    ) -> Result<Value, String> {
        self.allowed()?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let invocation =
            launch::opencode_favorite_invocation(computer, value, favorite, &request_id);
        let exit = run_bounded(&self.0.hgs, &invocation, REPLY_LIMIT);
        launch::opencode_favorite(exit, &request_id, value, favorite)
            .map_err(|error| launch::opencode_error_text(&error, computer_name(computer, name)))
    }

    /// Start OpenCode on {computer} (`requestAccounts {"start",
    /// "native-opencode", "--json"}`, 60 s); the view then reads the
    /// computer's accounts again.
    pub fn opencode_start(&self, computer: &ComputerId) -> Result<Value, String> {
        self.allowed()?;
        let invocation = launch::opencode_start_invocation(computer);
        launch::opencode_started(run_bounded(&self.0.hgs, &invocation, REPLY_LIMIT))
    }
}

/// A worktree Create manually… created: its path, and the catalog the dialog
/// opened with listing it as an available linked worktree, which New session
/// keeps as that folder's verified catalog.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CreatedWorktree {
    pub path: String,
    pub catalog: Value,
}

/// The choices the next dialog starts with (`workspace/launchAgent`,
/// `workspace/launchAccounts` as `{host: {agent: account}}` and
/// `workspace/launchOpenTerminal`), read from the client's settings store.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Remembered {
    /// Empty for none.
    pub agent: String,
    pub accounts: Value,
    pub open_terminal: bool,
}

/// Reading invalid account JSON gives Qt's empty object without rewriting
/// the stored text. Only a later durable creation remembers new choices.
pub fn remembered(settings: &zerus_client_core::settings::Settings) -> Remembered {
    Remembered {
        agent: settings.text("workspace/launchAgent"),
        accounts: serde_json::from_str::<Value>(&settings.text("workspace/launchAccounts"))
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({})),
        open_terminal: settings.flag("workspace/launchOpenTerminal"),
    }
}

/// What a new dialog starts with.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Context {
    /// `work-` and eight lower-case hex digits of a fresh UUID (151).
    pub branch: String,
    /// This process's Windows session; None off Windows.
    pub own_windows_session: Option<u64>,
    /// The platform this client was built for (`paths::build`): this
    /// computer's folder style before it reports a platform.
    pub build_style: Style,
    pub remembered: Remembered,
    /// The catalog's `swarm_id`, empty before one arrives: a folder's
    /// project inference counts only from this swarm (SessionsWindow.cpp:2922).
    pub swarm: String,
}

/// A new dialog's context; the command adds the swarm.
pub fn context() -> Context {
    Context {
        branch: launch::generated_branch(&uuid::Uuid::new_v4().simple().to_string()),
        own_windows_session: launch::own_windows_session(),
        build_style: paths::build(),
        remembered: Remembered {
            agent: String::new(),
            accounts: json!({}),
            open_terminal: false,
        },
        swarm: String::new(),
    }
}

/// What the form passes; the command derives the rest with the core.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct FormInput {
    /// Its `automatic_available`, `automatic_path` and `project_worktree` may
    /// be left out: they are derived here. `catalog_pending` is Qt's
    /// `m_catalogRequest`.
    pub form: Form,
    /// The folder's catalog (`m_worktreeCatalog`).
    #[serde(default)]
    pub catalog: Value,
    /// The verified catalog of this computer and folder
    /// (`m_verifiedWorktrees[host\npath]`), or null.
    #[serde(default)]
    pub verified: Value,
    #[serde(default)]
    pub project: Option<Project>,
    /// This computer "" or the alias, as the project's folders name it.
    #[serde(default)]
    pub host: String,
    /// This computer's machine card: with it, `can_enable_local_sessions`
    /// is derived here (`LocalSessionHost::canEnable`, 729).
    #[serde(default)]
    pub local: Option<MachineMeta>,
}

/// The form's state with what the core derived for it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FormView {
    pub shown: Shown,
    pub automatic_available: bool,
    pub automatic_path: String,
    pub project_worktree: bool,
}

/// The form with Auto, its path and the project worktree derived (1092-1113).
fn derived(input: FormInput) -> Form {
    let FormInput {
        mut form,
        catalog,
        verified,
        project,
        host,
        local,
    } = input;
    if let Some(local) = &local {
        form.can_enable_local_sessions = launch::can_enable_local_sessions(local);
    }
    form.automatic_available = launch::automatic_worktree_available(
        &catalog,
        &form.path,
        form.style,
        form.catalog_pending,
    );
    form.automatic_path = launch::automatic_worktree_path(&catalog, &form.branch, form.style);
    let project = project.as_ref();
    form.project_worktree =
        launch::project_worktree(&form.path, &catalog, project, &host, form.style)
            || launch::project_worktree(&form.path, &verified, project, &host, form.style);
    form
}

/// The form's view (`updateForm`).
pub fn form_view(input: FormInput) -> FormView {
    let form = derived(input);
    FormView {
        shown: launch::shown(&form),
        automatic_available: form.automatic_available,
        automatic_path: form.automatic_path,
        project_worktree: form.project_worktree,
    }
}

/// The local Windows-session warning's text when Start must ask first
/// (`LocalSessionHost::confirmLaunch`); `computer` is "" for this one.
pub fn warning(local: &MachineMeta, computer: &str, own: Option<u64>) -> Option<String> {
    launch::other_windows_session(local, !computer.is_empty(), own).map(launch::warning_text)
}

/// The repository source's fields.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RepositoryFields {
    pub url: String,
    /// The destination's parent and name, as `editRepositorySource` splits it.
    pub parent: String,
    pub name: String,
    /// The Clone to field's text.
    pub field: String,
    /// `PARENT/NAME`, the clone's destination as Start and the plan take it.
    pub destination: String,
    /// The folder name the URL suggests.
    pub suggested: String,
    pub can_submit: bool,
}

/// `editRepositorySource` (628-639): `\` reads as `/` on Windows, a separator
/// at 0 gives the parent `/` and a parent `X:` becomes `X:/`.
fn split_destination(destination: &str, style: Style) -> (String, String) {
    let destination = match style {
        Style::Windows => destination.replace('\\', "/"),
        Style::Posix => destination.to_owned(),
    };
    match destination.rfind('/') {
        None => (String::new(), destination),
        Some(0) => ("/".into(), destination[1..].to_owned()),
        Some(at) => {
            let mut parent = destination[..at].to_owned();
            if style == Style::Windows && parent.ends_with(':') {
                parent.push('/');
            }
            (parent, destination[at + 1..].to_owned())
        }
    }
}

/// The repository source's fields over a fresh clone record (the client
/// keeps no pending clone): with `fresh` (a new computer or style,
/// `syncRepositoryComputer` 608-627) the destination starts in `~` with the
/// old record's folder name, kept even when empty (613); without a custom
/// destination it follows the URL (289-295), from `~` while the field is
/// empty (the first record, named after the URL); a custom one is kept as
/// typed.
pub fn repository_source(
    url: &str,
    destination: &str,
    style: Style,
    custom: bool,
    fresh: Option<&str>,
) -> RepositoryFields {
    let mut clone = repository::Clone::new(style, None, None);
    let suggested = repository::suggested_name(url);
    let field = match fresh {
        Some(name) => paths::native(&repository::destination(clone.parent(), name), style),
        None if custom => destination.to_owned(),
        None => {
            let parent = if destination.is_empty() {
                clone.parent().to_owned()
            } else {
                split_destination(destination, style).0
            };
            paths::native(&format!("{parent}/{suggested}"), style)
        }
    };
    let (parent, name) = split_destination(&field, style);
    clone.set_source(url, &parent, &name);
    RepositoryFields {
        url: url.to_owned(),
        destination: clone.destination(),
        can_submit: clone.can_submit(),
        parent,
        name,
        field,
        suggested,
    }
}

/// Where the local picker opens (`DirectoryDialog.cpp:40-45`): the home
/// folder for an empty start or `~`, `~/` from home.
fn start_folder(start: &str, home: Option<&Path>) -> Option<PathBuf> {
    if start.is_empty() || start == "~" {
        home.map(Path::to_path_buf)
    } else if let Some(rest) = start.strip_prefix("~/") {
        home.map(|home| home.join(rest))
    } else {
        Some(PathBuf::from(start))
    }
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(job)
        .await
        .map_err(|error| error.to_string())?
}

/// A new dialog's context, with the swarm of the organization the list
/// shows (`crate::organization`): a read of what the app holds.
#[tauri::command(async)]
pub fn new_session_context(app: AppHandle) -> Context {
    use tauri::Manager;
    let swarm = app
        .try_state::<crate::organization::Organization>()
        .map(|organization| organization.swarm_id())
        .unwrap_or_default();
    let remembered = remembered(&app.state::<crate::settings::Store>().0);
    Context {
        swarm,
        remembered,
        ..context()
    }
}

#[tauri::command(async)]
pub fn new_session_form(input: FormInput) -> FormView {
    form_view(input)
}

/// What Start does on the same derived form, with background creation
/// (`start`, 743-778). The view asks again at the click, so a stale view
/// never starts anything.
#[tauri::command(async)]
pub fn new_session_start(input: FormInput, submitted: bool) -> Start {
    launch::start(&derived(input), true, submitted)
}

/// The background plan at the click (`start`, 757-776): the form's folder,
/// agent, name, branch, Auto, listed folder, untouched Inbox, catalog and
/// what the core derives from them come from `input`, as Start reads them;
/// the rest of `choices` is the view's (computer, account, project, model,
/// terminal, repository, predecessor).
#[tauri::command(async)]
pub fn new_session_plan(input: FormInput, choices: Choices) -> Plan {
    let catalog = input.catalog.clone();
    let form = derived(input);
    launch::plan(&Choices {
        path: form.path,
        agent: form.agent,
        name: form.name,
        branch: form.branch,
        auto_preferred: form.auto_preferred,
        listed_folder: form.listed_folder,
        implicit_inbox: form.implicit_inbox,
        automatic_available: form.automatic_available,
        automatic_path: form.automatic_path,
        project_worktree: form.project_worktree,
        catalog,
        ..choices
    })
}

#[tauri::command(async)]
pub fn launch_warning(local: MachineMeta, computer: String) -> Option<String> {
    warning(&local, &computer, launch::own_windows_session())
}

#[tauri::command(async)]
pub fn repository_fields(
    url: String,
    destination: String,
    style: Style,
    custom: bool,
    fresh: Option<String>,
) -> RepositoryFields {
    repository_source(&url, &destination, style, custom, fresh.as_deref())
}

#[tauri::command(async)]
pub fn worktree_snapshot(state: State<'_, NewSession>, computer: String, path: String) -> Value {
    state.snapshot(&ComputerId(computer), &path)
}

#[tauri::command]
pub async fn worktree_catalog(
    state: State<'_, NewSession>,
    computer: String,
    path: String,
    refresh: bool,
) -> Result<Value, String> {
    let state = state.inner().clone();
    blocking(move || state.catalog(&ComputerId(computer), &path, refresh)).await
}

#[tauri::command]
pub async fn create_worktree(
    state: State<'_, NewSession>,
    computer: String,
    path: String,
    catalog: Value,
    branch: String,
    base: String,
    destination: String,
) -> Result<CreatedWorktree, String> {
    let state = state.inner().clone();
    blocking(move || {
        state.create_worktree(
            &ComputerId(computer),
            &path,
            &catalog,
            &branch,
            &base,
            &destination,
        )
    })
    .await
}

#[tauri::command]
pub async fn list_directories(
    state: State<'_, NewSession>,
    computer: String,
    path: String,
    hidden: bool,
) -> Result<Value, String> {
    let state = state.inner().clone();
    blocking(move || state.directories(&ComputerId(computer), &path, hidden)).await
}

#[tauri::command]
pub async fn create_directory(
    state: State<'_, NewSession>,
    computer: String,
    path: String,
) -> Result<Value, String> {
    let state = state.inner().clone();
    blocking(move || state.create_directory(&ComputerId(computer), &path)).await
}

/// The system folder dialog on this computer, opened from Rust: the webview
/// holds none of the dialog plugin's permissions. The chosen folder in Qt's
/// form (`/` separators), None when cancelled.
#[tauri::command]
pub async fn pick_local_folder(app: AppHandle, start: String) -> Option<String> {
    use tauri::Manager;
    use tauri_plugin_dialog::DialogExt;
    let directory = start_folder(&start, app.path().home_dir().ok().as_deref());
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app.dialog().file().set_title(CHOOSE_A_FOLDER);
        if let Some(directory) = directory {
            dialog = dialog.set_directory(directory);
        }
        if let Some(window) = crate::main_window(&app) {
            dialog = dialog.set_parent(&window);
        }
        dialog.blocking_pick_folder()
    })
    .await
    .ok()
    .flatten()?;
    let path = picked.into_path().ok()?;
    Some(paths::clean(&path.to_string_lossy(), paths::build()))
}

/// Whether a folder of this computer exists (`ProjectsDialog.cpp:530`).
fn folder_exists(path: &str) -> bool {
    !path.is_empty() && Path::new(path).is_dir()
}

/// [`folder_exists`] on the blocking pool: a stat can wait on a network drive.
#[tauri::command]
pub async fn local_folder_exists(path: String) -> bool {
    tauri::async_runtime::spawn_blocking(move || folder_exists(&path))
        .await
        .unwrap_or(false)
}

#[tauri::command]
pub async fn opencode_models(
    state: State<'_, NewSession>,
    computer: String,
    folder: String,
    name: String,
) -> Result<Value, String> {
    let state = state.inner().clone();
    blocking(move || state.opencode_models(&ComputerId(computer), &folder, &name)).await
}

#[tauri::command]
pub async fn opencode_favorite(
    state: State<'_, NewSession>,
    computer: String,
    value: String,
    favorite: bool,
    name: String,
) -> Result<Value, String> {
    let state = state.inner().clone();
    blocking(move || state.opencode_favorite(&ComputerId(computer), &value, favorite, &name)).await
}

#[tauri::command]
pub async fn opencode_start(
    state: State<'_, NewSession>,
    computer: String,
) -> Result<Value, String> {
    let state = state.inner().clone();
    blocking(move || state.opencode_start(&ComputerId(computer))).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerus_client_core::machine::LocalHost;

    #[test]
    fn reading_malformed_remembered_accounts_does_not_rewrite_the_store() {
        let dir =
            std::env::temp_dir().join(format!("zerus-launch-settings-{}", uuid::Uuid::new_v4()));
        let settings = zerus_client_core::settings::Settings::open(dir.join("settings.json"), None);
        settings
            .set("workspace/launchAgent", json!("claude"))
            .unwrap();
        settings
            .set("workspace/launchOpenTerminal", json!(true))
            .unwrap();
        for accounts in ["malformed", "[]"] {
            settings
                .set("workspace/launchAccounts", json!(accounts))
                .unwrap();
            let read = remembered(&settings);
            assert_eq!(
                read,
                Remembered {
                    agent: "claude".into(),
                    accounts: json!({}),
                    open_terminal: true
                }
            );
            assert_eq!(settings.text("workspace/launchAccounts"), accounts);
        }
        assert!(settings.flush_within(std::time::Duration::from_secs(2)));
        drop(settings);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn windows(state: &str, session: Option<u64>) -> MachineMeta {
        MachineMeta {
            platform: Some("windows".into()),
            local_host: Some(LocalHost {
                state: Some(state.into()),
                windows_session: session,
                ..LocalHost::default()
            }),
            ..MachineMeta::default()
        }
    }

    // launchIntoAnotherWindowsSessionAsksFirst and
    // launchWithoutAnotherWindowsSessionDoesNotAsk_data.
    #[test]
    fn the_windows_warning_asks_only_before_another_session_here() {
        let text = warning(&windows("running", Some(2)), "", Some(1)).unwrap();
        assert!(text.contains("another Windows session (2), for example one started over SSH"));
        assert!(text.contains("cannot show windows on this desktop"));
        assert!(text.ends_with("Launch anyway?"));
        assert_eq!(
            warning(&windows("running", Some(1)), "", Some(1)),
            None,
            "same session"
        );
        assert_eq!(
            warning(&windows("stopped", Some(2)), "", Some(1)),
            None,
            "no host runs"
        );
        assert_eq!(
            warning(&windows("running", Some(2)), "mac", Some(1)),
            None,
            "remote"
        );
        assert_eq!(
            warning(&windows("running", Some(2)), "", None),
            None,
            "no own session"
        );
        let mut linux = windows("running", Some(2));
        linux.platform = Some("linux".into());
        assert_eq!(warning(&linux, "", Some(1)), None, "linux computer");
    }

    // repositorySourceKeepsCommonChoicesAndManualDestination.
    #[test]
    fn the_repository_destination_follows_the_url_until_it_is_typed() {
        // The first repository source has no record: the URL suggests the name.
        let first = repository_source(
            "https://github.com/example/first.git",
            "",
            Style::Posix,
            false,
            None,
        );
        assert_eq!(
            (first.field.as_str(), first.name.as_str()),
            ("~/first", "first")
        );
        assert_eq!(
            (first.parent.as_str(), first.destination.as_str()),
            ("~", "~/first")
        );
        assert!(first.can_submit);
        let second = repository_source(
            "git@example.invalid:team/second.git",
            &first.field,
            Style::Posix,
            false,
            None,
        );
        assert_eq!(
            (second.field.as_str(), second.suggested.as_str()),
            ("~/second", "second")
        );
        // A typed destination is kept through URL changes.
        let custom = repository_source(
            "git@example.invalid:team/third.git",
            "/workspace/custom",
            Style::Posix,
            true,
            None,
        );
        assert_eq!(custom.field, "/workspace/custom");
        assert_eq!(
            (custom.parent.as_str(), custom.name.as_str()),
            ("/workspace", "custom")
        );
        assert_eq!(custom.destination, "/workspace/custom");
        assert_eq!(custom.suggested, "third");
        // A new computer starts in `~` and keeps the folder name.
        let moved = repository_source(
            &custom.url,
            &custom.field,
            Style::Windows,
            true,
            Some(&custom.name),
        );
        assert_eq!(
            (moved.field.as_str(), moved.parent.as_str()),
            ("~\\custom", "~")
        );
        // Without a URL or name nothing can be cloned.
        let empty = repository_source("", "", Style::Posix, false, Some(""));
        assert_eq!(empty.field, "~/");
        assert!(!empty.can_submit);
        assert!(!repository_source("  ", "/srv/x", Style::Posix, true, None).can_submit);
    }

    // syncRepositoryComputer (613): another computer keeps the old record's
    // folder name, an empty one too.
    #[test]
    fn a_new_computer_keeps_the_old_records_empty_name() {
        let moved = repository_source(
            "https://github.com/example/first.git",
            "~/",
            Style::Windows,
            true,
            Some(""),
        );
        assert_eq!((moved.field.as_str(), moved.name.as_str()), ("~\\", ""));
        assert_eq!(moved.suggested, "first");
        assert!(!moved.can_submit);
    }

    #[test]
    fn windows_destinations_split_as_qt_does() {
        let split = |text: &str| {
            let fields = repository_source(
                "https://example.invalid/r.git",
                text,
                Style::Windows,
                true,
                None,
            );
            (fields.parent, fields.name, fields.destination)
        };
        assert_eq!(
            split("C:\\work\\repo"),
            ("C:/work".into(), "repo".into(), "C:/work/repo".into())
        );
        assert_eq!(
            split("C:\\repo"),
            ("C:/".into(), "repo".into(), "C:/repo".into())
        );
        assert_eq!(split("\\repo"), ("/".into(), "repo".into(), "/repo".into()));
        assert_eq!(split("repo"), ("".into(), "repo".into(), "/repo".into()));
        assert!(
            !repository_source("u", "repo", Style::Windows, true, None).can_submit,
            "no parent"
        );
        // On POSIX a backslash is part of the name.
        let posix = repository_source("u", "/srv/a\\b", Style::Posix, true, None);
        assert_eq!(
            (posix.parent.as_str(), posix.name.as_str()),
            ("/srv", "a\\b")
        );
        // The URL's suggestion goes beside the parent in native form.
        let suggested = repository_source(
            "https://example.invalid/team/app.git",
            "C:\\work\\old",
            Style::Windows,
            false,
            None,
        );
        assert_eq!(suggested.field, "C:\\work\\app");
        assert_eq!(suggested.destination, "C:/work/app");
    }

    fn catalog() -> Value {
        json!({"state": "ok", "common_dir": "/srv/repo/.git", "selected_root": "/srv/repo", "path": "/srv/repo",
               "worktrees": [{"path": "/srv/repo", "kind": "main", "available": true},
                             {"path": "/srv/repo-work-1", "kind": "linked", "available": true}]})
    }

    fn input(path: &str, catalog: Value, verified: Value) -> FormInput {
        let form = Form {
            online: true,
            agent: "codex".into(),
            name: "work-1a2b3c4d".into(),
            account_count: 1,
            account_selected: true,
            path: path.into(),
            folder_online: true,
            auto_preferred: true,
            branch: "work-1a2b3c4d".into(),
            style: Style::Posix,
            ..Form::default()
        };
        FormInput {
            form,
            catalog,
            verified,
            project: Some(Project {
                id: "p".into(),
                name: "Example".into(),
                folders: vec![launch::ProjectFolder {
                    machine: String::new(),
                    path: "/srv/repo".into(),
                }],
            }),
            host: String::new(),
            local: None,
        }
    }

    // NewSessionDialog 1092-1113 and updateForm.
    #[test]
    fn the_form_derives_auto_and_the_project_worktree_from_either_catalog() {
        let view = form_view(input("/srv/repo", catalog(), Value::Null));
        assert!(view.automatic_available && view.shown.automatic);
        assert_eq!(view.automatic_path, "/srv/repo-work-1a2b3c4d");
        assert_eq!(view.shown.worktree_target, "/srv/repo-work-1a2b3c4d");
        assert_eq!(
            view.shown.worktree_tooltip.as_ref().map(|t| t.english()),
            Some("New branch: work-1a2b3c4d\nStart from: HEAD".to_owned())
        );
        assert!(view.project_worktree, "the project's own checkout");
        assert!(view.shown.start_enabled);
        // A pending read, or an ordinary folder, gives no Auto: it starts directly.
        let mut pending = input("/srv/repo", catalog(), Value::Null);
        pending.form.catalog_pending = true;
        let view = form_view(pending);
        assert!(!view.automatic_available && !view.shown.start_enabled);
        let plain = form_view(input(
            "/srv/notes",
            json!({"state": "not_repo", "worktrees": []}),
            Value::Null,
        ));
        assert!(
            !plain.automatic_available && !plain.project_worktree && plain.shown.outside_project
        );
        assert!(plain.shown.start_enabled && plain.shown.worktree_target.is_empty());
        // A worktree created in this dialog is a project worktree by its verified catalog.
        let created = launch::created_catalog(&catalog(), "/srv/repo-new");
        let view = form_view(input(
            "/srv/repo-new",
            json!({"state": "unavailable", "worktrees": []}),
            created,
        ));
        assert!(view.project_worktree && !view.shown.outside_project);
        // Start asks the same derived form; a submitted Start does nothing.
        assert_eq!(
            new_session_start(input("/srv/repo", catalog(), Value::Null), false),
            Start::Background
        );
        assert_eq!(
            new_session_start(input("/srv/repo", catalog(), Value::Null), true),
            Start::Nothing
        );
        let mut busy = input("/srv/repo", catalog(), Value::Null);
        busy.form.catalog_pending = true;
        assert_eq!(new_session_start(busy, false), Start::Nothing);
    }

    // updateForm 727-730: without a computer, a Windows console with a session
    // host is told to enable local sessions; the view passes its card.
    #[test]
    fn the_form_derives_the_local_sessions_hint_from_this_computers_card() {
        let mut none = input("", Value::Null, Value::Null);
        none.form.no_machine = true;
        let mut console = windows("stopped", None);
        console.local_sessions = Some(false);
        let mut with_card = none.clone();
        with_card.local = Some(console.clone());
        assert_eq!(
            form_view(with_card)
                .shown
                .notice
                .as_ref()
                .map(|t| t.source.as_ref()),
            Some(launch::ENABLE_OR_REMOTE)
        );
        assert_eq!(
            form_view(none.clone())
                .shown
                .notice
                .as_ref()
                .map(|t| t.source.as_ref()),
            Some(launch::REMOTE_ONLY)
        );
        console.platform = Some("linux".into());
        let mut linux = none;
        linux.local = Some(console);
        assert_eq!(
            form_view(linux)
                .shown
                .notice
                .as_ref()
                .map(|t| t.source.as_ref()),
            Some(launch::REMOTE_ONLY)
        );
    }

    #[test]
    fn a_dialog_starts_with_a_generated_branch_and_qt_defaults() {
        let first = context();
        assert!(first.branch.starts_with("work-") && first.branch.len() == 13);
        assert!(first.branch[5..]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(first.branch, context().branch);
        assert_eq!(
            first.remembered,
            Remembered {
                agent: String::new(),
                accounts: json!({}),
                open_terminal: false
            }
        );
        assert_eq!(first.own_windows_session, launch::own_windows_session());
        assert_eq!(first.build_style, paths::build());
        assert_eq!(first.swarm, "");
    }

    #[test]
    fn the_local_picker_opens_at_home_for_tilde() {
        let home = Path::new("/home/example");
        assert_eq!(start_folder("", Some(home)), Some(home.to_path_buf()));
        assert_eq!(start_folder("~", Some(home)), Some(home.to_path_buf()));
        assert_eq!(start_folder("~/src", Some(home)), Some(home.join("src")));
        assert_eq!(
            start_folder("/srv", Some(home)),
            Some(PathBuf::from("/srv"))
        );
        assert_eq!(start_folder("~", None), None);
        assert!(!folder_exists(""));
        assert!(folder_exists(&std::env::temp_dir().to_string_lossy()));
        assert!(!folder_exists("/nonexistent/zerus-example"));
    }

    // start (757-776) reads the form live at the click: the plan takes the
    // form's folder, agent, name, branch, Auto and its derived fields from the
    // same input Start derives them from, never the view's copies.
    #[test]
    fn the_plan_derives_from_the_same_form_as_start() {
        let input = || input("/srv/repo", catalog(), Value::Null);
        let stale = Choices {
            agent: "claude".into(),
            name: "old-name".into(),
            path: "/srv/other".into(),
            branch: "work-00000000".into(),
            automatic_available: false,
            automatic_path: String::new(),
            project_worktree: false,
            listed_folder: true,
            catalog: json!({}),
            model: "fixture/local".into(),
            effort: "high".into(),
            open_terminal: true,
            machine: "machine-1".into(),
            ..Choices::default()
        };
        assert_eq!(new_session_start(input(), false), Start::Background);
        let plan = new_session_plan(input(), stale.clone());
        assert_eq!(
            (plan.cmd.as_str(), plan.tag.as_str()),
            ("codex", "work-1a2b3c4d")
        );
        assert_eq!(
            (plan.target.as_str(), plan.source.as_str()),
            ("/srv/repo", "/srv/repo")
        );
        assert!(plan.auto, "Auto as Start saw it");
        assert_eq!(plan.branch, "work-1a2b3c4d");
        assert_eq!(plan.destination.as_deref(), Some("/srv/repo-work-1a2b3c4d"));
        assert!(!plan.add_folder, "a project worktree adds no folder");
        assert_eq!(plan.catalog, catalog());
        // What only the view knows comes from its choices.
        assert_eq!(
            (plan.model.as_str(), plan.effort.as_str()),
            ("", ""),
            "a model only for OpenCode"
        );
        assert!(plan.terminal);
        assert_eq!(plan.machine, "machine-1");
        let mut opencode = input();
        opencode.form.agent = "opencode".into();
        let plan = new_session_plan(opencode, stale);
        assert_eq!(
            (plan.model.as_str(), plan.effort.as_str(), plan.terminal),
            ("fixture/local", "high", false)
        );
    }

    #[cfg(unix)]
    mod calls {
        use super::*;
        use std::sync::atomic::AtomicUsize;
        use std::time::Duration;

        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        /// The core's fake hgs (client/core/tests/support/fake_hgs_creation.py).
        struct Fake {
            dir: PathBuf,
            hgs: PathBuf,
            _tree: std::sync::RwLockReadGuard<'static, ()>,
        }

        impl Fake {
            fn new(config: Value) -> Self {
                let tree = crate::PROCESS_TREE
                    .read()
                    .unwrap_or_else(PoisonError::into_inner);
                let dir = std::env::temp_dir().join(format!(
                    "zerus-newsession-{}-{}",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&dir).unwrap();
                let support = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../core/tests/support/fake_hgs_creation.py");
                let hgs = dir.join("hgs");
                // A symlink, never a copy (see the core's tests/actions.rs).
                std::os::unix::fs::symlink(support, &hgs).unwrap();
                let fake = Self {
                    dir,
                    hgs,
                    _tree: tree,
                };
                fake.configure(config);
                fake
            }

            fn configure(&self, config: Value) {
                std::fs::write(self.dir.join("config.json"), config.to_string()).unwrap();
            }

            fn log(&self) -> Vec<Vec<String>> {
                std::fs::read_to_string(self.dir.join("log.ndjson"))
                    .unwrap_or_default()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect()
            }

            fn session(&self, clock: Arc<Mutex<f64>>, file: Option<PathBuf>) -> NewSession {
                NewSession::new(
                    self.hgs.clone(),
                    true,
                    file,
                    Arc::new(move || *lock(&clock)),
                )
            }
        }

        impl Drop for Fake {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }

        fn catalog_at(sampled_at: f64) -> Value {
            let mut value = catalog();
            value["sampled_at"] = json!(sampled_at);
            value
        }

        const CREATED: &str = "Creation could not be confirmed. Refresh the catalog and check the destination before retrying.";

        fn creating(log: &[Vec<String>]) -> Vec<String> {
            log.iter()
                .map(|call| call.join(" "))
                .filter(|call| {
                    call.contains("worktrees create")
                        || call.contains("dirs --create")
                        || call.contains("account start")
                        || call.contains("account favorite")
                        || call.starts_with("clone")
                        || call.contains("repository")
                        || call.contains("swarm")
                        || call.contains("--launch-id")
                })
                .collect()
        }

        #[test]
        fn a_fresh_snapshot_answers_without_a_call_and_refresh_reads_again() {
            let fake = Fake::new(json!({"replies": {"worktrees": {"stdout": catalog_at(1000.0)}}}));
            let clock = Arc::new(Mutex::new(1000.0));
            let session = fake.session(Arc::clone(&clock), None);
            let local = ComputerId::local();
            assert_eq!(session.snapshot(&local, "/srv/repo"), json!({}));
            let first = session.catalog(&local, "/srv/repo", false).unwrap();
            assert_eq!(first["received_at"], json!(1000.0));
            *lock(&clock) = 1010.0;
            assert_eq!(session.catalog(&local, "/srv/repo", false).unwrap(), first);
            // Each worktree root gets the same catalog.
            let sibling = session.catalog(&local, "/srv/repo-work-1", false).unwrap();
            assert_eq!(sibling["selected_root"], "/srv/repo-work-1");
            assert_eq!(
                fake.log(),
                [vec!["worktrees", "--path", "/srv/repo", "--json"]]
            );
            session.catalog(&local, "/srv/repo", true).unwrap();
            assert_eq!(
                fake.log()[1],
                ["worktrees", "--path", "/srv/repo", "--json", "--refresh"]
            );
            *lock(&clock) = 1030.0;
            session.catalog(&local, "/srv/repo", false).unwrap();
            assert_eq!(fake.log().len(), 3, "thirty seconds later it reads again");
            assert_eq!(session.snapshot(&local, "/srv/repo")["state"], "ok");
            // Another computer's identical path is unrelated, and a peer waits 15 s.
            assert_eq!(
                session.snapshot(&ComputerId::from("mac"), "/srv/repo"),
                json!({})
            );
            // A bad reply is refused and kept from the snapshots.
            fake.configure(json!({"replies": {"worktrees": {"stdout": {"state": "ok"}}}}));
            assert_eq!(
                session.catalog(&local, "/srv/other", false),
                Err("Invalid worktree catalog response".into())
            );
            assert_eq!(session.snapshot(&local, "/srv/other"), json!({}));
            fake.configure(
                json!({"replies": {"worktrees": {"exit": 1, "stderr": "not reachable"}}}),
            );
            assert_eq!(
                session.catalog(&local, "/srv/other", true),
                Err("not reachable".into())
            );
            assert!(creating(&fake.log()).is_empty());
        }

        // A read that panics leaves no flight behind: the next read calls.
        #[test]
        fn a_panicking_read_leaves_no_shared_flight() {
            let fake = Fake::new(json!({"replies": {"worktrees": {"stdout": catalog_at(1000.0)}}}));
            let fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let session = {
                let fail = Arc::clone(&fail);
                NewSession::new(
                    fake.hgs.clone(),
                    true,
                    None,
                    Arc::new(move || {
                        assert!(!fail.load(Ordering::SeqCst), "the clock fails");
                        1000.0
                    }),
                )
            };
            let local = ComputerId::local();
            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                session.catalog(&local, "/srv/repo", true)
            }));
            assert!(panicked.is_err());
            assert!(lock(&session.0.flights).is_empty(), "the guard removed it");
            fail.store(false, Ordering::SeqCst);
            assert!(session.catalog(&local, "/srv/repo", true).is_ok());
            assert_eq!(fake.log().len(), 2, "the next read made its own call");
        }

        #[test]
        fn two_reads_of_one_folder_share_one_call() {
            let fake = Fake::new(
                json!({"replies": {"worktrees": {"stdout": catalog_at(0.0), "sleep": 0.4}}}),
            );
            let session = fake.session(Arc::new(Mutex::new(1000.0)), None);
            let mac = ComputerId::from("mac");
            let reads: Vec<_> = (0..2)
                .map(|_| {
                    let (session, mac) = (session.clone(), mac.clone());
                    std::thread::spawn(move || session.catalog(&mac, "/srv/repo", false))
                })
                .collect();
            let answers: Vec<_> = reads.into_iter().map(|read| read.join().unwrap()).collect();
            // Its snapshot is never fresh, so only sharing explains one call.
            assert!(answers[0].is_ok());
            assert_eq!(answers[0], answers[1]);
            assert_eq!(
                fake.log(),
                [vec!["@mac", "worktrees", "--path", "/srv/repo", "--json"]]
            );
        }

        #[test]
        fn a_worktree_created_meanwhile_fails_the_read_under_way() {
            let fake = Fake::new(json!({"replies": {
                "worktrees": {"stdout": catalog_at(1000.0), "sleep": 0.6},
                "worktrees create": {"stdout": {"status": "created", "branch": "feature", "common_dir": "/srv/repo/.git",
                                                "path": "/srv/repo-feature"}, "echo": {"request_id": "--request-id"}}}}));
            let session = fake.session(Arc::new(Mutex::new(1000.0)), None);
            let local = ComputerId::local();
            let read = {
                let session = session.clone();
                std::thread::spawn(move || {
                    session.catalog(&ComputerId::local(), "/srv/repo", false)
                })
            };
            let until = std::time::Instant::now() + Duration::from_secs(10);
            while fake.log().is_empty() {
                assert!(std::time::Instant::now() < until, "the read started");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                session
                    .create_worktree(
                        &local,
                        "/srv/repo",
                        &catalog(),
                        "feature",
                        "HEAD",
                        "/srv/repo-feature"
                    )
                    .map(|created| created.path),
                Ok("/srv/repo-feature".into())
            );
            assert_eq!(
                read.join().unwrap(),
                Err("Folder changed. Refresh its catalog.".into())
            );
            assert_eq!(
                session.snapshot(&local, "/srv/repo"),
                json!({}),
                "nothing was kept"
            );
        }

        // NewWorktreeDialog with createWorktree: one fresh identity, no expected
        // machine and no receipt; the catalogs are invalidated on every outcome.
        #[test]
        fn create_worktree_confirms_its_receipt_with_qt_texts() {
            let reply = json!({"status": "created", "branch": "feature", "common_dir": "/srv/repo/.git", "path": "/srv/repo-feature"});
            let fake = Fake::new(json!({"replies": {
                "worktrees": {"stdout": catalog_at(1000.0)},
                "worktrees create": {"stdout": reply, "echo": {"request_id": "--request-id"}}}}));
            let session = fake.session(Arc::new(Mutex::new(1000.0)), None);
            let mac = ComputerId::from("mac");
            let opened = session.catalog(&mac, "/srv/repo", false).unwrap();
            let create = || {
                session
                    .create_worktree(
                        &mac,
                        "/srv/repo",
                        &opened,
                        "feature",
                        "HEAD",
                        "/srv/repo-feature",
                    )
                    .map(|created| created.path)
            };
            // The created checkout joins the catalog the dialog opened with, as verified.
            let created = session
                .create_worktree(
                    &mac,
                    "/srv/repo",
                    &opened,
                    "feature",
                    "HEAD",
                    "/srv/repo-feature",
                )
                .unwrap();
            assert_eq!(created.path, "/srv/repo-feature");
            assert_eq!(
                created.catalog,
                launch::created_catalog(&opened, "/srv/repo-feature")
            );
            assert_eq!(
                created.catalog["worktrees"].as_array().unwrap().last(),
                Some(&json!({"path": "/srv/repo-feature", "kind": "linked", "available": true}))
            );
            assert_eq!(created.catalog["common_dir"], "/srv/repo/.git");
            let call = fake.log()[1].clone();
            assert_eq!(
                call[..15],
                [
                    "@mac",
                    "worktrees",
                    "create",
                    "--path",
                    "/srv/repo",
                    "--common-dir",
                    "/srv/repo/.git",
                    "--branch",
                    "feature",
                    "--base",
                    "HEAD",
                    "--destination",
                    "/srv/repo-feature",
                    "--request-id",
                    call[14].as_str()
                ]
            );
            assert!(uuid::Uuid::parse_str(&call[14]).is_ok());
            assert_eq!(call[15..], ["--json"], "no receipt, no expected machine");
            assert_eq!(
                session.snapshot(&mac, "/srv/repo")["stale"],
                json!(true),
                "invalidated"
            );
            session.catalog(&mac, "/srv/repo", false).unwrap();
            assert_eq!(fake.log().len(), 3, "a stale snapshot is read again");
            // A receipt for another request is not a confirmation.
            fake.configure(json!({"replies": {"worktrees create": {"stdout": reply}}}));
            assert_eq!(create(), Err(CREATED.into()));
            fake.configure(json!({"replies": {"worktrees create": {"stdout": {"status": "created", "branch": "other",
                "common_dir": "/srv/repo/.git", "path": "/srv/repo-feature"}, "echo": {"request_id": "--request-id"}}}}));
            assert_eq!(create(), Err(CREATED.into()));
            fake.configure(
                json!({"replies": {"worktrees create": {"exit": 1, "stderr": "branch exists"}}}),
            );
            assert_eq!(
                create(),
                Err("branch exists\nCheck /srv/repo-feature before retrying. Any created worktree is kept.".into())
            );
            let identities: Vec<String> = fake
                .log()
                .iter()
                .filter(|call| call.get(2).is_some_and(|word| word == "create"))
                .map(|call| call[14].clone())
                .collect();
            assert_eq!(identities.len(), 4);
            let distinct: std::collections::BTreeSet<&String> = identities.iter().collect();
            assert_eq!(distinct.len(), 4, "one identity per click, never reused");
        }

        #[test]
        fn folder_listings_take_qt_arguments_and_refuse_a_bad_reply() {
            let fake = Fake::new(
                json!({"replies": {"dirs": {"stdout": {"path": "/home/example", "directories": [{"name": "src"}]}}}}),
            );
            let session = fake.session(Arc::new(Mutex::new(0.0)), None);
            let listing = session
                .directories(&ComputerId::from("mac"), "", true)
                .unwrap();
            assert_eq!(listing["path"], "/home/example");
            fake.configure(json!({"replies": {"dirs": {"raw": "not json"}}}));
            assert_eq!(
                session.directories(&ComputerId::local(), "/srv", false),
                Err("Invalid directory response".into())
            );
            assert_eq!(
                fake.log(),
                [vec!["@mac", "dirs", "--hidden", "~"], vec!["dirs", "/srv"]]
            );
        }

        #[test]
        fn a_new_folder_pins_its_parent_and_checks_its_receipt() {
            let receipt =
                json!({"status": "created", "path": "/srv/real/new", "parent": "/srv/real"});
            let fake = Fake::new(
                json!({"help": "hgs dirs --create <path> --request-id ID", "replies": {
                "dirs": {"stdout": {"path": "/srv/real", "directories": []}},
                "dirs --create": {"stdout": receipt, "echo": {"request_id": "--request-id", "requested_path": "--create"}}}}),
            );
            let session = fake.session(Arc::new(Mutex::new(0.0)), None);
            let mac = ComputerId::from("mac");
            let created = session.create_directory(&mac, "/srv/repo/new").unwrap();
            assert_eq!(created["path"], "/srv/real/new");
            let log = fake.log();
            assert_eq!(
                log[..2],
                [vec!["@mac", "--help"], vec!["@mac", "dirs", "/srv/repo"]]
            );
            assert_eq!(
                log[2][..5],
                ["@mac", "dirs", "--create", "/srv/repo/new", "--request-id"]
            );
            assert!(uuid::Uuid::parse_str(&log[2][5]).is_ok());
            // A receipt for another folder is not a confirmation.
            fake.configure(json!({"help": "dirs --create <path>", "replies": {
                "dirs": {"stdout": {"path": "/srv/real", "directories": []}},
                "dirs --create": {"stdout": {"status": "created", "path": "/srv/real/other", "parent": "/srv/real"},
                                  "echo": {"request_id": "--request-id", "requested_path": "--create"}}}}));
            assert_eq!(
                session.create_directory(&mac, "/srv/repo/new"),
                Err("Folder creation could not be confirmed. Open the path to check it before trying again.".into())
            );
            // An unconfirmed parent creates nothing.
            fake.configure(json!({"help": "dirs --create <path>", "replies": {"dirs": {"stdout": {"path": "srv"}}}}));
            let before = fake.log().len();
            assert_eq!(
                session.create_directory(&mac, "/srv/repo/new"),
                Err("The computer did not confirm the parent folder. Open it before creating a folder.".into())
            );
            assert_eq!(
                fake.log().len(),
                before + 2,
                "the probe and the parent only"
            );
            // A help text past its bound is not read as support.
            let mut help = "x".repeat(HELP_LIMIT);
            help.push_str(" dirs --create <path>");
            fake.configure(json!({"help": help}));
            let before = fake.log().len();
            assert_eq!(
                session.create_directory(&mac, "/srv/repo/new"),
                Err(
                    "Update hgs on mac to create folders. You can still choose existing folders."
                        .into()
                )
            );
            assert_eq!(fake.log()[before..], [vec!["@mac", "--help"]]);
            // An older CLI is never asked to create.
            fake.configure(json!({"help": "usage: hgs dirs PATH"}));
            let before = fake.log().len();
            assert_eq!(
                session.create_directory(&ComputerId::local(), "/srv/repo/new"),
                Err("Update hgs on this computer to create folders. You can still choose existing folders.".into())
            );
            assert_eq!(fake.log()[before..], [vec!["--help"]]);
        }

        // openCodeStarSendsOneFavoriteRequestWithIdentity.
        #[test]
        fn a_star_sends_one_favorite_request_with_its_identity() {
            let fake = Fake::new(json!({"replies": {"account favorite": {
                "stdout": {"status": "applied", "model": "fixture/plain", "favorite": true},
                "echo": {"request_id": "--request-id"}}}}));
            let session = fake.session(Arc::new(Mutex::new(0.0)), None);
            let local = ComputerId::local();
            let receipt = session
                .opencode_favorite(&local, "fixture/plain", true, "pc")
                .unwrap();
            let call = fake.log()[0].clone();
            assert_eq!(
                call[..7],
                [
                    "account",
                    "favorite",
                    "native-opencode",
                    "--model",
                    "fixture/plain",
                    "--favorite",
                    "on"
                ]
            );
            assert_eq!(call[7], "--request-id");
            assert!(uuid::Uuid::parse_str(&call[8]).is_ok());
            assert_eq!(call[9..], ["--json"]);
            assert_eq!(receipt["request_id"], call[8]);
            // A failure is shown, never sent again.
            fake.configure(json!({"replies": {"account favorite": {"exit": 1,
                "stderr": "Account catalog is busy. Retry after the current operation finishes.\n"}}}));
            assert_eq!(
                session.opencode_favorite(&local, "fixture/other", true, "pc"),
                Err("Account catalog is busy. Retry after the current operation finishes.".into())
            );
            std::thread::sleep(Duration::from_millis(100));
            assert_eq!(fake.log().len(), 2);
            // An older CLI's usage reads as the update text.
            fake.configure(json!({"replies": {"account favorite": {"exit": 1, "stderr": "hgs: usage: hgs account ls\n"}}}));
            assert_eq!(
                session.opencode_favorite(&ComputerId::from("mac"), "fixture/other", false, ""),
                Err("Update Zerus on mac to search all models and star favorites.".into())
            );
            fake.configure(json!({"replies": {"account favorite": {"stdout": {"status": "applied", "request_id": "old",
                "model": "fixture/plain", "favorite": true}}}}));
            assert_eq!(
                session.opencode_favorite(&local, "fixture/plain", true, "pc"),
                Err("Invalid favorite response".into())
            );
        }

        // openCodeStartsStoppedServiceExplicitly.
        #[test]
        fn the_opencode_service_starts_only_when_asked() {
            let fake = Fake::new(
                json!({"replies": {"account start": {"stdout": {"catalog_state": "ready", "model_options": []}}}}),
            );
            let session = fake.session(Arc::new(Mutex::new(0.0)), None);
            let mac = ComputerId::from("mac");
            assert_eq!(
                session.opencode_start(&mac).unwrap()["catalog_state"],
                "ready"
            );
            assert_eq!(
                fake.log(),
                [vec![
                    "@mac",
                    "account",
                    "start",
                    "native-opencode",
                    "--json"
                ]]
            );
            fake.configure(json!({"replies": {"account start": {"raw": "[]"}}}));
            assert_eq!(
                session.opencode_start(&mac),
                Err("Invalid account catalog response".into())
            );
        }

        #[test]
        fn a_folders_model_catalog_is_bounded_and_worded_for_its_computer() {
            let fake = Fake::new(
                json!({"replies": {"account models": {"stdout": {"catalog_state": "ready", "model_options": []}}}}),
            );
            let session = fake.session(Arc::new(Mutex::new(0.0)), None);
            let mac = ComputerId::from("mac");
            assert!(session
                .opencode_models(&mac, "/workspace/example", "mac")
                .is_ok());
            assert_eq!(
                fake.log(),
                [vec![
                    "@mac",
                    "account",
                    "models",
                    "native-opencode",
                    "--location",
                    "/workspace/example",
                    "--json"
                ]]
            );
            fake.configure(json!({"replies": {"account models": {"exit": 1,
                "stderr": "usage: hgs account ls | add ID --provider codex|claude|kimi --label NAME\n"}}}));
            assert_eq!(
                session.opencode_models(&ComputerId::local(), "/workspace/example", "pc"),
                Err("Update Zerus on pc to search all models and star favorites.".into())
            );
            fake.configure(
                json!({"replies": {"account models": {"raw": "x", "repeat": 5 * 1024 * 1024 + 1}}}),
            );
            assert_eq!(
                session.opencode_models(&mac, "/workspace/example", "mac"),
                Err("The OpenCode model catalog is too large".into())
            );
            fake.configure(json!({"replies": {"account models": {"raw": "[]"}}}));
            assert_eq!(
                session.opencode_models(&mac, "/workspace/example", "mac"),
                Err("Invalid OpenCode model catalog response".into())
            );
        }

        // Review Focus 1: opening, inspecting, browsing and Cancel only read.
        #[test]
        fn opening_inspecting_and_browsing_send_only_reads() {
            let fake = Fake::new(json!({"replies": {
                "worktrees": {"stdout": catalog_at(1000.0)},
                "dirs": {"stdout": {"path": "/srv", "directories": []}},
                "account models": {"stdout": {"catalog_state": "ready", "model_options": []}}}}));
            let session = fake.session(Arc::new(Mutex::new(1000.0)), None);
            let mac = ComputerId::from("mac");
            let _ = context();
            session.snapshot(&mac, "/srv/repo");
            session.catalog(&mac, "/srv/repo", false).unwrap();
            session.catalog(&mac, "/srv/repo", true).unwrap();
            let view = form_view(input(
                "/srv/repo",
                session.snapshot(&mac, "/srv/repo"),
                Value::Null,
            ));
            assert!(view.automatic_available);
            let _ = new_session_start(input("/srv/repo", catalog(), Value::Null), false);
            let _ = repository_source(
                "https://example.invalid/r.git",
                "",
                Style::Posix,
                false,
                Some(""),
            );
            session.directories(&mac, "/srv", false).unwrap();
            session.directories(&mac, "~", true).unwrap();
            session.opencode_models(&mac, "/srv/repo", "mac").unwrap();
            let log = fake.log();
            assert_eq!(log.len(), 5);
            assert!(creating(&log).is_empty(), "{log:?}");
        }

        #[test]
        fn snapshots_reach_their_file_and_come_back() {
            let fake = Fake::new(json!({"replies": {"worktrees": {"stdout": catalog_at(1000.0)}}}));
            let file = fake.dir.join("data").join("worktree-snapshots.json");
            let clock = Arc::new(Mutex::new(1000.0));
            let session = fake.session(Arc::clone(&clock), Some(file.clone()));
            session
                .catalog(&ComputerId::local(), "/srv/repo", false)
                .unwrap();
            let saved: Value =
                serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            assert_eq!(
                saved.as_object().unwrap().len(),
                2,
                "the folder, which is a root, and the other root"
            );
            let restored = fake.session(clock, Some(file.clone()));
            assert_eq!(
                restored.snapshot(&ComputerId::local(), "/srv/repo")["state"],
                "ok"
            );
            restored
                .catalog(&ComputerId::local(), "/srv/repo", false)
                .unwrap();
            assert_eq!(fake.log().len(), 1, "the restored snapshot is fresh");
            // A broken file gives none.
            std::fs::write(&file, "{").unwrap();
            assert_eq!(
                fake.session(Arc::new(Mutex::new(0.0)), Some(file))
                    .snapshot(&ComputerId::local(), "/srv/repo"),
                json!({})
            );
        }

        #[test]
        fn a_replay_without_hgs_calls_nothing() {
            let fake = Fake::new(json!({}));
            let session = NewSession::new(fake.hgs.clone(), false, None, Arc::new(|| 0.0));
            let mac = ComputerId::from("mac");
            let refused = Err::<Value, String>(REPLAY_NEEDS_HGS.into());
            assert_eq!(session.catalog(&mac, "/srv", true), refused);
            assert_eq!(session.directories(&mac, "/srv", false), refused);
            assert_eq!(session.create_directory(&mac, "/srv/new"), refused);
            assert_eq!(session.opencode_models(&mac, "/srv", "mac"), refused);
            assert_eq!(session.opencode_favorite(&mac, "m", true, "mac"), refused);
            assert_eq!(session.opencode_start(&mac), refused);
            assert_eq!(
                session.create_worktree(&mac, "/srv", &catalog(), "b", "HEAD", "/srv-b"),
                Err(REPLAY_NEEDS_HGS.into())
            );
            assert!(fake.log().is_empty());
        }
    }
}
