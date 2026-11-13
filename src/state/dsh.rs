//! Official DeepSeek Harness backend. Native host lifetime is independent of
//! Zerus, tmux, and individual sessions. HGS stores bindings, not credentials.
use super::*;
use crate::config::Config;
use serde_json::Map;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
// The native host's endpoint and start (see `rpc` and `ensure_host`).
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Seek},
    time::{Duration, Instant},
};

#[cfg(unix)]
mod host_identity;
#[cfg(unix)]
mod shell_identity;
#[cfg(all(test, unix))]
pub(super) use host_identity::record_running_for_test;
#[cfg(all(test, unix))]
pub(super) use host_identity::recorded::UNDER_HOST as UNDER_HOST_FOR_TESTS;
/// The shared check (zerus-ppd26.15): Agent Teams' caller proof calls it too.
#[cfg(unix)]
pub(crate) use host_identity::recorded::{hosted_ancestor, Host, Refusal};

/// Whether this command runs under a DeepSeek host this computer records
/// (zerus-ppd26.15). Such a command can be only that host's own hosted
/// session, never another agent's session and never the person. The process
/// tree and the recorded host files decide, never a marker the caller sets.
/// See `host_identity/recorded.rs` for the rules and limits.
// DeepSeek hosts never run on Windows, so there only `Outside` is built.
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostCheck {
    /// Every record was read and none is an ancestor.
    Outside,
    /// A recorded host is an ancestor.
    Under,
    /// The ancestry or a record could not be verified: refused like `Under`,
    /// with the sentence that names the path and how to clear it.
    Unverified(String),
}

/// The check for this process. DeepSeek hosts never run on Windows, so no
/// host is recorded there.
pub(crate) fn host_check() -> HostCheck {
    #[cfg(unix)]
    {
        let found: std::result::Result<Option<Host>, Refusal> = hosted_ancestor();
        match found {
            Ok(None) => HostCheck::Outside,
            Ok(Some(_)) => HostCheck::Under,
            Err(refusal) => HostCheck::Unverified(refusal.sentence()),
        }
    }
    #[cfg(not(unix))]
    {
        HostCheck::Outside
    }
}

impl HostCheck {
    /// The sentence that refuses the command, or `None` outside every
    /// recorded host.
    pub(crate) fn sentence(self) -> Option<String> {
        match self {
            HostCheck::Outside => None,
            #[cfg(unix)]
            HostCheck::Under => Some(host_identity::recorded::UNDER_HOST.to_owned()),
            #[cfg(not(unix))]
            HostCheck::Under => None,
            HostCheck::Unverified(sentence) => Some(sentence),
        }
    }
}

/// The sentence that refuses this command as an agent session and as the
/// person, or `None` outside every recorded host.
pub(crate) fn host_refusal() -> Option<String> {
    host_check().sentence()
}

// Resident bridge identity is independent of the installed harness release.
const ADAPTER_VERSION: &str = "0.2.1-rc.2";
// Task #61 (team ede724c4): 0.2.2-rc.1 was reviewed upstream on 2026-11-05
// (a27dd5141, with the bridge's native interfaces that are in this tree, and
// scripts/ci/agent-compatibility.json); a fork merge kept the older list.
const REVIEWED_VERSIONS: &[&str] = &["0.2.1-rc.2", "0.2.1-rc.5", "0.2.2-rc.1"];
/// The harness release recommended to install: the newest reviewed one.
const VERSION: &str = "0.2.1-rc.5";
fn reviewed_harness_version(version: &str) -> bool {
    REVIEWED_VERSIONS.contains(&version)
}
/// The owner, 2026-11-04: every DeepSeek Harness release starts, reviewed or
/// not. The release is the first `X.Y.Z[-pre]` on the first line of
/// `dsh --version` (a bare version, or one after a package name); None when
/// the harness prints no version at all.
fn harness_release(output: &str) -> Option<String> {
    static RELEASE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let first = output.lines().map(str::trim).find(|line| !line.is_empty())?;
    RELEASE
        .get_or_init(|| regex::Regex::new(r"(?:^|[^0-9A-Za-z.])v?([0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.]+)?)").expect("release pattern"))
        .captures(first)
        .map(|found| found[1].to_owned())
}
/// The note an unreviewed release starts with: the bridge uses harness
/// internals, so a feature may fail until Zerus reviews that release.
fn harness_unreviewed_note(release: &str) -> String {
    format!(
        "hgs: DeepSeek Harness {release} is not a release Zerus reviewed ({}); it is started anyway. \
         If a DeepSeek feature fails, install a reviewed one: npm install -g @deepseek-ai/dsh@{VERSION}",
        REVIEWED_VERSIONS.join(", ")
    )
}
/// A refusal made before any binding, host or session exists; the desktop
/// reads it as a definite creation failure (the session can be created again).
pub(crate) const NOTHING_STARTED: &str = "(nothing was started; create the session again after the fix)";
/// Owner report 2026-11-04: the refusal names the installed release, the
/// reviewed ones and the command that installs the recommended one; nothing
/// started, so the session can be created again after the install.
fn harness_version_refusal(installed: Option<&str>) -> String {
    let found = match installed {
        Some(version) if !version.is_empty() => {
            format!("the installed DeepSeek Harness printed {:?} instead of its version", version.lines().next().unwrap_or(version))
        }
        _ => "the installed DeepSeek Harness did not report its version".to_owned(),
    };
    format!(
        "Zerus supports DeepSeek Harness {}; {found}. Install a supported release with: npm install -g @deepseek-ai/dsh@{VERSION}\n{NOTHING_STARTED}",
        REVIEWED_VERSIONS.join(" or ")
    )
}
fn compatible_adapter(ping: &Value) -> bool {
    ping["version"] == ADAPTER_VERSION && ping["protocol"] == 1
}
const BRIDGE: &str = include_str!("../dsh/bridge.mjs");
const COMMAND_GUARD: &str = include_str!("../dsh/command-guard.mjs");
const CONFIG_PRESETS: &str = include_str!("../dsh/config-presets.mjs");
const CONFIG_MODELS: &str = include_str!("../dsh/config-models.mjs");
fn binding_path(name: &str) -> PathBuf {
    PROVIDER.label_path(name)
}
/// `save` for a test record the test keeps using as it is (a copy is stamped and written).
#[cfg(test)]
fn save_ref(value: &Value) -> Result<()> {
    save(&mut value.clone())
}
const PROVIDER: provider_binding::Provider = provider_binding::Provider::DeepSeek;
pub(crate) fn exists(name: &str) -> bool { PROVIDER.exists(name) }
pub(super) fn binding(name: &str) -> Result<Value> { PROVIDER.read(name) }
pub(super) fn read_id(id: &str) -> Result<Value> { PROVIDER.read_id(id) }
/// Saves the binding, then the identity its hosted tool calls carry
/// (`shell_identity`). Every path that starts a run ends here, so the file
/// names the run this save established. A file that cannot be written
/// leaves that session's tool calls without session markers, never with
/// another session's, and is reported without undoing the saved binding.
pub(super) fn save(record: &mut Value) -> Result<()> {
    PROVIDER.save(record)?;
    #[cfg(unix)]
    if let Err(error) = shell_identity::write(record) {
        eprintln!("hgs: {}'s tool calls carry no session markers until its identity can be written: {error}", string(record, "name"));
    }
    Ok(())
}
/// Forgets the binding and the identity its tool calls carried.
fn remove(record: &Value) -> Result<()> {
    PROVIDER.remove(record)?;
    #[cfg(unix)]
    shell_identity::remove(record);
    Ok(())
}
fn native_home() -> PathBuf {
    nonempty_env("DSH_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".dsh"))
}
pub(crate) fn account_context(name: &str) -> Result<Value> {
    let record = binding(name)?;
    Ok(
        json!({"provider":"dsh","id":"native-dsh","home":record["agent_home"],"run_id":record["run_id"]}),
    )
}
pub(crate) fn account_ui_url(home: &Path) -> Result<String> {
    // Reading Accounts must not start or restart a native agent host.
    let record = json!({"agent_home":home});
    let log = fs::read_to_string(host_dir(&record).join("host.log"))
        .map_err(|_| "Open the native DeepSeek UI to connect this account.".to_owned())?;
    regex::Regex::new(r"http://127\.0\.0\.1:[0-9]+/\?token=[A-Za-z0-9_%.-]+")
        .unwrap()
        .find_iter(&log)
        .last()
        .map(|m| m.as_str().to_owned())
        .ok_or_else(|| "Native DeepSeek UI is unavailable.".into())
}
fn catalog_models(catalog: &Value) -> Vec<Value> {
    catalog["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| {
            group["models"].as_array().into_iter().flatten().map(|model| {
                json!({
                    "id":format!("{}/{}",string(group,"id"),string(model,"id")),
                    "label":format!("{} ({})",string(model,"name"),string(group,"name")),
                    "effort_options":model["reasoning"]["efforts"].as_array().into_iter().flatten().map(|effort|effort["id"].clone()).collect::<Vec<_>>(),
                    "default_effort":model["reasoning"]["defaultEffort"].clone(),
                })
            })
        })
        .collect()
}
/// The native account's model catalog through its resident host, never a host
/// start. Task #19: without a running host, the models the composed profile
/// configures ([`config_models`]), named as the host names them.
pub(crate) fn account_models(home: &Path) -> Value {
    let record = json!({"agent_home":home});
    match rpc(&record, "catalog", json!({})) {
        Ok(catalog) => account_models_from(&catalog),
        Err(error) => match cached_config_models(home) {
            Ok(mut models) if !models.is_empty() => {
                let truncated = models.len() > 64;
                models.truncate(64);
                json!({"models":models,"truncated":truncated,"source":"dsh-profile",
                    "provider_defaults":{"model":"","effort":""}})
            }
            _ => json!({"models":[],"truncated":false,"reason":error,"provider_defaults":{"model":"","effort":""}}),
        },
    }
}

/// Task #19: the composed profile's configuration is read again after this
/// long; account listings and the New team form ask repeatedly.
const MODELS_CACHE_SECONDS: f64 = 15.0;
static MODELS_CACHE: std::sync::Mutex<Option<(String, f64, Vec<Value>)>> = std::sync::Mutex::new(None);

fn cached_config_models(home: &Path) -> Result<Vec<Value>> {
    let key = home.display().to_string();
    if let Ok(cache) = MODELS_CACHE.lock() {
        if let Some((cached, at, models)) = cache.as_ref() {
            if cached == &key && now() - *at < MODELS_CACHE_SECONDS {
                return Ok(models.clone());
            }
        }
    }
    let models = config_models(home)?;
    if let Ok(mut cache) = MODELS_CACHE.lock() {
        *cache = Some((key, now(), models.clone()));
    }
    Ok(models)
}

/// Task #19: the models the composed profile's API-key route (`deepseek-official`)
/// offers: the `models` its `llm-deepseek` entry configures, else the
/// installed transport's bundled catalog, resolved by the transport itself
/// (`dsh --profile hgs --dump-config`, which starts no host). Never a guess:
/// a profile or harness that cannot be read gives an error.
fn config_models(home_dir: &Path) -> Result<Vec<Value>> {
    let dump = composed_config_dump(home_dir)?;
    let text = String::from_utf8(dump).map_err(|_| "The DeepSeek Harness reported an unreadable profile".to_owned())?;
    let mut request = json!({"dump":text});
    if let Some(root) = dsh_package_root() {
        request["packageRoot"] = json!(root.to_string_lossy().into_owned());
    }
    let value = node_json(CONFIG_MODELS, &request, Duration::from_secs(5), 64 * 1024)?;
    Ok(profile_models(&value))
}

/// The helper's models in the host catalog's shape: `provider/model` ids and
/// `Name (DeepSeek)` labels, the official route's group name.
fn profile_models(value: &Value) -> Vec<Value> {
    value["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|model| !string(model, "provider").is_empty() && !string(model, "id").is_empty())
        .take(256)
        .map(|model| {
            let name = if string(model, "name").is_empty() { string(model, "id") } else { string(model, "name") };
            json!({
                "id":format!("{}/{}",string(model,"provider"),string(model,"id")),
                "label":format!("{name} (DeepSeek)"),
                "effort_options":model["efforts"].as_array().cloned().unwrap_or_default(),
                "default_effort":model["defaultEffort"].clone(),
            })
        })
        .collect()
}

/// Task #19: a model named without its provider (`deepseek-flash`, as a team
/// member or the owner may name it) is the one catalog group that has it;
/// None when no group or several do.
fn qualified_model(catalog: &Value, model: &str) -> Option<String> {
    let groups: Vec<&str> = catalog["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|group| group["models"].as_array().is_some_and(|models| models.iter().any(|entry| entry["id"] == model)))
        .map(|group| string(group, "id"))
        .collect();
    match groups.as_slice() {
        [group] if !group.is_empty() => Some(format!("{group}/{model}")),
        _ => None,
    }
}

/// `mode` is the account permission mode a paused session resumes under.
fn apply_model(record: &Value, model: &str, effort: &str, mode: &str) -> Result<(String, Value)> {
    let selected = if model.contains('/') {
        model.to_owned()
    } else {
        let catalog = rpc(record, "catalog", json!({}))?;
        qualified_model(&catalog, model).ok_or("Choose a native provider and model")?
    };
    let selected = selected.as_str();
    let (provider, model) = selected
        .split_once('/')
        .ok_or("Choose a native provider and model")?;
    let mut params = json!({"sessionId":record["conversation_id"],"provider":provider,"model":model,"permissionMode":mode});
    if !effort.is_empty() {
        params["reasoningEffort"] = json!(effort);
    }
    let applied = rpc(record, "settings", params)?;
    Ok((selected.to_owned(), applied["selected"].clone()))
}
/// One configured permission preset the installed harness actually offers.
/// Only display metadata is kept; the native configuration never leaves the
/// bounded discovery helper.
#[derive(Clone, Debug, PartialEq)]
struct CatalogPreset {
    value: String,
    label: String,
    description: Option<String>,
}

const CATALOG_OPTIONS_LIMIT: usize = 64;
/// Account listings ask repeatedly; discovery runs a bounded local command, so
/// one answer serves this long. Launches validate against a fresh catalog.
const CATALOG_CACHE_SECONDS: f64 = 15.0;
static CATALOG_CACHE: std::sync::Mutex<Option<(String, f64, Value)>> = std::sync::Mutex::new(None);

fn display_text(value: &str, limit: usize) -> String {
    let cleaned: String = value.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_owned();
    }
    let mut clipped: String = trimmed.chars().take(limit.saturating_sub(3)).collect();
    clipped.push_str("...");
    clipped
}

fn valid_preset_name(name: &str) -> bool {
    // Native keys are arbitrary configured strings; the /permission command
    // takes the whole trimmed remainder. Keep the value bounded, untrimmed
    // edges unselectable (the native handler trims) and control-free.
    !name.is_empty()
        && name.len() <= 128
        && name.trim() == name
        && !name.chars().any(char::is_control)
}

fn valid_control_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 64 {
        return false;
    }
    let mut chars = id.chars();
    let Some(first) = chars.next() else { return false };
    first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn presets_from_options(options: &Value) -> Result<Vec<CatalogPreset>> {
    let options = options
        .as_array()
        .ok_or("The DeepSeek Harness did not report permission presets")?;
    if options.is_empty() {
        return Err("The DeepSeek Harness reported no selectable permission presets".into());
    }
    if options.len() > CATALOG_OPTIONS_LIMIT {
        return Err("The DeepSeek Harness reported too many permission presets".into());
    }
    let mut presets = Vec::new();
    for option in options {
        let name = option["value"]
            .as_str()
            .or_else(|| option["name"].as_str())
            .ok_or("The DeepSeek Harness reported an invalid permission preset")?;
        if !valid_preset_name(name) {
            return Err("The DeepSeek Harness reported a permission preset this session cannot select".into());
        }
        if presets.iter().any(|preset: &CatalogPreset| preset.value == name) {
            continue;
        }
        let label = option["label"].as_str().unwrap_or(name);
        let description = option["description"].as_str().filter(|text| !text.trim().is_empty());
        presets.push(CatalogPreset {
            value: name.to_owned(),
            label: display_text(label, 80),
            description: description.map(|text| display_text(text, 240)),
        });
    }
    if presets.len() > CATALOG_OPTIONS_LIMIT {
        return Err("The DeepSeek Harness reported too many permission presets".into());
    }
    Ok(presets)
}

fn catalog_options(presets: &[CatalogPreset]) -> Vec<Value> {
    presets
        .iter()
        .map(|preset| {
            let mut option = json!({"value":preset.value,"label":preset.label});
            if let Some(description) = &preset.description {
                option["description"] = json!(description);
            }
            option
        })
        .collect()
}

fn available_catalog(
    presets: &[CatalogPreset],
    default: Option<String>,
    version: Option<String>,
    source: &str,
) -> Value {
    let default = default.filter(|name| presets.iter().any(|preset| &preset.value == name));
    let mut control = json!({"id":"preset","label":"Permission preset","kind":"select","scope":"launch",
        "options":catalog_options(presets)});
    if let Some(name) = &default {
        control["default"] = json!(name);
    }
    let mut catalog = json!({"status":"available","source":source,"session_supported":true,
        "selected":default.as_ref().map(|name| json!({"preset":name})).unwrap_or_else(|| json!({})),
        "controls":[control]});
    if let Some(version) = version {
        catalog["version"] = json!(version);
    }
    catalog
}

/// No native discovery: the backend keeps its Default/Bypass shortcuts, so the
/// catalog carries no synthesized native options.
fn unavailable_catalog(reason: &str, version: Option<String>) -> Value {
    let mut catalog = json!({"status":"unavailable","status_reason":display_text(reason, 240),
        "source":"dsh-profile","session_supported":true,"selected":{},"controls":[]});
    if let Some(version) = version {
        catalog["version"] = json!(version);
    }
    catalog
}

fn dsh_home(config: &Config) -> PathBuf {
    nonempty_env("DSH_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(&config.home).join(".dsh"))
}

fn package_is_dsh(directory: &Path) -> bool {
    fs::read_to_string(directory.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|value| value["name"] == "@deepseek-ai/dsh")
}

fn package_root_from(path: &Path) -> Option<PathBuf> {
    let mut directory = path.parent()?;
    for _ in 0..6 {
        if package_is_dsh(directory) {
            return Some(directory.to_owned());
        }
        directory = directory.parent()?;
    }
    None
}

/// The installed package of the same `dsh` the host launch resolves. The
/// account listing runs in a different shell than an agent, so never consult
/// another PATH entry.
fn dsh_package_root() -> Option<PathBuf> {
    if let Ok(command) = dsh_command() {
        if let Some(first) = command.get_args().next() {
            let script = PathBuf::from(first.to_os_string());
            if script.is_file() {
                if let Some(root) = package_root_from(&script) {
                    return Some(root);
                }
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join("dsh");
        if !candidate.is_file() {
            continue;
        }
        let Ok(real) = fs::canonicalize(&candidate) else { continue };
        if let Some(root) = package_root_from(&real) {
            return Some(root);
        }
    }
    None
}

fn installed_version() -> Option<String> {
    let root = dsh_package_root()?;
    let value: Value = serde_json::from_slice(&fs::read(root.join("package.json")).ok()?).ok()?;
    let version = value["version"].as_str()?;
    (version.len() <= 64
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '_')))
    .then(|| version.to_owned())
}

fn node_program() -> PathBuf {
    if let Ok(command) = dsh_command() {
        let program = command.get_program();
        let name = Path::new(program).file_name().and_then(|name| name.to_str()).unwrap_or("");
        if name.eq_ignore_ascii_case("node") || name.eq_ignore_ascii_case("node.exe") {
            return PathBuf::from(program.to_os_string());
        }
    }
    PathBuf::from("node")
}

/// One bounded native helper process through the maintained runner: private
/// bounded input, a hard deadline and the child's whole process group. No
/// reader thread or unbounded wait outlives the call.
fn bounded_output(
    mut command: Command,
    input: Option<&[u8]>,
    limit: usize,
    timeout: Duration,
) -> Result<(bool, Vec<u8>)> {
    let input = match input {
        Some(bytes) => {
            // A regular file already holding the request: a helper that never
            // reads stdin cannot block this process the way a pipe writer would.
            let mut file = tempfile::tempfile().map_err(|e| e.to_string())?;
            file.write_all(bytes).map_err(|e| e.to_string())?;
            file.seek(std::io::SeekFrom::Start(0)).map_err(|e| e.to_string())?;
            Stdio::from(file)
        }
        None => Stdio::null(),
    };
    let deadline = Instant::now() + timeout;
    let (status, stdout, _stderr) = crate::platform::run_bounded_stdin(
        &mut command,
        input,
        [limit, 16 * 1024],
        deadline,
        &|| false,
    )
    .map_err(|error| match error {
        crate::platform::RunError::TooLarge => {
            "Native permission discovery returned too much data".to_owned()
        }
        crate::platform::RunError::Stopped => "Native permission discovery timed out".to_owned(),
        crate::platform::RunError::Spawn(error) => {
            format!("Native permission discovery could not start: {error}")
        }
        _ => "Native permission discovery failed".to_owned(),
    })?;
    Ok((status.success(), stdout))
}

fn node_json(script: &str, request: &Value, timeout: Duration, limit: usize) -> Result<Value> {
    let mut command = Command::new(node_program());
    command.args(["--input-type=module", "-e", script]);
    let input = request.to_string();
    let (_, output) = bounded_output(command, Some(input.as_bytes()), limit, timeout)?;
    let value: Value = serde_json::from_slice(&output)
        .map_err(|_| "Native permission helper returned an invalid answer".to_owned())?;
    if value["ok"] == true {
        return Ok(value);
    }
    let reason = string(&value, "reason");
    Err(if reason.is_empty() { "unknown".to_owned() } else { reason.to_owned() })
}

fn composed_config_dump(home_dir: &Path) -> Result<Vec<u8>> {
    // The first host launch composes the hgs profile from the shipped web
    // profile (--from-default-profile). Before that profile exists, discover
    // the same shipped table instead of reporting a missing profile.
    let profile = if home_dir.join("profiles/hgs/package.json").is_file() {
        "hgs"
    } else {
        "web"
    };
    let mut command = dsh_command()
        .map_err(|_| "Install the official DeepSeek Harness to read its permission presets".to_owned())?;
    command
        .args(["--profile", profile, "--dump-config"])
        .env("DSH_HOME", home_dir)
        .current_dir(home())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let (success, output) = bounded_output(command, None, 256 * 1024, Duration::from_secs(3))?;
    if !success {
        return Err("The DeepSeek Harness could not report its configured permission presets".into());
    }
    Ok(output)
}

/// The composed profile's configured presets. `--dump-config` exits without
/// starting a host or creating a session; only the preset row is parsed.
fn composed_presets(config: &Config) -> Result<(Vec<CatalogPreset>, Option<String>)> {
    let home_dir = dsh_home(config);
    let root = dsh_package_root();
    let dump = composed_config_dump(&home_dir)?;
    let text = String::from_utf8(dump)
        .map_err(|_| "The DeepSeek Harness reported unreadable permission presets".to_owned())?;
    let mut request = json!({"dump":text});
    if let Some(root) = root.as_deref() {
        request["packageRoot"] = json!(root.to_string_lossy().into_owned());
    }
    let value = node_json(CONFIG_PRESETS, &request, Duration::from_secs(2), 64 * 1024)?;
    let presets = presets_from_options(&value["presets"])?;
    let default = value["default"].as_str().map(str::to_owned);
    Ok((presets, default))
}

fn permissions_compat_json(
    url: &str,
    session: Option<&str>,
    operation: &str,
    preset: Option<&str>,
    pending: bool,
    explicit: bool,
) -> Result<Value> {
    let script = concat!(
        include_str!("../dsh/account-client.mjs"),
        "\n",
        include_str!("../dsh/permissions-compat.mjs")
    );
    let mut request = json!({"url":url,"operation":operation,"pending":pending,"explicit":explicit});
    if let Some(session) = session {
        request["sessionId"] = json!(session);
    }
    if let Some(preset) = preset {
        request["preset"] = json!(preset);
    }
    node_json(script, &request, Duration::from_secs(30), 64 * 1024)
}

/// The running host's authenticated catalog, read through its own launch
/// token. Never starts a host; an absent host fails and the caller falls back.
fn runtime_presets(record: &Value) -> Result<(Vec<CatalogPreset>, Option<String>)> {
    let home_dir = Path::new(string(record, "agent_home"));
    let url = account_ui_url(home_dir)?;
    let value = permissions_compat_json(&url, None, "catalog", None, false, false)?;
    let presets = presets_from_options(&value["options"])?;
    let default = value["default"].as_str().map(str::to_owned);
    Ok((presets, default))
}

/// Fresh validation before a launch: the running host's actual catalog first,
/// the composed profile when the host cannot answer yet.
fn fresh_presets(record: &Value) -> Result<Vec<CatalogPreset>> {
    if let Ok((presets, _)) = runtime_presets(record) {
        return Ok(presets);
    }
    let config = Config::load().map_err(|e| e.message)?;
    composed_presets(&config).map(|(presets, _)| presets)
}

fn permission_apply_error(reason: &str, preset: bool) -> String {
    if preset {
        match reason {
            "preset" => "The selected permission preset is no longer offered by DeepSeek; choose another preset before resuming.".into(),
            "busy" => "This DeepSeek session is already running; its permission preset applies at its next start or explicit resume. Stop it first.".into(),
            "state" => "The DeepSeek session is not running yet; resume it to apply the permission preset.".into(),
            "command" => "The running DeepSeek host does not expose its permission command; use Native UI or resume after its next ordinary start.".into(),
            _ => "DeepSeek did not confirm the permission preset. Resume this session to finish setup; no message was sent.".into(),
        }
    } else {
        match reason {
            "preset" => "The running DeepSeek host does not offer the full-access preset that Bypass uses; choose Default or a native preset.".into(),
            "busy" => "This DeepSeek session is already running; its permission mode applies at its next start or explicit resume. Stop it first.".into(),
            "state" => "The DeepSeek session is not running yet; resume it to finish permission setup.".into(),
            _ => "DeepSeek did not confirm bypass permissions. Resume this session to finish setup; no message was sent.".into(),
        }
    }
}

/// The account row's sanitized permission catalog. Discovery reads the same
/// resolved harness release the host launch uses, never starts a host, starts
/// no session and runs no shell in an agent's environment.
pub(crate) fn permission_catalog(config: &Config) -> Result<Value> {
    let key = dsh_home(config).display().to_string();
    if let Ok(cache) = CATALOG_CACHE.lock() {
        if let Some((cached, at, value)) = cache.as_ref() {
            if cached == &key && now() - *at < CATALOG_CACHE_SECONDS {
                return Ok(value.clone());
            }
        }
    }
    let version = installed_version();
    let value = match composed_presets(config) {
        Ok((presets, default)) => available_catalog(&presets, default, version, "dsh-profile"),
        Err(_) => {
            let reason = if dsh_command().is_err() {
                "Install the official DeepSeek Harness to read its permission presets."
            } else {
                "Could not read the configured permission presets from the installed DeepSeek Harness."
            };
            unavailable_catalog(reason, version)
        }
    };
    if let Ok(mut cache) = CATALOG_CACHE.lock() {
        *cache = Some((key, now(), value.clone()));
    }
    Ok(value)
}

/// H3: the host serving a member's dsh session: its pid and when it started
/// (the pid file's time), for team status. Read only.
pub(crate) fn host_of(name: &str) -> Option<Value> {
    let record = binding(name).ok()?;
    let path = host_dir(&record).join("host.pid");
    let pid: u32 = fs::read_to_string(&path).ok()?.trim().parse().ok()?;
    let started = fs::metadata(&path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    Some(json!({"pid":pid,"started":started}))
}

/// H4: the arguments Zerus gives the native DeepSeek host. They never enable
/// session-log upload (`session-log-deepseek`); only the private overlay,
/// profile and local socket are set.
fn host_arguments(overlay: &Path, default_profile: bool) -> Vec<String> {
    let mut args = vec!["--profile".to_owned(), "hgs".to_owned()];
    if default_profile {
        args.extend(["--from-default-profile".to_owned(), "web".to_owned()]);
    }
    args.extend([
        "--patch".to_owned(),
        overlay.to_string_lossy().into_owned(),
        "--no-open".to_owned(),
        "--host".to_owned(),
        "127.0.0.1".to_owned(),
        "--port".to_owned(),
        "0".to_owned(),
    ]);
    args
}

/// H4: the one overlay Zerus writes for the host: its private bridge only.
fn host_overlay(script: &Path, config: Value, block_upload: bool) -> Value {
    let mut insert = vec![json!({"id":"hgs-native-dsh","name":script,"config":config})];
    if block_upload {
        // U22: the row replaces the shipped plugin's config; the user's global
        // dsh configuration is never edited.
        insert.push(session_upload::overlay_row()["insert"][0].clone());
    }
    json!([{"insert":insert}])
}

fn host_dir(record: &Value) -> PathBuf {
    root().join("dsh/hosts").join(
        &format!(
            "{:x}",
            Sha256::digest(string(record, "agent_home").as_bytes())
        )[..16],
    )
}
/// The bridge's private endpoint: a Unix socket in the host directory.
#[cfg(unix)]
fn endpoint(dir: &Path) -> PathBuf {
    dir.join("host.sock")
}
/// On Windows a named pipe named after the host directory, which is per user
/// and per native home. Its default security lets other accounts open it only
/// for reading, which cannot send the bridge a request.
#[cfg(windows)]
fn endpoint(dir: &Path) -> PathBuf {
    let digest = format!("{:x}", Sha256::digest(dir.to_string_lossy().as_bytes()));
    PathBuf::from(format!(r"\\.\pipe\hgs-dsh-{}", &digest[..32]))
}
const HOST_UNAVAILABLE: &str = "DeepSeek host is unavailable; resume the session to reconnect";
/// A request the host never got whole. Every `bridge.mjs` since the first
/// runs a request only once a chunk holds its terminating newline and has no
/// end handler, so a request cut short never runs: a definite refusal
/// (zerus-1wchz, pinned by `the_bridge_runs_only_a_newline_terminated_request`).
const INCOMPLETE: &str = "The DeepSeek host never received a complete request";
#[cfg(unix)]
fn connect(path: &Path) -> io::Result<UnixStream> {
    UnixStream::connect(path)
}
#[cfg(windows)]
fn connect(path: &Path) -> io::Result<fs::File> {
    // ERROR_PIPE_BUSY: every listening instance is taken; the bridge adds more.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match OpenOptions::new().read(true).write(true).open(path) {
            Err(error) if error.raw_os_error() == Some(231) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            result => return result,
        }
    }
}
/// The official harness as a session would start it. On Windows its npm shim
/// resolves to node and the package's script, never through cmd.exe.
#[cfg(unix)]
fn dsh_command() -> io::Result<Command> {
    Ok(Command::new("dsh"))
}
#[cfg(windows)]
fn dsh_command() -> io::Result<Command> {
    crate::platform::agent_command(&["dsh".to_owned()])
}
fn rpc(record: &Value, method: &str, params: Value) -> Result<Value> {
    rpc_within(record, method, params, Duration::from_secs(35))
}
/// Interactive search waits at most this long for one host: the adapter's own
/// 5 s scan budget plus a margin. A host that does not answer counts as
/// unavailable instead of holding up every other provider's results.
fn search_rpc(record: &Value, params: Value) -> Result<Value> {
    rpc_within(record, "search-history", params, Duration::from_secs(7))
}
/// WhiteWh fork: the list rows of these conversations and their subagents. A
/// bridge with `rows` reads only them; an older one lists every stored session
/// of the host's home, which takes seconds on a long history.
fn session_rows(record: &Value, conversations: &[&str]) -> Result<Value> {
    match rpc(record, "rows", json!({ "sessionIds": conversations })) {
        Err(error) if error == "Unsupported HGS native request" => rpc(record, "list", json!({})),
        rows => rows,
    }
}

/// Task 234: how long a stopped conversation's row is reused when the host
/// cannot say which sessions it has loaded (a bridge before `loadedSessions`).
/// A launch or resume through Zerus gives the binding a new run, which asks
/// again at once; only a session loaded in the host's own web UI waits for it.
const STOPPED_ROW_SECONDS: f64 = 900.0;
/// With `loadedSessions` a stopped row stays valid until the host loads the
/// session; this bounds it all the same.
const STOPPED_ROW_MAX_SECONDS: f64 = 3600.0;
/// At most this many rows are kept per host.
const STOPPED_ROWS_KEPT: usize = 2000;
/// A listing refreshes at most this many rows past their bound (the oldest
/// first) and reuses the others until their turn: refreshing all twelve
/// stopped rows of this machine at once took 7 s again every 5 minutes.
const STALE_ROWS_PER_LISTING: usize = 1;

/// Task #47 (team ede724c4, 2026-11-10): how long a listing waits for the
/// host's row of one conversation. Each conversation is asked on its own, side
/// by side, so one slow conversation holds up only its own row: the host took
/// 45 s (its socket timeout) and answered nothing for two stopped sessions
/// whose projections it rebuilt, and the one batch request held every
/// `hgs ls` for its 35 s.
const ROWS_WAIT: Duration = Duration::from_millis(1500);
/// Task #47: a conversation whose row did not come within `ROWS_WAIT` is asked
/// again only after this long. The host keeps rebuilding a projection it was
/// asked for after the request is gone, so asking on every listing piled that
/// work up (the host ran 1.5 cores for hours); its last row stands meanwhile.
const SLOW_RETRY_SECONDS: f64 = 900.0;

fn rows_cache_path(record: &Value) -> PathBuf {
    host_dir(record).join("listing-rows.json")
}

/// A row the host built for a session it has not loaded: neither available
/// nor running. Building one replays the stored history (about half a second
/// each on a long session), and it changes only when the session is loaded.
fn stopped_row(row: &Value) -> bool {
    row["agentAvailable"] != true && row["running"] != true && row["pendingQuestions"].as_array().is_none_or(Vec::is_empty)
}

/// Task 234 (owner's hgs ls under 1 s): which conversations of one host a
/// listing asks for, and which stopped rows it reuses from `cache` (the
/// host's last answers, `{generation, rows: {id: {row, run_id, at}}}`).
/// A cached row is reused while the host's generation and the binding's run
/// are the same and the session is not loaded (`loaded`, when the host names
/// them). A row past its bound is refreshed, at most
/// `STALE_ROWS_PER_LISTING` a listing, oldest first; the others are reused
/// until their turn. A reused row brings its subagents' cached rows along.
fn plan_rows(cache: &Value, records: &[&Value], generation: &Value, loaded: Option<&BTreeSet<String>>, now: f64) -> (Vec<Value>, Vec<String>) {
    let bound = if loaded.is_some() { STOPPED_ROW_MAX_SECONDS } else { STOPPED_ROW_SECONDS };
    let same_host = !generation.is_null() && cache["generation"] == *generation;
    let mut ask: Vec<String> = Vec::new();
    let mut valid: Vec<(f64, &str)> = Vec::new();
    for record in records {
        let id = string(record, "conversation_id");
        if id.is_empty() || ask.iter().any(|seen| seen == id) || valid.iter().any(|(_, seen)| *seen == id) {
            continue;
        }
        let entry = &cache["rows"][id];
        let at = entry["at"].as_f64().filter(|at| now >= *at);
        match at {
            Some(at) if same_host && entry["run_id"] == record["run_id"] && loaded.is_none_or(|loaded| !loaded.contains(id)) => {
                valid.push((at, id));
            }
            _ => ask.push(id.to_owned()),
        }
    }
    let mut stale: Vec<(f64, &str)> = valid.iter().copied().filter(|(at, _)| now - at >= bound).collect();
    stale.sort_by(|a, b| a.0.total_cmp(&b.0));
    let refresh: BTreeSet<&str> = stale.into_iter().take(STALE_ROWS_PER_LISTING).map(|(_, id)| id).collect();
    let mut reused = Vec::new();
    for (_, id) in valid {
        if refresh.contains(id) {
            ask.push(id.to_owned());
            continue;
        }
        let entry = &cache["rows"][id];
        reused.push(entry["row"].clone());
        for child in entry["row"]["projections"]["values"]["subagentCatalog"].as_array().into_iter().flatten() {
            if let Some(found) = child["id"].as_str().map(|child| &cache["rows"][child]["row"]).filter(|row| row.is_object()) {
                reused.push(found.clone());
            }
        }
    }
    (reused, ask)
}

/// The cache after an answer: its stopped rows are kept (with the run of the
/// binding they answered for; a subagent keeps its parent's), its live rows
/// leave; rows of conversations no longer bound go, and the oldest past
/// `STOPPED_ROWS_KEPT`.
fn update_rows(cache: &Value, records: &[&Value], generation: &Value, items: &[Value], now: f64) -> Value {
    let mut rows = if cache["generation"] == *generation { cache["rows"].as_object().cloned().unwrap_or_default() } else { Map::new() };
    let runs: BTreeMap<&str, &Value> = records.iter().map(|record| (string(record, "conversation_id"), &record["run_id"])).collect();
    let mut parent_run: BTreeMap<String, Value> = BTreeMap::new();
    for item in items {
        if let Some(run) = item["sessionId"].as_str().and_then(|id| runs.get(id)) {
            for child in item["projections"]["values"]["subagentCatalog"].as_array().into_iter().flatten() {
                if let Some(child) = child["id"].as_str() {
                    parent_run.insert(child.to_owned(), (*run).clone());
                }
            }
        }
    }
    for item in items {
        let Some(id) = item["sessionId"].as_str() else { continue };
        let run = runs.get(id).map(|run| (*run).clone()).or_else(|| parent_run.get(id).cloned());
        match run.filter(|_| stopped_row(item)) {
            Some(run) => {
                rows.insert(id.to_owned(), json!({"row":item,"run_id":run,"at":now}));
            }
            None => {
                rows.remove(id);
            }
        }
    }
    let bound: BTreeSet<&str> = runs.keys().copied().collect();
    let children: BTreeSet<String> = rows
        .iter()
        .filter(|(id, _)| bound.contains(id.as_str()))
        .flat_map(|(_, entry)| entry["row"]["projections"]["values"]["subagentCatalog"].as_array().cloned().unwrap_or_default())
        .filter_map(|child| child["id"].as_str().map(str::to_owned))
        .collect();
    rows.retain(|id, _| bound.contains(id.as_str()) || children.contains(id));
    if rows.len() > STOPPED_ROWS_KEPT {
        let mut ages: Vec<(f64, String)> = rows.iter().map(|(id, entry)| (entry["at"].as_f64().unwrap_or(0.0), id.clone())).collect();
        ages.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, id) in ages.into_iter().take(rows.len() - STOPPED_ROWS_KEPT) {
            rows.remove(&id);
        }
    }
    json!({"generation":generation,"rows":rows})
}

