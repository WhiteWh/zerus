//! Public hgs CLI. tmux remains the terminal supervisor; durable agent identity belongs to state.
use crate::{
    config::{self, Config},
    platform::{self, TabGuard},
    state::{
        self,
        backend::{DryRun, LaunchSpec, PaneField, Target, TerminalBackend},
    },
};
use serde_json::{json, Value};
#[cfg(unix)]
use std::process::Command;
use std::{
    borrow::Cow,
    env, fmt, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::Instant,
};

#[derive(Debug)]
pub struct Error {
    pub code: i32,
    pub message: String,
}
impl Error {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::new(1, error.to_string())
    }
}
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) const USAGE: &str = r#"usage: hgs [@host] <cmd> [project] [-c [ID]] [-n tag] [-d] [--rc] [--dry-run] [-- args...]
       hgs external register|checkpoint|handoff|inbox|ack|list --json   cooperative external chats (external-workstream-v1)
       hgs path-ref describe|validate|map   explicit lexical identity (JSON stdin)
       hgs mobile-peers --json        guarded direct-peer discovery (mobile-peer ABI 1)
       hgs approval-log begin|finish|list|detail  local automatic-approval journal (JSON stdin)
       hgs desktop serve|status [--json]   per-user background desktop service
       hgs desktop revoke --json          revoke desktop writers before Qt rollback
       hgs desktop watch|request --json    private local desktop-v1 IPC
       hgs desktop feed --json             passive owner source data, no reader launch
       hgs desktop registration --json    preview this user's passive service definition
       hgs desktop registration --json --background    preview Unix background definition
       hgs desktop registration mode-check --json   verify an interactive mode-switch caller
       hgs desktop revoke-hold --json --request-id UUID   retain original rollback exclusion until stdin EOF
       hgs desktop registration install|remove|status --json   Windows interactive-user logon task
       hgs desktop notification-registration install|status --json   Windows Toast registration
       hgs desktop notification-registration remove --json --request-id UUID   after exact settled revoke
       hgs desktop import-evidence --json private staged migration input on stdin
       hgs desktop import-status --json --import-id UUID  read one import manifest
       Desktop service ABI: desktop-v1
       Desktop approval audit ABI: desktop-approval-audit-v1
       Desktop frontend request ABI: desktop-ui-request-v1
       Desktop ownership ABI: desktop-ownership-v1
       Notification controls ABI: desktop-notification-controls-v1
       Desktop creation clone ABI: desktop-creation-clone-v1
       Desktop pending read import ABI: desktop-pending-reads-v1
       Desktop activation ABI: desktop-activation-v1
       Desktop maintenance ABI: desktop-maintenance-v1
       Desktop cache maintenance ABI: desktop-cache-maintenance-v1
       Desktop startup ABI: desktop-startup-v1
       Desktop logon registration ABI: desktop-logon-task-v1
       Desktop Toast registration ABI: desktop-notification-registration-v1
        Desktop revoke ABI: desktop-revoke-v1
        Desktop revoke hold ABI: desktop-revoke-hold-v1
        Desktop import ABI: desktop-import-v1
        Desktop source feed ABI: desktop-source-feed-v1
        Desktop owner ABI: desktop-delegation-v1
        Desktop catalog ABI: desktop-catalog-v1
        Desktop catalog changes ABI: desktop-catalog-changes-v1
        Desktop catalog relay ABI: desktop-catalog-relay-v1
       hgs mobile-peer --json         guarded direct-peer native request from JSON stdin
       hgs [@host] voice <session> --json  send and inspect persistent voice messages (JSON stdin; voice-video-v1)
       hgs [@host] speech --json      host speech providers and durable jobs (JSON stdin)
       hgs [@host] analytics --json   private bounded computer analytics (JSON stdin)
       hgs [@host] mobile phones|invite|revoke-phone <id> [--workspace ID --server URL] --json   phones paired through this mobile connection
       Speech profile management v1
       hgs [@host] a <session>        attach by full name (see hgs ls)
       hgs [@host] a <session> --existing [--run-id ID]   attach only to this live run
       hgs [@host] ls                 sessions here and on peers
       hgs ls --json --local          this box as JSON (for the tray; no ssh)
       hgs [@host] status [--json] [--local]   version, sessions and background services here and on peers
       hgs [@host] version            this hgs version (also --version)
       hgs [@host] hooks <session> --json   read scoped hook health (hooks-health-v1)
       hgs [@host] hooks --account ID [--repair] --json   inspect or explicitly repair handlers
       hgs [@host] inspect <session> [--archive <id>] [--after <cursor>]   activity as JSON
       hgs [@host] inspect <session> --read-observation   raw shared reads for a current journal-backed main session
       hgs [@host] reference <session> --json [--archive ID] [--agent ID]   copy a session reference
       hgs self [--json]              the calling agent's own session reference
       hgs [@host] guide [topic]      how agents message, wait for, launch and stop sessions and use tasks
       hgs inspect 'zerus://session?...'   resolve and inspect a referenced conversation
       hgs [@host] watch --json   long-lived NDJSON event stream (stdin control)
       Event stream ABI: watch-v1
       hgs [@host] processes <session> [--archive <id>]   observed shell commands as JSON
       hgs [@host] processes <session> --output ID | --stop ID --run RUN --conversation ID [--generation ID]
       hgs [@host] send <session> --json   submit message/attachments from JSON stdin
       hgs [@host] send-cancel <session> --json   cancel a message hgs still holds ({"request_id"})
       hgs [@host] recovery action --scoped-json exact waiting-job control
       hgs [@host] recovery team-override   a team's reset wait for one session (JSON stdin)
       hgs [@host] history <session> --json bounded exact public history
       hgs [@host] history-projection <session> --json retired and refreshed history rows
       hgs [@host] graph snapshot --json   bounded saved relationship metadata (partial coverage)
       hgs [@host] wait begin|ack|finish|cancel|list --json   explicit independent-agent waits (JSON stdin)
       hgs [@host] team OPERATION --json   agent teams: plan, run, report and decide tasks (JSON stdin)
       hgs [@host] rules get|set|migrate --json   owner-managed iron rules, iron-rules-v1 (JSON stdin)
       hgs run --json [--wait] | --list --json   a build Zerus tracks as this session's work (JSON stdin)
       hgs test config|map|affected|durations|coverage|tools …   the project's tests (.zerus/tests.toml; hgs guide tests)
       hgs test runner --json   a test runner's side (test-runner-v1): the queue's suite of an exact commit, isolated
       hgs [@host] terminal <session> --json scoped terminal screen/input; draft/submit_draft for the composer draft
       hgs [@host] AGENT DIRECTORY --new -n TAG -d --launch-id UUID
       hgs [@host] session-action <session> --json scoped native lifecycle action
       hgs session-action-status <session> --json read an original exact lifecycle receipt
       hgs @host --expect-machine ID session-action-status <session> --json
       lifecycle-receipt-status-v1
       hgs [@host] compact-context <session> --json compact the idle agent context using its native command
       hgs [@host] clear-context <session> --json   clear the idle agent context using its native command
       hgs [@host] interrupt <session> --json   interrupt the current turn, keep the session
       hgs [@host] attachment <session> --request ID --index N --conversation ID   read a sent attachment
       hgs [@host] file-read <session> --json   read a document on the conversation computer
       hgs @host download --path REMOTE_PATH --output LOCAL_FILE   download a copy through configured SSH or explicit WSL
       hgs [@host] answer <session> --json   answer an exact pending question from JSON stdin
       hgs [@host] read <session> --json   mark the current reply/attention read (JSON stdin)
       hgs [@host] notifications <session> --json   shared mute preference (session-notifications-v1)
       hgs [@host] fork <session> [-n name] [-d] [--archive ID]  branch into a new conversation
       hgs [@host] effort <session> --json   change this agent's effort from JSON stdin
       hgs [@host] settings <session> --json model and effort for this session (JSON stdin)
       hgs [@host] switch-account <session> --account ID --request-id UUID   move a running session to another account of its agent
       hgs [@host] recovery get | set | action   automatic recovery policy (JSON stdin for writes)
       hgs [@host] search --query <text> [--limit N]   search saved session contents
       hgs [@host] search --indexed --query <text> [--limit 1..100] [--cursor C]   content-search-query-v1
       hgs [@host] search --index-status | --index-step | --index-discover | --index-catchup   bounded index diagnostics (JSON stdin for step)
       hgs [@host] schedules <session> [--json]   native DeepSeek Schedule tasks (JSON stdin changes)
       hgs machine ls | set <alias> --json <profile> | remove <alias> | reset <alias>
       hgs machine resolve <alias>          effective SSH settings (no connection)
       hgs swarm get | preview PEER | join PEER | sync | worker   shared project catalog
       hgs swarm assign-launch --json   assign an exact launch to its selected project
       hgs swarm session-project --json  read an exact session's logical project choices
       hgs swarm assign-session --json   move only the exact session's project grouping
       hgs presence set --json        desktop idle/lock time from JSON stdin (written by the tray)
       hgs subagents-confirm get|set --json   ask before an agent starts subagents on this computer (JSON stdin for set)
       hgs session-upload-block get|set --json   block DeepSeek's session-log upload on this computer (JSON stdin for set)
       hgs sandbox-folder grant DIRECTORY --json   give this user the folder rights the DeepSeek sandbox needs
       hgs swarm preference get --json   shared phone notification timing
       hgs swarm preference set mobile_delivery VALUE --json   immediate|away|away_5|away_10|away_15|away_30
       hgs machine check <alias> | ssh <alias> [--directory PATH] | setup <alias> [--source <checkout>]
       hgs model-server list [--json] | status SERVER [--json] | up SERVER MODEL [--detach] | down SERVER MODEL [--yes] | logs SERVER [MODEL] [--lines N] | save --json | test --json   model servers on this computer (local private configuration)
       hgs [@host] account ls | add ID --provider AGENT --label NAME | rm ID | restore ID | login ID
       hgs [@host] account rename ID --label NAME | inspect ID [--refresh]
       hgs [@host] account default [ID]   show or select the default account for new sessions
       hgs [@host] account model-settings ID   inspect this account's native model catalog and launch defaults
       hgs [@host] account model-defaults ID --model MODEL [--effort LEVEL] [--revision REV]   defaults for new sessions
       hgs [@host] account set-key native-dsh --json   save API key from private JSON stdin
       hgs machine file-proof <alias> --json   verify a scoped WSL file before explicit local mapping (wsl-file-proof-v1)
       hgs [@host] account bind-existing native-opencode --json   preview and bind one native conversation (opencode-bind-existing-v1)
       hgs [@host] account start native-opencode   start the shared OpenCode service if none runs; inspection never starts it
       hgs [@host] account favorite native-opencode --model PROVIDER/MODEL --favorite on|off --request-id UUID   star an OpenCode model on that computer
       hgs [@host] account models native-opencode [--location DIR]   every OpenCode model of that computer and folder as JSON; never starts OpenCode
       hgs [@host] account install codex|claude|kimi|dsh   install an agent in this terminal
       hgs [@host] account inspect --session SESSION [--refresh]
       hgs account copy ID --from MACHINE --to MACHINE --as NEW_ID [--label NAME]
       hgs [@host] claude-app ls | resume ID [-n NAME] [-d] [--force]   continue a Claude desktop app session (JSON list)
       hgs [@host] socraticode status|start|stop|connect PATH [--qdrant native|docker] | install | install-qdrant   a folder's SocratiCode index (JSON; extension, cargo feature socraticode)
       hgs [@host] worktrees --path PATH [--refresh] [--json]   existing checkouts as JSON
       hgs [@host] git-status --path PATH --json   read-only checkout/publication status
       hgs [@host] git-diff --path PATH --json     read-only diff against HEAD (256 KiB)
       hgs [@host] worktrees create --path REPO --branch NAME --destination PATH [--base HEAD] [--clone] [--sandbox] [--json]
       hgs [@host] worktrees create ... --request-id UUID --receipt --json   durable creation, no replay
       hgs [@host] worktrees status|cancel --request-id UUID --json   original creation receipt
       Worktree creation ABI: worktree-requests-v1
       hgs [@host] worktrees review --path PATH [--worktree WORKTREE] [--json]   cleanup verdicts and fingerprints
       hgs [@host] worktrees policy get|set --json   worktree-maintenance-v1 (host-local cleanup rules)
       hgs [@host] worktrees sweep --json   one bounded automatic cleanup step
       hgs [@host] worktrees inspect --path WORKTREE --common-dir DIR [--file RELATIVE_PATH] [--json]   bounded read-only changes
       hgs [@host] worktrees remove --path WORKTREE --common-dir DIR --fingerprint HASH [--ignored-fingerprint HASH] [--discard-fingerprint HASH] [--preserve-archive-restore] [--delete-branch] [--json]
       Mobile worktree ABI: worktrees-v1 (catalog/create/verified project placement)
       hgs [@host] repository check|clone --url URL --parent PATH --name NAME --json
       hgs [@host] repository status --request-id UUID --json
       Repository acquisition ABI: repository-clone-v1
       Mobile repository acquisition ABI: repository-acquisition-v1
       Mobile Git status ABI: git-status-v1
       Mobile project assignment ABI: session-project-v1
       Mobile clear-context ABI: dismiss-prompt-v1
       session-notifications-v1
       hgs [@host] dirs [--hidden] [path]   directories as JSON (default: home)
       hgs [@host] dirs --create <path> [--request-id UUID]   create one folder (existing parent)
       hgs [@host] kill <session> [--archive <id>]
       hgs [@host] rename <session> <new-full-name> [--archive <id>]
       hgs [@host] archives --json           date-filtered archive pages (JSON stdin)
       hgs [@host] archive <session>          move a stopped conversation to Archive
       hgs [@host] pause <session>|--all       save and gracefully close idle agents
       hgs [@host] resume <session>|--all [-d] [--archive <id>]   restore exact conversations
       hgs [@host] resume <session> --fresh [-d] [--expected-run-id ID]   same settings, new conversation
       hgs notify [--bell] <session> [text]   event for the tray (no tray = no-op)
       hgs project ls [--json] | add <name> <dir> | set <name> <dir> | rm <name>
       hgs task list|get|add|edit|move|stage|target|start|send|handoff|cancel|report|comment|settings …   project tasks (JSON; agents read one line)
       hgs task exchange|digests --json                 the phone's task exchange (mobile)
       hgs task mobile-send|mobile-cancel|mobile-done --json   the phone's task actions (mobile)
       hgs clip                       PNG from this box's clipboard to stdout (rc 1: no image)
       hgs paste <session> <pane> [client]   the tmux C-v binding; not for hand use
  cmd      claude | codex | kimi (know --resume) | opencode (native API) | sh (login shell) | any command
  project  name from ~/.config/hgs/projects, a path, or omitted = current dir
  -c       resume the dir's last conversation (claude/kimi -c, codex resume --last)
  -c ID    resume this exact conversation UUID; --resume=ID is also accepted
  -n tag   separate session <cmd>/<project>/<tag>
  --account ID   use a named native agent profile for this session
  --permissions provider|bypass|native:<json>   this session's discovered native permission selection instead of the account's
  --local-model NAME   this session's model of a local Ollama account (claude)
  --model-server NAME   this session runs on a model server's API (needs --model MODEL; a new session)
  --model PROVIDER/MODEL   OpenCode native model for a new session
  --effort VARIANT   OpenCode effort; needs --model, one of that model's effort_options (omit for the model's default)
  --new    always create a separate session; generate a name unless -n is given
  -d       start detached (do not attach)      --fresh  explicitly replace a saved conversation
  --rc     claude --remote-control (phone)     --bare   ignore a project run_<cmd>.sh launcher
  --dry-run  print the tmux/ssh command
~/.config/hgs/config sets HGS_SELF (names that mean "this box") and HGS_PEERS
(the ssh aliases that hgs ls polls and @host hops to); environment variables win.
The terminal tab is renamed while attached (HGS_TAB=0 disables this); HGS_TAB_COLORS
sets per-box colours in Konsole. Configuration uses NAME=value assignments.
Ctrl+V in a session attached from a peer pulls the image from that box's clipboard.
--client <peer> identifies the attaching box; @host hops set it automatically.
Saved names resume their exact conversation; --fresh explicitly replaces the binding.
Launch options apply only when a session is CREATED; existing sessions attach as is.
Claude/Codex/Kimi install tracking hooks; Codex hooks need trust in /hooks.
pause refuses untracked/busy sessions. Saved sessions survive reboot; a opens them again.
Archive holds completed conversations; only explicit resume --archive restores them.
terminate stops a tracked session and preserves it in Archive; kill explicitly forgets it.
pause/resume --all covers tracked terminal agents; OpenCode uses exact per-session controls.
resume --all excludes Archive. kill --archive forgets only that archived entry.
rename changes the session label while preserving its agent/project, process and conversation.
"#;

