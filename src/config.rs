//! Compatibility with hgs' assignment-based configuration and project maps.
//! Configuration is data: unlike the former shell implementation, reading it does not execute it.
use crate::{
    cli::{Error, Result},
    platform,
};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub struct Config {
    pub home: String,
    pub dir: PathBuf,
    pub state: PathBuf,
    pub selves: Vec<String>,
    pub peers: Vec<String>,
    pub base_peers: Vec<String>,
    pub peers_from_environment: bool,
    pub machines: std::collections::BTreeMap<String, crate::machines::Connection>,
    pub tab: bool,
    pub tab_colors: String,
}

impl Config {
    pub fn load() -> Result<Self> {
        let home = platform::home().ok_or_else(|| {
            Error::new(
                1,
                if cfg!(windows) {
                    "HOME or USERPROFILE is not set"
                } else {
                    "HOME is not set"
                },
            )
        })?;
        let dir = config_dir(&home);
        let raw_state = env::var("HGS_STATE_DIR").unwrap_or_else(|_| {
            platform::below(
                &env::var("XDG_STATE_HOME")
                    .unwrap_or_else(|_| platform::below(&home, ".local/state")),
                "hgs/sessions",
            )
        });
        let state = if Path::new(&raw_state).is_absolute() {
            PathBuf::from(raw_state)
        } else {
            env::current_dir()?.join(raw_state)
        };
        env::set_var("HGS_STATE_DIR", &state);
        let values = match fs::read_to_string(dir.join("config")) {
            Ok(text) => configuration_values(&text, &dir, &home)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e.into()),
        };
        let get = |key: &str, default: &str| {
            env::var(key)
                .ok()
                .or_else(|| values.get(key).cloned())
                .unwrap_or_else(|| default.to_owned())
        };
        let mut self_names = env::var("HGS_SELF")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| values.get("HGS_SELF").cloned())
            .unwrap_or_default();
        if self_names.is_empty() {
            self_names = platform::hostname();
            if self_names.is_empty() {
                self_names = "localhost".into();
            }
        }
        let base_peers: Vec<String> = get("HGS_PEERS", "")
            .split_whitespace()
            .map(String::from)
            .collect();
        let machines = crate::machines::load(&dir)?;
        let peers_from_environment = env::var("HGS_PEERS").is_ok();
        let peers =
            crate::machines::effective_peers(&base_peers, &machines, peers_from_environment);
        Ok(Self {
            home,
            dir,
            state,
            selves: self_names.split_whitespace().map(String::from).collect(),
            peers,
            base_peers,
            peers_from_environment,
            machines,
            tab: get("HGS_TAB", "1") == "1",
            tab_colors: get("HGS_TAB_COLORS", ""),
        })
    }
    pub fn host(&self) -> &str {
        self.selves
            .first()
            .map(String::as_str)
            .unwrap_or("localhost")
    }
    pub fn is_self(&self, host: &str) -> bool {
        self.selves.iter().any(|v| v == host)
    }
    pub fn expand_home(&self, path: &str) -> String {
        if cfg!(windows) {
            if let Some(tail) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
                return platform::below(&self.home, tail);
            }
        }
        path.strip_prefix('~')
            .map(|tail| format!("{}{tail}", self.home))
            .unwrap_or_else(|| path.to_owned())
    }
    pub fn projects(&self) -> Result<Vec<Project>> {
        let mut projects = Vec::new();
        let mut seen = HashSet::new();
        for (file, source) in [("projects.local", "local")] {
            let text = match fs::read_to_string(self.dir.join(file)) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            for line in text.lines() {
                let Some((name, path)) = line.split_once('=') else {
                    continue;
                };
                let (name, path) = (name.trim(), path.trim());
                if name.is_empty()
                    || name.starts_with('#')
                    || path.is_empty()
                    || !seen.insert(name.to_owned())
                {
                    continue;
                }
                projects.push(Project {
                    name: name.into(),
                    dir: path.into(),
                    src: source.into(),
                });
            }
        }
        Ok(projects)
    }
    /// Serialize project read/modify/write across CLI and GUI processes, then atomically replace.
    pub fn write_project(&self, name: &str, directory: Option<&str>, add_only: bool) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join("projects.lock"))?;
        let _lock = crate::platform::FileLock::exclusive(lock)?;
        let existing = self.projects()?.into_iter().find(|p| p.name == name);
        if add_only {
            if let Some(project) = existing.as_ref() {
                return Err(Error::new(1, format!("project '{name}' already exists ({}); use 'hgs project set' to override it", project.src)));
            }
        }
        if directory.is_none() {
            match existing.as_ref().map(|p| p.src.as_str()) {
                Some("local") => {}
                _ => return Err(Error::new(1, format!("unknown project '{name}'"))),
            }
        }
        let file = self.dir.join("projects.local");
        let old = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        let mut temporary = tempfile::NamedTempFile::new_in(&self.dir)?;
        if let Ok(metadata) = fs::metadata(&file) {
            temporary
                .as_file()
                .set_permissions(metadata.permissions())?;
        }
        for line in old.lines() {
            if line
                .split_once('=')
                .map(|(key, _)| key.trim())
                .unwrap_or(line.trim())
                != name
            {
                writeln!(temporary, "{line}")?;
            }
        }
        if let Some(directory) = directory {
            writeln!(temporary, "{name}={directory}")?;
        }
        temporary.as_file().sync_all()?;
        platform::persist(temporary, &file)
            .map_err(|e| Error::new(1, format!("cannot replace {}: {e}", file.display())))?;
        platform::sync_dir(&self.dir)?;
        Ok(())
    }
}

