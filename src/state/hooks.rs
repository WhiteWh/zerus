use super::*;
use sha2::{Digest, Sha256};
use std::fs;

const COMMON: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
    "Stop",
];

fn events(agent: &str) -> Vec<&'static str> {
    let mut events = COMMON.to_vec();
    match agent {
        "claude" => events.extend(["PostToolUseFailure", "StopFailure", "Notification"]),
        "codex" => events.push("Interrupt"),
        "kimi" => events.extend([
            "PostToolUseFailure",
            "StopFailure",
            "Notification",
            "Interrupt",
            "TurnStarted",
            "UserPromptQueued",
            "PermissionResult",
            "TaskStarted",
            "SessionHeartbeat",
        ]),
        _ => {}
    }
    events
}

/// The oldest Claude Code whose `PostModelSwitch` hgs has verified: the exact
/// model after each switch (`from_model`, `to_model`, `requested_model`,
/// `source`). The event name first appears in 2.1.251.
const CLAUDE_MODEL_SWITCH: Version = (2, 1, 296);

/// Events of `agent` that only newer versions have, each with the oldest
/// version hgs registers it for. Claude Code (2.1.200 and 2.1.296 alike) stops
/// every start at a settings warning while `settings.json` names a hook event
/// it does not know, so these are added only for a version read from the
/// agent's native CLI, never for a project launcher or a version that cannot
/// be read, and hgs's entry goes again before an older version starts
/// (`prune_json`). `PreModelSwitch` stays
/// unregistered: its allow would also skip Claude's cache-miss confirmation
/// for the user.
/// The oldest Claude Code whose `PermissionRequest` hook answers AskUserQuestion
/// with `updatedInput.answers` beside its open panel, and a tool approval with
/// a one-time allow or a deny (verified 2026-10-17).
const CLAUDE_ANSWER_CHANNEL: Version = (2, 1, 296);
/// The arguments with which the answer channel runs hgs.
const CHANNEL_ARGS: [&str; 2] = ["__state", "answer-channel"];
/// Long enough for a person to answer: the channel ends itself sooner.
const CHANNEL_TIMEOUT: u64 = 86_400;

fn versioned_events(agent: &str) -> &'static [(&'static str, Version)] {
    match agent {
        "claude" => &[("PostModelSwitch", CLAUDE_MODEL_SWITCH)],
        _ => &[],
    }
}

fn owns_hook(command: &str) -> bool {
    let Ok(argv) = shell_words::split(command) else {
        return false;
    };
    if argv.len() < 3 || argv.last().map(String::as_str) != Some("hook") {
        return false;
    }
    if Path::new(&argv[argv.len() - 2])
        .file_name()
        .and_then(|name| name.to_str())
        == Some("hgs_state.py")
    {
        return true;
    }
    argv[argv.len() - 2] == "__state"
        && Path::new(&argv[argv.len() - 3])
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name == "hgs" || name == "hgs.exe")
            .unwrap_or(false)
}

/// Whether `hook`, one item of a matcher group's `hooks`, is hgs's answer
/// channel: `hgs __state answer-channel` as a command line or in exec form.
fn owns_channel(hook: &Value) -> bool {
    let Some(command) = hook.get("command").and_then(Value::as_str) else {
        return false;
    };
    if let Some(args) = hook.get("args").and_then(Value::as_array) {
        return names_hgs(command) && args.iter().map(Value::as_str).eq(CHANNEL_ARGS.map(Some));
    }
    let Ok(argv) = shell_words::split(command) else {
        return false;
    };
    argv.len() >= 3 && argv[argv.len() - 2..] == CHANNEL_ARGS && names_hgs(&argv[argv.len() - 3])
}

pub(super) fn resolved_target(path: &Path) -> Result<PathBuf> {
    // Resolve even dangling symlinks, as pathlib.resolve() did, so replacing a
    // dotfile never replaces the user's symlink itself.
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let mut resolved = PathBuf::new();
    let mut remaining: std::collections::VecDeque<std::ffi::OsString> = absolute
        .components()
        .map(|component| component.as_os_str().to_owned())
        .collect();
    let mut links = 0;
    while let Some(component) = remaining.pop_front() {
        if component == "/" {
            resolved.push("/");
            continue;
        }
        if component == "." {
            continue;
        }
        if component == ".." {
            resolved.pop();
            continue;
        }
        resolved.push(&component);
        if let Ok(target) = fs::read_link(&resolved) {
            links += 1;
            if links > 40 {
                return Err(format!("too many symlinks: {}", path.display()));
            }
            resolved.pop();
            if target.is_absolute() {
                resolved.clear();
            }
            let mut pieces: std::collections::VecDeque<_> = target
                .components()
                .map(|c| c.as_os_str().to_owned())
                .collect();
            pieces.append(&mut remaining);
            remaining = pieces;
        }
    }
    Ok(resolved)
}

/// The hook entry hgs adds to a JSON hook configuration (Claude Code's
/// `settings.json`, Codex's `hooks.json`) for each event without one.
///
/// On Unix it is the shell command `'/path/to/hgs' __state hook`. On Windows
/// (spec D12) it names the stable CLI by its absolute path with forward
/// slashes: Claude Code gets its exec form when the installed version has it,
/// else the shell form `"C:/…/hgs.exe" __state hook` that Git Bash, `cmd.exe`
/// and `CreateProcess` read alike; Codex gets the shell form as `command` and
/// its PowerShell form as its Windows override `commandWindows`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    /// The command line a shell runs, or with `args` the executable.
    command: String,
    /// Claude Code's exec form: the executable's arguments, run without a shell.
    args: Option<Vec<String>>,
    /// Codex's command for Windows, which Codex prefers there.
    command_windows: Option<String>,
}

impl Entry {
    fn shell(command: String) -> Self {
        Self {
            command,
            args: None,
            command_windows: None,
        }
    }

    /// A matcher group holding this entry, as hgs writes it for one event.
    fn group(&self) -> Value {
        let mut hook = json!({"type": "command", "command": self.command, "timeout": 5});
        if let Some(args) = &self.args {
            hook["args"] = json!(args);
        }
        if let Some(command) = &self.command_windows {
            hook["commandWindows"] = json!(command);
        }
        json!({"hooks": [hook]})
    }

    /// The answer channel beside this hgs entry: the same program with
    /// `answer-channel` in place of `hook`. A legacy `hgs_state.py` has none.
    fn channel(&self) -> Option<Self> {
        if let Some(args) = &self.args {
            return args.iter().map(String::as_str).eq(HOOK_ARGS).then(|| Self {
                args: Some(CHANNEL_ARGS.map(str::to_owned).to_vec()),
                command_windows: None,
                ..self.clone()
            });
        }
        let argv = shell_words::split(&self.command).ok()?;
        if argv.len() < 3 || argv[argv.len() - 2..] != HOOK_ARGS || !names_hgs(&argv[argv.len() - 3]) {
            return None;
        }
        let program = self.command.strip_suffix("hook")?.trim_end();
        Some(Self::shell(format!("{program} answer-channel")))
    }

    /// The matcher group of the answer channel: every tool, so that it can
    /// answer AskUserQuestion and tool approvals, with a timeout long enough
    /// for a person.
    fn channel_group(&self) -> Value {
        let mut group = self.group();
        group["hooks"][0]["timeout"] = json!(CHANNEL_TIMEOUT);
        group
    }

    /// The hgs entry that `hook`, one item of a matcher group's `hooks`, is:
    /// a command line that runs `hgs __state hook` (or a legacy
    /// `hgs_state.py hook`), or on Windows also Claude Code's exec form that
    /// runs hgs with `__state hook`. A Windows entry keeps the fields hgs
    /// writes there, so a new event repeats it exactly.
    fn owned(hook: &Value, windows: bool) -> Option<Self> {
        let command = hook.get("command").and_then(Value::as_str)?;
        let text = |key: &str| hook.get(key).and_then(Value::as_str).map(str::to_owned);
        if owns_hook(command) {
            return Some(if windows {
                Self {
                    command_windows: text("commandWindows"),
                    ..Self::shell(command.to_owned())
                }
            } else {
                Self::shell(command.to_owned())
            });
        }
        let args: Vec<&str> = hook
            .get("args")?
            .as_array()?
            .iter()
            .map(Value::as_str)
            .collect::<Option<_>>()?;
        (windows && args == HOOK_ARGS && names_hgs(command)).then(|| Self {
            args: Some(HOOK_ARGS.map(str::to_owned).to_vec()),
            ..Self::shell(command.to_owned())
        })
    }
}

/// The arguments with which every hook runs hgs.
const HOOK_ARGS: [&str; 2] = ["__state", "hook"];

/// Whether `program` names hgs: `hgs` or `hgs.exe` (any case) after its last
/// `/` or `\`.
fn names_hgs(program: &str) -> bool {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    name == "hgs" || name.eq_ignore_ascii_case("hgs.exe")
}

/// The Kimi hook block's markers. On Windows each line may also end with CR LF
/// (a `config.toml` that an editor saved with Windows line ends), so such a
/// file never gets a second block.
const KIMI_BEGIN: &str = "# BEGIN hgs session tracking";
const KIMI_END: &str = "# END hgs session tracking";

/// Where the first `marker` line of `text` starts, and its length with its
/// line end: `\n`, or with `crlf` also `\r\n`.
fn marker_line(text: &str, marker: &str, crlf: bool) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(found) = text[from..].find(marker) {
        let start = from + found;
        let rest = &text[start + marker.len()..];
        if rest.starts_with('\n') {
            return Some((start, marker.len() + 1));
        }
        if crlf && rest.starts_with("\r\n") {
            return Some((start, marker.len() + 2));
        }
        from = start + marker.len();
    }
    None
}

/// The Kimi `config.toml` text `original` with hgs's block of hooks, or `None`
/// when its block already has every event: then its bytes are kept. A block
/// that misses events is written again, with the hgs command it holds when it
/// holds one and else the command `fresh` gives.
fn merge_kimi(
    original: &str,
    path: &Path,
    crlf: bool,
    fresh: &mut dyn FnMut() -> Result<String>,
) -> Result<Option<String>> {
    let mut command = None;
    let mut content_without = original.to_owned();
    if let Some((start, begin)) = marker_line(original, KIMI_BEGIN, crlf) {
        let rest = &original[start + begin..];
        let (finish, end) = marker_line(rest, KIMI_END, crlf)
            .ok_or_else(|| format!("incomplete hgs hook block in {}", path.display()))?;
        let block = &rest[..finish];
        let document = block
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| format!("cannot merge tracking hooks into {}: {e}", path.display()))?;
        let without = format!("{}{}", &original[..start], &rest[finish + end..]);
        // Older installations registered the compatibility shim outside this
        // block. It executes the same Rust hook: adding a second complete set
        // records every callback twice. Remove only an entirely redundant HGS
        // block, retaining the original trusted commands and all personal bytes.
        if let Ok(existing) = without.parse::<toml_edit::DocumentMut>() {
            if kimi_complete(&existing)
                && document.as_table().len() == 1
                && document.get("hooks").and_then(toml_edit::Item::as_array_of_tables).is_some_and(|tables| {
                    !tables.is_empty() && tables.iter().all(|table| {
                        table.get("command").and_then(toml_edit::Item::as_str).is_some_and(owns_hook)
                            && existing.get("hooks").and_then(toml_edit::Item::as_array_of_tables)
                                .is_some_and(|registered| registered.iter().any(|other| {
                                    other.get("command").and_then(toml_edit::Item::as_str).is_some_and(owns_hook)
                                        && kimi_hook_fields(table) == kimi_hook_fields(other)
                                }))
                    })
                })
            {
                return Ok(Some(without));
            }
        }
        if let Some(tables) = document
            .get("hooks")
            .and_then(toml_edit::Item::as_array_of_tables)
        {
            command = tables
                .iter()
                .filter_map(|table| table.get("command").and_then(toml_edit::Item::as_str))
                .find(|value| owns_hook(value))
                .map(str::to_owned);
            if events("kimi").iter().all(|event| {
                tables.iter().any(|table| {
                    table.get("event").and_then(toml_edit::Item::as_str) == Some(event)
                        && table
                            .get("command")
                            .and_then(toml_edit::Item::as_str)
                            .map(owns_hook)
                            .unwrap_or(false)
                })
            }) {
                // Validate the complete document but retain exact trusted
                // hook bytes, comments, ordering and user settings.
                original
                    .parse::<toml_edit::DocumentMut>()
                    .map_err(|e| e.to_string())?;
                return Ok(None);
            }
        }
        content_without = format!("{}{}", &original[..start], &rest[finish + end..]);
    }
    let document = original.parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("cannot merge tracking hooks into {}: {e}", path.display()))?;
    if kimi_complete(&document) {
        return Ok(None);
    }
    let command = match command {
        Some(command) => command,
        None => fresh()?,
    };
    let mut block = format!("{KIMI_BEGIN}\n");
    for event in events("kimi") {
        block.push_str(&format!(
            "[[hooks]]\nevent = {}\ncommand = {}\ntimeout = 5\n",
            json!(event),
            json!(command)
        ));
    }
    block.push_str(&format!("{KIMI_END}\n"));
    let content = format!("{}\n\n{block}", content_without.trim_end_matches('\n'));
    content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("cannot merge tracking hooks into {}: {e}", path.display()))?;
    Ok(Some(content))
}

fn kimi_hook_fields(table: &toml_edit::Table) -> std::collections::BTreeMap<String, String> {
    table.iter().filter(|(key, _)| *key != "command")
        .map(|(key, value)| (key.to_owned(), value.to_string().trim().to_owned())).collect()
}

fn kimi_complete(document: &toml_edit::DocumentMut) -> bool {
    document.get("hooks").and_then(toml_edit::Item::as_array_of_tables).is_some_and(|tables| {
        events("kimi").iter().all(|event| tables.iter().filter(|table| {
            table.get("event").and_then(toml_edit::Item::as_str) == Some(event)
                && table.get("command").and_then(toml_edit::Item::as_str).is_some_and(owns_hook)
                && !table.contains_key("matcher")
        }).count() == 1)
    })
}

/// The JSON hook configuration text `original` of `agent` with an hgs entry
/// for each of its events, or `None` when every event has one: then nothing
/// is written. New events repeat the hgs entry already there, exactly;
/// only without one is `fresh` asked for the entry to write. An event of
/// `versioned_events` without an entry asks `version` for the agent's version
/// and is added only when that version has it.
fn merge_json(
    original: &str,
    path: &Path,
    agent: &str,
    windows: bool,
    fresh: &mut dyn FnMut() -> Result<Entry>,
    version: &mut dyn FnMut() -> Option<Version>,
) -> Result<Option<String>> {
    merge_json_mode(original,path,agent,windows,fresh,version,true)
}