/// Fails where this computer runs no local sessions (a Windows computer whose local
/// sessions are not enabled). Forwarded `@host` commands never reach it.
fn local_sessions() -> Result<()> {
    if platform::local_sessions() {
        Ok(())
    } else {
        Err(Error::new(2, platform::LOCAL_SESSIONS_UNSUPPORTED))
    }
}

// Repository preparation owns only its checkout and receipt. Like the existing
// configuration/control commands, it does not require or enable a session host.
fn needs_local_sessions(command: &str) -> bool {
    !matches!(command, "ls" | "watch" | "machine" | "presence" | "swarm" | "project" | "account" | "speech" | "analytics" | "graph" | "wait" | "team" | "rules" | "mobile" | "hooks" | "repository" | "session-action-status")
}

/// Fails unless this process may run what the processes of a session run:
/// `--run`, the session's supervisor, and `__state`, which its tracking hooks
/// call. That is wherever local sessions run, and on Windows also inside a
/// session of an isolated test host (spec D2). A control console refuses both
/// with exit code 2, as in Phase 1.
fn session_process() -> Result<()> {
    state::session_operations().map_err(|error| Error::new(2, error))
}
#[cfg(unix)]
fn mobile_peers(config: &Config, args: &[String]) -> Result<i32> {
    crate::mobile_peers::dispatch(config, args)
}
#[cfg(not(unix))]
fn mobile_peers(_: &Config, _: &[String]) -> Result<i32> {
    Err(Error::new(
        2,
        "One-hop mobile peer routing is unavailable on a Windows gateway; local Windows sessions are supported.",
    ))
}
fn state_command(verb: &str, args: &[String]) -> Result<i32> {
    let mut command = vec![verb.to_owned()];
    command.extend_from_slice(args);
    state::dispatch(&command).map_err(|e| Error::new(1, e))
}

/// WhiteWh fork: settings hgs keeps for this computer (U19, U22). They live in
/// its state, where the hooks that follow them run, so they need no local
/// sessions; they are never an agent launch.
fn kept_setting(command: &str) -> bool {
    matches!(command, "subagents-confirm" | "session-upload-block")
}

/// What a `@host` that is a WSL profile of this computer runs: every command
/// in that distribution, except a download, which publishes on this Windows
/// client, and a `kept_setting`, which never leaves this computer. None for
/// any other host.
fn wsl_forward(config: &Config, target: &str, command: &str, args: &[String], dry: bool) -> Option<Result<i32>> {
    if target.is_empty() || config.is_self(target) {
        return None;
    }
    let profile = crate::wsl::profile(config, target)?;
    if kept_setting(command) {
        return Some(setting_command(false, command, args, dry));
    }
    // Download publishes on this Windows client. Its output path must
    // never be forwarded as a Linux receiver destination.
    if command == "download" {
        return Some(crate::download::dispatch(config, target, args, dry));
    }
    Some(crate::wsl::run(profile, command, args, dry))
}

/// A `kept_setting` command. Only this computer's: its owner gate trusts a set
/// from a process outside every agent session, which an SSH hop or a WSL
/// distribution cannot prove.
fn setting_command(local: bool, command: &str, args: &[String], dry: bool) -> Result<i32> {
    if !local {
        return Err(Error::new(1, format!("hgs {command} changes the computer it runs on; run it on that computer")));
    }
    if dry {
        return Err(Error::new(1, format!("hgs {command} does not support --dry-run")));
    }
    state_command(command, args)
}
/// The owner's one-click fix for a DeepSeek session that cannot start: give
/// this user inheritable full control of one folder, the grant the sandbox
/// needs (U34). Only this user's own rights change, and only when the check
/// says the sandbox lacks them; the result is checked again before it is told.
fn sandbox_folder(args: &[String]) -> Result<i32> {
    if args.first().map(String::as_str) != Some("grant") {
        return Err(Error::new(1, "usage: hgs sandbox-folder grant DIRECTORY --json"));
    }
    let path = require(args, 1, "Missing folder")?;
    let folder = Path::new(path);
    if !folder.is_absolute() || !folder.is_dir() {
        return Err(Error::new(1, format!("{path} is not an existing absolute folder")));
    }
    #[cfg(windows)]
    {
        let fail = |error: std::io::Error| Error::new(1, format!("Could not change the rights of {path}: {error}"));
        if crate::platform::sandbox_folder_problem(folder).map_err(fail)?.is_none() {
            println!("{}", serde_json::json!({"status":"ok","path":path}));
            return Ok(0);
        }
        crate::platform::grant_full_control(folder).map_err(fail)?;
        if let Some(problem) = crate::platform::sandbox_folder_problem(folder).map_err(fail)? {
            return Err(Error::new(1, format!("The grant did not take: {problem}")));
        }
        println!("{}", serde_json::json!({"status":"granted","path":path}));
        Ok(0)
    }
    #[cfg(not(windows))]
    {
        println!("{}", serde_json::json!({"status":"ok","path":path}));
        Ok(0)
    }
}
fn has(args: &[String], arg: &str) -> bool {
    args.iter().any(|a| a == arg)
}
fn require<'a>(args: &'a [String], index: usize, message: &str) -> Result<&'a str> {
    args.get(index)
        .filter(|s| !s.is_empty())
        .map(String::as_str)
        .ok_or_else(|| Error::new(1, message))
}
fn sanitize(name: &str) -> String {
    name.replace([':', '.'], "_")
}
/// A Windows folder's name after either separator (spec D15).
#[cfg(windows)]
fn basename(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return "/".into();
    }
    trimmed.rsplit(['/', '\\']).next().unwrap_or(path).into()
}
#[cfg(unix)]
fn basename(path: &str) -> String {
    if path.trim_end_matches('/').is_empty() {
        return "/".into();
    }
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .into()
}
/// The name a session takes from `directory`: on Windows in the letter case of
/// the folder on disk, so `cd C:\work\myproj` names it after `MyProj` (spec D15).
#[cfg(windows)]
fn folder_name(directory: &str) -> String {
    let canonical = platform::canonical(directory)
        .ok()
        .map(|path| basename(&path.to_string_lossy()));
    on_disk_name(basename(directory), canonical)
}
#[cfg(unix)]
fn folder_name(directory: &str) -> String {
    basename(directory)
}
/// The `canonical` folder name when it is the `logical` one in another letter
/// case; a junction, link or short name with another name keeps its own.
/// Plain Rust, so its tests run on every platform.
#[cfg_attr(unix, allow(dead_code))]
fn on_disk_name(logical: String, canonical: Option<String>) -> String {
    let fold = |name: &str| {
        name.chars()
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    match canonical {
        Some(canonical) if fold(&canonical) == fold(&logical) => canonical,
        _ => logical,
    }
}
fn session_exists(name: &str) -> bool {
    terminal().session_exists(name)
}
/// This computer's session terminal (tmux on Unix).
fn terminal() -> &'static dyn TerminalBackend {
    state::backend::current()
}
fn binding_exists(config: &Config, name: &str) -> Result<bool> {
    if !config.state.is_dir() {
        return Ok(false);
    }
    Ok(state_command("exists", &[name.into()])? == 0)
}
/// On Windows only the stable CLI, whichever hgs.exe runs now: records, hooks
/// and recipes never name a portable folder that an update replaces (spec D8).
#[cfg(windows)]
pub(crate) fn executable_path() -> Result<PathBuf> {
    platform::stable_executable().map_err(|error| Error::new(1, error))
}
#[cfg(unix)]
pub(crate) fn executable_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("HGS_EXECUTABLE").filter(|s| !s.is_empty()) {
        let path = PathBuf::from(path);
        if path.is_absolute() && platform::executable(&path) {
            return Ok(path);
        }
    }
    // Preserve the invoked stable install path, including a symlink, in durable recipes.
    let invoked = env::args().next().unwrap_or_else(|| "hgs".into());
    let path = PathBuf::from(&invoked);
    if path.is_absolute() {
        return Ok(path);
    }
    if invoked.contains('/') {
        return Ok(env::current_dir()?.join(path));
    }
    if let Some(path) = env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join(&invoked))
        .find(|path| platform::executable(path))
    {
        return Ok(if path.is_absolute() {
            path
        } else {
            env::current_dir()?.join(path)
        });
    }
    Ok(env::current_exe()?)
}
/// The detached form that a launch refused inside a session pane names: a new
/// session started beside that one, as `fork -d` and `resume -d` start theirs.
const DETACHED_LAUNCH: &str = "--new -d";
/// Refuse to attach this terminal inside a session pane: `hgs a`, and a launch
/// without `-d`. A detached launch, `fork -d` and `resume -d` start their
/// sessions from inside one; `retry` names that detached form, if any.
fn nested_guard(dry: bool, retry: Option<&str>) -> Result<()> {
    if !dry && terminal().inside() {
        let name = terminal().enclosing_session();
        return Err(Error::new(
            2,
            nested_refusal(cfg!(unix), Some(&name), retry),
        ));
    }
    Ok(())
}
/// Why a command refuses to run inside a session pane (`$TMUX` on Unix,
/// `HGS_HOST_PANE` in the Windows session host, spec D8): the enclosing
/// `session` when it is looked up ("" when unknown) and the detached command to
/// `retry` with. Only tmux is named, so Windows sessions are not.
fn nested_refusal(tmux: bool, session: Option<&str>, retry: Option<&str>) -> String {
    let inside = match (tmux, session) {
        (true, Some(name)) => format!("tmux session {name}"),
        (true, None) => "tmux".to_owned(),
        (false, Some(name)) if !name.is_empty() => format!("session {name}"),
        (false, _) => "a session".to_owned(),
    };
    match retry {
        Some(command) => format!("already inside {inside}; use {command} or open a new tab"),
        None => format!("already inside {inside}; open a new tab"),
    }
}

#[cfg(test)]
pub(crate) fn usage() -> &'static str {
    USAGE
}
/// Commands that only Unix dispatches: `notify` for the tray, and `clip` and
/// `paste`, which tmux's C-v binding runs.
const UNIX_ONLY_COMMANDS: [&str; 3] = ["notify", "clip", "paste"];
/// Lines for Unix commands/input bindings or the POSIX creation-clone ABI.
fn unix_only_usage(line: &str) -> bool {
    if line.trim() == zerus_client_core::desktop::creation_clone::HELP_MARKER {
        return true;
    }
    let mut words = line.split_whitespace();
    match words.next() {
        Some("hgs") => words
            .next()
            .is_some_and(|word| UNIX_ONLY_COMMANDS.contains(&word)),
        Some("Ctrl+V") => true,
        _ => false,
    }
}
/// Refuses a command of `UNIX_ONLY_COMMANDS` where Unix does not dispatch it
/// (`unix` false: Windows), so that it fails there instead of starting a
/// program of that name.
fn unix_only(command: &str, unix: bool) -> Result<()> {
    if unix || !UNIX_ONLY_COMMANDS.contains(&command) {
        return Ok(());
    }
    Err(Error::new(2, format!("{command} is not supported on Windows")))
}
/// The usage text, `USAGE` byte for byte where tmux hosts the sessions. Windows
/// has no tmux C-v binding, which pulls a peer's image through `hgs paste` and
/// `hgs clip`, no `hgs notify`, and its dry runs print the session host's
/// requests, so its text tells of none of them and does not name tmux.
fn usage_text(tmux: bool) -> Cow<'static, str> {
    if tmux {
        return USAGE.into();
    }
    USAGE
        .lines()
        .filter(|line| !unix_only_usage(line))
        .map(|line| line.replace("the tmux/ssh command", "the session-host/ssh command") + "\n")
        .collect::<String>()
        .into()
}