/// Task 234: the listing's rows of one host's conversations. One `ping`
/// (milliseconds) names the host's generation, and a newer bridge the
/// sessions it has loaded; a stopped conversation's row is reused from the
/// host's last answer, and only the others go to `rows`. Without a cache, or
/// when the ping fails, every conversation is asked for, as before.
fn listing_rows(records: &[&Value]) -> Result<Value> {
    let Some(first) = records.first() else { return Ok(json!({"items":[]})) };
    let conversations: Vec<&str> = records.iter().map(|record| string(record, "conversation_id")).collect();
    let Ok(ping) = rpc_within(first, "ping", json!({}), Duration::from_secs(5)) else {
        return session_rows(first, &conversations);
    };
    let generation = ping["generation"].clone();
    let loaded: Option<BTreeSet<String>> =
        ping["loadedSessions"].as_array().map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_owned).collect());
    let path = rows_cache_path(first);
    let cache = fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()).unwrap_or(Value::Null);
    let now = now();
    let (mut items, planned) = plan_rows(&cache, records, &generation, loaded.as_ref(), now);
    // Task #47: a conversation that was slow lately is not asked again yet;
    // its last row (if the host ever answered one) stands.
    let (resting, ask) = rest_slow(&cache, &generation, planned, now);
    for id in &resting {
        if let Some(row) = Some(&cache["rows"][id.as_str()]["row"]).filter(|row| row.is_object()) {
            items.push(row.clone());
        }
    }
    let mut answer_generation = generation.clone();
    if !ask.is_empty() {
        let (answers, slow) = rows_each(&ask, |id| rpc_within(first, "rows", json!({ "sessionIds": [id] }), ROWS_WAIT));
        let mut fresh = Vec::new();
        for answer in &answers {
            if !answer["generation"].is_null() {
                answer_generation = answer["generation"].clone();
            }
            fresh.extend(answer["items"].as_array().cloned().unwrap_or_default());
        }
        // A bridge without `rows` lists every stored session at once, as
        // before (bounded like one row).
        if answers.is_empty() && slow.iter().any(|(_, error)| error == "Unsupported HGS native request") {
            return rpc_within(first, "list", json!({}), Duration::from_secs(35));
        }
        // A host that answered with another generation restarted meanwhile:
        // its reused rows are not trusted; only what it answered now stands.
        if answer_generation != generation {
            return Ok(json!({"generation":answer_generation,"items":fresh}));
        }
        let slow: Vec<String> = slow.into_iter().map(|(id, _)| id).collect();
        let mut updated = update_rows(&cache, records, &generation, &fresh, now);
        updated["slow"] = slow_marks(&cache, &generation, records, &fresh, &slow, now);
        if updated != cache && private_dir(path.parent().unwrap_or(Path::new("."))).is_ok() {
            let _ = atomic_private(&path, &updated.to_string());
        }
        for id in &slow {
            if let Some(row) = Some(&cache["rows"][id.as_str()]["row"]).filter(|row| row.is_object()) {
                items.push(row.clone());
            }
        }
        items.extend(fresh);
    }
    Ok(json!({"generation":answer_generation,"items":items}))
}

/// Task #47: of the conversations a listing would ask for, the ones that did
/// not answer within `ROWS_WAIT` less than `SLOW_RETRY_SECONDS` ago on this
/// host generation rest; the others are asked. Returns (resting, ask).
fn rest_slow(cache: &Value, generation: &Value, planned: Vec<String>, now: f64) -> (Vec<String>, Vec<String>) {
    let same_host = !generation.is_null() && cache["generation"] == *generation;
    planned.into_iter().partition(|id| {
        same_host && cache["slow"][id.as_str()]["at"].as_f64().is_some_and(|at| at <= now && now - at < SLOW_RETRY_SECONDS)
    })
}

/// Task #47: asks `ask` one conversation per request, side by side, each
/// within `ROWS_WAIT`. Returns the answers, and the conversations that gave
/// none in time (or an error) with their error.
fn rows_each(ask: &[String], row: impl Fn(&str) -> Result<Value> + Sync) -> (Vec<Value>, Vec<(String, String)>) {
    let results: Vec<(String, Result<Value>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = ask.iter().map(|id| (id.clone(), scope.spawn(|| row(id)))).collect();
        handles
            .into_iter()
            .map(|(id, handle)| (id, handle.join().unwrap_or_else(|_| Err("the row request failed".into()))))
            .collect()
    });
    let mut answers = Vec::new();
    let mut slow = Vec::new();
    for (id, result) in results {
        match result {
            Ok(answer) => answers.push(answer),
            Err(error) => slow.push((id, error)),
        }
    }
    (answers, slow)
}

/// Task #47: the cache's slow marks after an answer: kept on the same host
/// generation for conversations still bound and not answered now, and set
/// now for those that gave no row in time.
fn slow_marks(cache: &Value, generation: &Value, records: &[&Value], fresh: &[Value], slow: &[String], now: f64) -> Value {
    let mut marks = if cache["generation"] == *generation { cache["slow"].as_object().cloned().unwrap_or_default() } else { Map::new() };
    let bound: BTreeSet<&str> = records.iter().map(|record| string(record, "conversation_id")).collect();
    let answered: BTreeSet<&str> = fresh.iter().filter_map(|item| item["sessionId"].as_str()).collect();
    marks.retain(|id, _| bound.contains(id.as_str()) && !answered.contains(id.as_str()));
    for id in slow {
        marks.insert(id.clone(), json!({"at":now}));
    }
    Value::Object(marks)
}
fn rpc_within(record: &Value, method: &str, params: Value, read_timeout: Duration) -> Result<Value> {
    #[cfg(unix)]
    {
        return rpc_at(&host_dir(record).join("host.sock"), method, params, read_timeout);
    }
    #[cfg(not(unix))]
    {
        let request = json!({"method":method,"params":params}).to_string();
        let response = exchange(&endpoint(&host_dir(record)), &request, read_timeout)?;
        parse_response(&response)
    }
}
#[cfg(unix)]
fn rpc_at(path: &Path, method: &str, params: Value, read_timeout: Duration) -> Result<Value> {
    let socket = crate::platform::connect_unix(path, read_timeout).map_err(|_| HOST_UNAVAILABLE)?;
    exchange_socket(socket, method, params, read_timeout)
}
/// One request over a connected host socket.
#[cfg(unix)]
fn exchange_socket(
    mut socket: UnixStream,
    method: &str,
    params: Value,
    read_timeout: Duration,
) -> Result<Value> {
    // The request's last byte is its newline, and the compact JSON holds no
    // other, so a failed write never wrote it and the host never ran it.
    (|| -> io::Result<()> {
        socket.set_read_timeout(Some(read_timeout))?;
        socket.set_write_timeout(Some(Duration::from_secs(10)))?;
        writeln!(socket, "{}", json!({"method":method,"params":params}))
    })()
    .map_err(|e| format!("{INCOMPLETE}: {e}"))?;
    let mut response = String::new();
    BufReader::new(socket.take(32 * 1024 * 1024))
        .read_line(&mut response)
        .map_err(|e| format!("Native request delivery uncertain: {e}"))?;
    parse_response(&response)
}
fn parse_response(response: &str) -> Result<Value> {
    let response: Value = serde_json::from_str(response)
        .map_err(|e| format!("Invalid native response; delivery uncertain: {e}"))?;
    if response["ok"] != true {
        return Err(string(&response, "error").into());
    }
    Ok(response["value"].clone())
}
/// A refused connect is ECONNREFUSED, or ECONNRESET on some BSD paths: the
/// host's socket is there but the connect was not queued (zerus-xt6re).
#[cfg(unix)]
fn refused(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
    )
}
/// The claim's connect, with a test-only refusal so the BSD full-queue answer
/// (ECONNREFUSED at once) can be exercised on Linux (zerus-xt6re).
#[cfg(unix)]
fn claim_connect(path: &Path, limit: Duration) -> io::Result<UnixStream> {
    #[cfg(test)]
    if let Some(kind) = injected_refusal() {
        return Err(io::Error::new(kind, "injected connect refusal"));
    }
    crate::platform::connect_unix(path, limit)
}
#[cfg(all(test, unix))]
thread_local! {
    static INJECTED_REFUSAL: std::cell::Cell<Option<io::ErrorKind>> =
        const { std::cell::Cell::new(None) };
}
#[cfg(all(test, unix))]
fn injected_refusal() -> Option<io::ErrorKind> {
    INJECTED_REFUSAL.with(std::cell::Cell::get)
}
#[cfg(all(test, unix))]
pub(super) fn refuse_connect_for_test(kind: io::ErrorKind) {
    INJECTED_REFUSAL.with(|refused| refused.set(Some(kind)));
}
#[cfg(all(test, unix))]
pub(super) fn allow_connect_for_test() {
    INJECTED_REFUSAL.with(|refused| refused.set(None));
}
/// A socket path exists and its verified recorded owner process is alive:
/// a refused connect to it means the host is saturated, not gone.
#[cfg(unix)]
fn live_owner_socket(dir: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    fs::symlink_metadata(dir.join("host.sock")).is_ok_and(|meta| meta.file_type().is_socket())
        && host_identity::owner_alive(dir)
}
/// One request line and its response line.
#[cfg(unix)]
fn exchange(path: &Path, request: &str, read_timeout: Duration) -> Result<String> {
    let mut socket = connect(path)
        .map_err(|_| "DeepSeek host is unavailable; resume the session to reconnect")?;
    socket
        .set_read_timeout(Some(read_timeout))
        .map_err(|e| e.to_string())?;
    socket
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    writeln!(socket, "{request}").map_err(|e| e.to_string())?;
    let mut response = String::new();
    BufReader::new(socket.take(32 * 1024 * 1024))
        .read_line(&mut response)
        .map_err(|e| format!("Native request delivery uncertain: {e}"))?;
    Ok(response)
}
#[cfg(windows)]
fn exchange(path: &Path, request: &str, read_timeout: Duration) -> Result<String> {
    let mut pipe = connect(path)
        .map_err(|_| "DeepSeek host is unavailable; resume the session to reconnect")?;
    writeln!(pipe, "{request}").map_err(|e| e.to_string())?;
    // A pipe handle has no read timeout: the deadline abandons a reading thread,
    // which ends with this short-lived process.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut response = String::new();
        let result = BufReader::new(pipe.take(32 * 1024 * 1024))
            .read_line(&mut response)
            .map(|_| response);
        let _ = sender.send(result);
    });
    match receiver.recv_timeout(read_timeout) {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(e)) => Err(format!("Native request delivery uncertain: {e}")),
        Err(_) => Err("Native request delivery uncertain: the DeepSeek host did not answer in time".into()),
    }
}
/// How long a host start waits for the native host to answer.
const HOST_START_SECONDS: u64 = 35;

/// Task 22: the pid of a host an earlier start launched that is still running
/// and was launched within `within` (the time of `host.pid`), so it may only be
/// slow to answer. A start finding one waits for it instead of launching a
/// second host beside it. An old pid file or an ended process is no host.
fn starting_host(dir: &Path, within: Duration) -> Option<u32> {
    let path = dir.join("host.pid");
    let pid: u32 = fs::read_to_string(&path).ok()?.trim().parse().ok()?;
    let age = fs::metadata(&path).ok()?.modified().ok()?.elapsed().unwrap_or_default();
    if age > within {
        return None;
    }
    // A host this process launched that has ended stays a zombie, which still
    // looks alive, until it is reaped; another process's pid is left alone.
    #[cfg(unix)]
    if let Ok(id) = libc::pid_t::try_from(pid) {
        let mut status = 0;
        if id > 0 && unsafe { libc::waitpid(id, &mut status, libc::WNOHANG) } == id {
            return None;
        }
    }
    crate::platform::process_alive(pid).then_some(pid)
}

