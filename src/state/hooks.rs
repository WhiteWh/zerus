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

thread_local! {
    /// The programs hgs owns in the settings file at hand: the one the running
    /// installation writes and those recorded for that file (zerus-b64dl).
    static OWN_PROGRAMS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// While alive, entries that run one of these exact programs count as hgs's
/// own, whatever their name: an `HGS_EXECUTABLE` wrapper writes them
/// (zerus-s3wax). Nothing else is ever guessed from a name or path.
struct OwnProgram(Vec<String>);

impl OwnProgram {
    #[cfg(all(test, unix))]
    fn scoped(program: &str) -> Self {
        Self::set(vec![program.to_owned()])
    }

    fn set(programs: Vec<String>) -> Self {
        Self(OWN_PROGRAMS.with(|own| own.replace(programs)))
    }

    /// The scope for the settings file `config`: the program `executable`
    /// writes, when known, and every program an install recorded for it.
    fn for_config(executable: Option<&str>, config: &Path) -> Self {
        let mut programs: Vec<String> = executable.map(written_program).into_iter().collect();
        programs.extend(recorded_programs(config));
        Self::set(programs)
    }
}

/// The program as hook entries name it (on Windows with forward slashes).
fn written_program(executable: &str) -> String {
    #[cfg(windows)]
    {
        windows::path(executable).unwrap_or_else(|_| executable.to_owned())
    }
    #[cfg(not(windows))]
    {
        executable.to_owned()
    }
}

/// Whether `program` is one of the programs owned in the current scope.
fn own_program(program: &str) -> bool {
    OWN_PROGRAMS.with(|own| own.borrow().iter().any(|own| own == program))
}

impl Drop for OwnProgram {
    fn drop(&mut self) {
        let previous = std::mem::take(&mut self.0);
        OWN_PROGRAMS.with(|own| *own.borrow_mut() = previous);
    }
}

/// The most programs, and the longest program, recorded for one settings file.
const RECORDED_PROGRAMS: usize = 8;
const RECORDED_PROGRAM_BYTES: usize = 4096;

/// Where hgs keeps the exact programs it wrote into the settings file
/// `config` (resolved past symlinks), in private state.
fn owner_record(config: &Path) -> PathBuf {
    root().join("hook-owners").join(format!(
        "{:x}.json",
        Sha256::digest(config.to_string_lossy().as_bytes())
    ))
}

fn recordable(program: &str) -> bool {
    !program.is_empty()
        && program.len() <= RECORDED_PROGRAM_BYTES
        && !program.chars().any(char::is_control)
}

/// The programs recorded in `record`, newest first; anything unreadable is none.
fn recorded_at(record: &Path) -> Vec<String> {
    let Ok(Some(bytes)) = super::hook_health::read_bounded(record, 64 * 1024) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Vec::new();
    };
    if value["schema"] != 1 {
        return Vec::new();
    }
    value["programs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|program| recordable(program))
        .take(RECORDED_PROGRAMS)
        .map(str::to_owned)
        .collect()
}

fn recorded_programs(config: &Path) -> Vec<String> {
    recorded_at(&owner_record(config))
}

/// Puts `program` first in `record`, keeping the newest few. The caller holds
/// the settings file's lock.
fn record_at(record: &Path, program: &str) -> Result<()> {
    if !recordable(program) {
        return Ok(());
    }
    let mut programs = recorded_at(record);
    if programs.first().map(String::as_str) == Some(program) {
        return Ok(());
    }
    programs.retain(|recorded| recorded != program);
    programs.insert(0, program.to_owned());
    programs.truncate(RECORDED_PROGRAMS);
    atomic(record, &json!({"schema":1,"programs":programs}).to_string())
}

/// The program a hook command line runs before hgs's two arguments.
fn program_of(command: &str) -> Option<String> {
    let argv = shell_words::split(command).ok()?;
    (argv.len() >= 3).then(|| argv[argv.len() - 3].clone())
}

/// Whether `content`, a settings file of `agent`, holds an hgs entry that runs
/// exactly `program`. Call inside the scope that owns `program`.
fn holds_program(content: &str, agent: &str, program: &str) -> bool {
    if agent == "kimi" {
        let Ok(document) = content.parse::<toml_edit::DocumentMut>() else {
            return false;
        };
        return document
            .get("hooks")
            .and_then(toml_edit::Item::as_array_of_tables)
            .is_some_and(|tables| {
                tables.iter().any(|table| {
                    table
                        .get("command")
                        .and_then(toml_edit::Item::as_str)
                        .is_some_and(|command| {
                            owns_hook(command) && program_of(command).as_deref() == Some(program)
                        })
                })
            });
    }
    let Ok(document) = serde_json::from_str::<Value>(content) else {
        return false;
    };
    document["hooks"]
        .as_object()
        .into_iter()
        .flat_map(|hooks| hooks.values())
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
        .flatten()
        .any(|hook| {
            let Some(command) = hook.get("command").and_then(Value::as_str) else {
                return false;
            };
            let named = if hook.get("args").is_some() {
                command == program
            } else {
                program_of(command).as_deref() == Some(program)
            };
            named && (Entry::owned(hook, cfg!(windows)).is_some() || owns_channel(hook))
        })
}

/// After hgs wrote or kept `content` as the settings file `config`, under its
/// lock: records the program this installation writes when an entry runs it,
/// so a later health read or repair without that `HGS_EXECUTABLE` still
/// recognises it (zerus-b64dl). A failed record only costs that recognition.
pub(super) fn record_written(config: &Path, agent: &str, content: &str) {
    let Ok(executable) = runner_executable() else {
        return;
    };
    let program = written_program(&executable);
    let _own = OwnProgram::for_config(Some(&executable), config);
    if holds_program(content, agent, &program) {
        let _ = record_at(&owner_record(config), &program);
    }
}

/// Repoint only tracking/channel entries whose exact program was recorded for
/// this file. Personal fields, trusted legacy shims and unrecorded entries stay.
fn migrate_recorded(
    original: &str,
    config: &Path,
    agent: &str,
    executable: &str,
    windows: bool,
) -> Result<Option<String>> {
    let programs = recorded_programs(config);
    migrate_programs(original, agent, executable, windows, programs)
}

fn migrate_programs(
    original: &str,
    agent: &str,
    executable: &str,
    windows: bool,
    programs: Vec<String>,
) -> Result<Option<String>> {
    let current = written_program(executable);
    let programs: Vec<_> = programs
        .into_iter()
        .filter(|program| program != &current)
        .collect();
    if programs.is_empty() {
        return Ok(None);
    }
    let line = |command: &str| -> Result<Option<String>> {
        let Ok(mut argv) = shell_words::split(command) else {
            return Ok(None);
        };
        // Only the standalone form hgs writes. Prefixes belong to the person;
        // parsing and re-quoting them would change shell operators/assignments.
        if argv.len() != 3
            || !programs.contains(&argv[0])
            || !(argv[1..] == HOOK_ARGS || argv[1..] == CHANNEL_ARGS)
        {
            return Ok(None);
        }
        let channel = argv[2] == "answer-channel";
        let with_args = |base: String| {
            if channel {
                format!("{}answer-channel", base.strip_suffix("hook").unwrap())
            } else {
                base
            }
        };
        let original = if windows {
            let Ok(base) = windows::shell_command(&argv[0]) else {
                return Ok(None);
            };
            with_args(base)
        } else {
            join_command(&argv)
        };
        // shell_words drops comments and normalizes quoting. Tokens alone
        // therefore cannot prove that the original line was written by hgs.
        if command != original {
            return Ok(None);
        }
        if windows {
            return windows::shell_command(executable).map(|base| Some(with_args(base)));
        }
        argv[0] = current.clone();
        Ok(Some(join_command(&argv)))
    };
    if agent == "kimi" {
        let mut document = original
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| e.to_string())?;
        let mut changed = false;
        for table in document
            .get_mut("hooks")
            .and_then(toml_edit::Item::as_array_of_tables_mut)
            .into_iter()
            .flat_map(|tables| tables.iter_mut())
        {
            let Some(command) = table.get("command").and_then(toml_edit::Item::as_str) else {
                continue;
            };
            if let Some(replacement) = line(command)? {
                // Preserve the value's comments and surrounding whitespace.
                if let Some(toml_edit::Value::String(value)) = table
                    .get_mut("command")
                    .and_then(toml_edit::Item::as_value_mut)
                {
                    let decor = value.decor().clone();
                    *value = toml_edit::Formatted::new(replacement);
                    *value.decor_mut() = decor;
                    changed = true;
                }
            }
        }
        return Ok(changed.then(|| document.to_string()));
    }
    let mut document: Value = if original.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(original).map_err(|e| e.to_string())?
    };
    let mut changed = false;
    for hook in document
        .get_mut("hooks")
        .and_then(Value::as_object_mut)
        .into_iter()
        .flat_map(|hooks| hooks.values_mut())
        .filter_map(Value::as_array_mut)
        .flatten()
        .filter_map(|group| group.get_mut("hooks").and_then(Value::as_array_mut))
        .flatten()
    {
        let Some(command) = hook.get("command").and_then(Value::as_str) else {
            continue;
        };
        let replacement = if let Some(args) = hook.get("args") {
            ((args == &json!(HOOK_ARGS) || args == &json!(CHANNEL_ARGS))
                && programs.iter().any(|program| program == command))
            .then(|| current.clone())
        } else {
            line(command)?
        };
        if let Some(replacement) = replacement {
            hook["command"] = json!(replacement);
            changed = true;
        }
        // Codex prefers its PowerShell override on Windows. Rewrite only the
        // exact override hgs wrote; a personal override remains untouched.
        if let Some(command) = hook.get("commandWindows").and_then(Value::as_str) {
            if programs.iter().any(|program| {
                windows::powershell_command(program).ok().as_deref() == Some(command)
            }) {
                hook["commandWindows"] = json!(windows::powershell_command(executable)?);
                changed = true;
            }
        }
    }
    if changed {
        Ok(Some(format!(
            "{}\n",
            serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?
        )))
    } else {
        Ok(None)
    }
}