fn merge_json_mode(
    original: &str,
    path: &Path,
    agent: &str,
    windows: bool,
    fresh: &mut dyn FnMut() -> Result<Entry>,
    version: &mut dyn FnMut() -> Option<Version>,
    channels: bool,
) -> Result<Option<String>> {
    let mut document: Value = if original.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(original)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?
    };
    let object = document
        .as_object_mut()
        .ok_or("hook config must be an object")?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("hooks must be an object")?;
    let mut entry = hooks
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
        .flatten()
        .find_map(|hook| Entry::owned(hook, windows));
    let mut changed = false;
    // One version reading serves every versioned entry.
    let mut known = None;
    let mut version = || *known.get_or_insert_with(&mut *version);
    let versioned = versioned_events(agent)
        .iter()
        .map(|&(event, oldest)| (event, Some(oldest)));
    for (event, oldest) in events(agent)
        .into_iter()
        .map(|event| (event, None))
        .chain(versioned)
    {
        let installed = match hooks.get(event) {
            Some(entries) => entries
                .as_array()
                .ok_or("hook event must be an array")?
                .iter()
                .filter_map(|group| group.get("hooks").and_then(Value::as_array))
                .flatten()
                .any(|hook| Entry::owned(hook, windows).is_some()),
            None => false,
        };
        // A version that lacks the event, or cannot be read, gets no key for it.
        if installed || oldest.is_some_and(|oldest| version().is_none_or(|v| v < oldest)) {
            continue;
        }
        let group = match &entry {
            Some(entry) => entry.group(),
            None => {
                let new = fresh()?;
                let group = new.group();
                entry = Some(new);
                group
            }
        };
        hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("hook event must be an array")?
            .push(group);
        changed = true;
    }
    let channel_installed = hooks
        .get("PermissionRequest")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
        .flatten()
        .any(owns_channel);
    // The group hgs wrote for AskUserQuestion alone also answers approvals
    // once it matches every tool: the same entry, changed in place. A group
    // that holds anything else keeps its matcher. hgs never wrote that group
    // on Windows.
    let narrowed = |group: &Value| {
        group.get("matcher").and_then(Value::as_str) == Some("AskUserQuestion")
            && group
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|hooks| !hooks.is_empty() && hooks.iter().all(owns_channel))
    };
    if channels && agent == "claude"
        && !windows
        && hooks
            .get("PermissionRequest")
            .and_then(Value::as_array)
            .is_some_and(|groups| groups.iter().any(narrowed))
        && version().is_some_and(|version| version >= CLAUDE_ANSWER_CHANNEL)
    {
        for group in hooks["PermissionRequest"].as_array_mut().into_iter().flatten() {
            if narrowed(group) {
                group.as_object_mut().ok_or("hook group must be an object")?.remove("matcher");
                changed = true;
            }
        }
    }
    if channels && agent == "claude"
        && !channel_installed
        && version().is_some_and(|version| version >= CLAUDE_ANSWER_CHANNEL)
    {
        let base = match &entry {
            Some(entry) => entry.clone(),
            None => fresh()?,
        };
        // On Windows only Claude's exec form runs the channel: Claude starts
        // hgs itself, so the hook process is the waiting channel. The shell
        // form puts Git Bash in between and keeps the terminal path.
        if let Some(channel) = base.channel().filter(|channel| !windows || channel.args.is_some()) {
            hooks
                .entry("PermissionRequest")
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .ok_or("hook event must be an array")?
                .push(channel.channel_group());
            changed = true;
        }
    }
    if !changed {
        return Ok(None);
    }
    Ok(Some(format!(
        "{}\n",
        serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?
    )))
}

/// The JSON hook configuration text `original` of `agent` without hgs's
/// entries for a `versioned_events` event that the agent's version lacks, or
/// `None` when nothing goes: then its bytes are kept. Only where hgs has such
/// an entry is `version` asked, and only a version read as older removes it:
/// one that cannot be read keeps a working entry. The user's hooks stay, a
/// matcher group goes only when hgs's entries were all it held, and the event
/// key only when nothing is left in it. Text that `merge_json` cannot read
/// stays for it to report.
fn prune_json(
    original: &str,
    agent: &str,
    windows: bool,
    version: &mut dyn FnMut() -> Option<Version>,
) -> Option<String> {
    let mut document: Value = serde_json::from_str(original).ok()?;
    let hooks = document.get_mut("hooks")?.as_object_mut()?;
    let owned = |hook: &Value| Entry::owned(hook, windows).is_some();
    let mut changed = false;
    let mut known = None;
    let mut version = || *known.get_or_insert_with(&mut *version);
    for &(event, oldest) in versioned_events(agent) {
        let Some(groups) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
            continue;
        };
        let installed = groups
            .iter()
            .filter_map(|group| group.get("hooks").and_then(Value::as_array))
            .flatten()
            .any(owned);
        // A version that cannot be read may well have the event: its entry stays.
        if !installed || version().is_none_or(|v| v >= oldest) {
            continue;
        }
        groups.retain_mut(|group| {
            let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let before = entries.len();
            entries.retain(|hook| !owned(hook));
            entries.len() == before || !entries.is_empty()
        });
        if groups.is_empty() {
            hooks.remove(event);
        }
        changed = true;
    }
    if agent == "claude" {
        if let Some(groups) = hooks.get_mut("PermissionRequest").and_then(Value::as_array_mut) {
            let installed = groups
                .iter()
                .filter_map(|group| group.get("hooks").and_then(Value::as_array))
                .flatten()
                .any(owns_channel);
            // A version that cannot be read keeps a working channel.
            if installed && version().is_some_and(|version| version < CLAUDE_ANSWER_CHANNEL) {
                groups.retain_mut(|group| {
                    let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                        return true;
                    };
                    let before = entries.len();
                    entries.retain(|hook| !owns_channel(hook));
                    entries.len() == before || !entries.is_empty()
                });
                if groups.is_empty() {
                    hooks.remove("PermissionRequest");
                }
                changed = true;
            }
        }
    }
    if !changed {
        return None;
    }
    Some(format!(
        "{}\n",
        serde_json::to_string_pretty(&document).ok()?
    ))
}

/// The file that holds the hooks of `agent`, past any symlinks.
fn hook_config(agent: &str) -> Result<PathBuf> {
    let home_key = home_var(agent)?;
    let directory = nonempty_env(home_key)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            home().join(if agent == "kimi" {
                ".kimi-code".to_owned()
            } else {
                format!(".{agent}")
            })
        });
    resolved_target(&directory.join(match agent {
        "kimi" => "config.toml",
        "claude" => "settings.json",
        _ => "hooks.json",
    }))
}

/// Where the version last read for the hooks of the file `path` is kept.
fn seen_version(path: &Path) -> PathBuf {
    root().join("hook-versions").join(format!(
        "{:x}.json",
        Sha256::digest(path.to_string_lossy().as_bytes())
    ))
}

/// The version of the native Claude Code CLI `program` as its hooks know it:
/// the one last read while the executable stays the same file, else read once
/// now and kept. `None` for a project launcher, which runs only under
/// supervision, and for a version that cannot be read.
pub(super) fn claude_version(program: &str) -> Option<(u64, u64, u64)> {
    let installed = Installed {
        program,
        agent: "Claude Code",
        version: std::cell::OnceCell::new(),
        seen: seen_version(&hook_config("claude").ok()?),
    };
    installed.registered_version("claude")
}

/// Install hgs's tracking hooks into the configuration of `agent`, whose
/// executable is `program` (spec D12 on Windows).
pub(super) fn install_hooks(agent: &str, program: &str) -> Result<()> {
    let path = hook_config(agent)?;
    let lock_path = path.with_file_name(format!(
        "{}.hgs.lock",
        path.file_name().unwrap().to_string_lossy()
    ));
    let _guard = lock(Some(&lock_path))?;
    let original = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.to_string()),
    };
    let executable = runner_executable()?;
    let installed = Installed {
        program,
        agent: if agent == "claude" { "Claude Code" } else { agent },
        version: std::cell::OnceCell::new(),
        seen: seen_version(&path),
    };
    let merged = if agent == "kimi" {
        merge_kimi(&original, &path, cfg!(windows), &mut || {
            fresh_command(&executable)
        })?
    } else {
        // An older version run with this configuration later would stop at
        // hgs's entry for an event it lacks: the entry goes before it starts.
        let pruned = prune_json(&original, agent, cfg!(windows), &mut || {
            installed.registered_version(agent)
        });
        merge_json(
            pruned.as_deref().unwrap_or(&original),
            &path,
            agent,
            cfg!(windows),
            &mut || fresh_entry(agent, &executable, &installed),
            &mut || installed.native_version(agent),
        )?
        .or(pruned)
    };
    let Some(content) = merged else {
        return Ok(());
    };
    if content != original {
        let backup = path.with_file_name(format!(
            "{}.pre-hgs",
            path.file_name().unwrap().to_string_lossy()
        ));
        if !original.is_empty() && !backup.exists() {
            atomic(&backup, &original)?;
        }
        atomic(&path, &content)?;
    }
    Ok(())
}

/// The agent whose hooks an installation writes: its executable `program`, its
/// name for messages, and the version `program --version` reports, read at
/// most once and only when a hook depends on it. `seen` keeps the version last
/// read for this configuration with the executable it was read from.
struct Installed<'a> {
    program: &'a str,
    agent: &'a str,
    version: std::cell::OnceCell<Result<Version>>,
    seen: PathBuf,
}

impl Installed<'_> {
    fn version(&self) -> Result<Version> {
        self.version
            .get_or_init(|| version::agent_version(self.program, self.agent))
            .clone()
    }

    /// The version of the native CLI `cli`, never of a project launcher such
    /// as `run_claude.sh`: a launcher runs only under supervision, since it may
    /// prompt or unlock secrets, so its version stays unknown.
    fn native_version(&self, cli: &str) -> Option<Version> {
        if self.program != cli {
            return None;
        }
        let identity = version::executable_identity(self.program);
        let version = self.version().ok()?;
        if let Some(identity) = identity {
            version::remember(&self.seen, &identity, version);
        }
        Some(version)
    }

    /// `native_version` for an event whose entry is already there: the version
    /// last read while the executable stays the same file, so a current agent
    /// is not asked at every start, else read again. A launcher, an
    /// executable that cannot be found and a version that cannot be read stay
    /// unknown.
    fn registered_version(&self, cli: &str) -> Option<Version> {
        if self.program != cli {
            return None;
        }
        let identity = version::executable_identity(self.program)?;
        if let Some(version) = version::recalled(&self.seen, &identity) {
            return Some(version);
        }
        let version = self.version().ok()?;
        version::remember(&self.seen, &identity, version);
        Some(version)
    }
}

/// Read-only evidence: never runs a provider or refreshes the version cache.
pub(super) fn health_version(path: &Path, program: &str) -> Option<(u64, u64, u64)> {
    let identity = version::executable_identity(program)?;
    let bytes = super::hook_health::read_bounded(&seen_version(path), 4096).ok()??;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    (value["executable"] == identity).then(|| version::version_in(value["version"].as_str()?))?
}

pub(super) fn health_events(agent: &str) -> Vec<&'static str> {
    events(agent).into_iter().chain(versioned_events(agent).iter().map(|(event,_)| *event)).collect()
}

/// Content-free registration inventory. Native trust/policy is deliberately not inferred.
pub(super) fn health_document(original: &str, agent: &str, known: Option<(u64, u64, u64)>) -> Result<Value> {
    health_document_mode(original, agent, known, cfg!(windows))
}

fn health_document_mode(original: &str, agent: &str, known: Option<Version>, windows: bool) -> Result<Value> {
    let mut present = std::collections::BTreeSet::new();
    let mut channel = false;
    let mut channel_shell = false;
    let mut counts = std::collections::BTreeMap::<String,usize>::new();
    let mut conditional = false;
    if agent == "kimi" {
        let document = original.parse::<toml_edit::DocumentMut>().map_err(|_| "Invalid hook configuration")?;
        if let Some(item) = document.get("hooks") {
            let tables = item.as_array_of_tables().ok_or("Invalid hook configuration")?;
            for table in tables {
                if table.get("command").and_then(toml_edit::Item::as_str).is_some_and(owns_hook) {
                    if let Some(event) = table.get("event").and_then(toml_edit::Item::as_str) {
                        if events(agent).contains(&event) { present.insert(event.to_owned()); *counts.entry(event.to_owned()).or_default() += 1; }
                    }
                }
            }
        }
    } else {
        let document: Value = if original.trim().is_empty() { json!({}) } else {
            serde_json::from_str(original).map_err(|_| "Invalid hook configuration")?
        };
        let object = document.as_object().ok_or("Invalid hook configuration")?;
        if let Some(hooks) = object.get("hooks") {
            let hooks = hooks.as_object().ok_or("Invalid hook configuration")?;
            // Only the bounded event names hgs itself knows may be returned.
            let names = events(agent).into_iter().chain(versioned_events(agent).iter().map(|(event, _)| *event));
            for event in names {
                if let Some(groups) = hooks.get(event) {
                    for group in groups.as_array().ok_or("Invalid hook configuration")? {
                        for hook in group.get("hooks").and_then(Value::as_array).ok_or("Invalid hook configuration")? {
                            let tracking = Entry::owned(hook, windows).is_some();
                            let answer = owns_channel(hook);
                            if (tracking || answer) && hook.get("type").and_then(Value::as_str) != Some("command") { return Err("Invalid hook configuration".into()); }
                            if tracking { present.insert(event.to_owned()); *counts.entry(event.to_owned()).or_default() += 1; }
                            if event == "PermissionRequest" && answer {
                                channel = true;
                                channel_shell |= hook.get("args") != Some(&json!(CHANNEL_ARGS));
                            }
                            if (tracking || answer) && group.get("matcher").is_some_and(|value| value.as_str().is_none_or(|s| !s.is_empty() && s != "*")) {
                                conditional = true;
                            }
                        }
                    }
                }
            }
        }
    }
    let expected: Vec<_> = events(agent).into_iter().chain(versioned_events(agent).iter()
        .filter(|(_, minimum)| known.is_some_and(|version| version >= *minimum)).map(|(event, _)| *event)).collect();
    let missing: Vec<_> = expected.iter().filter(|event| !present.contains(**event)).copied().collect();
    let incompatible: Vec<_> = versioned_events(agent).iter().filter(|(event, minimum)| present.contains(*event) && known.is_some_and(|version| version < *minimum)).map(|(event,_)| *event).collect();
    let duplicates: Vec<_> = counts.iter().filter(|(_,count)| **count > 1).map(|(event,_)| event).collect();
    let channel_incompatible = channel && (windows && channel_shell || agent != "claude" || known.is_some_and(|v| v < CLAUDE_ANSWER_CHANNEL));
    Ok(json!({"state":if missing.is_empty(){"configured"}else if present.is_empty(){"missing"}else{"partial"},
        "expected_events":expected,"registered_events":present,"missing_events":missing,"duplicate_events":duplicates,"conditional_matchers":conditional,
        "answer_channel_registered":channel,"compatibility":{"state":if !incompatible.is_empty() || channel_incompatible {"incompatible"}else if known.is_some(){"compatible"}else{"unknown"},
        "basis":"Cached executable identity and known HGS versioned entries only",
        "native_version":known.map(|(a,b,c)|format!("{a}.{b}.{c}")),"unsupported_registered_events":incompatible}}))
}

