//! Named, per-machine native agent profiles. The catalog contains labels, never credentials.
mod inspection;
mod model_defaults;
mod claude;
mod claude_settings;
mod mcp_servers;
mod permission_catalog;
mod permission_status;
mod install;
mod deepseek;
mod local_model;
use crate::{
    cli::{Error, Result},
    config::Config,
    platform,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    provider: String,
    label: String,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    #[serde(default)]
    profiles: BTreeMap<String, Profile>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    native_labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    hidden_profiles: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    permission_modes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    defaults: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    model_defaults: BTreeMap<String, model_defaults::Defaults>,
}
fn read(config: &Config) -> Result<Vec<u8>> {
    match fs::read(config.dir.join("accounts.json")) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => Ok(bytes),
        Ok(_) => Err(Error::new(1, "Account catalog is too large.")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}
fn decode(bytes: &[u8]) -> Result<Catalog> {
    if bytes.is_empty() {
        return Ok(Catalog::default());
    }
    let catalog: Catalog = serde_json::from_slice(bytes).map_err(|_| {
        Error::new(
            1,
            "Invalid accounts.json; repair the account catalog before editing profiles.",
        )
    })?;
    for (id, p) in &catalog.profiles {
        validate(id, p)?;
    }
    for (id, label) in &catalog.native_labels {
        if !["native-codex", "native-claude", "native-kimi", "native-dsh", "native-opencode"].contains(&id.as_str()) {
            return Err(Error::new(1, "Invalid native account label."));
        }
        validate_label(label)?;
    }
    for id in &catalog.hidden_profiles {
        if !native_id(id) && !catalog.profiles.contains_key(id) {
            return Err(Error::new(1, "Invalid removed account profile."));
        }
    }
    for (id, mode) in &catalog.permission_modes {
        if !native_id(id) && !catalog.profiles.contains_key(id) {
            return Err(Error::new(1,"Invalid account permission mode."));
        }
        permission_catalog::check_stored_mode(mode)?;
    }
    for (provider, id) in &catalog.defaults {
        if !["codex", "claude", "kimi"].contains(&provider.as_str())
            || catalog.hidden_profiles.contains(id)
            || (id != &format!("native-{provider}") && catalog.profiles.get(id).is_none_or(|p| &p.provider != provider)) {
            return Err(Error::new(1, "Invalid default account profile."));
        }
    }
    model_defaults::validate_catalog(&catalog)?;
    Ok(catalog)
}
pub fn default_account(config: &Config, provider: &str) -> Result<Option<String>> {
    if !["codex", "claude", "kimi"].contains(&provider) { return Ok(None); }
    Ok(decode(&read(config)?)?.defaults.get(provider).cloned())
}
fn native_id(id: &str) -> bool {
    ["native-codex", "native-claude", "native-kimi", "native-dsh", "native-opencode"].contains(&id)
}
fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn validate(id: &str, p: &Profile) -> Result<()> {
    if id.is_empty()
        || id.len() > 80
        || id.starts_with('-')
        || id.starts_with("native-")
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(Error::new(1,"Account ID: use letters, digits, underscores and hyphens, without a leading hyphen or native- prefix."));
    }
    home_var(&p.provider)?;
    validate_label(&p.label)
}
fn validate_label(label: &str) -> Result<()> {
    if label.trim().is_empty() || label.chars().count() > 160 || label.chars().any(char::is_control)
    {
        return Err(Error::new(
            1,
            "Account label must contain 1–160 characters, without control characters.",
        ));
    }
    Ok(())
}
pub fn home_var(provider: &str) -> Result<&'static str> {
    match provider {
        "codex" => Ok("CODEX_HOME"),
        "claude" => Ok("CLAUDE_CONFIG_DIR"),
        "kimi" => Ok("KIMI_CODE_HOME"),
        _ => Err(Error::new(1, "Accounts support Codex, Claude and Kimi.")),
    }
}
fn native_home(config: &Config, provider: &str) -> PathBuf {
    if provider=="opencode" {return env::var_os("OPENCODE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(||env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(||Path::new(&config.home).join(".config")).join("opencode"));}
    env::var(if provider == "dsh" {
        "DSH_HOME"
    } else {
        home_var(provider).unwrap()
    })
    .ok()
    .filter(|s| !s.is_empty())
    .map(PathBuf::from)
    .unwrap_or_else(|| {
        Path::new(&config.home).join(if provider == "kimi" {
            ".kimi-code"
        } else if provider == "codex" {
            ".codex"
        } else if provider == "dsh" {
            ".dsh"
        } else {
            ".claude"
        })
    })
}
fn profile_home(config: &Config, id: &str) -> PathBuf {
    config.dir.join("accounts").join(id)
}
fn lookup(config: &Config, id: &str) -> Result<(Profile, PathBuf, bool)> {
    let catalog = decode(&read(config)?)?;
    if let Some(provider) = id.strip_prefix("native-") {
        if !["dsh","opencode"].contains(&provider) {
            home_var(provider)?;
        }
        return Ok((
            Profile {
                provider: provider.into(),
                label: catalog
                    .native_labels
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| default_label(provider).into()),
            },
            native_home(config, provider),
            true,
        ));
    }
    let profile = catalog
        .profiles
        .get(id)
        .ok_or_else(|| {
            Error::new(
                1,
                format!("Unknown account profile '{id}' on {}.", config.host()),
            )
        })?
        .clone();
    Ok((profile, profile_home(config, id), false))
}
/// Resolve a hook health target without inspecting credentials or running a provider.
pub(crate) fn hook_profile(config: &Config, id: &str) -> Result<(String, PathBuf)> {
    let (profile, path, _) = lookup(config, id)?;
    Ok((profile.provider, path))
}

/// The Claude account config a new launch without `--account` reads (U16,
/// N18): the default managed profile's `.claude.json` (the launch sets
/// `CLAUDE_CONFIG_DIR` to that profile), else the native one.
pub(crate) fn claude_launch_config(config: &Config) -> Result<PathBuf> {
    match default_account(config, "claude")? {
        Some(id) if !native_id(&id) => Ok(lookup(config, &id)?.1.join(".claude.json")),
        _ => claude::account_config(),
    }
}

/// U16: mark Zerus's own Team designer folder trusted in the account config the
/// designer's Claude session reads. Only that folder; start-up trust stays
/// manual for every other folder.
pub(crate) fn trust_claude_folder(config: &Path, folder: &str) -> Result<bool> {
    claude::trust_folder(config, folder)
}

fn default_label(provider: &str) -> &'static str {
    if provider=="opencode" {return "Native OpenCode configuration";}
    if provider == "dsh" {
        "Native DeepSeek account"
    } else {
        "Default account"
    }
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::new(
            1,
            "Account profile directory must be a real directory.",
        ));
    }
    platform::restrict_dir(path)?;
    Ok(())
}
fn locked(config: &Config) -> Result<platform::FileLock> {
    fs::create_dir_all(&config.dir)?;
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(config.dir.join("accounts.lock"))?;
    let started = std::time::Instant::now();
    loop {
        match f.try_lock_exclusive() {
            Ok(()) => break,
            Err(error) if platform::lock_contended(&error) && started.elapsed().as_secs() < 5 => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            Err(error) if platform::lock_contended(&error) => {
                return Err(Error::new(
                    1,
                    "Account catalog is busy. Retry after the current operation finishes.",
                ))
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(platform::FileLock::held(f))
}
fn save(config: &Config, catalog: &Catalog) -> Result<()> {
    let mut f = tempfile::NamedTempFile::new_in(&config.dir)?;
    platform::make_private(f.as_file())?;
    f.write_all(
        &serde_json::to_vec_pretty(catalog)
            .map_err(|_| Error::new(1, "Cannot encode account catalog."))?,
    )?;
    f.as_file().sync_all()?;
    platform::persist(f, config.dir.join("accounts.json"))
        .map_err(|e| Error::new(1, e.to_string()))?;
    platform::sync_dir(&config.dir)?;
    Ok(())
}
fn credential_file(home: &Path, provider: &str) -> bool {
    match provider {
        "codex" => home.join("auth.json").is_file(),
        "claude" => home.join(".credentials.json").is_file(),
        "kimi" => fs::read_dir(home.join("credentials"))
            .ok()
            .is_some_and(|entries| {
                entries
                    .filter_map(|x| x.ok())
                    .any(|x| x.path().extension().is_some_and(|x| x == "json"))
            }),
        _ => false,
    }
}
fn row(config: &Config, id: &str, p: &Profile, native: bool) -> Value {
    let home = if native {
        native_home(config, &p.provider)
    } else {
        profile_home(config, id)
    };
    let found = credential_file(&home, &p.provider);
    json!({"id":id,"provider":p.provider,"label":p.label,"home":home,"native":native,
        "account_status":inspection::catalog_status(config,id,&p.provider,&home),
        "auth_revision":inspection::signature(&home,&p.provider),
        "credential_file":found,"auth_status":if found {"credential_file"} else {"unknown"},
        "portable":p.provider!="claude","installed":platform::available(&p.provider)})
}
fn list(config: &Config) -> Result<Value> {
    let bytes = read(config)?;
    let catalog = decode(&bytes)?;
    let mut rows = Vec::new();
    for provider in ["codex", "claude", "kimi"] {
        rows.push(row(
            config,
            &format!("native-{provider}"),
            &Profile {
                provider: provider.into(),
                label: catalog
                    .native_labels
                    .get(&format!("native-{provider}"))
                    .cloned()
                    .unwrap_or_else(|| default_label(provider).into()),
            },
            true,
        ));
    }
    for (id, p) in &catalog.profiles {
        rows.push(row(config, id, p, false));
    }
    if platform::available("dsh") || catalog.hidden_profiles.contains("native-dsh") {
        let home = native_home(config, "dsh");
        rows.push(json!({"id":"native-dsh","provider":"dsh","label":catalog.native_labels.get("native-dsh").map(String::as_str).unwrap_or(default_label("dsh")),"home":home,
            "account_status":inspection::catalog_status(config,"native-dsh","dsh",&home),
            "native":true,"credential_file":false,"auth_status":"unknown","portable":false,"installed":platform::available("dsh")}));
    }
    if platform::available("opencode") || catalog.hidden_profiles.contains("native-opencode") {
        let row=json!({"id":"native-opencode","provider":"opencode","label":catalog.native_labels.get("native-opencode").map(String::as_str).unwrap_or(default_label("opencode")),"home":native_home(config,"opencode"),"native":true,"credential_file":false,"auth_status":"native_configuration","portable":false,"installed":false,"permission_mode_supported":false});
        #[cfg(unix)]
        let row = {
            let mut row = row;
            let (installed, reason) = crate::state::opencode::install_state();
            row["installed"] = json!(installed);
            if !installed {
                row["install_reason"] = json!(reason);
            }
            let models = crate::state::opencode::catalog(None);
            for key in ["model_options", "model_options_truncated", "providers", "providers_truncated", "settings_model", "catalog_state", "catalog_reason", "catalog_at"] {
                row[key] = models[key].clone();
            }
            row
        };
        rows.push(row);
    }
    let providers: BTreeSet<String> = rows
        .iter()
        .filter_map(|row| row["provider"].as_str())
        .filter(|provider| ["claude", "codex", "kimi", "dsh"].contains(provider))
        .map(str::to_owned)
        .collect();
    let mut catalogs = permission_catalog::Cache::prefetch(config, &providers);
    for row in &mut rows {
        let provider = row["provider"].as_str().unwrap_or("").to_owned();
        row["is_default"] = json!(row["id"].as_str() == Some(catalog.defaults.get(&provider).map(String::as_str).unwrap_or(&format!("native-{provider}"))));
        let mode = catalog.permission_modes.get(row["id"].as_str().unwrap_or("")).map(String::as_str).unwrap_or("provider");
        permission_status::describe(config, row, mode, &mut catalogs);
        row["permission_modes"] = json!(permission_modes(&provider));
        model_defaults::describe(row, &catalog);
    }
    // The connector forwards at most 128 KiB of accounts; this row keeps within 32 KiB.
    #[cfg(unix)]
    for row in rows.iter_mut().filter(|r| r["id"] == "native-opencode" && r["model_options"].is_array()) {
        crate::state::opencode::fit_row(row, crate::state::opencode::ROW_BUDGET);
    }
    let (removed, visible): (Vec<_>, Vec<_>) = rows.into_iter().partition(|r| {
        catalog
            .hidden_profiles
            .contains(r["id"].as_str().unwrap_or(""))
    });
    Ok(
        json!({"host":config.host(),"profiles":visible,"removed_profiles":removed,"defaults":catalog.defaults,"revision":revision(&bytes),"setup_revision":true,"copy_revision":true}),
    )
}
pub const AUTH_ENV: &[&str] = &[
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "CODEX_ACCESS_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_OAUTH_SCOPES",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "KIMI_API_KEY",
    "MOONSHOT_API_KEY",
    "OPENAI_BASE_URL",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_PROFILE",
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_ORGANIZATION_ID",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];
/// Called before state lookup: a resumed/forked record still retains its original native home.
pub fn permission_mode(config: &Config, account: &str, provider: &str) -> Result<String> {
    let id = if account.is_empty() {format!("native-{provider}")} else {account.into()};
    let (profile,_,_) = lookup(config,&id)?;
    if profile.provider != provider { return Err(Error::new(1,"The account belongs to another provider.")); }
    Ok(decode(&read(config)?)?.permission_modes.get(&id).cloned().unwrap_or_else(||"provider".into()))
}

/// Modes Zerus can set for a provider's account or a single launch; `provider`
/// leaves the native settings in charge. Only Claude has a native Auto mode.
pub fn permission_modes(provider: &str) -> &'static [&'static str] {
    match provider {
        "claude" => &["provider", "auto", "bypass"],
        "codex" | "kimi" | "dsh" => &["provider", "bypass"],
        _ => &[],
    }
}
/// Validate a requested mode before any slow discovery. Fresh native options
/// are checked by `permission_args` at launch or by `validate_for_account` on a
/// write.
pub fn validate_permission_mode(provider: &str, mode: &str) -> Result<()> {
    permission_catalog::check_mode(provider, mode)
}

/// Canonical stored form for account writes and launch binding overrides.
pub fn normalize_permission_mode(mode: &str) -> Result<String> {
    permission_catalog::normalize_mode(mode)
}

/// The native switches for a mode. `provider` and `bypass` are static fallbacks;
/// a native selection is validated against freshly discovered options first.
pub fn permission_args(provider: &str, mode: &str) -> Result<Vec<String>> {
    match mode {
        "provider" => Ok(Vec::new()),
        "bypass" => Ok(permission_catalog::bypass_args(provider)),
        "auto" if provider == "claude" => Ok(vec!["--permission-mode".into(), "auto".into()]),
        "auto" => Err(Error::new(1, "Auto is available only for Claude accounts.")),
        _ => {
            let config = Config::load()?;
            let home = native_home(&config, provider);
            permission_catalog::args(&config, provider, mode, &home)
        }
    }
}
/// Whether the agent's own arguments, before any `--`, already choose a mode.
pub fn explicit_permissions(provider: &str, argv: &[String]) -> bool {
    let at = argv.iter().position(|arg|arg=="--").unwrap_or(argv.len());
    let explicit: &[&str] = match provider {
        "claude" => &["--permission-mode", "--dangerously-skip-permissions"],
        "codex" => &["--ask-for-approval", "-a", "--sandbox", "-s", "--full-auto", "--yolo", "--approve-for-me", "--dangerously-bypass-approvals-and-sandbox"],
        "kimi" => &["--auto", "--yolo", "-y", "--plan"],
        _ => &[],
    };
    argv[..at].iter().any(|v|explicit.contains(&v.split('=').next().unwrap_or("")))
}
/// Append already-resolved native switches only to the spawned argv; saved
/// resume recipes stay unchanged so returning to provider settings takes effect
/// on the next start.
pub fn apply_permission_values(provider: &str, values: &[String], argv: &mut Vec<String>) {
    // An explicit per-launch mode takes precedence over the account default.
    if values.is_empty() || argv.is_empty() || explicit_permissions(provider, argv) { return; }
    let at = argv.iter().position(|arg|arg=="--").unwrap_or(argv.len());
    argv.splice(at..at, values.iter().cloned());
}
/// Append native switches only to the spawned argv; saved resume recipes stay
/// unchanged so returning to provider settings takes effect on the next start.
#[cfg(test)]
pub fn apply_permissions(provider: &str, mode: &str, argv: &mut Vec<String>) -> Result<()> {
    // Explicit native flags or an empty argv need no account-mode resolution;
    // a stale or unavailable account default must not block them.
    if argv.is_empty() || explicit_permissions(provider, argv) {
        return Ok(());
    }
    let values = permission_args(provider, mode)?;
    apply_permission_values(provider, &values, argv);
    Ok(())
}

pub fn prepare_claude_settings() -> Result<()> {
    let account = env::var("HGS_ACCOUNT_ID").unwrap_or_default();
    if account.is_empty() || account.starts_with("native-") { return Ok(()); }
    let profile = env::var("CLAUDE_CONFIG_DIR").ok().filter(|v|!v.is_empty())
        .ok_or_else(||Error::new(1,"Managed Claude profile directory is missing."))?;
    let home = platform::home().ok_or_else(||Error::new(1,"User home is missing."))?;
    claude_settings::inherit(&Path::new(&home).join(".claude"),Path::new(&profile))
}
/// Managed profiles use the user's MCP servers; provider logins stay separate.
pub fn prepare_mcp_servers(agent: &str) -> Result<()> {
    let account = env::var("HGS_ACCOUNT_ID").unwrap_or_default();
    if account.is_empty() || account.starts_with("native-") { return Ok(()); }
    let Ok(key) = home_var(agent) else { return Ok(()) };
    let profile = env::var(key).ok().filter(|v|!v.is_empty())
        .ok_or_else(||Error::new(1,"Managed agent profile directory is missing."))?;
    let home = platform::home().ok_or_else(||Error::new(1,"User home is missing."))?;
    mcp_servers::inherit(agent,Path::new(&home),Path::new(&profile))
}
/// U53: an account a running session may move to: it must belong to the
/// session's own agent and be signed in on this computer. The owner chooses it.
pub(crate) fn switch_target(config: &Config, id: &str, provider: &str) -> Result<(String, PathBuf)> {
    let (profile, home, native) = lookup(config, id)?;
    if profile.provider != provider {
        return Err(Error::new(
            1,
            format!(
                "Account '{id}' belongs to another agent ({}); a session moves only to an account of its own agent.",
                profile.provider
            ),
        ));
    }
    if !native && !home.is_dir() {
        return Err(Error::new(
            1,
            format!("Account '{id}' has no profile folder on this computer; log in to it first."),
        ));
    }
    if !signed_in(&home, &profile.provider, native) {
        return Err(Error::new(
            1,
            format!(
                "Account '{id}' is not signed in on {}; log in to it before moving a session.",
                config.host()
            ),
        ));
    }
    Ok((profile.label, home))
}

/// Whether the account has a sign-in to run with: a credential file, a keychain
/// item (macOS Claude), or the provider's key in this process's environment.
fn signed_in(home: &Path, provider: &str, native: bool) -> bool {
    if credential_file(home, provider) {
        return true;
    }
    let key = |name: &str| env::var(name).ok().is_some_and(|value| !value.is_empty());
    match provider {
        "claude" => {
            key("ANTHROPIC_API_KEY")
                || (cfg!(target_os = "macos") && claude::missing_login_status(home, native) != "signed_out")
        }
        "codex" => key("OPENAI_API_KEY") || key("CODEX_API_KEY") || key("CODEX_ACCESS_TOKEN"),
        "kimi" => key("KIMI_API_KEY") || key("MOONSHOT_API_KEY"),
        _ => false,
    }
}

/// U53: an account id's label, as `inspect` and `ls` show it beside the id.
pub(crate) fn label(config: &Config, id: &str) -> String {
    let mut labels = labels(config);
    labels.remove(id).unwrap_or_else(|| id.to_owned())
}
/// U53: every known account id's label: the native accounts and the profiles.
pub(crate) fn labels(config: &Config) -> BTreeMap<String, String> {
    let catalog = read(config).ok().and_then(|bytes| decode(&bytes).ok()).unwrap_or_default();
    let mut labels: BTreeMap<String, String> = ["codex", "claude", "kimi"]
        .into_iter()
        .map(|provider| {
            let id = format!("native-{provider}");
            let label = catalog.native_labels.get(&id).cloned().unwrap_or_else(|| default_label(provider).into());
            (id, label)
        })
        .collect();
    for provider in ["dsh", "opencode"] {
        let id = format!("native-{provider}");
        labels.insert(
            id.clone(),
            catalog.native_labels.get(&id).cloned().unwrap_or_else(|| default_label(provider).into()),
        );
    }
    for (id, profile) in &catalog.profiles {
        labels.insert(id.clone(), profile.label.clone());
    }
    labels
}

pub fn select(config: &Config, id: &str, provider: &str) -> Result<Vec<String>> {
    let (profile, home, native) = lookup(config, id)?;
    if profile.provider != provider {
        return Err(Error::new(
            1,
            "The selected account belongs to a different agent.",
        ));
    }
    if !native && !home.is_dir() {
        return Err(Error::new(
            1,
            "Account profile home is missing; create the profile or log in first.",
        ));
    }
    let key = home_var(provider)?;
    // On macOS Claude distinguishes an unset config dir from an explicitly set
    // default path in its keychain namespace. Preserve the native default exactly.
    let selected_home = if native && env::var(key).ok().is_none_or(|s| s.is_empty()) {
        String::new()
    } else {
        home.to_string_lossy().into_owned()
    };
    if selected_home.is_empty() {
        env::remove_var(key);
    } else {
        env::set_var(key, &selected_home);
    }
    let mut environment = vec![
        format!("{key}={selected_home}"),
        format!("HGS_ACCOUNT_ID={id}"),
    ];
    if !native {
        if provider == "claude" {
            let config_home = home.join("anthropic");
            env::set_var("ANTHROPIC_CONFIG_DIR", &config_home);
            environment.push(format!("ANTHROPIC_CONFIG_DIR={}", config_home.display()));
        }
        for key in AUTH_ENV {
            env::remove_var(key);
            environment.push(format!("{key}="));
        }
        if let Some(local) = local_model::read(&home).filter(|_| provider == "claude") {
            for (key, value) in local_model::environment(&local) {
                env::set_var(&key, &value);
                environment.push(format!("{key}={value}"));
            }
        }
    }
    Ok(environment)
}

/// The variables that run a local model account's session on another model of
/// the same endpoint (`--local-model`), without changing the account.
pub fn local_model_session(config: &Config, id: &str, provider: &str, model: &str) -> Result<Vec<String>> {
    let (profile, home, native) = lookup(config, id)?;
    let local = local_model::read(&home).filter(|_| !native && provider == "claude" && profile.provider == "claude");
    let Some(mut local) = local else {
        return Err(Error::new(1, "--local-model needs a local model account (hgs account add ID --provider claude --ollama MODEL)."));
    };
    local.model = model.into();
    local_model::validate(&local)?;
    Ok(local_model::environment(&local).into_iter().map(|(key, value)| {
        env::set_var(&key, &value);
        format!("{key}={value}")
    }).collect())
}

/// Starts a local model account's endpoint (a proxy before Ollama, say) when it
/// does not answer, so a new session's first request finds it.
pub fn prepare_local_model(config: &Config, id: &str, provider: &str) -> Result<()> {
    let (profile, home, native) = lookup(config, id)?;
    if native || provider != "claude" || profile.provider != "claude" {
        return Ok(());
    }
    match local_model::read(&home) {
        Some(local) => local_model::prepare(&local),
        None => Ok(()),
    }
}
/// The variables, as `var` reads them, that hold the account profile of the
/// session a command runs in: with `HGS_ACCOUNT_ID` naming a profile, not a
/// native login, an agent home equal to that profile's (`home` of its ID) and
/// the Anthropic configuration in it, as `select` and `run_environment` set
/// them. A value that differs is not the profile's and is kept.
fn session_profile_variables(
    home: impl Fn(&str) -> PathBuf,
    var: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<&'static str> {
    let Some(id) = var("HGS_ACCOUNT_ID")
        .and_then(|id| id.into_string().ok())
        .filter(|id| !id.is_empty() && !id.starts_with("native-"))
    else {
        return Vec::new();
    };
    let home = home(&id);
    let is = |key: &str, path: &Path| var(key).is_some_and(|value| Path::new(&value) == path);
    let mut found: Vec<&'static str> = ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "KIMI_CODE_HOME"]
        .into_iter()
        .filter(|key| is(key, &home))
        .collect();
    if is("ANTHROPIC_CONFIG_DIR", &home.join("anthropic")) {
        found.push("ANTHROPIC_CONFIG_DIR");
    }
    found
}
/// A launch from inside a session starts a session of its own. The account
/// profile that the enclosing session runs under is in this process's
/// environment, but it is not this computer's native login: drop it, so that
/// the new session's account (`--account`, the default, or none) alone decides
/// its home, and a native one never runs on the caller's profile.
pub fn forget_session_profile(config: &Config) {
    for key in session_profile_variables(|id| profile_home(config, id), |key| env::var_os(key)) {
        env::remove_var(key);
    }
}
fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|x| x == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}
fn check_revision(bytes: &[u8], args: &[String]) -> Result<()> {
    if arg(args, "--revision").is_some_and(|expected| expected != revision(bytes)) {
        return Err(Error::new(
            1,
            "Accounts changed on this machine. Refresh before saving.",
        ));
    }
    Ok(())
}
pub fn dispatch(config: &Config, args: &[String], dry: bool) -> Result<i32> {
    let dry = dry || args.iter().any(|arg| arg == "--dry-run");
    let operation = args.first().map(String::as_str).unwrap_or("ls");
    let id = args.get(1).map(String::as_str).unwrap_or("");
    if !platform::local_sessions() {
        // No agents run here: an empty catalog, and copies between remote machines.
        // As on Unix, `ls` ignores further arguments and `default` lists without an ID.
        match operation {
            "ls" | "default" if operation == "ls" || id.is_empty() => {
                println!("{}", json!({"host":config.host(),"profiles":[],"removed_profiles":[],"defaults":{},"revision":revision(b"")}));
                return Ok(0);
            }
            "copy" => {}
            _ => return Err(Error::new(2, platform::LOCAL_SESSIONS_UNSUPPORTED)),
        }
    }
    // A terminal may execute well after the Accounts dialog was confirmed.
    // New clients pin its catalog; legacy terminal calls remain compatible.
    if matches!(operation, "login" | "install" | "start") && arg(args, "--revision").is_some() {
        let _lock = locked(config)?;
        check_revision(&read(config)?, args)?;
    }
    match operation {
        "ls"=>{println!("{}",list(config)?);},
        "inspect"=>{println!("{}",inspection::inspect(config,args)?);},
        "model-settings"=>{println!("{}",model_defaults::inspect(config,id)?);},
        "model-defaults"=>{println!("{}",model_defaults::set(config,args,dry)?);},
        "set-key"=>{println!("{}",deepseek::set_key(config,args,dry)?);},
        "install"=>return install::run(id,dry),
        "bind-existing"=>{
            if id!="native-opencode" || args.len()!=3 || args[2]!="--json" {return Err(Error::new(2,"usage: hgs [@host] account bind-existing native-opencode --json (JSON stdin)"));}
            #[cfg(unix)]
            {println!("{}",crate::state::opencode::bind_existing(config,dry).map_err(|e|Error::new(1,e))?);}
            #[cfg(not(unix))]
            return Err(Error::new(2,"Bind OpenCode on its Linux or macOS environment; select its WSL or SSH machine"));
        },
        "start"=>{
            if id!="native-opencode" {return Err(Error::new(1,"Only the native OpenCode account has a service to start"));}
            #[cfg(unix)]
            {if dry {println!("Start the shared OpenCode service");return Ok(0);}println!("{}",crate::state::opencode::start_from_accounts().map_err(|e|Error::new(1,e))?);}
            #[cfg(not(unix))]
            return Err(Error::new(2,platform::LOCAL_SESSIONS_UNSUPPORTED));
        },
        "models"=>{
            if id!="native-opencode" {return Err(Error::new(1,"Only the native OpenCode account lists OpenCode models"));}
            // A dangling --location is an empty folder, refused like any other relative one.
            #[cfg(unix)]
            {let location=args.iter().position(|x|x=="--location").map(|i|PathBuf::from(args.get(i+1).map(String::as_str).unwrap_or("")));println!("{}",crate::state::opencode::models(location.as_deref()).map_err(|e|Error::new(1,e))?);}
            #[cfg(not(unix))]
            return Err(Error::new(2,platform::LOCAL_SESSIONS_UNSUPPORTED));
        },
        "favorite"=>{
            if id!="native-opencode" {return Err(Error::new(1,"Only the native OpenCode account has favorite models"));}
            let model=arg(args,"--model").ok_or_else(||Error::new(1,"Choose --model PROVIDER/MODEL"))?;
            let favorite=match arg(args,"--favorite") {Some("on")=>true,Some("off")=>false,_=>return Err(Error::new(1,"Choose --favorite on or off"))};
            let request=arg(args,"--request-id").ok_or_else(||Error::new(1,"A favorite change needs --request-id UUID"))?;
            crate::state::opencode::validate_favorite(model,request).map_err(|e|Error::new(1,e))?;
            if dry {println!("{}",json!({"dry_run":true,"model":model,"favorite":favorite}));return Ok(0);}
            #[cfg(unix)]
            {let _lock=locked(config)?;println!("{}",crate::state::opencode::set_favorite(&config.dir,model,favorite,request).map_err(|e|Error::new(1,e))?);}
            #[cfg(not(unix))]
            {return Err(Error::new(2,platform::LOCAL_SESSIONS_UNSUPPORTED));}
        },
        "default"=>{
            if id.is_empty() {println!("{}",list(config)?);return Ok(0);}
            let _lock=locked(config)?;let bytes=read(config)?;check_revision(&bytes,args)?;let mut catalog=decode(&bytes)?;
            let (profile,_,_)=lookup(config,id)?;
            home_var(&profile.provider)?;
            if catalog.hidden_profiles.contains(id) {return Err(Error::new(1,"Restore this account before making it the default."));}
            catalog.defaults.insert(profile.provider,id.into());
            if !dry {save(config,&catalog)?;}println!("{}",list(config)?);
        },
        "permissions"=>{
            let mode=arg(args,"--mode").ok_or_else(||Error::new(1,"Choose provider, bypass or a native permission selection."))?;
            // Probe and validate outside the account lock. The lock is taken
            // only for the revision-checked write below.
            let bytes=read(config)?;check_revision(&bytes,args)?;let original=bytes.clone();let snapshot=decode(&bytes)?;
            if !native_id(id) && !snapshot.profiles.contains_key(id) {return Err(Error::new(1,"Account profile does not exist."));}
            if id=="native-opencode" && mode!="provider" {return Err(Error::new(1,"OpenCode permissions are managed in its native configuration"));}
            let provider=if native_id(id) {id.trim_start_matches("native-").to_owned()} else {snapshot.profiles[id].provider.clone()};
            validate_permission_mode(&provider,mode)?;
            let home=if native_id(id) {native_home(config,&provider)} else {profile_home(config,id)};
            permission_catalog::validate_for_account(config,&provider,&home,mode)?;
            let normalized=normalize_permission_mode(mode)?;
            let _lock=locked(config)?;let bytes=read(config)?;check_revision(&bytes,args)?;
            if bytes != original {return Err(Error::new(1,"Account catalog changed while its permission options were checked; retry."));}
            let mut catalog=decode(&bytes)?;
            if !native_id(id) && !catalog.profiles.contains_key(id) {return Err(Error::new(1,"Account profile does not exist."));}
            let current_provider=if native_id(id) {id.trim_start_matches("native-").to_owned()} else {catalog.profiles[id].provider.clone()};
            if current_provider != provider {return Err(Error::new(1,"Account changed while its permission options were checked; retry."));}
            let current_home=if native_id(id) {native_home(config,&current_provider)} else {profile_home(config,id)};
            if current_home != home {return Err(Error::new(1,"Account changed while its permission options were checked; retry."));}
            if normalized=="provider" {catalog.permission_modes.remove(id);}
            else {catalog.permission_modes.insert(id.into(),normalized);}
            if !dry {save(config,&catalog)?;}println!("{}",list(config)?);
        },
        "rename"=>{
            let label=arg(args,"--label").ok_or_else(||Error::new(1,"Account name is required."))?.trim();validate_label(label)?;
            let _lock=locked(config)?;let bytes=read(config)?;check_revision(&bytes,args)?;let mut catalog=decode(&bytes)?;
            if ["native-codex","native-claude","native-kimi","native-dsh","native-opencode"].contains(&id) {
                catalog.native_labels.insert(id.into(),label.into());
            } else {
                catalog.profiles.get_mut(id).ok_or_else(||Error::new(1,"Account profile does not exist."))?.label=label.into();
            }
            if !dry {save(config,&catalog)?;}println!("{}",list(config)?);
        },
        "add"=>{
            let p=Profile{provider:arg(args,"--provider").unwrap_or("").into(),label:arg(args,"--label").unwrap_or(id).into()};validate(id,&p)?;
            // WhiteWh fork: a Claude Code account that runs a local Ollama model.
            let local=match arg(args,"--ollama") {
                None=>None,
                Some(model)=>{
                    if p.provider!="claude" {return Err(Error::new(1,"--ollama makes a Claude Code account; use --provider claude."));}
                    let context=match arg(args,"--context") {None=>0,Some(v)=>v.parse().map_err(|_|Error::new(1,"--context needs a number of tokens."))?};
                    let prepare=match arg(args,"--prepare") {None=>Vec::new(),Some(v)=>local_model::split_command(v)?};
                    let local=local_model::LocalModel{endpoint:arg(args,"--endpoint").unwrap_or(local_model::DEFAULT_ENDPOINT).into(),model:model.into(),context,prepare};
                    local_model::validate(&local)?;Some(local)
                }
            };
            let _lock=locked(config)?;let bytes=read(config)?;check_revision(&bytes,args)?;let mut catalog=decode(&bytes)?;
            if catalog.profiles.contains_key(id) || profile_home(config,id).exists() {return Err(Error::new(1,"Account ID or its profile directory already exists. Choose another ID."));}
            if dry {println!("{}",json!({"dry_run":true,"id":id,"provider":p.provider}));return Ok(0);}
            private_dir(&config.dir.join("accounts"))?;private_dir(&profile_home(config,id))?;
            if p.provider == "claude" { private_dir(&profile_home(config,id).join("anthropic"))?; }
            if p.provider == "codex" {
                let path = profile_home(config,id).join("config.toml");
                let mut file = fs::OpenOptions::new().create_new(true).write(true).open(path)?;
                platform::make_private(&file)?;
                file.write_all(b"cli_auth_credentials_store = \"file\"\n")?; file.sync_all()?;
            }
            if let Some(local)=&local {local_model::write(&profile_home(config,id),local)?;}
            catalog.profiles.insert(id.into(),p);save(config,&catalog)?;println!("{}",list(config)?);
        },
        "ollama-models"=>{
            let endpoint=arg(args,"--endpoint").unwrap_or(local_model::DEFAULT_ENDPOINT);
            let details=local_model::models(endpoint)?;
            let names:Vec<Value>=details.iter().map(|m|m["name"].clone()).collect();
            println!("{}",json!({"endpoint":endpoint,"models":names,"details":details}));
        },
        "rm"|"restore"=>{
            let _lock=locked(config)?;let bytes=read(config)?;check_revision(&bytes,args)?;let mut catalog=decode(&bytes)?;
            if !native_id(id) && !catalog.profiles.contains_key(id){return Err(Error::new(1,"Account profile does not exist."));}
            if operation=="rm" {catalog.hidden_profiles.insert(id.into());catalog.defaults.retain(|_, selected| selected!=id);}
            else if !catalog.hidden_profiles.remove(id){return Err(Error::new(1,"Account profile is already in the catalog."));}
            if !dry {save(config,&catalog)?;}println!("{}",list(config)?);
        },
        "copy"=>{println!("{}",copy_account(config,args,dry)?);},
        "_export"=>{
            use std::io::IsTerminal;
            if std::io::stdout().is_terminal() {return Err(Error::new(1,"Credential transport cannot write to a terminal. Use account copy."));}
            if dry{return Err(Error::new(1,"Credential transport has no dry-run mode."));}
            let _lock=if arg(args,"--revision").is_some()||arg(args,"--auth-revision").is_some(){Some(locked(config)?)}else{None};
            check_revision(&read(config)?,args)?;
            check_export_auth(config,id,args)?;
            let bytes=export(config,id)?;
            check_export_auth(config,id,args)?;
            std::io::stdout().write_all(&bytes)?;
        },
        "_import"=>{
            use std::io::{IsTerminal,Read};
            if std::io::stdin().is_terminal() || dry {return Err(Error::new(1,"Credential transport requires a private input pipe. Use account copy."));}
            if args.iter().any(|a|a=="--guarded-stream") {
                // The physical identity guard has already passed in cli.rs.
                // Admit the catalog too before the sender releases any bytes.
                {let _lock=locked(config)?;check_revision(&read(config)?,args)?;}
                std::io::stdout().write_all(IMPORT_READY)?;
                std::io::stdout().flush()?;
            }
            let mut payload=Vec::new();std::io::stdin().take((MAX_TRANSFER+1)as u64).read_to_end(&mut payload)?;
            println!("{}",import(config,id,arg(args,"--label").unwrap_or(id),crate::state::without_bom(&payload),args)?);
        },
        "login"=>{
            if id=="native-dsh" {return crate::native_ui::open(config,"","@account",dry);}
            if id=="native-opencode" {
                const GUIDANCE: &str = "Connect OpenCode providers with `opencode auth login` in a terminal on this computer";
                if dry {println!("{GUIDANCE}");return Ok(0);}
                return Err(Error::new(1,GUIDANCE));
            }
            let (p,home,native)=lookup(config,id)?; let key=home_var(&p.provider)?;
            let arguments: Vec<&str> = if p.provider == "claude" && !native { vec!["auth", "login", "--claudeai"] }
                else if p.provider == "claude" { vec!["auth", "login"] } else { vec!["login"] };
            if dry {println!("{}",json!({"provider":p.provider,"home":home,"arguments":arguments}));return Ok(0);}
            if !native {private_dir(&home)?;}
            let mut command = inspection::native_command(&p.provider, &home, native);
            command.args(arguments);
            if !native || env::var(key).is_ok_and(|s| !s.is_empty()) { command.env(key, &home); }
            else { command.env_remove(key); }
            if !native {
                if p.provider == "claude" { command.env("ANTHROPIC_CONFIG_DIR", home.join("anthropic")); }
                for key in AUTH_ENV {command.env_remove(key);}
            }
            let kimi_before = if cfg!(windows) && p.provider == "kimi" { inspection::kimi_login_grant(&home) } else { None };
            let code = platform::wait_interactive(&mut command)?;
            if code == 0 && p.provider == "claude" {
                let verified = if native { claude::verify_login(&home, true) } else { claude::finish_login(&home) };
                if let Err(error) = verified {
                    return Err(Error::new(1, format!("Sign-in setup is incomplete: {}", error.message)));
                }
                eprintln!("hgs: Sign-in verified for this profile.");
            }
            let kimi_after = if cfg!(windows) && p.provider == "kimi" { inspection::kimi_login_grant(&home) } else { None };
            // The Windows C runtime uses exit 3 for the libuv assertion. Native
            // login errors (including incomplete model setup) keep their code.
            let kimi_changed = cfg!(windows) && p.provider == "kimi" && code == 3
                && kimi_after.is_some() && kimi_before != kimi_after;
            if code == 0 || kimi_changed {
                let report = inspection::inspect(config, &["inspect".into(), id.into(), "--refresh".into()]);
                if kimi_changed && report.as_ref().is_ok_and(|report| inspection::kimi_login_recovered(code, kimi_before.as_deref(), kimi_after.as_deref(), report)) {
                    eprintln!("hgs: Kimi exited with code {code} after saving credentials. Sign-in verified for this profile.");
                    return Ok(0);
                }
            }
            return Ok(code);
        },
        _=>return Err(Error::new(1,"usage: hgs account ls | add ID --provider codex|claude|kimi --label NAME [--ollama MODEL [--endpoint URL] [--context TOKENS] [--prepare COMMAND]] | ollama-models [--endpoint URL] | rm ID | login ID | favorite native-opencode --model PROVIDER/MODEL --favorite on|off --request-id UUID | models native-opencode [--location DIR]")),
    }
    Ok(0)
}
// Export/import are private transport endpoints. Only the coordinator reports a receipt.
const MAX_TRANSFER: usize = 2 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transfer {
    version: u8,
    provider: String,
    files: BTreeMap<String, String>,
}
fn safe_file(home: &Path, name: &str) -> Result<Vec<u8>> {
    let path = home.join(name);
    let canonical = platform::canonical(&path).map_err(|_| {
        Error::new(
            1,
            "Credential file is unavailable. Sign in on the destination machine.",
        )
    })?;
    if !canonical.starts_with(platform::canonical(home)?)
        || !fs::metadata(&canonical)?.is_file()
        || fs::metadata(&canonical)?.len() > MAX_TRANSFER as u64
    {
        return Err(Error::new(
            1,
            "Credential file is outside the profile or exceeds the size limit.",
        ));
    }
    let bytes = fs::read(canonical)?;
    if bytes.len() > MAX_TRANSFER {
        return Err(Error::new(1, "Credential file exceeds the size limit."));
    }
    Ok(bytes)
}
fn export(config: &Config, id: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    let (profile, home, _) = lookup(config, id)?;
    let mut files = BTreeMap::new();
    match profile.provider.as_str() {
        "codex"=>{
            let bytes=safe_file(&home,"auth.json")?;
            serde_json::from_slice::<Value>(&bytes).map_err(|_|Error::new(1,"Invalid native credential file. Sign in on the destination machine."))?;
            files.insert("auth.json".into(),base64::engine::general_purpose::STANDARD.encode(bytes));
            files.insert("config.toml".into(),base64::engine::general_purpose::STANDARD.encode(b"cli_auth_credentials_store = \"file\"\n"));
        },
        "kimi"=>{
            let raw=safe_file(&home,"config.toml")?;
            let document=String::from_utf8(raw).ok().and_then(|s|s.parse::<toml_edit::DocumentMut>().ok()).ok_or_else(||Error::new(1,"Invalid Kimi configuration."))?;
            let mut selected=toml_edit::DocumentMut::new();
            for key in ["default_model","default_thinking","default_reasoning_effort","providers","models"] {if document.contains_key(key) {selected[key]=document[key].clone();}}
            files.insert("config.toml".into(),base64::engine::general_purpose::STANDARD.encode(selected.to_string()));
            if let Ok(entries)=fs::read_dir(home.join("credentials")) {
                for entry in entries.take(65) {
                    let entry=entry?;let name=entry.file_name().to_string_lossy().into_owned();
                    if !valid_credential_name(&name) {continue;}
                    if files.len()>64 {return Err(Error::new(1,"Too many credential files in this profile."));}
                    let relative=format!("credentials/{name}");let bytes=safe_file(&home,&relative)?;
                    serde_json::from_slice::<Value>(&bytes).map_err(|_|Error::new(1,"Invalid native credential file."))?;
                    files.insert(relative,base64::engine::general_purpose::STANDARD.encode(bytes));
                }
            }
            let inline_key = document.get("providers").and_then(toml_edit::Item::as_table_like)
                .is_some_and(|providers| providers.iter().any(|(_, item)| item.as_table_like()
                    .and_then(|provider| provider.get("api_key")).and_then(toml_edit::Item::as_str)
                    .is_some_and(|key| !key.trim().is_empty())));
            if files.len() == 1 && !inline_key {return Err(Error::new(1,"No portable credentials were found. Sign in on the destination machine."));}
        },
        _=>return Err(Error::new(1,"Claude credentials can use the system keychain. Create a profile and sign in on the destination machine.")),
    }
    let payload = serde_json::to_vec(&Transfer {
        version: 1,
        provider: profile.provider,
        files,
    })
    .map_err(|_| Error::new(1, "Cannot prepare account transfer."))?;
    if payload.len() > MAX_TRANSFER {
        return Err(Error::new(1, "Account transfer exceeds the size limit."));
    }
    Ok(payload)
}
fn valid_credential_name(name: &str) -> bool {
    name.ends_with(".json")
        && !name.starts_with('.')
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
fn check_export_auth(config: &Config, id: &str, args: &[String]) -> Result<()> {
    if let Some(expected)=arg(args,"--auth-revision") {
        let (profile,_,native)=lookup(config,id)?;
        if expected.is_empty() || row(config,id,&profile,native)["auth_revision"] != expected {
            return Err(Error::new(1,"The source sign-in changed. Refresh Accounts before copying."));
        }
    }
    Ok(())
}
fn import(config: &Config, id: &str, label: &str, payload: &[u8], args: &[String]) -> Result<Value> {
    use base64::Engine;
    if payload.len() > MAX_TRANSFER {
        return Err(Error::new(1, "Account transfer exceeds the size limit."));
    }
    let transfer: Transfer = serde_json::from_slice(payload)
        .map_err(|_| Error::new(1, "Invalid account transfer payload."))?;
    let profile = Profile {
        provider: transfer.provider.clone(),
        label: label.into(),
    };
    validate(id, &profile)?;
    if transfer.version != 1
        || !matches!(transfer.provider.as_str(), "codex" | "kimi")
        || transfer.files.is_empty()
        || transfer.files.len() > 65
    {
        return Err(Error::new(1, "Unsupported account transfer."));
    }
    if !transfer.files.contains_key("config.toml")
        || (transfer.provider == "codex" && !transfer.files.contains_key("auth.json"))
    {
        return Err(Error::new(
            1,
            "Account transfer is missing required native files.",
        ));
    }
    let mut files = BTreeMap::new();
    for (name, data) in transfer.files {
        let allowed = match transfer.provider.as_str() {
            "codex" => matches!(name.as_str(), "auth.json" | "config.toml"),
            "kimi" => {
                name == "config.toml"
                    || name
                        .strip_prefix("credentials/")
                        .is_some_and(valid_credential_name)
            }
            _ => false,
        };
        if !allowed {
            return Err(Error::new(
                1,
                "Account transfer contains an unsupported file.",
            ));
        }
        let data = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| Error::new(1, "Invalid account transfer encoding."))?;
        if name == "config.toml" {
            let document = std::str::from_utf8(&data)
                .ok()
                .and_then(|s| s.parse::<toml_edit::DocumentMut>().ok())
                .ok_or_else(|| Error::new(1, "Invalid account transfer configuration."))?;
            let allowed_keys: &[&str] = if transfer.provider == "codex" {
                &["cli_auth_credentials_store"]
            } else {
                &[
                    "default_model",
                    "default_thinking",
                    "default_reasoning_effort",
                    "providers",
                    "models",
                ]
            };
            if document.iter().any(|(key, _)| !allowed_keys.contains(&key)) {
                return Err(Error::new(
                    1,
                    "Account transfer contains unrelated settings.",
                ));
            }
        } else {
            serde_json::from_slice::<Value>(&data)
                .map_err(|_| Error::new(1, "Invalid account credential encoding."))?;
        }
        files.insert(name, data);
    }
    let _lock = locked(config)?;
    let bytes=read(config)?;
    check_revision(&bytes,args)?;
    let mut catalog = decode(&bytes)?;
    if catalog.profiles.contains_key(id) || profile_home(config, id).exists() {
        return Err(Error::new(
            1,
            "Destination account ID already exists. No credentials were overwritten.",
        ));
    }
    let root = config.dir.join("accounts");
    private_dir(&root)?;
    let staging = tempfile::tempdir_in(&root)?;
    private_dir(staging.path())?;
    for (name, data) in files {
        let path = staging.path().join(name);
        if let Some(parent) = path.parent() {
            private_dir(parent)?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        platform::make_private(&file)?;
        file.write_all(&data)?;
        file.sync_all()?;
    }
    fs::rename(staging.path(), profile_home(config, id))?;
    catalog.profiles.insert(id.into(), profile.clone());
    save(config, &catalog)?;
    Ok(json!({"ok":true,"id":id,"provider":profile.provider,"host":config.host()}))
}
fn transport(
    config: &Config,
    host: &str,
    args: &[String],
    input: Option<&[u8]>,
    ready: bool,
) -> Result<Vec<u8>> {
    use std::io::Read;
    use std::process::Stdio;
    let mut command;
    if host.is_empty() || host == "@local" || config.is_self(host) {
        command = Command::new(env::current_exe()?);
        command.args(args);
    } else {
        if !config.peers.iter().any(|s| s == host) {
            return Err(Error::new(
                1,
                "Account transfer requires a configured, enabled machine.",
            ));
        }
        command = platform::ssh();
        command.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ConnectTimeout=8",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=2",
        ]);
        command
            .args(crate::machines::ssh_options(config, host))
            .arg(host)
            .arg(crate::windows_peer::hgs(config, host, args)?);
    }
    command.stdin(Stdio::null()).stderr(Stdio::null());
    let ssh = platform::spawn_ssh(
        platform::own_process_group(&mut command),
        input.is_some(),
        true,
        false,
    )?;
    let mut child = ssh.child;
    let (write_tx, write_rx) = std::sync::mpsc::channel();
    let (ready_tx,ready_rx)=std::sync::mpsc::channel();
    if let Some(bytes) = input {
        let bytes = bytes.to_vec();
        let mut pipe = ssh.stdin.unwrap();
        std::thread::spawn(move || {
            let admitted=!ready || ready_rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap_or(false);
            let _ = write_tx.send(admitted && pipe.write_all(&bytes).is_ok());
        });
    }
    let (read_tx, read_rx) = std::sync::mpsc::channel();
    let mut pipe = ssh.stdout.unwrap();
    std::thread::spawn(move || {
        if ready {
            let mut marker=[0;IMPORT_READY.len()];
            let admitted=pipe.read_exact(&mut marker).is_ok() && marker==IMPORT_READY;
            let _=ready_tx.send(admitted);
            if !admitted {let _=read_tx.send(Err(std::io::Error::other("Destination did not admit the guarded transfer")));return;}
        }
        let mut output = Vec::new();
        let result = pipe
            .take((MAX_TRANSFER + 1) as u64)
            .read_to_end(&mut output)
            .map(|_| output);
        let _ = read_tx.send(result);
    });
    let started = std::time::Instant::now();
    let mut status = None;
    let mut output = None;
    let mut written = if input.is_some() { None } else { Some(true) };
    while started.elapsed().as_secs() < 20 {
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(result)) => {
                    status = Some(result);
                    // A proxy/helper may retain inherited pipe handles after its parent exits.
                    platform::kill_process_group(&mut child);
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
        if output.is_none() {
            if let Ok(value) = read_rx.try_recv() {
                output = Some(value);
            }
        }
        if written.is_none() {
            if let Ok(value) = write_rx.try_recv() {
                written = Some(value);
            }
        }
        if status.is_some() && output.is_some() && written.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if status.is_none() {
        platform::kill_process_group(&mut child);
        let _ = child.wait();
    }
    // Never join unfinished I/O workers: even a helper escaping its process group
    // cannot extend the operation deadline. Process exit closes any remaining pipe.
    let out = output.and_then(std::result::Result::ok);
    if !status.is_some_and(|s| s.success())
        || written != Some(true)
        || out.as_ref().is_none_or(|b| b.len() > MAX_TRANSFER)
    {
        return Err(Error::new(1, "Account transfer failed or timed out. Refresh destination accounts before retrying; verify current HGS, portable credentials and an unused account ID."));
    }
    let out = out.unwrap();
    Ok(out)
}
const IMPORT_READY:&[u8]=b"zerus-account-import-ready-v1\n";
fn copy_account(config: &Config, args: &[String], dry: bool) -> Result<Value> {
    let id = args
        .get(1)
        .ok_or_else(|| Error::new(1, "Source account ID is required."))?;
    let dest = arg(args, "--as").ok_or_else(|| {
        Error::new(
            1,
            "--as NEW_ID is required; existing profiles are never overwritten.",
        )
    })?;
    let label = arg(args, "--label").unwrap_or(dest);
    let from = arg(args, "--from").unwrap_or("@local");
    let to = arg(args, "--to").ok_or_else(|| Error::new(1, "--to MACHINE is required."))?;
    let local = |host: &str| host.is_empty() || host == "@local" || config.is_self(host);
    let guards=["--source-machine","--destination-machine","--source-revision","--destination-revision","--source-auth-revision"];
    let guarded=guards.iter().any(|g|arg(args,g).is_some());
    if guarded && guards.iter().any(|g|arg(args,g).is_none_or(str::is_empty)) {
        return Err(Error::new(1,"Account copy needs both machine identities, catalog revisions and the source sign-in revision."));
    }
    if !platform::local_sessions() && (local(from) || local(to)) {
        return Err(Error::new(2, platform::LOCAL_SESSIONS_UNSUPPORTED));
    }
    validate(
        dest,
        &Profile {
            provider: "codex".into(),
            label: label.into(),
        },
    )?;
    if dry {
        return Ok(json!({"dry_run":true,"source":from,"destination":to,"id":dest}));
    }
    let mut export_args=vec!["account".into(),"_export".into(),id.clone()];
    let mut import_args=vec!["account".into(),"_import".into(),dest.into(),"--label".into(),label.into()];
    if guarded {
        export_args.splice(0..0,["--expect-machine".into(),arg(args,"--source-machine").unwrap().into()]);
        export_args.extend(["--revision".into(),arg(args,"--source-revision").unwrap().into(),"--auth-revision".into(),arg(args,"--source-auth-revision").unwrap().into()]);
        import_args.splice(0..0,["--expect-machine".into(),arg(args,"--destination-machine").unwrap().into()]);
        import_args.extend(["--revision".into(),arg(args,"--destination-revision").unwrap().into()]);
        import_args.push("--guarded-stream".into());
    }
    let exported = transport(
        config,
        from,
        &export_args,
        None,
        false,
    )?;
    let receipt = transport(
        config,
        to,
        &import_args,
        Some(&exported),
        guarded,
    )?;
    serde_json::from_slice(&receipt)
        .map_err(|_| Error::new(1, "Destination returned an invalid account receipt."))
}

/// Account selection is reapplied after shell startup on resume/fork as on initial launch.
#[cfg(unix)]
pub fn run_script(account: &str, provider: &str, home: &str, extra: &[String]) -> String {
    let mut environment = run_environment(account, provider, home);
    environment.extend_from_slice(extra);
    supervisor_script(&environment)
}

/// Explicit assignments follow login-shell startup, ahead of the unchanged
/// supervisor and native arguments. The empty hint exclusion list is explicit.
#[cfg(unix)]
pub fn supervisor_script(environment: &[String]) -> String {
    if environment.is_empty() {
        return "exec \"$0\" --run \"$@\"".into();
    }
    format!(
        "exec env {} \"$0\" --run \"$@\"",
        platform::join(environment)
    )
}

/// The account variables a resumed or forked session's supervisor starts with
/// (`run_script` on Unix; a Windows session's own variables, spec D8): the
/// agent's home, the account ID and, for a profile that is not the native
/// login, a private Anthropic configuration and blank credentials. None without
/// an account.
pub fn run_environment(account: &str, provider: &str, home: &str) -> Vec<String> {
    if account.is_empty() {
        return Vec::new();
    }
    let Ok(key) = home_var(provider) else {
        return Vec::new();
    };
    let mut environment = vec![format!("{key}={home}"), format!("HGS_ACCOUNT_ID={account}")];
    if !account.starts_with("native-") {
        if provider == "claude" {
            environment.push(format!(
                "ANTHROPIC_CONFIG_DIR={}",
                Path::new(home).join("anthropic").display()
            ));
        }
        for key in AUTH_ENV {
            environment.push(format!("{key}="));
        }
    }
    environment
}

/// Apply saved model defaults only when creating a new conversation.
pub fn apply_model_defaults(config: &Config, account: &str, provider: &str, argv: &mut Vec<String>) -> Result<()> {
    model_defaults::apply(config, account, provider, argv)
}

#[cfg(test)]
mod permission_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn agent_hint_effective_values_follow_shell_startup_without_config_redirect_or_argv_change() {
        let hint = vec!["HGS_AGENT_HINT=1".into(), "HGS_AGENT_HINT_OFF=".into()];
        let script = run_script(
            "work-account",
            "claude",
            "/private/profile with space",
            &hint,
        );
        let words = shell_words::split(&script).unwrap();
        assert_eq!(&words[..2], &["exec", "env"]);
        assert_eq!(&words[words.len() - 3..], &["$0", "--run", "$@"]);
        assert!(words.contains(&"CLAUDE_CONFIG_DIR=/private/profile with space".into()));
        assert!(words.contains(&"HGS_ACCOUNT_ID=work-account".into()));
        assert_eq!(&words[words.len() - 5..words.len() - 3], hint.as_slice());
        assert!(!words.iter().any(|word| word.starts_with("HGS_CONFIG_DIR=")));
        assert_eq!(supervisor_script(&[]), "exec \"$0\" --run \"$@\"");
        assert_eq!(
            shell_words::split(&run_script("", "codex", "", &hint)).unwrap(),
            [
                "exec",
                "env",
                "HGS_AGENT_HINT=1",
                "HGS_AGENT_HINT_OFF=",
                "$0",
                "--run",
                "$@"
            ]
        );
        let quoted = vec![
            "HGS_AGENT_HINT=1".into(),
            "HGS_AGENT_HINT_OFF=work 'quoted' $(not-executed)".into(),
        ];
        let words = shell_words::split(&supervisor_script(&quoted)).unwrap();
        assert_eq!(&words[2..4], quoted.as_slice());
    }
    #[test]
    fn a_launch_from_a_profile_session_drops_only_that_profiles_variables() {
        let accounts = Path::new("/private/hgs/accounts");
        let found = |pairs: &[(&str, &str)]| {
            session_profile_variables(
                |id| accounts.join(id),
                |key| {
                    pairs
                        .iter()
                        .find(|(name, _)| *name == key)
                        .map(|(_, value)| std::ffi::OsString::from(value))
                },
            )
        };
        let work = "/private/hgs/accounts/work";
        // A Claude profile session: its agent home and Anthropic configuration.
        // Another agent's home, which its own launch passed on, stays.
        assert_eq!(
            found(&[
                ("HGS_ACCOUNT_ID", "work"),
                ("CLAUDE_CONFIG_DIR", "/private/hgs/accounts/work/"),
                (
                    "ANTHROPIC_CONFIG_DIR",
                    "/private/hgs/accounts/work/anthropic"
                ),
                ("CODEX_HOME", "/custom/codex"),
            ]),
            ["CLAUDE_CONFIG_DIR", "ANTHROPIC_CONFIG_DIR"]
        );
        assert_eq!(
            found(&[
                ("HGS_ACCOUNT_ID", "review"),
                ("CODEX_HOME", "/private/hgs/accounts/review")
            ]),
            ["CODEX_HOME"]
        );
        // A native login, no account, or values that are not the profile's
        // own are this computer's settings and are kept.
        for pairs in [
            &[
                ("HGS_ACCOUNT_ID", "native-claude"),
                ("CLAUDE_CONFIG_DIR", work),
            ][..],
            &[("HGS_ACCOUNT_ID", ""), ("CLAUDE_CONFIG_DIR", work)],
            &[("CLAUDE_CONFIG_DIR", work)],
            &[
                ("HGS_ACCOUNT_ID", "work"),
                ("CLAUDE_CONFIG_DIR", "/elsewhere"),
            ],
            &[("HGS_ACCOUNT_ID", "other"), ("KIMI_CODE_HOME", work)],
            &[("HGS_ACCOUNT_ID", "work"), ("ANTHROPIC_CONFIG_DIR", work)],
        ] {
            assert!(found(pairs).is_empty(), "{pairs:?}");
        }
    }
    #[test]
    fn account_defaults_respect_explicit_launch_mode_and_prompt_delimiter() {
        for (provider, flag) in [("claude","--permission-mode"),("codex","--sandbox"),("kimi","--yolo")] {
            let mut explicit=vec![provider.into(),format!("{flag}=explicit")];let original=explicit.clone();
            apply_permissions(provider,"bypass",&mut explicit).unwrap();assert_eq!(explicit,original);
            let mut prompt=vec![provider.into(),"--".into(),flag.into()];apply_permissions(provider,"bypass",&mut prompt).unwrap();
            assert!(prompt.iter().position(|s|s=="--").unwrap()>1);assert_eq!(prompt.last().unwrap(),flag);
            let saved=prompt.clone();apply_permissions(provider,"bypass",&mut prompt).unwrap();assert_eq!(prompt,saved);
        }
    }
    #[test]
    fn the_designer_is_trusted_in_the_profile_its_launch_uses() {
        // N18: a launch without --account uses the default account; with a
        // managed default that is <config>/accounts/<id>/.claude.json.
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            home: dir.path().to_str().unwrap().into(), dir: dir.path().join("config"), state: dir.path().join("state"),
            selves: vec![], peers: vec![], base_peers: vec![], peers_from_environment: false,
            machines: BTreeMap::new(), tab: false, tab_colors: String::new(),
        };
        assert_eq!(claude_launch_config(&config).unwrap(), claude::account_config().unwrap(), "no catalog: the native config");
        fs::create_dir_all(&config.dir).unwrap();
        fs::write(config.dir.join("accounts.json"), json!({
            "profiles": {"work": {"provider": "claude", "label": "Work"}},
            "defaults": {"claude": "work"}
        }).to_string()).unwrap();
        assert_eq!(claude_launch_config(&config).unwrap(), config.dir.join("accounts").join("work").join(".claude.json"));
        fs::write(config.dir.join("accounts.json"), json!({
            "profiles": {"work": {"provider": "claude", "label": "Work"}},
            "defaults": {"claude": "native-claude"}
        }).to_string()).unwrap();
        assert_eq!(claude_launch_config(&config).unwrap(), claude::account_config().unwrap(), "a native default: the native config");
    }
    #[test]
    fn claude_project_keys_use_forward_slashes_on_windows() {
        // N19: as Claude writes them; the mixed form was another project.
        let mixed = "C:\\Users\\example\\.local/state\\hgs/sessions\\team-designer";
        assert_eq!(claude::project_key(mixed, true), "C:/Users/example/.local/state/hgs/sessions/team-designer");
        assert_eq!(claude::project_key("/home/example/a\\b", false), "/home/example/a\\b");
    }
    #[test]
    fn explicit_agent_flags_win_before_account_discovery() {
        let mut argv = vec!["claude".into(), "--permission-mode".into(), "plan".into(), "--".into(), "task".into()];
        let original = argv.clone();
        apply_permissions("claude", "native:{\"mode\":\"not-installed\"}", &mut argv).unwrap();
        assert_eq!(argv, original);
        let mut empty: Vec<String> = Vec::new();
        apply_permissions("claude", "native:{\"mode\":\"not-installed\"}", &mut empty).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn auto_mode_is_claude_only() {
        let mut claude=vec!["claude".to_owned(),"--".into(),"task".into()];apply_permissions("claude","auto",&mut claude).unwrap();
        assert_eq!(claude,["claude","--permission-mode","auto","--","task"]);
        for provider in ["codex","kimi","dsh","opencode"] {
            let mut argv=vec![provider.to_owned()];assert!(apply_permissions(provider,"auto",&mut argv).is_err());assert_eq!(argv,[provider]);
            assert!(!permission_modes(provider).contains(&"auto"));
        }
        assert!(permission_modes("opencode").is_empty());
        assert!(explicit_permissions("claude",&["claude","--permission-mode","plan"].map(String::from)));
        assert!(!explicit_permissions("claude",&["claude","--","--permission-mode"].map(String::from)));
    }
}