pub fn dispatch(mut args: Vec<String>) -> Result<i32> {
    if args.first().is_some_and(|s| s == "swarm") && args.get(1).is_some_and(|s| s == crate::wsl::RECEIVER) {
        crate::wsl::validate_request(&args[2..])?;
        if !cfg!(target_os = "linux") { return Err(Error::new(2,"The wsl-local receiver requires a Linux hgs build")); }
        env::set_var(crate::wsl::POLICY, "existing-only");
        args.drain(..2);
    }
    if crate::desktop::launchd_command(&args) {
        args.pop();
        return dispatch(args).map_err(|mut error| {
            if error.code == crate::desktop::STARTUP_REFUSED {
                error.code = 0;
                error.message = format!("{}; automatic restart paused", error.message);
            }
            error
        });
    }
    // Private: the screen model behind the differential tmux capture test.
    if args.first().is_some_and(|a| a == "__screen-capture") {
        return crate::screen::cli(&args[1..]).map_err(|e| Error::new(2, e));
    }
    // A Windows `--run` supervisor belongs to its session's job (spec D9).
    #[cfg(windows)]
    if args.first().is_some_and(|a| a == "--run") {
        platform::prepare_supervisor();
    } else {
        platform::prepare_environment();
    }
    #[cfg(unix)]
    platform::prepare_environment();
    // Existing hooks call this private stable ABI, including hooks installed before migration.
    if args.first().is_some_and(|a| a == "__state") {
        session_process()?;
        return state::dispatch(&args[1..]).map_err(|e| Error::new(1, e));
    }
    crate::desktop::launch_configuration(&args)?;
    let config = Config::load().map_err(|mut error| {
        if crate::desktop::registered_command(&args) {
            error.code = crate::desktop::STARTUP_REFUSED;
        }
        error
    })?;
    // WhiteWh fork: both ends of the lasting channel to a Windows peer.
    if args.first().is_some_and(|a| a == "__peer") {
        return crate::peer_channel::command(&config, &args[1..]);
    }
    // An encrypted gateway binds every local native call to the physical
    // machine it advertised. This check also works on Windows, without a
    // shell, stdin rewriting or a second native process.
    if args.first().is_some_and(|a| a == "--expect-machine") {
        let expected = args.get(1).ok_or_else(|| Error::new(2, "Missing expected machine identity"))?;
        if uuid::Uuid::parse_str(expected).ok().is_none_or(|id| id.to_string() != *expected) {
            return Err(Error::new(1,"Mobile target identity changed; refresh computers"));
        }
        let guarded_at = if args.get(2).is_some_and(|a| a=="--client") {4} else {2};
        let actual = if args.get(guarded_at).is_some_and(|a| a=="session-action-status") {
            state::lifecycle_receipt_machine(&config).map_err(|_| Error::new(1,"Native machine identity is unavailable"))?
        } else {
            crate::swarm::local_node_id(&config).map_err(|_| Error::new(1,"Native machine identity is unavailable"))?
        };
        if actual != *expected {
            return Err(Error::new(1,"Mobile target identity changed; refresh computers"));
        }
        args.drain(..2);
        let command_at=if args.first().is_some_and(|a|a=="--client") {
            if args.get(1).is_none_or(|client|client.is_empty()) { return Err(Error::new(2,"--client needs a peer name")); }
            2
        }else{0};
        if args.get(command_at).is_none_or(|a| a.starts_with('@')
            || (a.starts_with('-') && !(args.len()==command_at+1 && matches!(a.as_str(),"--help"|"-h"|"--version")))) {
            return Err(Error::new(2,"A guarded mobile call requires a local command"));
        }
        if args.get(command_at).is_some_and(|a| a == "__state") {
            session_process()?;
            return state::dispatch(&args[command_at+1..]).map_err(|e| Error::new(1,e));
        }
    }
    // The existing swarm namespace fails closed on unknown operations in older
    // installations, so transport helpers cannot fall through to agent launch.
    if args.first().is_some_and(|a| a == "swarm")
        && args.get(1).is_some_and(|a| {
            matches!(a.as_str(), "mobile-peers" | "mobile-peer" | "__mobile-peer-local" | "__mobile-peer-identity" | "__mobile-peer-transport")
        })
    {
        return mobile_peers(&config, &args[1..]);
    }
    if args.first().is_some_and(|a| {
        matches!(a.as_str(), "mobile-peers" | "mobile-peer" | "__mobile-peer-local" | "__mobile-peer-identity" | "__mobile-peer-transport")
    }) {
        return mobile_peers(&config, &args);
    }
    if args.first().is_some_and(|a| a == "--run") {
        // A control console (local sessions off) still refuses it as in Phase 1;
        // an isolated test host's sessions may run it (spec D2).
        #[cfg(windows)]
        session_process()?;
        let start = Instant::now();
        let result = run(&args[1..]);
        // A launch the desktop waits for learns how its session ended.
        state::launches::finished(
            start.elapsed(),
            result
                .as_ref()
                .copied()
                .map_err(|error| (error.code, error.message.as_str())),
        );
        return result;
    }
    let mut target = String::new();
    let mut dry = false;
    let mut client = "local".to_owned();
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if let Some(host) = arg.strip_prefix('@') {
            target = host.into();
            index += 1;
        } else if arg == "--dry-run" {
            dry = true;
            index += 1;
        } else if arg == "--client" {
            client = require(&args, index + 1, "--client needs a peer name")?.into();
            index += 2;
        } else {
            break;
        }
    }
    let Some(command) = args.get(index) else {
        eprint!("{}", usage_text(cfg!(unix)));
        return Ok(1);
    };
    let args = &args[index + 1..];
    if let Some(result) = wsl_forward(&config, &target, command, args, dry) {
        return result;
    }
    match command.as_str() {
        "--expect-machine" => {
            if dry { return Err(Error::new(2,"Guarded calls do not support --dry-run")); }
            if !target.is_empty()&&!config.is_self(&target) { return remote(&config,&target,command,args,false); }
            let expected=require(args,0,"Missing expected machine identity")?;
            let mut local=vec!["--expect-machine".to_owned(),expected.to_owned()];
            if client!="local" { local.extend(["--client".into(),client]); }
            local.extend_from_slice(&args[1..]);return dispatch(local);
        }
        "-h" | "--help" | "help" => {
            // Capability probes for a selected computer must inspect that
            // computer's installed CLI, including retained older versions.
            if !target.is_empty() && !config.is_self(&target) {
                return remote(&config, &target, "--help", args, dry);
            }
            print!("{}", usage_text(cfg!(unix)));
            println!("Encrypted workspace ABI: guarded-local-v1");
            println!("WSL local receiver ABI: wsl-local-v1 (OpenCode existing-only)");
            #[cfg(target_os = "linux")]
            println!("WSL file snapshot ABI: wsl-file-snapshot-v1");
            println!("Desktop creation ABI: guarded-create-v1");
            println!("Account transfer ABI: revision-copy-v1");
            #[cfg(windows)]
            print!("{}\nMobile gateway ABI: windows-local-v1\n", crate::host::admin::HELP);
            return Ok(0);
        }
        "path-ref" => {
            if !target.is_empty() && !config.is_self(&target) {
                return Err(Error::new(2, "path-ref is local-only; specify the environment in JSON"));
            }
            if dry { return Err(Error::new(2, "path-ref does not support --dry-run")); }
            return crate::path_ref::run(args).map(|()| 0).map_err(|e| Error::new(2, format!("{e:#}")));
        }
        "--version" | "version" => return crate::status::version(&config, &target, args),
        // Read-only on every machine; never routed as an agent launch.
        "status" => return crate::status::run(&config, &target, args, dry),
        // U60: model servers are configured on this computer, privately.
        "model-server" => {
            if !target.is_empty() && !config.is_self(&target) {
                return Err(Error::new(2, "model-server is configured on this computer; omit @host"));
            }
            if dry {
                return Err(Error::new(2, "model-server does not support --dry-run"));
            }
            return state::model_servers::dispatch(&config, args);
        }
        // Text compiled into this hgs; a peer answers with its own.
        // WhiteWh fork: a build or long test Zerus tracks as this session's work.
        "run" => {
            if !target.is_empty() && !config.is_self(&target) {
                return Err(Error::new(1, "hgs run tracks the calling session's build; run it on that session's computer"));
            }
            if dry {
                return Err(Error::new(1, "hgs run does not support --dry-run"));
            }
            return state_command("build", args);
        }
        // U62: the project's tests, from .zerus/tests.toml and its map.
        "test" => {
            if !target.is_empty() && !config.is_self(&target) {
                // #732: a test runner is reached through hgs's own SSH; every
                // other test command reads the project in this folder.
                if args.first().is_some_and(|action| action == "runner") && !dry {
                    return remote(&config, &target, command, args, dry);
                }
                return Err(Error::new(2, "hgs test reads the project in this folder; omit @host"));
            }
            if dry {
                return Err(Error::new(2, "hgs test does not support --dry-run"));
            }
            return crate::test_discipline::dispatch(args);
        }
        "guide" => {
            if !target.is_empty() && !config.is_self(&target) {
                return remote(&config, &target, command, args, dry);
            }
            return crate::agent_guide::dispatch(args);
        }
        "self" => {
            if !target.is_empty() && !config.is_self(&target) {
                return Err(Error::new(1, "hgs self names the session that runs it; omit @host"));
            }
            if dry {
                return Err(Error::new(1, "hgs self does not support --dry-run"));
            }
            local_sessions()?;
            return crate::session_reference::current(&config, args);
        }
        setting if kept_setting(setting) => {
            return setting_command(target.is_empty() || config.is_self(&target), setting, args, dry);
        }
        "sandbox-folder" => {
            if !(target.is_empty() || config.is_self(&target)) {
                return Err(Error::new(1, "hgs sandbox-folder changes a folder's rights on the computer it runs on; run it there"));
            }
            if dry {
                return Err(Error::new(1, "hgs sandbox-folder does not support --dry-run"));
            }
            return sandbox_folder(args);
        }
        _ => {}
    }
    if args.first().is_some_and(|value| value.starts_with("zerus://")) {
        let local_only = args.len() == 2 && args[1] == "--local";
        if command != "inspect" || (args.len() != 1 && !local_only) || dry {
            return Err(Error::new(1, "Use hgs inspect '<session reference>' for read-only session references"));
        }
        let reference = crate::session_reference::Reference::parse(&args[0])?;
        let local_id = crate::swarm::local_node_id(&config).map_err(|e| Error::new(1,e.to_string()))?;
        if local_only && local_id != reference.machine {
            return Err(Error::new(1,"Session reference belongs to another computer; the selected SSH alias does not match"));
        }
        if target.is_empty() && local_id != reference.machine {
            if config.peers.contains(&reference.host) && !config.is_self(&reference.host) {
                return remote(&config, &reference.host, command, args, false);
            }
            return Err(Error::new(1, "The referenced computer is not configured here; choose its configured @host"));
        }
        if !target.is_empty() && !config.is_self(&target) {
            if local_only { return Err(Error::new(1,"A local session reference cannot route to another computer")); }
            return remote(&config,&target,command,args,false);
        }
        local_sessions()?;
        println!("{}", crate::session_reference::inspect(&config,&reference)?);
        return Ok(0);
    }
    if command == "native-ui" {
        let name = require(args, 0, "usage: hgs native-ui <session> [--json]")?;
        if args.get(1).is_some_and(|v| v == "--json") && !dry {
            println!(
                "{}",
                serde_json::json!({"url":crate::native_ui::address(&config,&target,name)?})
            );
            return Ok(0);
        }
        return crate::native_ui::open(&config, &target, name, dry);
    }
    if command == "account"
        && args.first().is_some_and(|v| v == "login")
        && args.get(1).is_some_and(|v| v == "native-dsh")
    {
        return crate::native_ui::open(&config, &target, "@account", dry);
    }
    if command == "a" && args.first().is_some_and(|v| v.starts_with("dsh/")) {
        return crate::native_ui::open(&config, &target, &args[0], dry);
    }
    if command == "download" {
        return crate::download::dispatch(&config, &target, args, dry);
    }
    if command == "approval-log" {
        return crate::approval_log::dispatch(&config, &target, args, dry);
    }
    if command == "desktop" {
        return crate::desktop::dispatch(&config, &target, args, dry);
    }
    // Windows only, before local sessions are enabled, and never forwarded: it
    // enables them on this computer (spec D2). On Unix the word stays a program.
    #[cfg(windows)]
    if command == "session-host" {
        if !target.is_empty() && !config.is_self(&target) {
            return Err(Error::new(
                2,
                "hgs session-host manages the computer it runs on; run it on that computer",
            ));
        }
        return crate::host::admin::dispatch(&config, args, dry);
    }
    if command == "send" && args.len() == 2 && args[1] == "--json" && !dry {
        if let Some(origin) = state::session_graph::send_origin() {
            return state::session_graph::forward_send(&target, args, origin);
        }
    }
    if !target.is_empty() && !config.is_self(&target) {
        return remote(&config, &target, command, args, dry);
    }
    if command == "external" { return crate::external_workstreams::dispatch(&config,args,dry); }
    if args.first().is_some_and(|name| crate::external_workstreams::reserved(name))
        && matches!(command.as_str(), "inspect" | "send" | "history" | "a" | "kill" | "rename" | "archive" | "terminate" | "pause" | "resume" | "fork" | "processes" | "send-now" | "session-action" | "terminal" | "interrupt" | "clear-context" | "compact-context" | "answer" | "effort" | "settings" | "read" | "notifications" | "attachment" | "file-read") {
        return crate::external_workstreams::session_command(&config,command,args,dry);
    }
    // Commands that only read or change this machine's configuration, or reach
    // remote machines, also run on a build without local sessions.
    if needs_local_sessions(command) {
        local_sessions()?;
    }
    // Only Unix has arms for these below; elsewhere they would reach the launch arm.
    unix_only(command, cfg!(unix))?;
    match command.as_str() {
        "hooks" => state::hook_health::dispatch(&config, args, dry),
        "analytics" => crate::analytics::dispatch(&config, args, dry),
        "speech" => crate::speech::dispatch(&config, args, dry),
        "mobile" => crate::phones::dispatch(&config, args, dry),
        "__dsh-ui-url" => {
            let url = state::dsh::native_ui_url(require(args, 0, "Choose a native session")?)
                .map_err(|e| Error::new(1, e))?;
            println!("{}", json!({"url":url}));
            Ok(0)
        }
        "ls" => list(&config, args, dry),
        "watch" => crate::watch::run(&config, args, dry),
        "archives" => {
            let mobile = cfg!(windows) && args.last().map(String::as_str)==Some("--windows-mobile");
            let args = if mobile { &args[..args.len()-1] } else { args };
            if (args != ["--json"] && args != ["lookup","--json"]) || dry { return Err(Error::new(1,"usage: hgs archives [lookup] --json (JSON query on stdin)")); }
            let query = state::archive_catalog::read_query().map_err(|e| Error::new(1,e))?;
            let result = if args.first().map(String::as_str) == Some("lookup") {
                if mobile { state::archive_catalog::lookup_scoped(&query,true) } else { state::archive_catalog::lookup(&query) }
            } else if mobile { state::archive_catalog::page_scoped(&query,true) }
            else { state::archive_catalog::page(&query) }.map_err(|e| Error::new(1,e))?;
            println!("{result}"); Ok(0)
        },
        "reference" => {
            if dry { return Err(Error::new(1,"Session references do not support --dry-run")); }
            crate::session_reference::create(&config,args)
        }
        "a" => attach_command(&config, args, &client, dry),
        "kill" => kill(&config, args, dry),
        "rename" => rename(&config, args, dry),
        "archive" | "terminate" => {
            let mut args = args.to_vec();
            if dry {
                args.push("--dry-run".into());
            }
            state_command(command, &args)
        }
        "inspect" | "processes" | "worktrees" | "git-status" | "git-diff" | "repository" | "dirs" | "send" | "send-now" | "send-cancel" | "session-action" | "session-action-status" | "terminal" | "history" | "history-projection" | "interrupt" | "clear-context" | "compact-context" | "answer" | "effort" | "settings" | "search" | "read" | "notifications"
        | "attachment" | "file-read" | "recovery" | "voice" | "schedules" | "graph" | "wait" | "team" | "rules" | "switch-account" => {
            if command == "rules" && dry { return Err(Error::new(1,"Rules operations do not support --dry-run")); }
            if command == "session-action-status" && dry { return Err(Error::new(1,"Lifecycle receipt reads do not support --dry-run")); }
            if command == "repository" && dry { return Err(Error::new(1,"Repository operations do not support --dry-run")); }
            if command == "wait" && dry { return Err(Error::new(1,"Wait operations do not support --dry-run")); }
            if command == "team" && dry { return Err(Error::new(1,"Team operations do not support --dry-run")); }
            if command == "switch-account" && dry { return Err(Error::new(1,"Changing a session's account does not support --dry-run")); }
            if command == "graph" && dry { return Err(Error::new(1,"Graph reads do not support --dry-run")); }
            if command == "dirs" && has(args, "--create") && dry {
                return Err(Error::new(1, "folder creation does not support --dry-run"));
            }
            if command == "worktrees" && args.first().is_some_and(|s| s == "create" || s == "cancel" || s == "remove" || s == "sweep" || s == "policy" && args.get(1).is_some_and(|s|s=="set")) && dry {
                return Err(Error::new(1, "worktree creation, removal and policy changes do not support --dry-run"));
            }
            if (matches!(command.as_str(), "processes" | "send" | "send-now" | "send-cancel" | "session-action" | "terminal" | "interrupt" | "clear-context" | "compact-context" | "answer" | "effort" | "settings" | "recovery" | "read" | "notifications" | "voice" | "schedules") || (command=="attachment" && has(args,"--stage"))) && dry {
                return Err(Error::new(1, "message input does not support --dry-run"));
            }
            state_command(command, args)
        }
        "pause" | "resume" => pause_resume(&config, command, args, &client, dry),
        "fork" => fork_session(&config, args, &client, dry),
        #[cfg(unix)]
        "notify" => notify(&config, args, dry),
        #[cfg(unix)]
        "clip" => platform::clip(),
        #[cfg(unix)]
        "paste" => platform::paste(
            &config,
            require(
                args,
                0,
                "usage: hgs paste <session> <pane> [client]  (the tmux C-v binding)",
            )?,
            require(
                args,
                1,
                "usage: hgs paste <session> <pane> [client]  (the tmux C-v binding)",
            )?,
            args.get(2).map(String::as_str).unwrap_or(""),
        ),
        "machine" => crate::machines::dispatch(&config, args, dry),
        "presence" => crate::presence::dispatch(&config, args, dry),
        "swarm" => crate::swarm::dispatch(&config, args, dry),
        "account" => crate::accounts::dispatch(&config, args, dry),
        // WhiteWh fork: a folder's SocratiCode index, an optional extension.
        "socraticode" => crate::extensions::socraticode(&config, args, dry),
        // WhiteWh fork: continue a Claude desktop app session in Zerus.
        "claude-app" => match crate::claude_app::dispatch(args)? {
            crate::claude_app::Outcome::Done(code) => Ok(code),
            crate::claude_app::Outcome::Launch(argv) => launch(&config, "claude", &argv, &client, dry),
        },
        "project" => project(&config, args, dry),
        "task" => crate::tasks::dispatch(&config, args, dry),
        _ => launch(&config, command, args, &client, dry),
    }
}

/// A session's root process: `--run N RESUME... BASE...`. Tracked agents run
/// under `state`'s supervisor; anything else first tries the N resume words,
/// then runs the base command in place of this process on Unix and as its
/// child on Windows, which has no exec (spec D8).
fn run(args: &[String]) -> Result<i32> {
    let n: usize = require(args, 0, "--run needs a resume-argument count")?
        .parse()
        .map_err(|_| Error::new(1, "invalid --run resume-argument count"))?;
    if n >= args.len() - 1 {
        return Err(Error::new(
            1,
            "--run needs a base command after the resume arguments",
        ));
    }
    if env::var("HGS_SESSION").is_ok_and(|s| !s.is_empty())
        && env::var("HGS_TRACKING").as_deref() != Ok("0")
    {
        return state_command("run", args);
    }
    let resume = &args[1..n + 1];
    let base = &args[n + 1..];
    // The agent gets Ctrl+C; a stop request or a closing console waits for it.
    // The session host pauses the agent and reads its console through the
    // supervisor channel (spec D10, D11).
    #[cfg(windows)]
    let _stops = platform::ShutdownGuard::install()?;
    #[cfg(windows)]
    crate::host::supervisor::open();
    // U60 (#658 N3): an untracked launch takes the model server's secret
    // environment from the private file the launch left and removes it.
    crate::state::model_servers::apply_session_secrets()?;
    if n != 0 {
        let start = Instant::now();
        let rc = run_resume(resume)?;
        let seconds: u64 = env::var("HGS_FALLBACK_SECS")
            .unwrap_or_else(|_| "5".into())
            .parse()
            .map_err(|_| Error::new(1, "HGS_FALLBACK_SECS must be a nonnegative integer"))?;
        if resume_stands(
            rc,
            start.elapsed().as_secs(),
            seconds,
            platform::shutdown_signal() != 0,
        ) {
            return Ok(rc);
        }
        eprintln!(
            "hgs: nothing to resume (exit {rc} after {}s) — starting fresh",
            start.elapsed().as_secs()
        );
    }
    run_base(base)
}