/// An unrecorded hgs-like basename is ambiguous, never proof of ownership.
/// An unrelated program's choice of arguments cannot block explicit repair.
fn unrecognised_program(command: &str, args: Option<&Value>) -> bool {
    let ambiguous = |program: &str| {
        let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
        name.to_ascii_lowercase().contains("hgs") && !names_hgs(program)
    };
    if let Some(args) = args {
        return (args == &json!(HOOK_ARGS) || args == &json!(CHANNEL_ARGS)) && ambiguous(command);
    }
    shell_words::split(command).is_ok_and(|argv| {
        let tail = argv.len().saturating_sub(2);
        argv.len() >= 3
            && (argv[tail..] == HOOK_ARGS || argv[tail..] == CHANNEL_ARGS)
            && ambiguous(&argv[tail - 1])
    })
}

/// An explicit repair never writes beside an ambiguous unrecorded hgs entry.
const UNRECOGNISED_TRACKING: &str = "A hook entry runs an unrecorded hgs-like program; nothing was repaired. If it is your hgs wrapper, run repair or a supervised launch with HGS_EXECUTABLE set to that program to record it. Otherwise review or remove the ambiguous entry manually.";

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
    argv[argv.len() - 2] == "__state" && names_hgs(&argv[argv.len() - 3])
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
/// `/` or `\`, or exactly the program the running installation writes, which
/// an `HGS_EXECUTABLE` wrapper may name otherwise. A look-alike for any other
/// program stays the user's.
fn names_hgs(program: &str) -> bool {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    name == "hgs" || name.eq_ignore_ascii_case("hgs.exe") || own_program(program)
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
    // Earlier launches that did not recognise their own entries (zerus-s3wax)
    // left exact copies of hgs's groups: each delivered every event again.
    // Keep the first; a group with any personal hook is never touched.
    let mut changed = false;
    for groups in hooks.values_mut().filter_map(Value::as_array_mut) {
        let mut kept: Vec<Value> = Vec::new();
        let before = groups.len();
        groups.retain(|group| {
            let hgs_only = group
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|hooks| {
                    !hooks.is_empty()
                        && hooks
                            .iter()
                            .all(|hook| Entry::owned(hook, windows).is_some() || owns_channel(hook))
                });
            if hgs_only && kept.contains(group) {
                return false;
            }
            if hgs_only {
                kept.push(group.clone());
            }
            true
        });
        changed |= groups.len() != before;
    }
    let mut entry = hooks
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
        .flatten()
        .find_map(|hook| Entry::owned(hook, windows));
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
    let _own = OwnProgram::for_config(Some(&executable), &path);
    let installed = Installed {
        program,
        agent: if agent == "claude" { "Claude Code" } else { agent },
        version: std::cell::OnceCell::new(),
        seen: seen_version(&path),
    };
    let migrated = migrate_recorded(&original, &path, agent, &executable, cfg!(windows))?;
    let source = migrated.as_deref().unwrap_or(&original);
    let merged = if agent == "kimi" {
        merge_kimi(source, &path, cfg!(windows), &mut || {
            fresh_command(&executable)
        })?
    } else {
        // An older version run with this configuration later would stop at
        // hgs's entry for an event it lacks: the entry goes before it starts.
        let pruned = prune_json(source, agent, cfg!(windows), &mut || {
            installed.registered_version(agent)
        });
        merge_json(
            pruned.as_deref().unwrap_or(source),
            &path,
            agent,
            cfg!(windows),
            &mut || fresh_entry(agent, &executable, &installed),
            &mut || installed.native_version(agent),
        )?
        .or(pruned)
    }
    .or(migrated);
    let Some(content) = merged else {
        record_written(&path, agent, &original);
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
    record_written(&path, agent, &content);
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
/// `config` is the settings file the text came from: programs recorded for it
/// stay hgs's from any environment.
pub(super) fn health_document(
    original: &str,
    config: &Path,
    agent: &str,
    known: Option<(u64, u64, u64)>,
) -> Result<Value> {
    let _own = OwnProgram::for_config(runner_executable().ok().as_deref(), config);
    health_document_mode(original, agent, known, cfg!(windows))
}

fn health_document_mode(original: &str, agent: &str, known: Option<Version>, windows: bool) -> Result<Value> {
    let mut present = std::collections::BTreeSet::new();
    let mut channel = false;
    let mut channel_shell = false;
    let mut counts = std::collections::BTreeMap::<String,usize>::new();
    let mut conditional = false;
    // An hgs-like command for a program that is not hgs's in this scope.
    let mut unrecognised = false;
    if agent == "kimi" {
        let document = original.parse::<toml_edit::DocumentMut>().map_err(|_| "Invalid hook configuration")?;
        if let Some(item) = document.get("hooks") {
            let tables = item.as_array_of_tables().ok_or("Invalid hook configuration")?;
            for table in tables {
                let command = table.get("command").and_then(toml_edit::Item::as_str);
                unrecognised |= command.is_some_and(|command| unrecognised_program(command, None));
                if command.is_some_and(owns_hook) {
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
            unrecognised = hooks
                .values()
                .filter_map(Value::as_array)
                .flatten()
                .filter_map(|group| group.get("hooks").and_then(Value::as_array))
                .flatten()
                .any(|hook| {
                    hook.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|command| unrecognised_program(command, hook.get("args")))
                });
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
        "answer_channel_registered":channel,"unrecognised_tracking":unrecognised,"compatibility":{"state":if !incompatible.is_empty() || channel_incompatible {"incompatible"}else if known.is_some(){"compatible"}else{"unknown"},
        "basis":"Cached executable identity and known HGS versioned entries only",
        "native_version":known.map(|(a,b,c)|format!("{a}.{b}.{c}")),"unsupported_registered_events":incompatible}}))
}

/// Explicit repair uses only a previously verified version. It never probes a provider.
pub(super) fn repair_document(original: &str, path: &Path, agent: &str, known: (u64,u64,u64)) -> Result<Option<String>> {
    repair_with(original, path, agent, known, &runner_executable()?)
}

/// The repair `executable` would write. Health is read in the same scope, so
/// the program it writes and the programs it owns are one set (zerus-b64dl).
fn repair_with(
    original: &str,
    path: &Path,
    agent: &str,
    known: (u64, u64, u64),
    executable: &str,
) -> Result<Option<String>> {
    let _own = OwnProgram::for_config(Some(executable), path);
    let migrated = migrate_recorded(original, path, agent, executable, cfg!(windows))?;
    repair_owned(
        migrated.as_deref().unwrap_or(original),
        path,
        agent,
        known,
        executable,
    )
    .map(|merged| merged.or(migrated))
}

fn repair_owned(
    original: &str,
    path: &Path,
    agent: &str,
    known: Version,
    executable: &str,
) -> Result<Option<String>> {
    if agent == "kimi" {
        let health = health_document_mode(original, agent, Some(known), cfg!(windows))?;
        if health["unrecognised_tracking"] == true {
            return Err(UNRECOGNISED_TRACKING.into());
        }
        if health["state"]=="configured" {
            if health["duplicate_events"].as_array().is_none_or(Vec::is_empty) { return Ok(None); }
            return merge_kimi(original, path, cfg!(windows), &mut || fresh_command(executable));
        }
        // Rewriting an existing Kimi block can discard custom fields/personal
        // entries within it. Explicit repair refuses that lossy transformation.
        if health["state"]=="partial" || original.contains(KIMI_BEGIN) || original.contains(KIMI_END) {
            return Err("Partial Kimi hooks require a supervised launch or manual review; explicit repair preserved the existing block".into());
        }
        return merge_kimi(original, path, cfg!(windows), &mut || fresh_command(executable));
    }
    // Trusted legacy commands and personal fields remain exact.
    // Version-incompatible registrations are not removed by explicit repair.
    let health = health_document_mode(original, agent, Some(known), cfg!(windows))?;
    if health["unrecognised_tracking"] == true {
        return Err(UNRECOGNISED_TRACKING.into());
    }
    if health["compatibility"]["state"] != "compatible" { return Err("Hook configuration is incompatible; use a qualified supervised launch".into()); }
    merge_json_mode(original, path, agent, cfg!(windows), &mut || {
        #[cfg(windows)]
        { if agent=="claude" { windows::claude_entry(executable, known, windows::git_bash().is_some()) } else { windows::codex_entry(executable) } }
        #[cfg(unix)]
        { Ok(Entry::shell(fresh_command(executable)?)) }
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
            .env_remove("HGS_SESSION_ID")
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
/// Task 135 (docs/lore.md 6.2 and 11.12): the start context of a team member's
/// session after a compaction or a clear gets the handoff brief in recall form,
/// at most `RECALL_UNITS`, and never so much that the whole start context passes
/// `MAX_START_CONTEXT` (the iron rules and the hint keep their room: a recall
/// that would not leave 600 units is left out). It reads the team's record only
/// and writes nothing.
fn with_team_recall(hint: Option<String>, tasks: Option<&Value>, record: &Value, event: &Value) -> Option<String> {
    if string(event, "hook_event_name") != "SessionStart"
        || !string(event, "agent_id").is_empty()
        || !matches!(string(event, "source"), "compact" | "clear")
    {
        return hint;
    }
    let used = hint.as_deref().map_or(0, |text| text.encode_utf16().count())
        + tasks
            .and_then(|context| context["hookSpecificOutput"]["additionalContext"].as_str())
            .map_or(0, |text| text.encode_utf16().count())
        + 4;
    let room = super::iron_rules::MAX_START_CONTEXT.saturating_sub(used).min(teams::handoff::RECALL_UNITS);
    if room < 600 {
        return hint;
    }
    let Some(recall) = team_recall(record, room) else { return hint };
    Some(match hint {
        Some(hint) if !hint.is_empty() => format!("{hint}\n\n{recall}"),
        _ => recall,
    })
}

/// docs/lore.md 6.2 (RogueDOD, 2026-11-11): every own session start (startup,
/// clear, compact; team or not) gets the titles of the project's faults and
/// decisions in force from `lore digest`, at most 2000 bytes and only in the
/// room the start context has left, so the iron rules, the hint and a team's
/// recall come first. The store is the repository's (its git root's lore.toml
/// with real areas) unless its own hooks give the digest, else the project's
/// Zerus store when its memory is on. A slow or missing lore adds nothing.
fn with_lore_digest(hint: Option<String>, tasks: Option<&Value>, record: &Value, event: &Value, project: &str) -> Option<String> {
    if string(event, "hook_event_name") != "SessionStart"
        || !string(event, "agent_id").is_empty()
        || !matches!(string(event, "source"), "startup" | "clear" | "compact")
    {
        return hint;
    }
    let used = hint.as_deref().map_or(0, |text| text.encode_utf16().count())
        + tasks
            .and_then(|context| context["hookSpecificOutput"]["additionalContext"].as_str())
            .map_or(0, |text| text.encode_utf16().count())
        + 4;
    // UTF-8 bytes are never fewer than UTF-16 units: a digest of N bytes fits N units.
    let room = super::iron_rules::MAX_START_CONTEXT.saturating_sub(used).min(2000 + 200);
    if room < 600 {
        return hint;
    }
    let project = lore_project(record, project);
    let Some(digest) = super::project_memory::session_digest(Path::new(string(record, "launch_dir")), &project, room - 200) else { return hint };
    Some(match hint {
        Some(hint) if !hint.is_empty() => format!("{hint}

{digest}"),
        _ => digest,
    })
}

/// The project a session's lore hooks read: the iron rules' project, else
/// its team's.
fn lore_project(record: &Value, project: &str) -> String {
    if !project.is_empty() {
        return project.to_owned();
    }
    let team = || -> Option<String> {
        if !teams::store::path().exists() {
            return None;
        }
        let (team, _) = teams::store::Store::open(false).ok()?.rules_member(record).ok()??;
        Some(team.project_id)
    };
    team().unwrap_or_default()
}

/// CodeLore 92a1c04 (2026-11-11): the store `lore hook pre-edit` reads for a
/// Claude session, chosen once at its own start and kept in its record, so an
/// edit costs no project lookup; none when the repository's own hooks run it.
fn remember_edit_store(record: &mut Value, project: &str) {
    let project = lore_project(record, project);
    let store = super::project_memory::session_store(Path::new(string(record, "launch_dir")), &project, &super::project_memory::EDIT_HOOKS);
    match store {
        Some(store) => record["lore_edit_store"] = json!(store.display().to_string()),
        None => {
            if let Some(map) = record.as_object_mut() {
                map.remove("lore_edit_store");
            }
        }
    }
}

/// The store whose `lore hook pre-edit` this event asks: a Claude Edit, Write,
/// MultiEdit or NotebookEdit in a session that has one.
fn edit_store(record: &Value, event: &Value, agent: &str) -> Option<PathBuf> {
    (agent == "claude"
        && string(event, "hook_event_name") == "PreToolUse"
        && super::project_memory::EDIT_TOOLS.contains(&string(event, "tool_name")))
    .then(|| string(record, "lore_edit_store"))
    .filter(|store| !store.is_empty())
    .map(PathBuf::from)
}

fn team_recall(record: &Value, room: usize) -> Option<String> {
    let store = teams::store::Store::open(false).ok()?;
    let (team, member) = store.rules_member(record).ok()??;
    let items = store.events(&team.id, None, 60).ok().and_then(|page| page["items"].as_array().cloned()).unwrap_or_default();
    let pulses = teams::handoff::pulse_lines(&team, &member, &items, now());
    Some(teams::handoff::build(&team, &member, teams::handoff::Why::Compaction, &pulses, room).text)
}

fn start_context(hint: Option<String>, tasks: Option<Value>) -> Option<String> {
    let tasks = tasks.and_then(|context| context["hookSpecificOutput"]["additionalContext"].as_str().map(str::to_owned));
    let parts: Vec<String> = hint.into_iter().chain(tasks).collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// DCB-svn report (2026-11-12): Claude Code cancels a hook that runs past its
/// 5 s timeout. On a busy computer a new session's SessionStart was cancelled
/// before it bound the conversation, every later event was dropped as another
/// conversation's, and the team member never showed Ready. The first main
/// event of a new Claude session that has no conversation yet binds it as
/// that start would have; a resume, a fork or a fresh session that expects a
/// conversation still waits for its own start.
fn late_start(record: &Value, event: &Value, agent: &str) -> bool {
    let sid = string(event, "session_id");
    agent == "claude"
        && string(event, "hook_event_name") != "SessionStart"
        && string(event, "agent_id").is_empty()
        && !sid.is_empty()
        && string(record, "conversation_id").is_empty()
        && string(record, "expected_id").is_empty()
        && string(record, "fork_parent_id").is_empty()
        && string(record, "restore_archive_id").is_empty()
        && string(&record["fresh_from"], "conversation_id") != sid
        && string(record, "startup_kind") == "new"
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
/// The agent whose hint shape the start context takes: the decided record's,
/// else the environment's.
fn format_agent(record: &str, environment: Option<String>) -> String {
    if record.is_empty() {
        environment.unwrap_or_default()
    } else {
        record.to_owned()
    }
}

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
    if kind == "SessionHeartbeat" {
        // Task 121: the heartbeat's whole effect is the session's last activity;
        // it is a fast append that never waits on the state lock (Kimi stops a
        // hook after 5 s, and a slow store under a delivery's lock outlived it).
        super::heartbeat::observed(&token);
        return Ok(0);
    }
    if let Some(refusal) = command_refusal(&event, &home().to_string_lossy()) {
        eprintln!("{} [{}]", refusal.message, refusal.rule);
        return Ok(2);
    }
    let _guard = lock(None)?;
    let panes = live()?;
    let own = read_own_run(&name, &token)?;
    let record = if continuations::foreign(own.as_ref(), sid) {
        continuations::owner(&event, sid, &panes)?
    } else {
        own
    };
    let Some(mut record) = record else {
        return Ok(0);
    };
    if !matches(&record, panes_of(&panes, &record)) {
        return Ok(0);
    }
    if !terminating::completion_hook(&record, kind) {
        eprintln!("hgs is terminating this session; no new work or binding was admitted");
        return Ok(2);
    }
    if let Some(scoped) = external_agents::route(&mut record, &event) {
        if scoped["hgs_retired_external_hook"] == true { return Ok(0); }
        processes::observe(&mut record, &scoped);
        journal::update_activity(&mut record, &scoped);
        subagent_lifecycle::reconcile(&mut record);
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
    let successor = codex_clear_recovery::observe(&record, &event)
        .or_else(|| codex_clear_recovery::terminal_start(&record, &event));
    let main_start = (kind == "SessionStart" && string(&event, "agent_id").is_empty())
        || successor.is_some()
        || late_start(&record, &event, &agent);
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
            && !clear_context::resumed_start(&record, &event) && successor.is_none()
            && !clear_context::resumed_session(&event)
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
                "subagent_retirements",
                "subagents_ended_pruned",
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
    let terminal_resume_end = kind == "SessionEnd" && string(&event,"agent_id").is_empty()
        && clear_context::ending_terminal_resume(&record) && external_agents::main_codex_hook(&record);
    let terminal_clear_at = if terminal_resume_end && clear_context::awaiting_start(&record)
        && record["session_clear"]["source"] == "native_terminal" { record["session_clear"]["at"].clone() } else { Value::Null };
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
    keep_warm::observe(&mut record, &mut event, now());
    journal::update_activity(&mut record, &event);
    keep_warm::settle(&mut record, &event, now());
    if resumed_clear_end {
        // The real main hook ended this requested clear, not its supervised
        // process. Keep the exact deferred-start fence, never fabricate Ready.
        record["session_end"]["clear_request_id"] = record["clear_context_request"]["request_id"].clone();
        record["session_end"]["clear_event_at"] = record["last_event_at"].clone();
    }
    if terminal_resume_end {
        record["session_end"]["terminal_resume"] = json!({"event_at":record["last_event_at"],
            "pid":record["pid"],"process_start":record["process_start"],"supervisor":record["supervisor"],
            "transcript":record["transcript"],"clear_at":terminal_clear_at});
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
    if changed_conversation && clear_context::cleanup_pending(&record) {
        archive::reserve_clear_source(&record)?;
    }
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
    // Task 135: a team session whose context was compacted or cleared starts
    // with the team's recall, the handoff brief built from its own record.
    let hint = with_team_recall(hint, tasks.as_ref(), &record, &event);
    // docs/lore.md 6.2: the project's lore digest, after the recall.
    let hint = with_lore_digest(hint, tasks.as_ref(), &record, &event, rules.as_ref().map_or("", |(project, _)| project.as_str()));
    if own_start && agent == "claude" {
        remember_edit_store(&mut record, rules.as_ref().map_or("", |(project, _)| project.as_str()));
    }
    if let Some(context) = start_context(hint, tasks) {
        // A failed write or flush leaves only the early invalidation persisted.
        // Never expose a fresh conversation as started before this succeeds.
        // Kimi reads its start context from its own shape (upstream, `output_for`).
        agent_hint::output_for(&format_agent(&agent, nonempty_env("HGS_AGENT")), output, &context)?;
        if let Some((project, text)) = rules.filter(|_| context.encode_utf16().count() <= super::iron_rules::MAX_START_CONTEXT) {
            record["iron_rules_start_supported"] = json!(true);
            record["iron_rules_start_project"] = json!(project);
            record["iron_rules_start_digest"] = json!(teams::store::digest(&json!(text)));
            record["iron_rules_start_run"] = record["run_id"].clone();
            record["iron_rules_start_conversation"] = record["conversation_id"].clone();
        }
    }
    // Task 134: a team member's unread messages go into this turn as the
    // turn-start hook's added context (the agent reads it; nothing is typed).
    // They stay unread until the member reads them with hgs team inbox.
    if kind == "UserPromptSubmit" && string(&event, "agent_id").is_empty() && teams::doorbell::turn_hook(&agent) {
        if let Some(context) = teams::working::turn_context_for(string(&record, "name"), string(&record, "run_id")) {
            agent_hint::output_event(output, &context, "UserPromptSubmit")?;
        }
    }
    // Task 186 (docs/lore.md 11.11): a tool boundary is a safe point for an
    // urgent team message: Claude Code's PostToolUse hook returns it as added
    // context, never typed into the working composer. One file check per
    // tool call until such a message waits.
    if kind == "PostToolUse" && string(&event, "agent_id").is_empty() && agent == "claude" {
        if let Some(context) = teams::working::tool_context_for(string(&record, "name"), string(&record, "run_id")) {
            agent_hint::output_event(output, &context, "PostToolUse")?;
        }
    }
    let edit_store = edit_store(&record, &event, &agent);
    // The ready conversation and its proof become visible in the same atomic
    // record update; hook() performs no later proof-only binding write.
    write(&mut record)?;
    if own_start {
        archive::confirm_restore(&mut record)?;
    }
    // CodeLore's pre-edit hook: what lore knows in force about the file about
    // to change, once per file and session. It runs after the state lock is
    // released, so a slow lore (2 seconds at most) holds no other session.
    if let Some(store) = edit_store {
        drop(_guard);
        if let Some(context) = super::project_memory::pre_edit_context(&store, &event) {
            agent_hint::output_event(output, &context, "PreToolUse")?;
        }
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
    fn a_new_claude_session_whose_start_hook_was_cancelled_binds_on_its_next_event() {
        // DCB-svn report (2026-11-12): Claude Code cancelled the SessionStart
        // hook after 5 s; the prompt and the Stop that followed were dropped and
        // the member never showed Ready.
        for (key, value, binds) in [
            ("startup_kind", json!("new"), true),
            ("expected_id", json!("resumed-conversation"), false),
            ("fork_parent_id", json!("source-conversation"), false),
        ] {
            let directory = tempfile::tempdir().unwrap();
            super::super::with_test_state_root(directory.path(), || {
                let mut record = pending_rules_start();
                record["startup_kind"] = json!("new");
                record[key] = value.clone();
                write(&mut record).unwrap();
                let prompt = json!({"hook_event_name":"UserPromptSubmit","session_id":"late-conversation","prompt":"You are Tester",
                    "transcript_path":"C:/fixture/late-conversation.jsonl"});
                crate::state::backend::with_fake(&StartupBackend, || {
                    assert_eq!(track("claude/hooks/parent".into(), "parent-run".into(), prompt, &mut Vec::new()).unwrap(), 0);
                    let stop = json!({"hook_event_name":"Stop","session_id":"late-conversation","last_assistant_message":"Ready."});
                    assert_eq!(track("claude/hooks/parent".into(), "parent-run".into(), stop, &mut Vec::new()).unwrap(), 0);
                });
                let saved: Value = serde_json::from_slice(&fs::read(record_path("claude/hooks/parent")).unwrap()).unwrap();
                if binds {
                    assert_eq!(saved["conversation_id"], "late-conversation", "{key}");
                    assert_eq!(saved["activity"], "idle", "{key}: {saved}");
                    assert_eq!(saved["transcript"], "C:/fixture/late-conversation.jsonl");
                } else {
                    assert_eq!(saved["conversation_id"], "", "{key}");
                    assert_eq!(saved["activity"], "unknown", "{key}");
                }
            });
        }
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
    fn hint_output_format_comes_from_the_decided_record_not_the_environment() {
        assert_eq!(format_agent("kimi", Some("claude".into())), "kimi");
        assert_eq!(format_agent("", Some("kimi".into())), "kimi");
        assert_eq!(format_agent("", None), "");
    }

    #[test]
    fn health_registration_preserves_personal_and_legacy_hooks_and_never_grants_trust() {
        let personal=json!({"type":"command","command":"personal-hook --secret example"});
        let legacy=json!({"type":"command","command":"python3 /opt/old/hgs_state.py hook","timeout":5});
        let original=json!({"apiKey":"never disclose", "hooks":{"Stop":[{"hooks":[personal.clone(),legacy.clone()]}]}}).to_string();
        let health=health_document(&original,&path(),"claude",None).unwrap();
        assert_eq!(health["state"],"partial");assert_eq!(health["compatibility"]["state"],"unknown");
        assert!(!health.to_string().contains("secret"));assert!(!health.to_string().contains("apiKey"));
        let merged=merge_json(&original,Path::new("/config"),"claude",false,&mut ||panic!("must retain legacy command"),&mut ||Some((2,1,296))).unwrap().unwrap();
        let doc:Value=serde_json::from_str(&merged).unwrap();
        assert_eq!(doc["hooks"]["Stop"][0]["hooks"],json!([personal,legacy]));
        assert_eq!(health_document(&merged,&path(),"claude",Some((2,1,296))).unwrap()["state"],"configured");
        assert!(merge_json(&merged,Path::new("/config"),"claude",false,&mut ||panic!(),&mut ||Some((2,1,296))).unwrap().is_none());
    }
    #[test]
    fn tracking_repair_does_not_enable_or_widen_answer_channels() {
        let empty=merge_json_mode("{}",Path::new("/config"),"claude",false,&mut ||Ok(Entry::shell("hgs __state hook".into())),&mut ||Some((2,1,296)),false).unwrap().unwrap();
        assert!(!health_document(&empty,&path(),"claude",Some((2,1,296))).unwrap()["answer_channel_registered"].as_bool().unwrap());
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
        assert_eq!(health_document(&text,&path(),"claude",None).unwrap()["duplicate_events"],json!(["Stop"]));
        let kimi="# BEGIN hgs session tracking\n[[hooks]]\nevent = \"Stop\"\ncommand = \"hgs __state hook\"\ntimeout = 7\n[[hooks]]\nevent = \"Stop\"\ncommand = \"personal-hook\"\n# END hgs session tracking\n";
        assert!(repair_document(kimi,Path::new("/config"),"kimi",(2,1,1)).is_err());
    }

    #[test]
    fn health_reports_version_drift_and_rejects_malformed_owned_handlers() {
        let future=json!({"hooks":{"PostModelSwitch":[{"hooks":[{"type":"command","command":"hgs __state hook"}]}]}}).to_string();
        assert_eq!(health_document(&future,&path(),"claude",Some((2,1,200))).unwrap()["compatibility"]["state"],"incompatible");
        assert_eq!(health_document(&future,&path(),"claude",None).unwrap()["compatibility"]["state"],"unknown");
        for text in ["[]","{",r#"{"hooks":{"Stop":"invalid"}}"#,r#"{"hooks":{"Stop":[{"hooks":[{"type":"prompt","command":"hgs __state hook"}]}]}}"#] {
            assert!(health_document(text,&path(),"claude",Some((2,1,296))).is_err(),"{text}");
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

    /// zerus-s3wax: a wrapper-named HGS_EXECUTABLE writes entries hgs must own.
    #[cfg(unix)]
    const WRAPPER: &str = "/opt/tools/hgs-wrapper";

    #[cfg(unix)]
    fn wrapper_entry() -> Entry {
        Entry::shell(fresh_command(WRAPPER).unwrap())
    }

    /// Repeated launches: a merge that changes nothing returns `None`.
    #[cfg(unix)]
    fn launches(agent: &str, times: usize) -> (String, Vec<bool>) {
        let _own = OwnProgram::scoped(WRAPPER);
        let mut text = String::new();
        let mut writes = Vec::new();
        for _ in 0..times {
            let merged = merge_json(
                &text,
                &path(),
                agent,
                false,
                &mut || Ok(wrapper_entry()),
                &mut || Some(CLAUDE_ANSWER_CHANNEL),
            )
            .unwrap();
            writes.push(merged.is_some());
            if let Some(merged) = merged {
                text = merged;
            }
        }
        (text, writes)
    }

    #[cfg(unix)]
    fn wrapper_groups(text: &str, event: &str) -> usize {
        let document: Value = serde_json::from_str(text).unwrap();
        document["hooks"][event].as_array().map_or(0, |groups| {
            groups
                .iter()
                .filter(|g| {
                    g["hooks"].as_array().is_some_and(|hooks| {
                        hooks
                            .iter()
                            .any(|h| h["command"].as_str().is_some_and(|c| c.contains(WRAPPER)))
                    })
                })
                .count()
        })
    }

    #[cfg(unix)]
    #[test]
    fn a_wrapper_named_executable_owns_its_claude_and_codex_entries_across_launches() {
        for agent in ["claude", "codex"] {
            let (text, writes) = launches(agent, 3);
            assert_eq!(
                writes,
                [true, false, false],
                "{agent}: only the first launch writes"
            );
            for event in events(agent) {
                assert_eq!(
                    wrapper_groups(&text, event),
                    if event == "PermissionRequest" && agent == "claude" {
                        2
                    } else {
                        1
                    },
                    "{agent} {event}"
                );
            }
            if agent == "claude" {
                assert_eq!(wrapper_groups(&text, "PostModelSwitch"), 1);
                let _own = OwnProgram::scoped(WRAPPER);
                let channels = hook_entries(&text, "PermissionRequest")
                    .iter()
                    .filter(|h| owns_channel(h))
                    .count();
                assert_eq!(channels, 1, "one answer channel");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_wrapper_named_executable_owns_its_kimi_block_across_launches() {
        let _own = OwnProgram::scoped(WRAPPER);
        let first = kimi_merge("model = \"k\"\n", false, &fresh_command(WRAPPER).unwrap()).unwrap();
        assert!(first.contains(WRAPPER));
        assert_eq!(
            kimi_merge(&first, false, "unused"),
            None,
            "a later launch keeps every byte"
        );
    }

    #[cfg(unix)]
    #[test]
    fn duplicate_groups_hgs_wrote_collapse_and_personal_hooks_stay() {
        let _own = OwnProgram::scoped(WRAPPER);
        let ours = wrapper_entry().group();
        let personal = json!({"hooks": [{"type": "command", "command": "notify-send done"}]});
        let shared = json!({"matcher": "Bash", "hooks": [{"type": "command", "command": "audit"}, ours["hooks"][0].clone()]});
        let foreign = json!({"hooks": [{"type": "command", "command": "/srv/other-tool __state hook", "timeout": 5}]});
        let original = json!({"model": "keep", "hooks": {
            "SessionStart": [ours.clone(), personal.clone(), ours.clone(), ours.clone(), foreign.clone(), foreign.clone()],
            "Stop": [shared.clone(), ours.clone(), ours.clone()],
        }}).to_string();
        let merged = merge_json(
            &original,
            &path(),
            "codex",
            false,
            &mut || Ok(wrapper_entry()),
            &mut || None,
        )
        .unwrap()
        .unwrap();
        let document: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(document["model"], "keep");
        // Later copies of hgs's own group go; the personal group, a group that
        // also holds a personal hook, and a look-alike for another program stay.
        assert_eq!(
            document["hooks"]["SessionStart"],
            json!([ours, personal, foreign, foreign])
        );
        assert_eq!(document["hooks"]["Stop"], json!([shared, ours]));
        for event in events("codex") {
            assert!(wrapper_groups(&merged, event) >= 1, "{event}");
        }
        assert!(merge_json(
            &merged,
            &path(),
            "codex",
            false,
            &mut || panic!("reuses its entry"),
            &mut || None
        )
        .unwrap()
        .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn another_programs_state_hook_is_never_adopted_without_its_identity() {
        // Not the current executable and not named hgs: a personal hook.
        let foreign = json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "/srv/other-tool __state hook", "timeout": 5}]}]}}).to_string();
        let _own = OwnProgram::scoped(WRAPPER);
        let merged = json_merge(&foreign, "codex", false, wrapper_entry()).unwrap();
        let start = hook_entries(&merged, "SessionStart");
        assert_eq!(start.len(), 2);
        assert_eq!(start[0]["command"], "/srv/other-tool __state hook");
        assert!(start[1]["command"].as_str().unwrap().contains(WRAPPER));
        drop(_own);
        // Outside an installation's scope the wrapper is not hgs either.
        assert!(!owns_hook(&fresh_command(WRAPPER).unwrap()));
    }

    #[test]
    fn migration_preserves_windows_forms_channels_and_personal_fields() {
        let old = "C:/old path/runner.exe";
        let current = "C:/new path/runner.exe";
        let mut codex = windows::codex_entry(old).unwrap().group();
        codex["matcher"] = json!("personal matcher");
        codex["hooks"][0]["custom"] = json!({"keep": true});
        let mut exec = windows::claude_entry(old, (2, 1, 296), false)
            .unwrap()
            .group();
        exec["hooks"][0]["args"] = json!(CHANNEL_ARGS);
        let foreign = json!({"hooks": [{"type": "command", "command": "C:/other/tool.exe", "args": HOOK_ARGS, "timeout": 23}]});
        let mut personal_override = windows::codex_entry(old).unwrap().group();
        personal_override["hooks"][0]["commandWindows"] = json!("personal override");
        let original = json!({"permissions": {"keep": true}, "hooks": {"Stop": [codex.clone(), exec.clone(), foreign.clone(), personal_override]}});
        let merged = migrate_programs(
            &original.to_string(),
            "codex",
            current,
            true,
            vec![old.into()],
        )
        .unwrap()
        .unwrap();
        let result: Value = serde_json::from_str(&merged).unwrap();
        let hooks = &result["hooks"]["Stop"];
        assert_eq!(
            hooks[0]["hooks"][0]["command"],
            windows::shell_command(current).unwrap()
        );
        assert_eq!(
            hooks[0]["hooks"][0]["commandWindows"],
            windows::powershell_command(current).unwrap()
        );
        assert_eq!(hooks[0]["matcher"], codex["matcher"]);
        assert_eq!(hooks[0]["hooks"][0]["custom"], codex["hooks"][0]["custom"]);
        assert_eq!(hooks[1]["hooks"][0]["command"], current);
        assert_eq!(hooks[1]["hooks"][0]["args"], json!(CHANNEL_ARGS));
        assert_eq!(hooks[2], foreign);
        assert_eq!(hooks[3]["hooks"][0]["commandWindows"], "personal override");
        assert_eq!(result["permissions"], original["permissions"]);
        assert!(migrate_programs(
            &merged,
            "codex",
            current,
            true,
            vec![old.into(), current.into()]
        )
        .unwrap()
        .is_none());
        // Basename ownership alone is not a reason to migrate a command; a
        // legacy shim and an unrecorded wrapper likewise keep their bytes.
        let unrecorded = json!({"hooks": {"Stop": [{"hooks": [
            {"type": "command", "command": "/usr/bin/hgs __state hook"},
            {"type": "command", "command": "/old/hgs-wrapper __state hook"},
            {"type": "command", "command": "python3 /old/hgs_state.py hook"},
            {"type": "command", "command": old, "args": ["notify"]}
        ]}]}})
        .to_string();
        assert!(
            migrate_programs(&unrecorded, "claude", current, true, vec![old.into()])
                .unwrap()
                .is_none()
        );
    }

    /// zerus-b64dl: the exact programs hgs recorded for one settings file.
    #[test]
    fn owner_records_keep_the_newest_exact_programs_within_bounds() {
        let temp = tempfile::tempdir().unwrap();
        let record = temp.path().join("hook-owners").join("file.json");
        assert!(recorded_at(&record).is_empty());
        for index in 0..10 {
            record_at(&record, &format!("/opt/hgs-{index}")).unwrap();
        }
        let newest: Vec<String> = (2..10).rev().map(|i| format!("/opt/hgs-{i}")).collect();
        assert_eq!(recorded_at(&record), newest);
        let written: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        assert_eq!(
            written,
            json!({"schema": 1, "programs": newest}),
            "the file itself keeps 8"
        );
        // A program recorded again moves first; it is never kept twice.
        record_at(&record, "/opt/hgs-5").unwrap();
        let programs = recorded_at(&record);
        assert_eq!(programs[0], "/opt/hgs-5");
        assert_eq!(programs.len(), RECORDED_PROGRAMS);
        assert_eq!(programs.iter().filter(|p| *p == "/opt/hgs-5").count(), 1);
        // An empty, oversized or control-character program is never written.
        let before = fs::read(&record).unwrap();
        for program in [
            String::new(),
            "x".repeat(RECORDED_PROGRAM_BYTES + 1),
            "/opt/a\nb".into(),
        ] {
            record_at(&record, &program).unwrap();
            assert_eq!(fs::read(&record).unwrap(), before);
        }
        record_at(&record, &"y".repeat(RECORDED_PROGRAM_BYTES)).unwrap();
        assert_eq!(recorded_at(&record)[0].len(), RECORDED_PROGRAM_BYTES);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o077;
            assert_eq!(mode(&record), 0, "the record is private");
            assert_eq!(mode(record.parent().unwrap()), 0, "its folder is private");
        }
        // Replaced atomically: no temporary file stays beside the record.
        assert_eq!(fs::read_dir(record.parent().unwrap()).unwrap().count(), 1);
        // A damaged, foreign, oversized or hostile record grants only valid programs.
        let many: Vec<String> = (0..12).map(|i| format!("/opt/many-{i}")).collect();
        for (text, granted) in [
            ("not json".to_owned(), vec![]),
            (
                json!({"schema": 2, "programs": ["/opt/x"]}).to_string(),
                vec![],
            ),
            (json!({"programs": ["/opt/x"]}).to_string(), vec![]),
            (
                json!({"schema": 1, "programs": ["", "/opt/a\u{7}b", 5, "/opt/ok"]}).to_string(),
                vec!["/opt/ok".to_owned()],
            ),
            (
                json!({"schema": 1, "programs": ["/opt/x"], "pad": "p".repeat(64 * 1024)})
                    .to_string(),
                vec![],
            ),
            (
                json!({"schema": 1, "programs": many}).to_string(),
                many[..RECORDED_PROGRAMS].to_vec(),
            ),
        ] {
            fs::write(&record, &text).unwrap();
            assert_eq!(
                recorded_at(&record),
                granted,
                "{}",
                &text[..text.len().min(80)]
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn recorded_programs_stay_hgs_and_unrecorded_look_alikes_are_flagged() {
        for agent in ["claude", "codex"] {
            let (text, _) = launches(agent, 1);
            // A plain environment that recorded nothing: the wrapper's entries
            // are not hgs's, and the report says so without naming them.
            let plain =
                health_document_mode(&text, agent, Some(CLAUDE_ANSWER_CHANNEL), false).unwrap();
            assert_ne!(plain["state"], "configured", "{agent}");
            assert_eq!(plain["unrecognised_tracking"], true, "{agent}");
            assert!(!plain.to_string().contains(WRAPPER), "{agent}");
            // The same text once the wrapper is recorded for this file.
            let _own = OwnProgram::set(vec!["/usr/local/bin/hgs".into(), WRAPPER.into()]);
            let recorded =
                health_document_mode(&text, agent, Some(CLAUDE_ANSWER_CHANNEL), false).unwrap();
            assert_eq!(recorded["state"], "configured", "{agent}");
            assert_eq!(recorded["unrecognised_tracking"], false, "{agent}");
            assert!(
                json_reuse(&text, agent, false).is_none(),
                "{agent}: a recorded entry is reused"
            );
        }
        let kimi = {
            let _own = OwnProgram::scoped(WRAPPER);
            kimi_merge("model = \"k\"\n", false, &fresh_command(WRAPPER).unwrap()).unwrap()
        };
        let plain = health_document_mode(&kimi, "kimi", None, false).unwrap();
        assert_eq!(plain["unrecognised_tracking"], true);
        assert!(!plain.to_string().contains(WRAPPER));
        let _own = OwnProgram::set(vec![WRAPPER.into()]);
        let recorded = health_document_mode(&kimi, "kimi", None, false).unwrap();
        assert_eq!(recorded["state"], "configured");
        assert_eq!(recorded["unrecognised_tracking"], false);
    }

    #[test]
    fn personal_hooks_and_hgs_named_entries_are_never_flagged() {
        let hooks = [
            "/opt/old/hgs-wrapper --notify done",
            "/opt/old/hgs-wrapper __state hooks",
            "__state hook",
            "/opt/other-tool __state hook",
            "/opt/other-tool __state answer-channel",
            "/usr/local/bin/hgs __state hook",
            "'/opt/hgs dir/hgs' __state answer-channel",
            "python3 /opt/old/hgs_state.py hook",
        ];
        let document = json!({"hooks": {"Stop": [{"hooks": hooks.map(|command| json!({"type": "command", "command": command}))}]}}).to_string();
        assert_eq!(
            health_document_mode(&document, "claude", None, false).unwrap()
                ["unrecognised_tracking"],
            false
        );
        let kimi: String = hooks
            .iter()
            .map(|command| {
                format!(
                    "[[hooks]]\nevent = \"Stop\"\ncommand = {}\n",
                    toml_edit::Value::from(*command)
                )
            })
            .collect();
        assert_eq!(
            health_document_mode(&kimi, "kimi", None, false).unwrap()["unrecognised_tracking"],
            false
        );
        // Either form of an hgs-like command for another program is flagged.
        for hook in [
            json!({"type": "command", "command": "/opt/old/hgs-wrapper __state hook"}),
            json!({"type": "command", "command": "/opt/old/hgs-wrapper __state answer-channel"}),
            json!({"type": "command", "command": "/opt/old/hgs-wrapper", "args": ["__state", "hook"]}),
            json!({"type": "command", "command": "/opt/tools/myhgs __state hook"}),
        ] {
            let document = json!({"hooks": {"Stop": [{"hooks": [hook.clone()]}]}}).to_string();
            assert_eq!(
                health_document_mode(&document, "claude", None, false).unwrap()
                    ["unrecognised_tracking"],
                true,
                "{hook}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn only_an_owned_entry_running_the_exact_program_is_recorded() {
        let (text, _) = launches("claude", 1);
        let kimi = {
            let _own = OwnProgram::scoped(WRAPPER);
            kimi_merge("", false, &fresh_command(WRAPPER).unwrap()).unwrap()
        };
        let personal = json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": format!("{WRAPPER} --notify done")}]}]}}).to_string();
        let personal_kimi =
            format!("[[hooks]]\nevent = \"Stop\"\ncommand = \"{WRAPPER} notify\"\n");
        {
            let _own = OwnProgram::scoped(WRAPPER);
            assert!(holds_program(&text, "claude", WRAPPER));
            assert!(holds_program(&kimi, "kimi", WRAPPER));
            assert!(!holds_program(&text, "claude", "/opt/tools/other"));
            assert!(!holds_program(&kimi, "kimi", "/opt/tools/other"));
            assert!(
                !holds_program(&personal, "claude", WRAPPER),
                "a personal hook is never recorded"
            );
            assert!(!holds_program(&personal_kimi, "kimi", WRAPPER));
            assert!(!holds_program("not json", "claude", WRAPPER));
        }
        // Outside the scope that owns it the program is not hgs's: nothing to record.
        assert!(!holds_program(&text, "claude", WRAPPER));
        assert!(!holds_program(&kimi, "kimi", WRAPPER));
    }

    #[test]
    fn explicit_repair_refuses_beside_an_unrecorded_look_alike_without_writing() {
        // An explicit runner: a Windows test host has no installed stable CLI.
        let runner = if cfg!(windows) {
            r"C:\Tools\hgs.exe"
        } else {
            "/usr/local/bin/hgs"
        };
        let look_alike = json!({"model": "keep", "hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "/opt/old/hgs-wrapper __state hook", "timeout": 5}]}]}}).to_string();
        let error = repair_with(&look_alike, &path(), "claude", (2, 1, 296), runner).unwrap_err();
        assert!(error.contains("HGS_EXECUTABLE"), "{error}");
        assert!(!error.contains("/opt/old"), "{error}");
        let kimi = "[[hooks]]\nevent = \"Stop\"\ncommand = \"/opt/old/hgs-wrapper __state hook\"\n";
        let error = repair_with(kimi, &path(), "kimi", (2, 3, 1), runner).unwrap_err();
        assert!(error.contains("HGS_EXECUTABLE"), "{error}");
        // Without the look-alike the same repair goes ahead.
        assert!(repair_with(
            r#"{"model":"keep"}"#,
            &path(),
            "claude",
            (2, 1, 296),
            runner
        )
        .unwrap()
        .is_some());
    }

    /// Repair reads health in its own scope: the entries the program it
    /// writes wrote are hgs's, whatever runs the test.
    #[cfg(unix)]
    #[test]
    fn explicit_repair_owns_the_program_it_writes() {
        for agent in ["claude", "codex"] {
            let (text, _) = launches(agent, 1);
            assert_eq!(
                repair_with(&text, &path(), agent, CLAUDE_ANSWER_CHANNEL, WRAPPER),
                Ok(None),
                "{agent}"
            );
        }
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
        let health = health_document(&merged, &path(), "kimi", Some((2, 3, 1))).unwrap();
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

    /// CodeLore's pre-edit hook runs only for a Claude file edit in a session
    /// whose start chose a store.
    #[test]
    fn only_a_claude_file_edit_asks_lores_pre_edit_hook() {
        let record = json!({"lore_edit_store":"D:/repo"});
        let edit = json!({"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"D:/repo/a.rs"}});
        assert_eq!(edit_store(&record, &edit, "claude"), Some(PathBuf::from("D:/repo")));
        for tool in ["Write", "MultiEdit", "NotebookEdit"] {
            assert!(edit_store(&record, &json!({"hook_event_name":"PreToolUse","tool_name":tool}), "claude").is_some(), "{tool}");
        }
        assert_eq!(edit_store(&record, &json!({"hook_event_name":"PreToolUse","tool_name":"Bash"}), "claude"), None);
        assert_eq!(edit_store(&record, &json!({"hook_event_name":"PostToolUse","tool_name":"Edit"}), "claude"), None);
        assert_eq!(edit_store(&record, &edit, "codex"), None);
        assert_eq!(edit_store(&json!({}), &edit, "claude"), None);
    }

    /// Task 135: a team member's session that starts after a compaction or a
    /// clear gets the handoff brief in recall form, within what the start
    /// context leaves; a plain start, a subagent's start and a full context get
    /// none, and a session outside every team gets none.
    #[test]
    fn a_compacted_team_session_starts_with_the_teams_recall() {
        use crate::state::teams::{model::Team, ops, store::Store};
        let dir = tempfile::tempdir().unwrap();
        super::super::with_test_state_root(dir.path(), || {
            let (mut team, _) = ops::create(
                "t",
                &json!({"name":"Recall","goal":"Ship the recall","lead":{"id":"lead","agent":"claude","session":{"name":"claude/x/lead","run_id":"r0","conversation_id":"c0"}}}),
            )
            .unwrap();
            let mut events = Vec::new();
            ops::apply(
                &mut team,
                "plan",
                &json!({"members":[{"id":"dev","role":"Backend","agent":"claude","session":{"name":"claude/x/dev","run_id":"run-dev","conversation_id":"c1"}}],
                    "tasks":[{"id":1,"title":"Recall task","assignee":"dev","pipeline":"none","description":"Make the recall work"}]}),
                &ops::Actor::Human,
                &std::collections::BTreeSet::new(),
                &mut events,
            )
            .unwrap();
            ops::apply(&mut team, "approve", &json!({}), &ops::Actor::Human, &std::collections::BTreeSet::new(), &mut events).unwrap();
            let team: Team = team;
            let mut store = Store::open(true).unwrap();
            store.create("11111111-1111-4111-8111-111111111136", &json!({}), team, vec![]).unwrap();
            let record = json!({"agent":"claude","name":"claude/x/dev","run_id":"run-dev","conversation_id":"c1"});
            let compact = json!({"hook_event_name":"SessionStart","source":"compact"});
            let context = with_team_recall(None, None, &record, &compact).expect("a compacted team session gets the recall");
            for wanted in ["handoff brief", "You are Backend in the team Recall", "your context was compacted or cleared", "Goal: Ship the recall", "Your work: #1 Recall task", "Make the recall work", "hgs team current"] {
                assert!(context.contains(wanted), "{wanted:?} missing in\n{context}");
            }
            assert!(context.encode_utf16().count() <= crate::state::teams::handoff::RECALL_UNITS);
            // Joined with what the start context already holds, never replacing it.
            let joined = with_team_recall(Some("Agent hint".into()), None, &record, &json!({"hook_event_name":"SessionStart","source":"clear"})).unwrap();
            assert!(joined.starts_with("Agent hint\n\n") && joined.contains("handoff brief"), "{joined}");
            // A plain start, a resume, a subagent and another event get none.
            for event in [
                json!({"hook_event_name":"SessionStart","source":"startup"}),
                json!({"hook_event_name":"SessionStart","source":"resume"}),
                json!({"hook_event_name":"SessionStart","source":"compact","agent_id":"child"}),
                json!({"hook_event_name":"UserPromptSubmit","source":"compact"}),
            ] {
                assert_eq!(with_team_recall(Some("keep".into()), None, &record, &event).as_deref(), Some("keep"), "{event}");
            }
            // A full start context leaves no room: the rules keep theirs.
            let full = "x".repeat(super::super::iron_rules::MAX_START_CONTEXT - 200);
            assert_eq!(with_team_recall(Some(full.clone()), None, &record, &compact), Some(full));
            // A session that belongs to no team gets none.
            let stranger = json!({"agent":"claude","name":"claude/x/other","run_id":"run-other","conversation_id":"c9"});
            assert_eq!(with_team_recall(None, None, &stranger, &compact), None);
        });
    }

    /// Task 121: the heartbeat hook is a fast append. While another delivery
    /// holds the state lock (a slow store), the heartbeat still finishes at
    /// once and its time reaches the record on the next read.
    #[test]
    fn a_heartbeat_never_waits_on_the_state_lock() {
        let _env = super::super::ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let dir = std::env::temp_dir().join(format!("hgs-heartbeat-hook-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HGS_STATE_DIR", &dir);
        std::env::set_var("HGS_SESSION", "kimi/p/lead");
        std::env::set_var("HGS_RUN_ID", "run-heartbeat");
        super::super::storage::set_test_root(None);
        let held = lock(None).unwrap();
        let started = std::time::Instant::now();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let event = json!({"hook_event_name":"SessionHeartbeat","session_id":"s","agent_id":"main"});
            let result = track("kimi/p/lead".into(), "run-heartbeat".into(), event, &mut output);
            let _ = sender.send(result);
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the heartbeat hook waited on the state lock");
        assert_eq!(result, Ok(0));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        drop(held);
        let beat = super::super::storage::root().join("heartbeats").join("run-heartbeat.json");
        let beat: Value = serde_json::from_slice(&fs::read(&beat).unwrap()).unwrap();
        assert_eq!(beat["run_id"], "run-heartbeat");
        // The next read folds it into the run's record, as the old write did.
        let mut record = json!({"run_id":"run-heartbeat","last_event_at":1.0});
        super::super::heartbeat::apply(&mut record);
        assert!(record["last_event_at"].as_f64().unwrap() > 1.0);
        std::env::remove_var("HGS_STATE_DIR");
        std::env::remove_var("HGS_SESSION");
        std::env::remove_var("HGS_RUN_ID");
        let _ = fs::remove_dir_all(&dir);
    }
}