/// Explicit repair uses only a previously verified version. It never probes a provider.
pub(super) fn repair_document(original: &str, path: &Path, agent: &str, known: (u64,u64,u64)) -> Result<Option<String>> {
    let executable = runner_executable()?;
    if agent == "kimi" {
        let health=health_document(original,agent,Some(known))?;
        if health["state"]=="configured" {
            if health["duplicate_events"].as_array().is_none_or(Vec::is_empty) { return Ok(None); }
            return merge_kimi(original, path, cfg!(windows), &mut || fresh_command(&executable));
        }
        // Rewriting an existing Kimi block can discard custom fields/personal
        // entries within it. Explicit repair refuses that lossy transformation.
        if health["state"]=="partial" || original.contains(KIMI_BEGIN) || original.contains(KIMI_END) {
            return Err("Partial Kimi hooks require a supervised launch or manual review; explicit repair preserved the existing block".into());
        }
        return merge_kimi(original, path, cfg!(windows), &mut || fresh_command(&executable));
    }
    // Existing owned entries (including trusted legacy commands) remain exact.
    // Version-incompatible registrations are not removed by explicit repair.
    let health = health_document(original, agent, Some(known))?;
    if health["compatibility"]["state"] != "compatible" { return Err("Hook configuration is incompatible; use a qualified supervised launch".into()); }
    merge_json_mode(original, path, agent, cfg!(windows), &mut || {
        #[cfg(windows)]
        { if agent=="claude" { windows::claude_entry(&executable, known, windows::git_bash().is_some()) } else { windows::codex_entry(&executable) } }
        #[cfg(unix)]
        { Ok(Entry::shell(fresh_command(&executable)?)) }
    }, &mut || Some(known), false)
}

/// The command line a hook runs `executable __state hook` with.
#[cfg(unix)]
fn fresh_command(executable: &str) -> Result<String> {
    Ok(join_command(&[
        executable.to_owned(),
        HOOK_ARGS[0].into(),
        HOOK_ARGS[1].into(),
    ]))
}

#[cfg(unix)]
fn fresh_entry(_agent: &str, executable: &str, _installed: &Installed) -> Result<Entry> {
    fresh_command(executable).map(Entry::shell)
}

#[cfg(windows)]
fn fresh_command(executable: &str) -> Result<String> {
    windows::shell_command(executable)
}

#[cfg(windows)]
fn fresh_entry(agent: &str, executable: &str, installed: &Installed) -> Result<Entry> {
    if agent == "claude" {
        windows::claude_entry(executable, installed.version()?, windows::git_bash().is_some())
    } else {
        windows::codex_entry(executable)
    }
}

/// Agent versions, `MAJOR.MINOR.PATCH`, and reading the installed one.
mod version {
    use super::{atomic, Result};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::{Read, Seek, SeekFrom};
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    pub(super) type Version = (u64, u64, u64);

    /// How long `--version` may take: a first start of a large agent under
    /// real-time scanning is slow, and a hook form is decided once.
    const VERSION_TIMEOUT: Duration = Duration::from_secs(20);

    /// The first `MAJOR.MINOR.PATCH` word of `text` (a `--version` answer such
    /// as `2.1.296 (Claude Code)` or `codex-cli 0.162.0`), any suffix after
    /// `-` or `+` ignored.
    pub(super) fn version_in(text: &str) -> Option<Version> {
        text.split_whitespace().find_map(|word| {
            let word = word.strip_prefix('v').unwrap_or(word);
            let core = word.split(['-', '+']).next()?;
            let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
            let version = (parts.next()??, parts.next()??, parts.next()??);
            parts.next().is_none().then_some(version)
        })
    }

    /// The version that `program --version` reports, without a session's
    /// identity. A version that cannot be read is an error: a form chosen
    /// without it would be kept for ever.
    pub(super) fn agent_version(program: &str, agent: &str) -> Result<Version> {
        let fail = |why: String| {
            format!(
                "cannot read the {agent} version ({program} --version: {why}), which decides \
                 its tracking hook form; set HGS_TRACKING=0 to run it untracked"
            )
        };
        let mut output = tempfile::tempfile().map_err(|e| fail(e.to_string()))?;
        let mut command = crate::platform::agent_command(&[program.to_owned(), "--version".into()])
            .map_err(|e| fail(e.to_string()))?;
        command
            .env_remove("HGS_RUN_ID")
            .env_remove("HGS_SESSION")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(output.try_clone().map_err(|e| fail(e.to_string()))?);
        // On Unix in a process group of its own, so a timeout also stops
        // whatever a wrapper script started.
        #[cfg(unix)]
        let spawned = crate::platform::spawn_group(&mut command);
        #[cfg(windows)]
        let spawned = crate::platform::spawn(&mut command);
        let mut child = spawned.map_err(|e| fail(e.to_string()))?;
        let deadline = Instant::now() + VERSION_TIMEOUT;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| fail(e.to_string()))? {
                break status;
            }
            if Instant::now() >= deadline {
                crate::platform::kill_process_group(&mut child);
                let _ = child.wait();
                return Err(fail("timed out".into()));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if !status.success() {
            return Err(fail(format!("exit {status}")));
        }
        let mut text = String::new();
        output
            .seek(SeekFrom::Start(0))
            .and_then(|_| output.take(64 * 1024).read_to_string(&mut text))
            .map_err(|e| fail(e.to_string()))?;
        version_in(&text).ok_or_else(|| fail(format!("no version in {:?}", text.trim())))
    }

    /// What `program` starts, as a few file system calls tell it: each file it
    /// runs (on Windows also the script of an npm shim) by its real path, size
    /// and times, and on Unix by its inode too. Installing another version
    /// changes it; a wrapper script that starts another program does not.
    /// `None` when `program` cannot be found.
    pub(super) fn executable_identity(program: &str) -> Option<String> {
        let command = crate::platform::agent_command(&[program.to_owned()]).ok()?;
        let started = crate::platform::which(command.get_program().to_str()?)?;
        let nanos = |time: std::io::Result<SystemTime>| {
            time.ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos())
        };
        let mut identity = String::new();
        for file in std::iter::once(started).chain(command.get_args().map(PathBuf::from)) {
            let file = fs::canonicalize(file).ok()?;
            let meta = fs::metadata(&file).ok()?;
            #[cfg(unix)]
            let changed = {
                use std::os::unix::fs::MetadataExt;
                format!(
                    "{}:{}:{}.{}",
                    meta.dev(),
                    meta.ino(),
                    meta.ctime(),
                    meta.ctime_nsec()
                )
            };
            #[cfg(windows)]
            let changed = nanos(meta.created());
            identity.push_str(&format!(
                "{}\0{}\0{}\0{changed}\n",
                file.display(),
                meta.len(),
                nanos(meta.modified())
            ));
        }
        Some(format!("{:x}", Sha256::digest(identity.as_bytes())))
    }

    /// The version that `remember` kept in `seen` for the executable `identity`.
    pub(super) fn recalled(seen: &Path, identity: &str) -> Option<Version> {
        let record: Value = serde_json::from_slice(&fs::read(seen).ok()?).ok()?;
        if record["executable"] != identity {
            return None;
        }
        version_in(record["version"].as_str()?)
    }

    /// Keep in `seen` that the executable `identity` reported `version`. A
    /// record that cannot be written only means the version is read again.
    pub(super) fn remember(seen: &Path, identity: &str, version: Version) {
        if recalled(seen, identity) != Some(version) {
            let (major, minor, patch) = version;
            let record =
                json!({"executable": identity, "version": format!("{major}.{minor}.{patch}")});
            let _ = atomic(seen, &record.to_string());
        }
    }
}

use self::version::Version;

/// The Windows hook forms of spec D12. The decisions are plain functions
/// tested on every platform; finding Git Bash is Windows-only.
#[cfg_attr(not(windows), allow(dead_code))]
mod windows {
    use super::{Entry, Result, Version, HOOK_ARGS};

    /// The oldest Claude Code whose exec-form hooks (`command` plus `args`, no
    /// shell) hgs has verified: the settings schema of 2.1.296 describes them.
    pub(super) const CLAUDE_EXEC_FORM: Version = (2, 1, 296);

    /// Characters that Git Bash, `cmd.exe`, `CreateProcess` and PowerShell
    /// read differently inside double quotes: `"` ends them, `%` expands a
    /// variable in `cmd.exe` and `!` with its delayed expansion, `$` and `` ` ``
    /// expand in bash and PowerShell.
    const SHELL_SPECIAL: &[char] = &['"', '%', '!', '$', '`'];

    /// What PowerShell also reads as a double quote: a path with one would end
    /// the quoted program of Codex's PowerShell form.
    const POWERSHELL_QUOTES: &[char] = &['\u{201C}', '\u{201D}', '\u{201E}'];

    /// The absolute Windows path `path` of the stable CLI with forward
    /// slashes, as every Windows hook names it: `C:/Users/example/.local/bin/hgs.exe`.
    pub(super) fn path(path: &str) -> Result<String> {
        let path = path.replace('\\', "/");
        let bytes = path.as_bytes();
        let drive = bytes.len() > 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'/';
        let unc = path.starts_with("//") && !path.starts_with("//?/") && !path.starts_with("//./");
        if !drive && !unc {
            return Err(format!(
                "the stable hgs CLI {path} is not an absolute drive or UNC path, so tracking \
                 hooks cannot name it"
            ));
        }
        if path.chars().any(char::is_control) {
            return Err(format!(
                "the stable hgs CLI path {path:?} contains a control character"
            ));
        }
        Ok(path)
    }

    /// The shell form: `"C:/Users/example/.local/bin/hgs.exe" __state hook`, which
    /// Git Bash, `cmd.exe` (as `cmd /c` and `cmd /d /s /c` run it) and a
    /// direct `CreateProcess` all parse into that program and `__state hook`.
    /// PowerShell does not: it reads a quoted program as a string.
    pub(super) fn shell_command(stable: &str) -> Result<String> {
        let path = path(stable)?;
        if let Some(special) = path.chars().find(|c| SHELL_SPECIAL.contains(c)) {
            return Err(format!(
                "the stable hgs CLI path {path} contains {special:?}, which Git Bash, cmd.exe \
                 and CreateProcess read differently in a hook command; tracked sessions need \
                 a profile path without it. Set HGS_TRACKING=0 to run the agent untracked"
            ));
        }
        Ok(format!("\"{path}\" {} {}", HOOK_ARGS[0], HOOK_ARGS[1]))
    }

    /// Claude Code's entry for the installed `version`: its exec form from
    /// `CLAUDE_EXEC_FORM` on, else the shell form, which Claude Code runs with
    /// Git Bash. Without Git Bash it would run that in PowerShell, which cannot
    /// run it, so then nothing is written.
    pub(super) fn claude_entry(stable: &str, version: Version, git_bash: bool) -> Result<Entry> {
        if version >= CLAUDE_EXEC_FORM {
            return Ok(Entry {
                command: path(stable)?,
                args: Some(HOOK_ARGS.map(str::to_owned).to_vec()),
                command_windows: None,
            });
        }
        if !git_bash {
            let (major, minor, patch) = version;
            let (a, b, c) = CLAUDE_EXEC_FORM;
            return Err(format!(
                "Claude Code {major}.{minor}.{patch} runs hook commands in PowerShell when Git \
                 Bash is missing, and PowerShell cannot run hgs's tracking hook: install Git \
                 for Windows or update Claude Code to {a}.{b}.{c} or later. Set HGS_TRACKING=0 \
                 to run it untracked"
            ));
        }
        shell_command(stable).map(Entry::shell)
    }

    /// The PowerShell form, `& "C:/…/hgs.exe" __state hook; exit
    /// $LASTEXITCODE`: PowerShell runs a quoted program only with its call
    /// operator, and its `-Command` turns a native exit code other than 0 or 1
    /// into 1 unless the command exits with it (hgs's 2 blocks a prompt during
    /// a pause). The command never reads `$input`, so PowerShell leaves its
    /// standard input, the hook's payload, to hgs unread.
    pub(super) fn powershell_command(stable: &str) -> Result<String> {
        shell_command(stable)?;
        let path = path(stable)?;
        if let Some(quote) = path.chars().find(|c| POWERSHELL_QUOTES.contains(c)) {
            return Err(format!(
                "the stable hgs CLI path {path} contains {quote:?}, which PowerShell reads as \
                 a double quote in a hook command; tracked sessions need a profile path \
                 without it. Set HGS_TRACKING=0 to run the agent untracked"
            ));
        }
        Ok(format!(
            "& \"{path}\" {} {}; exit $LASTEXITCODE",
            HOOK_ARGS[0], HOOK_ARGS[1]
        ))
    }

    /// Codex's entry: the shell form as `command`, and the PowerShell form as
    /// `commandWindows`, which Codex prefers on Windows and runs with the
    /// session's shell, PowerShell there (`-NoProfile -Command`, Codex 0.140 to
    /// 0.162: `build_hooks_config`, `default_user_shell`). `cmd.exe` runs it
    /// only for a session without one local environment, which hgs never
    /// starts, or on a computer without PowerShell.
    pub(super) fn codex_entry(stable: &str) -> Result<Entry> {
        Ok(Entry {
            command_windows: Some(powershell_command(stable)?),
            ..Entry::shell(shell_command(stable)?)
        })
    }

    #[cfg(windows)]
    pub(super) use self::probe::git_bash;

    #[cfg(windows)]
    mod probe {
        use crate::state::nonempty_env;
        use std::path::PathBuf;

        /// The Git Bash that Claude Code would run a hook's shell form with:
        /// `CLAUDE_CODE_GIT_BASH_PATH`, Git for Windows in Program Files, or
        /// the `bin\bash.exe` of the `git` on PATH (Claude Code 2.1.296).
        pub(in crate::state::hooks) fn git_bash() -> Option<PathBuf> {
            if let Some(path) = nonempty_env("CLAUDE_CODE_GIT_BASH_PATH").map(PathBuf::from) {
                let named = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        ["bash.exe", "sh.exe", "bash", "sh"].contains(&name.to_lowercase().as_str())
                    });
                if named && path.is_file() {
                    return Some(path);
                }
            }
            [
                r"C:\Program Files\Git\bin\bash.exe",
                r"C:\Program Files (x86)\Git\bin\bash.exe",
            ]
            .into_iter()
            .map(PathBuf::from)
            .chain(
                crate::platform::which("git")
                    .and_then(|git| Some(git.parent()?.parent()?.join(r"bin\bash.exe"))),
            )
            .find(|path| path.is_file())
        }
    }
}

fn command_refusal(event: &Value, home: &str) -> Option<command_guard::Refusal> {
    if string(event, "hook_event_name") != "PreToolUse" {
        return None;
    }
    let input = &event["tool_input"];
    for key in ["command", "cmd"] {
        if let Some(command) = input[key].as_str() {
            return command_guard::check(command, home);
        }
        if let Some(parts) = input[key].as_array() {
            let command = parts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ");
            return command_guard::check(&command, home);
        }
    }
    None
}