/// Whether a resume attempt that exited with `rc` after `elapsed` seconds is
/// final, rather than "nothing to resume" (a quick ordinary failure, after
/// which the base command starts fresh): it succeeded, ran for at least
/// `limit` seconds, crashed or was signalled (128 and above; Windows reads
/// crash statuses as signals, `platform::ntstatus_exit`), or a stop request
/// reached the supervisor (`stopped`, Windows only: there a paused agent may
/// exit with any code).
fn resume_stands(rc: i32, elapsed: u64, limit: u64, stopped: bool) -> bool {
    rc == 0 || elapsed >= limit || rc >= 128 || stopped
}

#[cfg(unix)]
fn run_resume(argv: &[String]) -> Result<i32> {
    platform::wait_interactive(Command::new(&argv[0]).args(&argv[1..]))
}

#[cfg(unix)]
fn run_base(base: &[String]) -> Result<i32> {
    // The untracked runner has no durable process state and can replace itself directly.
    use std::os::unix::process::CommandExt;
    let error = Command::new(&base[0]).args(&base[1..]).exec();
    Err(Error::new(
        if error.kind() == std::io::ErrorKind::NotFound {
            127
        } else {
            126
        },
        format!("{}: {error}", base[0]),
    ))
}

#[cfg(windows)]
fn run_resume(argv: &[String]) -> Result<i32> {
    run_child(argv)
}

#[cfg(windows)]
fn run_base(base: &[String]) -> Result<i32> {
    run_child(base)
}

/// Run `argv` on this console as the pane's program, which shares this
/// process group, and wait for it; its exit code, a crash read as 128 plus a
/// signal (`platform::code`).
#[cfg(windows)]
fn run_child(argv: &[String]) -> Result<i32> {
    let mut child = platform::agent_command(argv)
        .and_then(|mut command| command.spawn())
        .map_err(|error| {
            Error::new(
                if error.kind() == std::io::ErrorKind::NotFound {
                    127
                } else {
                    126
                },
                format!("{}: {error}", argv[0]),
            )
        })?;
    platform::forward_shutdown(&child);
    let status = child.wait()?;
    platform::shutdown_child_exited();
    Ok(platform::code(status))
}

fn peer_session(command: &str, args: &[String]) -> String {
    if command == "fork" {
        if let Some(source) = args.first() {
            let parts: Vec<_> = source.splitn(3, '/').collect();
            if parts.len() >= 2 {
                let tag = args
                    .windows(2)
                    .find(|pair| pair[0] == "-n")
                    .map(|pair| pair[1].clone())
                    .unwrap_or_else(|| {
                        format!("{}-fork", parts.get(2).copied().unwrap_or("session"))
                    });
                return format!("{}/{}/{tag}", parts[0], parts[1]);
            }
        }
    }
    if matches!(command, "a" | "resume") {
        return args.first().cloned().unwrap_or_default();
    }
    let mut project = String::new();
    let mut tag = String::new();
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--" => break,
            "-n" => {
                tag = args.get(i + 1).cloned().unwrap_or_default();
                i += 1;
            }
            "--launch-id" | "--permissions" | "--local-model" => i += 1,
            option if option.starts_with('-') => {}
            name if project.is_empty() => project = name.into(),
            _ => {}
        }
        i += 1;
    }
    if matches!(project.as_str(), "." | "..") {
        project.clear();
    } else if project.contains('/') || project.starts_with('~') {
        project = basename(&project);
    }
    format!(
        "{}{}{}",
        basename(command),
        if project.is_empty() {
            String::new()
        } else {
            format!("/{}", sanitize(&project))
        },
        if tag.is_empty() {
            String::new()
        } else {
            format!("/{tag}")
        }
    )
}
/// `hgs ls --local --json` on `peer`, in that computer's shell.
fn peer_ls(config: &Config, peer: &str, archives: bool) -> String {
    let mut args: Vec<String> = ["ls", "--local", "--json"].map(String::from).into();
    if !archives {
        args.push("--no-archives".into());
    }
    crate::windows_peer::hgs(config, peer, &args).unwrap_or_default()
}
fn peer_json_archives(config: &Config, peer: &str, archives: bool) -> Value {
    if let Some(profile) = crate::wsl::profile(config, peer) { return crate::wsl::listing(profile, peer, archives); }
    // WhiteWh fork: a Windows peer answers through its lasting channel.
    if crate::windows_peer::is_windows(config, peer) {
        let mut args: Vec<String> = ["ls", "--local", "--json"].map(String::from).into();
        if !archives {
            args.push("--no-archives".into());
        }
        if let Some(Ok(answer)) = crate::peer_channel::request(config, peer, &args, None) {
            if answer.code == 0 {
                if let Ok(value @ Value::Object(_)) = serde_json::from_slice::<Value>(&answer.stdout) {
                    return value;
                }
            }
        }
    }
    let output = platform::ssh_output(
        platform::ssh()
            .args(crate::machines::ssh_options(config, peer))
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                &crate::windows_peer::connect_timeout(config, peer, 3),
                peer,
                peer_ls(config, peer, archives).as_str(),
            ])
            .stderr(Stdio::piped()),
    );
    let (error, detail) = match output {
        Ok(out) if out.status.success() && !out.stdout.is_empty() => {
            match serde_json::from_slice::<Value>(&out.stdout) {
                Ok(value) if value.is_object() => return value,
                _ => (
                    "bad_response",
                    "Invalid response from hgs on the remote machine".to_string(),
                ),
            }
        }
        Ok(out) => {
            let detail: String = String::from_utf8_lossy(&out.stderr)
                .chars()
                .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                .take(2048)
                .collect();
            (
                "offline",
                if detail.trim().is_empty() {
                    format!("SSH command failed ({})", out.status)
                } else {
                    detail.trim().to_owned()
                },
            )
        }
        Err(err) => ("offline", format!("Could not start SSH: {err}")),
    };
    // Preserve the legacy object key order used by simple shell consumers.
    let mut value = serde_json::Map::new();
    value.insert("host".into(), json!(peer));
    value.insert("ok".into(), json!(false));
    value.insert("error".into(), json!(error));
    value.insert("error_detail".into(), json!(detail));
    value.insert("projects".into(), json!({}));
    value.insert("sessions".into(), json!([]));
    Value::Object(value)
}
#[cfg(test)]
thread_local! {
    static REMOTE_DRY_PLAN: std::cell::RefCell<Option<(Vec<String>, String)>> = const { std::cell::RefCell::new(None) };
}

fn remote(
    config: &Config,
    target: &str,
    command: &str,
    args: &[String],
    global_dry: bool,
) -> Result<i32> {
    if let Some(profile) = crate::wsl::profile(config, target) { return crate::wsl::run(profile, command, args, global_dry); }
    if target.starts_with('-') || target.chars().any(char::is_whitespace) {
        return Err(Error::new(1, "invalid SSH host alias"));
    }
    let mut args = args.to_vec();
    if command == "inspect" && args.first().is_some_and(|arg| arg.starts_with("zerus://session?")) && args.len()==1 {
        args.push("--local".into());
    }
    if global_dry {
        args.push("--dry-run".into());
    }
    let mut flags: Vec<String>;
    let mut link = None;
    let mut tab = None;
    let mut ssh = platform::ssh();
    let remote_args: Vec<String> = match command {
        "ls" => {
            flags = vec![
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                crate::windows_peer::connect_timeout(config, target, 3),
            ];
            let mut all = vec!["ls".into(), "--local".into()];
            all.extend(args.clone());
            all
        }
        "account" if args.first().is_some_and(|s| s != "login" && s != "install") => {
            flags = vec![
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "ConnectTimeout=5".into(),
            ];
            let mut all = vec![command.into()];
            all.extend(args.clone());
            all
        }
        // One long-lived stream: keepalives detect a dropped link, and EOF on
        // the local stdin reaches the peer's watcher, which then exits.
        "watch" => {
            flags = vec![
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "ConnectTimeout=5".into(),
                "-o".into(),
                "ServerAliveInterval=10".into(),
                "-o".into(),
                "ServerAliveCountMax=3".into(),
            ];
            let mut all = vec![command.into()];
            all.extend(args.clone());
            all
        }
        // Phone management reaches the relay through the gateway peer.
        "mobile" => {
            flags = vec![
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "ConnectTimeout=5".into(),
            ];
            let mut all = vec![command.into()];
            all.extend(args.clone());
            all
        }
        "--expect-machine" => {
            let expected=require(&args,0,"Missing expected machine identity")?;
            let actual=require(&args,1,"Missing guarded command")?;
            let terminal=!has(&args,"-d")&&!matches!(actual,"dirs"|"worktrees"|"repository"|"session-action-status"|"--help"|"dsh"|"opencode");
            flags=vec!["-o".into(),"BatchMode=yes".into(),"-o".into(),crate::windows_peer::connect_timeout(config, target, 3)];
            if terminal { flags.insert(0,"-t".into()); }
            let mut all=vec!["--expect-machine".into(),expected.into(),"--client".into(),config.host().into()];
            all.extend_from_slice(&args[1..]);all
        }
        "session-action-status" => return Err(Error::new(2,"A remote lifecycle receipt read requires --expect-machine ID before session-action-status")),
        "archives" | "kill" | "terminate" | "pause" | "inspect" | "reference" | "processes" | "worktrees" | "git-status" | "git-diff" | "repository" | "dirs" | "project" | "archive" | "rename"
        | "send" | "send-now" | "send-cancel" | "session-action" | "terminal" | "history" | "history-projection" | "interrupt" | "clear-context" | "compact-context" | "answer" | "effort" | "settings" | "search" | "schedules" | "graph" | "wait" | "team" | "rules" | "guide" | "dsh" | "opencode" | "attachment" | "file-read" | "recovery" | "swarm" | "read" | "notifications" | "speech" | "analytics" | "voice" | "hooks" | "test" | "--help" => {
            flags = vec![
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                crate::windows_peer::connect_timeout(config, target, 3),
            ];
            let mut all = vec![command.into()];
            all.extend(args.clone());
            all
        }
        _ => {
            if has(&args, "-d") || (command == "resume" && has(&args, "--all")) {
                flags = vec!["-o".into(), "BatchMode=yes".into()];
            } else {
                let directory = platform::link_marker_dir()?;
                let marker = directory.path().join("up");
                flags = vec![
                    "-t".into(),
                    "-o".into(),
                    "PermitLocalCommand=yes".into(),
                    "-o".into(),
                    format!(
                        "LocalCommand={}",
                        platform::create_file_command(&mut ssh, &marker)
                    ),
                ];
                link = Some(directory);
                tab = Some(format!("{target}:{}", peer_session(command, &args)));
            }
            let mut all = vec!["--client".into(), config.host().into(), command.into()];
            all.extend(args.clone());
            all
        }
    };
    flags.extend(crate::machines::ssh_options(config, target));
    let remote = if crate::windows_peer::is_windows(config, target) {
        // WhiteWh fork: a Windows computer runs the same build's commands; its
        // cmd.exe has no POSIX capability probes.
        let mut windows_args = remote_args.clone();
        if command == "analytics" {
            windows_args.push("--read-only".into());
        }
        // Listed commands use the lasting channel (src/peer_channel.rs); `ls --json`
        // does so in peer_json_archives below.
        let json_listing = command == "ls" && has(&args, "--json");
        if let Some(reads_stdin) = crate::peer_channel::routed(command, &args).filter(|_| !json_listing && !global_dry && !has(&args, "--dry-run")) {
            let input = if reads_stdin {
                let mut bytes = Vec::new();
                io::stdin().read_to_end(&mut bytes)?;
                Some(bytes)
            } else {
                None
            };
            match crate::peer_channel::request(config, target, &windows_args, input.clone()) {
                Some(Ok(answer)) => {
                    if command == "ls" {
                        println!("{target}:");
                    }
                    io::stdout().write_all(&answer.stdout)?;
                    io::stdout().flush()?;
                    io::stderr().write_all(&answer.stderr)?;
                    return Ok(answer.code);
                }
                Some(Err(error)) => return Err(Error::new(3, error)),
                // No broker took it: the command's own ssh, with the stdin already read.
                None => {
                    if let Some(input) = input {
                        return run_with_input(&mut ssh, flags, target, crate::windows_peer::command(&windows_args)?, input);
                    }
                }
            }
        }
        crate::windows_peer::command(&windows_args)?
    } else if command == "--expect-machine" && args.get(1).is_some_and(|actual| actual=="session-action-status") {
        // Probe the same executable, without reading the JSON stream. An old
        // peer must never interpret this reader's name as a program to launch.
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'guarded-create-v1' && ~/.local/bin/hgs --help 2>/dev/null | grep -qEx '[[:space:]]*lifecycle-receipt-status-v1[[:space:]]*'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to read lifecycle receipts' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "--expect-machine" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'guarded-create-v1'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to continue verified creation' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "repository" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'repository-clone-v1'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to clone repositories' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "wait" {
        // Old peers otherwise interpret an unknown command as an agent launch.
        // Probe the same installed executable without consuming JSON stdin.
        // These probes are POSIX scripts; a fish login shell runs them under sh.
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'hgs [@host] wait'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to track waits' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "rules" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'iron-rules-v1'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to use iron rules' >&2; exit 1; fi", platform::join(&remote_args)))
    } else if command == "team" {
        // WhiteWh fork: the same probe as wait, for peers without agent teams.
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'hgs [@host] team OPERATION'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to use agent teams' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "notifications" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qEx '[[:space:]]*session-notifications-v1[[:space:]]*'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to change notification preferences' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "hooks" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'hooks-health-v1'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to inspect hook health' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "analytics" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'hgs [@host] analytics --json'; then exec ~/.local/bin/hgs {} --read-only; else echo 'hgs: update the selected computer to read analytics' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "watch" {
        // An older peer would read an unknown command as an agent launch.
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF '{}'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to stream events (watch-v1)' >&2; exit 1; fi",zerus_watch_protocol::HELP_MARKER,platform::join(&remote_args)))
    } else if command == "test" {
        // #732: a test runner without this hgs would read it as an agent launch.
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'test-runner-v1'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to run tests (test-runner-v1)' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "guide" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'hgs [@host] guide'; then exec ~/.local/bin/hgs {}; else echo 'hgs: the selected computer has no agent guide; check hgs @host -h' >&2; exit 1; fi",platform::join(&remote_args)))
    } else { format!("~/.local/bin/hgs {}", platform::remote_join(&remote_args)) };
    if global_dry || has(&args, "--dry-run") {
        if let Some(title) = tab {
            if config.tab {
                println!("tab {title} color={}", platform::tab_color(config, target));
            }
        }
        #[cfg(test)]
        REMOTE_DRY_PLAN.with(|plan| *plan.borrow_mut() = Some((flags.clone(), remote.clone())));
        println!("ssh {} {target} {remote}", platform::join(&flags));
        return Ok(0);
    }
    if command == "ls" && has(&args, "--json") {
        println!("{}", peer_json_archives(config, target, !has(&args,"--no-archives")));
        return Ok(0);
    }
    if command == "ls" {
        println!("{target}:");
    }
    let _tab = tab
        .as_ref()
        .map(|title| TabGuard::begin(config, title, &platform::tab_color(config, target)));
    flags.push(target.into());
    flags.push(remote);
    let rc = platform::wait_interactive(ssh.args(flags))?;
    if rc == 255 {
        if link
            .as_ref()
            .is_some_and(|dir| dir.path().join("up").exists())
        {
            platform::restore_terminal();
            return Err(Error::new(
                3,
                format!("link to {target} dropped; the session is still there, rerun to reattach"),
            ));
        }
        return Err(Error::new(3, format!("{target} unreachable")));
    }
    Ok(rc)
}

/// WhiteWh fork: a Windows peer's command whose stdin was read for the channel,
/// run over its own ssh with that input when no broker took it.
fn run_with_input(ssh: &mut std::process::Command, mut flags: Vec<String>, target: &str, remote: String, input: Vec<u8>) -> Result<i32> {
    flags.push(target.into());
    flags.push(remote);
    let mut child = platform::spawn_ssh(ssh.args(flags), true, true, true)?;
    let mut stdin = child.stdin.take();
    let writer = std::thread::spawn(move || {
        if let Some(stdin) = stdin.as_mut() {
            let _ = stdin.write_all(&input);
        }
    });
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(err) = err.as_mut() {
            let _ = err.read_to_end(&mut bytes);
        }
        bytes
    });
    let mut output = Vec::new();
    if let Some(out) = out.as_mut() {
        out.read_to_end(&mut output)?;
    }
    let status = child.child.wait()?;
    let _ = writer.join();
    let errors = errors.join().unwrap_or_default();
    io::stdout().write_all(&output)?;
    io::stderr().write_all(&errors)?;
    let rc = platform::code(status);
    if rc == 255 {
        return Err(Error::new(3, format!("{target} unreachable")));
    }
    Ok(rc)
}