/// Task 22: waits for a host that is starting to answer; `still` says whether
/// it is still worth waiting for (its process runs).
fn wait_for_host(record: &Value, deadline: Instant, still: &mut dyn FnMut() -> Result<bool>) -> Result<()> {
    while Instant::now() < deadline {
        if let Ok(value) = rpc(record, "ping", json!({})) {
            if value["protocol"] == 1 {
                return Ok(());
            }
        }
        if !still()? {
            return Err(format!(
                "Official DeepSeek host failed to start. Private log: {}",
                host_dir(record).join("host.log").display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("DeepSeek host is still starting; retry opening the session".into())
}

fn ensure_host(record: &Value) -> Result<bool> {
    let dir = host_dir(record);
    private_dir(&dir)?;
    let _guard = lock(Some(&dir.join("start.lock")))?;
    if let Ok(ping) = rpc(record, "ping", json!({})) {
        if compatible_adapter(&ping) {
            // N17: a host that was already running keeps the block state it was
            // started with; sessions launched into it may still upload.
            return Ok(running_host_block_stale(&dir));
        }
        return Err("The running DeepSeek host uses an incompatible adapter; close its sessions before upgrading".into());
    }
    match connect(&endpoint(&dir)) {
        Ok(_) => return Err("The running DeepSeek host is not responding; it has been left running. Retry when it recovers".into()),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {},
        Err(e) => return Err(format!("Cannot check the native host: {e}")),
    }
    // Task 22: an earlier start's host that has not opened its endpoint yet
    // is waited for, never doubled by a second host.
    let recent = Duration::from_secs(4 * HOST_START_SECONDS);
    if let Some(pid) = starting_host(&dir, recent) {
        let deadline = Instant::now() + Duration::from_secs(HOST_START_SECONDS);
        wait_for_host(record, deadline, &mut || Ok(crate::platform::process_alive(pid)))?;
        return Ok(running_host_block_stale(&dir));
    }
    #[cfg(unix)]
    host_identity::before_start(&dir)?;
    let version = dsh_command().and_then(|mut command| command.arg("--version").output()).map_err(|_| {
        format!("Install the official harness and its reviewed security overrides: see docs/installation.md#agents (@deepseek-ai/dsh@{VERSION})")
    })?;
    let installed = String::from_utf8_lossy(&version.stdout).trim().to_owned();
    // The owner, 2026-11-04: any release starts; only a harness that reports
    // no version at all is refused. An unreviewed one is noted and marked.
    let Some(release) = version.status.success().then(|| harness_release(&installed)).flatten() else {
        return Err(harness_version_refusal(version.status.success().then_some(installed.as_str())));
    };
    let marker = dir.join("harness-unreviewed");
    if reviewed_harness_version(&release) {
        let _ = std::fs::remove_file(&marker);
    } else {
        eprintln!("{}", harness_unreviewed_note(&release));
        let _ = atomic(&marker, &format!("{release}\n"));
    }
    let script = dir.join("hgs-bridge.mjs");
    atomic(&dir.join("command-guard.mjs"), COMMAND_GUARD)?;
    atomic(&script, BRIDGE)?;
    let socket = endpoint(&dir);
    // The startup lock excludes competing HGS starters. Only unlink a stale
    // endpoint after a failed connection, never an active native host. A named
    // pipe ends with its server.
    #[cfg(unix)]
    {
        if socket.exists() {
            fs::remove_file(&socket).map_err(|e| e.to_string())?;
        }
    }
    let overlay = dir.join("hgs.patch.json");
    // The bridge reads each hosted session's own markers from here, one
    // file per native conversation that `save` writes (zerus-ppd26).
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut config = bridge_config(&socket, agent_hint::for_account("native-dsh"));
    #[cfg(unix)]
    {
        let identities = shell_identity::folder(record);
        private_dir(&identities)?;
        config["shellIdentities"] = json!(identities);
    }
    atomic(
        &overlay,
        &host_overlay(&script, config, session_upload::block()).to_string(),
    )?;
    // N17: the block state this host starts with is recorded beside it, so a
    // later launch into a host started before the block can say so instead of
    // promising a block the running host never got.
    atomic(
        &dir.join("upload-block.json"),
        &json!({"block":session_upload::block(),"at":now()}).to_string(),
    )?;
    let log = crate::platform::private_open(OpenOptions::new().create(true).truncate(true).write(true))
        .open(dir.join("host.log"))
        .map_err(|e| e.to_string())?;
    let mut command = dsh_command().map_err(|_| {
        format!("Install the official harness: npm install -g @deepseek-ai/dsh@{VERSION}")
    })?;
    #[cfg(target_os = "linux")]
    {
        let runtime = PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
        if runtime.join("systemd/private").exists() && crate::platform::available("systemd-run") {
            // setsid detaches the terminal, but systemd still kills every
            // descendant in the GUI's cgroup on restart. A sibling scope
            // preserves the caller's environment and private log descriptors,
            // while giving the native host an independent lifetime.
            command = Command::new("systemd-run");
            command
                .args([
                    "--user",
                    "--scope",
                    "--quiet",
                    "--collect",
                    "--expand-environment=no",
                    "--unit",
                ])
                .arg(format!("hgs-dsh-{}", uuid::Uuid::new_v4()))
                .args(["--", "dsh"])
                .env("XDG_RUNTIME_DIR", runtime);
        }
    }
    command.args(host_arguments(&overlay, !Path::new(string(record, "agent_home")).join("profiles/hgs/package.json").exists()))
        .env("DSH_HOME", string(record, "agent_home"))
        .current_dir(home());
    // The variables a native host never takes from the process that starts
    // it: those of the session that process runs in (`host::SESSION_CONTEXT`,
    // which the Windows session host also drops) and its terminal's name. One
    // host serves every DeepSeek session of its agent home, so whichever
    // session happened to start it is not the session of any hosted agent.
    for name in crate::state::backend::host::SESSION_CONTEXT
        .iter()
        .chain(&["HGS_TERMINAL_NAME"])
    {
        command.env_remove(name);
    }
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    // Detach only the dedicated native host. Closing its launching terminal or
    // HGS process must not stop conversations.
    #[cfg(unix)]
    let mut child = host_identity::spawn(&dir, &mut command)?;
    #[cfg(not(unix))]
    let mut child = {
        let child = crate::platform::spawn_detached(&mut command).map_err(|e| e.to_string())?;
        atomic(&dir.join("host.pid"), &child.id().to_string())?;
        child
    };
    let deadline = Instant::now() + Duration::from_secs(HOST_START_SECONDS);
    wait_for_host(record, deadline, &mut || Ok(child.try_wait().map_err(|e| e.to_string())?.is_none()))?;
    Ok(false)
}

/// N17: whether a host that was already running was started without the block
/// asked for now. The state is written when a host starts; a host from before
/// the setting, or one started while it was off, is not protecting anything.
fn running_host_block_stale(dir: &Path) -> bool {
    if !session_upload::block() {
        return false;
    }
    let started = fs::read(dir.join("upload-block.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| value["block"].as_bool());
    started != Some(true)
}

/// N17/U22: what a binding says about DeepSeek's session-log upload: the
/// verification mark when its own records still accepted one after the launch
/// (U22), and the "may be active" mark when the running host was started
/// without the block. `upload_note` is the sentence the session card, `hgs ls`
/// and the team status show.
pub(crate) fn upload_state(record: &Value) -> Value {
    let mut value = json!({});
    if let Some(seen) = record.get("upload_seen").filter(|seen| seen.is_object()) {
        value["upload_seen"] = seen.clone();
        value["upload_note"] = json!("DeepSeek session-log upload still active");
    } else if session_upload::block() {
        if let Some(maybe) = record.get("upload_may_be_active").filter(|maybe| maybe.is_object()) {
            value["upload_may_be_active"] = maybe.clone();
            value["upload_note"] =
                json!("DeepSeek session-log upload may still be active: the running DeepSeek host was started before the block");
        }
    }
    value
}

/// N17/U22: the upload marks for one bound session, verifying its own records
/// first when it is not marked yet (bounded and throttled).
pub(crate) fn upload_state_for(name: &str) -> Value {
    if !session_upload::block() {
        return json!({});
    }
    let Ok(mut record) = binding(name) else { return json!({}) };
    if record.get("upload_seen").is_none() {
        if let Some(seen) = check_upload(name) {
            record["upload_seen"] = seen;
        }
    }
    upload_state(&record)
}

pub(crate) fn native_ui_url(name: &str) -> Result<String> {    let record = if name == "@account" {
        json!({"agent_home":native_home()})
    } else {
        binding(name)?
    };
    // N17: opening the native UI never marks a session; only a launch does.
    let _block_stale = ensure_host(&record)?;
    let pattern =
        regex::Regex::new(r"http://127\.0\.0\.1:[0-9]+/\?token=[A-Za-z0-9_%.-]+").unwrap();
    // A new host's adapter can answer before its web server logs the address.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let url = loop {
        let log = fs::read_to_string(host_dir(&record).join("host.log")).map_err(|e| e.to_string())?;
        if let Some(found) = pattern.find(&log) {
            break found.as_str().to_owned();
        }
        if std::time::Instant::now() >= deadline {
            return Err("Native UI is still starting; try again in a moment".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    if name != "@account" {
        match rpc(
            &record,
            "workspace",
            json!({"sessionId":record["conversation_id"]}),
        ) {
            Ok(_) => {}
            Err(error) if error == "Unsupported HGS native request" => {
                // Upgrade the UI of an active legacy host without restarting
                // its agents. Tokens never appear in process arguments.
                let mut child = crate::platform::spawn(
                    Command::new("node")
                        .args([
                            "--input-type=module",
                            "-e",
                            include_str!("../dsh/ui-compat.mjs"),
                        ])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .stderr(Stdio::piped()),
                )
                .map_err(|e| e.to_string())?;
                child.stdin.take().ok_or("Could not prepare native UI")?
                    .write_all(json!({"url":url,"sessionId":record["conversation_id"],"cwd":record["cwd"]}).to_string().as_bytes())
                    .map_err(|e| e.to_string())?;
                if !child
                    .wait_with_output()
                    .map_err(|e| e.to_string())?
                    .status
                    .success()
                {
                    return Err(
                        "Could not register the session in the native DeepSeek workspace".into(),
                    );
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(url)
}

#[derive(Clone, Debug, PartialEq)]
enum PermissionSelection {
    Provider,
    Bypass,
    Preset(String),
}

impl PermissionSelection {
    fn legacy_mode(&self) -> &'static str {
        match self {
            Self::Bypass => "bypass",
            _ => "provider",
        }
    }

    fn preset(&self) -> Option<&str> {
        match self {
            Self::Preset(name) => Some(name),
            _ => None,
        }
    }

    /// Stored record form: the common native selection map (canonical compact
    /// JSON) or one of the preserved shortcuts.
    fn canonical(&self) -> String {
        match self {
            Self::Provider => "provider".into(),
            Self::Bypass => "bypass".into(),
            Self::Preset(name) => format!(
                "native:{{\"preset\":{}}}",
                serde_json::to_string(name).unwrap_or_else(|_| "\"\"".into())
            ),
        }
    }
}

fn decode_permission_selection(raw: &str) -> Result<PermissionSelection> {
    let value = raw;
    match value {
        "" | "provider" => return Ok(PermissionSelection::Provider),
        "bypass" => return Ok(PermissionSelection::Bypass),
        "auto" => {
            return Err("Auto permission mode is not available for DeepSeek sessions; choose Default, Bypass or a native preset".into())
        }
        _ => {}
    }
    if let Some(rest) = value.strip_prefix("native:") {
        if rest.len() > 1024 {
            return Err("The native permission selection is too large".into());
        }
        let map: Value = serde_json::from_str(rest)
            .map_err(|_| "The native permission selection is not valid JSON".to_owned())?;
        let object = map
            .as_object()
            .ok_or("The native permission selection must be a JSON object")?;
        let mut preset = None;
        for (control, selected) in object {
            if !valid_control_id(control) {
                return Err("The native permission selection has an invalid control ID".into());
            }
            if control != "preset" {
                return Err(format!(
                    "The native permission selection uses an unsupported control \"{control}\""
                ));
            }
            let name = selected
                .as_str()
                .ok_or("The native permission preset must be a string")?;
            if name.len() > 128 || name.trim() != name || name.chars().any(char::is_control) {
                return Err("The native permission preset is invalid".into());
            }
            preset = Some(name.to_owned());
        }
        return Ok(match preset.filter(|name| !name.is_empty()) {
            Some(name) => PermissionSelection::Preset(name),
            None => PermissionSelection::Provider,
        });
    }
    // A caller may pass a decoded control value; a bare key is accepted only
    // when it still looks like a preset, and the fresh catalog confirms it.
    if valid_preset_name(value) {
        return Ok(PermissionSelection::Preset(value.to_owned()));
    }
    Err("Choose Default, Bypass or a native DeepSeek permission preset".into())
}

fn effective_selection(record: &Value, config: &Config) -> Result<PermissionSelection> {
    if let Some(override_value) = record["permission_override"]
        .as_str()
        .filter(|value| !value.is_empty())
    {
        return decode_permission_selection(override_value);
    }
    let mode = crate::accounts::permission_mode(config, "native-dsh", "dsh").map_err(|e| e.message)?;
    decode_permission_selection(&mode)
}

fn bridge_config(socket: &Path, hint: Option<&str>) -> Value {
    let mut config = json!({"socket": socket, "recoveryPolicy": recovery::policy_path()});
    if let Some(hint) = hint {
        config["agentHint"] = json!(hint);
    }
    config
}

struct PermissionPlan {
    mode: String,
    preset: Option<String>,
    native_preset: bool,
    legacy_url: Option<String>,
    needs_setup: bool,
    generation: Value,
}

// Extend legacy residents through their authenticated native API, without
// reloading their bridge (which would dispose its owned live agents).
fn permission_plan(record: &Value) -> Result<PermissionPlan> {
    let config = crate::config::Config::load().map_err(|e| e.message)?;
    let selection = effective_selection(record, &config)?;
    let ping = rpc(record, "ping", json!({}))?;
    let mut plan = PermissionPlan {
        mode: selection.legacy_mode().to_owned(),
        preset: selection.preset().map(str::to_owned),
        native_preset: supports(&ping, "permission-preset"),
        legacy_url: None,
        needs_setup: false,
        generation: ping["generation"].clone(),
    };
    match &selection {
        PermissionSelection::Provider => {}
        PermissionSelection::Bypass => {
            if ping["accountPermissions"] != true || record["permissions_pending"] == true {
                let url = account_ui_url(Path::new(string(record, "agent_home")))?;
                let checked = permissions_compat_json(
                    &url,
                    Some(string(record, "conversation_id")),
                    "check",
                    None,
                    record["permissions_pending"] == true,
                    false,
                )
                .map_err(|reason| permission_apply_error(&reason, false))?;
                plan.needs_setup = checked["needs_setup"]
                    .as_bool()
                    .ok_or("Invalid native permissions response")?;
                plan.legacy_url = Some(url);
            }
        }
        PermissionSelection::Preset(name) => {
            // Fresh actual catalog: the running host first, then its composed
            // profile. A withdrawn preset fails explicitly; never a fallback.
            let presets = fresh_presets(record).map_err(|_| {
                format!("Could not verify the permission preset \"{name}\" against the running DeepSeek host; choose Default or Bypass")
            })?;
            if !presets.iter().any(|preset| &preset.value == name) {
                return Err(format!(
                    "The permission preset \"{name}\" is no longer offered by the DeepSeek host; choose another preset before launching"
                ));
            }
            plan.needs_setup = true;
            if !plan.native_preset {
                plan.legacy_url = Some(account_ui_url(Path::new(string(record, "agent_home")))?);
            }
        }
    }
    Ok(plan)
}

/// Returns the host generation the session was created or resumed under and
/// the account permission mode it used.
fn create_session(record: &mut Value, explicit: bool) -> Result<(Value, String)> {
    // Deterministic capability/account checks precede persistence; identities
    // still precede native creation so an uncertain reply cannot duplicate it.
    let plan = permission_plan(record)?;
    if rpc(record, "ping", json!({}))?["generation"] != plan.generation {
        return Err("DeepSeek host changed before session creation; retry opening the session".into());
    }
    let was_pending = record["permissions_pending"] == true;
    if plan.needs_setup {
        record["permissions_pending"] = json!(true);
    }
    save(record)?;
    let mut params = json!({"sessionId":record["conversation_id"],"cwd":record["cwd"],
        "title":string(record,"name").rsplit('/').next(),"permissionMode":plan.mode});
    if plan.native_preset {
        if let Some(preset) = &plan.preset {
            params["permissionPreset"] = json!(preset);
            // Only an explicit pending-recovery launch may touch a live agent.
            if was_pending {
                params["permissionRecovery"] = json!(true);
            }
            if explicit {
                params["permissionExplicit"] = json!(true);
            }
        }
    }
    let answer = rpc(record, "create", params)?;
    if plan.native_preset && plan.preset.is_some() && answer["presetApplied"] != true {
        if was_pending {
            return Err("DeepSeek is already running but did not confirm the permission preset; resume to finish setup".into());
        }
        // The agent was already live and this was an ordinary ensure, not a
        // pending initial-setup recovery. Clear the mark this create added;
        // only an explicit request is an error, an implicit account default
        // leaves the live native choice untouched.
        if let Some(object) = record.as_object_mut() {
            object.remove("permissions_pending");
        }
        save(record)?;
        if explicit {
            return Err("This DeepSeek session is already running; its permission preset applies at its next start or explicit resume. Stop it first.".into());
        }
    }
    if !plan.native_preset {
        if let Some(preset) = plan.preset.as_deref() {
            let url = plan.legacy_url.as_deref().ok_or(
                "Could not verify native permissions on the running DeepSeek host; retry opening the session",
            )?;
            if rpc(record, "ping", json!({}))?["generation"] != plan.generation {
                return Err("DeepSeek host changed before permission setup; resume the session".into());
            }
            if let Err(reason) = permissions_compat_json(
                url,
                Some(string(record, "conversation_id")),
                "apply-preset",
                Some(preset),
                was_pending,
                explicit,
            ) {
                if reason == "busy" && !was_pending {
                    if let Some(object) = record.as_object_mut() {
                        object.remove("permissions_pending");
                    }
                    save(record)?;
                    if explicit {
                        return Err("This DeepSeek session is already running; its permission preset applies at its next start or explicit resume. Stop it first.".into());
                    }
                } else {
                    return Err(permission_apply_error(&reason, true));
                }
            }
            if rpc(record, "ping", json!({}))?["generation"] != plan.generation {
                return Err("DeepSeek host changed during permission setup; resume the session".into());
            }
        } else if let Some(url) = plan
            .legacy_url
            .as_deref()
            .filter(|_| plan.needs_setup && plan.mode == "bypass")
        {
            if rpc(record, "ping", json!({}))?["generation"] != plan.generation {
                return Err("DeepSeek host changed before permission setup; resume the session".into());
            }
            permissions_compat_json(
                url,
                Some(string(record, "conversation_id")),
                "apply",
                None,
                true,
                explicit,
            )
            .map_err(|reason| permission_apply_error(&reason, false))?;
            if rpc(record, "ping", json!({}))?["generation"] != plan.generation {
                return Err("DeepSeek host changed during permission setup; resume the session".into());
            }
        }
    }
    record
        .as_object_mut()
        .ok_or("Invalid DeepSeek binding")?
        .remove("permissions_pending");
    save(record)?;
    Ok((plan.generation, plan.mode))
}

/// Whether an RPC error leaves the request possibly executed by the host.
fn delivery_uncertain(error: &str) -> bool {
    error.contains("delivery uncertain")
}

/// Account launch defaults (zerus-v7tqa) reach only the session this launch
/// just created, through the host's existing scoped `settings` method and
/// before any prompt, so a resident older bridge needs no reload. The binding
/// records `pending` first, then `applied`, `failed` (never executed) or
/// `uncertain` (may have executed). The launch still succeeds and nothing is
/// retried; the returned text is the warning to report.
fn apply_account_defaults_with(
    record: &mut Value,
    (generation, mode): (&Value, &str),
    model: &str,
    effort: &str,
    mut call: impl FnMut(&str, Value) -> Result<Value>,
    mut persist: impl FnMut(&mut Value) -> Result<()>,
) -> Option<String> {
    let mut outcome = json!({"model":model,"effort":effort,"status":"pending","at":now()});
    record["account_defaults"] = outcome.clone();
    if let Err(error) = persist(record) {
        record["account_defaults"]["status"] = json!("failed");
        record["account_defaults"]["detail"] = json!(error);
        return Some(format!("DeepSeek session started with its native model; account defaults were not applied: {error}"));
    }
    let attempt = (|| -> std::result::Result<Value, (bool, String)> {
        let (provider, id) = model
            .split_once('/')
            .ok_or((false, "Invalid account model default".to_owned()))?;
        let ping = call("ping", json!({})).map_err(|e| (false, e))?;
        if ping["generation"] != *generation {
            return Err((false, "DeepSeek host changed after creating the session".into()));
        }
        // The mode matters only if the session was paused natively meanwhile:
        // the method then resumes it, as a scoped resume would.
        let mut params = json!({"sessionId":record["conversation_id"],"provider":provider,
            "model":id,"generation":generation,"permissionMode":mode});
        if !effort.is_empty() {
            params["reasoningEffort"] = json!(effort);
        }
        let applied = call("settings", params).map_err(|e| (delivery_uncertain(&e), e))?;
        if applied["scope"] != "session" {
            return Err((true, "DeepSeek did not confirm the session selection".into()));
        }
        Ok(applied["selected"].clone())
    })();
    let warning = match attempt {
        Ok(selected) => {
            outcome["status"] = json!("applied");
            outcome["selected"] = selected;
            None
        }
        Err((uncertain, detail)) => {
            outcome["status"] = json!(if uncertain { "uncertain" } else { "failed" });
            outcome["detail"] = json!(detail);
            Some(if uncertain {
                format!("DeepSeek may have applied the account defaults; check this session's model and effort: {detail}")
            } else {
                format!("DeepSeek session started with its native model; account defaults were not applied: {detail}")
            })
        }
    };
    record["account_defaults"] = outcome;
    match persist(record) {
        Ok(()) => warning,
        Err(error) => Some(warning.map_or_else(
            || format!("DeepSeek applied the account defaults, but recording that failed: {error}"),
            |w| format!("{w}; recording the outcome also failed: {error}"),
        )),
    }
}

/// U34: a DeepSeek session's sandbox needs full control (WRITE_DAC and
/// WRITE_OWNER) of the folder it confines. On Windows a folder Zerus did not
/// create may lack it — a worktree on a second drive inherits only
/// Authenticated Users Modify — and every command then fails before it runs, so
/// the session would not start. The owner, 2026-11-01: hgs gives this user
/// the rights itself (what Fix folder rights did on a click): only this user's
/// own entry on the session's own folder changes. Only when that grant fails
/// (no WRITE_DAC to change it with) does the owner get the one-line fix. Other
/// systems compile the check out.
#[cfg(windows)]
fn ensure_sandbox_folder(folder: &Path) -> Result<()> {
    if crate::platform::sandbox_folder_problem(folder).map_err(|e| e.to_string())?.is_none() {
        return Ok(());
    }
    let granted = crate::platform::grant_full_control(folder);
    if let Some(problem) = crate::platform::sandbox_folder_problem(folder).map_err(|e| e.to_string())? {
        // The check runs before any binding or host exists: the desktop
        // shows a definite refusal, not an unknown creation result.
        return Err(match granted {
            Err(error) => format!("{problem}\n(hgs could not grant it itself: {error})\n{NOTHING_STARTED}"),
            Ok(()) => format!("{problem}\n{NOTHING_STARTED}"),
        });
    }
    Ok(())
}
#[cfg(not(windows))]
fn ensure_sandbox_folder(_: &Path) -> Result<()> {
    Ok(())
}

/// A per-launch permission selection. `permissions` accepts the preserved
/// shortcuts (`provider`, `bypass`), a legacy `auto` (rejected for DeepSeek) or
/// a `native:` control map. `None` keeps a binding's own override and otherwise
/// falls back to the account default.
pub(crate) fn launch_with_model(
    name: &str,
    cwd: &str,
    launch_id: Option<&str>,
    model: &str,
    effort: &str,
    permissions: Option<&str>,
    dry: bool,
    new_only: bool,
) -> Result<()> {
    let selection = permissions.map(decode_permission_selection).transpose()?;
    let override_value = selection.as_ref().map(PermissionSelection::canonical);
    if dry {
        println!("Create official dsh session {name} in {cwd}");
        return Ok(());
    }
    // Verify a native preset against the installed harness before creating a
    // binding or starting a host. If discovery is unavailable here, the fresh
    // running-host check in create_session still decides; Provider and Bypass
    // never probe, so their fallback keeps working without discovery.
    if let Some(PermissionSelection::Preset(preset)) = &selection {
        if let Ok(config) = Config::load() {
            if let Ok((presets, _)) = composed_presets(&config) {
                if !presets.iter().any(|option| &option.value == preset) {
                    return Err(format!(
                        "The permission preset \"{preset}\" is not offered by the installed DeepSeek Harness; choose another preset"
                    ));
                }
            }
        }
    }
    let parent = parent_link::launcher_link()?;
    let _guard = lock(Some(&root().join("dsh/bindings.lock")))?;
    if let Some(mut record) = PROVIDER.existing_for_launch(name)? {
        if parent_link::claimed_launch() { return Err("--parent applies only to new sessions; use hgs parent adopt for an existing session".into()); }
        if new_only {
            return Err("This DeepSeek session name already exists".into());
        }
        if let Some(value) = &override_value {
            record["permission_override"] = json!(value);
        }
        let block_stale = ensure_host(&record)?;
        mark_upload_may_be_active(&mut record, block_stale);
        create_session(&mut record, permissions.is_some())?;
        record["paused"] = json!(false);
        record["run_id"] = json!(uuid::Uuid::new_v4().to_string());
        // U22/N8: the verification reads records written after this launch,
        // starting at the store's length at it.
        mark_upload_start(&mut record);
        save(&mut record)?;
        return Ok(());
    }
    let cwd = crate::platform::canonical(cwd).map_err(|e| e.to_string())?;
    // RogueDOD (owner, 2026-11-12): danger-full-access runs without the
    // sandbox, so the folder (often a shared one) keeps its rights as they are.
    if !matches!(&selection, Some(PermissionSelection::Preset(preset)) if preset == "danger-full-access") {
        ensure_sandbox_folder(&cwd)?;
    }
    // Only a session created here takes the account defaults; relaunching an
    // existing name above resumes it with its own stored selection.
    let config = crate::config::Config::load().map_err(|e| e.message)?;
    let defaults = crate::accounts::launch_selection(&config, "dsh").map_err(|e| e.message)?;
    let mut record = json!({"backend":"dsh","name":name,"agent":"dsh","agent_home":native_home(),"cwd":cwd,
        "conversation_id":format!("session-{}",uuid::Uuid::new_v4()),"run_id":uuid::Uuid::new_v4().to_string(),
        "created":now(),"launch_id":launch_id,"paused":false});
    if let Some(value) = &override_value {
        record["permission_override"] = json!(value);
    }
    if let Some(parent) = parent { record["parent"] = parent; }
    let block_stale = ensure_host(&record)?;
    mark_upload_may_be_active(&mut record, block_stale);
    let (generation, mode) = create_session(&mut record, permissions.is_some())?;
    // U22/N8: a new session's store appears with this launch; remember where it
    // is and count only what comes after it.
    mark_upload_start(&mut record);
    save(&mut record)?;
    // PR #33: a model named at launch wins over the account's launch defaults.
    if let Some((model, effort)) = defaults.filter(|_| model.is_empty()) {
        let host = record.clone();
        if let Some(warning) = apply_account_defaults_with(
            &mut record,
            (&generation, &mode),
            &model,
            &effort,
            |method, params| rpc(&host, method, params),
            save,
        ) {
            eprintln!("hgs: {warning}");
        }
    }
    // A child its proven parent launched starts in the parent's project,
    // written after the bindings lock is released, like a fork (zerus-d5twt).
    if let Some(parent) = record
        .get("parent")
        .filter(|link| crate::swarm::placing_parent(link))
    {
        drop(_guard);
        if let Ok(config) = crate::config::Config::load() {
            crate::swarm::place_child_session(
                &config,
                name,
                &cwd,
                string(&record, "run_id"),
                &record["conversation_id"],
                parent,
            );
        }
    } else if let Ok(config) = crate::config::Config::load() {
        crate::swarm::infer_new_session(&config, name, &cwd);
    }
    if !model.is_empty() {
        apply_model(&record, model, effort, &mode).map_err(|error| format!("DeepSeek session was created, but its model settings could not be applied: {error}"))?;
    }
    Ok(())
}

pub(crate) fn launch_with_permissions(
    name: &str,
    cwd: &str,
    launch_id: Option<&str>,
    permissions: Option<&str>,
    dry: bool,
    new_only: bool,
) -> Result<()> {
    launch_with_model(name, cwd, launch_id, "", "", permissions, dry, new_only)
}

#[allow(dead_code)] // Kept for callers on the previous launch signature.
pub(crate) fn launch(
    name: &str,
    cwd: &str,
    launch_id: Option<&str>,
    dry: bool,
    new_only: bool,
) -> Result<()> {
    launch_with_permissions(name, cwd, launch_id, None, dry, new_only)
}

/// U22: how often an unmarked session's own records are read again.
const UPLOAD_CHECK_SECONDS: f64 = 60.0;

/// N17: a session launched or resumed into a host that was started without the
/// block is marked, so the desktop and status say the block may not hold for it
/// until the host restarts. A host started with the block clears the mark.
fn mark_upload_may_be_active(record: &mut Value, stale: bool) {
    let Some(object) = record.as_object_mut() else { return };
    if stale {
        object.insert(
            "upload_may_be_active".into(),
            json!({"at":now(),"reason":"the running DeepSeek host was started before the block"}),
        );
    } else {
        object.remove("upload_may_be_active");
    }
}

/// U22: verifies the block from the session's own records. A session started or
/// resumed with the block on that still writes a session-log-deepseek
/// acceptance record after that launch is marked on its binding. One bounded
/// pass per minute from the stored frame boundary (512 KiB, 200 frames), so an
/// acceptance written later is found however large the store grows; this runs
/// off the hook path, when the session list or a team status asks.
pub(crate) fn check_upload(name: &str) -> Option<Value> {
    if !super::session_upload::block() {
        return None;
    }
    let record = binding(name).ok()?;
    if let Some(seen) = record.get("upload_seen").filter(|seen| seen.is_object()) {
        return Some(seen.clone());
    }
    let at = now();
    if at - record["upload_checked_at"].as_f64().unwrap_or(0.0) < UPLOAD_CHECK_SECONDS {
        return None;
    }
    // N8: the store's path is remembered on the binding; one lookup is bounded
    // and only made until it is known. A session with no store yet still
    // records its check time, so a listing does not scan for it on every poll.
    let mut path = PathBuf::from(string(&record, "upload_path"));
    if !path.is_file() {
        match super::session_upload::file(string(&record, "agent_home"), string(&record, "conversation_id")) {
            Some(found) => path = found,
            None => {
                remember_upload(name, &record, at, None, None, false);
                return None;
            }
        }
    }
    let offset = record["upload_offset"].as_u64().unwrap_or(0);
    let since = record["upload_since"].as_f64().or_else(|| record["created"].as_f64()).unwrap_or(0.0);
    let Ok(tail) = super::session_upload::read_tail(&path, offset) else { return None };
    let seen = super::session_upload::uploaded_after(&tail.records, since)
        .then(|| json!({"at":at,"plugin":"session-log-deepseek","offset":tail.offset}));
    remember_upload(name, &record, at, Some((&path, tail.offset)), seen.as_ref(), tail.unverifiable);
    seen
}

/// N8: starts the verification at the store's length at launch and remembers
/// the store's path, so the first pass reads only what the launch appended and
/// a view never scans the agent home again.
fn mark_upload_start(record: &mut Value) {
    let path = super::session_upload::file(string(record, "agent_home"), string(record, "conversation_id"));
    let offset = path
        .as_ref()
        .and_then(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    record["upload_since"] = json!(now());
    record["upload_checked_at"] = json!(0);
    record["upload_offset"] = json!(offset);
    match path {
        Some(path) => record["upload_path"] = json!(path),
        None => {
            if let Some(object) = record.as_object_mut() {
                object.remove("upload_path");
            }
        }
    }
}

/// C2: writes only the upload fields into a binding re-read under the bindings
/// lock, so a poll overlapping a resume, rename or forget never reverts it. A
/// binding that is gone or changed since the check read it is skipped.
fn remember_upload(name: &str, read: &Value, at: f64, tail: Option<(&Path, u64)>, seen: Option<&Value>, unverifiable: bool) -> Option<()> {
    let _guard = lock(Some(&root().join("dsh/bindings.lock"))).ok()?;
    let mut current = binding(name).ok()?;
    if current["run_id"] != read["run_id"] || current["conversation_id"] != read["conversation_id"] {
        return None;
    }
    current["upload_checked_at"] = json!(at);
    if let Some((path, offset)) = tail {
        current["upload_path"] = json!(path);
        current["upload_offset"] = json!(offset);
    }
    if let Some(seen) = seen {
        current["upload_seen"] = seen.clone();
    }
    // L1: a store the reader cannot step through is marked instead of being
    // re-read forever; a store that walks again clears the mark.
    if unverifiable {
        current["upload_unverifiable"] = json!(true);
    } else if let Some(object) = current.as_object_mut() {
        object.remove("upload_unverifiable");
    }
    save(&mut current).ok()?;
    Some(())
}

pub(crate) fn snapshots() -> Result<Vec<Value>> {
    // Task 234: where the DeepSeek rows of a listing spend their time.
    let mut phases = super::slow_calls::Phases::start();
    let mut out = Vec::new();
    let preferences = notifications::load();
    let mut records = binding_records();
    phases.mark("bindings");
    // U22: a session whose own records still accept the upload is marked, so
    // the session card and Accounts can say so. Bounded and throttled per
    // session; only when the block is on and the session is not marked yet.
    if super::session_upload::block() {
        for record in &mut records {
            if record.get("upload_seen").is_none() {
                if let Some(seen) = check_upload(string(record, "name")) {
                    record["upload_seen"] = seen;
                }
            }
        }
    }
    phases.mark("upload");
    // One request per host home, for exactly the conversations bound here.
    let mut hosts = BTreeMap::new();
    for record in &records {
        let key = string(record, "agent_home").to_owned();
        if !hosts.contains_key(&key) {
            // Task 234: a stopped conversation's row comes from the host's
            // last answer; only the rest are asked for (`listing_rows`).
            let home: Vec<&Value> = records.iter().filter(|other| string(other, "agent_home") == key).collect();
            let list = listing_rows(&home).unwrap_or(Value::Null);
            hosts.insert(key.clone(), list);
            phases.mark("host_rows");
        }
        let mut value = snapshot_from(record, &hosts[&key]);
        phases.mark("project");
        notifications::enrich(&mut value, preferences.as_ref(), journal::journal_name(record));
        phases.mark("notifications");
        out.push(value);
    }
    phases.note("records", records.len() as u64);
    phases.note("hosts", hosts.len() as u64);
    phases.finish_at("ls-dsh", "", 100);
    Ok(out)
}

/// The same listing projection, without opening every saved DeepSeek binding.
/// The host's rows (or, on an older bridge, list) RPC and its generation
/// semantics are unchanged.
pub(super) fn snapshot(name: &str) -> Result<Value> {
    let mut record = binding(name)?;
    if record.get("upload_seen").is_none() {
        if let Some(seen) = check_upload(name) {
            record["upload_seen"] = seen;
        }
    }
    snapshot_record(&record)
}
fn snapshot_record(record: &Value) -> Result<Value> {
    let list = session_rows(record, &[string(record, "conversation_id")]).unwrap_or(Value::Null);
    Ok(snapshot_from(record, &list))
}
/// Whether a claimed parent runs now, asking the host for at most `limit`.
/// No host is a definite no; a host that has not answered by then is busy,
/// never live.
#[cfg(unix)]
pub(super) fn claim_live(record: &Value, limit: Duration) -> Result<bool> {
    let started = std::time::Instant::now();
    let dir = host_dir(record);
    let socket = match claim_connect(&dir.join("host.sock"), limit) {
        Ok(socket) => socket,
        // BSD refuses a connect on a full listen queue at once (ECONNREFUSED,
        // sometimes ECONNRESET). The recorded owner decides: while it is alive
        // the host is saturated, not gone, so the claim is busy; a dead owner
        // or a missing socket stays a definite no. A refusal is never live
        // (zerus-xt6re).
        Err(error) if refused(&error) => {
            return if live_owner_socket(&dir) {
                Err(parent_link::CLAIM_BUSY.into())
            } else {
                Ok(false)
            };
        }
        Err(_) if started.elapsed() >= limit => return Err(parent_link::CLAIM_BUSY.into()),
        Err(_) => return Ok(false),
    };
    match exchange_socket(socket, "list", json!({}), limit) {
        Ok(list) => {
            let row = snapshot_from(record, &list);
            Ok(row["state"] == "running" && row["process_state"] == "running")
        }
        Err(_) if started.elapsed() >= limit => Err(parent_link::CLAIM_BUSY.into()),
        Err(_) => Ok(false),
    }
}
#[cfg(not(unix))]
pub(super) fn claim_live(_: &Value, _: std::time::Duration) -> Result<bool> {
    Ok(false)
}

fn snapshot_from(record: &Value, list: &Value) -> Value {
    snapshot_with(record, list, &mut |method, params| rpc(record, method, params))
}
/// `snapshot_from`, reading the host through `rpc`.
fn snapshot_with(record: &Value, list: &Value, rpc: &mut dyn FnMut(&str, Value) -> Result<Value>) -> Value {
    let row = list["items"].as_array().and_then(|items| {
        items
            .iter()
            .find(|v| v["sessionId"] == record["conversation_id"])
    });
    let row = row.unwrap_or(&Value::Null);
    let (failure, outline) = idle_failure(record, row, &list["generation"], rpc);
    let mut outlined;
    let row = match outline {
        Some(outline) => {
            outlined = row.clone();
            outlined["projections"]["values"]["turnOutline"] = outline;
            &outlined
        }
        None => row,
    };
    let mut value = normalize(record, row, &json!({"provider_error":failure}));
    child_summaries(record, &mut value, list, &list["generation"]);
    // The host lists a tool approval as a question; it is the person's.
    let approval = row["pendingQuestions"].as_array().is_some_and(|questions| {
        questions
            .iter()
            .any(|question| question["approval"] == true)
    });
    parent_link::mark_human_request(&mut value, None, approval);
    if !list.is_null() {
        value["host_generation"] = list["generation"].clone();
    }
    // U22/N17: the session still uploads session logs, or the running host was
    // started before the block; the row says which with its time.
    if let Some(upload) = upload_state(record).as_object() {
        for (key, mark) in upload {
            value[key] = mark.clone();
        }
    }
    value
}

// A native turn can end unsuccessfully while the agent stays alive. Cache only
// that terminal result; polling must not reload conversation pages every time.
//
// The same idle inspection supplies the turn outline when the host's list
// omits it: from dsh 0.2.2 `turnOutline` is a `summary: false` projection, so
// a list row no longer carries the settled reply that `hgs read` and
// `--current-read` name (zerus-a4mub.24). It comes from the session's full
// projections in that one cached read, never from a separate request. A running
// or stopped row keeps the outline last read for its run; a row whose list
// still carries the outline is unchanged.
fn idle_failure(
    record: &Value,
    row: &Value,
    generation: &Value,
    rpc: &mut dyn FnMut(&str, Value) -> Result<Value>,
) -> (Value, Option<Value>) {
    let summarized = !row["projections"]["values"].is_object()
        || row["projections"]["values"].get("turnOutline").is_some();
    let idle = row["agentAvailable"] == true && row["running"] != true;
    if summarized && !idle {
        return (Value::Null, None);
    }
    let key = format!(
        "{}:{}",
        string(record, "agent_home"),
        string(record, "conversation_id")
    );
    let path = root()
        .join("dsh/errors")
        .join(format!("{:x}.json", Sha256::digest(key.as_bytes())));
    let stamp = json!([record["run_id"], row["updatedAt"], generation]);
    let cached = fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or(Value::Null);
    let cached_outline = || {
        (!summarized && cached["stamp"][0] == record["run_id"] && cached["outline"].is_array())
            .then(|| cached["outline"].clone())
    };
    if !idle {
        return (Value::Null, cached_outline());
    }
    if !row["updatedAt"].is_null()
        && cached["stamp"] == stamp
        && (summarized || cached.get("outline").is_some())
    {
        return (cached["error"].clone(), cached_outline());
    }
    let Ok(inspected) = rpc("inspect", json!({"sessionId":record["conversation_id"]})) else {
        return (Value::Null, None);
    };
    let error = latest_failure(&inspected["page"]["records"]);
    let outline = inspected["baseline"]["values"]["turnOutline"].clone();
    if private_dir(path.parent().unwrap()).is_ok() {
        let _ = atomic(
            &path,
            &json!({"stamp":stamp,"error":error,"outline":outline}).to_string(),
        );
    }
    (
        error,
        (!summarized && outline.is_array()).then_some(outline),
    )
}
fn turn_failure(event: &Value) -> Value {
    let reason = &event["data"]["reason"];
    let detail = match string(reason, "kind") {
        "error" => {
            let message = string(&reason["error"], "message");
            if message.is_empty() {
                "Provider request failed".to_owned()
            } else {
                message.to_owned()
            }
        }
        "max-tokens" => "Output token limit reached".to_owned(),
        _ => return Value::Null,
    };
    let mut failure = provider_errors::event(
        &format!("dsh-error-{}", event["seq"]),
        event["time"].as_f64().unwrap_or(0.0) / 1000.0,
        &detail,
        "dsh_native",
    );
    let category = provider_errors::category(&format!(
        "{} {}",
        string(reason, "kind"),
        string(&reason["error"], "code")
    ));
    if category != "provider" {
        failure["error_kind"] = json!(category);
    }
    failure
}
fn latest_failure(records: &Value) -> Value {
    for record in records.as_array().into_iter().flatten().rev() {
        let event = &record["event"];
        match string(event, "type") {
            "turn/end" => return turn_failure(event),
            "turn/start" | "user/message" => return Value::Null,
            _ => {}
        }
    }
    Value::Null
}

fn goal(value: &Value) -> Value {
    if !value.is_object() || string(value, "id").is_empty() {
        return Value::Null;
    }
    json!({"id":value["id"],"objective":value["objective"],"status":value["phase"],"revision":value["revision"],
        "rounds_used":value["roundsStarted"],"round_budget":value["maxGoalRounds"],"activation":value["activation"],
        "reason":value["blockedReason"]["message"],"source":"dsh_goal_v1"})
}

fn child_summaries(record: &Value, value: &mut Value, listed: &Value, generation: &Value) {
    if !generation.is_string() || listed["generation"] != *generation { return; }
    let Some(children) = value["subagents"].as_object_mut() else { return; };
    let mut active = 0;
    let mut completed = 0;
    let mut unknown = 0;
    for (id, child) in children.iter_mut() {
        let candidates: Vec<_> = listed["items"].as_array().into_iter().flatten()
            .filter(|row| row["sessionId"] == id.as_str() && row["parentSessionId"] == record["conversation_id"]
                && row["origin"] == "subagent").collect();
        let [row] = candidates.as_slice() else { unknown += 1; continue; };
        let (Some(running),Some(available)) = (row["running"].as_bool(),row["agentAvailable"].as_bool()) else { unknown += 1; continue; };
        if running && !available { unknown += 1; continue; }
        child["state"] = json!(if running { "working" } else { "finished" });
        child["updated"] = json!(row["updatedAt"].as_f64().unwrap_or(0.0) / 1000.0);
        child["tracking_evidence"] = json!({"source":"dsh_native_api","kind":"live_state","generation":generation,"native_id":id});
        if running { active += 1; } else { completed += 1; }
    }
    value["subagent_active_count"] = json!(active);
    value["subagent_completed_count"] = json!(completed);
    value["subagent_unknown_count"] = json!(unknown);
    value["subagent_count"] = json!(active);
    value["subagent_counts_complete"] = json!(unknown == 0);
}
fn normalize(record: &Value, row: &Value, inspected: &Value) -> Value {
    let parts = string(record, "name").splitn(3, '/').collect::<Vec<_>>();
    let projections = if inspected["baseline"].is_object() {
        &inspected["baseline"]["values"]
    } else {
        &row["projections"]["values"]
    };
    let alive = inspected["live"]
        .as_bool()
        .unwrap_or(row["agentAvailable"] == true);
    let working = row["running"] == true;
    let selection = if inspected["selection"].is_object() {
        &inspected["selection"]
    } else if row["selection"].is_object() {
        &row["selection"]
    } else if projections["modelSelection"]["next"].is_object() {
        &projections["modelSelection"]["next"]
    } else {
        &projections["modelSelection"]["lastUsed"]
    };
    let last_used = &projections["modelSelection"]["lastUsed"];
    let effort = selection
        .get("reasoningEffort")
        .filter(|v| v.is_string())
        .or_else(|| {
            (last_used["provider"] == selection["provider"]
                && last_used["model"] == selection["model"])
                .then(|| last_used.get("reasoningEffort"))
                .flatten()
        })
        .cloned()
        .unwrap_or(Value::Null);
    let pending = inspected["pendingQuestions"]
        .as_array()
        .or(row["pendingQuestions"].as_array())
        .is_some_and(|v| !v.is_empty());
    let auth_required = alive
        && inspected
            .get("authRequired")
            .unwrap_or(&row["authRequired"])
            == true;
    let phase = if pending || auth_required {
        "input"
    } else if working {
        "working"
    } else {
        "idle"
    };
    let native_goal = if inspected.get("goal").is_some() {
        &inspected["goal"]
    } else if row.get("goal").is_some() {
        &row["goal"]
    } else {
        &projections["goal"]
    };
    let outline = projections["turnOutline"].as_array().and_then(|v| v.last());
    let reply = projections["turnOutline"].as_array().and_then(|rows| {
        rows.iter()
            .rev()
            .find(|v| !string(v, "response").is_empty())
    });
    let mut children = serde_json::Map::new();
    for child in projections["subagentCatalog"]
        .as_array()
        .into_iter()
        .flatten()
    {
        children.insert(string(child,"id").to_owned(),json!({"name":child.get("label").unwrap_or(&child["id"]),"state":"unknown","mode":child["mode"]}));
    }
    let mut value = json!({"name":record["name"],"cmd":"dsh","agent":"dsh","backend":"dsh","project":parts.get(1),"tag":parts.get(2),
        "conversation_id":record["conversation_id"],"run_id":record["run_id"],"launch_id":record["launch_id"],"created":record["created"],
        "tracked":true,"resumable":true,"cwd":record["cwd"],"cwd_source":"native_session","attached":0,"clients":[],
        "state":if alive{"running"}else if record["paused"]==true{"paused"}else{"stopped"},
        "process_state":if alive{"running"}else{"exited"},"conversation_state":"active","runtime_state":if alive{"live"}else{"stopped"},
        "phase":phase,"activity":if pending || auth_required{"attention"}else if working{"busy"}else if alive{"idle"}else{"unknown"},
        "activity_summary":if auth_required{"Sign in required"}else if pending{"Needs attention"}else if working{"Working"}else if alive{"Ready"}else{"Stopped"},
        "model":selection["model"],"effort":effort,"model_provider":selection["provider"],
        "goal":goal(native_goal),"goal_source":"dsh_goal_v1","last_event_at":row["updatedAt"].as_f64().unwrap_or(0.0)/1000.0,
        "subagent_source":"dsh_native","subagent_counts_complete":false,"subagent_total_count":children.len(),"subagents":children,
        "prompt":outline.map(|v|v["prompt"].clone()),"activity_detail":outline.map(|v|v["response"].clone()),
        "reply_id":reply.map(|v|format!("dsh-turn-{}",v["seq"].as_u64().unwrap_or(0))),"reply_at":if reply.is_some(){row["updatedAt"].as_f64().unwrap_or(0.0)/1000.0}else{0.0},
        "native_ui_available":true,"settings_change_supported":alive,"send_available":alive && !auth_required && record["permissions_pending"] != true});
    if !string(record, "session_id").is_empty() {
        value["session_id"] = record["session_id"].clone();
    }
    if let Some(parent) = parent_link::listed(record) { value["parent"] = parent; }
    if pending {
        // Native question IDs distinguish one request from the next for read
        // marks and notifications; their generation-bound hashes do not.
        let ids: Vec<&str> = inspected["pendingQuestions"]
            .as_array()
            .or(row["pendingQuestions"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|q| q["id"].as_str())
            .collect();
        value["attention_id"] = json!(format!("dsh_question:{}", ids.join("\n")));
    }
    if record["permissions_pending"] == true {
        value["phase"] = json!("error");
        value["activity"] = json!("attention");
        value["activity_summary"] = json!("Resume to finish permission setup");
        value["settings_change_supported"] = json!(false);
    }
    value["account_id"] = json!("native-dsh");
    value["account_home"] = record["agent_home"].clone();
    // Always present, null for a non-fork: as fork::summary, so a client tells
    // a session that is not a fork from an older hgs without these fields.
    for key in ["fork_parent_id", "fork_source_name"] {
        value[key] = if record[key].is_string() { record[key].clone() } else { Value::Null };
    }
    // Present only while the official Schedule bundle is loaded.
    if row["schedules"].is_object() {
        value["schedules"] = json!({"count":row["schedules"]["count"],"recurring":row["schedules"]["recurring"],
            "next_at":row["schedules"]["nextAt"]});
    }
    value["auth_required"] = json!(auth_required);
    value["account_status"] = inspected
        .get("accountStatus")
        .unwrap_or(&row["accountStatus"])
        .clone();
    if alive && !working && !pending && !auth_required && record["permissions_pending"] != true {
        let failure = inspected
            .get("provider_error")
            .cloned()
            .unwrap_or_else(|| latest_failure(&inspected["page"]["records"]));
        if failure.is_object() {
            provider_errors::apply(&mut value, &failure);
        }
    }
    if let Some(todos) = projections["todos"].as_array() {
        value["task_lists"] = json!({"main":{"source":"dsh_native","run_id":record["run_id"],"items":todos.iter().enumerate().map(|(index,t)|json!({"id":index.to_string(),"title":t["content"],"status":t["status"]})).collect::<Vec<_>>()}});
    }
    let pending_recovery = inspected.get("nativeRecovery").unwrap_or(&row["nativeRecovery"]);
    if alive && !pending && !auth_required && pending_recovery.is_object() && record["permissions_pending"] != true {
        let mut error = provider_errors::event(string(pending_recovery,"id"),
            pending_recovery["at"].as_f64().unwrap_or(0.0),string(pending_recovery,"detail"),"dsh_request_error");
        error["retry_not_before"] = pending_recovery["retryNotBefore"].clone();
        provider_errors::apply(&mut value,&error);
        value["native_recovery"] = pending_recovery.clone();
    }
    recovery::enrich(record,&mut value);
    // An inspection reads the host's processes. A listing row carries the
    // bridge's bounded `listJobs` summary instead (zerus-yj7ef). Only a
    // complete summary is a count; an incomplete one, or a row without one
    // from an older bridge, leaves the count unknown, never idle.
    let listed_jobs = &row["listJobs"];
    let listed = listed_jobs["complete"] == true
        && listed_jobs["jobs"]
            .as_array()
            .map(Vec::len)
            .is_some_and(|n| listed_jobs["count"] == json!(n));
    let listed_source =
        json!({"jobs": if listed { listed_jobs["jobs"].clone() } else { Value::Null }});
    let process_source = if inspected["jobs"].is_array() || inspected["shellCalls"].is_array() {
        inspected
    } else if listed {
        &listed_source
    } else {
        row
    };
    value["process_work"] = processes::dsh_work(record,process_source,alive && value["activity"] == "idle" && value["phase"] == "idle");
    // Task 278: its own running jobs, whatever its activity.
    value["background_jobs"] = processes::dsh_registered(record,process_source,alive);
    value
}

fn native_events(records: &Value) -> Vec<Value> {
    let mut events: Vec<Value> = Vec::new();
    let mut turn_key = String::new();
    let mut last_answer: Option<usize> = None;
    for record in records.as_array().into_iter().flatten() {
        let e = &record["event"];
        let data = &e["data"];
        let kind = string(e, "type");
        if kind == "turn/start" {
            turn_key = format!("dsh-turn-{}", e["seq"].as_u64().unwrap_or(0));
            last_answer = None;
            continue;
        }
        if kind == "turn/end" && !turn_key.is_empty() {
            if let Some(index) = last_answer {
                events[index]["reply_id"] = json!(turn_key);
            }
        }
        let (typ, detail, tool) = match kind {
            "user/message"
                if ["user", "user-question-reply"].contains(&string(&data["source"], "kind")) =>
            {
                ("UserPromptSubmit", message_text(data), String::new())
            }
            "assistant/message" => (
                "AgentMessage",
                message_text(&data["message"]),
                String::new(),
            ),
            "tool/call" => (
                "PreToolUse",
                journal::clipped(&data["arguments"], 1800),
                string(data, "name").to_owned(),
            ),
            "tool/result" => (
                "PostToolUse",
                message_text(&data["message"]),
                string(&data["message"], "name").to_owned(),
            ),
            "turn/end" => {
                let failure = turn_failure(e);
                if failure.is_object() {
                    (
                        "StopFailure",
                        string(&failure, "detail").to_owned(),
                        String::new(),
                    )
                } else {
                    ("Stop", String::new(), String::new())
                }
            }
            _ => continue,
        };
        if detail.is_empty() && typ != "Stop" && typ != "PreToolUse" {
            continue;
        }
        if typ == "AgentMessage" {
            last_answer = Some(events.len());
        }
        events.push(json!({"type":typ,"agent_id":"","seq":e["seq"].as_i64().unwrap_or(0)+1,"at":e["time"].as_f64().unwrap_or(0.0)/1000.0,
            "detail":detail,"tool":tool,"source":"dsh_native","message_id":data["message"]["id"]}));
    }
    events
}
fn message_text(value: &Value) -> String {
    let content = value.get("content").unwrap_or(value);
    if let Some(s) = content.as_str() {
        return journal::clipped(&json!(s), 32000);
    }
    journal::clipped(
        &json!(content
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["type"] == "text")
            .map(|p| string(p, "text"))
            .collect::<Vec<_>>()
            .join("\n")),
        32000,
    )
}

pub(super) fn process_rpc(record: &Value, method: &str, params: Value) -> Result<Value> { rpc(record,method,params) }

/// A resident adapter advertises each native operation it implements. Older
/// adapters stay running and keep the operation in Native UI.
fn supports(ping: &Value, operation: &str) -> bool {
    ping["nativeOperations"].as_array().is_some_and(|values| values.iter().any(|value| value == operation))
}
const OLDER_ADAPTER: &str = "The running DeepSeek host uses an older adapter; use Native UI until its next ordinary start";

/// Valid DeepSeek bindings, read without contacting any host.
pub(super) fn binding_records() -> Vec<Value> { PROVIDER.records() }

fn fork_reason(record: &Value, ping: &Value, inspected: &Value) -> &'static str {
    if ping["generation"] != inspected["generation"] {
        "DeepSeek host changed; refresh before forking"
    } else if !supports(ping, "fork") || inspected["forkSupported"] != true {
        "The running DeepSeek host uses an older adapter; fork this session in Native UI"
    } else if record["permissions_pending"] == true {
        "Resume this DeepSeek session to finish permission setup before forking"
    } else if inspected["forkReady"] != true {
        "Wait for a completed reply before forking"
    } else {
        ""
    }
}

fn fork_pending_path(record: &Value, tag: Option<&str>) -> PathBuf {
    root().join("dsh/forks").join(format!("{:x}.json", Sha256::digest(format!("{}\n{}", string(record,"session_id"), json!(tag)).as_bytes())))
}

/// Old releases did not retain the requested tag or a stable source key.
/// An exact old record can be migrated by its original retry, below. If that
/// path no longer matches (rename or a newly occupied suffix), refuse rather
/// than mint a different request. This is a bounded pending-intent read only.
fn legacy_fork_unconfirmed(record: &Value) -> Result<bool> {
    let entries = match fs::read_dir(root().join("dsh/forks")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut bytes_read = 0;
    for (count, entry) in entries.enumerate() {
        if count == 5000 || std::time::Instant::now() >= until { return Err("Original fork intents cannot be checked; nothing was changed".into()); }
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_none_or(|s| s != "json") { continue; }
        let bytes = labels::read_file(&path, 16 << 10).map_err(|e| e.to_string())?.ok_or("A fork intent changed while it was read")?;
        bytes_read += bytes.len();
        if bytes_read > 4 << 20 { return Err("Original fork intents exceed the review bound; nothing was changed".into()); }
        let intent: Value = serde_json::from_slice(&bytes).map_err(|_| "An original fork intent is unreadable; check Native UI")?;
        if !intent.is_object() || !session_id::canonical(string(&intent,"request_id"))
            || string(&intent,"source_conversation_id").is_empty() || string(&intent,"generation").is_empty()
            || string(&intent,"source").is_empty() || string(&intent,"target").is_empty()
        { return Err("An original fork intent is unreadable; check Native UI".into()); }
        if intent["version"] == 2 {
            let tag = if intent["intent_tag"].is_null() { None } else { Some(intent["intent_tag"].as_str().ok_or("An original fork tag is unreadable")?) };
            if !session_id::canonical(string(&intent,"source_session_id"))
                || fork_pending_path(&json!({"session_id":intent["source_session_id"]}),tag) != path
                || !session_id::canonical(string(&intent,"request_id"))
                || string(&intent,"source_conversation_id").is_empty() || string(&intent,"generation").is_empty()
            { return Err("An original fork intent is unreadable; check Native UI".into()); }
        } else if string(&intent,"source_conversation_id") == string(record,"conversation_id") {
            return Ok(true);
        }
    }
    if std::time::Instant::now() >= until { return Err("Original fork intents cannot be checked in time".into()); }
    Ok(false)
}

/// Native fork through the resident adapter's official Session Controller.
/// The new binding keeps the source's native profile and folder and records
/// the exact identity the engine returned; the source binding never changes.
/// The engine keeps the new live agent until its host next restarts.
pub(super) fn fork(
    name: &str,
    tag: Option<&str>,
    expected_run: Option<&str>,
    expected_conversation: Option<&str>,
    dry: bool,
    scope: Option<&session_action::Scope>,
) -> Result<String> {
    let _guard = lock(Some(&root().join("dsh/bindings.lock")))?;
    let mut record = binding(name)?;
    if let Some(scope) = scope { scope.check(&record)?; }
    if expected_run.is_some_and(|id| id != string(&record, "run_id"))
        || expected_conversation.is_some_and(|id| id != string(&record, "conversation_id"))
    {
        return Err("source session changed; refresh before forking".into());
    }
    // An archived session's name is free for a new one, as for other agents.
    let mut occupied: std::collections::BTreeSet<String> = binding_records().iter()
        .chain(records()?.iter())
        .map(|record| string(record, "name").to_owned()).collect();
    occupied.insert(name.to_owned());
    let requested = fork::choose_name(name, tag, &Default::default())?;
    let mut target = fork::choose_name(name, tag, &occupied)?;
    if dry {
        println!("Fork official dsh session {name} as {target}");
        return Ok(target);
    }
    let ping = rpc(&record, "ping", json!({}))?;
    if !supports(&ping, "fork") { return Err(OLDER_ADAPTER.into()); }
    let inspected = rpc(&record, "inspect", json!({"sessionId":record["conversation_id"],"includeProcesses":false}))?;
    let reason = fork_reason(&record, &ping, &inspected);
    if !reason.is_empty() { return Err(reason.into()); }
    if string(&record, "session_id").is_empty() { save(&mut record)?; }
    if !session_id::canonical(string(&record,"session_id")) { return Err("The source session identity is unavailable; nothing was forked".into()); }
    // A fork whose reply was lost is retried under the same request ID, which
    // the same host answers with the fork it already made.
    let pending = fork_pending_path(&record, tag);
    let legacy = root().join("dsh/forks").join(format!("{:x}.json", Sha256::digest(format!("{name}\n{target}").as_bytes())));
    let previous = labels::read_file(&pending, 16 << 10).map_err(|e| format!("Cannot read the original fork intent: {e}"))?;
    let old = if previous.is_none() { labels::read_file(&legacy, 16 << 10).map_err(|e| e.to_string())? } else { None };
    if previous.is_none() && old.is_none() && legacy_fork_unconfirmed(&record)? {
        return Err("An earlier legacy fork of this conversation is unconfirmed; check Native UI before forking again".into());
    }
    let mut request_id = uuid::Uuid::new_v4().to_string();
    if let Some(bytes) = previous.as_ref().or(old.as_ref()) {
        let intent: Value = serde_json::from_slice(bytes).map_err(|_| "The original fork intent is unreadable; check Native UI")?;
        let is_legacy = previous.is_none();
        let valid_id = uuid::Uuid::parse_str(string(&intent, "request_id"))
            .is_ok_and(|id| !id.is_nil() && id.to_string() == string(&intent, "request_id"));
        if !valid_id || intent["source_conversation_id"] != record["conversation_id"]
            || intent["generation"] != ping["generation"]
            || (!is_legacy && (intent["version"] != 2 || intent["source_session_id"] != record["session_id"]
                || intent["intent_tag"] != json!(tag) || intent["cwd"] != record["cwd"] || intent["agent_home"] != record["agent_home"]))
        {
            return Err(format!("An earlier fork of {name} was not confirmed before its source or DeepSeek host changed; check Native UI before forking again"));
        }
        let original_target = string(&intent, "target");
        // The allocated target is part of the original intent. A later occupied
        // name never selects a suffix and remints the unknown native request.
        if fork::choose_name(name, original_target.rsplit('/').next(), &Default::default())? != original_target
            || occupied.contains(original_target)
        {
            return Err("The original fork destination is no longer available; check Native UI".into());
        }
        target = original_target.to_owned();
        request_id = string(&intent, "request_id").to_owned();
    }
    if !PROVIDER.vacant(&target)? || storage::label_taken(&target) {
        return Err("The original fork destination is not vacant; check Native UI".into());
    }
    if previous.is_none() {
        private_dir(pending.parent().unwrap())?;
        atomic(&pending, &json!({"version":2,"request_id":request_id,"source":name,"source_session_id":record["session_id"],
            "source_conversation_id":record["conversation_id"],"intent_tag":tag,"requested":requested,"target":target,"generation":ping["generation"],
            "cwd":record["cwd"],"agent_home":record["agent_home"],"at":now()}).to_string())?;
        if let Some(bytes) = old {
            if labels::read_file(&legacy, 16 << 10).map_err(|e| e.to_string())?.as_ref() != Some(&bytes) {
                return Err("The original fork intent changed; check Native UI".into());
            }
            archive::remove_synced(&legacy)?;
        }
    }
    if let Some(scope) = scope { scope.changing(); }
    let answer = match rpc(&record, "fork", json!({"sessionId":record["conversation_id"],"cwd":record["cwd"],
        "generation":ping["generation"],"requestId":request_id,"title":target.rsplit('/').next()})) {
        Ok(answer) => answer,
        Err(error) if error.contains("delivery uncertain") => {
            return Err(format!("{error}. Fork {name} as {target} again to adopt the native result"));
        }
        Err(error) => return Err(error),
    };
    let child = string(&answer, "sessionId");
    if answer["requestId"] != request_id.as_str() || answer["sourceSessionId"] != record["conversation_id"]
        || answer["generation"] != ping["generation"] || answer["cwd"] != record["cwd"]
        || !child.starts_with("session-") || child == string(&record, "conversation_id")
    {
        return Err("DeepSeek fork acknowledgement did not match; check Native UI".into());
    }
    let run = uuid::Uuid::new_v4().to_string();
    let mut forked = json!({"backend":"dsh","name":target,"agent":"dsh","agent_home":record["agent_home"],"cwd":record["cwd"],
        "conversation_id":child,"run_id":run,"created":now(),"launch_id":null,"paused":false,
        "fork_parent_id":record["conversation_id"],"fork_source_name":name});
    // The native fork keeps the source's permission state; carry the explicit
    // launch override so a later resume does not silently replace it.
    if let Some(value) = record.get("permission_override") {
        forked["permission_override"] = value.clone();
    }
    save(&mut forked)?;
    let _ = fs::remove_file(&pending);
    // A fork starts in its source session's project (zerus-zjsj), written
    // after the bindings lock is released (zerus-wx3k).
    drop(_guard);
    if let Ok(config) = crate::config::Config::load() {
        crate::swarm::inherit_fork_project(&config, &target, name, None, &run, &json!(child));
    }
    match scope {
        Some(scope) => scope.result(&target, &run, child, None),
        None => println!("hgs: forked {name} as {target}"),
    }
    Ok(target)
}

/// Public messages matching `query` in exact bound sessions, by binding name:
/// `Some((events, truncated))`, or `None` when that history is unavailable.
/// Searching never starts, resumes or reloads a host.
pub(super) fn history_search(records: &[&Value], query: &str, per_session: usize) -> BTreeMap<String, Option<(Vec<Value>, bool)>> {
    let mut homes: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for record in records {
        homes.entry(string(record, "agent_home")).or_default().push(record);
    }
    // Hosts are independent processes: ask them concurrently, each bounded.
    let answers: Vec<Option<Value>> = std::thread::scope(|scope| {
        let queries: Vec<_> = homes.values().map(|group| scope.spawn(move || {
            let ids: Vec<&str> = group.iter().map(|record| string(record, "conversation_id")).collect();
            search_rpc(group[0], json!({"query":query,"sessionIds":ids,"perSession":per_session})).ok()
        })).collect();
        queries.into_iter().map(|query| query.join().ok().flatten()).collect()
    });
    let mut found = BTreeMap::new();
    for (group, answer) in homes.values().zip(answers) {
        for record in group {
            // The host's time budget skips later sessions: limited, not unavailable.
            let session = answer.as_ref().and_then(|answer| answer["sessions"].as_array()?.iter()
                .find(|session| session["sessionId"] == record["conversation_id"] && ["ok", "skipped"].contains(&string(session, "status"))));
            found.insert(storage::owner_key(record), session.map(|session| {
                let rows: Vec<Value> = session["events"].as_array().into_iter().flatten().map(|event| json!({"event":event})).collect();
                (native_events(&json!(rows)), session["truncated"] == true || session["status"] == "skipped")
            }));
        }
    }
    found
}

fn schedule_item(entry: &Value) -> Value {
    let mut record = entry.clone();
    if let Some(fields) = record.as_object_mut() {
        for key in ["sessionId", "status", "lastDelivery"] { fields.remove(key); }
    }
    let next = chrono::DateTime::parse_from_rfc3339(string(entry, "scheduledAt")).map(|at| at.timestamp_millis() as f64 / 1000.0).ok();
    let recurring = ["every", "daily", "weekly", "cron", "rrule"].contains(&string(entry, "kind"));
    let delivery = entry.get("lastDelivery").cloned().unwrap_or(Value::Null);
    json!({"id":entry["id"],"title":entry["title"],"prompt":entry["prompt"],"kind":entry["kind"],"status":entry["status"],
        "recurring":recurring,"scheduled_at":next,"last_delivery":delivery,"record":record})
}

/// Official DeepSeek Schedule tasks of one exact session: list them, or with
/// `--json` delete, edit (native compare-and-set) or read delivery history.
fn schedules(name: &str, record: &Value, args: &[String]) -> Result<Value> {
    if record["permissions_pending"] == true && args.iter().any(|arg| arg == "--json") {
        return Err("Resume this DeepSeek session to finish permission setup before managing its schedules".into());
    }
    let ping = rpc(record, "ping", json!({}))?;
    if !supports(&ping, "schedules") { return Err(OLDER_ADAPTER.into()); }
    let identity = json!({"name":name,"run_id":record["run_id"],"conversation_id":record["conversation_id"]});
    let unavailable = "Native Schedule is not enabled for this DeepSeek profile; enable Automation tasks in Native UI Plugins";
    if !args.iter().any(|arg| arg == "--json") {
        let listed = rpc(record, "schedules", json!({"sessionId":record["conversation_id"],"action":"list"}))?;
        if listed["available"] != true { return Err(unavailable.into()); }
        let mut output = identity;
        output["generation"] = listed["generation"].clone();
        output["items"] = json!(listed["items"].as_array().into_iter().flatten().map(schedule_item).collect::<Vec<_>>());
        return Ok(output);
    }
    let request = checked_request(record)?;
    let action = string(&request, "action");
    if !["delete", "update", "history"].contains(&action) { return Err("Choose delete, update or history".into()); }
    if request["expected_generation"] != ping["generation"] { return Err("DeepSeek host changed; refresh schedules".into()); }
    let mut params = json!({"sessionId":record["conversation_id"],"action":action,"generation":ping["generation"],"id":request["id"]});
    for (from, to) in [("expected", "expected"), ("title", "title"), ("prompt", "prompt"), ("change", "change"), ("limit", "limit"), ("before", "before")] {
        if !request[from].is_null() { params[to] = request[from].clone(); }
    }
    let answer = rpc(record, "schedules", params)?;
    if answer["available"] != true { return Err(unavailable.into()); }
    if answer["generation"] != ping["generation"] { return Err("DeepSeek host changed during the schedule change".into()); }
    let mut output = identity;
    output["request_id"] = request["request_id"].clone();
    output["action"] = json!(action);
    output["result"] = answer["result"].clone();
    Ok(output)
}

pub(super) fn processes(name: &str, output: Option<&str>, stop: Option<&str>, run: Option<&str>, conversation: Option<&str>, generation: Option<&str>) -> Result<i32> {
    let record=binding(name)?;
    if (output.is_some() || stop.is_some()) && (run!=Some(string(&record,"run_id")) || conversation!=Some(string(&record,"conversation_id"))) {
        return Err("The process belongs to a different session run or conversation".into());
    }
    let inspected=rpc(&record,"inspect",json!({"sessionId":record["conversation_id"]}))?;
    println!("{}",processes::dsh_dispatch(&record,&inspected,output,stop,generation)?);Ok(0)
}

pub(crate) fn inspect(name: &str, agent_id: Option<&str>) -> Result<Value> {
    // Internal recovery/input checks never need a process inventory.
    inspect_with_processes(name, agent_id, false, false)
}
/// Task 209: whether the host can compact this session natively. Capability is
/// the host's (`compactSupported`, from the command or the native seam), never
/// the member being idle at the moment of the read.
fn compact_capability(agent_id: Option<&str>, inspected: &Value) -> bool {
    agent_id.is_none() && inspected["compactSupported"] == true
}

/// Task 209: why a native compaction cannot run now (empty when it can).
fn compact_reason(supported: bool, phase: &str) -> &'static str {
    if !supported {
        "Native DeepSeek compaction is unavailable"
    } else if !matches!(phase, "" | "idle" | "ready") {
        "Wait for the DeepSeek agent to become idle before compacting its context"
    } else {
        ""
    }
}

/// Reads the session's token counters and records them as a sample (a team
/// profile takes a window's tokens from the samples at its two ends). `force`
/// records even closer than the minimum gap to the previous sample, for an
/// attempt's hand-out. Returns whether the counters changed since the previous
/// sample. Without a session binding or counters nothing is recorded.
///
/// This is an rpc to the session host (35 s timeout): never call it on the
/// dispatcher's tick or hand-out path (`teams::usage_sampler` runs it on its
/// own thread). `moment` is the hand-out moment a reading stands for: an idle
/// member's counters do not move, so the reading taken a moment later is
/// recorded at `moment`; a reading that came more than `HANDOUT_FRESH` seconds
/// after it may include the attempt's own work and is dropped (the start
/// stays unknown rather than wrong).
pub(crate) fn sample_usage(name: &str, force: bool, moment: Option<f64>) -> Result<bool> {
    let record = binding(name)?;
    let inspected = rpc(
        &record,
        "inspect",
        json!({"sessionId":record["conversation_id"],"agentId":Value::Null,"includeProcesses":false}),
    )?;
    let read = now();
    let at = match moment {
        Some(moment) if read - moment > usage_samples::HANDOUT_FRESH => return Ok(false),
        Some(moment) => moment,
        None => read,
    };
    usage_samples::record(string(&record, "name"), string(&record, "conversation_id"), at, &inspected["baseline"]["values"], force)
}
/// `hgs inspect NAME` with its process inventory, for `hgs watch`.
pub(crate) fn inspect_processes(name: &str) -> Result<Value> {
    inspect_with_processes(name, None, true, false)
}
/// `hgs inspect NAME` options: the subagent, whether to list processes and whether to name the current read.
/// Native DeepSeek has no archives, so `--archive` is refused rather than answered with the live session
/// (zerus-a4mub.33), as reference details refuse it.
fn inspect_options(args: &[String]) -> Result<(Option<&str>, bool, bool)> {
    if args.iter().any(|arg| arg == "--archive") {
        return Err("DeepSeek archives are unavailable".into());
    }
    let agent = args.windows(2).find(|pair| pair[0] == "--agent").map(|pair| pair[1].as_str());
    Ok((agent, !args.iter().any(|arg| arg == "--skip-processes"), args.iter().any(|arg| arg == "--current-read")))
}
/// The host reads of one `current_read` capture share this budget (zerus-a4mub.32), as search-history bounds
/// one host: a stalled host adds at most about this much to an inspection, which then names nothing.
const CAPTURE_BUDGET: std::time::Duration = std::time::Duration::from_secs(7);
const CAPTURE_EXPIRED: &str = "The DeepSeek host did not answer the current read in time";
/// One capture read from the host socket at `path`, ending by `deadline`.
#[cfg(unix)]
fn rpc_by(path: &Path, method: &str, params: Value, deadline: std::time::Instant) -> Result<Value> {
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        return Err(CAPTURE_EXPIRED.into());
    }
    rpc_at(path, method, params, left)
}
/// WhiteWh fork: on Windows the capture read goes through the host's named
/// pipe (`endpoint`), bounded by the same deadline.
#[cfg(not(unix))]
fn rpc_by(path: &Path, method: &str, params: Value, deadline: std::time::Instant) -> Result<Value> {
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        return Err(CAPTURE_EXPIRED.into());
    }
    let request = json!({"method":method,"params":params}).to_string();
    parse_response(&exchange(path, &request, left)?)
}
fn inspect_with_processes(name: &str, agent_id: Option<&str>, include_processes: bool, current_read: bool) -> Result<Value> {
    let record = binding(name)?;
    #[cfg(unix)]
    let socket = host_dir(&record).join("host.sock");
    #[cfg(not(unix))]
    let socket = endpoint(&host_dir(&record));
    let deadline = std::time::Instant::now() + CAPTURE_BUDGET;
    inspect_record(name, &record, agent_id, include_processes, current_read,
        &mut |method, params| rpc_by(&socket, method, params, deadline),
        &mut |method, params| rpc(&record, method, params))
}
/// `hgs read`'s own view of a DeepSeek session (`snapshot`) and the host list
/// it came from, for `current_read` (zerus-a4mub.24). The inspection captures
/// it before it reads the page it shows. The reply is the turn outline's
/// settled response, committed at `turn/end` after the turn's final message,
/// so that message is in the page or an earlier one. Its requests are signed
/// through `attention_id` before the page lists its own, so every signed
/// request still pending is listed. `capture` reads the host within the
/// capture's own deadline (zerus-a4mub.32). None when any of its reads fails,
/// including an idle row's failure read: a view missing that failure would
/// sign what `hgs read` may not accept.
fn read_capture(record: &Value, capture: &mut dyn FnMut(&str, Value) -> Result<Value>) -> Option<(Value, Value)> {
    let failed = std::cell::Cell::new(false);
    let mut checked = |method: &str, params: Value| {
        let answer = capture(method, params);
        if answer.is_err() {
            failed.set(true);
        }
        answer
    };
    let list = checked("list", json!({})).ok()?;
    let view = snapshot_with(record, &list, &mut checked);
    (!failed.get()).then_some((view, list))
}
/// The values `hgs read` would accept now from a `read_capture`, only for
/// the host generation that served the inspection's page. As `hgs read`, the
/// signature carries no separate question set; a host row without its
/// pending requests leaves them unknown, never acknowledgeable.
fn current_read_of(name: &str, record: &Value, (view, list): (Value, Value), inspected: &Value) -> Option<Value> {
    let generation = list["generation"].as_str()?;
    if inspected["generation"].as_str() != Some(generation) {
        return None;
    }
    let row = list["items"].as_array().into_iter().flatten().find(|row| row["sessionId"] == record["conversation_id"]);
    let known = row.is_some_and(|row| row["pendingQuestions"].is_array());
    read_marks::native_current_read(name, view, known.then_some(&[][..]))
}
/// One inspection: `capture` serves the bounded `current_read` capture, `rpc` the inspection's own reads.
fn inspect_record(name: &str, record: &Value, agent_id: Option<&str>, include_processes: bool, current_read: bool,
    capture: &mut dyn FnMut(&str, Value) -> Result<Value>, rpc: &mut dyn FnMut(&str, Value) -> Result<Value>) -> Result<Value> {
    let record = record.clone();
    // Only a main inspection names what a read mark may acknowledge.
    let read = if current_read && agent_id.is_none() { read_capture(&record, capture) } else { None };
    let inspected = rpc(
        "inspect",
        json!({"sessionId":record["conversation_id"],"agentId":agent_id,"includeProcesses":include_processes}),
    )?;
    // WhiteWh fork: the rows of this conversation; an older bridge lists all.
    let listed = match rpc("rows", json!({"sessionIds":[string(&record, "conversation_id")]})) {
        Err(error) if error == "Unsupported HGS native request" => rpc("list", json!({}))?,
        rows => rows?,
    };
    let row = listed["items"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|v| v["sessionId"] == record["conversation_id"])
        })
        .unwrap_or(&Value::Null);
    let child_row = agent_id.and_then(|id| listed["items"].as_array().and_then(|items| {
        let rows: Vec<_> = items.iter().filter(|child| child["sessionId"] == id
            && child["parentSessionId"] == record["conversation_id"] && child["origin"] == "subagent").collect();
        if let [row] = rows.as_slice() { Some(*row) } else { None }
    })).filter(|_| listed["generation"] == inspected["generation"]);
    let mut result = normalize(&record, child_row.unwrap_or(row), &inspected);
    if agent_id.is_none() {
        waits::decorate(std::slice::from_mut(&mut result));
        session_state::decorate(std::slice::from_mut(&mut result));
    }
    child_summaries(&record, &mut result, &listed, &inspected["generation"]);
    let mut events = native_events(&inspected["page"]["records"]);
    let process_snapshot=if include_processes { processes::dsh_snapshot(&record,&inspected) } else { Value::Null };
    for event in &mut events {
        let source=inspected["page"]["records"].as_array().into_iter().flatten().map(|v|&v["event"]).find(|e|e["seq"]==event["seq"]);
        if let Some(source)=source {
            let call=source["data"].get("callId").or_else(||source["data"]["message"].get("toolCallId"));
            if let Some(job)=process_snapshot["items"].as_array().into_iter().flatten().find(|j|j["owner"]=="main" && Some(&j["call_id"])==call) {event["process_id"]=job["id"].clone();}
        }
    }
    result["turn_started"] = json!(inspected["page"]["records"].as_array().into_iter().flatten().rev()
        .find(|v|v["event"]["type"]=="turn/start").and_then(|v|v["event"]["time"].as_f64()).unwrap_or(0.0)/1000.0);
    for event in &events {
        if event["type"] == "UserPromptSubmit" {
            result["recovery_user_at"] = event["at"].clone();
            result["recovery_user_text"] = event["detail"].clone();
        }
    }
    for event in inspected["page"]["records"].as_array().into_iter().flatten().map(|v|&v["event"]) {
        if event["type"] == "turn/end" && event["data"]["reason"]["kind"] == "completed" {
            result["recovery_success_at"] = json!(event["time"].as_f64().unwrap_or(0.0) / 1000.0);
        }
        if event["type"] == "turn/end" && ["aborted","interrupted","blocked"].contains(&string(&event["data"]["reason"],"kind")) {
            result["recovery_cancel_at"] = json!(event["time"].as_f64().unwrap_or(0.0) / 1000.0);
        }
    }
    result["cursor"] = json!(0);
    result["events"] = json!(events);
    attachments::decorate(&record, agent_id.unwrap_or(""), &mut result);
    result["voice_messages"] = conversation_history::inspected_voice_messages(&record, &result, agent_id);
    result["replace_events"] = json!(true);
    result["history_truncated"] = inspected["page"]["hasMore"].clone();
    result["host_generation"] = inspected["generation"].clone();
    let ping=rpc("ping",json!({})).unwrap_or(Value::Null);
    result["terminal_supported"]=json!(false);
    result["terminal_reason"]=json!(backend::DEEPSEEK_TERMINAL.text());
    result["allowed_actions"]=if agent_id.is_none() && ping["sessionActions"]==true && ping["generation"]==inspected["generation"] {
        inspected["lifecycleActions"].clone()
    } else {json!([])};
    result["action_reasons"]=json!({"archive":"Native DeepSeek has no archive lifecycle API","fork":"Native DeepSeek has no fork lifecycle API",
        "terminate":"Use Pause for this native DeepSeek session","restore":"Native DeepSeek has no archive lifecycle API",
        "pause":"Requires a supported resident adapter and its owned handle","resume":"Requires a supported resident adapter and a stopped session",
        "rename":"Requires the supported resident scoped adapter","forget":"Requires a supported resident adapter and its owned handle"});
    let fork_reason = if agent_id.is_some() { "Fork the parent DeepSeek session" } else { fork_reason(&record, &ping, &inspected) };
    result["fork_supported"] = json!(fork_reason.is_empty());
    result["fork_reason"] = json!(fork_reason);
    if fork_reason.is_empty() {
        if let Some(actions) = result["allowed_actions"].as_array_mut() { actions.push(json!("fork")); }
        result["action_reasons"].as_object_mut().unwrap().remove("fork");
    } else {
        result["action_reasons"]["fork"] = json!(fork_reason);
    }
    result["pending_questions"] = json!(question_cards(&record, &inspected));
    result["native_questions"] = inspected["pendingQuestions"].clone();
    result["native_projections"] = inspected["baseline"]["values"].clone();
    result["session_usage"] = usage::dsh(&inspected["baseline"]["values"]);
    if agent_id.is_none() {
        // The counters are cumulative and dsh keeps no history: this reading
        // is one sample a team profile takes its windows from.
        let _ = usage_samples::record(string(&record, "name"), string(&record, "conversation_id"), now(), &inspected["baseline"]["values"], false);
    }
    if include_processes { result["processes"] = process_snapshot; }
    // Task 209: the host's capability, never the member's momentary phase: an
    // idle check here made a session that was merely busy report that its agent
    // cannot compact at all. The reason says why a command cannot run now, and
    // the command itself refuses a busy agent.
    let compact_supported = compact_capability(agent_id, &inspected);
    result["compact_context_supported"] = json!(compact_supported);
    result["compact_context_reason"] = json!(compact_reason(compact_supported, string(&result, "phase")));
    result["compact_context_request"] = inspected["compactRequest"].clone();
    if result["compact_context_request"].is_object() {
        result["compact_context_request"]["run_id"] = record["run_id"].clone();
        result["compact_context_request"]["conversation_id"] = record["conversation_id"].clone();
        if result["compact_context_request"]["status"] == "compacting" {
            result["phase"] = json!("compacting"); result["activity"] = json!("busy");
            result["compaction_started"] = result["compact_context_request"]["at"].clone();
            result["compact_context_reason"] = json!("A compaction is already running");
        }
    }
    result["interrupt_supported"] = json!(inspected["interruptSupported"] == true &&
        ["working","tool","compacting"].contains(&string(&result,"phase")) && result["process_state"] == "running" && result["turn_started"].as_f64().unwrap_or(0.0)>0.0);
    if let Some(id) = agent_id {
        // The child's native status must never inherit the parent's running flag.
        let children: Vec<_> = listed["items"].as_array().into_iter().flatten()
            .filter(|child| child["sessionId"] == id && child["parentSessionId"] == record["conversation_id"]
                && child["origin"] == "subagent").collect();
        if let [child] = children.as_slice() {
            if listed["generation"] == inspected["generation"] && child["running"].is_boolean()
                && child["agentAvailable"].is_boolean() && (child["running"] != true || child["agentAvailable"] == true) {
                let running = child["running"] == true;
                if !["input","approval","error"].contains(&string(&result,"phase")) {
                    result["phase"] = json!(if running { "working" } else { "idle" });
                    result["activity"] = json!(if running { "busy" } else { "idle" });
                    result["activity_summary"] = json!(if running { "Working" } else { "Ready" });
                }
                result["tracking_evidence"] = json!({"source":"dsh_native_api","kind":"live_state","generation":inspected["generation"],"native_id":id});
            } else {
                result["phase"] = json!("unknown");
                result["activity"] = json!("unknown");
                result["activity_summary"] = json!("Status unavailable");
            }
        } else {
            result["phase"] = json!("unknown");
            result["activity"] = json!("unknown");
            result["activity_summary"] = json!("Status unavailable");
        }
        result["agent_id"] = json!(id);
        result["parent_conversation_id"] = record["conversation_id"].clone();
        result["conversation_id"] = json!(format!("{}/{id}", string(&record, "conversation_id")));
        result["send_supported"] = inspected["childSendSupported"].clone();
        result["read_only"] = json!(inspected["childSendSupported"] != true);
        result["child_mode"] = inspected["childMode"].clone();
        result["history_scope"] = json!("Native DeepSeek subagent history");
    } else if let Ok(catalog) = rpc("catalog", json!({})) {
        if string(&result, "effort").is_empty() {
            if let Some(model) = catalog["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|g| g["id"] == result["model_provider"])
                .and_then(|g| g["models"].as_array())
                .and_then(|models| models.iter().find(|m| m["id"] == result["model"]))
            {
                result["effort"] = model["reasoning"]["defaultEffort"].clone();
            }
        }
        result["settings_model"] = json!(format!(
            "{}/{}",
            string(&result, "model_provider"),
            string(&result, "model")
        ));
        result["model_options"] = json!(catalog_models(&catalog));
    }
    if agent_id.is_none() && record.get("account_defaults").is_some() {
        result["account_defaults"] = record["account_defaults"].clone();
    }
    if agent_id.is_none() { notifications::enrich(&mut result, notifications::load().as_ref(), journal::journal_name(&record)); }
    if let Some(current) = read.and_then(|read| current_read_of(name, &record, read, &inspected)) {
        result["current_read"] = current;
    }
    Ok(result)
}

/// Read-only resident host methods; never ensure_host or a lifecycle RPC.
/// Settings > Accounts: the resident host's catalog through its read-only
/// `catalog` method (`account_models`). The native default
/// (agent-default-model) comes along.
fn account_models_from(catalog: &Value) -> Value {
    let models = catalog_models(catalog);
    let default = &catalog["default"];
    let model = match (string(default, "provider"), string(default, "model")) {
        (provider, model) if !provider.is_empty() && !model.is_empty() => format!("{provider}/{model}"),
        _ => String::new(),
    };
    json!({"models":models.iter().take(64).collect::<Vec<_>>(),"truncated":models.len()>64,
        "provider_defaults":{"model":model,"effort":string(default,"reasoningEffort")}})
}

pub(super) fn notification_snapshot(record: &Value) -> Result<(Value, Option<Vec<Value>>)> {
    notification_snapshot_with(record, |method, params| match method {
        "list" => session_rows(record, &[string(record, "conversation_id")]),
        _ => rpc(record, method, params),
    })
}
fn notification_snapshot_with(record: &Value, mut observe: impl FnMut(&str, Value) -> Result<Value>) -> Result<(Value, Option<Vec<Value>>)> {
    let inspected = observe("inspect", json!({"sessionId":record["conversation_id"],"includeProcesses":false}))?;
    let listed = observe("list", json!({}))?;
    if !inspected["generation"].is_string() || inspected["generation"] != listed["generation"] { return Err("baseline_unavailable".into()); }
    let pending = inspected["pendingQuestions"].as_array().ok_or("baseline_unavailable")?;
    if pending.iter().any(|question| string(question, "id").is_empty() || string(question, "id").len() > 256 || string(question, "id").chars().any(char::is_control)) { return Err("baseline_unavailable".into()); }
    let row = listed["items"].as_array().and_then(|rows| rows.iter().find(|row| row["sessionId"] == record["conversation_id"])).ok_or("baseline_unavailable")?;
    let mut view = normalize(record, row, &inspected);
    child_summaries(record, &mut view, &listed, &inspected["generation"]);
    // Listing read signatures represent these requests through attention_id;
    // only Codex optional questions have a separate listing question set here.
    Ok((view, Some(Vec::new())))
}

fn question_cards(record: &Value, inspected: &Value) -> Vec<Value> {
    inspected["pendingQuestions"].as_array().into_iter().flatten().map(|q| {
        let items=q["questions"].as_array().into_iter().flatten().map(|item|json!({
            "id":item["id"],"question":item["question"],"header":item["header"],"body":item["detail"],"multi_select":item["multiSelect"],
            "allow_other":q["approval"]!=true,"required":true,"options":item["options"].as_array().into_iter().flatten().enumerate().map(|(i,o)|json!({"id":if q["approval"]==true {if i==0 {"allow".to_owned()}else{"deny".to_owned()}}else{i.to_string()},"label":o["label"],"description":o["description"]})).collect::<Vec<_>>()
        })).collect::<Vec<_>>();
        let hash=format!("{:x}",Sha256::digest(json!([inspected["generation"],q]).to_string().as_bytes()));
        let mut card=json!({"question_id":q["id"],"question_hash":hash,"tool_call_id":q["id"],"run_id":record["run_id"],"conversation_id":record["conversation_id"],
            "agent_id":"main","source":"dsh_native","approval":q["approval"]==true,"can_answer":true,"answer_transport":"native_dsh","questions":items});
        // The exact shell command the bridge tied to the approval's call, for a
        // team's safe actions; a host without it shows no command.
        if q["approval"]==true {
            if let Some(command)=q["command"].as_str() {card["tool_input"]=json!({"command":command});}
            if let Some(tool)=q["tool"].as_str() {card["tool"]=json!(tool);}
        }
        card
    }).collect()
}

pub(super) fn recover(snapshot: &Value, request_id: &str) -> Result<Value> {
    let _guard = lock(None)?;
    let record = binding(string(snapshot,"name"))?;
    if record["permissions_pending"] == true {
        return Err("Resume this DeepSeek session to finish permission setup before recovery".into());
    }
    if record["run_id"] != snapshot["run_id"] || record["conversation_id"] != snapshot["conversation_id"] {
        return Err("DeepSeek session changed before recovery".into());
    }
    recovery::check_native_submission(snapshot)?;
    let fresh=inspect(string(snapshot,"name"),None)?;
    for key in ["run_id","conversation_id","host_generation","model","provider_error","recovery_user_at","recovery_cancel_at"] {
        if fresh[key]!=snapshot[key] {return Err("DeepSeek failed request changed before recovery".into());}
    }
    if fresh["phase"]!="error" || fresh["process_state"]!="running" {
        return Err("DeepSeek is no longer waiting after this failure".into());
    }
    if snapshot["native_recovery"].is_object() {
        rpc(&record,"recover",json!({"sessionId":record["conversation_id"],"id":snapshot["native_recovery"]["id"],
            "generation":snapshot["host_generation"],"requestId":request_id}))
    } else {
        let native=rpc(&record,"send",json!({"sessionId":record["conversation_id"],"requestId":request_id,
            "content":[{"type":"text","text":recovery::CONTINUATION}]}))?;
        Ok(json!({"status":"confirmed","request_id":request_id,"native":native}))
    }
}
pub(super) fn dismiss_recovery(snapshot: &Value) -> Result<Value> {
    let record = binding(string(snapshot,"name"))?;
    rpc(&record,"cancel-recovery",json!({"sessionId":record["conversation_id"],"id":snapshot["native_recovery"]["id"]}))
}

pub(super) fn interrupt(snapshot: &Value) -> Result<Value> {
    let record=binding(string(snapshot,"name"))?;
    if record["run_id"]!=snapshot["run_id"] || record["conversation_id"]!=snapshot["conversation_id"] {
        return Err("DeepSeek session changed before interruption".into());
    }
    interrupt_with(&record, snapshot, rpc)
}
/// The interrupt request of `snapshot`'s turn, through `rpc`, with each
/// error classified by [`interrupt_refusal`].
fn interrupt_with(
    record: &Value,
    snapshot: &Value,
    rpc: impl FnOnce(&Value, &str, Value) -> Result<Value>,
) -> Result<Value> {
    rpc(record,"interrupt",json!({"sessionId":record["conversation_id"],"generation":snapshot["host_generation"],"turnStarted":snapshot["turn_started"]}))
        .map_err(interrupt_refusal)
}
/// The host disarms goals and aborts a compaction before its native cancel,
/// so only a refusal before those effects is definite (zerus-1wchz): no host
/// to reach, a request it never got whole, or one of the bridge's own checks
/// ahead of them.
fn interrupt_refusal(error: String) -> String {
    const BEFORE_EFFECTS: [&str; 4] = [
        HOST_UNAVAILABLE,
        "DeepSeek host changed",
        "Use the parent to manage a subagent",
        "DeepSeek turn changed; nothing was stopped",
    ];
    if BEFORE_EFFECTS.contains(&error.as_str())
        || error.starts_with(INCOMPLETE)
        || error.contains("delivery uncertain")
    {
        error
    } else {
        format!("Interrupt delivery uncertain: {error}")
    }
}

fn checked_request(record: &Value) -> Result<Value> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(30 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 30 * 1024 * 1024 {
        return Err("Native request is too large".into());
    }
    let request: Value = serde_json::from_slice(crate::state::without_bom(&bytes)).map_err(|e| e.to_string())?;
    uuid::Uuid::parse_str(string(&request, "request_id")).map_err(|_| "Invalid request ID")?;
    if request["expected_run_id"] != record["run_id"]
        || request["expected_conversation_id"] != record["conversation_id"]
    {
        return Err("Session changed; refresh before trying again".into());
    }
    Ok(request)
}
/// Lifecycle remains confined to a supported resident host. Never start,
/// dispose, reload or replace a host to implement a mobile scoped action.
pub(super) fn session_action(name: &str, action: &str, new_name: Option<&str>, scope: &session_action::Scope) -> Result<()> {
    if !["pause","resume","rename","forget"].contains(&action) {return Err("This native DeepSeek adapter has no archive lifecycle API".into());}
    let _guard=lock(Some(&root().join("dsh/bindings.lock")))?;
    let mut record=binding(name)?;scope.check(&record)?;
    if let Some(new)=new_name {
        let old:Vec<_>=name.split('/').collect();let parts:Vec<_>=new.split('/').collect();
        if parts.len()!=3 || old.len()!=3 || parts[..2]!=old[..2] || parts.iter().any(|p|p.is_empty() || p.trim()!=*p || p.contains([':', '.']) || p.chars().any(char::is_control)) {
            return Err("Rename must preserve the original DeepSeek provider and project".into());
        }
        if new!=name && exists(new) {return Err("The destination session already exists".into());}
    }
    if action=="rename" {
        if legacy_fork_unconfirmed(&record)? { return Err("Resolve the original legacy fork before renaming this session".into()); }
        PROVIDER.prepare_label(&record,new_name.unwrap())?;
    }
    let ping=rpc(&record,"ping",json!({}))?;
    if ping["sessionActions"]!=true {return Err("The resident DeepSeek adapter lacks scoped lifecycle; it was left running".into());}
    let inspected=rpc(&record,"inspect",json!({"sessionId":record["conversation_id"],"includeProcesses":false}))?;
    if ping["generation"]!=inspected["generation"] || !inspected["lifecycleActions"].as_array().is_some_and(|values|values.iter().any(|value|value==action)) {
        return Err("This action is unavailable for the exact native DeepSeek handle".into());
    }
    // A resume applies the binding's explicit override before its account
    // default; a fresh native preset is validated against the running host.
    let resume_selection = if action == "resume" {
        let config = crate::config::Config::load().map_err(|e| e.message)?;
        Some(effective_selection(&record, &config)?)
    } else {
        None
    };
    if let Some(PermissionSelection::Preset(preset)) = &resume_selection {
        let presets = fresh_presets(&record).map_err(|_| {
            format!("Could not verify the permission preset \"{preset}\" against the running DeepSeek host; choose Default or Bypass")
        })?;
        if !presets.iter().any(|option| &option.value == preset) {
            return Err(format!("The permission preset \"{preset}\" is no longer offered by the DeepSeek host; choose another preset before resuming"));
        }
    }
    let native_preset = supports(&ping, "permission-preset");
    let mode = resume_selection.as_ref().map(PermissionSelection::legacy_mode).unwrap_or("provider");
    let mut params = json!({"sessionId":record["conversation_id"],"generation":inspected["generation"],
        "action":action,"title":new_name.map(|name|name.rsplit('/').next().unwrap()),"permissionMode":mode});
    if let Some(PermissionSelection::Preset(preset)) = &resume_selection {
        if native_preset { params["permissionPreset"] = json!(preset); }
    }
    let was_pending = record["permissions_pending"] == true;
    scope.changing();
    let answer=rpc(&record,"session-action",params)?;
    if answer["sessionId"]!=record["conversation_id"] || answer["action"]!=action {return Err("DeepSeek lifecycle acknowledgement did not match".into());}
    if answer["generation"]!=inspected["generation"] {return Err("DeepSeek host changed during lifecycle handoff".into());}
    if answer["status"]=="failed" {scope.unchanged();return Err(string(&answer,"error").into());}
    if answer["status"]!="completed" {
        // A preset handoff that may have changed the native handle stays
        // pending, so the next explicit launch/resume verifies and finishes it.
        if answer["status"]=="uncertain"
            && matches!(resume_selection.as_ref(), Some(PermissionSelection::Preset(_))) {
            record["permissions_pending"]=json!(true);
            save(&mut record)?;
        }
        return Err("DeepSeek lifecycle handoff was not confirmed".into());
    }
    match action {
        "pause"=>record["paused"]=json!(true),
        "resume"=>{
            record["paused"]=json!(false);
            record["run_id"]=json!(uuid::Uuid::new_v4().to_string());
            record.as_object_mut().unwrap().remove("permissions_pending");
            // An older resident cannot take the preset in its resume call;
            // finish the explicit resume through its authenticated API. The
            // durable intent is saved first so an uncertain answer is retried
            // by the next explicit resume, never blindly replayed.
            if let Some(PermissionSelection::Preset(preset)) = &resume_selection {
                if !native_preset {
                    record["permissions_pending"]=json!(true);
                    save(&mut record)?;
                    let url=account_ui_url(Path::new(string(&record,"agent_home")))?;
                    if let Err(reason)=permissions_compat_json(&url,Some(string(&record,"conversation_id")),
                        "apply-preset",Some(preset.as_str()),was_pending,true) {
                        return Err(permission_apply_error(&reason,true));
                    }
                    record.as_object_mut().unwrap().remove("permissions_pending");
                }
            }
        },
        "rename"=>{notifications::anchor(&mut record);if storage::in_id_layout(&record) { recovery::rename_job_in_place(&record,new_name.unwrap())?; } else { recovery::rename_job(name,new_name.unwrap(),string(&record,"run_id"))?; } record["name"]=json!(new_name.unwrap());},
        "forget"=>{remove(&record)?;return Ok(());},
        _=>unreachable!(),
    }
    save(&mut record)?;
    if action=="rename" && new_name!=Some(name) && !storage::in_id_layout(&record) {
        archive::remove_synced(&binding_path(name))?;
        // The rename is done; losing read marks only shows a reply as unread again.
        let _=read_marks::rename_owner(name,new_name.unwrap());
    }
    scope.result_record(&record);Ok(())
}

/// A binding's rename, with its native title when `native`; the caller holds
/// the bindings lock and has checked the new name.
fn rename_binding(name: &str, record: &mut Value, new: &str, native: bool) -> Result<()> {
    if legacy_fork_unconfirmed(record)? { return Err("Resolve the original legacy fork before renaming this session".into()); }
    PROVIDER.prepare_label(record, new)?;
    if native {
        rpc(
            record,
            "rename",
            json!({"sessionId":record["conversation_id"],"title":new.rsplit('/').next()}),
        )?;
    }
    notifications::anchor(record);
    record["name"] = json!(new);
    if storage::in_id_layout(record) { recovery::rename_job_in_place(record,new)?; } else { recovery::rename_job(name,new,string(record,"run_id"))?; }
    save(record)?;
    if !storage::in_id_layout(record) { archive::remove_synced(&binding_path(name))?; }
    // The token samples follow the name; losing them only turns old windows unknown.
    let _ = usage_samples::rename(name, new);
    // The rename is done; losing read marks only shows a reply as unread again.
    if !storage::in_id_layout(record) { let _ = read_marks::rename_owner(name, new); }
    Ok(())
}

/// What setting a replaced DeepSeek session aside gave.
#[derive(Debug, PartialEq)]
pub(crate) enum Aside {
    /// No DeepSeek session of that conversation holds the name any more.
    Gone,
    /// It is in a turn or waits on a question: it keeps the name for now.
    Busy,
    /// Paused (when it ran) and renamed to `target`.
    Renamed { target: String, paused: bool },
    /// Renamed to `target`, but the host refused to pause it (`error`): a turn
    /// began meanwhile, or the native UI owns it.
    StillRuns { target: String, error: String },
}

/// The phases of a running session that hold a turn: never paused under it.
const TURN_PHASES: [&str; 5] = ["working", "tool", "compacting", "input", "approval"];

/// RogueDOD (2026-11-12, team 75549318): a team member's DeepSeek session
/// that a relaunch or a swap replaced is set aside the way a terminal session
/// is: paused once its turn has ended and renamed to `<name>-replaced`, so the
/// new session takes the member's name. Without it every new session failed
/// with "This DeepSeek session name already exists" while the old one kept
/// running. A given `conversation` must still be the binding's: a resume gives
/// the session a new run id but keeps its conversation.
pub(crate) fn set_aside(name: &str, conversation: &str) -> Result<Aside> {
    let Ok(record) = binding(name) else { return Ok(Aside::Gone) };
    if !conversation.is_empty() && string(&record, "conversation_id") != conversation {
        return Ok(Aside::Gone);
    }
    // Read before the bindings lock is taken: an inspect may record upload
    // state under it. A host that is down is started, as a launch would.
    let current = match inspect(name, None) {
        Ok(current) => current,
        Err(_) => {
            ensure_host(&record)?;
            inspect(name, None)?
        }
    };
    let running = current["state"] == "running";
    if running && TURN_PHASES.contains(&string(&current, "phase")) {
        return Ok(Aside::Busy);
    }
    let target = (1..=9)
        .map(|n| if n == 1 { format!("{name}-replaced") } else { format!("{name}-replaced-{n}") })
        .find(|candidate| !exists(candidate))
        .ok_or("the old session's -replaced names are all taken; rename it or hgs kill it")?;
    let _guard = lock(Some(&root().join("dsh/bindings.lock")))?;
    let mut record = binding(name)?;
    if record["conversation_id"] != current["conversation_id"] {
        return Err("the old DeepSeek session changed while it was set aside".into());
    }
    // A native rename opens a stopped session in the host, where the bridge
    // does not own it and cannot pause it: a stopped one is renamed in Zerus
    // only (its native title stays). A running one is renamed while the bridge
    // still owns it, then paused through the host's checked action, which
    // refuses while a native turn runs (a plain pause would end that turn).
    rename_binding(name, &mut record, &target, running)?;
    if !running {
        return Ok(Aside::Renamed { target, paused: false });
    }
    let answer = rpc(
        &record,
        "session-action",
        json!({"sessionId":record["conversation_id"],"generation":current["host_generation"],"action":"pause","permissionMode":"provider"}),
    )?;
    if answer["status"] != "completed" || answer["sessionId"] != record["conversation_id"] {
        let error = string(&answer, "error");
        return Ok(Aside::StillRuns { target, error: if error.is_empty() { "the pause was not confirmed".into() } else { error.to_owned() } });
    }
    record["paused"] = json!(true);
    save(&mut record)?;
    Ok(Aside::Renamed { target, paused: true })
}

pub(crate) fn dispatch(command: &str, args: &[String]) -> Result<i32> {
    let name = args.first().ok_or("Choose a DeepSeek session")?;
    let _guard = if !["inspect", "exists", "schedules"].contains(&command) {
        Some(lock(Some(&root().join("dsh/bindings.lock")))?)
    } else {
        None
    };
    let mut record = binding(name)?;
    match command {
        "inspect" => {
            let (agent, include_processes, current_read) = inspect_options(args)?;
            println!("{}", inspect_with_processes(name, agent, include_processes, current_read)?);
        }
        "compact-context" => {
            let request = checked_request(&record)?;
            let current = inspect(name, None)?;
            // Task 209: supported says the host can compact; this says it can
            // do it now.
            if current["compact_context_supported"] != true
                || !matches!(string(&current, "phase"), "" | "idle" | "ready")
            {
                return Err("Wait for the DeepSeek agent to become idle before compacting its context".into());
            }
            rpc(&record, "compact", json!({"sessionId":record["conversation_id"],"generation":current["host_generation"],"requestId":request["request_id"]}))?;
            println!("{}",json!({"request_id":request["request_id"],"name":name,"run_id":record["run_id"],"conversation_id":record["conversation_id"],"status":"submitted"}));
        }
        "send" | "settings" => {
            if record["permissions_pending"] == true {
                return Err("Resume this DeepSeek session to finish permission setup before sending or changing settings".into());
            }
            let request = checked_request(&record)?;
            let receipt = json!({"request_id":request["request_id"],"name":name,"run_id":record["run_id"],"conversation_id":record["conversation_id"],"at":now()});
            let mut receipt = receipt;
            if command == "send" {
                let agent_id = string(&request, "agent_id");
                let text = string(&request, "text");
                if text.len() > 65536 {
                    return Err("Message exceeds 64 KiB".into());
                }
                let files = request["attachments"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let references = files
                    .iter()
                    .map(|f| string(f, "reference"))
                    .collect::<Vec<_>>();
                attachments::validate_references(&references)?;
                let image = |file: &Value| {
                    ["image/png", "image/jpeg", "image/webp", "image/gif"].contains(&string(file, "mime"))
                };
                // WhiteWh fork: other files go through the host's own upload
                // service, as its native composer sends them; an older bridge
                // takes images only.
                if !files.iter().all(image)
                    && rpc(&record, "ping", json!({}))?["fileAttachments"] != true
                {
                    return Err("This DeepSeek composer supports image attachments; use the native UI for other files".into());
                }
                let mut content = Vec::new();
                for part in attachments::ordered_parts(text, &references) {
                    match part {
                        attachments::MessagePart::Text(value) => {
                            if !value.trim().is_empty() {
                                content.push(json!({"type":"text","text":value}));
                            }
                        }
                        attachments::MessagePart::Attachment(index) => {
                            let file = &files[index];
                            if !references[index].is_empty() && !text.contains(references[index]) {
                                content.push(json!({"type":"text","text":references[index]}));
                            }
                            if image(file) {
                                content.push(json!({"type":"image","mediaType":file["mime"],"data":file["data_base64"],"name":file["name"]}));
                            } else {
                                content.push(json!({"type":"file","data":file["data_base64"],"name":file["name"]}));
                            }
                        }
                    }
                }
                if content.is_empty() {
                    return Err("Message is empty".into());
                }
                let receipt_path = root()
                    .join("dsh/receipts")
                    .join(format!("{}.json", string(&request, "request_id")));
                let digest = format!(
                    "{:x}",
                    Sha256::digest(json!([name, request]).to_string().as_bytes())
                );
                if receipt_path.exists() {
                    let previous: Value = serde_json::from_slice(
                        &fs::read(&receipt_path).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                    if previous["digest"] != digest {
                        return Err("This request ID was already used for another message".into());
                    }
                    if previous["status"] == "submitted" {
                        println!("{previous}");
                        return Ok(0);
                    }
                    return Err("Delivery uncertain: this message may already be in DeepSeek. Check Activity before sending it again".into());
                }
                if !agent_id.is_empty() {
                    let child = inspect(name, Some(agent_id))?;
                    if child["send_supported"] != true {
                        return Err("This child cannot accept messages; it must be continuable with a live parent".into());
                    }
                    receipt["agent_id"] = json!(agent_id);
                }
                // Authentication is a definite preflight failure, not an
                // uncertain delivery. Do not leave a pending receipt for it.
                let readiness = rpc(
                    &record,
                    "inspect",
                    json!({"sessionId":record["conversation_id"]}),
                )?;
                if readiness["authRequired"] == true {
                    return Err(
                        "Sign in to DeepSeek in the native UI before sending a message".into(),
                    );
                }
                receipt["digest"] = json!(digest);
                receipt["attachments"] = attachments::stage_native(&request)?;
                receipt["text"] = json!(text);
                receipt["submitted_text"] = json!(text);
                receipt["submitted_at"] = json!(now());
                receipt["status"] = json!("pending");
                if let Some(target) = session_graph::target_receipt(&record) {
                    delivery::bind(&mut receipt, &record, &target);
                    if agent_id.is_empty() {
                        receipt["graph_target"] = target;
                    }
                }
                receipt_index::persist(&receipt_path, &receipt)?;
                if agent_id.is_empty() {
                    rpc(
                        &record,
                        "send",
                        json!({"sessionId":record["conversation_id"],"requestId":request["request_id"],"content":content,"mode":"steer","expectedCompactionId":request["expected_compaction_id"]}),
                    )?;
                } else {
                    rpc(
                        &record,
                        "send-child",
                        json!({"parentSessionId":record["conversation_id"],"childSessionId":agent_id,
                        "requestId":request["request_id"],"content":content,"mode":"continuable","delivery":"steer"}),
                    )?;
                }
                receipt["status"] = json!("submitted");
                receipt["transport"] = json!("native_dsh");
                receipt_index::persist(&receipt_path, &receipt)?;
            } else {
                let current = inspect(name, None)?;
                let selected = if string(&request, "model").is_empty() {
                    string(&current, "settings_model")
                } else {
                    string(&request, "model")
                };
                // A paused session resumes under the account permission mode.
                let mode = crate::accounts::permission_mode(&crate::config::Config::load().map_err(|e|e.message)?,"native-dsh","dsh").map_err(|e|e.message)?;
                let (selected, selection) = apply_model(&record, &selected, string(&request, "effort"), &mode)?;
                receipt["status"] = json!("applied");
                receipt["scope"] = json!("session");
                receipt["model"] = json!(selected);
                receipt["effort"] = selection["reasoningEffort"].clone();
            }
            println!("{receipt}");
        }
        "answer" => {
            let request = checked_request(&record)?;
            let inspected = rpc(
                &record,
                "inspect",
                json!({"sessionId":record["conversation_id"]}),
            )?;
            let cards = question_cards(&record, &inspected);
            let card = cards
                .iter()
                .find(|q| {
                    q["question_id"] == request["question_id"]
                        && q["question_hash"] == request["expected_question_hash"]
                })
                .ok_or("This question changed or is no longer pending")?;
            let mut answers = Vec::new();
            for answer in request["answers"]
                .as_array()
                .ok_or("Answers are required")?
            {
                let item = card["questions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|q| q["id"] == answer["question_id"])
                    .ok_or("Unknown question")?;
                let mut labels = Vec::new();
                for id in answer["selected_option_ids"]
                    .as_array()
                    .ok_or("Invalid selected options")?
                {
                    let option = item["options"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|o| o["id"] == *id)
                        .ok_or("Unknown answer option")?;
                    labels.push(option["label"].clone());
                }
                if item["allow_other"] != true && !string(answer, "text").is_empty() {
                    // Task #37 (team ede724c4): name the way through, not only the refusal.
                    return Err("A DeepSeek approval takes Allow once or Reject only, no text. Send the reason as the answer's top-level \"text\" with hgs team answer: it reaches the agent as a note after the decision".into());
                }
                answers.push(json!({"id":answer["question_id"],"selected":labels,"custom":string(answer,"text")}));
            }
            rpc(
                &record,
                "answer",
                json!({"sessionId":record["conversation_id"],"id":request["question_id"],"generation":inspected["generation"],"answers":answers}),
            )?;
            println!(
                "{}",
                json!({"status":"answered","request_id":request["request_id"],"name":name,"question_id":request["question_id"],"question_hash":request["expected_question_hash"],"run_id":record["run_id"],"conversation_id":record["conversation_id"]})
            );
        }
        "pause" | "resume" => {
            if args.iter().any(|v| v == "--dry-run") {
                println!("{command} official dsh {name}");
                return Ok(0);
            }
            if command == "resume" {
                let block_stale = ensure_host(&record)?;
                mark_upload_may_be_active(&mut record, block_stale);
            }
            if command == "resume" {
                create_session(&mut record, false)?;
                // N17/U22/N8: a resume is verified from the records written
                // after it, starting at the store's length at it.
                mark_upload_start(&mut record);
            } else {
                rpc(
                    &record,
                    command,
                    json!({"sessionId":record["conversation_id"]}),
                )?;
            }
            record["paused"] = json!(command == "pause");
            if command == "resume" {
                record["run_id"] = json!(uuid::Uuid::new_v4().to_string());
            }
            save(&mut record)?;
        }
        "rename" => {
            let new = args.get(1).ok_or("Choose a new session name")?;
            if !new.starts_with("dsh/")
                || new.split('/').count() != 3
                || new.contains(['\n', '\r', ':'])
                || new.chars().any(char::is_whitespace)
            {
                return Err("Invalid DeepSeek session name".into());
            }
            if exists(new) {
                return Err("This name is already used".into());
            }
            if args.iter().any(|v| v == "--dry-run") {
                return Ok(0);
            }
            rename_binding(name, &mut record, new, true)?;
            println!(
                "{}",
                json!({"old_name":name,"name":new,"new_name":new,"run_id":record["run_id"],"conversation_id":record["conversation_id"]})
            );
        }
        "forget" => {
            if args.iter().any(|v| v == "--dry-run") {
                return Ok(0);
            }
            rpc(
                &record,
                "pause",
                json!({"sessionId":record["conversation_id"]}),
            )?;
            remove(&record)?;
            usage_samples::forget(name);
        }
        "exists" => return Ok(0),
        "schedules" => println!("{}", schedules(name, &record, &args[1..])?),
        // RogueDOD (2026-11-12): the way to free a DeepSeek session's name.
        "archive" => {
            return Err(format!(
                "A DeepSeek session has no archive: its conversation stays in the DeepSeek host. To free the name {name}, pause it (hgs pause {name}) and rename it (hgs rename {name} {name}-old); hgs kill {name} forgets it"
            ))
        }
        _ => {
            return Err(format!(
                "{command} is not supported for native DeepSeek sessions yet"
            ))
        }
    }
    Ok(0)
}

// DeepSeek is a Phase 2 non-goal on Windows.
#[cfg(all(test, unix))]
mod current_read_tests {
    use super::*;

    /// zerus-a4mub.24: a DeepSeek inspection names exactly what `hgs read`
    /// accepts on the host list read before its page, never a later one. The
    /// inspection reads attachment, wait and recovery state, so the fixture
    /// runs in a child process with private state and a scripted host.
    #[test]
    fn current_read_is_hgs_reads_view_captured_before_the_page() {
        run_private("state::dsh::current_read_tests::fixture");
    }

    /// zerus-lgr9s: a supervised DeepSeek child's listed row says whether it
    /// waits for the person, by the generic rule. Its unread reply or its own
    /// question is the parent's, so the row stays quiet; a tool approval or
    /// signing in is the person's.
    #[test]
    fn listed_child_rows_mark_only_person_requests() {
        run_private("state::dsh::current_read_tests::child_rows");
    }

    /// zerus-yj7ef: an idle DeepSeek row in a listing reports its running
    /// background jobs from the bridge's bounded summary: one running job is
    /// Background, a settled one is none, and a row from an older bridge, or
    /// one over the bound, stays unknown.
    #[test]
    fn listed_rows_report_background_work() {
        run_private("state::dsh::current_read_tests::list_processes");
    }

    /// Runs the ignored fixture `test` in a child process with private state.
    fn run_private(test: &str) {
        let state = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--ignored", "--test-threads=1"])
            .env("HGS_STATE_DIR", state.path())
            .env("ZERUS_DSH_CURRENT_READ_FIXTURE", "1")
            .env_remove("HGS_SESSION")
            .env_remove("HGS_SESSION_ID")
            .env_remove("HGS_RUN_ID")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success() && stdout.contains("test result: ok. 1 passed"),
            "{stdout}{}", String::from_utf8_lossy(&output.stderr));
    }

    const NAME: &str = "dsh/project/current";
    fn record() -> Value {
        json!({"name":NAME,"backend":"dsh","conversation_id":"session-A","run_id":"run-A","agent_home":"/nonexistent/dsh-home"})
    }
    /// One host row: a working session with `questions` pending and the
    /// settled turns `turns` in its outline.
    fn row(turns: &[u64], questions: Option<&[&str]>) -> Value {
        let outline: Vec<Value> = turns.iter().map(|seq| json!({"turn":seq,"seq":seq,"prompt":"prompt","response":format!("reply {seq}")})).collect();
        let mut row = json!({"sessionId":"session-A","agentAvailable":true,"running":true,"updatedAt":1_000_000.0,
            "projections":{"values":{"turnOutline":outline}}});
        if let Some(questions) = questions {
            row["pendingQuestions"] = json!(questions.iter().map(|id| json!({"id":id,"questions":[]})).collect::<Vec<_>>());
        }
        row
    }
    fn list(generation: &str, rows: Vec<Value>) -> Value {
        json!({"generation":generation,"items":rows})
    }
    /// The page the inspection shows, through the newest turn.
    fn page(generation: &str, turns: &[u64], questions: &[&str]) -> Value {
        json!({"generation":generation,"live":true,"baseline":{"asOfSeq":20,"values":{"turnOutline":turns.iter()
            .map(|seq| json!({"turn":seq,"seq":seq,"prompt":"prompt","response":format!("reply {seq}")})).collect::<Vec<_>>()}},
            "page":{"records":[],"hasMore":false},
            "pendingQuestions":questions.iter().map(|id| json!({"id":id,"questions":[]})).collect::<Vec<_>>()})
    }
    type Script = BTreeMap<&'static str, Vec<Result<Value>>>;
    /// Inspects through a scripted host: each method answers from its queue. Reads through the capture seam
    /// are logged as `capture:METHOD`; `captured` answers them, else the shared `answers`.
    fn inspect_with(current_read: bool, agent: Option<&str>, answers: Script, captured: Script) -> (Result<Value>, Vec<String>) {
        let log = std::cell::RefCell::new(Vec::new());
        let answers = std::cell::RefCell::new(answers);
        let captured = std::cell::RefCell::new(captured);
        let answer = |script: &std::cell::RefCell<Script>, method: &str| {
            script.borrow_mut().get_mut(method).filter(|queue| !queue.is_empty()).map(|queue| queue.remove(0))
        };
        let result = inspect_record(NAME, &record(), agent, false, current_read,
            &mut |method, _| {
                log.borrow_mut().push(format!("capture:{method}"));
                answer(&captured, method).or_else(|| answer(&answers, method)).unwrap_or_else(|| Err("not scripted".into()))
            },
            &mut |method, _| {
                log.borrow_mut().push(method.to_owned());
                answer(&answers, method).unwrap_or_else(|| Err("not scripted".into()))
            });
        (result, log.into_inner())
    }
    fn inspect(current_read: bool, agent: Option<&str>, answers: Script) -> (Result<Value>, Vec<String>) {
        inspect_with(current_read, agent, answers, Script::new())
    }

    #[test]
    #[ignore = "run by listed_child_rows_mark_only_person_requests"]
    fn child_rows() {
        if std::env::var_os("ZERUS_DSH_CURRENT_READ_FIXTURE").is_none() {
            return;
        }
        let unasked =
            |method: &str, _: Value| -> Result<Value> { Err(format!("{method} was not expected")) };
        let link = json!({"schema":1,"machine":"11111111-1111-4111-8111-111111111111","host":"arch",
            "session_id":"22222222-2222-4222-8222-222222222222","name":"claude/p/lead",
            "conversation_id":"c","run_id":"r","source":"launch","at":1});
        let mut child = record();
        child["parent"] = link;
        let listed = |child: &Value, row: Value| {
            snapshot_with(child, &list("host-1", vec![row]), &mut |m, p| unasked(m, p))
        };
        // Idle with an unread reply: the parent reads it.
        let mut idle = row(&[3], None);
        idle["running"] = json!(false);
        let view = listed(&child, idle.clone());
        assert_eq!(
            (&view["reply_id"], &view["human_request"]),
            (&json!("dsh-turn-3"), &json!(false)),
            "{view}"
        );
        // The child's own question is the parent's to answer.
        let mut asked = idle.clone();
        asked["pendingQuestions"] = json!([{"id":"q1","questions":[],"approval":false}]);
        let view = listed(&child, asked.clone());
        assert_eq!(
            (&view["phase"], &view["human_request"]),
            (&json!("input"), &json!(false)),
            "{view}"
        );
        // A tool approval and signing in are the person's.
        asked["pendingQuestions"][0]["approval"] = json!(true);
        let view = listed(&child, asked);
        assert_eq!(
            (&view["phase"], &view["human_request"]),
            (&json!("input"), &json!(true)),
            "{view}"
        );
        let mut sign_in = idle.clone();
        sign_in["authRequired"] = json!(true);
        let view = listed(&child, sign_in);
        assert_eq!(
            (&view["activity_summary"], &view["human_request"]),
            (&json!("Sign in required"), &json!(true)),
            "{view}"
        );
        // A row without a parent link carries no field.
        assert!(listed(&record(), idle).get("human_request").is_none());
    }

    #[test]
    #[ignore = "run by listed_rows_report_background_work"]
    fn list_processes() {
        if std::env::var_os("ZERUS_DSH_CURRENT_READ_FIXTURE").is_none() {
            return;
        }
        let unasked =
            |method: &str, _: Value| -> Result<Value> { Err(format!("{method} was not expected")) };
        let listed = |row: Value| {
            snapshot_with(&record(), &list("host-1", vec![row]), &mut |m, p| {
                unasked(m, p)
            })
        };
        let job = |status: &str, owner: &str| json!({"id":"7","kind":"bash","owner":owner,"status":status,"startedAt":1_000_000.0,"label":"npm run dev","callId":"call-1"});
        let mut idle = row(&[3], None);
        idle["running"] = json!(false);
        let with = |jobs: Vec<Value>| {
            let mut row = idle.clone();
            row["listJobs"] = json!({"complete":true,"count":jobs.len(),"jobs":jobs});
            row
        };
        let work = |row: Value| listed(row)["process_work"].clone();
        let running = work(with(vec![job("running", "session-A")]));
        assert_eq!(
            (&running["count"], &running["command"]),
            (&json!(1), &json!("npm run dev")),
            "{running}"
        );
        // A subagent's job is its parent's background work too, as in an inspection.
        assert_eq!(work(with(vec![job("running", "child-1")]))["count"], 1);
        assert_eq!(work(with(vec![]))["count"], 0, "a settled job leaves none");
        assert_eq!(work(with(vec![job("completed", "session-A")]))["count"], 0);
        // An older bridge sends no summary, and an incomplete one (no registry,
        // or over the bound) is no count: both stay unknown.
        assert!(work(idle.clone())["count"].is_null());
        let mut over = idle.clone();
        over["listJobs"] = json!({"complete":false,"count":null,"jobs":[]});
        assert!(work(over)["count"].is_null());
        let mut torn = with(vec![job("running", "session-A")]);
        torn["listJobs"]["count"] = json!(2);
        assert!(
            work(torn)["count"].is_null(),
            "a count that disagrees with its jobs is unknown"
        );
        // A working row reports no count, as before.
        let mut busy = with(vec![job("running", "session-A")]);
        busy["running"] = json!(true);
        assert!(work(busy)["count"].is_null());
    }

    #[test]
    #[ignore = "run by current_read_is_hgs_reads_view_captured_before_the_page"]
    fn fixture() {
        if std::env::var_os("ZERUS_DSH_CURRENT_READ_FIXTURE").is_none() {
            return;
        }
        // Read before the page: turn 3 settled, request q1 pending. Turn 9
        // and request q2 arrive before the page and the inspection's own list.
        let before = list("host-1", vec![row(&[3], Some(&["q1"]))]);
        let after = list("host-1", vec![row(&[3, 9], Some(&["q1", "q2"]))]);
        let script = || BTreeMap::from([
            ("list", vec![Ok(before.clone()), Ok(after.clone())]),
            ("inspect", vec![Ok(page("host-1", &[3, 9], &["q1", "q2"]))]),
        ]);
        let (shown, called) = inspect(true, None, script());
        let shown = shown.unwrap();
        // The capture is the first host read, through its own bounded seam; then the page, then the
        // inspection's own list.
        assert_eq!(called[..3], ["capture:list", "inspect", "list"]);
        assert_eq!(called.iter().filter(|m| m.ends_with("list") || m.ends_with("inspect")).count(), 3);
        let current = &shown["current_read"];
        let view = snapshot_from(&record(), &before);
        assert_eq!(current["reply_id"], "dsh-turn-3");
        assert_eq!(*current, json!({"schema":1,"run_id":"run-A","conversation_id":"session-A","reply_id":"dsh-turn-3",
            "attention_signature":read_marks::signature(&view, Some(&[])),"attention_complete":true}));
        assert!(current["attention_signature"].as_str().is_some_and(|s| s.len() == 64));
        // Parity: `hgs read` on the same snapshot applies both values; on the
        // newer host state neither applies.
        assert_eq!(read_marks::accepts(&view, Some(&[]), current), (true, true));
        assert_eq!(read_marks::accepts(&snapshot_from(&record(), &after), Some(&[]), current), (false, false));
        // The page itself shows the newer turn and both requests.
        assert_eq!(shown["reply_id"], "dsh-turn-9");
        assert_eq!(shown["pending_questions"].as_array().unwrap().len(), 2);

        // Without the flag nothing is captured or added.
        let (plain, called) = inspect(false, None, script());
        assert!(plain.unwrap().get("current_read").is_none());
        assert_eq!(called[..2], ["inspect", "list"]);
        assert_eq!(called.iter().filter(|m| m.ends_with("list")).count(), 1);
        assert!(!called.iter().any(|m| m.starts_with("capture:")));
        // A subagent inspection names nothing.
        let (child, called) = inspect(true, Some("child-1"), script());
        assert!(child.unwrap().get("current_read").is_none());
        assert_eq!(called[0], "inspect");
        assert!(!called.iter().any(|m| m.starts_with("capture:")));

        // Another host generation served the page: nothing is claimed.
        let (changed, _) = inspect(true, None, BTreeMap::from([
            ("list", vec![Ok(list("host-0", vec![row(&[3], Some(&["q1"]))])), Ok(after.clone())]),
            ("inspect", vec![Ok(page("host-1", &[3, 9], &["q1", "q2"]))]),
        ]));
        assert!(changed.unwrap().get("current_read").is_none());
        // A capture the host refused: the inspection still answers, unbound.
        let (refused, _) = inspect(true, None, BTreeMap::from([
            ("list", vec![Err("busy".into()), Ok(after.clone())]),
            ("inspect", vec![Ok(page("host-1", &[3, 9], &["q1", "q2"]))]),
        ]));
        assert!(refused.unwrap().get("current_read").is_none());

        // Unknown requests: a row without its pending requests, or no row.
        for rows in [vec![row(&[3], None)], vec![]] {
            let unknown = list("host-1", rows);
            let (shown, _) = inspect(true, None, BTreeMap::from([
                ("list", vec![Ok(unknown.clone()), Ok(after.clone())]),
                ("inspect", vec![Ok(page("host-1", &[3, 9], &["q1"]))]),
            ]));
            let current = shown.unwrap()["current_read"].clone();
            assert_eq!(current["attention_complete"], false, "{unknown}");
            assert!(!read_marks::accepts(&snapshot_from(&record(), &unknown), Some(&[]), &current).1, "{unknown}");
        }
        // zerus-a4mub.32: an idle row's failure read is part of the capture, through the same bounded seam.
        // When it fails or its deadline passes, the view would miss a failure hgs read may see: nothing is
        // claimed, and the inspection still answers.
        // A successful failure read is cached by its stamp; each case has its own update time.
        let idle_row = |updated: f64| { let mut row = row(&[3], Some(&[])); row["running"] = json!(false); row["updatedAt"] = json!(updated); row };
        let failure_page = json!({"generation":"host-1","page":{"records":[]}});
        for (updated, failure, named) in [(2_000_000.0, Ok(failure_page), true),
            (3_000_000.0, Err("The DeepSeek host did not answer the current read in time".into()), false)] {
            let idle_list = list("host-1", vec![idle_row(updated)]);
            let (shown, called) = inspect_with(true, None,
                BTreeMap::from([("list", vec![Ok(idle_list.clone())]), ("inspect", vec![Ok(page("host-1", &[3], &[]))])]),
                BTreeMap::from([("list", vec![Ok(idle_list.clone())]), ("inspect", vec![failure])]));
            assert_eq!(called[..4], ["capture:list", "capture:inspect", "inspect", "list"]);
            assert_eq!(shown.unwrap().get("current_read").is_some(), named);
        }

        // zerus-a4mub.24 with dsh 0.2.2: `turnOutline` is a `summary: false` projection, so a list row no
        // longer carries it. The idle row's one cached inspection supplies the outline: the capture and
        // `hgs read` name the same settled reply, and a later running row keeps it without another read.
        let summary_row = |running: bool, updated: f64| {
            let mut row = row(&[], Some(&[]));
            row["projections"]["values"]
                .as_object_mut()
                .unwrap()
                .remove("turnOutline");
            row["running"] = json!(running);
            row["updatedAt"] = json!(updated);
            row
        };
        let idle_summary = list("host-1", vec![summary_row(false, 4_000_000.0)]);
        let (shown, called) = inspect_with(
            true,
            None,
            BTreeMap::from([
                ("list", vec![Ok(idle_summary.clone())]),
                ("inspect", vec![Ok(page("host-1", &[3, 7], &[]))]),
            ]),
            BTreeMap::from([
                ("list", vec![Ok(idle_summary.clone())]),
                ("inspect", vec![Ok(page("host-1", &[3, 7], &[]))]),
            ]),
        );
        assert_eq!(
            called[..4],
            ["capture:list", "capture:inspect", "inspect", "list"]
        );
        let shown = shown.unwrap();
        let current = &shown["current_read"];
        assert_eq!(current["reply_id"], "dsh-turn-7", "{shown}");
        assert_eq!(shown["reply_id"], "dsh-turn-7");
        let unasked =
            |method: &str, _: Value| -> Result<Value> { Err(format!("{method} was not expected")) };
        let view = snapshot_with(&record(), &idle_summary, &mut |m, p| unasked(m, p));
        assert_eq!(view["reply_id"], "dsh-turn-7");
        // Idle with nothing pending: the reply applies; a null signature acknowledges nothing.
        assert!(current["attention_signature"].is_null());
        assert_eq!(
            read_marks::accepts(&view, Some(&[]), current),
            (true, false)
        );
        let running = list("host-1", vec![summary_row(true, 5_000_000.0)]);
        assert_eq!(
            snapshot_with(&record(), &running, &mut |m, p| unasked(m, p))["reply_id"],
            "dsh-turn-7"
        );
        // A list that still carries the outline is read as before: its own outline wins.
        let mut carried = row(&[3], Some(&[]));
        carried["running"] = json!(false);
        carried["updatedAt"] = json!(4_000_000.0);
        let carried = list("host-1", vec![carried]);
        assert_eq!(
            snapshot_with(&record(), &carried, &mut |m, p| unasked(m, p))["reply_id"],
            "dsh-turn-3"
        );

        // zerus-a4mub.32 end to end, through the production seams: a host that never answers the capture's
        // list (holding it past the ordinary read timeout) costs about the capture budget, and the inspection
        // still answers, without current_read.
        let mut bound = record();
        bound["session_id"] = json!("11111111-1111-4111-8111-111111111111");
        bound["cwd"] = json!("/nonexistent/workspace");
        bound["created"] = json!(1.0);
        save(&mut bound).unwrap();
        let socket = host_dir(&bound).join("host.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let host = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let served = page("host-1", &[3], &[]);
        let listed = list("host-1", vec![row(&[3], Some(&[]))]);
        std::thread::spawn(move || {
            let mut lists = 0;
            for stream in host.incoming().flatten() {
                let mut line = String::new();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                if reader.read_line(&mut line).is_err() { continue; }
                let method = serde_json::from_str::<Value>(&line).map(|request| string(&request, "method").to_owned()).unwrap_or_default();
                let value = match method.as_str() {
                    "list" => { lists += 1; if lists == 1 { std::thread::spawn(move || { std::thread::sleep(Duration::from_secs(45)); drop(stream) }); continue; } listed.clone() }
                    "inspect" => served.clone(),
                    _ => { let _ = writeln!(&stream, "{}", json!({"ok":false,"error":"not scripted"})); continue; }
                };
                let _ = writeln!(&stream, "{}", json!({"ok":true,"value":value}));
            }
        });
        let started = Instant::now();
        let shown = inspect_with_processes(NAME, None, false, true).unwrap();
        let elapsed = started.elapsed();
        assert!(shown.get("current_read").is_none());
        assert_eq!(shown["reply_id"], "dsh-turn-3");
        assert!(elapsed >= CAPTURE_BUDGET - Duration::from_millis(500) && elapsed < CAPTURE_BUDGET + Duration::from_secs(5), "{elapsed:?}");

        // No request pending: nothing to sign, and the reply still applies.
        let idle = list("host-1", vec![row(&[3], Some(&[]))]);
        let (shown, _) = inspect(true, None, BTreeMap::from([
            ("list", vec![Ok(idle.clone()), Ok(idle.clone())]),
            ("inspect", vec![Ok(page("host-1", &[3], &[]))]),
        ]));
        let current = shown.unwrap()["current_read"].clone();
        assert_eq!((current["attention_signature"].clone(), current["attention_complete"].clone()), (Value::Null, json!(true)));
        assert_eq!(read_marks::accepts(&snapshot_from(&record(), &idle), Some(&[]), &current), (true, false));
    }
}

#[cfg(all(test, unix))]
mod current_read_bounds {
    use super::*;

    /// zerus-a4mub.32: the capture's host reads share one short deadline; a stalled host costs at most about that.
    #[test]
    fn a_capture_read_ends_by_its_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        // A host that accepts and never answers.
        let held = std::thread::spawn(move || listener.accept().map(|(stream, _)| { std::thread::sleep(Duration::from_secs(4)); drop(stream) }));
        let started = Instant::now();
        assert!(rpc_by(&path, "list", json!({}), started + Duration::from_millis(300)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        // An expired deadline never reaches the host.
        assert_eq!(rpc_by(&dir.path().join("absent.sock"), "list", json!({}), Instant::now()).unwrap_err(), CAPTURE_EXPIRED);
        assert!(CAPTURE_BUDGET <= Duration::from_secs(7));
        held.join().unwrap().unwrap();
    }

    /// zerus-a4mub.33: DeepSeek has no archive inspection; the arm refuses it rather than inspecting the live session.
    #[test]
    fn inspect_refuses_an_archive_and_reads_its_options() {
        let args = |list: &[&str]| list.iter().map(|value| (*value).to_owned()).collect::<Vec<_>>();
        for refused in [args(&["dsh/project/a", "--archive", "11111111-1111-4111-8111-111111111111"]),
            args(&["dsh/project/a", "--current-read", "--archive", "11111111-1111-4111-8111-111111111111"])] {
            assert_eq!(inspect_options(&refused).unwrap_err(), "DeepSeek archives are unavailable");
        }
        assert_eq!(inspect_options(&args(&["dsh/project/a"])).unwrap(), (None, true, false));
        assert_eq!(inspect_options(&args(&["dsh/project/a", "--skip-processes", "--current-read"])).unwrap(), (None, false, true));
        assert_eq!(inspect_options(&args(&["dsh/project/a", "--agent", "child-1"])).unwrap(), (Some("child-1"), true, false));
    }
}

#[cfg(all(test, unix))]
mod provider_failure_tests {
    use super::*;
    #[test]
    fn account_defaults_reach_only_the_created_generation_once() {
        type Calls = Vec<(String, Value)>;
        let run = |answer: &dyn Fn(&str) -> Result<Value>, persist_ok: bool| {
            let mut record = json!({"conversation_id":"session-fixture"});
            let (mut calls, mut saved): (Calls, Vec<Value>) = (Vec::new(), Vec::new());
            let warning = apply_account_defaults_with(
                &mut record,
                (&json!("generation-a"), "bypass"),
                "route/model",
                "max",
                |method, params| {
                    calls.push((method.into(), params));
                    answer(method)
                },
                |r| {
                    saved.push(r["account_defaults"]["status"].clone());
                    if persist_ok { Ok(()) } else { Err("read-only binding".into()) }
                },
            );
            (record, calls, saved, warning)
        };
        let selected = json!({"provider":"route","model":"model","reasoningEffort":"max"});
        let applied = |method: &str| Ok(if method == "ping" { json!({"generation":"generation-a"}) }
            else { json!({"scope":"session","selected":selected}) });
        // Pending is durable before the one scoped settings call on the same generation.
        let (record, calls, saved, warning) = run(&applied, true);
        assert!(warning.is_none());
        assert_eq!(saved, [json!("pending"), json!("applied")]);
        assert_eq!(calls.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["ping", "settings"]);
        assert_eq!(calls[1].1, json!({"sessionId":"session-fixture","provider":"route","model":"model",
            "generation":"generation-a","permissionMode":"bypass","reasoningEffort":"max"}));
        assert_eq!(record["account_defaults"]["selected"], selected);
        // A replaced host never receives settings: that would resume the session.
        let moved = |_: &str| Ok(json!({"generation":"generation-b"}));
        let (record, calls, saved, warning) = run(&moved, true);
        assert_eq!(calls.len(), 1);
        assert_eq!(saved, [json!("pending"), json!("failed")]);
        assert!(warning.unwrap().contains("not applied"));
        assert!(record["account_defaults"]["detail"].as_str().unwrap().contains("host changed"));
        // A refusal is definite; a lost reply may have applied. Neither retries.
        for (error, status) in [
            ("This model is unavailable for the native account", "failed"),
            (HOST_UNAVAILABLE, "failed"),
            ("Native request delivery uncertain: timed out", "uncertain"),
        ] {
            let answer = |method: &str| if method == "ping" { Ok(json!({"generation":"generation-a"})) } else { Err(error.to_owned()) };
            let (record, calls, saved, warning) = run(&answer, true);
            assert_eq!(calls.len(), 2, "{error}");
            assert_eq!(saved, [json!("pending"), json!(status)]);
            assert_eq!(record["account_defaults"]["status"], status);
            assert!(warning.is_some());
        }
        // Without a durable pending record nothing is sent.
        let (_, calls, saved, warning) = run(&applied, false);
        assert!(calls.is_empty());
        assert_eq!(saved, [json!("pending")]);
        assert!(warning.unwrap().contains("not applied"));
    }

    #[test]
    fn account_catalog_lists_native_routes_and_the_native_default() {
        let catalog = json!({"default":{"provider":"deepseek-official","model":"deepseek-flash"},
            "groups":[{"id":"deepseek-official","name":"DeepSeek","models":[{"id":"deepseek-flash","name":"DeepSeek-V4.1-Flash",
            "reasoning":{"efforts":[{"id":"off"},{"id":"low"},{"id":"high"},{"id":"max"}],"defaultEffort":"high"}}]}]});
        let models = account_models_from(&catalog);
        assert_eq!(models["models"], json!([{"id":"deepseek-official/deepseek-flash","label":"DeepSeek-V4.1-Flash (DeepSeek)",
            "effort_options":["off","low","high","max"]}]));
        assert_eq!(models["provider_defaults"], json!({"model":"deepseek-official/deepseek-flash","effort":""}));
        let empty = account_models_from(&Value::Null);
        assert_eq!(empty["models"], json!([]));
        assert_eq!(empty["provider_defaults"], json!({"model":"","effort":""}));
    }

    #[test]
    fn child_summaries_require_own_identity_and_one_host_generation() {
        let record=json!({"conversation_id":"parent"});
        let mut value=json!({"subagents":{"active":{"state":"unknown"},"idle":{"state":"unknown"},"foreign":{"state":"unknown"}}});
        let list=json!({"generation":"host-one","items":[
            {"sessionId":"active","parentSessionId":"parent","origin":"subagent","running":true,"agentAvailable":true},
            {"sessionId":"idle","parentSessionId":"parent","origin":"subagent","running":false,"agentAvailable":false},
            {"sessionId":"foreign","parentSessionId":"other","origin":"subagent","running":false,"agentAvailable":true}]});
        child_summaries(&record,&mut value,&list,&json!("host-one"));
        assert_eq!(value["subagents"]["active"]["state"],"working");
        assert_eq!(value["subagents"]["idle"]["state"],"finished");
        assert_eq!(value["subagents"]["foreign"]["state"],"unknown");
        assert_eq!(value["subagent_active_count"],1);assert_eq!(value["subagent_counts_complete"],false);
        let mut stale=json!({"subagents":{"active":{"state":"unknown"}}});let before=stale.clone();
        child_summaries(&record,&mut stale,&list,&json!("host-two"));assert_eq!(stale,before);
    }
    #[test]
    fn failed_native_turn_needs_attention_without_confusing_tool_results() {
        let record =
            json!({"name":"dsh/test/session","conversation_id":"session-one","run_id":"run"});
        let row = json!({"agentAvailable":true,"running":false});
        let failed = json!({"event":{"type":"turn/end","seq":12,"time":100000,"data":{"reason":{"kind":"error","error":{"code":"insufficient_quota","message":"Insufficient balance"}}}}});
        let mut records = json!([{"event":{"type":"tool/result","seq":11,"data":{"message":{"content":"model at capacity"}}}},failed]);
        let normalized = normalize(&record, &row, &json!({"page":{"records":records}}));
        assert_eq!(normalized["phase"], "error");
        assert_eq!(normalized["activity_summary"], "Usage limit reached");
        assert_eq!(normalized["attention_id"], "dsh_native:dsh-error-12");
        assert_eq!(
            native_events(&records).last().unwrap()["type"],
            "StopFailure"
        );
        records
            .as_array_mut()
            .unwrap()
            .push(json!({"event":{"type":"turn/start","seq":13}}));
        assert!(latest_failure(&records).is_null());
        assert_eq!(
            normalize(&record, &row, &json!({"page":{"records":records}}))["phase"],
            "idle"
        );
        records.as_array_mut().unwrap().pop();
        let mut working = row.clone();
        working["running"] = json!(true);
        assert_eq!(
            normalize(&record, &working, &json!({"page":{"records":records}}))["phase"],
            "working"
        );
        let mut stopped = row.clone();
        stopped["agentAvailable"] = json!(false);
        assert_ne!(
            normalize(&record, &stopped, &json!({"page":{"records":records}}))["phase"],
            "error"
        );
        records[1]["event"]["data"]["reason"] = json!({"kind":"completed"});
        assert!(latest_failure(&records).is_null());
        assert_eq!(native_events(&records).last().unwrap()["type"], "Stop");
        records[1]["event"]["data"]["reason"] = json!({"kind":"max-tokens"});
        assert_eq!(latest_failure(&records)["error_kind"], "context_limit");
    }
}

#[cfg(all(test, unix))]
mod native_operation_tests {
    use super::*;
    #[test]
    fn fork_needs_one_advertising_host_generation_and_a_completed_reply() {
        let record = json!({"name":"dsh/test/session"});
        let ping = json!({"generation":"host","nativeOperations":["fork","history-search","schedules"]});
        let ready = json!({"generation":"host","forkSupported":true,"forkReady":true});
        assert_eq!(fork_reason(&record, &ping, &ready), "");
        assert!(fork_reason(&record, &json!({"generation":"host","sessionActions":true}), &ready).contains("older adapter"));
        let mut other = ready.clone();
        other["generation"] = json!("replacement");
        assert!(fork_reason(&record, &ping, &other).contains("host changed"));
        let mut empty = ready.clone();
        empty["forkReady"] = json!(false);
        assert_eq!(fork_reason(&record, &ping, &empty), "Wait for a completed reply before forking");
        let pending = json!({"permissions_pending":true});
        assert!(fork_reason(&pending, &ping, &ready).contains("permission setup"));
    }
    #[test]
    fn schedule_items_keep_the_exact_native_record_for_compare_and_set() {
        let entry = json!({"id":"schedule-1","kind":"cron","title":"Deploy","prompt":"Check","expression":"0 9 * * 1-5",
            "timeZone":"UTC","scheduledAt":"2026-10-16T09:00:00.000Z","sessionId":"session-a","status":"active",
            "lastDelivery":{"messageId":"m"}});
        let item = schedule_item(&entry);
        assert_eq!(item["record"], json!({"id":"schedule-1","kind":"cron","title":"Deploy","prompt":"Check",
            "expression":"0 9 * * 1-5","timeZone":"UTC","scheduledAt":"2026-10-16T09:00:00.000Z"}));
        assert_eq!((item["recurring"].as_bool(), item["scheduled_at"].as_f64()), (Some(true), Some(1792141200.0)));
        assert_eq!(item["last_delivery"]["messageId"], "m");
        let modern = json!({"id":"schedule-2","kind":"rrule","title":"Check","prompt":"Check",
            "rrule":"DTSTART:20261105T200000Z\nRRULE:FREQ=HOURLY","scheduledAt":"2026-11-05T20:00:00.000Z",
            "target":{"kind":"session","sessionId":"original"},"involvedSessionIds":["editor","original"],
            "updatedAt":"2026-11-05T18:00:00.001Z","status":"active"});
        let item = schedule_item(&modern);
        assert_eq!(item["recurring"], true);
        let mut expected = modern;
        expected.as_object_mut().unwrap().remove("status");
        assert_eq!(item["record"], expected);
    }
}

#[cfg(all(test, unix))]
mod question_identity_tests {
    use super::*;
    /// The approval card built from `src/dsh/bridge.mjs`'s published
    /// approval names its tool for automatic-approval categories.
    #[test]
    fn automatic_approval_categories_read_the_bridge_approval_card() {
        use zerus_client_core::approvals::automatic::{categories, once, Category};
        let record = json!({"name":"dsh/test/session","conversation_id":"session-one","run_id":"run"});
        let card = |tool: &str| {
            let inspected = json!({"generation":"g1","pendingQuestions":[{"id":"q1","approval":true,"questions":[
                {"id":"decision","header":"Approval required","question":format!("Allow {tool}?"),"detail":"",
                 "options":[{"label":"Allow once"},{"label":"Reject"}]}]}]});
            question_cards(&record, &inspected).remove(0)
        };
        assert!(once("dsh", &card("bash")).is_some());
        for (tool, expected) in [("bash", Category::Shell), ("web_search", Category::Paid),
            ("mcp__docs__lookup", Category::Mcp), ("create_worktree", Category::Other)] {
            assert_eq!(categories::tool("dsh", &card(tool)).category, expected, "{tool}");
        }
    }
    #[test]
    fn pending_native_questions_have_an_exact_attention_identity() {
        let record = json!({"name":"dsh/test/session","conversation_id":"session-one","run_id":"run"});
        let row = json!({"agentAvailable":true,"running":false});
        let first = normalize(&record, &row, &json!({"pendingQuestions":[{"id":"question-1"}]}));
        assert_eq!(first["phase"], "input");
        assert_eq!(first["attention_id"], "dsh_question:question-1");
        let next = normalize(&record, &row, &json!({"pendingQuestions":[{"id":"question-2"}]}));
        assert_ne!(next["attention_id"], first["attention_id"]);
        assert!(normalize(&record, &row, &json!({}))["attention_id"].is_null());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn each_host_directory_has_its_own_named_pipe() {
        let first = endpoint(Path::new(r"C:\Users\fixture\.local\state\hgs\dsh\hosts\one"));
        let second = endpoint(Path::new(r"C:\Users\fixture\.local\state\hgs\dsh\hosts\two"));
        assert!(first.to_string_lossy().starts_with(r"\\.\pipe\hgs-dsh-"), "{}", first.display());
        assert_ne!(first, second);
        assert_eq!(first, endpoint(Path::new(r"C:\Users\fixture\.local\state\hgs\dsh\hosts\one")));
    }

    #[test]
    fn a_missing_host_is_unavailable() {
        let pipe = endpoint(Path::new(&format!(r"C:\zerus-test\{}", uuid::Uuid::new_v4())));
        let error = exchange(&pipe, "{}", Duration::from_secs(1)).unwrap_err();
        assert!(error.contains("unavailable"), "{error}");
    }

    /// The test's Node server, killed however the test ends: a failed exchange
    /// (a sandbox that cannot open the pipe) used to leave it listening forever.
    struct Server(std::process::Child);

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    // The bridge answers one request line with one response line, as Node serves it.
    #[test]
    fn a_request_line_gets_the_bridge_response_line() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let pipe = endpoint(Path::new(&format!(r"C:\zerus-test\{}", uuid::Uuid::new_v4())));
        // The server also exits on its own after 30 s, should the test process die.
        let script = format!(
            "setTimeout(()=>process.exit(2),30000).unref();const net=require('net');const s=net.createServer(c=>c.on('data',d=>{{c.end(JSON.stringify({{ok:true,value:JSON.parse(d)}})+'\\n');s.close();}}));s.listen({:?},()=>console.log('ready'));",
            pipe.to_string_lossy()
        );
        let mut node = Server(Command::new("node").args(["-e", &script]).stdout(Stdio::piped()).spawn().unwrap());
        let mut ready = String::new();
        BufReader::new(node.0.stdout.take().unwrap()).read_line(&mut ready).unwrap();
        assert_eq!(ready.trim(), "ready");
        let response = exchange(&pipe, r#"{"method":"ping"}"#, Duration::from_secs(10)).unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["value"]["method"], "ping");
        assert!(node.0.wait().unwrap().success());
    }

    /// The owner, 2026-11-01: a DeepSeek session in a folder that inherits
    /// Modify only (a second drive) starts: hgs grants this user the rights
    /// the sandbox needs instead of failing with the icacls line.
    #[test]
    fn a_modify_only_folder_gets_the_sandbox_rights_before_the_start() {
        let dir = tempfile::Builder::new().prefix("hgs-acl-").tempdir_in(".").unwrap();
        let folder = dir.path().join("project");
        std::fs::create_dir(&folder).unwrap();
        crate::platform::set_modify_only(&folder).unwrap();
        if crate::platform::missing_sandbox_rights(&folder).unwrap().is_empty() {
            eprintln!("this account keeps full rights on a Modify-only folder; skipped");
            return;
        }
        ensure_sandbox_folder(&folder).unwrap();
        assert!(crate::platform::sandbox_folder_problem(&folder).unwrap().is_none());
    }
}

#[cfg(test)]
mod permission_catalog_tests {
    use super::*;

    #[test]
    fn permission_selections_decode_and_canonicalize() {
        assert_eq!(decode_permission_selection("").unwrap(), PermissionSelection::Provider);
        assert_eq!(decode_permission_selection("provider").unwrap(), PermissionSelection::Provider);
        assert_eq!(decode_permission_selection("bypass").unwrap(), PermissionSelection::Bypass);
        assert!(decode_permission_selection("auto").is_err());
        assert_eq!(decode_permission_selection("native:{}").unwrap(), PermissionSelection::Provider);
        assert_eq!(decode_permission_selection("native:{\"preset\":\"\"}").unwrap(), PermissionSelection::Provider);
        assert_eq!(
            decode_permission_selection("native:{\"preset\":\"Review workspace\"}").unwrap(),
            PermissionSelection::Preset("Review workspace".into())
        );
        assert_eq!(
            decode_permission_selection("workspace-write").unwrap(),
            PermissionSelection::Preset("workspace-write".into())
        );
        assert_eq!(
            PermissionSelection::Preset("Review workspace".into()).canonical(),
            "native:{\"preset\":\"Review workspace\"}"
        );
        for bad in [
            "native:[]",
            "native:{\"other\":\"x\"}",
            "native:{\"preset\":1}",
            "native:{\"preset\":\" Review\"}",
            "native:not-json",
            "two words\n",
        ] {
            assert!(decode_permission_selection(bad).is_err(), "{bad:?}");
        }
        let oversized = format!("native:{{\"preset\":\"{}\"}}", "x".repeat(1025));
        assert!(decode_permission_selection(&oversized).is_err());
    }

    #[test]
    fn custom_native_preset_names_are_preserved_as_raw_option_values() {
        let options = json!([
            {"value":"read-only","name":"Read only"},
            {"value":"Review workspace","name":"Review workspace","description":"Ask before writing"},
            {"value":"Проверка (strict)!","name":"Проверка"}
        ]);
        let presets = presets_from_options(&options).unwrap();
        assert_eq!(presets[1].value, "Review workspace");
        assert_eq!(presets[2].value, "Проверка (strict)!");
        let catalog = available_catalog(
            &presets,
            Some("Review workspace".into()),
            Some("0.2.1-rc.2".into()),
            "dsh-profile",
        );
        assert_eq!(catalog["controls"][0]["id"], "preset");
        assert_eq!(catalog["controls"][0]["options"][1]["value"], "Review workspace");
        assert_eq!(catalog["controls"][0]["default"], "Review workspace");
        assert_eq!(catalog["selected"], json!({"preset":"Review workspace"}));
        assert_eq!(catalog["session_supported"], true);
        // Provider and Bypass stay separate shortcuts, never native options.
        assert!(!catalog["controls"][0]["options"].as_array().unwrap().iter()
            .any(|option| option["value"] == "provider" || option["value"] == "bypass"));
    }

    #[test]
    fn unavailable_catalog_synthesizes_no_native_options() {
        let catalog = unavailable_catalog("The DeepSeek host did not answer", None);
        assert_eq!(catalog["status"], "unavailable");
        assert_eq!(catalog["controls"], json!([]));
        assert_eq!(catalog["selected"], json!({}));
        assert!(catalog["status_reason"].as_str().unwrap().contains("DeepSeek"));
    }

    #[test]
    fn preset_values_are_bounded_and_trimmed() {
        for value in ["", " leading", "trailing ", "line\nbreak", &"x".repeat(129)] {
            assert!(!valid_preset_name(value), "{value:?}");
        }
        assert!(valid_preset_name("Review workspace"));
        assert!(valid_preset_name("Проверка (strict)!"));
        assert!(valid_preset_name("a.b_c-d"));
    }

    #[test]
    fn too_many_presets_are_an_explicit_error_not_a_truncation() {
        let options = json!((0..65).map(|index| json!({"value":format!("preset-{index}")})).collect::<Vec<_>>());
        assert!(presets_from_options(&options).is_err());
    }

    #[test]
    fn display_metadata_is_bounded_without_dropping_choices() {
        assert_eq!(display_text(" short ", 20), "short");
        assert_eq!(display_text("one\ntwo", 20), "onetwo");
        let clipped = display_text(&"x".repeat(20), 10);
        assert_eq!(clipped.chars().count(), 10);
        assert!(clipped.ends_with("..."));
    }
}

#[cfg(test)]
mod permissions_compat_tests {
    use super::*;
    use std::io::{BufRead, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    const TEST_SESSION: &str = "session-00000000-0000-4000-8000-000000000000";

    #[derive(Clone)]
    struct CompatState {
        catalog: Vec<(String, String)>,
        current: String,
        running: bool,
        available: bool,
        list_calls: usize,
        projection_calls: usize,
        execute_calls: usize,
        executed: Vec<String>,
    }

    impl Default for CompatState {
        fn default() -> Self {
            Self {
                catalog: vec![("danger-full-access".into(), "Full access".into())],
                current: "workspace-write".into(),
                running: false,
                available: true,
                list_calls: 0,
                projection_calls: 0,
                execute_calls: 0,
                executed: Vec::new(),
            }
        }
    }

    fn node_available() -> bool {
        Command::new("node").arg("--version").output().is_ok()
    }

    fn write_compat_response(
        socket: &mut TcpStream,
        status: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str("\r\n");
        response.push_str(body);
        let _ = socket.write_all(response.as_bytes());
        let _ = socket.flush();
    }

    fn handle_compat_request(socket: &mut TcpStream, state: &Arc<Mutex<CompatState>>) {
        let Ok(clone) = socket.try_clone() else { return };
        let mut reader = BufReader::new(clone);
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).is_err() {
            return;
        }
        let mut content_length = 0usize;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).is_err() {
                return;
            }
            if header == "\r\n" || header == "\n" || header.is_empty() {
                break;
            }
            let lower = header.to_ascii_lowercase();
            if let Some(value) = lower.strip_prefix("content-length:") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; content_length];
        if content_length > 0 && reader.read_exact(&mut body).is_err() {
            return;
        }
        let method = request_line.split_whitespace().next().unwrap_or("");
        let path = request_line.split_whitespace().nth(1).unwrap_or("");
        if method == "GET" && path.starts_with("/?token=") {
            write_compat_response(
                socket,
                "303 See Other",
                &[("Location", "/"), ("Set-Cookie", "hgs=test; Path=/")],
                "",
            );
            return;
        }
        let value = {
            let mut state = state.lock().unwrap();
            match path {
                "/api/permissionPresets/catalog" => json!({
                    "defaultOptions": state.catalog.iter()
                        .map(|(value, label)| json!({"value":value,"name":label}))
                        .collect::<Vec<_>>(),
                    "defaultPreset": "workspace-write"
                }),
                "/api/session/projections" => {
                    state.projection_calls += 1;
                    json!({"values":{"permissions":{"currentValue":state.current}}})
                }
                "/api/session/list" => {
                    state.list_calls += 1;
                    json!({"items":[{"sessionId":TEST_SESSION,"agentAvailable":state.available,"running":state.running}]})
                }
                "/api/commands/list" => {
                    json!([{"name":"permission","definitionId":"@deepseek-ai/dsh-permission-presets"}])
                }
                "/api/commands/execute" => {
                    state.execute_calls += 1;
                    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let line = request["payload"]["args"]["line"].as_str().unwrap_or("").to_owned();
                    state.executed.push(line.clone());
                    if let Some(target) = line.strip_prefix("/permission ") {
                        state.current = target.to_owned();
                    }
                    json!({"result":{"kind":"success"}})
                }
                _ => Value::Null,
            }
        };
        let payload = json!({"result":{"ok":true,"value":value}}).to_string();
        write_compat_response(
            socket,
            "200 OK",
            &[("Content-Type", "application/json")],
            &payload,
        );
    }

    fn compat_server(
        state: CompatState,
    ) -> (String, Arc<Mutex<CompatState>>, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let shared = Arc::new(Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let server_state = shared.clone();
        let server_stop = stop.clone();
        let handle = std::thread::spawn(move || {
            while !server_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        // Windows accepted sockets inherit the listener's
                        // nonblocking mode; the fixture must block for headers.
                        let _ = socket.set_nonblocking(false);
                        handle_compat_request(&mut socket, &server_state);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    // Windows accept can report a client reset; the fixture
                    // must keep serving later requests instead of exiting.
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        (
            format!("http://{address}/?token=test"),
            shared,
            stop,
            handle,
        )
    }

    fn stop_server(stop: &AtomicBool, server: std::thread::JoinHandle<()>) {
        stop.store(true, Ordering::Relaxed);
        let _ = server.join();
    }

    #[test]
    fn legacy_bypass_check_and_apply_work_without_a_preset() {
        if !node_available() {
            return;
        }
        let (url, shared, stop, server) = compat_server(CompatState::default());
        let checked = permissions_compat_json(&url, Some(TEST_SESSION), "check", None, false, false).unwrap();
        assert_eq!(checked["needs_setup"], json!(false));
        let applied = permissions_compat_json(&url, Some(TEST_SESSION), "apply", None, false, false).unwrap();
        assert_eq!(applied["ok"], json!(true));
        let state = shared.lock().unwrap();
        assert_eq!(state.executed, vec!["/permission danger-full-access".to_owned()]);
        assert_eq!(state.execute_calls, 1);
        drop(state);
        stop_server(&stop, server);
    }

    #[test]
    fn preset_apply_keeps_custom_names_and_never_replays_a_confirmed_change() {
        if !node_available() {
            return;
        }
        let mut initial = CompatState::default();
        initial.catalog.push(("Review workspace".into(), "Review workspace".into()));
        initial.current = "read-only".into();
        let (url, shared, stop, server) = compat_server(initial);
        let checked = permissions_compat_json(
            &url,
            Some(TEST_SESSION),
            "check-preset",
            Some("Review workspace"),
            false,
            true,
        )
        .unwrap();
        assert_eq!(checked["needs_setup"], json!(false));
        permissions_compat_json(
            &url,
            Some(TEST_SESSION),
            "apply-preset",
            Some("Review workspace"),
            false,
            true,
        )
        .unwrap();
        // The durable projection already holds the preset; a second pass reads
        // it and executes nothing.
        permissions_compat_json(
            &url,
            Some(TEST_SESSION),
            "apply-preset",
            Some("Review workspace"),
            false,
            true,
        )
        .unwrap();
        let state = shared.lock().unwrap();
        assert_eq!(state.executed, vec!["/permission Review workspace".to_owned()]);
        assert_eq!(state.execute_calls, 1);
        assert_eq!(state.list_calls, 2);
        assert!(state.projection_calls >= 3);
        drop(state);
        stop_server(&stop, server);
    }

    #[test]
    fn a_running_session_refuses_a_preset_change() {
        if !node_available() {
            return;
        }
        let mut initial = CompatState::default();
        initial.catalog.push(("Review workspace".into(), "Review workspace".into()));
        initial.current = "read-only".into();
        initial.running = true;
        let (url, shared, stop, server) = compat_server(initial);
        let error = permissions_compat_json(
            &url,
            Some(TEST_SESSION),
            "apply-preset",
            Some("Review workspace"),
            false,
            true,
        )
        .unwrap_err();
        assert_eq!(error, "busy");
        let state = shared.lock().unwrap();
        assert_eq!(state.execute_calls, 0);
        drop(state);
        stop_server(&stop, server);
    }

    #[test]
    fn an_implicit_default_preserves_a_running_session() {
        if !node_available() {
            return;
        }
        let mut initial = CompatState::default();
        initial.catalog.push(("Review workspace".into(), "Review workspace".into()));
        initial.current = "read-only".into();
        initial.running = true;
        let (url, shared, stop, server) = compat_server(initial);
        let applied = permissions_compat_json(
            &url,
            Some(TEST_SESSION),
            "apply-preset",
            Some("Review workspace"),
            false,
            false,
        )
        .unwrap();
        assert_eq!(applied["skipped"], json!(true));
        let state = shared.lock().unwrap();
        assert_eq!(state.execute_calls, 0);
        assert_eq!(state.list_calls, 1);
        drop(state);
        stop_server(&stop, server);
    }

    #[test]
    fn an_already_applied_target_skips_the_liveness_lookup() {
        if !node_available() {
            return;
        }
        let initial = CompatState { current: "danger-full-access".into(), running: true, ..Default::default() };
        let (url, shared, stop, server) = compat_server(initial);
        let applied = permissions_compat_json(&url, Some(TEST_SESSION), "apply", None, false, false).unwrap();
        assert_eq!(applied["ok"], json!(true));
        let state = shared.lock().unwrap();
        assert_eq!(state.list_calls, 0);
        assert_eq!(state.execute_calls, 0);
        drop(state);
        stop_server(&stop, server);
    }

    #[test]
    fn unknown_presets_and_oversized_catalogs_fail_explicitly() {
        if !node_available() {
            return;
        }
        let initial = CompatState {
            catalog: (0..65).map(|index| (format!("preset-{index}"), format!("Preset {index}"))).collect(),
            ..Default::default()
        };
        let (url, _, stop, server) = compat_server(initial);
        assert_eq!(
            permissions_compat_json(&url, Some(TEST_SESSION), "catalog", None, false, false).unwrap_err(),
            "oversized"
        );
        stop_server(&stop, server);
        let (url, _, stop, server) = compat_server(CompatState::default());
        assert_eq!(
            permissions_compat_json(&url, Some(TEST_SESSION), "apply-preset", Some("gone"), false, true)
                .unwrap_err(),
            "preset"
        );
        stop_server(&stop, server);
    }

    #[test]
    fn bridge_gate_never_switches_an_ordinary_live_ensure() {
        if !node_available() {
            return;
        }
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/dsh/bridge.mjs");
        let url = format!("file:///{}", path.to_string_lossy().replace('\\', "/"));
        let script = "const m=await import(process.env.HGS_BRIDGE_URL);process.stdout.write(JSON.stringify({\
            ordinary:m.presetGate({preset:'x',live:true,recovery:false,running:false,explicit:false,current:'y'}),\
            explicitLive:m.presetGate({preset:'x',live:true,recovery:false,running:false,explicit:true,current:'y'}),\
            same:m.presetGate({preset:'x',live:true,recovery:false,running:false,explicit:true,current:'x'}),\
            recovery:m.presetGate({preset:'x',live:true,recovery:true,running:false,explicit:false,current:'y'}),\
            busy:m.presetGate({preset:'x',live:true,recovery:true,running:true,explicit:false,current:'y'}),\
            fresh:m.presetGate({preset:'x',live:false,recovery:false,running:false,explicit:false,current:null}),\
            none:m.presetGate({preset:undefined,live:true,recovery:true,running:false,explicit:false,current:'y'})}));";
        let output = Command::new("node")
            .args(["--input-type=module", "-e", script])
            .env("HGS_BRIDGE_URL", url)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["ordinary"]["applied"], json!(true));
        assert_eq!(value["ordinary"]["apply"], json!(false));
        assert_eq!(value["explicitLive"]["applied"], json!(false));
        assert_eq!(value["same"]["applied"], json!(true));
        assert_eq!(value["same"]["apply"], json!(false));
        assert_eq!(value["recovery"]["apply"], json!(true));
        assert_eq!(value["busy"]["refuse"], json!(true));
        assert_eq!(value["fresh"]["apply"], json!(true));
        assert_eq!(value["none"]["apply"], json!(false));
    }

    #[test]
    fn a_helper_that_closes_stdout_then_sleeps_is_bounded_and_orphan_free() {
        if !node_available() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("helper.pid");
        // The helper closes its output, starts a grandchild, then keeps running.
        // The maintained runner must end at its deadline and kill the group.
        let script = "const fs=require('fs'),{spawn}=require('child_process');\
            const g=spawn(process.execPath,['-e','setTimeout(()=>{},60000)'],{stdio:'ignore'});\
            fs.writeFileSync(process.argv[1],String(process.pid)+' '+String(g.pid));\
            try{fs.closeSync(1)}catch{};setTimeout(()=>{},60000);";
        let mut command = Command::new("node");
        command.args(["-e", script, pid_path.to_string_lossy().as_ref()]);
        let started = Instant::now();
        let error = bounded_output(command, None, 1024, Duration::from_millis(2000)).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        // The helper sleeps 60 s: well under that is bounded, also when a
        // parallel test load slows every process start.
        assert!(started.elapsed() < Duration::from_secs(30));
        // Under that load Node may not reach its first line before the
        // deadline: then nothing was started that could be left running.
        let Ok(pids) = std::fs::read_to_string(&pid_path) else { return };
        let pids: Vec<u32> = pids
            .split_whitespace()
            .filter_map(|pid| pid.parse().ok())
            .collect();
        assert_eq!(pids.len(), 2);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline
            && pids.iter().any(|pid| crate::platform::process_alive(*pid))
        {
            std::thread::sleep(Duration::from_millis(25));
        }
        for pid in pids {
            assert!(!crate::platform::process_alive(pid), "helper {pid} was left running");
        }
    }

    #[test]
    fn a_helper_that_never_reads_its_bounded_input_stays_bounded() {
        if !node_available() {
            return;
        }
        let mut command = Command::new("node");
        command.args(["-e", "setTimeout(()=>{},60000)"]);
        let input = vec![b'x'; 512 * 1024];
        let started = Instant::now();
        let error = bounded_output(command, Some(&input), 1024, Duration::from_millis(300))
            .unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        // The helper sleeps 60 s; a parallel test load slows the kill.
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn composed_preset_helper_preserves_custom_names() {
        if !node_available() {
            return;
        }
        let Some(root) = dsh_package_root() else { return };
        let dump = "- id: permission\n  name: '@deepseek-ai/dsh-permission-presets'\n  config:\n    presets:\n      read-only:\n        sandbox: read-only\n        approval: ask\n      Review workspace:\n        sandbox: workspace-write\n        approval: ask\n        description: Ask before writing\n      \"Проверка (strict)!\":\n        sandbox: danger-full-access\n        approval: never\n    defaultPreset: Review workspace\n- id: other\n  name: '@deepseek-ai/dsh-plugin-manager'\n  disabled: !!js '!ctx.get(''profileContext'')'\n";
        let Ok(value) = node_json(
            CONFIG_PRESETS,
            &json!({"dump":dump,"packageRoot":root.to_string_lossy().into_owned()}),
            Duration::from_secs(10),
            64 * 1024,
        ) else {
            return;
        };
        let presets = presets_from_options(&value["presets"]).unwrap();
        assert!(presets.iter().any(|preset| preset.value == "Review workspace"));
        assert!(presets.iter().any(|preset| preset.value == "Проверка (strict)!"));
        assert_eq!(value["default"], json!("Review workspace"));
    }
}

#[cfg(test)]
mod account_model_tests {
    use super::*;

    /// Task #19: the profile's models in the host catalog's shape, as the New
    /// team form and `account model-settings` show them.
    #[test]
    fn profile_models_take_the_host_catalogs_ids_and_labels() {
        let helper = json!({"ok":true,"models":[
            {"provider":"deepseek-official","id":"deepseek-flash","name":"DeepSeek-V4.1-Flash","efforts":["off","low","high","max"],"defaultEffort":"high"},
            {"provider":"deepseek-official","id":"deepseek-v4-pro","name":"","efforts":["off"],"defaultEffort":"off"},
            {"provider":"","id":"stray"}
        ]});
        let models = profile_models(&helper);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0], json!({"id":"deepseek-official/deepseek-flash","label":"DeepSeek-V4.1-Flash (DeepSeek)",
            "effort_options":["off","low","high","max"],"default_effort":"high"}));
        assert_eq!(models[1]["label"], "deepseek-v4-pro (DeepSeek)");
    }

    /// Task #19: a bare model id is the one provider group that offers it.
    #[test]
    fn a_bare_model_takes_the_one_provider_that_offers_it() {
        let catalog = json!({"groups":[
            {"id":"deepseek-official","models":[{"id":"deepseek-flash"},{"id":"deepseek-v4-pro"}]},
            {"id":"deepseek-account","models":[{"id":"deepseek-v4-pro"}]}
        ]});
        assert_eq!(qualified_model(&catalog, "deepseek-flash").as_deref(), Some("deepseek-official/deepseek-flash"));
        assert_eq!(qualified_model(&catalog, "deepseek-v4-pro"), None, "two providers offer it");
        assert_eq!(qualified_model(&catalog, "gpt-5"), None);
        assert_eq!(qualified_model(&json!({}), "deepseek-flash"), None);
    }

    /// Task #19: the installed harness's own catalog for the API-key route:
    /// its bundled models without a `models` entry, the configured list with
    /// one, `off` alone with thinking disabled; never a computed value.
    #[test]
    fn the_profile_catalog_is_the_installed_transports() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let Some(root) = dsh_package_root() else { return };
        let ask = |dump: &str| {
            node_json(
                CONFIG_MODELS,
                &json!({"dump":dump,"packageRoot":root.to_string_lossy().into_owned()}),
                Duration::from_secs(20),
                64 * 1024,
            )
        };
        let row = "- id: llm-deepseek\n  name: '@deepseek-ai/dsh-llm-deepseek-api-key'\n";
        let agent = "- id: agent\n  name: '@deepseek-ai/dsh-agent'\n";
        let Ok(bundled) = ask(&format!("{agent}{row}")) else { return };
        let models = profile_models(&bundled);
        let flash = models.iter().find(|model| model["id"] == "deepseek-official/deepseek-flash").expect("the bundled catalog offers deepseek-flash");
        assert!(flash["label"].as_str().unwrap().ends_with(" (DeepSeek)"), "{flash}");
        assert!(flash["effort_options"].as_array().unwrap().contains(&json!("high")), "{flash}");
        // A configured list replaces the bundled one; a later layer merges.
        let custom = format!(
            "{row}  config:\n    models:\n      - id: deepseek-flash\n        name: Flash here\n{row}  config:\n    thinking: disabled\n"
        );
        let configured = profile_models(&ask(&custom).unwrap());
        assert_eq!(configured, vec![json!({"id":"deepseek-official/deepseek-flash","label":"Flash here (DeepSeek)",
            "effort_options":["off"],"default_effort":"off"})]);
        // A disabled route offers nothing; a computed list is never guessed.
        assert_eq!(ask(&format!("{row}  disabled: true\n")).unwrap_err(), "provider");
        assert_eq!(ask(&format!("{row}  config:\n    models: !!js 'ctx.models'\n")).unwrap_err(), "computed");
        assert_eq!(ask(agent).unwrap_err(), "provider");
    }

    #[test]
    fn native_catalog_projects_provider_models_and_efforts() {
        let catalog = json!({"groups":[{"id":"deepseek","name":"DeepSeek","models":[
            {"id":"chat","name":"Chat","reasoning":{"defaultEffort":"medium","efforts":[{"id":"low"},{"id":"medium"},{"id":"high"}]}},
            {"id":"reasoner","name":"Reasoner","reasoning":{"defaultEffort":"high","efforts":[{"id":"high"}]}}
        ]}]});
        let models = catalog_models(&catalog);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["id"], "deepseek/chat");
        assert_eq!(models[0]["label"], "Chat (DeepSeek)");
        assert_eq!(models[0]["effort_options"], json!(["low", "medium", "high"]));
        assert_eq!(models[0]["default_effort"], "medium");
        assert_eq!(models[1]["id"], "deepseek/reasoner");
    }
}

#[cfg(test)]
mod notification_tests {
    use super::*;
    #[test]
    fn notification_baseline_observes_only_inspect_and_list_with_matching_generation() {
        let record = json!({"name":"dsh/test/session","backend":"dsh","conversation_id":"session-A","run_id":"run-A"});
        let mut called = Vec::new();
        let (view, questions) = notification_snapshot_with(&record, |method, params| {
            called.push(method.to_owned());
            match method {
                "inspect" => { assert_eq!(params, json!({"sessionId":"session-A","includeProcesses":false})); Ok(json!({"generation":"host-1","pendingQuestions":[],"live":true})) },
                "list" => { assert_eq!(params, json!({})); Ok(json!({"generation":"host-1","items":[{"sessionId":"session-A","agentAvailable":true,"running":false}]})) },
                _ => panic!("a preference must not issue a native action"),
            }
        }).unwrap();
        assert_eq!(called, ["inspect", "list"]);
        assert_eq!(view["conversation_id"], "session-A"); assert_eq!(view["run_id"], "run-A");
        assert_eq!(view["phase"], "idle"); assert_eq!(questions, Some(Vec::new()));
        let (pending, questions) = notification_snapshot_with(&record, |method, _| Ok(if method == "inspect" {
            json!({"generation":"host","pendingQuestions":[{"id":"question-1"}],"live":true})
        } else { json!({"generation":"host","items":[{"sessionId":"session-A","agentAvailable":true,"running":false}]}) })).unwrap();
        assert_eq!(pending["phase"], "input");
        assert_eq!(pending["attention_id"], "dsh_question:question-1");
        assert_eq!(read_marks::signature(&pending, questions.as_deref()), read_marks::signature(&pending, Some(&[])));
        assert!(notification_snapshot_with(&record, |method, _| Ok(if method == "inspect" { json!({"generation":"host","pendingQuestions":[{}]}) } else { json!({"generation":"host"}) })).is_err());
        assert!(notification_snapshot_with(&record, |method, _| Ok(if method == "inspect" { json!({"generation":"old","pendingQuestions":[]}) } else { json!({"generation":"new"}) })).is_err());
        assert!(notification_snapshot_with(&record, |_, _| Ok(json!({"generation":"host"}))).is_err());
    }

    #[test]
    fn the_deepseek_host_never_enables_session_log_upload() {
        // H4: member, lead and designer launches all use these arguments and
        // this overlay; neither may enable session-log-deepseek.
        let overlay = Path::new("state/dsh/hosts/x/hgs.patch.json");
        for (default_profile, kind) in [(false, "member"), (false, "lead"), (true, "designer")] {
            let args = host_arguments(overlay, default_profile).join(" ");
            let lowered = args.to_lowercase();
            for forbidden in ["session-log", "sessionlog", "logupload", "upload"] {
                assert!(!lowered.contains(forbidden), "{kind}: {args}");
            }
            assert!(args.contains("--patch") && args.contains("--profile hgs"), "{kind}: {args}");
        }
        let overlay = host_overlay(Path::new("state/bridge.mjs"), bridge_config(Path::new("state/socket"), None), false).to_string().to_lowercase();
        assert!(overlay.contains("hgs-native-dsh"), "{overlay}");
        for forbidden in ["session-log", "sessionlog", "logupload", "upload"] {
            assert!(!overlay.contains(forbidden), "{overlay}");
        }
        // U22: the block overlay disables the shipped plugin; off leaves dsh alone.
        let blocked = host_overlay(Path::new("state/bridge.mjs"), bridge_config(Path::new("state/socket"), None), true).to_string();
        assert!(blocked.contains("session-log-deepseek") && blocked.contains("\"enabled\":false"), "{blocked}");
    }

    /// Task 22: a start that finds the host an earlier start launched still
    /// running, and launched recently, waits for it instead of launching a
    /// second host; an ended process or an old pid file is no such host.
    #[test]
    fn a_recently_launched_running_host_is_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let within = Duration::from_secs(140);
        assert_eq!(starting_host(dir.path(), within), None, "no pid file");
        let pid = dir.path().join("host.pid");
        std::fs::write(&pid, std::process::id().to_string()).unwrap();
        assert_eq!(starting_host(dir.path(), within), Some(std::process::id()));
        // Launched long ago: a pid that may belong to anything now.
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&pid).unwrap().set_modified(old).unwrap();
        assert_eq!(starting_host(dir.path(), within), None);
        // An ended process.
        let mut ended = if cfg!(windows) {
            std::process::Command::new("cmd").args(["/C", "exit 0"]).spawn().unwrap()
        } else {
            std::process::Command::new("true").spawn().unwrap()
        };
        let id = ended.id();
        ended.wait().unwrap();
        std::fs::write(&pid, id.to_string()).unwrap();
        assert_eq!(starting_host(dir.path(), within), None);
        std::fs::write(&pid, "not a pid").unwrap();
        assert_eq!(starting_host(dir.path(), within), None);
    }

    /// N17: a host that was already running keeps the block state it started
    /// with; a session launched into a host started before the block is marked
    /// "may be active" instead of promising a block that host never got.
    #[test]
    fn a_host_started_without_the_block_marks_its_sessions() {
        let _guard = crate::state::ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let old = std::env::var("HGS_STATE_DIR").ok();
        std::env::set_var("HGS_STATE_DIR", dir.path());
        let host = dir.path().join("host");
        std::fs::create_dir_all(&host).unwrap();
        // A host from before the setting: stale.
        assert!(running_host_block_stale(&host));
        std::fs::write(host.join("upload-block.json"), json!({"block":true,"at":1.0}).to_string()).unwrap();
        assert!(!running_host_block_stale(&host), "a host started with the block protects its sessions");
        std::fs::write(host.join("upload-block.json"), json!({"block":false,"at":1.0}).to_string()).unwrap();
        assert!(running_host_block_stale(&host), "a host started with the block off does not");
        // The mark says which state the session's host is in.
        let mut record = json!({});
        mark_upload_may_be_active(&mut record, true);
        let state = upload_state(&record);
        assert_eq!(
            state["upload_note"],
            "DeepSeek session-log upload may still be active: the running DeepSeek host was started before the block"
        );
        assert!(state["upload_may_be_active"].is_object());
        // A verified record mark (U22) is the stronger one.
        let verified = json!({"upload_seen":{"at":2.0},"upload_may_be_active":{"at":1.0}});
        assert_eq!(upload_state(&verified)["upload_note"], "DeepSeek session-log upload still active");
        // A host started with the block clears the mark.
        mark_upload_may_be_active(&mut record, false);
        assert!(record.get("upload_may_be_active").is_none());
        assert!(upload_state(&record).as_object().is_some_and(|state| state.is_empty()));
        match old {
            Some(value) => std::env::set_var("HGS_STATE_DIR", value),
            None => std::env::remove_var("HGS_STATE_DIR"),
        }
    }

    /// C2: the upload check writes only its own fields, under the bindings
    /// lock, into a binding re-read there; a resume or forget between the read
    /// and the write is never reverted.
    #[test]
    fn an_upload_check_never_reverts_a_binding_changed_under_it() {
        let dir = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(dir.path(), || {
            let name = "dsh/project/session";
            let base = json!({"backend":"dsh","name":name,"agent":"dsh",
                "agent_home":dir.path().join("home").display().to_string(),
                "cwd":"/work/example","conversation_id":"session-one","run_id":"run-1","created":1.0});
            save_ref(&base).unwrap();
            // A resume replaced the run between the check's read and its write.
            let mut resumed = base.clone();
            resumed["run_id"] = json!("run-2");
            save_ref(&resumed).unwrap();
            assert!(remember_upload(name, &base, 10.0, None, None, false).is_none());
            let kept = binding(name).unwrap();
            assert_eq!(kept["run_id"], json!("run-2"));
            assert!(kept.get("upload_checked_at").is_none(), "a changed binding was written");
            // The same binding: only the upload fields change; a field the
            // check never read keeps what the fresh record says.
            let mut changed = base.clone();
            changed["paused"] = json!(true);
            save_ref(&changed).unwrap();
            let seen = json!({"at":10.0,"plugin":"session-log-deepseek","offset":42});
            assert!(remember_upload(name, &base, 10.0, None, Some(&seen), false).is_some());
            let saved = binding(name).unwrap();
            assert_eq!(saved["paused"], json!(true), "another field was reverted");
            assert_eq!(saved["upload_checked_at"], json!(10.0));
            assert_eq!(saved["upload_seen"], seen);
            // A forgotten binding is skipped, never recreated.
            std::fs::remove_file(binding_path(name)).unwrap();
            assert!(remember_upload(name, &base, 20.0, None, Some(&seen), false).is_none());
            assert!(!binding_path(name).exists());
        });
    }

    /// N8: a launch starts at the store's length and remembers its path.
    #[test]
    fn a_launch_starts_the_upload_offset_at_the_store_length_and_remembers_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let store = home.join("sessions").join("--encoded--").join("session-one");
        std::fs::create_dir_all(&store).unwrap();
        let file = store.join("session.v4.jsonl.zstd");
        std::fs::write(&file, b"0123456789").unwrap();
        let mut record = json!({"agent_home":home.display().to_string(),"conversation_id":"session-one"});
        mark_upload_start(&mut record);
        assert_eq!(std::path::PathBuf::from(string(&record, "upload_path")), file);
        assert_eq!(record["upload_offset"], json!(10));
        assert_eq!(record["upload_checked_at"], json!(0));
        assert!(record["upload_since"].as_f64().is_some_and(|since| since > 0.0));
        // A session without a store yet records no path and starts at zero.
        let mut fresh = json!({"agent_home":home.display().to_string(),"conversation_id":"session-two"});
        mark_upload_start(&mut fresh);
        assert!(fresh.get("upload_path").is_none());
        assert_eq!(fresh["upload_offset"], json!(0));
    }

    /// N8: a session that has written no store still records the check time, so
    /// a listing does not look for it on every poll.
    /// #595 L1: a store the upload reader cannot walk is marked unverifiable on
    /// its binding instead of being re-read forever; a store that walks again
    /// clears the mark on its next pass.
    #[test]
    fn an_unwalkable_upload_store_is_marked_unverifiable_and_cleared_again() {
        let dir = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(dir.path(), || {
            let name = "dsh/project/session";
            let home = dir.path().join("home");
            let store = home.join("sessions").join("--encoded--").join("session-one");
            std::fs::create_dir_all(&store).unwrap();
            let file = store.join("session.v4.jsonl.zstd");
            let mut bytes = ruzstd::encoding::compress_to_vec(
                json!({"type":"session","seq":1,"time":1_000_000.0}).to_string().as_bytes(),
                ruzstd::encoding::CompressionLevel::Fastest,
            );
            // An unknown magic after a whole frame: the reader cannot step.
            bytes.extend_from_slice(&[0x00, 0x01, 0x02, 0x03, 0x04]);
            std::fs::write(&file, &bytes).unwrap();
            save_ref(&json!({"backend":"dsh","name":name,"agent":"dsh",
                "agent_home":home.display().to_string(),"cwd":"/work/example",
                "conversation_id":"session-one","run_id":"run-1","created":1.0,"upload_offset":0})).unwrap();
            assert!(check_upload(name).is_none());
            let marked = binding(name).unwrap();
            assert_eq!(marked["upload_unverifiable"], json!(true), "{marked}");
            // A rewritten store that walks again clears the mark.
            std::fs::write(
                &file,
                ruzstd::encoding::compress_to_vec(
                    json!({"type":"session-log-deepseek/delivery-accepted","seq":2,"time":2_000_500.0})
                        .to_string()
                        .as_bytes(),
                    ruzstd::encoding::CompressionLevel::Fastest,
                ),
            )
            .unwrap();
            let mut reset = binding(name).unwrap();
            reset["upload_checked_at"] = json!(0);
            reset["upload_offset"] = json!(0);
            save_ref(&reset).unwrap();
            assert!(check_upload(name).is_some());
            assert!(binding(name).unwrap().get("upload_unverifiable").is_none());
        });
    }

    #[test]
    fn a_missing_store_records_the_check_time_and_is_not_scanned_again() {
        let dir = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(dir.path(), || {
            let name = "dsh/project/session";
            save_ref(&json!({"backend":"dsh","name":name,"agent":"dsh",
                "agent_home":dir.path().join("home").display().to_string(),
                "cwd":"/work/example","conversation_id":"session-one","run_id":"run-1","created":1.0})).unwrap();
            assert!(check_upload(name).is_none());
            let saved = binding(name).unwrap();
            let at = saved["upload_checked_at"].as_f64().unwrap_or(0.0);
            assert!(at > 0.0, "{saved}");
            assert!(saved.get("upload_path").is_none(), "{saved}");
            assert_eq!(saved["run_id"], json!("run-1"));
            // Within the minute it is neither checked nor scanned again.
            assert!(check_upload(name).is_none());
            let again = binding(name).unwrap();
            assert_eq!(again["upload_checked_at"], json!(at));
            assert!(again.get("upload_seen").is_none());
        });
    }
}

#[cfg(all(test, unix))]
mod compatibility_tests {
    use super::*;

    /// zerus-ppd26: the session that happens to start a native host is not
    /// the session of any hosted agent, so the host never takes its markers.
    #[test]
    fn a_native_host_never_takes_the_markers_of_the_session_that_starts_it() {
        let record = json!({"agent_home":"/nonexistent/hgs-native-home"});
        let command = host_command(&record, Path::new("/nonexistent/hgs.patch.json"));
        let envs: BTreeMap<String, Option<String>> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for name in crate::state::backend::host::SESSION_CONTEXT.iter().chain(&[
            "HGS_TERMINAL_NAME",
            "HGS_SESSION",
            "HGS_SESSION_ID",
            "HGS_RUN_ID",
            "HGS_AGENT",
            "HGS_ACCOUNT_ID",
        ]) {
            assert_eq!(envs.get(*name), Some(&None), "{name} must be removed");
        }
        assert_eq!(
            envs.get("DSH_HOME"),
            Some(&Some("/nonexistent/hgs-native-home".to_owned()))
        );
        // The host belongs to this state root and configuration.
        for kept in ["HGS_STATE_DIR", "HGS_CONFIG_DIR"] {
            assert!(!envs.contains_key(kept), "{kept} must be inherited");
        }
    }

    #[test]
    fn startup_accepts_only_the_explicitly_reviewed_harness_releases() {
        assert!(reviewed_harness_version("0.2.1-rc.2"));
        assert!(reviewed_harness_version("0.2.1-rc.5"));
        assert!(reviewed_harness_version("0.2.2-rc.1"));
        for unknown in ["0.2.1-rc.1", "0.2.1-rc.3", "0.2.1-rc.4", "0.2.1-rc.6",
            "0.2.1-rc.50", "0.2.1", "0.2.2", "0.2.2-rc.2", "0.3.0", "0.2.1-rc.5+unreviewed", "0.2.1-rc.5\nother"] {
            assert!(!reviewed_harness_version(unknown), "{unknown}");
        }
    }

    #[test]
    fn reviewed_engine_versions_do_not_change_the_resident_adapter_identity() {
        assert!(compatible_adapter(&json!({"protocol":1,"version":"0.2.1-rc.2"})));
        for ping in [json!({"protocol":1,"version":"0.2.1-rc.5"}), json!({"protocol":1,"version":"0.2.2-rc.1"}),
            json!({"protocol":2,"version":"0.2.1-rc.2"}), json!({"protocol":1})] {
            assert!(!compatible_adapter(&ping));
        }
    }
}

#[cfg(test)]
mod harness_refusal_tests {
    use super::*;

    #[test]
    fn a_harness_refusal_names_what_it_printed_and_the_fix() {
        let text = harness_version_refusal(Some("usage: dsh"));
        for part in ["printed \"usage: dsh\"", "0.2.1-rc.2 or 0.2.1-rc.5", "npm install -g @deepseek-ai/dsh@0.2.1-rc.5", "nothing was started"] {
            assert!(text.contains(part), "{part} missing: {text}");
        }
        assert!(harness_version_refusal(None).contains("did not report its version"));
    }

    /// The owner, 2026-11-04: every release starts; an unreviewed one is noted.
    #[test]
    fn every_release_starts_and_an_unreviewed_one_is_noted() {
        for (output, release) in [("0.2.1-rc.5", "0.2.1-rc.5"), ("0.2.2-rc.1\n", "0.2.2-rc.1"), ("0.3.0", "0.3.0"),
            ("@deepseek-ai/dsh/0.2.2-alpha.3 win32-x64 node-v22.19.0", "0.2.2-alpha.3"), ("dsh v0.2.4", "0.2.4"), ("\n0.2.1-rc.3\nmore", "0.2.1-rc.3")] {
            assert_eq!(harness_release(output).as_deref(), Some(release), "{output:?}");
        }
        for output in ["", "usage: dsh", "latest", "0.2"] {
            assert_eq!(harness_release(output), None, "{output:?}");
        }
        // Task #61: 0.2.2-rc.1 is reviewed since 2026-11-05; 0.2.2-rc.2 is not.
        let note = harness_unreviewed_note("0.2.2-rc.2");
        assert!(note.contains("0.2.2-rc.2 is not a release Zerus reviewed") && note.contains("started anyway"), "{note}");
        assert!(reviewed_harness_version("0.2.1-rc.5") && !reviewed_harness_version("0.2.2-rc.2"));
    }
}

#[cfg(all(test, unix))]
mod delivery_tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// zerus-1wchz: a host that takes the connection and goes away while the
    /// request is still being written. The bridge runs only a whole,
    /// newline-terminated request, so this one never ran: a definite refusal.
    #[test]
    fn a_request_cut_short_is_a_definite_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let host = std::thread::spawn(move || drop(listener.accept().unwrap()));
        // Larger than any socket buffer: the write is still running when the
        // host closes, so it fails rather than the read.
        let content = "x".repeat(16 * 1024 * 1024);
        let error = rpc_at(
            &path,
            "send",
            json!({"sessionId":"session-A","content":content}),
            Duration::from_secs(5),
        )
        .unwrap_err();
        host.join().unwrap();
        assert!(error.starts_with(INCOMPLETE), "{error}");
        assert!(!error.contains("delivery uncertain"), "{error}");
        assert_eq!(interrupt_refusal(error.clone()), error);
    }

    /// Every shipped bridge, the running ones included (a resident host is
    /// never reloaded), runs a request only from a chunk holding its newline
    /// and has no end handler. If that changes, a cut-short request may run,
    /// and `INCOMPLETE` stops being a definite refusal.
    #[test]
    fn the_bridge_runs_only_a_newline_terminated_request() {
        assert_eq!(BRIDGE.matches("socket.on('data', chunk => {").count(), 1);
        assert_eq!(
            BRIDGE.matches("if (!chunk.includes(10)) return;").count(),
            1
        );
        for handler in [
            "readline",
            "createInterface",
            "on('end'",
            "once('end'",
            "on('close'",
            "once('close'",
        ] {
            assert!(!BRIDGE.contains(handler), "{handler}");
        }
    }

    /// The production interrupt path, answered by a host: a failure after the
    /// bridge's own checks is uncertain, one of those checks stays definite.
    #[test]
    fn an_interrupt_refused_after_its_effects_is_uncertain_through_the_host() {
        let record = json!({"conversation_id":"session-A"});
        let snapshot = json!({"host_generation":"host-1","turn_started":1793000000.5});
        for (error, expected) in [
            (
                "Session not found",
                "Interrupt delivery uncertain: Session not found",
            ),
            (
                "DeepSeek turn changed; nothing was stopped",
                "DeepSeek turn changed; nothing was stopped",
            ),
            ("DeepSeek host changed", "DeepSeek host changed"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("host.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let host = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(&socket).read_line(&mut request).unwrap();
                writeln!(socket, "{}", json!({"ok":false,"error":error})).unwrap();
                serde_json::from_str::<Value>(&request).unwrap()
            });
            let answer = interrupt_with(&record, &snapshot, |record, method, params| {
                assert_eq!(record["conversation_id"], "session-A");
                rpc_at(&path, method, params, Duration::from_secs(5))
            });
            assert_eq!(answer.unwrap_err(), expected);
            assert_eq!(
                host.join().unwrap(),
                json!({"method":"interrupt","params":{"sessionId":"session-A",
                    "generation":"host-1","turnStarted":1793000000.5}})
            );
        }
    }

    #[test]
    fn no_host_to_reach_is_a_definite_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let error = rpc_at(
            &dir.path().join("host.sock"),
            "send",
            json!({}),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error, HOST_UNAVAILABLE);
        assert_eq!(interrupt_refusal(error), HOST_UNAVAILABLE);
    }

    /// The bridge's own checks ahead of its effects stay definite; anything
    /// after them may follow a stopped turn.
    #[test]
    fn only_an_interrupt_refused_before_its_effects_is_definite() {
        for definite in [
            "DeepSeek host changed",
            "Use the parent to manage a subagent",
            "DeepSeek turn changed; nothing was stopped",
            HOST_UNAVAILABLE,
        ] {
            assert_eq!(interrupt_refusal(definite.into()), definite);
        }
        let incomplete = format!("{INCOMPLETE}: Broken pipe (os error 32)");
        assert_eq!(interrupt_refusal(incomplete.clone()), incomplete);
        for already in [
            "Native request delivery uncertain: Broken pipe (os error 32)",
            "Invalid native response; delivery uncertain: EOF while parsing a value",
        ] {
            assert_eq!(interrupt_refusal(already.into()), already);
        }
        for after in [
            "Session not found",
            "DeepSeek host changed; refresh Processes",
            "",
        ] {
            assert_eq!(
                interrupt_refusal(after.into()),
                format!("Interrupt delivery uncertain: {after}")
            );
        }
    }
}

/// Task 209: a session's compaction capability is its host's, not its phase;
/// the phase only says why the command cannot run right now.
#[cfg(test)]
mod compact_support_tests {
    use super::*;

    #[test]
    fn compact_support_does_not_depend_on_the_member_being_idle() {
        assert!(compact_capability(None, &json!({"compactSupported": true})));
        assert!(!compact_capability(None, &json!({"compactSupported": false})));
        assert!(!compact_capability(Some("child"), &json!({"compactSupported": true})), "a subagent has no compaction of its own");
        // Busy is a reason, never a loss of capability: the reported bug said
        // "cannot compact" while the host supported it and the member was busy.
        assert_eq!(compact_reason(true, "idle"), "");
        assert_eq!(compact_reason(true, "ready"), "");
        assert_eq!(compact_reason(true, "working"), "Wait for the DeepSeek agent to become idle before compacting its context");
        assert_eq!(compact_reason(false, "idle"), "Native DeepSeek compaction is unavailable");
    }
}

/// Task 234: a listing reuses the host's rows of stopped DeepSeek sessions
/// (each costs the host a replay of the stored history) and asks only for
/// the rest.
#[cfg(test)]
mod listing_rows_tests {
    use super::*;

    fn record(id: &str, run: &str) -> Value {
        json!({"conversation_id":id,"run_id":run,"agent_home":"h"})
    }

    fn row(id: &str, live: bool) -> Value {
        json!({"sessionId":id,"agentAvailable":live,"running":false,"pendingQuestions":[]})
    }

    fn ids(rows: &[Value]) -> Vec<&str> {
        rows.iter().map(|row| row["sessionId"].as_str().unwrap()).collect()
    }

    #[test]
    fn stopped_rows_are_reused_and_only_the_rest_asked_for() {
        let (a, b, c) = (record("a", "r1"), record("b", "r2"), record("c", "r3"));
        let records = [&a, &b, &c];
        let generation = json!("g1");
        // Nothing cached: every conversation is asked for.
        let (reused, ask) = plan_rows(&Value::Null, &records, &generation, None, 100.0);
        assert!(reused.is_empty());
        assert_eq!(ask, ["a", "b", "c"]);
        // The answer: a and c stopped, b loaded. Only the stopped ones are kept.
        let cache = update_rows(&Value::Null, &records, &generation, &[row("a", false), row("b", true), row("c", false)], 100.0);
        assert_eq!(cache["rows"].as_object().unwrap().keys().collect::<Vec<_>>(), ["a", "c"]);
        let (reused, ask) = plan_rows(&cache, &records, &generation, None, 160.0);
        assert_eq!((ids(&reused), ask), (vec!["a", "c"], vec!["b".to_owned()]));
        // Another host generation, another run of a binding, or a session the
        // host says it loaded: asked for again.
        assert_eq!(plan_rows(&cache, &records, &json!("g2"), None, 160.0).1, ["a", "b", "c"]);
        let resumed = record("a", "r9");
        assert_eq!(plan_rows(&cache, &[&resumed, &b, &c], &generation, None, 160.0).1, ["a", "b"]);
        let loaded: BTreeSet<String> = ["c".to_owned()].into();
        assert_eq!(plan_rows(&cache, &records, &generation, Some(&loaded), 160.0).1, ["b", "c"]);
        // A row that came back live leaves the cache.
        let after = update_rows(&cache, &records, &generation, &[row("c", true)], 170.0);
        assert!(after["rows"]["c"].is_null() && after["rows"]["a"].is_object());
        // A binding that is gone takes its row along.
        let fewer = update_rows(&cache, &[&a, &b], &generation, &[], 170.0);
        assert!(fewer["rows"]["c"].is_null() && fewer["rows"]["a"].is_object());
    }

    #[test]
    fn rows_past_their_bound_are_refreshed_one_a_listing_oldest_first() {
        let (a, b) = (record("a", "r1"), record("b", "r2"));
        let records = [&a, &b];
        let generation = json!("g1");
        let cache = update_rows(&Value::Null, &records, &generation, &[row("a", false)], 100.0);
        let cache = update_rows(&cache, &records, &generation, &[row("b", false)], 200.0);
        let later = 200.0 + STOPPED_ROW_SECONDS + 1.0;
        let (reused, ask) = plan_rows(&cache, &records, &generation, None, later);
        assert_eq!((ids(&reused), ask), (vec!["b"], vec!["a".to_owned()]), "the oldest first, one a listing");
        // A host naming its loaded sessions keeps a stopped row much longer.
        let loaded = BTreeSet::new();
        assert!(plan_rows(&cache, &records, &generation, Some(&loaded), later).1.is_empty());
    }

    #[test]
    fn a_reused_row_brings_its_subagents() {
        let a = record("a", "r1");
        let generation = json!("g1");
        let parent = json!({"sessionId":"a","agentAvailable":false,"running":false,
            "projections":{"values":{"subagentCatalog":[{"id":"a-child"}]}}});
        let cache = update_rows(&Value::Null, &[&a], &generation, &[parent, row("a-child", false)], 100.0);
        assert_eq!(cache["rows"]["a-child"]["run_id"], "r1", "a subagent keeps its parent's run");
        let (reused, ask) = plan_rows(&cache, &[&a], &generation, None, 110.0);
        assert!(ask.is_empty());
        assert_eq!(ids(&reused), ["a", "a-child"]);
        // A row with a pending question is never cached.
        let asking = json!({"sessionId":"a","agentAvailable":false,"running":false,"pendingQuestions":[{"id":"q"}]});
        assert!(update_rows(&Value::Null, &[&a], &generation, &[asking], 100.0)["rows"]["a"].is_null());
    }

    /// Task #47 (team ede724c4, 2026-11-10): the host took 45 s and answered
    /// nothing for two stopped sessions while every other row came in
    /// milliseconds. Each conversation is asked on its own within ROWS_WAIT:
    /// the slow ones hold up only their own rows, rest SLOW_RETRY_SECONDS, and
    /// keep their last rows.
    #[test]
    fn a_slow_conversation_holds_up_only_its_own_row_and_then_rests() {
        let records_owned = [record("fast", "r1"), record("slow", "r2"), record("gone", "r3")];
        let records: Vec<&Value> = records_owned.iter().collect();
        let generation = json!("g1");
        let ask = vec!["fast".to_owned(), "slow".to_owned()];
        let started = std::time::Instant::now();
        let (answers, slow) = rows_each(&ask, |id| {
            if id == "slow" {
                // As rpc_within answers when the host gives nothing in time.
                std::thread::sleep(Duration::from_millis(300));
                Err("Native request delivery uncertain: timed out".into())
            } else {
                Ok(json!({"generation":"g1","items":[row(id, false)]}))
            }
        });
        assert!(started.elapsed() < Duration::from_secs(5), "side by side, not one after another");
        assert_eq!(answers.len(), 1);
        assert_eq!(slow.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["slow"]);
        // The slow mark is kept with the cache; an answered or unbound
        // conversation loses its mark, and another host generation drops all.
        let fresh = vec![row("fast", false)];
        let mut cache = update_rows(&Value::Null, &records, &generation, &fresh, 100.0);
        cache["slow"] = slow_marks(&Value::Null, &generation, &records, &fresh, &["slow".to_owned()], 100.0);
        assert_eq!(cache["slow"], json!({"slow":{"at":100.0}}));
        // The next listings do not ask it again until SLOW_RETRY_SECONDS.
        let (resting, ask) = rest_slow(&cache, &generation, vec!["slow".to_owned(), "other".to_owned()], 100.0 + SLOW_RETRY_SECONDS - 1.0);
        assert_eq!((resting, ask), (vec!["slow".to_owned()], vec!["other".to_owned()]));
        let (resting, ask) = rest_slow(&cache, &generation, vec!["slow".to_owned()], 100.0 + SLOW_RETRY_SECONDS);
        assert!(resting.is_empty() && ask == ["slow"], "asked again after its rest");
        let (resting, _) = rest_slow(&cache, &json!("g2"), vec!["slow".to_owned()], 101.0);
        assert!(resting.is_empty(), "a restarted host is asked at once");
        let answered = vec![row("slow", false)];
        assert_eq!(slow_marks(&cache, &generation, &records, &answered, &[], 200.0), json!({}), "an answer clears the mark");
        let unbound: Vec<&Value> = records[..1].to_vec();
        assert_eq!(slow_marks(&cache, &generation, &unbound, &[], &[], 200.0), json!({}), "an unbound conversation's mark goes");
        assert_eq!(slow_marks(&cache, &json!("g2"), &records, &[], &[], 200.0), json!({}));
    }
}

#[cfg(test)]
mod pending_identity_tests {
    use super::*;

    #[test]
    fn fork_intents_of_label_twins_are_distinct() {
        let source = |id, conversation| json!({"name":"dsh/example/shared","session_id":id,"conversation_id":conversation});
        let one = source("11111111-1111-4111-8111-111111111111", "session-one");
        let two = source("22222222-2222-4222-8222-222222222222", "session-two");
        assert_ne!(fork_pending_path(&one,Some("child")), fork_pending_path(&two,Some("child")));
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn a_new_host_gets_the_agent_hint_only_when_it_is_on() {
        let socket = Path::new("/state/dsh/hosts/key/host.sock");
        let on = bridge_config(socket, Some(agent_hint::HINT));
        assert_eq!(
            (on["socket"].as_str(), on["agentHint"].as_str()),
            (Some("/state/dsh/hosts/key/host.sock"), Some(agent_hint::HINT))
        );
        assert!(on["recoveryPolicy"].is_string());
        let off = bridge_config(socket, None);
        assert!(off.get("agentHint").is_none());
        assert_eq!(off["socket"], on["socket"]);
    }
}