/// The same full-file line decoder used by Config::load, isolated from process
/// environment mutation so optional policy controls can exercise that boundary.
fn configuration_values(
    text: &str,
    directory: &Path,
    home: &str,
) -> Result<HashMap<String, String>> {
    let mut values = HashMap::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, raw)) = line.split_once('=') else {
            // The optional hint reader diagnoses its own keys; malformed
            // hint policy must not reject the launch.
            if hint_key_without_assignment(line) {
                continue;
            }
            return Err(Error::new(
                1,
                format!(
                    "{}:{}: expected NAME=value",
                    directory.join("config").display(),
                    index + 1
                ),
            ));
        };
        let key = key.trim();
        if !matches!(key, "HGS_SELF" | "HGS_PEERS" | "HGS_TAB" | "HGS_TAB_COLORS") {
            continue;
        }
        let value = assignment_value(raw, home, &values).map_err(|message| {
            Error::new(
                1,
                format!(
                    "{}:{}: {message}",
                    directory.join("config").display(),
                    index + 1
                ),
            )
        })?;
        values.insert(key.to_owned(), value);
    }
    Ok(values)
}

/// The configuration directory below `home`: `HGS_CONFIG_DIR` when set, else
/// `~/.config/hgs`. Also used without a loaded `Config` (the Windows
/// local-sessions marker), so both always name the same directory.
pub fn config_dir(home: &str) -> PathBuf {
    PathBuf::from(
        env::var("HGS_CONFIG_DIR").unwrap_or_else(|_| platform::below(home, ".config/hgs")),
    )
}

const HINT_KEYS: [&str; 2] = ["HGS_AGENT_HINT", "HGS_AGENT_HINT_OFF"];
pub(crate) const HINT_POLICY_DIAGNOSTIC: &str =
    "hgs: optional agent hint disabled: invalid or unreadable hint policy";

fn hint_key_without_assignment(line: &str) -> bool {
    HINT_KEYS.iter().any(|key| {
        line == *key
            || line
                .strip_prefix(*key)
                .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    })
}

#[derive(Debug, PartialEq, Eq)]
struct HintPolicy {
    enabled: bool,
    off: String,
}

impl HintPolicy {
    fn transport_valid(&self) -> bool {
        self.off.len() <= 8192 && !self.off.contains('\0')
    }

    fn environment(self) -> Vec<String> {
        // Keep malformed optional data away from both Unix argv and the
        // Windows environment block, even at this final transport boundary.
        if !self.transport_valid() {
            eprintln!("{HINT_POLICY_DIAGNOSTIC}");
            return vec!["HGS_AGENT_HINT=0".into(), "HGS_AGENT_HINT_OFF=".into()];
        }
        vec![
            format!("HGS_AGENT_HINT={}", if self.enabled { "1" } else { "0" }),
            format!("HGS_AGENT_HINT_OFF={}", self.off),
        ]
    }

    fn allows(&self, account: &str) -> bool {
        self.enabled && !self.off.split_whitespace().any(|id| id == account)
    }
}