#[cfg(test)]
mod command_guard_tests {
    use super::*;

    #[test]
    fn checks_both_fields_and_codex_arrays_including_subagents() {
        for key in ["command", "cmd"] {
            for command in [json!("rm -rf ~"), json!(["bash", "-lc", "rm -rf ~"])] {
                for child in ["", "child-fixture"] {
                    let event = json!({"hook_event_name":"PreToolUse", "agent_id":child,
                        "tool_input":{key:command}, "permission_mode":"bypass"});
                    assert_eq!(command_refusal(&event, "/home/example").unwrap().rule, "protected-path");
                }
            }
        }
    }

    #[test]
    fn leaves_other_events_and_safe_commands_unchanged() {
        for event in [
            json!({"hook_event_name":"PostToolUse", "tool_input":{"command":"rm -rf ~"}}),
            json!({"hook_event_name":"PreToolUse", "tool_input":{"cmd":"rm -rf out/tmp"}}),
            json!({"hook_event_name":"PreToolUse", "tool_input":{"file_path":"/home/example"}}),
        ] { assert!(command_refusal(&event, "/home/example").is_none()); }
    }
}

pub(super) fn hook() -> Result<i32> {
    let (Some(name), Some(token)) = (nonempty_env("HGS_SESSION"), nonempty_env("HGS_RUN_ID"))
    else {
        return Ok(0);
    };
    let event = stdin_json()?;
    let stdout = io::stdout();
    track(name, token, event, &mut stdout.lock())
}