pub(crate) fn local_json(config: &Config) -> Result<Value> { local_json_archives(config, true) }
/// #569: the private stores refused now (path, reason, fix), read from the
/// small current set only; absent when none is refused.
fn private_stores(raw: &mut Value) {
    let stores = crate::private_stores::refused();
    if !stores.is_empty() {
        raw["private_stores"] = json!(stores);
    }
}
/// `ls --json --local --no-archives`, which `hgs watch` streams.
pub(crate) fn local_listing(config: &Config) -> Result<Value> { local_json_archives(config, false) }
fn local_json_archives(config: &Config, archives: bool) -> Result<Value> {
    if !platform::local_sessions() {
        // A control console: no tmux, session state or local projects. Absent
        // `local_sessions` means true, so Unix listings stay as they were.
        let mut raw = json!({"host":config.host(),"ok":true,"hgs_version":env!("CARGO_PKG_VERSION"),"projects":{},"sessions":[],"peers":config.peers,"metrics":crate::metrics::snapshot(&config.state),"local_sessions":false});
        if let Some(presence) = crate::presence::listing(&config.state, crate::presence::now()) {
            raw["desktop_presence"] = presence;
        }
        raw["preferences"] = crate::swarm::preferences(config);
        private_stores(&mut raw);
        #[cfg(windows)]
        {
            raw["local_host"] = crate::host::admin::local_host(&config.state);
        }
        crate::external_workstreams::append_listing(config,&mut raw)?;
        return Ok(raw);
    }
    let mut projects = serde_json::Map::new();
    for project in config.projects()? {
        projects.insert(project.name, json!(project.dir));
    }
    // Before the terminal lists: a launch's session that this listing does not
    // show existed before it began and has ended.
    let launches = state::launches::Evidence::read();
    // A listing that fails is an error: no sessions would show live ones as stopped.
    let listing = terminal().listing().map_err(|e| Error::new(1, e))?;
    let sessions = state::listed_rows(&listing);
    let mut raw = json!({"host":config.host(),"ok":true,"hgs_version":env!("CARGO_PKG_VERSION"),"projects":projects,"sessions":sessions,"peers":config.peers,"metrics":crate::metrics::snapshot(&config.state)});
    // Phone alerts wait while the owner is active here; see src/presence.rs.
    if let Some(presence) = crate::presence::listing(&config.state, crate::presence::now()) {
        raw["desktop_presence"] = presence;
    }
    raw["preferences"] = crate::swarm::preferences(config);
    private_stores(&mut raw);
    // Windows: the session host as found, never started (spec D2). Unix
    // listings stay as they were.
    #[cfg(windows)]
    {
        raw["local_host"] = crate::host::admin::local_host(&config.state);
    }
    // A session renamed before merge_listing reads the panes keeps its ID.
    let ids = state::listed_ids(&listing);
    let mut merged = state::merge_listing_archives(raw, &ids, archives).map_err(|e| Error::new(1, e))?;
    merged["archive_paging"] = json!(true);
    merged["archive_revision"] = json!(state::archive_catalog::revision().map_err(|e|Error::new(1,e))?);
    let ended = launches.ended(merged["sessions"].as_array().map_or(&[], Vec::as_slice));
    if !ended.is_empty() {
        merged["ended_launches"] = json!(ended);
    }
    crate::external_workstreams::append_listing(config,&mut merged)?;
    Ok(merged)
}
fn local_list(config: &Config) -> Result<()> {
    if !platform::local_sessions() {
        println!("  (control console: sessions run on remote machines)");
        return Ok(());
    }
    match terminal().attached_counts().map_err(|e| Error::new(1, e))? {
        Some(rows) => {
            for (name, count) in rows {
                println!("  {name:<40} {count} client(s)");
            }
        }
        None => println!("  (no sessions)"),
    }
    if config.state.is_dir() {
        state_command("list", &[])?;
    }
    Ok(())
}
fn list(config: &Config, args: &[String], global_dry: bool) -> Result<i32> {
    let dry = global_dry || has(args, "--dry-run");
    if dry && !platform::local_sessions() {
        return Ok(0); // this machine's listing runs no command
    }
    if has(args, "--json") {
        if dry {
            println!("{}", terminal().describe(&DryRun::ListJson));
            return Ok(0);
        }
        let archives = !has(args,"--no-archives");
        let local = local_json_archives(config, archives)?;
        if has(args, "--local") {
            println!("{local}");
        } else {
            let mut fleet = vec![local];
            fleet.extend(config.peers.iter().map(|peer| peer_json_archives(config, peer, archives)));
            println!("{}", json!(fleet));
        }
        return Ok(0);
    }
    if dry {
        println!("{}", terminal().describe(&DryRun::List));
        return Ok(0);
    }
    if args.first().is_some_and(|a| a == "--local") {
        local_list(config)?;
        return Ok(0);
    }
    println!("{}:", config.host());
    local_list(config)?;
    for peer in &config.peers {
        println!("{peer}:");
        if let Some(profile) = crate::wsl::profile(config, peer) {
            match crate::wsl::capture(profile, &["ls".into(), "--local".into()], 16 * 1024 * 1024, 10) {
                Ok(out) if out.status.success() => print!("{}", String::from_utf8_lossy(&out.stdout)),
                _ => println!("  offline"),
            }
            continue;
        }
        let mut flags = crate::machines::ssh_options(config, peer);
        flags.extend(
            [
                "-o",
                "BatchMode=yes",
                "-o",
                &crate::windows_peer::connect_timeout(config, peer, 3),
                peer,
            ]
            .into_iter()
            .map(str::to_owned),
        );
        flags.push(crate::windows_peer::hgs(config, peer, &["ls".into(), "--local".into()])?);
        match platform::ssh_capture(&flags) {
            Some(out) => println!("{}", out.trim_end_matches('\n')),
            None => println!("  offline"),
        }
    }
    Ok(0)
}

fn attach_command(config: &Config, args: &[String], client: &str, mut dry: bool) -> Result<i32> {
    const HELP: &str = "usage: hgs a <session> [--existing [--run-id ID]]";
    let session = require(args, 0, HELP)?;
    if state::dsh::exists(session) {
        return crate::native_ui::open(config, "", session, dry || has(args, "--dry-run"));
    }
    let mut existing = false;
    let mut run_id = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--existing" => existing = true,
            "--dry-run" => dry = true,
            "--run-id" if run_id.is_none() => {
                run_id = Some(require(args, i + 1, "--run-id needs an identity")?);
                i += 1;
            }
            _ => return Err(Error::new(1, HELP)),
        }
        i += 1;
    }
    if run_id.is_some() && !existing {
        return Err(Error::new(1, "--run-id requires --existing"));
    }
    if state::opencode::exists(session) {
        return state::opencode::attach(session, run_id, dry).map_err(|error| Error::new(1, error));
    }
    if !existing {
        return attach(config, session, client, dry);
    }
    if dry {
        println!(
            "{}",
            terminal().describe(&DryRun::AttachExisting { session, run_id })
        );
        return Ok(0);
    }
    nested_guard(false, None)?;
    // Resolve exact name to an immutable tmux session ID before checking the run.
    // If it disappears later, attach fails instead of creating or selecting a new one.
    const ENDED: &str = "session is no longer running; resume it explicitly";
    let target = terminal()
        .probe(
            Target::Name(session),
            &[PaneField::SessionId, PaneField::RunMarker],
        )
        .ok_or_else(|| Error::new(1, ENDED))?;
    let fields = target.fields();
    // A running tmux server expands an unknown target to empty fields instead of failing.
    if fields.first().is_some_and(|id| id.is_empty()) {
        return Err(Error::new(1, ENDED));
    }
    if fields.len() != 2
        || !fields[0]
            .strip_prefix('$')
            .is_some_and(|id| !id.is_empty() && id.chars().all(|ch| ch.is_ascii_digit()))
    {
        return Err(Error::new(1, state::backend::INVALID_SESSION_ID.text()));
    }
    if run_id.is_some_and(|expected| fields[1] != expected) {
        return Err(Error::new(1, crate::state::backend::RUN_CHANGED));
    }
    attach_target(config, session, client, Target::Session(fields[0]), run_id)
}

fn attach(config: &Config, session: &str, client: &str, dry: bool) -> Result<i32> {
    if dry {
        println!(
            "{}",
            terminal().describe(&DryRun::Attach { session, client })
        );
        return Ok(0);
    }
    nested_guard(false, None)?;
    terminal().attachable()?;
    if !session_exists(session) && config.state.is_dir() {
        env::set_var("HGS_CLIENT", client);
        let rc = state_command("resume", &[session.into()])?;
        if rc != 0 {
            return Ok(rc);
        }
    }
    attach_target(config, session, client, Target::Name(session), None)
}

fn attach_target(
    config: &Config,
    session: &str,
    client: &str,
    target: Target,
    run_id: Option<&str>,
) -> Result<i32> {
    terminal().set_client(target, client);
    let _tab = TabGuard::begin(config, session, &platform::tab_color(config, "local"));
    let exit = terminal().attach(target, run_id)?;
    report_startup_failure(session);
    Ok(exit.code)
}

fn report_startup_failure(session: &str) -> bool {
    if let Ok(Some(message)) = state::startup_failure(session) {
        eprintln!("hgs: {message}");
        true
    } else {
        false
    }
}

fn attach_requested(
    config: &Config,
    session: &str,
    client: &str,
    agent: &str,
    id: &str,
) -> Result<i32> {
    // Capture the tmux identity before rechecking the conversation. If the name
    // is reused afterwards, attach must fail instead of selecting its new owner.
    let target = terminal()
        .probe(Target::Name(session), &[PaneField::SessionId])
        .filter(|info| {
            info.value().strip_prefix('$').is_some_and(|value| {
                !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit())
            })
        })
        .ok_or_else(|| {
            Error::new(
                1,
                "requested session ended before attach; repeat the command",
            )
        })?;
    state::check_requested_session(session, agent, id).map_err(|error| Error::new(1, error))?;
    attach_target(
        config,
        session,
        client,
        Target::Session(target.value()),
        None,
    )
}
fn kill(config: &Config, args: &[String], mut dry: bool) -> Result<i32> {
    let session = require(args, 0, "usage: hgs kill <session> [--archive <id>]")?;
    let mut archive = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => dry = true,
            "--archive" if archive.is_none() => {
                archive = Some(require(args, index + 1, "--archive needs an archive ID")?);
                index += 1;
            }
            option => return Err(Error::new(1, format!("unknown kill option: {option}"))),
        }
        index += 1;
    }
    // Resolve archive-only operations before touching tmux. The same name can
    // belong to a live session and several independent archived conversations.
    if let Some(id) = archive {
        let mut args = vec![session.into(), "--archive".into(), id.into()];
        if dry {
            args.push("--dry-run".into());
        }
        return state_command("forget", &args);
    }
    if dry {
        println!("{}", terminal().describe(&DryRun::Kill { session }));
        return Ok(0);
    }
    if session_exists(session) {
        let rc = terminal().kill(session)?;
        if rc != 0 {
            return Ok(rc);
        }
    } else if !binding_exists(config, session)? {
        return Err(Error::new(1, format!("no such session: {session}")));
    }
    if config.state.is_dir() {
        state_command("forget", &[session.into()])
    } else {
        Ok(0)
    }
}

fn rename(config: &Config, args: &[String], mut dry: bool) -> Result<i32> {
    const USAGE: &str = "usage: hgs rename <session> <new-full-name> [--archive ID] [--dry-run]";
    let old = require(args, 0, USAGE)?;
    let new = require(args, 1, USAGE)?;
    let mut archive = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => dry = true,
            "--archive" if archive.is_none() => {
                archive = Some(require(args, index + 1, "--archive needs an archive ID")?);
                index += 1;
            }
            option => return Err(Error::new(1, format!("unknown rename option: {option}"))),
        }
        index += 1;
    }
    let mut state_args = vec![old.to_owned(), new.to_owned()];
    if let Some(id) = archive {
        state_args.extend(["--archive".into(), id.into()]);
    }
    if dry {
        state_args.push("--dry-run".into());
    }
    let result = state_command("rename", &state_args)?;
    if result == 0 && !dry && archive.is_none() && config.tab {
        if let Err(error) = terminal().refresh_titles(new) {
            eprintln!("hgs: session renamed; could not update a terminal title: {error}");
        }
    }
    Ok(result)
}
fn fork_session(config: &Config, args: &[String], client: &str, mut dry: bool) -> Result<i32> {
    let name = require(
        args,
        0,
        "usage: hgs fork <session> [-n name] [-d] [--archive ID]",
    )?;
    let (mut tag, mut archive, mut expected_run, mut expected_conversation) =
        (None, None, None, None);
    let mut detached = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => dry = true,
            "-d" => detached = true,
            "-n" | "--archive" | "--expected-run-id" | "--expected-conversation-id" => {
                let value = require(args, index + 1, "missing fork option value")?;
                let slot = match args[index].as_str() {
                    "-n" => &mut tag,
                    "--archive" => &mut archive,
                    "--expected-run-id" => &mut expected_run,
                    _ => &mut expected_conversation,
                };
                if slot.is_some() {
                    return Err(Error::new(1, "duplicate fork option"));
                }
                *slot = Some(value);
                index += 1;
            }
            option => return Err(Error::new(1, format!("unknown fork option: {option}"))),
        }
        index += 1;
    }
    if !detached && !dry && terminal().inside() {
        return Err(Error::new(
            2,
            nested_refusal(cfg!(unix), None, Some("fork -d")),
        ));
    }
    env::set_var("HGS_CLIENT", client);
    let target = state::fork_session(name, tag, archive, expected_run, expected_conversation, dry)
        .map_err(|error| Error::new(1, error))?;
    if detached || dry {
        Ok(0)
    } else if state::dsh::exists(&target) {
        crate::native_ui::open(config, "", &target, false)
    } else {
        attach(config, &target, client, false)
    }
}

fn pause_resume(
    config: &Config,
    command: &str,
    args: &[String],
    client: &str,
    mut dry: bool,
) -> Result<i32> {
    let session = require(
        args,
        0,
        &format!("usage: hgs {command} <session>|--all [-d] [--archive <id> | --fresh]"),
    )?;
    let mut detached = session == "--all";
    if session.starts_with('-') && session != "--all" {
        return Err(Error::new(
            1,
            format!("unknown {command} option: {session}"),
        ));
    }
    let mut archive = None;
    let mut fresh = false;
    let mut expected_run = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => dry = true,
            "-d" if command == "resume" => detached = true,
            "-d" => return Err(Error::new(1, "-d is only valid with resume")),
            "--archive" if command == "resume" && session != "--all" && archive.is_none() => {
                archive = Some(require(args, index + 1, "--archive needs an archive ID")?);
                index += 1;
            }
            "--fresh" if command == "resume" && session != "--all" => fresh = true,
            "--expected-run-id" if command == "resume" && expected_run.is_none() => {
                expected_run = Some(require(args, index + 1, "--expected-run-id needs a run ID")?);
                index += 1;
            }
            option => return Err(Error::new(1, format!("unknown {command} option: {option}"))),
        }
        index += 1;
    }
    if fresh && archive.is_some() {
        return Err(Error::new(1, "--fresh cannot restore an archive"));
    }
    if expected_run.is_some() && !fresh {
        return Err(Error::new(1, "--expected-run-id is only valid with --fresh"));
    }
    if command == "resume" && !detached && !dry && terminal().inside() {
        return Err(Error::new(
            2,
            nested_refusal(cfg!(unix), None, Some("resume -d")),
        ));
    }
    let mut state_args = vec![session.into()];
    if let Some(id) = archive {
        state_args.extend(["--archive".into(), id.into()]);
    }
    if let Some(run) = expected_run {
        state_args.extend(["--expected-run-id".into(), run.into()]);
    }
    if dry {
        state_args.push("--dry-run".into());
    }
    env::set_var("HGS_CLIENT", client);
    let rc = state_command(if fresh { "fresh" } else { command }, &state_args)?;
    if rc != 0 || dry || command != "resume" || detached || state::dsh::exists(session) || state::opencode::exists(session) {
        return Ok(rc);
    }
    attach(config, session, client, false)
}

