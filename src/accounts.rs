//! Named, per-machine native agent profiles. The catalog contains labels, never credentials.
mod inspection;
mod claude_usage;
mod model_defaults;
mod codex_catalog;
mod kimi_compaction;
mod claude;
mod claude_settings;
mod mcp_servers;
mod permission_catalog;
mod permission_status;
mod permission_scope;
pub(crate) mod marks;
mod install;
mod deepseek;
mod local_model;
mod deepseek_spend;
mod deepseek_usage;
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
    #[serde(default, skip_serializing_if = "model_defaults::Stored::is_empty")]
    model_defaults: model_defaults::Stored,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    kimi_compact_on_expiry: BTreeMap<String, bool>,
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
        if !["native-codex", "native-claude", "native-kimi", "native-qwen", "native-dsh", "native-opencode"].contains(&id.as_str()) {
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
    for id in catalog.kimi_compact_on_expiry.keys() {
        if id != "native-kimi" && catalog.profiles.get(id).is_none_or(|p| p.provider != "kimi") {
            return Err(Error::new(1, "Invalid Kimi cache-expiry account policy."));
        }
    }
    for (provider, id) in &catalog.defaults {
        if !["codex", "claude", "kimi", "qwen", "hermes"].contains(&provider.as_str())
            || catalog.hidden_profiles.contains(id)
            || (id != &format!("native-{provider}") && catalog.profiles.get(id).is_none_or(|p| &p.provider != provider)) {
            return Err(Error::new(1, "Invalid default account profile."));
        }
    }
    Ok(catalog)
}
pub fn default_account(config: &Config, provider: &str) -> Result<Option<String>> {
    if !["codex", "claude", "kimi", "qwen", "hermes"].contains(&provider) { return Ok(None); }
    Ok(decode(&read(config)?)?.defaults.get(provider).cloned())
}
fn native_id(id: &str) -> bool {
    ["native-codex", "native-claude", "native-kimi", "native-qwen", "native-hermes", "native-dsh", "native-opencode"].contains(&id)
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
        "qwen" => Ok("QWEN_HOME"),
        "hermes" => Ok("HERMES_HOME"),
        _ => Err(Error::new(1, "Accounts support Codex, Claude, Kimi, Qwen Code and Hermes.")),
    }
}
fn native_home(config: &Config, provider: &str) -> PathBuf {
    // WhiteWh fork: Hermes keeps its home in %LOCALAPPDATA%\hermes on Windows.
    if provider == "hermes" {
        if let Some(home) = env::var_os("HERMES_HOME").filter(|v| !v.is_empty()) {
            return PathBuf::from(home);
        }
        if cfg!(windows) {
            if let Some(local) = env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
                return PathBuf::from(local).join("hermes");
            }
        }
        return Path::new(&config.home).join(".hermes");
    }
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
        } else if provider == "qwen" {
            ".qwen"
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
/// Owner, 2026-11-11 (team 86821caa): the accounts of `provider` on this
/// computer a team member can name, as (id, label): the native account first,
/// then the profiles that are not removed, in id order.
pub(crate) fn team_accounts(config: &Config, provider: &str) -> Result<Vec<(String, String)>> {
    let catalog = decode(&read(config)?)?;
    let native = format!("native-{provider}");
    let mut found = Vec::new();
    if !catalog.hidden_profiles.contains(&native) {
        let label = catalog.native_labels.get(&native).cloned().unwrap_or_else(|| default_label(provider).into());
        found.push((native, label));
    }
    for (id, profile) in &catalog.profiles {
        if profile.provider == provider && !catalog.hidden_profiles.contains(id) {
            found.push((id.clone(), profile.label.clone()));
        }
    }
    Ok(found)
}

/// Owner, 2026-11-11: the account a team request names, as its id. A request
/// may name the id or the label the owner sees ("Additional"); a swap card
/// with `"account":"additional"` was accepted, allowed by the owner and failed
/// only at the launch ("Unknown account profile 'additional'"), after the old
/// session had been stopped. An unknown or ambiguous name is refused with the
/// accounts this computer has for the agent.
pub(crate) fn resolve_team_account(config: &Config, provider: &str, name: &str) -> std::result::Result<String, String> {
    let name = name.trim();
    let accounts = team_accounts(config, provider).map_err(|error| error.message)?;
    let listed = || {
        if accounts.is_empty() {
            format!("this computer has no {provider} account")
        } else {
            let names: Vec<String> = accounts.iter().map(|(id, label)| format!("{id} ({label})")).collect();
            format!("{provider} accounts on {}: {}", config.host(), names.join(", "))
        }
    };
    if let Some((id, _)) = accounts.iter().find(|(id, _)| id == name) {
        return Ok(id.clone());
    }
    // An id of another agent's account says so, rather than "unknown".
    if lookup(config, name).is_ok_and(|(profile, _, _)| profile.provider != provider) {
        return Err(format!("Account {name} belongs to another agent, not {provider}; {}", listed()));
    }
    let by_label: Vec<&(String, String)> = accounts.iter().filter(|(_, label)| label.trim().eq_ignore_ascii_case(name)).collect();
    match by_label.as_slice() {
        [(id, _)] => Ok(id.clone()),
        [] => Err(format!("Unknown account \"{name}\" for {provider}: name an account's id or its label; {}", listed())),
        several => Err(format!(
            "The label \"{name}\" names {} accounts ({}); name the id",
            several.len(),
            several.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// The account id a launch of `provider` uses for a member's `account` (empty:
/// this computer's default for the agent, else its native account).
pub(crate) fn effective_team_account(config: &Config, provider: &str, account: &str) -> String {
    if !account.trim().is_empty() {
        return account.trim().to_owned();
    }
    default_account(config, provider).ok().flatten().filter(|id| !id.is_empty()).unwrap_or_else(|| format!("native-{provider}"))
}

/// Resolve a hook health target without inspecting credentials or running a provider.
pub(crate) fn hook_profile(config: &Config, id: &str) -> Result<(String, PathBuf)> {
    let (profile, path, _) = lookup(config, id)?;
    Ok((profile.provider, path))
}

/// Task 115: the home a launch of `provider` reads for `account` (empty: the
/// default managed profile, else the native one). A caller that must prepare a
/// file there before the launch (Kimi's read-only team rules) uses the same
/// resolution the launch itself does.
pub(crate) fn launch_home(config: &Config, provider: &str, account: &str) -> PathBuf {
    let id = if account.is_empty() {
        default_account(config, provider).ok().flatten().unwrap_or_default()
    } else {
        account.to_owned()
    };
    if id.is_empty() || native_id(&id) {
        native_home(config, provider)
    } else {
        profile_home(config, &id)
    }
}

/// Task 115: the owner's opt-in for a Kimi account's read-only hgs rules
/// (`hgs account kimi-read-only [ACCOUNT] [--yes|--remove] [--json]`). A team
/// launch never writes them: without `--yes` this names every exact pattern it
/// would add and writes nothing. `--remove` takes back only those rules.
fn kimi_read_only(config: &Config, account: &str, args: &[String], dry: bool) -> Result<Value> {
    use crate::state::kimi_permissions::{apply, preview, remove, Write};
    let account = if account.starts_with('-') { "" } else { account };
    if !account.is_empty() {
        let (profile, _, _) = lookup(config, account)?;
        if profile.provider != "kimi" {
            return Err(Error::new(1, "The read-only hgs rules are a Kimi account setting."));
        }
    }
    let want_json = args.iter().any(|arg| arg == "--json");
    let home = launch_home(config, "kimi", account);
    if args.iter().any(|arg| arg == "--remove") {
        let removed = remove(&home)
            .map_err(|error| Error::new(1, format!("The Kimi rules were not removed: {error}")))?;
        return Ok(json!({"account":account,"home":home,"removed":removed}));
    }
    let preview = preview(&home)
        .map_err(|error| Error::new(1, format!("The Kimi config was not read: {error}")))?;
    let name = if account.is_empty() { "native" } else { account };
    if !args.iter().any(|arg| arg == "--yes") {
        // The confirmation names the exact patterns and the file they go to.
        if want_json {
            return Ok(json!({"account":account,"home":home,"config":preview.config,
                "patterns":preview.patterns,"present":preview.present,"missing":preview.missing,
                "written":false,
                "confirm":format!("Add {} read-only hgs rule(s) to the Kimi config at {}? Confirm with --yes.",
                    preview.missing.len(), preview.config.display())}));
        }
        eprintln!("hgs: nothing was written. These rules would be added to {} for the Kimi account \"{name}\":", preview.config.display());
        for pattern in &preview.missing {
            eprintln!("  {pattern}");
        }
        return Err(Error::new(1, format!(
            "Confirm with --yes to write {} read-only hgs rule(s); every command that changes anything still asks",
            preview.missing.len()
        )));
    }
    if dry {
        return Ok(json!({"account":account,"home":home,"patterns":preview.patterns,
            "missing":preview.missing,"written":false,"dry_run":true}));
    }
    let write = apply(&home, true)
        .map_err(|error| Error::new(1, format!("The Kimi rules were not written: {error}")))?;
    Ok(json!({"account":account,"home":home,"patterns":preview.patterns,
        "present":preview.present,"written":write == Write::Written}))
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

/// Task 159: the owner's scratch rule trusts `folder` for `provider` in the
/// account config a launch with `account` (empty: the default) reads. Claude
/// keeps it in `.claude.json`, Codex in `config.toml`; other agents have no
/// trust entry Zerus writes (they still ask at start-up). True when added.
pub(crate) fn trust_folder_for(config: &Config, provider: &str, account: &str, folder: &str) -> Result<bool> {
    let account = if account.is_empty() { default_account(config, provider)? } else { Some(account.to_owned()) };
    let profile = match &account {
        Some(id) if !native_id(id) => Some(lookup(config, id)?.1),
        _ => None,
    };
    match provider {
        "claude" => {
            let path = match profile {
                Some(home) => home.join(".claude.json"),
                None => claude::account_config()?,
            };
            claude::trust_folder(&path, folder)
        }
        "codex" => {
            let home = profile.unwrap_or_else(|| native_home(config, "codex"));
            trust_codex_folder(&home.join("config.toml"), folder)
        }
        _ => Ok(false),
    }
}

/// Task 159: `[projects."FOLDER"] trust_level = "trusted"` in a Codex
/// config.toml, every other key kept. True when added.
pub(crate) fn trust_codex_folder(path: &Path, folder: &str) -> Result<bool> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(Error::new(1, format!("{}: {error}", path.display()))),
    };
    let mut document: toml_edit::DocumentMut = text.parse().map_err(|error| Error::new(1, format!("{} is not valid TOML: {error}", path.display())))?;
    if document.get("projects").and_then(|projects| projects.get(folder)).and_then(|project| project.get("trust_level")).and_then(|level| level.as_str()) == Some("trusted") {
        return Ok(false);
    }
    let projects = document.entry("projects").or_insert(toml_edit::table());
    let projects = projects.as_table_mut().ok_or_else(|| Error::new(1, format!("{}: projects is not a table", path.display())))?;
    projects.set_implicit(true);
    let project = projects.entry(folder).or_insert(toml_edit::table());
    let project = project.as_table_mut().ok_or_else(|| Error::new(1, format!("{}: the folder's entry is not a table", path.display())))?;
    project.insert("trust_level", toml_edit::value("trusted"));
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, document.to_string())?;
    Ok(true)
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
        // Qwen OAuth's credentials, or a provider chosen in its settings (an
        // API key in its environment or in the settings' own `env`).
        "qwen" => home.join("oauth_creds.json").is_file() || qwen_auth_selected(home),
        // Hermes keeps its provider and model in config.yaml (`hermes setup`).
        "hermes" => home.join("config.yaml").is_file(),
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
/// Whether Qwen Code's settings in `home` choose an authentication type.
fn qwen_auth_selected(home: &Path) -> bool {
    fs::read(home.join("settings.json")).ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|settings| settings["security"]["auth"]["selectedType"].as_str().is_some_and(|kind| !kind.is_empty()))
}
fn row(config: &Config, id: &str, p: &Profile, native: bool) -> Value {
    let home = if native {
        native_home(config, &p.provider)
    } else {
        profile_home(config, id)
    };
    let found = credential_file(&home, &p.provider);
    let mut row = json!({"id":id,"provider":p.provider,"label":p.label,"home":home,"native":native,
        "account_status":inspection::catalog_status(config,id,&p.provider,&home),
        "auth_revision":inspection::signature(&home,&p.provider),
        "credential_file":found,"auth_status":if found {"credential_file"} else {"unknown"},
        "portable":p.provider!="claude","installed":platform::available(&p.provider)});
    // A local model's account: where its agent sends requests, so a client can
    // show that server's speed instead of cloud limits.
    if let Some(gateway) = crate::state::model_servers::account_gateway_url(&home, &p.provider) {
        row["gateway"] = json!(gateway);
    }
    row
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
    // Qwen Code's own account wherever it is installed or was removed by hand.
    if platform::available("qwen") || catalog.hidden_profiles.contains("native-qwen") {
        let id = "native-qwen";
        rows.push(row(config, id, &Profile {
            provider: "qwen".into(),
            label: catalog.native_labels.get(id).cloned().unwrap_or_else(|| default_label("qwen").into()),
        }, true));
    }
    // WhiteWh fork: Hermes Agent's own configuration, wherever it is installed.
    if platform::available("hermes") || catalog.hidden_profiles.contains("native-hermes") {
        let id = "native-hermes";
        rows.push(row(config, id, &Profile {
            provider: "hermes".into(),
            label: catalog.native_labels.get(id).cloned().unwrap_or_else(|| default_label("hermes").into()),
        }, true));
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
        .filter(|provider| ["claude", "codex", "kimi", "qwen", "dsh"].contains(provider))
        .map(str::to_owned)
        .collect();
    let mut catalogs = permission_catalog::Cache::prefetch(config, &providers);
    for row in &mut rows {
        let provider = row["provider"].as_str().unwrap_or("").to_owned();
        row["is_default"] = json!(row["id"].as_str() == Some(catalog.defaults.get(&provider).map(String::as_str).unwrap_or(&format!("native-{provider}"))));
        let explicit = catalog.permission_modes.get(row["id"].as_str().unwrap_or("")).map(String::as_str);
        // `zerus`: the person chose it; `default`: the Zerus default (T-6hcfnu).
        row["permission_mode_source"] = json!(if explicit.is_some() { "zerus" } else { "default" });
        permission_status::describe(config, row, explicit.unwrap_or(default_permission_mode(&provider)), &mut catalogs);
        row["permission_modes"] = json!(permission_modes(&provider));
        model_defaults::describe(row, &catalog);
        if provider == "kimi" {
            row["compact_on_expiry_supported"] = json!(true);
            row["compact_on_expiry"] = json!(catalog.kimi_compact_on_expiry.get(row["id"].as_str().unwrap_or("")).copied().unwrap_or(true));
        }
    }
    permission_scope::describe(config, &mut rows);
    marks::describe(config, &mut rows);
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
        json!({"host":config.host(),"profiles":visible,"removed_profiles":removed,"defaults":catalog.defaults,"revision":revision(&bytes),"setup_revision":true,"copy_revision":true,"permission_scope_supported":true,"marks_supported":true}),
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
/// Serialises tests that change process environment variables.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The permission mode of an account without an explicit choice (owner,
/// 2026-11-07, T-6hcfnu): bypass for Claude, Codex and Kimi. OpenCode and
/// DeepSeek keep their native configuration: OpenCode refuses a Zerus mode,
/// and DeepSeek's bypass needs its own confirmed setup.
pub fn default_permission_mode(provider: &str) -> &'static str {
    if matches!(provider, "claude" | "codex" | "kimi" | "qwen") { "bypass" } else { "provider" }
}

/// Called before state lookup: a resumed/forked record still retains its original native home.
/// An explicit choice (`provider` or `bypass`) always wins over the default.
pub fn permission_mode(config: &Config, account: &str, provider: &str) -> Result<String> {
    let id = if account.is_empty() {format!("native-{provider}")} else {account.into()};
    let (profile,_,_) = lookup(config,&id)?;
    if profile.provider != provider { return Err(Error::new(1,"The account belongs to another provider.")); }
    Ok(decode(&read(config)?)?.permission_modes.get(&id).cloned().unwrap_or_else(||default_permission_mode(provider).into()))
}

/// Modes Zerus can set for a provider's account or a single launch; `provider`
/// leaves the native settings in charge. Only Claude has a native Auto mode.
pub fn permission_modes(provider: &str) -> &'static [&'static str] {
    match provider {
        "claude" => &["provider", "auto", "bypass"],
        "codex" | "kimi" | "qwen" | "dsh" => &["provider", "bypass"],
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
        "qwen" => &["--approval-mode", "--yolo", "-y"],
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

/// Task 352: the agent an account id belongs to on this computer.
pub(crate) fn provider_of(config: &Config, id: &str) -> Result<String> {
    Ok(lookup(config, id)?.0.provider)
}

/// Task 352: an account of another agent a session may move to: the agent is
/// installed here and, for Claude, Codex and Kimi, the account is signed in.
/// DeepSeek and OpenCode run on their native configuration. Returns the
/// agent, the label and the account folder.
pub(crate) fn move_target(config: &Config, id: &str) -> Result<(String, String, PathBuf)> {
    let (profile, home, native) = lookup(config, id)?;
    let provider = profile.provider.clone();
    if !["claude", "codex", "kimi", "qwen", "hermes", "dsh", "opencode"].contains(&provider.as_str()) {
        return Err(Error::new(1, format!("Account '{id}' belongs to an agent Zerus cannot start ({provider}).")));
    }
    if !agent_installed(&provider) {
        return Err(Error::new(
            1,
            format!("{} is not installed on {}; install it in Accounts before moving a session to it.", agent_name(&provider), config.host()),
        ));
    }
    if ["claude", "codex", "kimi", "qwen"].contains(&provider.as_str()) {
        if !native && !home.is_dir() {
            return Err(Error::new(1, format!("Account '{id}' has no profile folder on this computer; log in to it first.")));
        }
        if !signed_in(&home, &provider, native) {
            return Err(Error::new(
                1,
                format!("Account '{id}' is not signed in on {}; log in to it before moving a session.", config.host()),
            ));
        }
    }
    Ok((provider, profile.label, home))
}

/// An agent's name as the owner reads it.
pub(crate) fn agent_name(provider: &str) -> &'static str {
    match provider {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "kimi" => "Kimi",
        "qwen" => "Qwen Code",
        "hermes" => "Hermes Agent",
        "dsh" => "DeepSeek",
        "opencode" => "OpenCode",
        _ => "This agent",
    }
}

#[cfg(test)]
thread_local! {
    /// Tests name the agents installed on their computer.
    static TEST_INSTALLED: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_installed(agents: Option<&[&str]>) {
    TEST_INSTALLED.with(|cell| *cell.borrow_mut() = agents.map(|agents| agents.iter().map(|agent| (*agent).to_owned()).collect()));
}

fn agent_installed(provider: &str) -> bool {
    #[cfg(test)]
    if let Some(agents) = TEST_INSTALLED.with(|cell| cell.borrow().clone()) {
        return agents.iter().any(|agent| agent == provider);
    }
    platform::available(provider)
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
        "qwen" => key("OPENAI_API_KEY") || key("DASHSCOPE_API_KEY"),
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
    let mut labels: BTreeMap<String, String> = ["codex", "claude", "kimi", "qwen", "hermes"]
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
    // The inspection's test seam is never passed to a native agent started in
    // this session, whichever account kind it uses.
    env::remove_var(inspection::KIMI_TEST_BASE_ENV);
    environment.push(format!("{}=", inspection::KIMI_TEST_BASE_ENV));
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
    let mut found: Vec<&'static str> = ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "KIMI_CODE_HOME", "QWEN_HOME"]
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
        // Task 115: the owner's explicit opt-in for a Kimi account's read-only
        // hgs rules. A team launch never writes them.
        "kimi-read-only"=>{println!("{}",kimi_read_only(config,id,args,dry)?);},
        "compact-on-expiry"=>{println!("{}",kimi_compaction::set(config,args,dry)?);},
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
            let mode=arg(args,"--mode").ok_or_else(||Error::new(1,"Choose provider, bypass, default or a native permission selection."))?;
            // Probe and validate outside the account lock. The lock is taken
            // only for the revision-checked write below.
            let bytes=read(config)?;check_revision(&bytes,args)?;let original=bytes.clone();let snapshot=decode(&bytes)?;
            if !native_id(id) && !snapshot.profiles.contains_key(id) {return Err(Error::new(1,"Account profile does not exist."));}
            if id=="native-opencode" && mode!="provider" {return Err(Error::new(1,"OpenCode permissions are managed in its native configuration"));}
            let provider=if native_id(id) {id.trim_start_matches("native-").to_owned()} else {snapshot.profiles[id].provider.clone()};
            let home=if native_id(id) {native_home(config,&provider)} else {profile_home(config,id)};
            // `default` returns to the Zerus default; every other mode is checked
            // against the provider's fresh options before it is stored.
            let normalized=if mode=="default" {"default".to_owned()} else {
                validate_permission_mode(&provider,mode)?;
                permission_catalog::validate_for_account(config,&provider,&home,mode)?;
                normalize_permission_mode(mode)?
            };
            let _lock=locked(config)?;let bytes=read(config)?;check_revision(&bytes,args)?;
            if bytes != original {return Err(Error::new(1,"Account catalog changed while its permission options were checked; retry."));}
            let mut catalog=decode(&bytes)?;
            if !native_id(id) && !catalog.profiles.contains_key(id) {return Err(Error::new(1,"Account profile does not exist."));}
            let current_provider=if native_id(id) {id.trim_start_matches("native-").to_owned()} else {catalog.profiles[id].provider.clone()};
            if current_provider != provider {return Err(Error::new(1,"Account changed while its permission options were checked; retry."));}
            let current_home=if native_id(id) {native_home(config,&current_provider)} else {profile_home(config,id)};
            if current_home != home {return Err(Error::new(1,"Account changed while its permission options were checked; retry."));}
            // A shared change names the scope it saw; a computer that keeps
            // its permissions per computer since then refuses it (zerus-f0uz8).
            if let Some(expected)=arg(args,"--expect-scope") {
                if !permission_scope::is(config,id,expected) {return Err(Error::new(1,"Permissions on this account changed between shared and per computer. Refresh before saving."));}
            }
            // Both choices are kept: an explicit provider mode must stay when
            // the default is bypass (T-6hcfnu); `default` returns to it.
            if normalized=="default" {catalog.permission_modes.remove(id);}
            else {catalog.permission_modes.insert(id.into(),normalized);}
            if !dry {save(config,&catalog)?;}println!("{}",list(config)?);
        },
        "permission-scope"=>{println!("{}",permission_scope::set(config,args,dry)?);},
        "mark"=>{println!("{}",marks::set(config,args,dry)?);},
        "rename"=>{
            let label=arg(args,"--label").ok_or_else(||Error::new(1,"Account name is required."))?.trim();validate_label(label)?;
            let _lock=locked(config)?;let bytes=read(config)?;check_revision(&bytes,args)?;let mut catalog=decode(&bytes)?;
            if ["native-codex","native-claude","native-kimi","native-qwen","native-dsh","native-opencode"].contains(&id) {
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
            if id=="native-dsh" {return crate::native_ui::open(config,"","@account",None,dry);}
            if id=="native-opencode" {
                const GUIDANCE: &str = "Connect OpenCode providers with `opencode auth login` in a terminal on this computer";
                if dry {println!("{GUIDANCE}");return Ok(0);}
                return Err(Error::new(1,GUIDANCE));
            }
            let (p,home,native)=lookup(config,id)?; let key=home_var(&p.provider)?;
            let arguments: Vec<&str> = if p.provider == "claude" && !native { vec!["auth", "login", "--claudeai"] }
                else if p.provider == "claude" { vec!["auth", "login"] }
                // Qwen Code signs in inside its own terminal (`/auth`); its
                // `auth` command is gone.
                else if p.provider == "qwen" { vec![] }
                // Hermes chooses its provider and model in its own wizard.
                else if p.provider == "hermes" { vec!["setup"] } else { vec!["login"] };
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
        _=>return Err(Error::new(1,"usage: hgs account ls | add ID --provider codex|claude|kimi --label NAME [--ollama MODEL [--endpoint URL] [--context TOKENS] [--prepare COMMAND]] | ollama-models [--endpoint URL] | rm ID | login ID | permission-scope ID --scope account|computer | mark ID --color #RRGGBB|none --emoji EMOJI|none | favorite native-opencode --model PROVIDER/MODEL --favorite on|off --request-id UUID | models native-opencode [--location DIR]")),
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
        command = Command::new(crate::self_exec::current()?);
        command.args(args);
    } else {
        if !config.peers.iter().any(|s| s == host) {
            return Err(Error::new(
                1,
                "Account transfer requires a configured, enabled machine.",
            ));
        }
        let remote = crate::remote_shell::hgs(config, host, args)?;
        command = crate::ssh_bytes::command(&remote)?;
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
            .arg(remote);
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
pub fn run_script(
    shell: &str,
    account: &str,
    provider: &str,
    home: &str,
    extra: &[String],
) -> String {
    let mut environment = run_environment(account, provider, home);
    environment.extend_from_slice(extra);
    supervisor_script(shell, &environment)
}

/// Known login-shell argument contracts only. Other shells still launch agents
/// through POSIX sh using the inherited environment and its login profile;
/// their own rc files are not interpreted as another language.
#[cfg(unix)]
pub fn supervisor_shell(shell: &str) -> (&str, &'static str) {
    match Path::new(shell).file_name().and_then(|name| name.to_str()) {
        Some("sh" | "bash" | "dash" | "ash" | "ksh" | "ksh93" | "zsh" | "fish") => (shell, "-lic"),
        _ => ("/bin/sh", "-lc"),
    }
}

/// Explicit assignments follow login-shell startup, ahead of the unchanged
/// supervisor and native arguments. The empty hint exclusion list is explicit.
#[cfg(unix)]
pub fn supervisor_script(shell: &str, environment: &[String]) -> String {
    let fish = Path::new(shell)
        .file_name()
        .is_some_and(|name| name == "fish");
    let arguments = if fish {
        "\"$argv[1]\" --run $argv[2..-1]"
    } else {
        "\"$0\" --run \"$@\""
    };
    if environment.is_empty() {
        return format!("exec {arguments}");
    }
    let assignments = if fish {
        platform::remote_join(environment)
    } else {
        platform::join(environment)
    };
    format!("exec env {assignments} {arguments}")
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
    // As in `select`: the inspection's test seam stays out of agent sessions.
    environment.push(format!("{}=", inspection::KIMI_TEST_BASE_ENV));
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
/// The DeepSeek account's saved launch selection as (provider/model, effort).
pub fn launch_selection(config: &Config, provider: &str) -> Result<Option<(String, String)>> {
    model_defaults::launch_selection(config, provider)
}

/// Read the exact account's Zerus policy. Missing policy defaults on; unreadable
/// catalogs and a changed home are errors, never permission to compact.
pub(crate) fn kimi_compact_on_expiry(config: &Config, id: &str, home: &Path) -> Result<bool> {
    kimi_compaction::enabled(config, id, home)
}

#[cfg(test)]
mod permission_tests {
    use super::*;

    #[test]
    fn selecting_an_account_clears_the_inspection_test_seam() {
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("config");
        fs::create_dir_all(dir.join("accounts/work")).unwrap();
        fs::write(
            dir.join("accounts.json"),
            r#"{"profiles":{"work":{"provider":"kimi","label":"Work"}}}"#,
        )
        .unwrap();
        let config = Config {
            home: root.path().to_string_lossy().into_owned(),
            dir,
            state: root.path().join("state"),
            selves: Vec::new(),
            peers: Vec::new(),
            base_peers: Vec::new(),
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        let keys: Vec<&str> = std::iter::once("KIMI_CODE_HOME")
            .chain(std::iter::once("HGS_ACCOUNT_ID"))
            .chain(std::iter::once(inspection::KIMI_TEST_BASE_ENV))
            .chain(AUTH_ENV.iter().copied())
            .collect();
        let saved: Vec<(&str, Option<std::ffi::OsString>)> =
            keys.into_iter().map(|key| (key, env::var_os(key))).collect();
        env::set_var(inspection::KIMI_TEST_BASE_ENV, "http://127.0.0.1:9");
        let environment = select(&config, "work", "kimi").unwrap();
        assert!(
            environment.contains(&format!("{}=", inspection::KIMI_TEST_BASE_ENV)),
            "{environment:?}"
        );
        assert!(
            env::var_os(inspection::KIMI_TEST_BASE_ENV).is_none(),
            "select left the seam in the launching process"
        );
        for (key, value) in saved {
            match value {
                Some(value) => env::set_var(key, value),
                None => env::remove_var(key),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_syntax_is_explicit_and_unknown_shells_use_posix_instead() {
        for shell in ["/bin/bash", "/bin/sh", "/bin/zsh", "/bin/fish"] {
            assert_eq!(supervisor_shell(shell), (shell, "-lic"));
        }
        for shell in [
            "/bin/nu",
            "/bin/xonsh",
            "/bin/tcsh",
            "/private/custom-shell",
        ] {
            assert_eq!(supervisor_shell(shell), ("/bin/sh", "-lc"));
        }
        let fish = supervisor_script("/bin/fish", &["HGS_AGENT_HINT_OFF=quoted ' value;".into()]);
        assert!(fish.contains("\"$argv[1]\" --run $argv[2..-1]"));
        assert!(!fish.contains("$0") && !fish.contains("$@"));
        let words = shell_words::split(&fish).unwrap();
        assert_eq!(words[2], "HGS_AGENT_HINT_OFF=quoted ' value;");
    }

    #[cfg(unix)]
    #[test]
    fn agent_hint_effective_values_follow_shell_startup_without_config_redirect_or_argv_change() {
        let hint = vec!["HGS_AGENT_HINT=1".into(), "HGS_AGENT_HINT_OFF=".into()];
        let script = run_script(
            "/bin/bash", "work-account",
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
        assert_eq!(supervisor_script("/bin/bash", &[]), "exec \"$0\" --run \"$@\"");
        assert_eq!(
            shell_words::split(&run_script("/bin/bash", "", "codex", "", &hint)).unwrap(),
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
        let words = shell_words::split(&supervisor_script("/bin/bash", &quoted)).unwrap();
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
    /// Owner, 2026-11-11 (team 86821caa): a team request names an account by
    /// its id or by the label the owner sees; an unknown name is refused with
    /// the accounts this computer has for the agent.
    #[test]
    fn a_team_account_is_named_by_id_or_label() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            home: dir.path().to_str().unwrap().into(), dir: dir.path().join("config"), state: dir.path().join("state"),
            selves: vec!["msi".into()], peers: vec![], base_peers: vec![], peers_from_environment: false,
            machines: BTreeMap::new(), tab: false, tab_colors: String::new(),
        };
        fs::create_dir_all(&config.dir).unwrap();
        fs::write(config.dir.join("accounts.json"), json!({
            "profiles": {
                "claude-58bd": {"provider": "claude", "label": "Additional"},
                "claude-5758": {"provider": "claude", "label": "Default"},
                "claude-twin": {"provider": "claude", "label": "default"},
                "codex-work": {"provider": "codex", "label": "Work"}
            },
            "defaults": {"claude": "claude-58bd"}
        }).to_string()).unwrap();
        let resolve = |provider: &str, name: &str| resolve_team_account(&config, provider, name);
        assert_eq!(resolve("claude", "claude-58bd").unwrap(), "claude-58bd");
        assert_eq!(resolve("claude", "additional").unwrap(), "claude-58bd");
        assert_eq!(resolve("claude", " Additional ").unwrap(), "claude-58bd");
        assert_eq!(resolve("claude", "native-claude").unwrap(), "native-claude");
        assert_eq!(resolve("claude", "Default account").unwrap(), "native-claude");
        assert_eq!(resolve("codex", "work").unwrap(), "codex-work");
        let unknown = resolve("claude", "spare").unwrap_err();
        assert!(unknown.contains("Unknown account \"spare\" for claude"), "{unknown}");
        assert!(unknown.contains("claude accounts on msi: native-claude (Default account), claude-5758 (Default), claude-58bd (Additional)"), "{unknown}");
        let ambiguous = resolve("claude", "default").unwrap_err();
        assert!(ambiguous.contains("names 2 accounts (claude-5758, claude-twin); name the id"), "{ambiguous}");
        let other = resolve("claude", "codex-work").unwrap_err();
        assert!(other.contains("belongs to another agent, not claude"), "{other}");
        // A member without an account runs on this computer's default for its
        // agent; with none set, on the native account.
        assert_eq!(effective_team_account(&config, "claude", ""), "claude-58bd");
        assert_eq!(effective_team_account(&config, "codex", ""), "native-codex");
        assert_eq!(effective_team_account(&config, "claude", "claude-5758"), "claude-5758");
    }
    #[test]
    fn a_launch_home_is_the_profiles_or_the_native_one() {
        // Task 115: the Kimi read-only rules are written where the launch reads
        // its config: a managed profile's own home, else the native one.
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            home: dir.path().to_str().unwrap().into(),
            dir: dir.path().join("config"),
            state: dir.path().join("state"),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: BTreeMap::new(),
            tab: false,
            tab_colors: String::new(),
        };
        let native = std::env::var("KIMI_CODE_HOME")
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(&config.home).join(".kimi-code"));
        assert_eq!(launch_home(&config, "kimi", ""), native, "no catalog: the native home");
        assert_eq!(launch_home(&config, "kimi", "native-kimi"), native);
        fs::create_dir_all(&config.dir).unwrap();
        fs::write(
            config.dir.join("accounts.json"),
            json!({
                "profiles": {"work": {"provider": "kimi", "label": "Work"}},
                "defaults": {"kimi": "work"}
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(launch_home(&config, "kimi", "work"), config.dir.join("accounts").join("work"));
        assert_eq!(launch_home(&config, "kimi", ""), config.dir.join("accounts").join("work"), "the default profile");
        fs::write(
            config.dir.join("accounts.json"),
            json!({
                "profiles": {"work": {"provider": "kimi", "label": "Work"}},
                "defaults": {"kimi": "native-kimi"}
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(launch_home(&config, "kimi", ""), native, "a native default");
    }

    #[test]
    fn the_kimi_read_only_rules_need_the_owners_confirmation_and_a_kimi_account() {
        // Task 115: a team launch never writes these; the owner's command names
        // the exact patterns and writes only with --yes, into the account's own
        // home, and --remove takes back only those rules.
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            home: dir.path().to_str().unwrap().into(),
            dir: dir.path().join("config"),
            state: dir.path().join("state"),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: BTreeMap::new(),
            tab: false,
            tab_colors: String::new(),
        };
        fs::create_dir_all(&config.dir).unwrap();
        fs::write(
            config.dir.join("accounts.json"),
            json!({
                "profiles": {"work": {"provider": "kimi", "label": "Work"}},
                "defaults": {"kimi": "work"}
            })
            .to_string(),
        )
        .unwrap();
        let home = config.dir.join("accounts").join("work");
        fs::create_dir_all(&home).unwrap();
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let preview = kimi_read_only(&config, "work", &args(&["--json"]), false).unwrap();
        assert_eq!(preview["written"], false);
        assert_eq!(preview["missing"].as_array().unwrap().len(), 12);
        assert!(!home.join("config.toml").exists(), "a preview writes nothing");
        assert!(kimi_read_only(&config, "work", &args(&[]), false).is_err(), "confirmation required");
        assert!(!home.join("config.toml").exists(), "a refusal writes nothing");
        assert!(!kimi_read_only(&config, "work", &args(&["--yes"]), true).unwrap()["written"].as_bool().unwrap());
        assert!(!home.join("config.toml").exists(), "a dry run writes nothing");
        let written = kimi_read_only(&config, "work", &args(&["--yes", "--json"]), false).unwrap();
        assert_eq!(written["written"], true);
        let text = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(text.contains("Bash(hgs team status --json)"), "{text}");
        assert!(!text.contains("Bash(hgs inspect") && !text.contains(" *)"), "{text}");
        let removed = kimi_read_only(&config, "work", &args(&["--remove", "--json"]), false).unwrap();
        assert_eq!(removed["removed"].as_u64(), Some(12));
        fs::write(
            config.dir.join("accounts.json"),
            json!({"profiles": {"claude-one": {"provider": "claude", "label": "C"}}}).to_string(),
        )
        .unwrap();
        assert!(kimi_read_only(&config, "claude-one", &args(&["--json"]), false).is_err(), "a Kimi-only setting");
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