fn hint_policy(
    text: &str,
    home: &str,
    mut environment: impl FnMut(&str) -> std::result::Result<Option<String>, ()>,
) -> std::result::Result<HintPolicy, ()> {
    let mut values = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, raw)) = line.split_once('=') else {
            // Unrelated configuration keeps its existing validation behavior.
            if hint_key_without_assignment(line) {
                return Err(());
            }
            continue;
        };
        let key = key.trim();
        if HINT_KEYS.contains(&key) {
            let value = assignment_value(raw, home, &values).map_err(|_| ())?;
            values.insert(key.to_owned(), value);
        }
    }
    for key in HINT_KEYS {
        if let Some(value) = environment(key)? {
            values.insert(key.to_owned(), value);
        }
    }
    if values.values().any(|value| value.contains('\0')) {
        return Err(());
    }
    let enabled = match values.get(HINT_KEYS[0]).map(String::as_str).unwrap_or("1") {
        "0" => false,
        "1" => true,
        _ => return Err(()),
    };
    let off = values.get(HINT_KEYS[1]).cloned().unwrap_or_default();
    if off.len() > 8192 {
        return Err(());
    }
    Ok(HintPolicy {
        enabled,
        off: off.split_whitespace().collect::<Vec<_>>().join(" "),
    })
}

fn read_hint_policy(directory: &Path, home: &str) -> std::result::Result<HintPolicy, ()> {
    let mut text = String::new();
    match fs::File::open(directory.join("config")) {
        Ok(file) => {
            file.take(65_537)
                .read_to_string(&mut text)
                .map_err(|_| ())?;
            if text.len() > 65_536 {
                return Err(());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(()),
    }
    hint_policy(&text, home, |key| match env::var(key) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(()),
    })
}

/// Resolve only the optional hint controls in this caller. Forward effective
/// values, including an empty exclusion list, after shell startup and on every
/// new native process. No config path or policy goes into the saved recipe.
pub(crate) fn agent_hint_environment(provider: &str) -> Vec<String> {
    if !["claude", "codex"].contains(&provider) {
        return Vec::new();
    }
    let policy = platform::home()
        .ok_or(())
        .and_then(|home| read_hint_policy(&config_dir(&home), &home));
    hint_policy_environment(policy)
}

fn hint_policy_environment(policy: std::result::Result<HintPolicy, ()>) -> Vec<String> {
    match policy {
        Ok(policy) => policy.environment(),
        Err(()) => {
            eprintln!("{HINT_POLICY_DIAGNOSTIC}");
            HintPolicy {
                enabled: false,
                off: String::new(),
            }
            .environment()
        }
    }
}

/// A tracked hook consumes a complete process snapshot; it never supplements
/// it from a different default config. Windows removes an empty environment
/// entry, so absent OFF with a resolved 0/1 means the empty exclusion list.
pub(crate) fn resolved_agent_hint(account: &str) -> bool {
    let get = |key| match env::var(key) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(()),
    };
    let policy = get(HINT_KEYS[0]).and_then(|enabled| {
        get(HINT_KEYS[1]).and_then(|off| resolved_hint_policy(enabled.as_deref(), off.as_deref()))
    });
    match policy {
        Ok(policy) => policy.allows(account),
        _ => {
            eprintln!("{HINT_POLICY_DIAGNOSTIC}");
            false
        }
    }
}

fn resolved_hint_policy(
    enabled: Option<&str>,
    off: Option<&str>,
) -> std::result::Result<HintPolicy, ()> {
    let enabled = match enabled {
        Some("0") => false,
        Some("1") => true,
        _ => return Err(()),
    };
    let off = off.unwrap_or("");
    let policy = HintPolicy {
        enabled,
        off: off.into(),
    };
    if !policy.transport_valid() {
        return Err(());
    }
    Ok(policy)
}

pub struct Project {
    pub name: String,
    pub dir: String,
    pub src: String,
}