#[cfg(unix)]
fn notify(config: &Config, args: &[String], dry: bool) -> Result<i32> {
    use std::{
        io::Write,
        time::{SystemTime, UNIX_EPOCH},
    };
    let bell = args.first().is_some_and(|s| s == "--bell");
    let args = if bell { &args[1..] } else { args };
    let session = require(args, 0, "usage: hgs notify [--bell] <session> [text...]")?;
    let mut text = args[1..].join(" ");
    if text.is_empty() {
        text = if bell {
            "rang the bell"
        } else {
            "waiting for you"
        }
        .into();
    }
    let path = env::var("HGS_EVENTS")
        .ok()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if platform::is_macos() {
                PathBuf::from(format!(
                    "{}/Library/Application Support/hgs/events",
                    config.home
                ))
            } else {
                PathBuf::from(
                    env::var("XDG_RUNTIME_DIR")
                        .unwrap_or_else(|_| format!("/tmp/hgs-{}", unsafe { libc::getuid() })),
                )
                .join("hgs/events")
            }
        });
    if dry {
        println!("notify {} {session}", path.display());
        return Ok(0);
    }
    // Never create or spool events when the tray is absent.
    if !path.is_file() {
        return Ok(0);
    }
    let mut file = match fs::OpenOptions::new().append(true).open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let line = format!(
        "{}\n",
        json!({"session":session,"host":config.host(),"text":text,"bell":bell,"ts":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()})
    );
    // O_APPEND plus a single write preserves records across simultaneous short notifications.
    file.write_all(line.as_bytes())?;
    Ok(0)
}
fn project(config: &Config, args: &[String], dry: bool) -> Result<i32> {
    let subcommand = args.first().map(String::as_str).unwrap_or("ls");
    let args = if args.is_empty() { args } else { &args[1..] };
    match subcommand {
        "ls" => {
            let projects = config.projects()?;
            if has(args, "--json") {
                println!("{}", json!(projects.iter().map(|p| json!({"name":p.name,"dir":p.dir,"src":p.src,"exists":Path::new(&p.dir).is_dir()})).collect::<Vec<_>>()));
            } else {
                for p in projects {
                    println!(
                        "{} {:<24} {:<8} {}",
                        if Path::new(&p.dir).is_dir() { " " } else { "!" },
                        p.name,
                        p.src,
                        p.dir
                    );
                }
            }
        }
        "add" | "set" => {
            let usage = format!("usage: hgs project {subcommand} <name> <dir>");
            let name = require(args, 0, &usage)?;
            let directory = require(args, 1, &usage)?;
            config::check_name(name)?;
            let expanded = config.expand_home(directory);
            config::check_directory(&expanded)?;
            let path = absolute_directory(&expanded)
                .map_err(|_| Error::new(1, format!("no such directory: {directory}")))?;
            let directory = path.to_string_lossy();
            config::check_directory(&directory)?;
            if subcommand == "add" {
                if let Some(p) = config.projects()?.into_iter().find(|p| p.name == name) {
                    return Err(Error::new(1, format!("project '{name}' already exists ({}); use 'hgs project set' to override it", p.src)));
                }
            }
            if dry {
                println!(
                    "project {subcommand} {name}={directory} -> {}/projects.local",
                    config.dir.display()
                );
            } else {
                config.write_project(name, Some(&directory), subcommand == "add")?;
            }
        }
        "rm" => {
            let name = require(args, 0, "usage: hgs project rm <name>")?;
            match config
                .projects()?
                .into_iter()
                .find(|p| p.name == name)
                .map(|p| p.src)
            {
                Some(source) if source == "local" => {}
                Some(_) => return Err(Error::new(1, format!("unknown project '{name}'"))),
                None => return Err(Error::new(1, format!("unknown project '{name}'"))),
            }
            if dry {
                println!(
                    "project rm {name} <- {}/projects.local",
                    config.dir.display()
                );
            } else {
                config.write_project(name, None, false)?;
            }
        }
        _ => {
            return Err(Error::new(
                1,
                "usage: hgs project ls [--json] | add <name> <dir> | set <name> <dir> | rm <name>",
            ))
        }
    }
    Ok(0)
}
fn logical_cwd() -> std::io::Result<PathBuf> {
    let actual = env::current_dir()?;
    Ok(env::var("PWD")
        .ok()
        .filter(|p| {
            Path::new(p).is_absolute() && platform::canonical(p).ok().as_ref() == Some(&actual)
        })
        .map(PathBuf::from)
        .unwrap_or(actual))
}
fn absolute_directory(path: &str) -> std::io::Result<PathBuf> {
    // Match `cd -L; pwd`: a project symlink's visible name remains the session name.
    // Canonicalizing here would rename ~/projects/site to its target's basename.
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        logical_cwd()?.join(path)
    };
    let mut directory = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                directory.pop();
            }
            component => directory.push(component.as_os_str()),
        }
    }
    if !directory.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "not a directory",
        ));
    }
    Ok(directory)
}