/// Claude Code 2.1.312 refuses a hook output of two JSON objects: the agent
/// hint and the task commands go out as one SessionStart additionalContext.
fn start_context(hint: Option<String>, tasks: Option<Value>) -> Option<String> {
    let tasks = tasks.and_then(|context| context["hookSpecificOutput"]["additionalContext"].as_str().map(str::to_owned));
    let parts: Vec<String> = hint.into_iter().chain(tasks).collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn clear_rules_proof(record: &mut Value, event: &Value) -> bool {
    if string(event,"hook_event_name") != "SessionStart" || !string(event,"agent_id").is_empty() { return false; }
    record["iron_rules_start_supported"] = json!(false);
    for key in ["iron_rules_start_project","iron_rules_start_digest","iron_rules_start_run","iron_rules_start_conversation"] {
        record.as_object_mut().unwrap().remove(key);
    }
    true
}

fn startup_rules(record: &Value, event: &Value) -> Option<(String,String)> {
    match iron_rules_start(record,event) {
        Ok(Some((project,text))) if text.encode_utf16().count() <= super::iron_rules::MAX_START_RULES => Some((project,text)),
        Ok(_) => None,
        Err(error) => { eprintln!("hgs: {error}; iron rules keep per-send delivery"); None }
    }
}

fn iron_rules_start(record: &Value, event: &Value) -> Result<Option<(String, String)>> {
    if string(record, "agent") != "claude"
        || string(event, "hook_event_name") != "SessionStart"
        || !string(event, "agent_id").is_empty()
    {
        return Ok(None);
    }
    let context = super::iron_rules::record_context(record)
        .map_err(|error| format!("Iron rules could not be read: {error}"))?;
    Ok(Some(context))
}

/// Records one hook event of this exact run. A main start publishes its
/// confirmed binding only after its combined context was delivered.
fn track(name: String, token: String, mut event: Value, output: &mut impl std::io::Write) -> Result<i32> {
    if nonempty_env("HGS_AGENT").as_deref() == Some("claude") {
        if let std::borrow::Cow::Owned(prompt) = journal::pasted_text(string(&event, "prompt")) {
            event["prompt"] = json!(prompt);
        }
    }
    let session = event
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|sid| !sid.is_empty() && !sid.chars().any(char::is_whitespace))
        .ok_or("hook did not supply a valid session_id")?.to_owned();
    let sid = session.as_str();
    let kind_value = string(&event, "hook_event_name").to_owned();
    let kind = kind_value.as_str();
    if let Some(refusal) = command_refusal(&event, &home().to_string_lossy()) {
        eprintln!("{} [{}]", refusal.message, refusal.rule);
        return Ok(2);
    }
    let _guard = lock(None)?;
    let panes = live()?;
    let own = read_run(&name, &token)?;
    let record = if continuations::foreign(own.as_ref(), sid) {
        continuations::owner(&event, sid, &panes)?
    } else {
        own
    };
    let Some(mut record) = record else {
        return Ok(0);
    };
    if !matches(&record, panes.get(string(&record, "name"))) {
        return Ok(0);
    }
    if let Some(scoped) = external_agents::route(&mut record, &event) {
        processes::observe(&mut record, &scoped);
        journal::update_activity(&mut record, &scoped);
        if kind == "PermissionRequest" {
            let child = &mut record["subagents"][string(&scoped, "agent_id")];
            child["display_state"] = json!("approval");
            // Each request has its own identity, so acknowledging one never hides the next.
            let tool = ["tool_use_id", "tool_call_id"]
                .iter()
                .map(|key| string(&scoped, key))
                .find(|id| !id.is_empty());
            child["attention_id"] =
                json!(tool.map_or_else(|| format!("request:{}", child["updated"]), str::to_owned));
        }
        journal::log_event(&record, &scoped)?;
        write(&mut record)?;
        return Ok(0);
    }
    if kind == "SessionStart" && string(&event, "agent_id").is_empty()
        && !external_agents::main_start(&record, &event) {
        return Ok(0);
    }
    // Only an accepted main start may invalidate this record's proof. A
    // foreign or child start must not alter its parent's startup delivery.
    // Clear before later work can fail, including rules resolution.
    if clear_rules_proof(&mut record, &event) { write(&mut record)?; }
    let agent = string(&record, "agent").to_owned();
    if !events(&agent).contains(&kind)
        && !versioned_events(&agent).iter().any(|(event, _)| *event == kind)
    {
        return Ok(0);
    }
    // U19: the owner confirms subagents before Claude starts them.
    if subagents_confirm::confirm() {
        if agent != "claude" && kind == "SessionStart" {
            record["subagent_confirm"] = json!(format!("subagent confirmation not supported by {agent}"));
        }
        if agent == "claude" {
            if matches!(kind, "UserPromptSubmit" | "Stop" | "SessionEnd") {
                subagents_confirm::group_reset(sid);
            }
            // U19: the ask becomes Claude's permission request, which the
            // approval channel brings to the owner, or to a member's lead.
            if let Some(output) = subagents_confirm::pre_tool(&mut record, &event, sid) {
                println!("{output}");
            }
            if kind == "PostToolUse" && subagents_confirm::subagent_tool(&event) {
                let tool = ["tool_use_id", "tool_call_id"].iter().map(|key| string(&event, key)).find(|id| !id.is_empty()).unwrap_or_default();
                subagents_confirm::group_remove(sid, tool);
            }
        }
    }
    if pause_active(&record) && ["UserPromptSubmit", "PreToolUse", "TurnStarted"].contains(&kind) {
        eprintln!("hgs is pausing this session; retry after resuming");
        return Ok(2);
    }
    let successor = codex_clear_recovery::observe(&record, &event);
    let main_start = (kind == "SessionStart" && string(&event, "agent_id").is_empty()) || successor.is_some();
    let changed_conversation = main_start && sid != string(&record, "conversation_id");
    if main_start {
        // A previous accepted restore/clear hook may have synced the identity but
        // failed archive cleanup. Finish that transaction before a later start
        // can replace its durable boundary; this never resends native input.
        if clear_context::cleanup_pending(&record) || (!string(&record,"restore_archive_id").is_empty() && string(&record,"expected_id").is_empty()) {
            archive::confirm_restore(&mut record)?;
        }
        if !string(&record, "fork_parent_id").is_empty() && sid == string(&record, "fork_parent_id")
        {
            record["error"] =
                json!("provider opened the source conversation instead of a new fork");
            record["activity"] = json!("unknown");
            record["phase"] = json!("unknown");
            write(&mut record)?;
            return Ok(0);
        }
        if sid == string(&record["fresh_from"], "conversation_id") && !sid.is_empty() {
            record["error"] =
                json!("provider opened the previous conversation instead of a fresh one");
            record["activity"] = json!("unknown");
            record["phase"] = json!("unknown");
            write(&mut record)?;
            return Ok(0);
        }
        if !string(&record, "expected_id").is_empty() && sid != string(&record, "expected_id")
            && !clear_context::resumed_start(&record, &event)
            && !clear_context::resumed_session(&event)
            && successor.is_none()
        {
            record["error"] = json!(codex_clear_recovery::MISMATCH);
            record["activity"] = json!("unknown");
            write(&mut record)?;
            return Ok(0);
        }
        if sid != string(&record, "conversation_id") {
            // The agent's own id wins: a resume or compaction may continue in a
            // new transcript, and the record follows it (H7).
            clear_context::follow(&mut record, &event);
            for key in [
                "clear_context_request",
                "compact_context_request",
                "prompt",
                "last_message",
                "subagents",
                "active_tools",
                "last_error",
                "subagent_groups",
                "subagent_groups_complete",
                "subagents_completed_pruned",
                "kimi_native_roster",
                "task_lists",
                "pending_task_updates",
                "shell_jobs",
                "pending_settings",
                "resume_settings",
                "effort_source_path",
                "effort_source_offset",
                "model_source_path",
                "model_source_offset",
            ] {
                if key != "clear_context_request" || !clear_context::cleanup_pending(&record) {
                    record.as_object_mut().unwrap().remove(key);
                }
            }
        }
        record.as_object_mut().unwrap().remove("expected_id");
        record.as_object_mut().unwrap().remove("session_end");
        if successor.is_some() { record.as_object_mut().unwrap().remove("error"); }
        record
            .as_object_mut()
            .unwrap()
            .remove("pending_task_updates");
        record["conversation_id"] = json!(sid);
        record["conversation_state"] = json!("active");
        record["activity"] = json!("idle");
        record["phase"] = json!("idle");
        record["main_done"] = json!(true);
        if !string(&event, "cwd").is_empty() {
            record["cwd"] = event["cwd"].clone();
        }
        record["transcript"] = successor.as_ref().map_or_else(|| event["transcript_path"].clone(),
            |proof| json!(proof.transcript));
    } else if sid != string(&record, "conversation_id") {
        return Ok(0);
    }
    let resumed_clear_end = kind == "SessionEnd" && string(&event, "agent_id").is_empty()
        && clear_context::ending_resume(&record);
    if kind == "SessionEnd" && string(&event, "agent_id").is_empty() {
        record["session_end"] = json!({"reason": string(&event, "reason"), "at": now(),
            "run_id": record["run_id"], "conversation_id": sid});
    }
    kimi_messages::bind_stop_turn(&record, &mut event);
    // Claude's suggested next message is no subagent and no Activity event.
    if prompt_suggestion::observe(&mut record, &event) {
        write(&mut record)?;
        return Ok(0);
    }
    super::hook_health::observe(&mut record, &event);
    telemetry::observe(&mut record, &event);
    tasks::observe(&mut record, &event);
    processes::observe(&mut record, &event);
    journal::update_activity(&mut record, &event);
    if resumed_clear_end {
        // The real main hook ended this requested clear, not its supervised
        // process. Keep the exact deferred-start fence, never fabricate Ready.
        record["session_end"]["clear_request_id"] = record["clear_context_request"]["request_id"].clone();
        record["session_end"]["clear_event_at"] = record["last_event_at"].clone();
    }
    compact_context::observe(&mut record, &event);
    questions::observe(&mut record, &event);
    let clear_boundary = changed_conversation && record["session_clear"]["journaled"] != true;
    if clear_boundary && successor.is_some() {
        if let Some(mut clear) = clear_context::event(&record) {
            clear["hook_event_name"] = json!("SessionCleared");
            journal::log_event(&record, &clear)?;
        }
    }
    journal::log_event(&record, &event)?;
    if clear_boundary && successor.is_none() {
        if let Some(mut clear) = clear_context::event(&record) {
            clear["hook_event_name"] = json!("SessionCleared");
            journal::log_event(&record, &clear)?;
        }
    }
    // A resume or compaction that opened a new transcript is one event in the
    // Activity timeline (H7).
    if changed_conversation && record["session_rebound"]["journaled"] != true {
        if let Some(mut rebound) = clear_context::rebound_event(&record) {
            rebound["hook_event_name"] = json!("SessionRebound");
            journal::log_event(&record, &rebound)?;
            record["session_rebound"]["journaled"] = json!(true);
        }
    }
    let hint = agent_hint::start(&mut record, &event);
    let own_start = kind == "SessionStart" && string(&event, "agent_id").is_empty();
    let rules = startup_rules(&record, &event);
    let parts: Vec<_> = hint.map(str::to_owned)
        .into_iter()
        .chain(rules.as_ref().map(|(_, text)| text.clone()))
        .filter(|text| !text.is_empty())
        .collect();
    let hint = (!parts.is_empty()).then(|| parts.join("\n\n"));
    let tasks = own_start
        .then(|| nonempty_env("HGS_AGENT").and_then(|agent| crate::tasks::session_context(&agent)))
        .flatten();
    if let Some(context) = start_context(hint, tasks) {
        // A failed write or flush leaves only the early invalidation persisted.
        // Never expose a fresh conversation as started before this succeeds.
        agent_hint::output(output, &context)?;
        if let Some((project, text)) = rules.filter(|_| context.encode_utf16().count() <= super::iron_rules::MAX_START_CONTEXT) {
            record["iron_rules_start_supported"] = json!(true);
            record["iron_rules_start_project"] = json!(project);
            record["iron_rules_start_digest"] = json!(teams::store::digest(&json!(text)));
            record["iron_rules_start_run"] = record["run_id"].clone();
            record["iron_rules_start_conversation"] = record["conversation_id"].clone();
        }
    }
    if changed_conversation && clear_context::cleanup_pending(&record) {
        archive::reserve_clear_source(&record)?;
    }
    // The ready conversation and its proof become visible in the same atomic
    // record update; hook() performs no later proof-only binding write.
    write(&mut record)?;
    if own_start {
        archive::confirm_restore(&mut record)?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::windows::{self as forms, CLAUDE_EXEC_FORM};
    use super::*;

    struct StartupBackend;
    impl crate::state::backend::TerminalBackend for StartupBackend {
        fn panes(&self) -> Result<Vec<crate::state::backend::PaneRow>> {
            Ok(vec![crate::state::backend::PaneRow {
                session: "claude/hooks/parent".into(), session_id: "$1".into(),
                pane: "%1".into(), dead: "0".into(), run_marker: "parent-run".into(),
            }])
        }
    }

    struct StartupOutput {
        path: PathBuf,
        bytes: Vec<u8>,
        visible: Vec<Value>,
        fail_write: bool,
        fail_flush: bool,
    }

    impl std::io::Write for StartupOutput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.visible.push(serde_json::from_slice(&fs::read(&self.path)?).unwrap());
            if self.fail_write { return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "fixture write")); }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.visible.push(serde_json::from_slice(&fs::read(&self.path)?).unwrap());
            if self.fail_flush { return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "fixture flush")); }
            Ok(())
        }
    }

    fn pending_rules_start() -> Value {
        json!({"version":1,"name":"claude/hooks/parent","run_id":"parent-run","pane":"%1",
            "agent":"claude","pid":0,"conversation_id":"","activity":"unknown","phase":"unknown",
            "iron_rules":["Keep this session rule"],"iron_rules_start_supported":true,
            "iron_rules_start_project":"old-project","iron_rules_start_digest":"old-digest",
            "iron_rules_start_run":"old-run","iron_rules_start_conversation":"old-conversation"})
    }

    #[test]
    fn startup_proof_is_published_in_the_first_confirmed_binding() {
        let directory = tempfile::tempdir().unwrap();
        super::super::with_test_state_root(directory.path(), || {
            let mut record = pending_rules_start();
            write(&mut record).unwrap();
            let mut output = StartupOutput { path: record_path("claude/hooks/parent"), bytes: Vec::new(),
                visible: Vec::new(), fail_write: false, fail_flush: false };
            let event = json!({"hook_event_name":"SessionStart","source":"startup","session_id":"new-conversation"});
            crate::state::backend::with_fake(&StartupBackend, || {
                assert_eq!(track("claude/hooks/parent".into(), "parent-run".into(), event, &mut output).unwrap(), 0);
            });
            assert!(!output.visible.is_empty(), "the real hook emitted its context");
            for visible in &output.visible {
                assert_eq!(string(visible, "conversation_id"), "", "start became visible before output finished: {visible}");
                assert_eq!(visible["iron_rules_start_supported"], false);
            }
            let published: Value = serde_json::from_slice(&fs::read(&output.path).unwrap()).unwrap();
            assert_eq!(published["conversation_id"], "new-conversation");
            assert_eq!(published["activity"], "idle");
            assert_eq!(published["iron_rules_start_supported"], true);
            assert_eq!(published["iron_rules_start_run"], published["run_id"]);
            assert_eq!(published["iron_rules_start_conversation"], published["conversation_id"]);
            let expected = teams::store::digest(&json!(startup_rules(&published,
                &json!({"hook_event_name":"SessionStart"})).unwrap().1));
            assert_eq!(published["iron_rules_start_digest"], expected);
            let sent: Value = serde_json::from_slice(&output.bytes).unwrap();
            assert!(sent["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("Keep this session rule"));
        });
    }

    #[test]
    fn broken_startup_output_never_publishes_a_proof_or_confirmed_binding() {
        for fail_flush in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            super::super::with_test_state_root(directory.path(), || {
                let mut record = pending_rules_start();
                write(&mut record).unwrap();
                let mut output = StartupOutput { path: record_path("claude/hooks/parent"), bytes: Vec::new(),
                    visible: Vec::new(), fail_write: !fail_flush, fail_flush };
                crate::state::backend::with_fake(&StartupBackend, || {
                    let event = json!({"hook_event_name":"SessionStart","session_id":"new-conversation"});
                    assert!(track("claude/hooks/parent".into(), "parent-run".into(), event, &mut output).is_err());
                });
                let saved: Value = serde_json::from_slice(&fs::read(&output.path).unwrap()).unwrap();
                assert_eq!(saved["conversation_id"], "");
                assert_eq!(saved["activity"], "unknown");
                assert_eq!(saved["iron_rules_start_supported"], false);
                for key in ["iron_rules_start_project", "iron_rules_start_digest", "iron_rules_start_run", "iron_rules_start_conversation"] {
                    assert!(saved.get(key).is_none(), "old proof survived failed output: {key}");
                }
            });
        }
    }

    #[test]
    fn a_foreign_session_start_leaves_the_record_and_rules_proof_byte_for_byte_unchanged() {
        struct Backend;
        impl crate::state::backend::TerminalBackend for Backend {
            fn panes(&self) -> Result<Vec<crate::state::backend::PaneRow>> {
                Ok(vec![crate::state::backend::PaneRow {
                    session: "claude/hooks/parent".into(),
                    session_id: "$1".into(),
                    pane: "%1".into(),
                    dead: "0".into(),
                    run_marker: "parent-run".into(),
                }])
            }
        }
        for has_proof in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            super::super::with_test_state_root(directory.path(), || {
                let name = "claude/hooks/parent";
                let mut record = json!({"version":1,"name":name,"run_id":"parent-run","pane":"%1",
                    "agent":"claude","pid":0,"conversation_id":"11111111-1111-4111-8111-111111111111",
                    "model":"parent-model","iron_rules":["Keep the session rule"]});
                if has_proof {
                    record["iron_rules_start_supported"] = json!(true);
                    record["iron_rules_start_project"] = json!("project");
                    record["iron_rules_start_digest"] = json!("digest");
                    record["iron_rules_start_run"] = json!("parent-run");
                    record["iron_rules_start_conversation"] = record["conversation_id"].clone();
                }
                write(&mut record).unwrap();
                let path = record_path(name);
                let before = fs::read(&path).unwrap();
                let mut output = Vec::new();
                crate::state::backend::with_fake(&Backend, || {
                    let event = json!({"hook_event_name":"SessionStart",
                        "session_id":"22222222-2222-4222-8222-222222222222","model":"foreign-model"});
                    assert_eq!(track(name.into(), "parent-run".into(), event,
                        &mut output).unwrap(), 0);
                });
                assert_eq!(fs::read(path).unwrap(), before, "foreign start changed the record (proof={has_proof})");
                assert!(output.is_empty(), "foreign starts emit no main-session context");
            });
        }
    }

    #[test]
    fn failed_or_oversized_start_rules_leave_hint_tasks_and_no_old_proof() {
        let dir=tempfile::tempdir().unwrap();
        super::super::with_test_state_root(dir.path(),|| {
            let mut record=json!({"agent":"claude","name":"claude/x/one","run_id":"r1","conversation_id":"c1",
                "iron_rules_start_supported":true,"iron_rules_start_digest":"old","iron_rules_start_run":"old-run","iron_rules":["Own"]});
            let event=json!({"hook_event_name":"SessionStart","source":"compact"});
            assert!(clear_rules_proof(&mut record,&event));
            // A malformed authoritative list forces a real resolution error.
            record["iron_rules"]=json!([5]);
            assert!(startup_rules(&record,&event).is_none());
            assert_eq!(record["iron_rules_start_supported"],false); assert!(record.get("iron_rules_start_run").is_none());
            let context=start_context(Some("Agent hint".into()),crate::tasks::session_context("claude")).unwrap();
            assert!(context.contains("Agent hint") && context.contains("Zerus tasks"));
            record["iron_rules"]=json!((0..20).map(|_|"\u{1f600}".repeat(150)).collect::<Vec<_>>());
            assert!(startup_rules(&record,&event).is_none(),"UTF-16 expansion must keep the fallback");
            record["iron_rules"]=json!(["Small"]);
            assert!(startup_rules(&record,&event).is_some());
        });
    }

    #[test]
    fn iron_rules_follow_claude_start_resume_clear_and_compact_but_not_messages() {
        let dir = tempfile::tempdir().unwrap();
        super::super::with_test_state_root(dir.path(), || {
            let record = json!({"agent":"claude","name":"claude/example/one","iron_rules":["Session requirement"]});
            for source in ["startup", "resume", "clear", "compact"] {
                let (_, text) = iron_rules_start(
                    &record,
                    &json!({"hook_event_name":"SessionStart","source":source}),
                )
                .unwrap()
                .unwrap();
                assert_eq!(text.matches("Session requirement").count(), 1);
                let mut out = Vec::new();
                agent_hint::output(&mut out, &start_context(Some(text), None).unwrap()).unwrap();
                let value: Value = serde_json::from_slice(&out).unwrap();
                assert_eq!(value["hookSpecificOutput"]["hookEventName"], "SessionStart");
            }
            for event in [
                json!({"hook_event_name":"UserPromptSubmit"}),
                json!({"hook_event_name":"PreToolUse"}),
                json!({"hook_event_name":"SessionStart","agent_id":"child"}),
            ] {
                assert!(iron_rules_start(&record, &event).unwrap().is_none());
            }
            for agent in ["codex", "kimi", "dsh", "opencode"] {
                let mut record = record.clone();
                record["agent"] = json!(agent);
                assert!(
                    iron_rules_start(&record, &json!({"hook_event_name":"SessionStart"}))
                        .unwrap()
                        .is_none()
                );
            }
        });
    }

    #[test]
    fn session_start_writes_one_json_object_with_the_hint_and_the_task_commands() {
        let tasks = crate::tasks::session_context("claude");
        let context = start_context(Some("Hint for this agent.".into()), tasks).unwrap();
        let mut out = Vec::new();
        agent_hint::output(&mut out, &context).unwrap();
        let text = String::from_utf8(out).unwrap();
        // One object, parseable as a whole, as Claude Code 2.1.312 requires.
        let value: Value = serde_json::from_str(text.trim()).unwrap();
        let joined = value["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(joined.starts_with("Hint for this agent.") && joined.contains("Zerus tasks are this project's shared task list"));
        assert_eq!(start_context(None, None), None);
        assert_eq!(start_context(Some("Only a hint.".into()), None).as_deref(), Some("Only a hint."));
    }

    #[test]
    fn health_registration_preserves_personal_and_legacy_hooks_and_never_grants_trust() {
        let personal=json!({"type":"command","command":"personal-hook --secret example"});
        let legacy=json!({"type":"command","command":"python3 /opt/old/hgs_state.py hook","timeout":5});
        let original=json!({"apiKey":"never disclose", "hooks":{"Stop":[{"hooks":[personal.clone(),legacy.clone()]}]}}).to_string();
        let health=health_document(&original,"claude",None).unwrap();
        assert_eq!(health["state"],"partial");assert_eq!(health["compatibility"]["state"],"unknown");
        assert!(!health.to_string().contains("secret"));assert!(!health.to_string().contains("apiKey"));
        let merged=merge_json(&original,Path::new("/config"),"claude",false,&mut ||panic!("must retain legacy command"),&mut ||Some((2,1,296))).unwrap().unwrap();
        let doc:Value=serde_json::from_str(&merged).unwrap();
        assert_eq!(doc["hooks"]["Stop"][0]["hooks"],json!([personal,legacy]));
        assert_eq!(health_document(&merged,"claude",Some((2,1,296))).unwrap()["state"],"configured");
        assert!(merge_json(&merged,Path::new("/config"),"claude",false,&mut ||panic!(),&mut ||Some((2,1,296))).unwrap().is_none());
    }
    #[test]
    fn tracking_repair_does_not_enable_or_widen_answer_channels() {
        let empty=merge_json_mode("{}",Path::new("/config"),"claude",false,&mut ||Ok(Entry::shell("hgs __state hook".into())),&mut ||Some((2,1,296)),false).unwrap().unwrap();
        assert!(!health_document(&empty,"claude",Some((2,1,296))).unwrap()["answer_channel_registered"].as_bool().unwrap());
        let original=json!({"hooks":{"PermissionRequest":[{"matcher":"AskUserQuestion","hooks":[{"type":"command","command":"hgs __state answer-channel"}]}]}});
        let merged=merge_json_mode(&original.to_string(),Path::new("/config"),"claude",false,&mut ||Ok(Entry::shell("hgs __state hook".into())),&mut ||Some((2,1,296)),false).unwrap().unwrap();
        let value:Value=serde_json::from_str(&merged).unwrap();assert_eq!(value["hooks"]["PermissionRequest"][0],original["hooks"]["PermissionRequest"][0]);
        // The explicit tracking-only repair must not opt Windows into the
        // newly supported channel either.
        let exec=forms::claude_entry(STABLE,CLAUDE_EXEC_FORM,false).unwrap();
        let repaired=merge_json_mode("{}",Path::new("/config"),"claude",true,&mut ||Ok(exec.clone()),&mut ||Some(CLAUDE_ANSWER_CHANNEL),false).unwrap().unwrap();
        assert!(!health_document_mode(&repaired,"claude",Some(CLAUDE_ANSWER_CHANNEL),true).unwrap()["answer_channel_registered"].as_bool().unwrap());
    }

    #[test]
    fn health_accepts_the_windows_exec_channel_but_not_a_shell_channel() {
        let exec = forms::claude_entry(STABLE, CLAUDE_EXEC_FORM, false).unwrap();
        let (registered, _) = versioned_merge("", "claude", true, exec, Some(CLAUDE_ANSWER_CHANNEL));
        let registered = registered.unwrap();
        let health = health_document_mode(&registered, "claude", Some(CLAUDE_ANSWER_CHANNEL), true).unwrap();
        assert_eq!(health["state"], "configured");
        assert_eq!(health["answer_channel_registered"], true);
        assert_eq!(health["compatibility"]["state"], "compatible");
        assert_eq!(health_document_mode(&registered, "claude", None, true).unwrap()["compatibility"]["state"], "unknown");
        assert_eq!(health_document_mode(&registered, "claude", Some((2, 1, 295)), true).unwrap()["compatibility"]["state"], "incompatible");
        let shell = json!({"hooks":{"PermissionRequest":[{"hooks":[
            {"type":"command","command":"hgs __state answer-channel","timeout":86400}
        ]}]}}).to_string();
        assert_eq!(health_document_mode(&shell, "claude", Some(CLAUDE_ANSWER_CHANNEL), true).unwrap()["compatibility"]["state"], "incompatible");
        assert_eq!(health_document_mode(&shell, "claude", Some(CLAUDE_ANSWER_CHANNEL), false).unwrap()["compatibility"]["state"], "compatible");
    }

    #[test]
    fn health_exposes_duplicates_and_partial_kimi_repair_preserves_every_entry() {
        let text=json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"hgs __state hook"},{"type":"command","command":"hgs __state hook"}]}]}}).to_string();
        assert_eq!(health_document(&text,"claude",None).unwrap()["duplicate_events"],json!(["Stop"]));
        let kimi="# BEGIN hgs session tracking\n[[hooks]]\nevent = \"Stop\"\ncommand = \"hgs __state hook\"\ntimeout = 7\n[[hooks]]\nevent = \"Stop\"\ncommand = \"personal-hook\"\n# END hgs session tracking\n";
        assert!(repair_document(kimi,Path::new("/config"),"kimi",(2,1,1)).is_err());
    }

    #[test]
    fn health_reports_version_drift_and_rejects_malformed_owned_handlers() {
        let future=json!({"hooks":{"PostModelSwitch":[{"hooks":[{"type":"command","command":"hgs __state hook"}]}]}}).to_string();
        assert_eq!(health_document(&future,"claude",Some((2,1,200))).unwrap()["compatibility"]["state"],"incompatible");
        assert_eq!(health_document(&future,"claude",None).unwrap()["compatibility"]["state"],"unknown");
        for text in ["[]","{",r#"{"hooks":{"Stop":"invalid"}}"#,r#"{"hooks":{"Stop":[{"hooks":[{"type":"prompt","command":"hgs __state hook"}]}]}}"#] {
            assert!(health_document(text,"claude",Some((2,1,296))).is_err(),"{text}");
        }
    }

    const UNIX: &str = "'/opt/hgs dir/hgs' __state hook";
    const STABLE: &str = r"C:\Profiles\Иван Петров\.local\bin\hgs.exe";
    const SHELL: &str = r#""C:/Profiles/Иван Петров/.local/bin/hgs.exe" __state hook"#;
    const POWERSHELL: &str =
        r#"& "C:/Profiles/Иван Петров/.local/bin/hgs.exe" __state hook; exit $LASTEXITCODE"#;

    fn path() -> PathBuf {
        PathBuf::from("/config/file")
    }

    /// The JSON merge before spec D12, for the Unix byte-compatibility check.
    fn reference_json(original: &str, agent: &str, fresh: &str) -> Result<Option<String>> {
        let mut command = fresh.to_owned();
        let mut document: Value = if original.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(original).map_err(|e| e.to_string())?
        };
        let hooks = document
            .as_object_mut()
            .ok_or("hook config must be an object")?
            .entry("hooks")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("hooks must be an object")?;
        if let Some(existing) = hooks
            .values()
            .filter_map(Value::as_array)
            .flatten()
            .filter_map(|group| group.get("hooks").and_then(Value::as_array))
            .flatten()
            .filter_map(|entry| entry.get("command").and_then(Value::as_str))
            .find(|value| owns_hook(value))
        {
            command = existing.to_owned();
        }
        let mut changed = false;
        for event in events(agent) {
            let entries = hooks
                .entry(event)
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .ok_or("hook event must be an array")?;
            let installed = entries
                .iter()
                .filter_map(|group| group.get("hooks").and_then(Value::as_array))
                .flatten()
                .any(|entry| owns_hook(string(entry, "command")));
            if !installed {
                entries.push(
                    json!({"hooks": [{"type": "command", "command": command, "timeout": 5}]}),
                );
                changed = true;
            }
        }
        Ok(changed.then(|| format!("{}\n", serde_json::to_string_pretty(&document).unwrap())))
    }

    /// The Kimi merge before spec D12.
    fn reference_kimi(original: &str, fresh: &str) -> Result<Option<String>> {
        let mut command = fresh.to_owned();
        let begin = "# BEGIN hgs session tracking\n";
        let end = "# END hgs session tracking\n";
        let mut content_without = original.to_owned();
        if let Some(start) = original.find(begin) {
            let rest = &original[start + begin.len()..];
            let finish = rest.find(end).ok_or("incomplete hgs hook block")?;
            let document = rest[..finish]
                .parse::<toml_edit::DocumentMut>()
                .map_err(|e| e.to_string())?;
            if let Some(tables) = document
                .get("hooks")
                .and_then(toml_edit::Item::as_array_of_tables)
            {
                if let Some(existing) = tables
                    .iter()
                    .filter_map(|table| table.get("command").and_then(toml_edit::Item::as_str))
                    .find(|value| owns_hook(value))
                {
                    command = existing.to_owned();
                }
                if events("kimi").iter().all(|event| {
                    tables.iter().any(|table| {
                        table.get("event").and_then(toml_edit::Item::as_str) == Some(event)
                            && table
                                .get("command")
                                .and_then(toml_edit::Item::as_str)
                                .is_some_and(owns_hook)
                    })
                }) {
                    original
                        .parse::<toml_edit::DocumentMut>()
                        .map_err(|e| e.to_string())?;
                    return Ok(None);
                }
            }
            content_without = format!("{}{}", &original[..start], &rest[finish + end.len()..]);
        }
        let mut block = begin.to_owned();
        for event in events("kimi") {
            block.push_str(&format!(
                "[[hooks]]\nevent = {}\ncommand = {}\ntimeout = 5\n",
                json!(event),
                json!(command)
            ));
        }
        block.push_str(end);
        let content = format!("{}\n\n{block}", content_without.trim_end_matches('\n'));
        content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| e.to_string())?;
        Ok(Some(content))
    }

    /// A merge for an agent whose version cannot be read.
    fn json_merge(original: &str, agent: &str, windows: bool, fresh: Entry) -> Option<String> {
        let mut asked = 0;
        let merged = merge_json(
            original,
            &path(),
            agent,
            windows,
            &mut || {
                asked += 1;
                Ok(fresh.clone())
            },
            &mut || None,
        )
        .unwrap();
        assert!(asked <= 1);
        merged
    }

    /// A merge that must reuse the hgs entry already there.
    fn json_reuse(original: &str, agent: &str, windows: bool) -> Option<String> {
        merge_json(
            original,
            &path(),
            agent,
            windows,
            &mut || Err("a fresh entry was asked for".into()),
            &mut || None,
        )
        .unwrap()
    }

    fn kimi_merge(original: &str, crlf: bool, fresh: &str) -> Option<String> {
        merge_kimi(original, &path(), crlf, &mut || Ok(fresh.to_owned())).unwrap()
    }

    fn hook_entries(text: &str, event: &str) -> Vec<Value> {
        let document: Value = serde_json::from_str(text).unwrap();
        document["hooks"][event]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|group| group["hooks"].as_array().unwrap().clone())
            .collect()
    }

    #[test]
    fn unix_json_hooks_keep_their_bytes() {
        let user = json!({"command": "notify-send done", "type": "command"});
        let legacy = format!("python3 {} hook", "/old/hgs_state.py");
        let cases = [
            String::new(),
            "  \n".into(),
            json!({"model": "opus", "hooks": {"Stop": [{"hooks": [user]}]}}).to_string(),
            json!({"hooks": {"Stop": [{"matcher": "", "hooks": [{"type": "command",
                "command": "'/usr/bin/hgs' __state hook"}]}]}})
            .to_string(),
            json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": legacy}]}]}})
                .to_string(),
            // An exec-form entry is not an hgs hook on Unix, as before.
            json!({"hooks": {"Stop": [{"hooks": [{"type": "command",
                "command": "/usr/bin/hgs", "args": ["__state", "hook"]}]}]}})
            .to_string(),
        ];
        for agent in ["claude", "codex"] {
            for original in &cases {
                let expected = reference_json(original, agent, UNIX).unwrap();
                assert_eq!(
                    json_merge(original, agent, false, Entry::shell(UNIX.into())),
                    expected,
                    "{agent}: {original}"
                );
                // Merging the result again changes nothing.
                let merged = expected.unwrap();
                assert_eq!(reference_json(&merged, agent, UNIX).unwrap(), None);
                assert_eq!(json_reuse(&merged, agent, false), None);
            }
        }
        // Errors keep their words.
        for (original, error) in [
            ("[]", "hook config must be an object"),
            (r#"{"hooks": []}"#, "hooks must be an object"),
            (r#"{"hooks": {"Stop": {}}}"#, "hook event must be an array"),
        ] {
            let result = merge_json(
                original,
                &path(),
                "claude",
                false,
                &mut || Ok(Entry::shell(UNIX.into())),
                &mut || None,
            );
            assert_eq!(result.unwrap_err(), error);
        }
        assert!(merge_json(
            "{",
            &path(),
            "claude",
            false,
            &mut || Ok(Entry::shell(UNIX.into())),
            &mut || None
        )
        .unwrap_err()
        .starts_with("cannot read /config/file: "));
    }

    #[test]
    fn unix_kimi_hooks_keep_their_bytes() {
        let complete = reference_kimi("default_model = \"probe\"\n", UNIX)
            .unwrap()
            .unwrap();
        let partial = complete.replace("event = \"SessionHeartbeat\"", "event = \"Other\"");
        let crlf = complete.replace('\n', "\r\n");
        let cases = [
            String::new(),
            "default_model = \"probe\"\n\n\n".into(),
            complete.clone(),
            partial,
            format!("{complete}\n[loop_control]\nmax_attempts_per_step = 1\n"),
        ];
        // Even without Unix marker recognition, a complete CRLF registration
        // must not receive another set of tracking callbacks.
        assert_eq!(kimi_merge(&crlf, false, UNIX), None);
        for original in &cases {
            assert_eq!(
                kimi_merge(original, false, UNIX),
                reference_kimi(original, UNIX).unwrap(),
                "{original:?}"
            );
        }
        let error = merge_kimi(
            "# BEGIN hgs session tracking\n[[hooks]]\n",
            &path(),
            false,
            &mut || Ok(UNIX.into()),
        )
        .unwrap_err();
        assert_eq!(error, "incomplete hgs hook block in /config/file");
    }

    fn legacy_kimi_document() -> String {
        let mut text = "# Personal settings\ndefault_model = \"synthetic\"\n".to_owned();
        for event in events("kimi") {
            text.push_str(&format!("\n[[hooks]]\nevent = {}\ncommand = \"python3 /synthetic/hgs_state.py hook\"\ntimeout = 5\n", json!(event)));
        }
        text.push_str("\n# Personal memory hook\n[[hooks]]\nevent = \"UserPromptSubmit\"\ncommand = \"personal-memory-hook\"\ntimeout = 10\n");
        text
    }

    #[test]
    fn complete_legacy_kimi_hooks_do_not_receive_another_managed_block() {
        let original = legacy_kimi_document();
        assert_eq!(merge_kimi(&original, &path(), false, &mut || panic!("existing hooks are complete")).unwrap(), None);
        let conditional = original.replace("event = \"TurnStarted\"", "event = \"TurnStarted\"\nmatcher = \"restricted-origin\"");
        assert!(kimi_merge(&conditional, false, UNIX).is_some());
    }

    #[test]
    fn redundant_kimi_block_is_removed_without_rewriting_legacy_or_personal_hooks() {
        let original = legacy_kimi_document();
        let block = reference_kimi("", UNIX).unwrap().unwrap();
        let duplicate = format!("{original}{block}");
        let merged = kimi_merge(&duplicate, false, UNIX).unwrap();
        assert_eq!(merged, format!("{original}\n\n"));
        let health = health_document(&merged, "kimi", Some((2, 3, 1))).unwrap();
        assert_eq!(health["duplicate_events"], json!([]));
        assert!(merged.contains("# Personal memory hook"));
        assert_eq!(kimi_merge(&merged, false, UNIX), None);
        // A personal entry or different callback parameters forbid block removal.
        for protected in [block.replace("timeout = 5", "timeout = 6"),
            block.replace(KIMI_END, &format!("[[hooks]]\nevent = \"Stop\"\ncommand = \"personal-hook\"\n{KIMI_END}"))] {
            assert_eq!(kimi_merge(&format!("{original}{protected}"), false, UNIX), None);
        }
    }

    #[test]
    fn windows_hook_commands_name_the_stable_cli_with_forward_slashes() {
        assert_eq!(forms::shell_command(STABLE).unwrap(), SHELL);
        assert_eq!(
            forms::shell_command(r"C:\Profiles\a & b (1)\^x;y'z\hgs.exe").unwrap(),
            r#""C:/Profiles/a & b (1)/^x;y'z/hgs.exe" __state hook"#
        );
        assert_eq!(
            forms::shell_command(r"\\server\share\me\.local\bin\hgs.exe").unwrap(),
            r#""//server/share/me/.local/bin/hgs.exe" __state hook"#
        );
        assert_eq!(
            forms::path(STABLE).unwrap(),
            "C:/Profiles/Иван Петров/.local/bin/hgs.exe"
        );
        // A shell form names hgs the way the other hgs hooks do.
        assert!(owns_hook(SHELL));
        for refused in [
            r"hgs.exe",
            r"\Users\me\hgs.exe",
            r"\\?\C:\Users\me\hgs.exe",
            r"\\.\C:\Users\me\hgs.exe",
            "C:\\Users\\me\n\\hgs.exe",
        ] {
            assert!(forms::path(refused).is_err(), "{refused:?}");
            assert!(forms::shell_command(refused).is_err(), "{refused:?}");
        }
        for special in ['%', '$', '`', '!', '"'] {
            let error =
                forms::shell_command(&format!(r"C:\Users\a{special}b\hgs.exe")).unwrap_err();
            assert!(error.contains(&format!("{special:?}")), "{error}");
            assert!(error.contains("HGS_TRACKING=0"), "{error}");
        }
        // The exec form has no shell to misread them.
        assert_eq!(
            forms::claude_entry(r"C:\Profiles\a%b$c\hgs.exe", CLAUDE_EXEC_FORM, false)
                .unwrap()
                .command,
            "C:/Profiles/a%b$c/hgs.exe"
        );
    }

    #[test]
    fn claude_uses_its_exec_form_when_its_version_has_it() {
        let exec = Entry {
            command: forms::path(STABLE).unwrap(),
            args: Some(vec!["__state".into(), "hook".into()]),
            command_windows: None,
        };
        for version in [CLAUDE_EXEC_FORM, (2, 1, 300), (2, 2, 0), (3, 0, 0)] {
            for git_bash in [false, true] {
                assert_eq!(
                    forms::claude_entry(STABLE, version, git_bash).unwrap(),
                    exec
                );
            }
        }
        // Older versions run the shell form with Git Bash; PowerShell, their
        // fallback without it, cannot run it, so nothing is written then.
        assert_eq!(
            forms::claude_entry(STABLE, (2, 1, 295), true).unwrap(),
            Entry::shell(SHELL.into())
        );
        let error = forms::claude_entry(STABLE, (2, 0, 9), false).unwrap_err();
        assert!(error.contains("Claude Code 2.0.9"), "{error}");
        assert!(error.contains("Git for Windows"), "{error}");
        assert!(error.contains("2.1.296"), "{error}");
        assert!(error.contains("HGS_TRACKING=0"), "{error}");

        let codex = forms::codex_entry(STABLE).unwrap();
        assert_eq!(
            codex,
            Entry {
                command: SHELL.into(),
                args: None,
                command_windows: Some(POWERSHELL.into()),
            }
        );
    }

    #[test]
    fn codex_gets_a_powershell_form_on_windows() {
        assert_eq!(forms::powershell_command(STABLE).unwrap(), POWERSHELL);
        assert_eq!(
            forms::powershell_command(r"C:\Profiles\a & b (1)\^x;y'z%\hgs.exe").unwrap_err(),
            forms::shell_command(r"C:\Profiles\a & b (1)\^x;y'z%\hgs.exe").unwrap_err()
        );
        assert_eq!(
            forms::powershell_command(r"C:\Profiles\a & b (1)\^x;y'z\hgs.exe").unwrap(),
            r#"& "C:/Profiles/a & b (1)/^x;y'z/hgs.exe" __state hook; exit $LASTEXITCODE"#
        );
        // PowerShell also ends a double-quoted string at these.
        for quote in ['\u{201C}', '\u{201D}', '\u{201E}'] {
            let stable = format!(r"C:\Users\a{quote}b\hgs.exe");
            assert!(forms::shell_command(&stable).is_ok());
            let error = forms::powershell_command(&stable).unwrap_err();
            assert!(error.contains(&format!("{quote:?}")), "{error}");
            assert!(error.contains("HGS_TRACKING=0"), "{error}");
            assert!(forms::codex_entry(&stable).is_err());
        }
        // The shell form, Codex's `command`, is what names the entry as hgs's.
        let hook = json!({"type": "command", "command": SHELL, "commandWindows": POWERSHELL});
        assert_eq!(
            Entry::owned(&hook, true),
            Some(forms::codex_entry(STABLE).unwrap())
        );
    }

    #[test]
    fn versions_are_read_from_version_answers() {
        assert_eq!(
            version::version_in("2.1.296 (Claude Code)\n"),
            Some((2, 1, 296))
        );
        assert_eq!(version::version_in("codex-cli 0.162.0"), Some((0, 162, 0)));
        assert_eq!(version::version_in("kimi v2.1.1-beta.3"), Some((2, 1, 1)));
        assert_eq!(
            version::version_in("tool 1.2.3+build.7 extra 4.5.6"),
            Some((1, 2, 3))
        );
        for text in ["", "Claude Code", "2.1", "1.2.3.4", "x1.2.3", "2.1.x"] {
            assert_eq!(version::version_in(text), None, "{text:?}");
        }
        assert!((2, 1, 296) > (2, 1, 99));
    }

    #[test]
    fn windows_json_hooks_repeat_the_entry_already_there() {
        let exec = forms::claude_entry(STABLE, CLAUDE_EXEC_FORM, false).unwrap();
        let settings = json_merge(r#"{"model": "opus"}"#, "claude", true, exec.clone()).unwrap();
        for event in events("claude") {
            assert_eq!(
                hook_entries(&settings, event),
                [json!({"type": "command", "command": exec.command,
                    "args": ["__state", "hook"], "timeout": 5})],
                "{event}"
            );
        }
        // The bytes Claude Code reads: its exec form, no shell.
        let stop = "    \"Stop\": [\n      {\n        \"hooks\": [\n          {\n            \
            \"args\": [\n              \"__state\",\n              \"hook\"\n            ],\n            \
            \"command\": \"C:/Profiles/Иван Петров/.local/bin/hgs.exe\",\n            \
            \"timeout\": 5,\n            \"type\": \"command\"\n          }\n        ]\n      }\n    ],\n";
        assert!(settings.contains(stop), "{settings}");
        assert_eq!(json_reuse(&settings, "claude", true), None);
        // A missing event gets the same exec form, never a new decision.
        let mut document: Value = serde_json::from_str(&settings).unwrap();
        document["hooks"].as_object_mut().unwrap().remove("Stop");
        let merged = json_reuse(&document.to_string(), "claude", true).unwrap();
        assert_eq!(
            hook_entries(&merged, "Stop"),
            hook_entries(&settings, "Stop")
        );
        // A shell form written by an older Claude Code stays a shell form.
        let shell = json_merge("", "claude", true, Entry::shell(SHELL.into())).unwrap();
        let mut document: Value = serde_json::from_str(&shell).unwrap();
        document["hooks"]
            .as_object_mut()
            .unwrap()
            .remove("Notification");
        let merged = json_reuse(&document.to_string(), "claude", true).unwrap();
        assert_eq!(
            hook_entries(&merged, "Notification"),
            [json!({"type": "command", "command": SHELL, "timeout": 5})]
        );
        // Unix never reads an exec form as an hgs hook.
        assert_eq!(
            Entry::owned(&hook_entries(&settings, "Stop")[0], false),
            None
        );
        // Another program, or other arguments, is not hgs's hook.
        for (command, args) in [
            ("C:/tools/other.exe", json!(["__state", "hook"])),
            ("C:/tools/hgs.exe", json!(["__state", "hook", "x"])),
            ("C:/tools/hgs.exe", json!(["hook"])),
            ("C:/tools/hgs.exe", json!("__state hook")),
        ] {
            let hook = json!({"type": "command", "command": command, "args": args});
            assert_eq!(Entry::owned(&hook, true), None, "{hook}");
        }
        let hook =
            json!({"type": "command", "command": r"C:\Tools\HGS.EXE", "args": ["__state", "hook"]});
        assert!(Entry::owned(&hook, true).is_some());

        let codex = forms::codex_entry(STABLE).unwrap();
        let hooks = json_merge("", "codex", true, codex).unwrap();
        for event in events("codex") {
            assert_eq!(
                hook_entries(&hooks, event),
                [
                    json!({"type": "command", "command": SHELL, "commandWindows": POWERSHELL,
                    "timeout": 5})
                ]
            );
        }
        let mut document: Value = serde_json::from_str(&hooks).unwrap();
        document["hooks"]
            .as_object_mut()
            .unwrap()
            .remove("Interrupt");
        let merged = json_reuse(&document.to_string(), "codex", true).unwrap();
        assert_eq!(
            hook_entries(&merged, "Interrupt"),
            hook_entries(&hooks, "Stop")
        );
    }

    #[test]
    fn a_crlf_kimi_config_never_gets_a_second_block_on_windows() {
        let lf = kimi_merge("default_model = \"probe\"\n", true, SHELL).unwrap();
        assert!(lf.contains(&format!("command = {}\n", json!(SHELL))));
        assert_eq!(kimi_merge(&lf, true, "unused"), None);
        // An editor saved the whole file with Windows line ends.
        let crlf = lf.replace('\n', "\r\n");
        assert_eq!(kimi_merge(&crlf, true, "unused"), None);
        // A block that misses events is replaced, keeping its command.
        let partial = crlf.replace("event = \"SessionHeartbeat\"", "event = \"Other\"");
        let merged = kimi_merge(&partial, true, "unused").unwrap();
        assert_eq!(merged.matches(KIMI_BEGIN).count(), 1, "{merged}");
        assert_eq!(merged.matches(KIMI_END).count(), 1, "{merged}");
        assert!(
            merged.starts_with("default_model = \"probe\"\r\n"),
            "{merged}"
        );
        assert_eq!(kimi_merge(&merged, true, "unused"), None);
        // Mixed line ends, and text around the block, are kept.
        let mixed = format!(
            "a = 1\r\n{}b = 2\r\n",
            &lf["default_model = \"probe\"\n".len()..]
        );
        let mixed = mixed.replacen(&format!("{KIMI_END}\n"), &format!("{KIMI_END}\r\n"), 1);
        assert_eq!(kimi_merge(&mixed, true, "unused"), None);
        // Only a marker line counts.
        let other = format!("{KIMI_BEGIN}X\r\nx = 1\n");
        let merged = kimi_merge(&other, true, SHELL).unwrap();
        assert!(merged.starts_with(&other), "{merged}");
        assert_eq!(merged.matches(&format!("{KIMI_BEGIN}\n")).count(), 1);
        assert_eq!(marker_line("a\r\nb", "a", false), None);
        assert_eq!(marker_line("a\r\nb", "a", true), Some((0, 3)));
        assert_eq!(marker_line("ab\na\n", "a", false), Some((3, 2)));
    }

    /// A merge for an agent that reports `version`, which says how often it was asked.
    fn versioned_merge(
        original: &str,
        agent: &str,
        windows: bool,
        fresh: Entry,
        version: Option<Version>,
    ) -> (Option<String>, usize) {
        let mut asked = 0;
        let merged = merge_json(
            original,
            &path(),
            agent,
            windows,
            &mut || Ok(fresh.clone()),
            &mut || {
                asked += 1;
                version
            },
        )
        .unwrap();
        (merged, asked)
    }

    #[test]
    fn claude_model_switch_is_registered_only_for_versions_that_have_it() {
        let unix = Entry::shell(UNIX.into());
        let older = json_merge(r#"{"model": "opus"}"#, "claude", false, unix.clone()).unwrap();
        // Older Claude Code stops every start at a warning for an event it does
        // not know, so neither its key nor PreModelSwitch is ever written.
        for version in [None, Some((2, 1, 295)), Some((2, 0, 9))] {
            let (merged, asked) = versioned_merge(
                r#"{"model": "opus"}"#,
                "claude",
                false,
                unix.clone(),
                version,
            );
            assert_eq!(merged.as_ref(), Some(&older), "{version:?}");
            assert_eq!(asked, 1);
            let (merged, asked) = versioned_merge(&older, "claude", false, unix.clone(), version);
            assert_eq!((merged, asked), (None, 1), "{version:?}");
        }
        let hooks: Value = serde_json::from_str(&older).unwrap();
        assert!(hooks["hooks"].get("PostModelSwitch").is_none());
        assert!(hooks["hooks"].get("PreModelSwitch").is_none());
        // A version that has it gets the entry already there, and nothing else changes.
        for version in [CLAUDE_MODEL_SWITCH, (2, 1, 300), (2, 2, 0), (3, 0, 0)] {
            let (merged, asked) = versioned_merge(
                &older,
                "claude",
                false,
                Entry::shell("unused".into()),
                Some(version),
            );
            let merged = merged.unwrap();
            assert_eq!(asked, 1);
            assert_eq!(
                hook_entries(&merged, "PostModelSwitch"),
                hook_entries(&older, "Stop")
            );
            let mut document: Value = serde_json::from_str(&merged).unwrap();
            document["hooks"]
                .as_object_mut()
                .unwrap()
                .remove("PostModelSwitch");
            // The same versions also get the answer channel beside it.
            without_channel(&mut document);
            assert_eq!(document, serde_json::from_str::<Value>(&older).unwrap());
            assert!(document["hooks"].get("PreModelSwitch").is_none());
            // Once it is there, the version is never read again.
            assert_eq!(
                versioned_merge(&merged, "claude", false, unix.clone(), None),
                (None, 0)
            );
        }
        // A fresh configuration gets every event with one entry.
        let (fresh, _) =
            versioned_merge("", "claude", false, unix.clone(), Some(CLAUDE_MODEL_SWITCH));
        let fresh = fresh.unwrap();
        for event in events("claude").into_iter().chain(["PostModelSwitch"]) {
            assert_eq!(
                journal_entries(&fresh, event),
                [json!({"type": "command", "command": UNIX, "timeout": 5})]
            );
        }
        // The user's own PostModelSwitch hook stays, beside hgs's.
        let user = json!({"type": "command", "command": "notify-send switched"});
        let mut document: Value = serde_json::from_str(&older).unwrap();
        document["hooks"]["PostModelSwitch"] = json!([{"matcher": "opus", "hooks": [user]}]);
        let (merged, _) = versioned_merge(
            &document.to_string(),
            "claude",
            false,
            unix.clone(),
            Some(CLAUDE_MODEL_SWITCH),
        );
        let merged = merged.unwrap();
        assert_eq!(
            hook_entries(&merged, "PostModelSwitch"),
            [user, hook_entries(&older, "Stop")[0].clone()]
        );
        let (merged, _) = versioned_merge(
            &document.to_string(),
            "claude",
            false,
            unix.clone(),
            Some((2, 1, 295)),
        );
        assert_eq!(merged, None);
        // Only Claude has versioned events; Codex is never asked.
        let codex = json_merge("", "codex", false, unix.clone()).unwrap();
        assert_eq!(
            versioned_merge("", "codex", false, unix.clone(), Some((9, 9, 9))),
            (Some(codex), 0)
        );
        // Windows repeats the exec form there too.
        let exec = forms::claude_entry(STABLE, CLAUDE_EXEC_FORM, false).unwrap();
        let (settings, _) =
            versioned_merge("", "claude", true, exec.clone(), Some(CLAUDE_MODEL_SWITCH));
        let settings = settings.unwrap();
        assert_eq!(
            hook_entries(&settings, "PostModelSwitch"),
            hook_entries(&settings, "Stop")
        );
        assert_eq!(
            Entry::owned(&hook_entries(&settings, "PostModelSwitch")[0], true),
            Some(exec)
        );
    }

    /// The hook entries of `event` other than hgs's answer channel.
    fn journal_entries(text: &str, event: &str) -> Vec<Value> {
        hook_entries(text, event).into_iter().filter(|hook| !owns_channel(hook)).collect()
    }

    /// `document` without hgs's answer channel groups.
    fn without_channel(document: &mut Value) {
        if let Some(groups) = document["hooks"]["PermissionRequest"].as_array_mut() {
            groups.retain(|group| !group["hooks"].as_array().unwrap().iter().any(owns_channel));
        }
    }

    /// hgs's answer channel groups of a settings text.
    fn channel_groups(text: &str) -> Vec<Value> {
        let document: Value = serde_json::from_str(text).unwrap();
        document["hooks"]["PermissionRequest"]
            .as_array()
            .map(|groups| {
                groups.iter().filter(|group| group["hooks"].as_array().unwrap().iter().any(owns_channel)).cloned().collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn answer_channel_is_registered_only_for_qualified_claude() {
        let unix = Entry::shell(UNIX.into());
        let older = json_merge(r#"{"model": "opus"}"#, "claude", false, unix.clone()).unwrap();
        assert!(channel_groups(&older).is_empty());
        for version in [None, Some((2, 1, 295)), Some((2, 0, 9))] {
            assert_eq!(versioned_merge(&older, "claude", false, unix.clone(), version), (None, 1), "{version:?}");
        }
        let (merged, asked) =
            versioned_merge(&older, "claude", false, unix.clone(), Some(CLAUDE_ANSWER_CHANNEL));
        assert_eq!(asked, 1, "one version reading serves every versioned entry");
        let merged = merged.unwrap();
        assert_eq!(
            channel_groups(&merged),
            [json!({"hooks": [{"type": "command",
                "command": "'/opt/hgs dir/hgs' __state answer-channel", "timeout": 86400}]})]
        );
        // The journaling entry keeps its own group and short timeout.
        assert!(hook_entries(&merged, "PermissionRequest")
            .contains(&json!({"type": "command", "command": UNIX, "timeout": 5})));
        assert_eq!(versioned_merge(&merged, "claude", false, unix.clone(), None), (None, 0));
        // Claude's exec form derives the channel's arguments.
        let exec = forms::claude_entry(STABLE, CLAUDE_EXEC_FORM, false).unwrap();
        let channel = exec.channel().unwrap().channel_group();
        assert_eq!(channel["hooks"][0]["command"], json!(exec.command));
        assert_eq!(channel["hooks"][0]["args"], json!(["__state", "answer-channel"]));
        assert!(owns_channel(&channel["hooks"][0]));
        // A legacy hgs_state.py entry has no channel to offer.
        let legacy = Entry::shell("python3 /opt/hgs_state.py hook".into());
        let (legacy, _) = versioned_merge("", "claude", false, legacy, Some(CLAUDE_ANSWER_CHANNEL));
        assert!(channel_groups(&legacy.unwrap()).is_empty());
    }

    #[test]
    fn windows_runs_the_answer_channel_in_claude_exec_form_only() {
        let exec = forms::claude_entry(STABLE, CLAUDE_EXEC_FORM, false).unwrap();
        let channel = json!({"hooks": [{"type": "command", "command": exec.command,
            "args": ["__state", "answer-channel"], "timeout": 86400}]});
        // A new configuration, and one that an earlier hgs wrote without it.
        let earlier = json_merge("", "claude", true, exec.clone()).unwrap();
        assert!(channel_groups(&earlier).is_empty());
        for original in ["", earlier.as_str()] {
            let (merged, asked) =
                versioned_merge(original, "claude", true, exec.clone(), Some(CLAUDE_ANSWER_CHANNEL));
            assert_eq!(asked, 1);
            let merged = merged.unwrap();
            assert_eq!(channel_groups(&merged), std::slice::from_ref(&channel));
            // The journaling entry keeps its own group and short timeout.
            assert!(hook_entries(&merged, "PermissionRequest").contains(&json!({"type": "command",
                "command": exec.command, "args": ["__state", "hook"], "timeout": 5})));
            assert_eq!(versioned_merge(&merged, "claude", true, exec.clone(), None), (None, 0));
            // An older version loses it before it starts.
            let (pruned, _) = versioned_prune(&merged, "claude", true, Some((2, 1, 295)));
            assert!(channel_groups(&pruned.unwrap()).is_empty());
        }
        for version in [None, Some((2, 1, 295))] {
            assert_eq!(versioned_merge(&earlier, "claude", true, exec.clone(), version).0, None, "{version:?}");
        }
        // Claude Code runs the shell form under Git Bash, between Claude and
        // hgs: no channel there.
        let shell = forms::claude_entry(STABLE, (2, 1, 200), true).unwrap();
        let kept = json_merge("", "claude", true, shell.clone()).unwrap();
        let (merged, _) = versioned_merge(&kept, "claude", true, shell, Some(CLAUDE_ANSWER_CHANNEL));
        assert!(channel_groups(&merged.unwrap()).is_empty());
    }

    #[test]
    fn answer_channel_entry_goes_for_an_older_version() {
        let unix = Entry::shell(UNIX.into());
        let older = json_merge(r#"{"model": "opus"}"#, "claude", false, unix.clone()).unwrap();
        let (registered, _) =
            versioned_merge(&older, "claude", false, unix.clone(), Some(CLAUDE_ANSWER_CHANNEL));
        let registered = registered.unwrap();
        for version in [None, Some(CLAUDE_ANSWER_CHANNEL), Some((2, 2, 0))] {
            assert_eq!(versioned_prune(&registered, "claude", false, version), (None, 1), "{version:?}");
        }
        let (pruned, asked) = versioned_prune(&registered, "claude", false, Some((2, 1, 295)));
        assert_eq!(asked, 1);
        let pruned = pruned.unwrap();
        assert!(channel_groups(&pruned).is_empty());
        assert_eq!(serde_json::from_str::<Value>(&pruned).unwrap(), serde_json::from_str::<Value>(&older).unwrap());
    }

    #[test]
    fn question_only_channel_widens_in_place_for_approvals() {
        let unix = Entry::shell(UNIX.into());
        let current = json_merge("", "claude", false, unix.clone()).unwrap();
        let (current, _) = versioned_merge(&current, "claude", false, unix.clone(), Some(CLAUDE_ANSWER_CHANNEL));
        let current = current.unwrap();
        // The group an earlier hgs wrote for AskUserQuestion alone.
        let mut document: Value = serde_json::from_str(&current).unwrap();
        for group in document["hooks"]["PermissionRequest"].as_array_mut().unwrap() {
            if group["hooks"].as_array().unwrap().iter().any(owns_channel) {
                group["matcher"] = json!("AskUserQuestion");
            }
        }
        let earlier = serde_json::to_string_pretty(&document).unwrap() + "\n";
        assert_eq!(channel_groups(&earlier)[0]["matcher"], "AskUserQuestion");
        let (widened, asked) = versioned_merge(&earlier, "claude", false, unix.clone(), Some(CLAUDE_ANSWER_CHANNEL));
        assert_eq!(asked, 1);
        let widened = widened.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&widened).unwrap(), serde_json::from_str::<Value>(&current).unwrap());
        assert_eq!(channel_groups(&widened).len(), 1, "never two channel groups");
        assert_eq!(versioned_merge(&widened, "claude", false, unix.clone(), None), (None, 0));
        // A version that cannot be read or is older keeps the group as it is.
        for version in [None, Some((2, 1, 295))] {
            assert_eq!(versioned_merge(&earlier, "claude", false, unix.clone(), version).0, None, "{version:?}");
        }
        // An older version loses either shape.
        for registered in [&earlier, &widened] {
            let (pruned, _) = versioned_prune(registered, "claude", false, Some((2, 1, 295)));
            assert!(channel_groups(&pruned.unwrap()).is_empty());
        }
        // A group that also holds the user's hooks keeps its matcher.
        let mut shared: Value = serde_json::from_str(&earlier).unwrap();
        for group in shared["hooks"]["PermissionRequest"].as_array_mut().unwrap() {
            if group["matcher"] == "AskUserQuestion" {
                group["hooks"].as_array_mut().unwrap().push(json!({"type": "command", "command": "notify-send asked"}));
            }
        }
        let shared = serde_json::to_string_pretty(&shared).unwrap() + "\n";
        assert_eq!(versioned_merge(&shared, "claude", false, unix.clone(), Some(CLAUDE_ANSWER_CHANNEL)).0, None);
        // Windows keeps the terminal path.
        assert_eq!(versioned_merge(&earlier, "claude", true, unix, Some(CLAUDE_ANSWER_CHANNEL)).0, None);
    }

    #[test]
    fn answer_channel_keeps_user_hooks() {
        let unix = Entry::shell(UNIX.into());
        let user = json!({"matcher": "AskUserQuestion", "hooks": [{"type": "command", "command": "notify-send asked"}]});
        let mut document: Value =
            serde_json::from_str(&json_merge("", "claude", false, unix.clone()).unwrap()).unwrap();
        document["hooks"]["PermissionRequest"].as_array_mut().unwrap().push(user.clone());
        let (merged, _) = versioned_merge(&document.to_string(), "claude", false, unix.clone(), Some(CLAUDE_ANSWER_CHANNEL));
        let merged = merged.unwrap();
        let groups: Value = serde_json::from_str::<Value>(&merged).unwrap()["hooks"]["PermissionRequest"].clone();
        assert!(groups.as_array().unwrap().contains(&user));
        assert_eq!(channel_groups(&merged).len(), 1);
        let (pruned, _) = versioned_prune(&merged, "claude", false, Some((2, 1, 200)));
        let groups: Value = serde_json::from_str::<Value>(&pruned.unwrap()).unwrap()["hooks"]["PermissionRequest"].clone();
        assert!(groups.as_array().unwrap().contains(&user));
    }

    #[test]
    fn codex_and_kimi_get_no_answer_channel() {
        let unix = Entry::shell(UNIX.into());
        let (codex, asked) = versioned_merge("", "codex", false, unix.clone(), Some((9, 9, 9)));
        assert_eq!(asked, 0);
        assert!(channel_groups(&codex.unwrap()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn model_switch_version_never_runs_a_project_launcher() {
        let folder = tempfile::tempdir().unwrap();
        let marker = folder.path().join("launcher-ran");
        let launcher = folder.path().join("run_claude.sh");
        let script = format!("#!/bin/sh\n: > '{}'\necho '2.1.296 (Claude Code)'\n", marker.display());
        crate::platform::write_script(&launcher, &script);
        let program = launcher.to_string_lossy();
        let installed = Installed {
            program: &program,
            agent: "Claude Code",
            version: std::cell::OnceCell::new(),
            seen: folder.path().join("seen.json"),
        };
        // A launcher may prompt or unlock secrets: its version stays unknown.
        assert_eq!(installed.native_version("claude"), None);
        assert_eq!(installed.registered_version("claude"), None);
        assert!(!marker.exists());
        // The same script read as the agent itself answers.
        assert_eq!(installed.native_version(&program), Some(CLAUDE_MODEL_SWITCH));
        assert!(marker.exists());
    }

    /// A pruning for an agent that reports `version`, which says how often it was asked.
    fn versioned_prune(
        original: &str,
        agent: &str,
        windows: bool,
        version: Option<Version>,
    ) -> (Option<String>, usize) {
        let mut asked = 0;
        let pruned = prune_json(original, agent, windows, &mut || {
            asked += 1;
            version
        });
        (pruned, asked)
    }

    #[test]
    fn claude_model_switch_entry_goes_only_for_an_older_version() {
        let unix = Entry::shell(UNIX.into());
        let older = json_merge(r#"{"model": "opus"}"#, "claude", false, unix.clone()).unwrap();
        let (registered, _) = versioned_merge(
            &older,
            "claude",
            false,
            unix.clone(),
            Some(CLAUDE_MODEL_SWITCH),
        );
        let registered = registered.unwrap();
        // A version that has the event keeps the entry and every byte, and so
        // does one that cannot be read: it may well be current.
        for version in [None, Some(CLAUDE_MODEL_SWITCH), Some((2, 2, 0))] {
            assert_eq!(
                versioned_prune(&registered, "claude", false, version),
                (None, 1),
                "{version:?}"
            );
        }
        // An older version gets back what it had before the entry, and keeps it.
        for version in [(2, 1, 295), (2, 0, 9)] {
            let (pruned, asked) = versioned_prune(&registered, "claude", false, Some(version));
            assert_eq!((pruned.as_deref(), asked), (Some(older.as_str()), 1));
            assert_eq!(
                versioned_merge(&older, "claude", false, unix.clone(), Some(version)),
                (None, 1)
            );
            assert_eq!(
                versioned_prune(&older, "claude", false, Some(version)),
                (None, 0)
            );
        }
        // Only hgs's entries go, as hgs knows them (a legacy one too): the
        // user's hooks stay, and so does a group of theirs beside hgs's.
        let user = json!({"type": "command", "command": "notify-send switched"});
        let entry = hook_entries(&registered, "PostModelSwitch")[0].clone();
        let legacy = json!({"type": "command", "command": "python3 /old/hgs_state.py hook"});
        let mut document: Value = serde_json::from_str(&registered).unwrap();
        document["hooks"]["PostModelSwitch"] = json!([
            {"matcher": "opus", "hooks": [user]},
            {"hooks": [entry]},
            {"matcher": "haiku", "hooks": [entry, user, legacy]},
            {"matcher": "sonnet", "hooks": []},
            {"matcher": "fable"},
        ]);
        let (pruned, _) =
            versioned_prune(&document.to_string(), "claude", false, Some((2, 1, 295)));
        let mut pruned: Value = serde_json::from_str(&pruned.unwrap()).unwrap();
        assert_eq!(
            pruned["hooks"]["PostModelSwitch"],
            json!([
                {"matcher": "opus", "hooks": [user]},
                {"matcher": "haiku", "hooks": [user]},
                {"matcher": "sonnet", "hooks": []},
                {"matcher": "fable"},
            ])
        );
        for document in [&mut document, &mut pruned] {
            document["hooks"]
                .as_object_mut()
                .unwrap()
                .remove("PostModelSwitch");
            without_channel(document);
        }
        assert_eq!(pruned, document);
        // Without an hgs entry there, nothing asks for the version.
        let mut theirs: Value = serde_json::from_str(&older).unwrap();
        theirs["hooks"]["PostModelSwitch"] = json!([{"hooks": [user]}]);
        assert_eq!(
            versioned_prune(&theirs.to_string(), "claude", false, Some((2, 1, 295))),
            (None, 0)
        );
        // Only Claude has versioned events, and what merge_json reports stays for it.
        assert_eq!(
            versioned_prune(&registered, "codex", false, Some((2, 1, 295))),
            (None, 0)
        );
        for original in [
            "",
            "{",
            "[]",
            r#"{"hooks": []}"#,
            r#"{"hooks": {"PostModelSwitch": {}}}"#,
        ] {
            assert_eq!(
                versioned_prune(original, "claude", false, Some((2, 1, 295))),
                (None, 0),
                "{original:?}"
            );
        }
        // Windows knows its exec form as hgs's too.
        let exec = forms::claude_entry(STABLE, CLAUDE_EXEC_FORM, false).unwrap();
        let (settings, _) =
            versioned_merge("", "claude", true, exec.clone(), Some(CLAUDE_MODEL_SWITCH));
        let settings = settings.unwrap();
        let (pruned, _) = versioned_prune(&settings, "claude", true, Some((2, 1, 295)));
        let pruned = pruned.unwrap();
        let hooks: Value = serde_json::from_str(&pruned).unwrap();
        assert!(hooks["hooks"].get("PostModelSwitch").is_none());
        for event in events("claude") {
            assert_eq!(hook_entries(&pruned, event), journal_entries(&settings, event));
        }
    }

    #[cfg(unix)]
    #[test]
    fn registered_version_is_read_again_only_for_another_executable() {
        let folder = tempfile::tempdir().unwrap();
        let reads = folder.path().join("reads");
        let answer = folder.path().join("answer");
        let claude = folder.path().join("claude");
        // A new file in its place, as an installer writes it.
        let install = |rest: &str| {
            let new = folder.path().join("claude.new");
            let script = format!(
                "#!/bin/sh\necho >> '{}'\ncat '{}'\n{rest}",
                reads.display(),
                answer.display()
            );
            crate::platform::write_script(&new, &script);
            fs::rename(&new, &claude).unwrap();
        };
        let asked = || fs::read_to_string(&reads).map_or(0, |text| text.lines().count());
        let program = claude.to_string_lossy().into_owned();
        let seen = folder.path().join("state/hook-versions/settings.json");
        let launch = || Installed {
            program: &program,
            agent: "Claude Code",
            version: std::cell::OnceCell::new(),
            seen: seen.clone(),
        };
        install("");
        fs::write(&answer, "2.1.296 (Claude Code)\n").unwrap();
        // The launch that registers the entry keeps the version it read.
        assert_eq!(launch().native_version(&program), Some(CLAUDE_MODEL_SWITCH));
        assert_eq!(asked(), 1);
        // Later launches of the same file are not asked, whatever it would answer.
        fs::write(&answer, "2.1.295 (Claude Code)\n").unwrap();
        for _ in 0..2 {
            assert_eq!(
                launch().registered_version(&program),
                Some(CLAUDE_MODEL_SWITCH)
            );
        }
        assert_eq!(asked(), 1);
        // Another version installed in its place is, once.
        install("# 2.1.295\n");
        let installed = launch();
        assert_eq!(installed.registered_version(&program), Some((2, 1, 295)));
        assert_eq!(installed.native_version(&program), Some((2, 1, 295)));
        assert_eq!(launch().registered_version(&program), Some((2, 1, 295)));
        assert_eq!(asked(), 2);
        // A version that cannot be read stays unknown and is asked again.
        install("exit 3\n");
        for _ in 0..2 {
            assert_eq!(launch().registered_version(&program), None);
        }
        assert_eq!(asked(), 4);
        // A launcher, and an executable that is gone, are never run.
        assert_eq!(launch().registered_version("claude"), None);
        fs::remove_file(&claude).unwrap();
        assert_eq!(launch().registered_version(&program), None);
        assert_eq!(asked(), 4);
    }
}