pub fn check_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains(['/', '='])
        || name.starts_with('#')
        || name.chars().any(char::is_whitespace)
    {
        return Err(Error::new(1, format!("bad project name '{name}': must be non-empty, no '/', no '=', no leading '#' and no whitespace")));
    }
    if name.contains(['.', ':']) {
        eprintln!("hgs: '{name}' contains . or : — the session name will use '_' instead");
    }
    Ok(())
}
pub fn check_directory(path: &str) -> Result<()> {
    if path.contains(['\n', '\r']) {
        return Err(Error::new(1, "bad directory: a path with a newline cannot be stored in projects.local (one 'name=dir' per line)"));
    }
    if path.trim() != path {
        return Err(Error::new(1, format!("bad directory '{path}': leading/trailing whitespace is stripped when the file is read back")));
    }
    Ok(())
}

/// Decode one shell-style assignment, including quotes, escapes and simple variable expansion.
/// Command substitutions and operators are rejected rather than executed or silently changed.
fn assignment_value(
    raw: &str,
    home: &str,
    values: &HashMap<String, String>,
) -> std::result::Result<String, String> {
    let mut out = String::new();
    let mut quote = None;
    let mut chars = raw.trim_start().chars().peekable();
    while let Some(c) = chars.next() {
        if quote == Some('\'') {
            if c == '\'' {
                quote = None;
            } else {
                out.push(c);
            }
            continue;
        }
        match c {
            '\'' if quote.is_none() => quote = Some('\''),
            '"' => if quote == Some('"') { quote = None; } else { quote = Some('"'); },
            '\\' => {
                let next = chars.next().ok_or("unterminated escape in config value")?;
                if quote == Some('"') && !matches!(next, '$' | '`' | '"' | '\\') { out.push('\\'); }
                out.push(next);
            },
            '$' => {
                let braced = chars.peek() == Some(&'{');
                if braced { chars.next(); }
                if chars.peek() == Some(&'(') { return Err("shell command substitution is not supported in config; use literal NAME=value assignments".into()); }
                let mut key = String::new();
                while chars.peek().is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_') { key.push(chars.next().unwrap()); }
                if braced && chars.next() != Some('}') { return Err("only simple ${NAME} variable references are supported in config".into()); }
                if key.is_empty() { out.push('$'); continue; }
                if key == "HOME" { out.push_str(home); }
                else if let Some(value) = values.get(&key) { out.push_str(value); }
                else if let Ok(value) = env::var(&key) { out.push_str(&value); }
            },
            '`' => return Err("shell command substitution is not supported in config; use literal NAME=value assignments".into()),
            c if quote.is_none() && c.is_whitespace() => {
                while chars.peek().is_some_and(|c| c.is_whitespace()) { chars.next(); }
                if chars.peek().is_none() || chars.peek() == Some(&'#') { break; }
                return Err("quote values containing spaces; shell commands are not supported in config".into());
            },
            '#' if quote.is_none() && out.is_empty() => break,
            ';' | '|' | '&' | '<' | '>' | '(' | ')' if quote.is_none() => return Err("shell operators are not supported in config; use literal NAME=value assignments".into()),
            _ => out.push(c),
        }
    }
    if quote.is_some() {
        return Err("unterminated quote in config value".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(text: &str, overrides: &[(&str, &str)]) -> std::result::Result<HintPolicy, ()> {
        hint_policy(text, "/home/example", |key| {
            Ok(overrides
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).into()))
        })
    }

    #[test]
    fn agent_hint_policy_defaults_and_exact_account_exclusions() {
        assert_eq!(
            policy("", &[]).unwrap().environment(),
            ["HGS_AGENT_HINT=1", "HGS_AGENT_HINT_OFF="]
        );
        let selected = policy(
            "export HGS_AGENT_HINT=1 # enabled\nHGS_AGENT_HINT_OFF='native-codex work'\n",
            &[],
        )
        .unwrap();
        assert!(selected.allows("native-claude"));
        assert!(!selected.allows("native-codex") && !selected.allows("work"));
        assert!(selected.allows("codex") && selected.allows("work-other"));
        assert!(!policy("HGS_AGENT_HINT=0", &[])
            .unwrap()
            .allows("native-claude"));
    }

    #[test]
    fn agent_hint_environment_overrides_file_and_clears_stale_exclusions() {
        let text = "HGS_AGENT_HINT=0\nHGS_AGENT_HINT_OFF='native-claude native-codex'\n";
        let values = policy(text, &[("HGS_AGENT_HINT", "1"), ("HGS_AGENT_HINT_OFF", "")]).unwrap();
        assert!(values.allows("native-claude") && values.allows("native-codex"));
        let snapshot = values.environment();
        let mut stale = HashMap::from([
            ("HGS_AGENT_HINT", "0".to_owned()),
            ("HGS_AGENT_HINT_OFF", "native-claude".to_owned()),
            ("HGS_CONFIG_DIR", "/unchanged/private-config".to_owned()),
        ]);
        for entry in &snapshot {
            let (key, value) = entry.split_once('=').unwrap();
            stale.insert(key, value.into());
        }
        assert_eq!(stale["HGS_AGENT_HINT"], "1");
        assert_eq!(stale["HGS_AGENT_HINT_OFF"], "");
        assert_eq!(stale["HGS_CONFIG_DIR"], "/unchanged/private-config");
        // A new caller resolves its current policy; a saved delivery mark is
        // not a policy snapshot that could override this file change.
        assert_ne!(
            snapshot,
            policy("HGS_AGENT_HINT=0", &[]).unwrap().environment()
        );
        assert_eq!(
            policy("HGS_AGENT_HINT_OFF=work", &[("HGS_AGENT_HINT", "1")])
                .unwrap()
                .off,
            "work"
        );
    }

    #[test]
    fn agent_hint_invalid_policy_does_not_parse_unrelated_values_or_execute_shell() {
        assert!(policy("HGS_PEERS=$(unrelated)\nHGS_TAB='unterminated\n", &[]).is_ok());
        for text in [
            "HGS_AGENT_HINT=maybe",
            "HGS_AGENT_HINT=",
            "HGS_AGENT_HINT 0",
            "HGS_AGENT_HINT_OFF=$(must_not_execute)",
            "HGS_AGENT_HINT_OFF='unterminated",
        ] {
            assert!(policy(text, &[]).is_err(), "{text}");
        }
        assert!(policy("", &[("HGS_AGENT_HINT", "maybe")]).is_err());
        assert!(hint_policy("", "/home/example", |_| Err(())).is_err());
        assert_eq!(
            hint_policy_environment(Err(())),
            ["HGS_AGENT_HINT=0", "HGS_AGENT_HINT_OFF="]
        );
        for provider in ["kimi", "opencode", "dsh", "sh"] {
            assert!(agent_hint_environment(provider).is_empty());
        }
        let absent = tempfile::tempdir().unwrap();
        // A directory in place of the file is an optional-policy read failure.
        fs::create_dir(absent.path().join("config")).unwrap();
        assert!(read_hint_policy(absent.path(), "/home/example").is_err());
    }

    #[test]
    fn agent_hint_malformed_assignment_is_optional_only_for_the_two_exact_keys() {
        let directory = Path::new("/fixture/hgs");
        for line in [
            "HGS_AGENT_HINT",
            "HGS_AGENT_HINT 0",
            "HGS_AGENT_HINT_OFF\twork",
        ] {
            // Config::load delegates these malformed optional lines to the
            // hint reader; that reader refuses them and selects policy off.
            assert!(hint_key_without_assignment(line));
            assert!(policy(line, &[]).is_err());
            let text = format!("HGS_SELF=fixture\n{line}\nHGS_TAB=0\n");
            let full = configuration_values(&text, directory, "/home/example").unwrap();
            assert_eq!(full["HGS_SELF"], "fixture");
            assert_eq!(full["HGS_TAB"], "0");
        }
        for line in [
            "HGS_PEERS peer",
            "HGS_SELF",
            "HGS_AGENT_HINT_OTHER 0",
            "unrelated malformed line",
        ] {
            assert!(!hint_key_without_assignment(line));
            let text = format!("HGS_SELF=fixture\n{line}\n");
            let error = configuration_values(&text, directory, "/home/example").unwrap_err();
            assert_eq!(
                error.message,
                format!(
                    "{}:2: expected NAME=value",
                    directory.join("config").display()
                )
            );
        }
        let text = "HGS_SELF=fixture\nHGS_PEERS=$(must-not-execute)\n";
        assert!(configuration_values(text, directory, "/home/example")
            .unwrap_err()
            .message
            .contains("shell command substitution is not supported"));
    }

    #[test]
    fn agent_hint_full_config_nul_falls_back_before_environment_transport() {
        for key in HINT_KEYS {
            let text = format!(
                "HGS_SELF=fixture\n{key}='work\0tail'\nHGS_PEERS='peer-a peer-b'\nHGS_TAB=0\n"
            );
            assert!(text.as_bytes().contains(&0));
            let full =
                configuration_values(&text, Path::new("/fixture/hgs"), "/home/example").unwrap();
            assert_eq!(full["HGS_SELF"], "fixture");
            assert_eq!(full["HGS_PEERS"], "peer-a peer-b");
            assert_eq!(full["HGS_TAB"], "0");
            let forwarded = hint_policy_environment(policy(&text, &[]));
            assert_eq!(forwarded, ["HGS_AGENT_HINT=0", "HGS_AGENT_HINT_OFF="]);
            assert!(forwarded.iter().all(|entry| !entry.contains('\0')));
            let block = crate::host::spawn::Environment::parse(&forwarded).unwrap();
            assert_eq!(block.get("HGS_AGENT_HINT"), Some("0"));
            assert_eq!(block.get("HGS_AGENT_HINT_OFF"), Some(""));
            assert!(policy("", &[(key, "work\0tail")]).is_err());
        }
        assert!(resolved_hint_policy(Some("1\0"), None).is_err());
        for enabled in ["0", "1"] {
            assert!(resolved_hint_policy(Some(enabled), Some("work\0tail")).is_err());
        }
        let malformed = HintPolicy {
            enabled: true,
            off: "work\0tail".into(),
        };
        assert_eq!(
            malformed.environment(),
            ["HGS_AGENT_HINT=0", "HGS_AGENT_HINT_OFF="]
        );
        assert_eq!(
            policy("HGS_AGENT_HINT_OFF='work\t native-codex'", &[])
                .unwrap()
                .off,
            "work native-codex"
        );
    }

    #[test]
    fn agent_hint_resolved_windows_empty_list_never_supplements_a_different_file() {
        let windows = resolved_hint_policy(Some("1"), None).unwrap();
        let unix = resolved_hint_policy(Some("1"), Some("")).unwrap();
        assert_eq!(windows, unix);
        assert!(windows.allows("native-claude") && windows.allows("work"));
        assert!(!resolved_hint_policy(Some("0"), None)
            .unwrap()
            .allows("native-claude"));
        assert!(!resolved_hint_policy(Some("1"), Some("work"))
            .unwrap()
            .allows("work"));
        assert!(resolved_hint_policy(None, Some("work")).is_err());
        assert!(resolved_hint_policy(Some(""), None).is_err());
    }
    #[test]
    fn assignment_quotes_comments_and_expansions_match_supported_shell_format() {
        let values = HashMap::from([("BOX".to_owned(), "arch".to_owned())]);
        let parse = |raw| assignment_value(raw, "/home/test user", &values).unwrap();
        assert_eq!(parse(r#""arch mac" # two aliases"#), "arch mac");
        assert_eq!(parse("'mac=#80ff80'"), "mac=#80ff80");
        assert_eq!(parse("mac=#80ff80 # colour"), "mac=#80ff80");
        assert_eq!(parse(r#""${HOME}/$BOX""#), "/home/test user/arch");
        assert_eq!(parse("'$HOME/${BOX}'"), "$HOME/${BOX}");
        assert_eq!(parse(r#""\$HOME \\ \"quoted\"""#), "$HOME \\ \"quoted\"");
        assert_eq!(parse("arch\\ mac"), "arch mac");
        assert_eq!(parse("# empty value"), "");
    }

    #[test]
    fn reading_config_never_executes_shell_commands() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("must-not-exist");
        for raw in [
            format!("$(touch {})", marker.display()),
            format!("\"$(touch {})\"", marker.display()),
            format!("`touch {}`", marker.display()),
            format!("arch; touch {}", marker.display()),
            "arch | sh".into(),
            "arch && echo oops".into(),
            "${HOME:-fallback}".into(),
            "\"unterminated".into(),
            "arch mac".into(),
        ] {
            assert!(
                assignment_value(&raw, "/home/test", &HashMap::new()).is_err(),
                "accepted {raw}"
            );
            assert!(!marker.exists(), "executed {raw}");
        }
        // Single quotes deliberately preserve command-looking text as inert data.
        assert_eq!(
            assignment_value("'$(touch never)'", "/home/test", &HashMap::new()).unwrap(),
            "$(touch never)"
        );
    }
}