#[derive(Default)]
struct LaunchOptions {
    account: Option<String>,
    permissions: Option<String>,
    local_model: Option<String>,
    model_server: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    launch_id: Option<String>,
    project: String,
    tag: String,
    fresh: bool,
    new_only: bool,
    resume: bool,
    resume_id: Option<String>,
    remote_control: bool,
    dry: bool,
    detached: bool,
    bare: bool,
    extra: Vec<String>,
}
fn conversation_id(value: &str) -> Option<String> {
    let (prefix, value) = match value.strip_prefix("session_") {
        Some(value) => ("session_", value),
        None => ("", value),
    };
    uuid::Uuid::parse_str(value)
        .ok()
        .map(|id| format!("{prefix}{id}"))
}
impl LaunchOptions {
    fn parse(args: &[String], dry: bool) -> Result<Self> {
        let mut options = Self {
            dry,
            ..Self::default()
        };
        let mut i = 0;
        while let Some(arg) = args.get(i) {
            match arg.as_str() {
                "-n" => {
                    options.tag = require(args, i + 1, "-n needs a tag")?.into();
                    i += 1;
                }
                "--model" => { options.model=Some(require(args,i+1,"--model needs provider/model")?.into()); i+=1; }
                "--model-server" => { options.model_server=Some(require(args,i+1,"--model-server needs a server name")?.into()); i+=1; }
                "--effort" => { options.effort=Some(require(args,i+1,"--effort needs a model variant")?.into()); i+=1; }
                "--account" => {
                    options.account =
                        Some(require(args, i + 1, "--account needs a profile ID")?.into());
                    i += 1;
                }
                "--permissions" => {
                    options.permissions =
                        Some(require(args, i + 1, "--permissions needs provider, bypass or native:<json>")?.into());
                    i += 1;
                }
                "--local-model" => {
                    options.local_model =
                        Some(require(args, i + 1, "--local-model needs a model name")?.into());
                    i += 1;
                }
                "--launch-id" => {
                    let id =
                        uuid::Uuid::parse_str(require(args, i + 1, "--launch-id needs a UUID")?)
                            .map_err(|_| Error::new(1, "--launch-id needs a UUID"))?;
                    if id.is_nil() || options.launch_id.replace(id.to_string()).is_some() {
                        return Err(Error::new(1, "provide one non-empty launch UUID"));
                    }
                    i += 1;
                }
                "-d" => options.detached = true,
                "-c" | "--resume" | "--continue" => {
                    options.resume = true;
                    if let Some(id) = args.get(i + 1).and_then(|value| conversation_id(value)) {
                        if options.resume_id.replace(id).is_some() {
                            return Err(Error::new(1, "only one conversation ID can be requested"));
                        }
                        i += 1;
                    }
                }
                option if option.starts_with("--resume=") || option.starts_with("--continue=") => {
                    let value = option.split_once('=').unwrap().1;
                    let id = conversation_id(value)
                        .ok_or_else(|| Error::new(1, "--resume=ID needs a conversation UUID"))?;
                    if options.resume_id.replace(id).is_some() {
                        return Err(Error::new(1, "only one conversation ID can be requested"));
                    }
                    options.resume = true;
                }
                "--fresh" => options.fresh = true,
                "--new" => options.new_only = true,
                "--bare" => options.bare = true,
                "--rc" => options.remote_control = true,
                "--dry-run" => options.dry = true,
                "--" => {
                    options.extra = args[i + 1..].to_vec();
                    break;
                }
                option if option.starts_with('-') => {
                    return Err(Error::new(
                        1,
                        format!("unknown option {option} (agent flags go after --)"),
                    ))
                }
                name if options.project.is_empty() => options.project = name.into(),
                name => {
                    return Err(Error::new(
                        1,
                        format!("unexpected argument {name} (agent flags go after --)"),
                    ))
                }
            }
            i += 1;
        }
        // Inside a session pane only a detached launch runs: it starts its
        // session beside that one, as `fork -d` and `resume -d` do, and never
        // attaches or switches this terminal.
        if !options.detached {
            nested_guard(options.dry, Some(DETACHED_LAUNCH))?;
        }
        if options.launch_id.is_some() && !options.new_only {
            return Err(Error::new(1, "--launch-id requires --new"));
        }
        if options.resume_id.is_some() && options.fresh {
            return Err(Error::new(
                1,
                "an exact conversation ID cannot be combined with --fresh",
            ));
        }
        if options.new_only {
            // WhiteWh fork: a new session may continue one exact conversation.
            if (options.resume && options.resume_id.is_none()) || options.fresh {
                return Err(Error::new(
                    1,
                    "--new cannot be combined with --resume or --fresh",
                ));
            }
            if options.tag.is_empty() {
                options.tag = format!("work-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
            }
            // Same tags as rename and fork; the GUI validates with SessionTag::problem.
            if options.tag.trim() != options.tag
                || options.tag.contains(['/', ':', '.'])
                || options.tag.chars().any(char::is_control)
            {
                return Err(Error::new(
                    1,
                    "new session name cannot contain '/', ':', '.', control characters or surrounding whitespace",
                ));
            }
        }
        Ok(options)
    }
}
fn resolve_project(
    config: &Config,
    command: &str,
    options: &LaunchOptions,
) -> Result<(String, String)> {
    let project = &options.project;
    if project.is_empty() {
        let directory = logical_cwd()?.to_string_lossy().into_owned();
        return Ok((directory.clone(), folder_name(&directory)));
    }
    // A Windows folder may also be `C:\work`, `work\site` or `C:work`.
    let windows_path = cfg!(windows) && (project.contains('\\') || project.get(1..2) == Some(":"));
    if project.contains('/')
        || project.starts_with('~')
        || matches!(project.as_str(), "." | "..")
        || windows_path
    {
        let directory = absolute_directory(&config.expand_home(project))
            .map_err(|_| Error::new(1, format!("no such directory: {project}")))?
            .to_string_lossy()
            .into_owned();
        return Ok((directory.clone(), folder_name(&directory)));
    }
    let projects = config.projects()?;
    if let Some(found) = projects.iter().find(|p| p.name == *project) {
        let directory = config.expand_home(&found.dir);
        if !Path::new(&directory).is_dir() {
            return Err(Error::new(
                1,
                format!("project '{project}' points to missing dir {directory}"),
            ));
        }
        return Ok((directory, project.clone()));
    }
    let session = format!(
        "{}/{}{}",
        basename(command),
        sanitize(project),
        if options.tag.is_empty() {
            String::new()
        } else {
            format!("/{}", options.tag)
        }
    );
    if session_exists(&session) {
        return Ok((String::new(), project.clone()));
    }
    Err(Error::new(
        1,
        format!(
            "unknown project '{project}' and no session {}/{}; known: {}",
            basename(command),
            sanitize(project),
            projects
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ),
    ))
}
fn launch(
    config: &Config,
    command: &str,
    args: &[String],
    client: &str,
    global_dry: bool,
) -> Result<i32> {
    let mut options = LaunchOptions::parse(args, global_dry)?;
    let (directory, project) = resolve_project(config, command, &options)?;
    let command_name = basename(command);
    // This launch's own mode becomes the agent's native flag, which the
    // account default then leaves alone.
    if let Some(mode) = options.permissions.as_deref() {
        // Syntax and static shortcuts only. Fresh native options are checked
        // once when the launch actually builds its flags.
        crate::accounts::validate_permission_mode(&command_name, mode)?;
        if crate::accounts::explicit_permissions(&command_name, &options.extra) {
            return Err(Error::new(1, "choose --permissions or the agent's own permission flags, not both"));
        }
    }
    if command_name=="opencode" {
        if options.account.as_deref().is_some_and(|id|id!="native-opencode") {return Err(Error::new(1,"Choose native-opencode; separate OpenCode profiles are not supported"));}
        if options.resume || !options.extra.is_empty() || options.fresh || options.remote_control {return Err(Error::new(1,"Use saved HGS session names to resume OpenCode; select a model with --model provider/model"));}
        if options.effort.is_some() && options.model.is_none() {return Err(Error::new(1,"--effort needs --model"));}
        // U60 (#658 N1): OpenCode has no per-session provider contract; a model
        // server session on it would need a global provider entry, so it is not
        // offered. Codex and Claude Code get session-scoped settings instead.
        if options.model_server.is_some() {
            return Err(Error::new(1, "OpenCode is not offered for a model server; start Codex or Claude Code"));
        }
        let project=if project=="/" {"root".into()}else{sanitize(&project)};
        let tag=if options.tag.is_empty(){"main"}else{&options.tag};let name=format!("opencode/{project}/{tag}");
        state::opencode::launch(&name,&directory,options.launch_id.as_deref(),options.model.as_deref(),options.effort.as_deref(),options.dry,options.new_only).map_err(|e|Error::new(1,e))?;
        if !options.dry {println!("OpenCode session ready: {name}\nOpen this session in Zerus.");}return Ok(0);
    }
    if options.model_server.is_some() {
        if options.model.is_none() { return Err(Error::new(1, "--model-server needs --model MODEL")); }
        if options.local_model.is_some() { return Err(Error::new(1, "Choose --model-server or --local-model, not both")); }
        if options.resume || options.resume_id.is_some() || options.fresh {
            return Err(Error::new(1, "--model-server starts a new session; resume keeps the conversation's original provider"));
        }
    } else if options.model.is_some() || options.effort.is_some() {return Err(Error::new(1,"--model and --effort are OpenCode launch options; other agent flags go after --"));}
    if command_name == "dsh" {
        if options
            .account
            .as_deref()
            .is_some_and(|id| id != "native-dsh")
        {
            return Err(Error::new(
                1,
                "Choose the native DeepSeek account; manage its login in the native UI",
            ));
        }
        if options.resume_id.is_some() || !options.extra.is_empty() || options.fresh {
            return Err(Error::new(
                1,
                "Use HGS session names to resume DeepSeek; native CLI flags belong in dsh itself",
            ));
        }
        let project = if project == "/" {
            "root".into()
        } else {
            sanitize(&project)
        };
        let name = format!(
            "dsh/{project}{}",
            if options.tag.is_empty() {
                String::new()
            } else {
                format!("/{}", options.tag)
            }
        );
        state::dsh::launch_with_permissions(
            &name,
            &directory,
            options.launch_id.as_deref(),
            options.permissions.as_deref(),
            options.dry,
            options.new_only,
        )
        .map_err(|e| Error::new(1, e))?;
        if !options.dry {
            println!("DeepSeek session ready: {name}\nOpen this session in Zerus.");
        }
        return Ok(0);
    }
    // From inside a session only a detached launch or a dry run gets here
    // (`nested_guard`). The new session takes an identity of its own: tmux
    // starts its pane from the server's environment and the Windows host drops
    // the caller's session variables (`host::SESSION_CONTEXT`), and its account
    // comes from this launch, never from the caller's profile.
    if terminal().inside() {
        crate::accounts::forget_session_profile(config);
    }
    let explicit_account = options.account.is_some();
    // Exact conversation lookup is scoped to the chosen account's native home.
    if !explicit_account && options.resume_id.is_some() {
        options.account = crate::accounts::default_account(config, &command_name)?;
    }
    let mut account_environment = if let Some(id) = &options.account {
        crate::accounts::select(config, id, &command_name)?
    } else {
        Vec::new()
    };
    let requested_session = if let Some(id) = &options.resume_id {
        if !matches!(command_name.as_str(), "claude" | "codex" | "kimi") {
            return Err(Error::new(
                1,
                "resuming a conversation ID requires claude, codex or kimi",
            ));
        }
        if options.tag.is_empty() {
            let found = state::find_requested_session(&command_name, id)
                .map_err(|error| Error::new(1, error))?;
            options.tag = format!("resume-{id}");
            found
        } else {
            None
        }
    } else {
        None
    };
    let project = if project == "/" {
        "root".into()
    } else {
        sanitize(&project)
    };
    let session = requested_session.unwrap_or_else(|| {
        format!(
            "{command_name}/{project}{}",
            if options.tag.is_empty() {
                String::new()
            } else {
                format!("/{}", options.tag)
            }
        )
    });
    let existing = session_exists(&session);
    if explicit_account
        && (existing || (!options.fresh && binding_exists(config, &session)?))
    {
        return Err(Error::new(1, "This session already has an account. Open it from Sessions, or choose a new session name."));
    }
    let mut requested_saved = false;
    if let Some(id) = &options.resume_id {
        if existing || binding_exists(config, &session)? {
            requested_saved = state::check_requested_session(&session, &command_name, id)
                .map_err(|error| Error::new(1, error))?;
        }
    }
    // WhiteWh fork: one tracked session per conversation.
    if let Some(id) = options.resume_id.as_ref().filter(|_| options.new_only) {
        if let Some(tracked) = state::find_requested_session(&command_name, id).map_err(|error| Error::new(1, error))? {
            return Err(Error::new(1, format!("conversation {id} is already open as {tracked}; open it from Sessions")));
        }
    }
    if options.new_only && (existing || binding_exists(config, &session)?) {
        return Err(Error::new(
            1,
            format!(
                "session '{session}' already exists; choose another name or open it from Sessions"
            ),
        ));
    }
    // Attaching/restoring a named session keeps its saved account binding.
    if !explicit_account && options.resume_id.is_none() && !existing
        && (options.fresh || !binding_exists(config, &session)?) {
        options.account = crate::accounts::default_account(config, &command_name)?;
        if let Some(id) = &options.account {
            account_environment = crate::accounts::select(config, id, &command_name)?;
        }
    }
    // WhiteWh fork: this session's model of a local model account, then its
    // endpoint, which starts with the session.
    if let Some(model) = options.local_model.as_deref() {
        let id = options.account.as_deref().ok_or_else(|| Error::new(1, "--local-model needs --account"))?;
        account_environment.extend(crate::accounts::local_model_session(config, id, &command_name, model)?);
    }
    // U60: this session runs on a model server's API; the settings are this
    // launch's environment only, so the global agent configuration is untouched.
    // The key is not part of that environment: it goes to a private file that
    // `hgs --run` reads and removes, so no command line, tmux start command or
    // dry-run carries it (#658 N3).
    let mut model_server_args: Vec<String> = Vec::new();
    let mut model_server_secrets: Vec<String> = Vec::new();
    let mut model_server_name = String::new();
    if let Some(server) = options.model_server.as_deref() {
        let model = options.model.as_deref().ok_or_else(|| Error::new(1, "--model-server needs --model MODEL"))?;
        let settings = crate::state::model_servers::session_settings(config, server, &command_name, model)?;
        account_environment.extend(settings.env);
        model_server_args = settings.args;
        model_server_secrets = settings.secrets;
        model_server_name = server.to_owned();
    }
    if !options.dry {
        if let Some(id) = &options.account {
            crate::accounts::prepare_local_model(config, id, &command_name)?;
        }
    }
    #[cfg(unix)]
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let (mut base, mut resume): (Vec<String>, Vec<String>) = match command_name.as_str() {
        "claude" | "kimi" => (
            vec![command_name.clone()],
            vec![command_name.clone(), "-c".into()],
        ),
        "codex" => (
            vec!["codex".into()],
            vec!["codex".into(), "resume".into(), "--last".into()],
        ),
        #[cfg(unix)]
        "sh" => (vec![shell.clone(), "-l".into()], Vec::new()),
        // PowerShell, untracked like the Unix login shell (spec D8).
        #[cfg(windows)]
        "sh" => (
            platform::powershell().map_err(|e| Error::new(1, e))?,
            Vec::new(),
        ),
        _ => (vec![command.into()], Vec::new()),
    };
    // U60: the server's model is this launch's flag (Claude takes its model from
    // the environment, which the settings above already set).
    if command_name == "codex" {
        base.extend(model_server_args.iter().cloned());
    }
    // The owner, 2026-11-02: Claude's args (the /model picker's --settings file)
    // reached no command line, so its picker offered one model. They apply to a
    // start and to a resume alike. A Claude account whose own settings point at
    // a gateway (GB10) gets the gateway's models the same way.
    if command_name == "claude" {
        if options.model_server.is_none() {
            let home = account_environment
                .iter()
                .find_map(|line| line.strip_prefix("CLAUDE_CONFIG_DIR="))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&config.home).join(".claude"));
            if let Some(server) = crate::state::model_servers::account_gateway(&home) {
                let model = options.model.clone().unwrap_or_default();
                if let Some(path) = crate::state::model_servers::picker_settings(config, &server, &model) {
                    model_server_args.extend(["--settings".to_owned(), path.to_string_lossy().into_owned()]);
                }
            }
        }
        base.extend(model_server_args.iter().cloned());
        if !resume.is_empty() {
            resume.extend(model_server_args.iter().cloned());
        }
    }
    let launcher = Path::new(&directory).join(format!("run_{command_name}.sh"));
    if !options.bare && !directory.is_empty() && platform::executable(&launcher) {
        base[0] = launcher.to_string_lossy().into_owned();
        if !resume.is_empty() {
            resume[0] = base[0].clone();
        }
    }
    if let Some(id) = &options.resume_id {
        base.extend([
            match command_name.as_str() {
                "codex" => "resume",
                "claude" => "--resume",
                "kimi" => "--session",
                _ => unreachable!(),
            }
            .into(),
            id.clone(),
        ]);
    }
    let mut auto = options.resume && !options.fresh && options.resume_id.is_none();
    if auto && resume.is_empty() {
        eprintln!("hgs: -c: no resume mode known for {command_name}; starting it as is");
        auto = false;
    }
    if command_name == "claude" && auto {
        let history_root = env::var("HGS_CLAUDE_PROJECTS").unwrap_or_else(|_| {
            let home = env::var("CLAUDE_CONFIG_DIR")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("{}/.claude", config.home));
            Path::new(&home)
                .join("projects")
                .to_string_lossy()
                .into_owned()
        });
        let escaped: String = directory
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        auto = fs::read_dir(Path::new(&history_root).join(escaped))
            .ok()
            .is_some_and(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .any(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
            });
    }
    if !options.extra.is_empty() {
        match command_name.as_str() {
            "codex" => auto = false,
            "claude"
                if options
                    .extra
                    .iter()
                    .any(|arg| matches!(arg.as_str(), "-c" | "--continue" | "-r" | "--resume")) =>
            {
                auto = false
            }
            "kimi"
                if options.extra.iter().any(|arg| {
                    matches!(arg.as_str(), "-c" | "--continue" | "-S" | "--session")
                }) =>
            {
                auto = false
            }
            _ => {}
        }
        base.extend(options.extra.clone());
        if !resume.is_empty() {
            resume.extend(options.extra.clone());
        }
    }
    let tracked_permissions = matches!(command_name.as_str(), "claude" | "codex" | "kimi")
        && env::var("HGS_TRACKING").as_deref() != Ok("0");
    if let Some(mode) = options.permissions.as_deref() {
        // A tracked agent receives the symbolic choice through
        // HGS_PERMISSION_MODE; the lifecycle applies and revalidates it on every
        // resume. Untracked and dry-run launches still need concrete argv.
        if !tracked_permissions || options.dry {
            let values = crate::accounts::permission_args(&command_name, mode)?;
            crate::accounts::apply_permission_values(&command_name, &values, &mut base);
            if !resume.is_empty() {
                crate::accounts::apply_permission_values(&command_name, &values, &mut resume);
            }
        }
    }
    // Capture the account defaults in the new conversation's launch recipe.
    // Saved resumes/forks and explicit native choices retain their own settings.
    if !existing && !auto && options.resume_id.is_none()
        && (options.fresh || !binding_exists(config, &session)?)
        && ["codex", "claude", "kimi"].contains(&command_name.as_str()) {
        crate::accounts::apply_model_defaults(config, options.account.as_deref().unwrap_or(""), &command_name, &mut base)?;
    }
    if options.remote_control {
        if command_name == "claude" {
            base.extend(["--remote-control".into(), session.clone()]);
            resume.extend(["--remote-control".into(), session.clone()]);
        } else {
            eprintln!("hgs: --rc is claude-only; ignored for {command_name}");
        }
    }
    let executable = executable_path()?.to_string_lossy().into_owned();
    let hint_environment = crate::config::agent_hint_environment(&command_name);
    #[cfg(unix)]
    let supervisor_environment = account_environment
        .iter().chain(&hint_environment).cloned().collect::<Vec<_>>();
    // The Unix login shell is the pane's root process. On Windows PowerShell
    // runs under `--run` as well: the host starts every root process as a new
    // process group, which ignores Ctrl+C together with everything it starts,
    // and only the supervisor turns Ctrl+C back on for its child (spec D11).
    let command = if command_name == "sh" && cfg!(unix) {
        base
    } else {
        #[cfg(unix)]
        let mut words = vec![
            shell,
            "-lic".into(),
            crate::accounts::supervisor_script(&supervisor_environment),
            executable.clone(),
        ];
        // The supervisor starts directly, with the session's variables, which
        // carry the account (spec D8).
        #[cfg(windows)]
        let mut words = vec![executable.clone(), "--run".to_owned()];
        if auto {
            words.push(resume.len().to_string());
            words.extend(resume);
        } else {
            words.push("0".into());
        }
        words.extend(base);
        words
    };
    if existing {
        if options.dry {
            if !options.detached && config.tab {
                println!(
                    "tab {session} color={}",
                    platform::tab_color(config, "local")
                );
            }
            return attach(config, &session, client, true);
        }
        if let Some(id) = &options.resume_id {
            eprintln!("hgs: conversation {id} is running as {session}");
        } else if options.fresh
            || options.resume
            || options.remote_control
            || options.permissions.is_some()
            || !options.extra.is_empty()
        {
            eprintln!("hgs: session {session} exists, attaching; launch options ignored");
        }
        if options.detached {
            println!("hgs: session {session} already running");
            return Ok(0);
        }
        if let Some(id) = &options.resume_id {
            return attach_requested(config, &session, client, &command_name, id);
        }
        return attach(config, &session, client, false);
    }
    let mut environment = vec![format!("HGS_CLIENT={client}")];
    if let Some(id) = &options.launch_id {
        // Set only on the session actually created by this command. A name
        // collision must never claim another session for the GUI's group.
        environment.push(format!("HGS_LAUNCH_ID={id}"));
    }
    environment.extend(account_environment);
    // The record names the server; the key reaches the agent only through the
    // private file `hgs --run` reads and removes.
    if !model_server_name.is_empty() {
        environment.push(format!("HGS_MODEL_SERVER={model_server_name}"));
        environment.push(format!("HGS_MODEL_SERVER_MODEL={}", options.model.as_deref().unwrap_or_default()));
    }
    environment.extend(hint_environment);
    environment.extend(platform::gui_env());
    if !options.fresh
        && (requested_saved || (options.resume_id.is_none() && binding_exists(config, &session)?))
    {
        if !options.detached && !options.dry {
            terminal().attachable()?;
        }
        env::set_var("HGS_CLIENT", client);
        if let Some(id) = &options.resume_id {
            env::set_var("HGS_REQUESTED_ID", id);
        } else {
            env::remove_var("HGS_REQUESTED_ID");
        }
        let mut state_args = vec![session.clone()];
        if options.dry {
            state_args.push("--dry-run".into());
        }
        if let Some(mode) = options.permissions.as_deref() {
            env::set_var("HGS_PERMISSION_MODE", mode);
        }
        let rc = state_command("resume", &state_args)?;
        if options.permissions.is_some() {
            env::remove_var("HGS_PERMISSION_MODE");
        }
        return if rc == 0 && !options.detached && !options.dry {
            attach(config, &session, client, false)
        } else {
            Ok(rc)
        };
    }
    let mut spec = LaunchSpec {
        name: session.clone(),
        cwd: directory,
        env: environment,
        command,
        detached: options.detached,
    };
    if options.dry {
        if !options.detached && config.tab {
            println!(
                "tab {session} color={}",
                platform::tab_color(config, "local")
            );
        }
        println!("{}", terminal().describe(&DryRun::Launch(&spec)));
        return Ok(0);
    }
    // U60 (#709 L1): the key file is written only here, right before the
    // backend launch, and removed if that launch fails; `hgs --run` removes it
    // on success. The resume branch above returned before this point, and a
    // dry-run returned before it too, so neither leaves a file.
    let mut secret_path: Option<std::path::PathBuf> = None;
    if !model_server_secrets.is_empty() {
        let path = crate::state::model_servers::write_secret_file(&config.state, &model_server_secrets)?;
        spec.env.push(format!("HGS_MODEL_SERVER_ENV={}", path.display()));
        secret_path = Some(path);
    }
    if matches!(command_name.as_str(), "claude" | "codex" | "kimi")
        && env::var("HGS_TRACKING").as_deref() != Ok("0")
    {
        if let Some(mode) = options.permissions.as_deref() {
            spec.env.push(format!("HGS_PERMISSION_MODE={mode}"));
        }
        spec.env.extend([
            format!("HGS_SESSION={session}"),
            format!("HGS_AGENT={command_name}"),
            format!(
                "HGS_ACCOUNT_ID={}",
                options.account.as_deref().unwrap_or("")
            ),
            format!("HGS_STATE_DIR={}", config.state.display()),
            format!("HGS_EXECUTABLE={executable}"),
            "HGS_RUN_ID=".into(),
            "HGS_EXPECTED_ID=".into(),
            "HGS_ARCHIVE_ID=".into(),
            "HGS_FRESH_REPLACES=".into(),
            format!("HGS_FRESH={}", if options.fresh { "1" } else { "0" }),
            format!(
                "HGS_REQUESTED_ID={}",
                options.resume_id.as_deref().unwrap_or("")
            ),
            "HGS_TRACKING=1".into(),
        ]);
        for key in ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "KIMI_CODE_HOME"] {
            spec.env
                .push(format!("{key}={}", env::var(key).unwrap_or_default()));
        }
    }
    if options.detached {
        let rc = match terminal().launch(&spec) {
            Ok(exit) => exit.code,
            Err(error) => {
                if let Some(path) = &secret_path {
                    let _ = std::fs::remove_file(path);
                }
                return Err(error);
            }
        };
        if rc == 0 {
            if let Some(id) = &options.launch_id {
                // A session that ends before a listing shows it still ends
                // its launch's placeholder (`state::launches`).
                if let Err(error) = state::launches::started(id, &session) {
                    eprintln!("hgs: cannot record launch {id}: {error}");
                }
            }
            println!("hgs: started {session}");
        }
        return Ok(rc);
    }
    let _tab = TabGuard::begin(config, &session, &platform::tab_color(config, "local"));
    let start = Instant::now();
    let exit = match terminal().launch(&spec) {
        Ok(exit) => exit,
        Err(error) => {
            if let Some(path) = &secret_path {
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
    };
    let rc = exit.code;
    if !report_startup_failure(&session)
        && !exit.server_died
        && !session_exists(&session)
        && start.elapsed().as_secs() < 3
    {
        eprintln!("hgs: session {session} ended immediately — command not found on this box? try --dry-run");
    }
    Ok(rc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_rules_are_forwarded_without_a_terminal_or_launch_client() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config { home:String::new(),dir:dir.path().into(),state:dir.path().join("state"),selves:vec!["self".into()],
            peers:vec!["peer".into()],base_peers:vec![],peers_from_environment:false,machines:Default::default(),tab:false,tab_colors:String::new() };
        for windows in [false,true] {
            crate::windows_peer::record(&config,"peer",windows).unwrap();
            for op in ["get","set","migrate"] {
                assert_eq!(remote(&config,"peer","rules",&[op.into(),"--json".into()],true).unwrap(),0);
                let (flags,line) = REMOTE_DRY_PLAN.with(|plan| plan.borrow_mut().take().unwrap());
                assert!(flags.iter().any(|flag|flag=="BatchMode=yes"));
                assert!(!flags.iter().any(|flag|flag=="-t"));
                assert!(line.contains("iron-rules-v1") && line.contains("rules") && line.contains(op),"{line}");
                assert!(!line.contains("--client") && !line.contains("__state"),"{line}");
                assert_eq!(crate::peer_channel::routed("rules", &[op.into(),"--json".into()]),Some(true));
            }
        }
    }

    #[test]
    fn repository_preparation_does_not_admit_session_commands_on_control_consoles() {
        for command in ["ls", "machine", "presence", "swarm", "project", "account",
            "speech", "analytics", "graph", "wait", "team", "mobile", "hooks", "repository", "session-action-status"] {
            assert!(!needs_local_sessions(command), "{command}");
        }
        for command in ["a", "kill", "send", "send-now", "send-cancel", "inspect", "history",
            "history-projection", "processes", "terminal", "session-action", "interrupt",
            "clear-context", "compact-context", "answer", "effort", "settings", "recovery",
            "read", "notifications", "voice", "schedules", "attachment", "file-read",
            "claude", "codex", "kimi", "sh", "resume", "pause", "fork", "reference",
            "archive", "terminate", "dirs", "worktrees", "git-status", "__state", "__host",
            "repository-git-worker", "Repository", "repository-extra", ""] {
            assert!(needs_local_sessions(command), "{command}");
        }
    }

    /// Settings > Sessions runs `hgs subagents-confirm` and `hgs
    /// session-upload-block`: settings this computer keeps, read from its hgs
    /// state like `hgs __state`, never an agent launch, and never sent to a peer.
    #[test]
    fn kept_settings_reach_hgs_state_instead_of_a_launch() {
        let dir = tempfile::tempdir().unwrap();
        state::with_test_state_root(dir.path(), || {
            let get = ["get", "--json"].map(String::from);
            for command in ["subagents-confirm", "session-upload-block"] {
                assert!(kept_setting(command), "{command}");
                assert!(usage_text(cfg!(unix)).contains(&format!("\n       hgs {command} get|set --json   ")), "{command}");
                assert_eq!(setting_command(true, command, &get, false).unwrap(), 0, "{command}");
                // A malformed request gets the setting's own usage, not the
                // launch's "unknown option --json".
                let error = setting_command(true, command, &get[..1], false).unwrap_err();
                assert_eq!(error.message, format!("usage: hgs {command} get|set --json"));
                let error = setting_command(false, command, &get, false).unwrap_err();
                assert_eq!(error.message, format!("hgs {command} changes the computer it runs on; run it on that computer"));
                let error = setting_command(true, command, &get, true).unwrap_err();
                assert_eq!(error.message, format!("hgs {command} does not support --dry-run"));
            }
        });
        for command in ["claude", "sh", "subagents", "session-upload", "__state"] {
            assert!(!kept_setting(command), "{command}");
        }
    }

    /// A WSL profile of this computer gets every other command, but not the
    /// settings this computer keeps: they are refused before the forward.
    #[test]
    fn kept_settings_never_go_to_a_wsl_profile() {
        let mut config = Config {
            home: String::new(),
            dir: PathBuf::new(),
            state: PathBuf::new(),
            selves: vec!["this-box".into()],
            peers: Vec::new(),
            base_peers: Vec::new(),
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        config.machines.insert("ubuntu".into(), crate::machines::Connection {
            transport: crate::wsl::Transport::WslLocal,
            wsl: Some(crate::wsl::WslConnection { distribution: "Ubuntu".into(), user: "dev".into(), hgs_path: String::new() }),
            ..Default::default()
        });
        config.machines.insert("peer".into(), crate::machines::Connection::default());
        let get = ["get", "--json"].map(String::from);
        for command in ["subagents-confirm", "session-upload-block"] {
            let refused = wsl_forward(&config, "ubuntu", command, &get, false).unwrap().unwrap_err();
            assert_eq!(refused.message, format!("hgs {command} changes the computer it runs on; run it on that computer"));
            // Neither a configured SSH peer nor this computer is a WSL profile.
            for host in ["", "this-box", "peer"] {
                assert!(wsl_forward(&config, host, command, &get, false).is_none(), "{host}");
            }
        }
    }

    #[test]
    fn shared_notifications_advertises_its_exact_standalone_marker_on_every_platform() {
        for tmux in [false, true] {
            let help = usage_text(tmux);
            assert_eq!(
                help.lines()
                    .filter(|line| line.trim() == "session-notifications-v1")
                    .count(),
                1
            );
            assert!(help.contains("notifications <session> --json"));
        }
    }

    #[test]
    fn session_names_take_the_on_disk_case_of_the_same_folder_only() {
        let name = |logical: &str, canonical: Option<&str>| {
            on_disk_name(logical.into(), canonical.map(str::to_owned))
        };
        // `cd C:\work\myproj` names the session after the folder on disk.
        assert_eq!(name("myproj", Some("MyProj")), "MyProj");
        assert_eq!(name("SITE", Some("site")), "site");
        assert_eq!(name("ölaf", Some("Ölaf")), "Ölaf");
        assert_eq!(name("MyProj", Some("MyProj")), "MyProj");
        // A junction, link or short name with another name keeps its own.
        assert_eq!(name("site", Some("example-site")), "site");
        assert_eq!(name("PROGRA~1", Some("Program Files")), "PROGRA~1");
        assert_eq!(name("myproj", Some("myproj2")), "myproj");
        assert_eq!(name("myproj2", Some("MyProj")), "myproj2");
        // Only letter case may differ, never the letters themselves.
        assert_eq!(name("STRASSE", Some("straße")), "STRASSE");
        // A folder that cannot be resolved keeps the name it was given.
        assert_eq!(name("myproj", None), "myproj");
        // Unix names stay exactly as given, even for a link to another case.
        #[cfg(unix)]
        {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("Site")).unwrap();
            let link = root.path().join("site");
            // A case-insensitive file system (macOS) already finds `Site` as `site`.
            if !link.exists() {
                std::os::unix::fs::symlink("Site", &link).unwrap();
            }
            assert_eq!(folder_name(&link.to_string_lossy()), "site");
        }
    }

    #[test]
    fn inside_a_session_only_tmux_is_named_and_only_a_detached_launch_starts() {
        // Unix keeps tmux's wording byte for byte.
        assert_eq!(
            nested_refusal(true, Some("sh/MyProj/work-1"), None),
            "already inside tmux session sh/MyProj/work-1; open a new tab"
        );
        assert_eq!(
            nested_refusal(true, Some(""), None),
            "already inside tmux session ; open a new tab"
        );
        assert_eq!(
            nested_refusal(true, None, Some("fork -d")),
            "already inside tmux; use fork -d or open a new tab"
        );
        assert_eq!(
            nested_refusal(true, None, Some("resume -d")),
            "already inside tmux; use resume -d or open a new tab"
        );
        assert_eq!(
            nested_refusal(true, Some("sh/MyProj/work-1"), Some(DETACHED_LAUNCH)),
            "already inside tmux session sh/MyProj/work-1; use --new -d or open a new tab"
        );
        // A Windows host session is named as the host resolves it, without tmux.
        assert_eq!(
            nested_refusal(false, Some("sh/MyProj/work-1"), None),
            "already inside session sh/MyProj/work-1; open a new tab"
        );
        assert_eq!(
            nested_refusal(false, Some(""), None),
            "already inside a session; open a new tab"
        );
        assert_eq!(
            nested_refusal(false, None, Some("fork -d")),
            "already inside a session; use fork -d or open a new tab"
        );
        assert_eq!(
            nested_refusal(false, None, Some("resume -d")),
            "already inside a session; use resume -d or open a new tab"
        );
        assert_eq!(
            nested_refusal(false, Some("sh/MyProj/work-1"), Some(DETACHED_LAUNCH)),
            "already inside session sh/MyProj/work-1; use --new -d or open a new tab"
        );

        // On either platform a launch inside a session starts only detached,
        // as `fork -d` and `resume -d` do: one that would attach this terminal
        // is refused and names the detached form; a dry run is not refused.
        struct Inside;
        impl TerminalBackend for Inside {
            fn inside(&self) -> bool {
                true
            }
            fn enclosing_session(&self) -> String {
                "sh/MyProj/work-1".into()
            }
        }
        struct Outside;
        impl TerminalBackend for Outside {}
        let launch = |words: &[&str], dry| {
            let args = words.iter().map(|word| word.to_string()).collect::<Vec<_>>();
            LaunchOptions::parse(&args, dry)
        };
        // What `hgs guide launch` tells agents to run, and shorter forms.
        let id = "0b6f4c9e-3c1d-4b8f-9a35-6f2e2d6f6a10";
        let guide = ["/abs/MyProj", "--new", "-n", "x", "-d", "--launch-id", id];
        let guide = [&guide[..], &["--", "prompt"]].concat();
        let detached: [&[&str]; 3] = [
            &guide,
            &["MyProj", "--new", "-n", "x", "-d"],
            &["MyProj", "-n", "x", "-d"],
        ];
        let without_d = guide.iter().copied().filter(|word| *word != "-d");
        let without_d = without_d.collect::<Vec<_>>();
        let attached: [&[&str]; 3] = [
            &without_d,
            &["MyProj", "--new", "-n", "x"],
            &["MyProj", "-n", "x"],
        ];
        state::backend::with_fake(&Inside, || {
            for words in detached {
                assert!(launch(words, false).unwrap().detached, "{words:?}");
            }
            let options = launch(detached[0], false).unwrap();
            assert!(options.new_only);
            assert_eq!(options.launch_id.as_deref(), Some(id));
            assert_eq!(options.extra, ["prompt"]);
            for words in attached {
                let error = launch(words, false).err().unwrap();
                assert_eq!(error.code, 2, "{words:?}");
                assert_eq!(
                    error.message,
                    nested_refusal(cfg!(unix), Some("sh/MyProj/work-1"), Some(DETACHED_LAUNCH))
                );
                assert!(launch(words, true).is_ok(), "{words:?}");
            }
            // The other launch rules still apply to a detached launch there.
            for words in [
                &["MyProj", "-n", "x", "-d", "--launch-id", id][..],
                &["MyProj", "--new", "-n", "x/y", "-d"],
                &["MyProj", "--new", "-c", "-d"],
            ] {
                assert_eq!(launch(words, false).err().unwrap().code, 1, "{words:?}");
            }
            // Attaching names no detached form.
            let error = nested_guard(false, None).err().unwrap();
            assert_eq!(error.code, 2);
            assert_eq!(
                error.message,
                nested_refusal(cfg!(unix), Some("sh/MyProj/work-1"), None)
            );
            assert!(nested_guard(true, None).is_ok());
        });
        state::backend::with_fake(&Outside, || {
            for words in detached.into_iter().chain(attached) {
                assert!(launch(words, false).is_ok(), "{words:?}");
            }
            assert!(nested_guard(false, None).is_ok());
        });
    }

    #[test]
    fn crashes_and_stops_of_a_resume_are_final_and_quick_failures_start_fresh() {
        // A quick ordinary failure means nothing to resume.
        assert!(!resume_stands(1, 0, 5, false));
        assert!(!resume_stands(127, 4, 5, false));
        // Success, a run long enough to have resumed, a signal or crash.
        assert!(resume_stands(0, 0, 5, false));
        assert!(resume_stands(1, 5, 5, false));
        assert!(resume_stands(130, 0, 5, false));
        // Windows: Ctrl+Break or a closed console (STATUS_CONTROL_C_EXIT), an
        // access violation, a missing DLL (spec D9) all read as signals.
        for status in [0xC000_013A_u32, 0xC000_0005, 0xC000_0135] {
            let rc = platform::ntstatus_exit(status).exit_code();
            assert!(rc >= 128, "{status:#x}");
            assert!(resume_stands(rc, 0, 5, false), "{status:#x}");
        }
        // exit(-1) stays an ordinary failure.
        let rc = platform::ntstatus_exit(u32::MAX).exit_code();
        assert_eq!(rc, -1);
        assert!(!resume_stands(rc, 0, 5, false));
        // A paused agent that exits with an ordinary failure was still stopped.
        assert!(resume_stands(1, 0, 5, true));
    }

    #[test]
    fn usage_names_tmux_only_where_it_hosts_the_sessions() {
        // Unix keeps tmux's text byte for byte, capability markers included.
        assert_eq!(usage_text(true), USAGE);
        let clone_marker = zerus_client_core::desktop::creation_clone::HELP_MARKER;
        assert!(USAGE.lines().any(|line| line.trim() == clone_marker));
        // Windows has no `hgs paste`, `hgs clip` or `hgs notify`, no Ctrl+V
        // that pulls a peer's image, and no POSIX creation-clone executor.
        // Its dry runs print the host's requests; every other line stays as is.
        let neutral = usage_text(false);
        assert!(!neutral.contains("tmux"), "{neutral}");
        assert!(!neutral.contains("Ctrl+V"), "{neutral}");
        assert!(!neutral.lines().any(|line| line.trim() == clone_marker));
        assert_eq!(
            neutral,
            USAGE
                .replace(
                    "       hgs paste <session> <pane> [client]   the tmux C-v binding; not for hand use\n",
                    ""
                )
                .replace(
                    "       hgs clip                       PNG from this box's clipboard to stdout (rc 1: no image)\n",
                    ""
                )
                .replace(
                    "       hgs notify [--bell] <session> [text]   event for the tray (no tray = no-op)\n",
                    ""
                )
                .replace(
                    "       Desktop creation clone ABI: desktop-creation-clone-v1\n",
                    ""
                )
                .replace(
                    "  --dry-run  print the tmux/ssh command\n",
                    "  --dry-run  print the session-host/ssh command\n"
                )
                .replace(
                    "Ctrl+V in a session attached from a peer pulls the image from that box's clipboard.\n",
                    ""
                )
        );
    }

    #[test]
    fn new_exact_resume_preserves_the_whitewh_contract() {
        let id = "11111111-2222-4333-8444-555555555555";
        for flag in ["-c", "--resume", "--continue"] {
            let args = ["--new", flag, id, "-n", "exact-new"].map(String::from);
            let parsed = LaunchOptions::parse(&args, true).unwrap();
            assert!(parsed.new_only && parsed.resume);
            assert_eq!(parsed.resume_id.as_deref(), Some(id));
            assert_eq!(parsed.tag, "exact-new");
        }
        let args = vec![format!("--resume={id}"), "--new".into()];
        assert_eq!(LaunchOptions::parse(&args, true).unwrap().resume_id.as_deref(), Some(id));
    }

    #[test]
    fn new_session_refuses_ambiguous_resume_and_fresh() {
        for args in [["--new", "-c"], ["--new", "--resume"], ["--new", "--fresh"]] {
            assert!(LaunchOptions::parse(&args.map(String::from), true).is_err());
        }
        let args = ["--new", "-c", "11111111-2222-4333-8444-555555555555", "--fresh"].map(String::from);
        assert!(LaunchOptions::parse(&args, true).is_err());
    }

    #[test]
    fn usage_lists_opencode_effort_and_the_explicit_service_start() {
        let usage = usage_text(true);
        // `--effort` is an OpenCode launch option that hgs refuses without
        // `--model` or with an empty value; omitting it keeps the model's default.
        assert!(usage.contains(
            "\n  --effort VARIANT   OpenCode effort; needs --model, one of that model's effort_options (omit for the model's default)\n"
        ), "{usage}");
        // The only explicit start of the shared service; inspection never starts it.
        assert!(usage.contains(
            "\n       hgs [@host] account start native-opencode   start the shared OpenCode service if none runs; inspection never starts it\n"
        ), "{usage}");
        // Clients look for these commands before offering stars and All models.
        assert!(usage.contains(
            "\n       hgs [@host] account favorite native-opencode --model PROVIDER/MODEL --favorite on|off --request-id UUID   star an OpenCode model on that computer\n"
        ), "{usage}");
        assert!(usage.contains(
            "\n       hgs [@host] account models native-opencode [--location DIR]   every OpenCode model of that computer and folder as JSON; never starts OpenCode\n"
        ), "{usage}");
    }

    #[test]
    fn unix_only_commands_are_refused_elsewhere_instead_of_launched() {
        for command in ["notify", "clip", "paste"] {
            // Unix dispatches them as before.
            assert!(unix_only(command, true).is_ok(), "{command}");
            // Windows refuses each with exit code 2 before the launch arm.
            let error = unix_only(command, false).unwrap_err();
            assert_eq!(error.code, 2);
            assert_eq!(
                error.message,
                format!("{command} is not supported on Windows")
            );
        }
        // Any other word stays a command or a program to launch.
        for command in ["claude", "sh", "ls", "clipboard", "pastes", "notify-send"] {
            assert!(unix_only(command, false).is_ok(), "{command}");
            assert!(unix_only(command, true).is_ok(), "{command}");
        }
    }

    #[test]
    fn an_existing_attach_names_tmux_only_where_it_hosts_the_sessions() {
        // A session ID in another form than `$N` is refused before attaching.
        use state::backend::PaneInfo;
        struct Answers;
        impl TerminalBackend for Answers {
            fn probe(&self, _target: Target, _fields: &[PaneField]) -> Option<PaneInfo> {
                Some(PaneInfo::new("@1\trun-1\n".into()))
            }
        }
        let config = Config {
            home: String::new(),
            dir: PathBuf::new(),
            state: PathBuf::new(),
            selves: Vec::new(),
            peers: Vec::new(),
            base_peers: Vec::new(),
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        let args = ["claude/MyProj/work-1", "--existing"].map(String::from);
        let error =
            state::backend::with_fake(&Answers, || attach_command(&config, &args, "local", false))
                .unwrap_err();
        assert_eq!(error.code, 1);
        assert_eq!(error.message, state::backend::INVALID_SESSION_ID.text());
    }
}
