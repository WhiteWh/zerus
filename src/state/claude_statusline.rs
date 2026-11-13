//! Claude Code's context-window capacity from its native status line.
//!
//! Claude Code passes a JSON description of the session to the `statusLine`
//! command on stdin each time its status line updates. The reviewed 2.1.296
//! input names the session (`session_id`), the model it runs now (`model.id`)
//! and that model's window (`context_window.context_window_size`); its
//! transcript and hooks never report the window. For a tracked run hgs passes
//! its own status line command through `--settings` (Claude's flag settings,
//! which no file holds). That command keeps the latest sample for the run and
//! then runs the status line the user configured, passing its output and exit
//! status through, so the user's status line stays as it was. hgs never
//! derives a capacity from a model name.
use super::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;

/// The oldest Claude Code whose status line input hgs has verified.
const VERIFIED: (u64, u64, u64) = (2, 1, 296);
/// Where samples live, below the state directory: one file per run.
const DIRECTORY: &str = "context-telemetry";
/// Samples of runs that ended this long ago are removed.
const RETAIN_SECONDS: u64 = 30 * 24 * 3600;
/// A status line input is a few kilobytes.
const MAX_INPUT: u64 = 4 * 1024 * 1024;
/// The largest settings file read.
const MAX_SETTINGS: u64 = 1024 * 1024;
/// The fields of the user's status line that its command does not consume and
/// that Claude Code reads from the registered entry instead.
const PRESENTATION: &[&str] = &["padding", "refreshInterval", "hideVimModeIndicator"];

fn number(value: &Value) -> Option<u64> {
    value.as_u64().filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
}

fn safe_model(value: &str) -> bool {
    !value.is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
}

/// Claude Code's configuration folder, as its `CLAUDE_CONFIG_DIR` names it.
fn config_dir() -> PathBuf {
    nonempty_env("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"))
}

/// A settings file as Claude Code reads it: `Ok(None)` when it is missing,
/// an error when it cannot be read or is not a JSON object.
fn settings_file(path: &Path) -> std::result::Result<Option<Value>, ()> {
    use std::io::Read;
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > MAX_SETTINGS {
        return Err(());
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    value.is_object().then_some(Some(value)).ok_or(())
}

/// The repository root that Claude Code may keep the local settings of
/// `folder` in: the nearest folder with a `.git` entry or, for a linked
/// worktree, the repository that its `commondir` names, as Claude 2.1.296
/// canonicalizes it. The worktree is not the root then.
fn git_root(folder: &Path) -> Option<PathBuf> {
    let nearest = folder
        .ancestors()
        .find(|path| path.join(".git").exists())?;
    Some(worktree_repository(nearest).unwrap_or_else(|| nearest.to_owned()))
}

/// The repository of the linked worktree `root`, whose `.git` file names
/// its folder below the repository's `worktrees`. Read leniently: a root found
/// where Claude would keep the worktree only makes hgs leave the status line
/// to Claude.
fn worktree_repository(root: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(root.join(".git")).ok()?;
    let gitdir = root.join(text.trim().strip_prefix("gitdir:")?.trim());
    let common = fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = crate::platform::canonical(gitdir.join(common.trim())).ok()?;
    if common.file_name()? == ".git" {
        common.parent().map(Path::to_path_buf)
    } else {
        Some(common)
    }
}

/// The status line that the user, project and local settings of `project`
/// give Claude Code, which the flag settings of a tracked run hide: the user
/// file of `config` (or Claude's cowork file when `cowork`), then
/// `.claude/settings.json` and `.claude/settings.local.json` in `project`,
/// each later file's fields over the earlier ones. `Ok(None)` when none sets
/// one. An error when a file cannot be read, a status line is not a command,
/// or local settings set one while the folder lies below another repository
/// root (an enclosing repository, or the repository of a linked worktree):
/// Claude then reads local settings either at that root or in the folder, by
/// ownership checks that hgs does not repeat.
fn user_status_line(
    config: &Path,
    project: &Path,
    cowork: bool,
) -> std::result::Result<Option<Value>, ()> {
    // Claude Code keeps local settings in the folder when the repository's
    // root is the folder itself or the home folder.
    let real = |path: &Path| crate::platform::canonical(path).unwrap_or_else(|_| path.to_owned());
    let root = git_root(project)
        .filter(|root| real(root) != real(project) && real(root) != real(&home()));
    let user = config.join(if cowork { "cowork_settings.json" } else { "settings.json" });
    let claude = project.join(".claude");
    let local = claude.join("settings.local.json");
    let mut merged: Option<serde_json::Map<String, Value>> = None;
    for path in [user, claude.join("settings.json"), local.clone()] {
        let Some(settings) = settings_file(&path)? else {
            continue;
        };
        let Some(line) = settings.get("statusLine") else {
            continue;
        };
        if root.is_some() && path == local {
            return Err(());
        }
        let line = line.as_object().ok_or(())?;
        merged.get_or_insert_with(Default::default).extend(line.clone());
    }
    if let Some(root) = root {
        let local = settings_file(&root.join(".claude/settings.local.json"))?;
        if local.is_some_and(|settings| settings.get("statusLine").is_some()) {
            return Err(());
        }
    }
    let Some(line) = merged else {
        return Ok(None);
    };
    let command = line.get("command").and_then(Value::as_str).unwrap_or("");
    if line.get("type").and_then(Value::as_str) != Some("command") || command.trim().is_empty() {
        return Err(());
    }
    Ok(Some(Value::Object(line)))
}

fn cowork() -> bool {
    std::env::var_os("CLAUDE_CODE_USE_COWORK_PLUGINS").is_some_and(|value| !value.is_empty())
}

/// Whether managed (policy) settings may apply. They rank above flag
/// settings and may restrict which settings files Claude Code reads, so a
/// composed status line could differ from the one Claude would run.
fn managed_settings() -> bool {
    if std::env::var_os("CLAUDE_CODE_MANAGED_SETTINGS_PATH").is_some() {
        return true;
    }
    let folder = Path::new(if cfg!(target_os = "macos") {
        "/Library/Application Support/ClaudeCode"
    } else {
        "/etc/claude-code"
    });
    ["managed-settings.json", "managed-settings.d"]
        .iter()
        .any(|name| fs::symlink_metadata(folder.join(name)).is_ok())
}

/// The options of Claude Code 2.1.296 that choose its flag settings, its
/// settings sources, the folder of its project and local settings
/// (`--project-config-root`, or a new worktree with `--worktree`) or policy
/// settings (`--managed-settings`).
const SETTINGS_OPTIONS: &[&str] = &[
    "--settings",
    "--setting-sources",
    "--project-config-root",
    "--managed-settings",
    "--worktree",
];

/// Whether `argv` already chooses which settings Claude Code reads: then hgs
/// adds no settings of its own.
fn chooses_settings(argv: &[String]) -> bool {
    let end = argv.iter().position(|arg| arg == "--").unwrap_or(argv.len());
    argv[..end].iter().any(|arg| {
        let key = arg.split('=').next().unwrap_or(arg);
        // `-w`, Claude's short `--worktree`, may carry its name attached.
        SETTINGS_OPTIONS.contains(&key) || arg.starts_with("-w")
    })
}

/// The flag settings that register hgs's status line for a tracked Claude
/// Code run of the native CLI `program`, started in `project`, or `None` when
/// this run keeps Claude's own: on Windows, for a project launcher, a version
/// older than the verified one or one that cannot be read, `argv` that choose
/// settings themselves, possible managed settings, and a user status line
/// that hgs cannot resolve exactly. The user's presentation fields stay.
pub(super) fn launch_settings(program: &str, project: &Path, argvs: &[&Vec<String>]) -> Option<String> {
    if !cfg!(unix) || argvs.iter().any(|argv| chooses_settings(argv)) || managed_settings() {
        return None;
    }
    let user = user_status_line(&config_dir(), project, cowork()).ok()?;
    if hooks::claude_version(program).is_none_or(|version| version < VERIFIED) {
        return None;
    }
    let command = join_command(&[
        runner_executable().ok()?,
        "__state".into(),
        "statusline".into(),
    ]);
    let mut line = json!({"type": "command", "command": command});
    if let Some(user) = user {
        for key in PRESENTATION {
            if let Some(value) = user.get(*key) {
                line[*key] = value.clone();
            }
        }
    }
    Some(json!({"statusLine": line}).to_string())
}

/// `argv` with the flag `settings` before its `--`, if any.
pub(super) fn add_settings(argv: &mut Vec<String>, settings: &str) {
    if argv.is_empty() {
        return;
    }
    let at = argv.iter().position(|arg| arg == "--").unwrap_or(argv.len());
    argv.splice(at..at, ["--settings".to_owned(), settings.to_owned()]);
}

/// The sample file of the run `run_id` in `folder`, the samples' folder.
fn sample_path(folder: &Path, run_id: &str) -> PathBuf {
    folder.join(format!("{:x}.json", Sha256::digest(run_id.as_bytes())))
}

/// The sample that `input`, one status line input of the run `run_id`,
/// gives: its conversation, model and that model's window, or `None` when it
/// lacks one of them.
fn sample(input: &Value, run_id: &str, observed_at: f64) -> Option<Value> {
    let conversation = input["session_id"].as_str()?;
    uuid::Uuid::parse_str(conversation).ok()?;
    let model = input["model"]["id"].as_str().filter(|model| safe_model(model))?;
    let size = number(&input["context_window"]["context_window_size"])?;
    let version = input["version"].as_str().filter(|v| v.len() <= 64 && !v.chars().any(char::is_control));
    Some(json!({"version": 1, "source": "native_statusline", "run_id": run_id,
        "conversation_id": conversation, "model": model, "context_window_size": size,
        "observed_at": observed_at, "agent_version": version}))
}

/// Keep `input`'s sample for this run, unless a later invocation already
/// kept a newer one. A first sample also removes those of long-ended runs.
fn record(input: &Value, observed_at: f64) -> Result<()> {
    let (Some(run_id), Some(_)) = (nonempty_env("HGS_RUN_ID"), nonempty_env("HGS_STATE_DIR")) else {
        return Ok(());
    };
    let Some(sample) = sample(input, &run_id, observed_at) else {
        return Ok(());
    };
    let folder = root().join(DIRECTORY);
    let path = sample_path(&folder, &run_id);
    let _guard = lock(Some(&folder.join(".lock")))?;
    let previous = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    if previous
        .as_ref()
        .and_then(|previous| previous["observed_at"].as_f64())
        .is_some_and(|at| at > observed_at)
    {
        return Ok(());
    }
    // Samples are refreshed at each update and rebuilt by the next one, so
    // they are replaced atomically without forcing them to disk.
    let mut temp = tempfile::Builder::new()
        .prefix(".hgs-")
        .tempfile_in(&folder)
        .map_err(|e| e.to_string())?;
    temp.write_all(sample.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    crate::platform::persist(temp, &path).map_err(|e| e.to_string())?;
    if previous.is_none() {
        prune(&folder);
    }
    Ok(())
}

fn prune(folder: &Path) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age.as_secs() > RETAIN_SECONDS);
        if old && name.to_string_lossy().ends_with(".json") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// `hgs __state statusline`, the status line command of a tracked Claude Code
/// run: keep the sample of Claude's input, then run the user's status line
/// with the same input as Claude would (`/bin/sh -c`, Claude's environment
/// and folder) and pass its output and exit status through. Without a user
/// status line it prints nothing. Telemetry never changes what is shown.
pub(super) fn run() -> Result<i32> {
    use std::io::Read;
    let observed_at = now();
    let mut input = Vec::new();
    let complete = io::stdin()
        .take(MAX_INPUT + 1)
        .read_to_end(&mut input)
        .is_ok_and(|read| read as u64 <= MAX_INPUT);
    let parsed = complete
        .then(|| serde_json::from_slice::<Value>(crate::state::without_bom(&input)).ok())
        .flatten();
    if let Some(parsed) = &parsed {
        let _ = record(parsed, observed_at);
    }
    let project = parsed
        .as_ref()
        .and_then(|value| value["workspace"]["project_dir"].as_str())
        .filter(|path| Path::new(path).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let line = project.and_then(|project| user_status_line(&config_dir(), &project, cowork()).ok().flatten());
    let Some(line) = line else {
        return Ok(0);
    };
    compose(string(&line, "command"), &input)
}

/// Run the user's status line `command` with `input` on its stdin; its
/// output and errors go straight to Claude Code.
fn compose(command: &str, input: &[u8]) -> Result<i32> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        // A command that does not read its input closes the pipe early.
        let _ = stdin.write_all(input);
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    Ok(status.code().unwrap_or(1))
}

/// The window that this run's status line reported for the session of
/// `record`, which runs `model` (empty when not known), as the fields of its
/// usage `context`: `None` when its latest sample belongs to another run,
/// conversation or model, so a stale sample never describes the session.
pub(super) fn capacity(record: &Value, model: &str) -> Option<Value> {
    capacity_in(&root().join(DIRECTORY), record, model)
}

fn capacity_in(folder: &Path, record: &Value, model: &str) -> Option<Value> {
    use std::io::Read;
    let run_id = string(record, "run_id");
    let conversation = string(record, "conversation_id");
    if string(record, "agent") != "claude" || run_id.is_empty() || conversation.is_empty() {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(sample_path(folder, run_id))
        .ok()?
        .take(64 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    let sample: Value = serde_json::from_slice(&bytes).ok()?;
    let size = number(&sample["context_window_size"])?;
    let sampled = string(&sample, "model");
    if sample["version"] != 1
        || string(&sample, "run_id") != run_id
        || string(&sample, "conversation_id") != conversation
        || !safe_model(sampled)
        || (!model.is_empty() && model != sampled)
    {
        return None;
    }
    Some(json!({"limit": size, "limit_source": "native_statusline", "limit_model": sampled,
        "limit_observed_at": sample["observed_at"]}))
}

/// Add the reported window of `record`'s session to its `usage`.
pub(super) fn apply(record: &Value, model: &str, usage: &mut Value) {
    add(capacity(record, model), usage);
}

fn add(fields: Option<Value>, usage: &mut Value) {
    let Some(fields) = fields else {
        return;
    };
    if !usage["context"].is_object() {
        usage["context"] = json!({});
    }
    for (key, value) in fields.as_object().unwrap() {
        usage["context"][key] = value.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, value: &Value) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value.to_string()).unwrap();
    }

    #[test]
    fn user_status_line_follows_claude_precedence_and_refuses_what_it_cannot_resolve() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        let project = temp.path().join("project");
        // The project is its own repository root, so the climb for an
        // enclosing repository stops at it wherever the temporary folder
        // lies, inside another checkout too (zerus-yzoh1).
        fs::create_dir_all(project.join(".git")).unwrap();
        assert_eq!(user_status_line(&config, &project, false), Ok(None));
        let user = json!({"type": "command", "command": "user-line", "padding": 2});
        write(&config.join("settings.json"), &json!({"model": "opus", "statusLine": user}));
        assert_eq!(user_status_line(&config, &project, false), Ok(Some(user.clone())));
        // Cowork reads another user file.
        assert_eq!(user_status_line(&config, &project, true), Ok(None));
        // Project, then local settings, override field by field.
        write(&project.join(".claude/settings.json"), &json!({"statusLine": {"command": "project-line"}}));
        write(&project.join(".claude/settings.local.json"), &json!({"statusLine": {"refreshInterval": 5}}));
        assert_eq!(
            user_status_line(&config, &project, false),
            Ok(Some(json!({"type": "command", "command": "project-line", "padding": 2, "refreshInterval": 5})))
        );
        // A file Claude cannot read either, or a status line that is no command.
        fs::write(project.join(".claude/settings.local.json"), "{").unwrap();
        assert_eq!(user_status_line(&config, &project, false), Err(()));
        write(&project.join(".claude/settings.local.json"), &json!({"statusLine": "text"}));
        assert_eq!(user_status_line(&config, &project, false), Err(()));
        write(&project.join(".claude/settings.local.json"), &json!({"statusLine": {"type": "other"}}));
        assert_eq!(user_status_line(&config, &project, false), Err(()));
        fs::remove_file(project.join(".claude/settings.local.json")).unwrap();
        // Local settings of an enclosing repository are not resolved.
        let nested = project.join("nested");
        fs::create_dir_all(nested.join(".claude")).unwrap();
        assert_eq!(user_status_line(&config, &nested, false), Ok(Some(user.clone())));
        write(&project.join(".claude/settings.local.json"), &json!({"theme": "dark"}));
        assert_eq!(user_status_line(&config, &nested, false), Ok(Some(user.clone())));
        write(&project.join(".claude/settings.local.json"), &json!({"statusLine": {"command": "x"}}));
        assert_eq!(user_status_line(&config, &nested, false), Err(()));
        // Nor the folder's own, which Claude reads instead when those checks fail.
        fs::remove_file(project.join(".claude/settings.local.json")).unwrap();
        write(&nested.join(".claude/settings.local.json"), &json!({"statusLine": {"command": "x"}}));
        assert_eq!(user_status_line(&config, &nested, false), Err(()));
        // A linked worktree's root is its repository, wherever the worktree lies.
        let repository = temp.path().join("repository");
        let admin = repository.join(".git/worktrees/linked");
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("commondir"), "../..\n").unwrap();
        let linked = temp.path().join("linked");
        fs::create_dir_all(&linked).unwrap();
        fs::write(linked.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        // Compared canonically: Windows canonical paths carry a verbatim prefix.
        assert_eq!(
            git_root(&linked.join("src")).map(|root| fs::canonicalize(root).unwrap()),
            Some(fs::canonicalize(&repository).unwrap())
        );
        assert_eq!(git_root(&nested), Some(project.clone()));
        assert_eq!(user_status_line(&config, &linked, false), Ok(Some(user.clone())));
        write(&repository.join(".claude/settings.local.json"), &json!({"statusLine": {"command": "x"}}));
        assert_eq!(user_status_line(&config, &linked, false), Err(()));
    }

    #[test]
    fn flag_settings_go_before_the_prompt_and_never_replace_the_users() {
        let mut argv: Vec<String> = ["claude", "--model", "opus", "--", "--settings"].map(String::from).to_vec();
        assert!(!chooses_settings(&argv));
        add_settings(&mut argv, "{}");
        assert_eq!(argv, ["claude", "--model", "opus", "--settings", "{}", "--", "--settings"]);
        assert!(chooses_settings(&argv));
        for flag in ["--settings=/tmp/x.json", "--setting-sources", "--setting-sources=user", "--project-config-root",
            "--project-config-root=/srv/project", "--managed-settings", "--worktree", "--worktree=name", "-w", "-wname"] {
            assert!(chooses_settings(&["claude".to_owned(), flag.to_owned()]));
        }
        let mut empty = Vec::new();
        add_settings(&mut empty, "{}");
        assert!(empty.is_empty());
    }

    #[test]
    fn samples_are_scoped_to_run_conversation_and_model() {
        let conversation = "11111111-2222-4333-8444-555555555555";
        let input = json!({"session_id": conversation, "version": "2.1.296",
            "model": {"id": "claude-sonnet-5-5[1m]", "display_name": "Sonnet 5.5"},
            "context_window": {"context_window_size": 1000000, "total_input_tokens": 100}});
        let value = sample(&input, "run-one", 10.0).unwrap();
        assert_eq!(value["context_window_size"], 1000000);
        assert_eq!(value["model"], "claude-sonnet-5-5[1m]");
        // Nothing is kept without the window, the model or a conversation ID.
        for (pointer, bad) in [("/context_window/context_window_size", json!(0)),
            ("/context_window/context_window_size", json!(-1)), ("/context_window/context_window_size", json!("200000")),
            ("/model/id", json!("")), ("/model/id", json!("a\nb")), ("/session_id", json!("not-a-uuid"))] {
            let mut changed = input.clone();
            *changed.pointer_mut(pointer).unwrap() = bad;
            assert!(sample(&changed, "run-one", 10.0).is_none(), "{pointer}");
        }
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path();
        write(&sample_path(folder, "run-one"), &value);
        let capacity = |record: &Value, model: &str| capacity_in(folder, record, model);
        let record = json!({"agent": "claude", "run_id": "run-one", "conversation_id": conversation});
        let expected = json!({"limit": 1000000, "limit_source": "native_statusline",
            "limit_model": "claude-sonnet-5-5[1m]", "limit_observed_at": 10.0});
        assert_eq!(capacity(&record, ""), Some(expected.clone()));
        assert_eq!(capacity(&record, "claude-sonnet-5-5[1m]"), Some(expected));
        // Another model, conversation, run or agent: stale for this session.
        assert_eq!(capacity(&record, "claude-sonnet-5-5"), None);
        for (key, other) in [("conversation_id", "99999999-2222-4333-8444-555555555555"), ("run_id", "run-two"), ("agent", "codex")] {
            let mut changed = record.clone();
            changed[key] = json!(other);
            assert_eq!(capacity(&changed, ""), None, "{key}");
        }
        let mut usage = json!({"status": "ok", "context": {"used": 100}});
        add(capacity(&record, ""), &mut usage);
        assert_eq!(usage["context"]["used"], 100);
        assert_eq!(usage["context"]["limit"], 1000000);
        let mut unavailable = json!({"status": "unavailable", "source": "claude"});
        add(capacity(&record, ""), &mut unavailable);
        assert_eq!(unavailable["context"]["limit_source"], "native_statusline");
        let mut fresh = json!({"status": "ok"});
        add(capacity(&record, "claude-haiku-5-5"), &mut fresh);
        assert_eq!(fresh, json!({"status": "ok"}));
        // A sample in another format is not read.
        let mut other = value.clone();
        other["version"] = json!(2);
        write(&sample_path(folder, "run-one"), &other);
        assert_eq!(capacity(&record, ""), None);
    }
}
