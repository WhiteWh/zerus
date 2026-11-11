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
       Desktop state preparation: desktop-state-prepare-v1
       Desktop ownership observation ABI: desktop-ownership-observation-v1
       Notification controls ABI: desktop-notification-controls-v1
       Desktop creation clone ABI: desktop-creation-clone-v1
       Desktop creation worktree ABI: desktop-creation-worktree-v1
       Desktop creation worktree receipt control: future-fence-v1
       Desktop creation launch ABI: desktop-creation-launch-v1
       Desktop task message ABI: desktop-task-message-v1
       Desktop task message references: desktop-task-message-reference-v1
       Desktop task message input review: desktop-task-message-input-review-v1
       Desktop deferred send ABI: desktop-deferred-send-v1
       Desktop deferred answer ABI: desktop-deferred-answer-v1
       Desktop deferred answer update phase: desktop-deferred-answer-update-phase-v1
       Desktop deferred compact to send ABI: desktop-deferred-compact-to-send-v1
       Desktop deferred session resume ABI: desktop-deferred-session-resume-v1
       Desktop deferred session resume Codex readiness: desktop-deferred-session-resume-codex-v1
       Desktop task start reconcile ABI: desktop-task-start-reconcile-v1
       Desktop pending read import ABI: desktop-pending-reads-v1
       Desktop activation ABI: desktop-activation-v1
       Desktop maintenance ABI: desktop-maintenance-v1
       Desktop cache maintenance ABI: desktop-cache-maintenance-v1
       Desktop termination outcome ABI: desktop-termination-outcome-v1
       Desktop startup ABI: desktop-startup-v1
       Desktop logon registration ABI: desktop-logon-task-v1
       Desktop registration refresh ABI: desktop-registration-refresh-v1
       Desktop registration mode ABI: desktop-registration-mode-v1
       Desktop registration mode review ABI: desktop-registration-mode-review-v1
       Desktop registration first mode choice ABI: desktop-registration-mode-first-choice-v1
       Desktop service mode choice scope ABI: desktop-service-mode-choice-scope-v1
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
       mobile-peer-session-id-v1
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
       hgs send <codex session> --observe --json   read whether a send to this computer's Codex session would be taken now, changing nothing
       hgs [@host] recovery action --scoped-json exact waiting-job control
       hgs [@host] history <session> --json bounded exact public history
       hgs [@host] history-projection <session> --json retired and refreshed history rows
       hgs [@host] graph snapshot --json   bounded saved relationship metadata (partial coverage)
       hgs [@host] wait begin|ack|finish|cancel|list --json   explicit independent-agent waits (JSON stdin)
       hgs [@host] terminal <session> --json scoped terminal screen/input
       hgs [@host] AGENT DIRECTORY --new -n TAG -d --launch-id UUID
       hgs [@host] session-action <session> --json scoped native lifecycle action
       hgs session-action-status <session> --json read an original exact lifecycle receipt
       hgs @host --expect-machine ID session-action-status <session> --json
       lifecycle-receipt-status-v1
       hgs [@host] <command> --session-id ID ...   select a live or saved session by its durable ID instead of its name
       session-id-selector-v1
       session-launch-identity-v1
       session-labels-v1
       hgs [@host] <command> 'zerus://session?...' ...   select the session a reference names, on every session command
       session-reference-selector-v1
       hgs [@host] compact-context <session> --json compact the idle agent context using its native command
       hgs [@host] clear-context <session> --json   clear the idle agent context using its native command
       hgs [@host] interrupt <session> --json   interrupt the current turn, keep the session
       hgs [@host] attachment <session> --request ID --index N --conversation ID   read a sent attachment
       hgs [@host] file-read <session> --json   read a document on the conversation computer
       hgs @host download --path REMOTE_PATH --output LOCAL_FILE   download a file through configured SSH
       hgs [@host] answer <session> --json   answer an exact pending question from JSON stdin
       hgs [@host] answer <session> --observe --json   read a pending question's consent identity, changing nothing
       hgs [@host] delivery receipt <original-session-name> --json   read exact original send evidence from JSON stdin
       hgs [@host] parent <session> --json   read, adopt (as the calling agent), set or clear its parent session from JSON stdin
       hgs [@host] read <session> --json   mark the current reply/attention read (JSON stdin)
       hgs [@host] subagent retire <session> --json   retire an exact stale child record (JSON stdin)
       Subagent retirement ABI: subagent-retire-v1
       hgs [@host] notifications <session> --json   shared mute preference (session-notifications-v1)
       hgs [@host] keep-warm <session> --json   keep the prompt cache warm for 1-7 hours or always (session-keep-warm-v1)
       hgs [@host] fork <session> [-n name] [-d] [--archive ID]  branch into a new conversation
       hgs [@host] effort <session> --json   change this agent's effort from JSON stdin
       hgs [@host] settings <session> --json model and effort for this session (JSON stdin)
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
       hgs swarm preference get --json   shared phone notification timing
       hgs swarm preference set mobile_delivery VALUE --json   immediate|away|away_5|away_10|away_15|away_30
       hgs machine check <alias> | ssh <alias> [--directory PATH] | setup <alias> [--source <checkout>]
       hgs [@host] account ls | add ID --provider AGENT --label NAME | rm ID | restore ID | login ID
       hgs [@host] account rename ID --label NAME | inspect ID [--refresh]
       hgs [@host] account default [ID]   show or select the default account for new sessions
       hgs [@host] account model-settings ID   inspect this account's native model catalog and launch defaults
       hgs [@host] account model-defaults ID --model MODEL [--effort LEVEL] [--revision REV]   defaults for new sessions
       hgs [@host] account permissions ID --mode bypass|provider|default [--revision REV]   default: bypass for Claude, Codex and Kimi
       hgs [@host] account permission-scope ID --scope account|computer [--revision REV]   share the permission mode across the account's computers, or keep it per computer
       hgs [@host] account mark ID [--color #RRGGBB|none] [--emoji EMOJI|none] [--if-unset] [--revision REV]   the account's color and emoji on session cards everywhere
       hgs [@host] account set-key native-dsh --json   save API key from private JSON stdin
       hgs [@host] account start native-opencode   start the shared OpenCode service if none runs; inspection never starts it
       hgs [@host] account favorite native-opencode --model PROVIDER/MODEL --favorite on|off --request-id UUID   star an OpenCode model on that computer
       hgs [@host] account models native-opencode [--location DIR]   every OpenCode model of that computer and folder as JSON; never starts OpenCode
       hgs [@host] account install codex|claude|kimi|dsh   install an agent in this terminal
       hgs [@host] account inspect --session SESSION [--refresh]
       hgs account copy ID --from MACHINE --to MACHINE --as NEW_ID [--label NAME]
       hgs [@host] worktrees --path PATH [--refresh] [--json]   existing checkouts as JSON
       hgs [@host] git-status --path PATH --json   read-only checkout/publication status
       hgs [@host] worktrees create --path REPO --branch NAME --destination PATH [--base HEAD] [--json]
       hgs [@host] worktrees create ... --request-id UUID --receipt --json   durable creation, no replay
       hgs [@host] worktrees status|cancel --request-id UUID --json   original creation receipt
       Worktree creation ABI: worktree-requests-v1
       hgs [@host] worktrees review --path PATH [--worktree WORKTREE] [--json]   cleanup verdicts and fingerprints
       hgs [@host] worktrees programs --json   bounded read-only native program manifest (stdin)
       Worktree programs ABI: worktree-programs-v1
       hgs [@host] worktrees program-holder --json   bounded read-only native checkout custody (stdin)
       Worktree program custody ABI: worktree-program-custody-v1
       Worktree program presentation ABI: worktree-program-presentation-v1
       hgs worktrees program-stop --json   original native-confirmed program stop (private stdin)
       hgs worktrees program-stop-status --json   original result only, never replay (stdin)
       Worktree program stop ABI: worktree-program-stop-v1
       hgs worktrees program-cleanup --json   original live Remove continuation (private stdin)
       hgs worktrees program-cleanup-status --json   original removal result only (stdin)
       Worktree program cleanup ABI: worktree-program-cleanup-v1
       hgs [@host] worktrees policy get|set --json   worktree-maintenance-v1 (host-local cleanup rules)
       hgs [@host] worktrees sweep --json   one bounded automatic cleanup step
       hgs [@host] worktrees inspect --path WORKTREE --common-dir DIR [--file RELATIVE_PATH] [--json]   bounded read-only changes
       hgs [@host] worktrees remove --path WORKTREE --common-dir DIR --fingerprint HASH [--ignored-fingerprint HASH] [--discard-fingerprint HASH] [--preserve-archive-restore] [--delete-branch] [--json]
       hgs [@host] worktrees keep-folder --path FOLDER --common-dir DIR --fingerprint HASH [--request-id ID] [--json]   forget a partial-removal record; the folder stays
       hgs [@host] worktrees delete-leftover --path FOLDER --common-dir DIR --fingerprint HASH --confirm-delete [--process-fingerprint HASH] [--request-id ID] [--json]   delete a reviewed leftover's files; the branch stays
       hgs [@host] worktrees leftovers [--json]   partial-removal leftovers whose repository is gone
       Worktree leftover ABI: worktree-leftovers-v1 (review "leftovers" after a partial removal, catalog "leftovers" count)
       Worktree leftover deletion ABI: worktree-leftover-delete-v1
       Worktree leftover keep ABI: worktree-leftover-keep-v1 (request-bound keep-folder outcome)
       Mobile worktree ABI: worktrees-v1 (catalog/create/verified project placement)
       hgs [@host] repository check|clone --url URL --parent PATH --name NAME --json
       hgs [@host] repository status --request-id UUID --json
       Repository acquisition ABI: repository-clone-v1
       Mobile repository acquisition ABI: repository-acquisition-v1
       Mobile Git status ABI: git-status-v1
       Mobile project assignment ABI: session-project-v1
       Mobile clear-context ABI: dismiss-prompt-v1
       session-notifications-v1
       session-keep-warm-v1
       delivery-receipt-v1
       session-parent-v1
       launch-parent-v1
       claimed-parent-v1
       hgs [@host] dirs [--hidden] [path]   directories as JSON (default: home)
       hgs [@host] dirs --create <path> [--request-id UUID]   create one folder (existing parent)
       hgs [@host] path-info --path <path>   whether a path exists, its kind and size (JSON schema 1); reads no content
       hgs [@host] kill <session> [--archive <id>]
       hgs [@host] rename <session> <new-full-name> [--archive <id>]
       hgs [@host] archives --json [--bounded]   date-filtered archive pages (JSON stdin); --bounded caps counts at 10,000 and each scan at 25,000 rows
       archive-bounded-pages-v1
       hgs [@host] archive <session>          move a stopped conversation to Archive
       hgs [@host] pause <session>|--all       save and gracefully close idle agents
       hgs [@host] resume <session>|--all [-d] [--archive <id>]   restore exact conversations
       hgs [@host] resume <session> --fresh [-d] [--expected-run-id ID]   same settings, new conversation
       hgs notify [--bell] <session> [text]   event for the tray (no tray = no-op)
       hgs project ls [--json] | add <name> <dir> | set <name> <dir> | rm <name>
       hgs task list|get|add|edit|move|stage|target|start|send|handoff|cancel|report|comment|settings …   project tasks (JSON; agents read one line)
       hgs task list --project P --stage S --limit N [--after|--before CURSOR] [--if-revision R]   one indexed page
       hgs task attachment REF --file N [--entry ID]   one task file's bytes, for metadata-only history pages
       hgs task exchange|digests --json                 the phone's task exchange (mobile)
       hgs task report-attempt REF --attempt UUID [--progress|--ready] [--considered REV]   pinned execution report (report-attempt-v1)
       hgs task begin-attempt REF --attempt UUID [--json]   inactive task-scoped acknowledgement
       hgs task control plan|execute|status --json   inactive task controls (JSON stdin)
       hgs task mobile-send|mobile-cancel|mobile-done --json   the phone's task actions (mobile)
       hgs clip                       PNG from this box's clipboard to stdout (rc 1: no image)
       hgs paste <session> <pane> [client]   the tmux C-v binding; not for hand use
  cmd      claude | codex | kimi (know --resume) | opencode (native API) | sh (login shell) | any command
  project  name from ~/.config/hgs/projects, a path, or omitted = current dir
  -c       resume the dir's last conversation (claude/kimi -c, codex resume --last)
  -c ID    resume this exact conversation UUID; --resume=ID is also accepted
  -n tag   separate session <cmd>/<project>/<tag>
  --account ID   use a named native agent profile for this session
  --model PROVIDER/MODEL   OpenCode native model for a new session
  --effort VARIANT   OpenCode effort; needs --model, one of that model's effort_options (omit for the model's default)
  --new    always create a separate session; generate a name unless -n is given
  -d       start detached (do not attach)      --fresh  explicitly replace a saved conversation
  --rc     claude --remote-control (phone)     --bare   ignore a project run_<cmd>.sh/.ps1 launcher
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
    !matches!(command, "ls" | "watch" | "machine" | "presence" | "swarm" | "project" | "account" | "speech" | "analytics" | "graph" | "wait" | "mobile" | "hooks" | "repository" | "session-action-status" | "delivery" | "subagent" | "path-info")
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
/// The refusal of every `send ... --observe` but the supported one.
const OBSERVE_ONLY: &str =
    "hgs send --observe takes exactly `hgs send <codex session> --observe --json` on this computer";

/// The one disposition of the send's effect-free observation
/// (zerus-5wmh.66.4.10, .10.1). It runs first, before the helper
/// rendezvous, any machine guard, `@host` or `--client` forwarding, a dry
/// run, a session reference or `--session-id` selection, label repair,
/// provider routing and stdin. Only the desktop service's own call goes on,
/// straight to the observation: exactly `hgs send <session> --observe
/// --json`, naming a local Codex session by its plain name. Any other argv
/// with `send` and `--observe` before a `--` separator refuses with no
/// effect. `None`: not an observation.
fn observe_gate(args: &[String]) -> Option<Result<i32>> {
    let head = args.split(|arg| arg == "--").next().unwrap_or_default();
    if !(head.iter().any(|arg| arg == "--observe") && head.iter().any(|arg| arg == "send")) {
        return None;
    }
    Some(match args {
        [send, name, observe, json]
            if send == "send"
                && observe == "--observe"
                && json == "--json"
                && zerus_client_core::desktop::session_resume::observed(name)
                && !name.contains('@') =>
        {
            platform::prepare_environment();
            local_sessions().and_then(|()| state::observe_send(name).map_err(|e| Error::new(1, e)))
        }
        _ => Err(Error::new(2, OBSERVE_ONLY)),
    })
}
fn state_command(verb: &str, args: &[String]) -> Result<i32> {
    let mut command = vec![verb.to_owned()];
    command.extend_from_slice(args);
    state::dispatch(&command).map_err(|e| Error::new(1, e))
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
/// Lines for Unix commands/input bindings or the POSIX creation ABIs.
fn unix_only_usage(line: &str) -> bool {
    if line.contains("worktrees program-holder ") || line.contains("worktrees program-stop") || line.contains("worktrees program-cleanup") {
        return true;
    }
    if [
        zerus_client_core::desktop::creation_clone::HELP_MARKER,
        zerus_client_core::desktop::creation_worktree::HELP_MARKER,
        zerus_client_core::desktop::creation_worktree::CONTROL_HELP_MARKER,
        zerus_client_core::desktop::creation_launch::HELP_MARKER,
        zerus_client_core::desktop::task_message::HELP_MARKER,
        zerus_client_core::desktop::task_message::REFERENCE_HELP_MARKER,
        zerus_client_core::desktop::task_message::review::HELP_MARKER,
        zerus_client_core::desktop::deferred_send::HELP_MARKER,
        zerus_client_core::desktop::deferred_answer::HELP_MARKER,
        zerus_client_core::desktop::deferred_answer::UPDATE_PHASE_MARKER,
        zerus_client_core::desktop::compact_to_send::HELP_MARKER,
        zerus_client_core::desktop::session_resume::HELP_MARKER,
        zerus_client_core::desktop::session_resume::OBSERVE_MARKER,
        zerus_client_core::desktop::task_start_reconcile::HELP_MARKER,
        zerus_client_core::worktree_programs::custody::HELP_MARKER,
        zerus_client_core::worktree_programs::presentation::HELP_MARKER,
        zerus_client_core::worktree_programs::stop::HELP_MARKER,
        zerus_client_core::worktree_programs::cleanup::HELP_MARKER,
    ]
    .contains(&line.trim())
    {
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
/// What this computer's `hgs --help` prints. The creation worktree executor
/// binds Git to a `/proc/self/fd` link, and no qualified seam yet attests the
/// code a macOS task message, deferred send or deferred answer child runs, so
/// only Linux advertises those ABIs.
fn help_text() -> Cow<'static, str> {
    let usage = advertised(usage_text(cfg!(unix)), cfg!(target_os = "linux"));
    match crate::tasks::controls::help_marker() {
        Some(marker) => format!("{usage}\n{marker}\n").into(),
        None => usage,
    }
}
fn advertised(usage: Cow<'static, str>, linux: bool) -> Cow<'static, str> {
    if linux {
        return usage;
    }
    let markers = [
        zerus_client_core::desktop::creation_worktree::HELP_MARKER,
        zerus_client_core::desktop::creation_worktree::CONTROL_HELP_MARKER,
        zerus_client_core::desktop::task_message::HELP_MARKER,
        zerus_client_core::desktop::task_message::REFERENCE_HELP_MARKER,
        zerus_client_core::desktop::task_message::review::HELP_MARKER,
        zerus_client_core::desktop::deferred_send::HELP_MARKER,
        zerus_client_core::desktop::deferred_answer::HELP_MARKER,
        zerus_client_core::desktop::deferred_answer::UPDATE_PHASE_MARKER,
        zerus_client_core::desktop::compact_to_send::HELP_MARKER,
        zerus_client_core::desktop::session_resume::HELP_MARKER,
        zerus_client_core::desktop::session_resume::OBSERVE_MARKER,
    ];
    usage
        .lines()
        .filter(|line| !markers.contains(&line.trim()))
        .map(|line| format!("{line}\n"))
        .collect::<String>()
        .into()
}

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

#[cfg(any(windows, test))]
mod swarm_restart;

pub fn dispatch(args: Vec<String>) -> Result<i32> {
    dispatch_at(args, Instant::now())
}

pub(crate) fn dispatch_at(args: Vec<String>, entered: Instant) -> Result<i32> {
    if let Some(result) = observe_gate(&args) {
        return result;
    }
    // Keep an original helper alive until its parent has observed live code.
    // Only a bounded pipe prefix is consumed; ordinary dispatch/auth is intact.
    #[cfg(unix)]
    let args = zerus_client_core::run::rendezvous::dispatch_args(args)
        .map_err(|_| Error::new(2, "original helper rendezvous refused"))?;
    #[cfg(any(windows, test))]
    if let Some(request) = swarm_restart::select(&args) {
        return swarm_restart::dispatch(request, entered);
    }
    #[cfg(not(any(windows, test)))]
    let _ = entered;
    let json = args.iter().any(|arg| arg == "--json");
    let result = dispatch_normal(args);
    // A read that refused because several sessions share the label it named,
    // with nothing in the request selecting one (zerus-ucck P3c), exits 5 with
    // the ambiguity report, as resolution does.
    match (result, state::session_id::refused()) {
        (Err(_), Some((text, value))) => {
            if json {
                println!("{value}");
            }
            Err(Error::new(state::session_id::AMBIGUOUS, text))
        }
        (result, _) => result,
    }
}

fn dispatch_normal(mut args: Vec<String>) -> Result<i32> {
    // This self-image owner inherits the original caller's already-selected
    // executable search/locale context. Repeating prepare_environment would
    // prepend PATH again before its exact context check. The shared state
    // entry still rejects extra argv and validates inherited objects/frame
    // before Root; no ordinary command or Windows control lane changes.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if args.first().map(String::as_str)==Some("__state")
        && args.get(1).map(String::as_str)==Some("native-picker-custodian-v1") {
        return state::dispatch(&args[1..]).map_err(|e| Error::new(1, e));
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
    // An encrypted gateway binds every local native call to the physical
    // machine it advertised. This check also works on Windows, without a
    // shell, stdin rewriting or a second native process.
    let mut guarded = false;
    if args.first().is_some_and(|a| a == "--expect-machine") {
        guarded = true;
        let expected = args.get(1).ok_or_else(|| Error::new(2, "Missing expected machine identity"))?;
        if uuid::Uuid::parse_str(expected).ok().is_none_or(|id| id.to_string() != *expected) {
            return Err(Error::new(1,"Mobile target identity changed; refresh computers"));
        }
        let guarded_at = if args.get(2).is_some_and(|a| a=="--client") {4} else {2};
        let actual = if args.get(guarded_at).is_some_and(|a| a=="session-action-status"||a=="delivery") {
            state::lifecycle_receipt_machine(&config).map_err(|_| Error::new(1,"Native machine identity is unavailable"))?
        } else {
            crate::swarm::local_node_id(&config).map_err(|_| Error::new(1,"Native machine identity is unavailable"))?
        };
        if actual != *expected {
            // A forwarded session reference names the machine it expects; the
            // SSH alias that carried it reached another one (zerus-ucck P1c).
            let hop = args.get(guarded_at).is_some_and(|command| {
                is_reference_call(command, args.get(guarded_at + 1..).unwrap_or_default())
            });
            return Err(Error::new(1, if hop {
                "Session reference belongs to another computer; the selected SSH alias does not match"
            } else {
                "Mobile target identity changed; refresh computers"
            }));
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if args.get(guarded_at).is_some_and(|a|a=="effort"||a=="settings")
            || (args.get(guarded_at).is_some_and(|a|a=="__state")&&args.get(guarded_at+1).is_some_and(|a|a=="effort"||a=="settings")) {
            state::remember_picker_caller_machine(&config,expected);
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
        eprint!("{}", help_text());
        return Ok(1);
    };
    let args = &args[index + 1..];
    // zerus-ucck P1c: a session reference selects the session it names on every
    // session command, through the same checks as `--session-id`. `inspect`
    // keeps its own read-only reference path below.
    let referenced;
    let args = if is_reference_call(command, args) {
        if session_id_flag(command, args)?.is_some() {
            return Err(Error::new(2, "Select a session by its reference or by --session-id, not both"));
        }
        let reference = crate::session_reference::Reference::parse(&args[0])?;
        if !reference.agent.is_empty() {
            return Err(Error::new(1, "A child agent's reference is read-only; use hgs inspect"));
        }
        let local_id = crate::swarm::local_node_id(&config).map_err(|e| Error::new(1, e.to_string()))?;
        if reference.machine != local_id {
            // A guarded call, including the one hop below, acts only on the
            // machine it verified; it never forwards a reference again.
            if guarded {
                return Err(Error::new(1, "Session reference belongs to another computer; a guarded call acts only on this computer"));
            }
            if matches!(command.as_str(), "native-ui" | "__dsh-ui-url") {
                return Err(Error::new(1, "Open another computer's native UI by name or --session-id"));
            }
            // Exactly one verified hop: the peer runs the command as a call
            // guarded by the reference's machine, so a peer that is not that
            // machine refuses, and the machine that is never forwards it.
            let host = if !target.is_empty() && !config.is_self(&target) {
                &target
            } else if config.peers.contains(&reference.host) && !config.is_self(&reference.host) {
                &reference.host
            } else {
                return Err(Error::new(1, "The referenced computer is not configured here; choose its configured @host"));
            };
            return remote_via(&config, host, command, args, dry, Some(&reference.machine));
        }
        if !target.is_empty() && !config.is_self(&target) {
            return Err(Error::new(1, "A local session reference cannot route to another computer"));
        }
        local_sessions()?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if command=="effort"||command=="settings" {state::remember_picker_caller_machine(&config,&reference.machine);}
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let effort_name=if command=="effort"||command=="settings" {
            Some(state::resolve_effort_reference(&reference).map_err(|e|Error::new(1,e))?)
        } else {None};
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let effort_name:Option<String>=None;
        let name=if let Some(name)=effort_name {name} else {
            let name = state::resolve_reference(&reference).map_err(|e| Error::new(1, e))?;
            state::session_id::select_reference(&name, &reference.session, &reference.conversation, &reference.run);
            name
        };
        let mut rest = vec![name];
        rest.extend_from_slice(&args[1..]);
        if !reference.archive.is_empty() {
            if !ARCHIVE_COMMANDS.contains(&command.as_str()) {
                return Err(Error::new(1, format!("The reference names an archived conversation; hgs {command} needs a saved session")));
            }
            if has(&rest, "--archive") {
                return Err(Error::new(2, "The reference already names its archive"));
            }
            rest.extend(["--archive".to_owned(), reference.archive.clone()]);
        }
        referenced = rest;
        &referenced[..]
    } else {
        args
    };
    // zerus-ucck P1: `--session-id ID` selects the session the command names
    // first. A peer receives it unchanged, only after advertising it.
    let selected;
    let args = if session_id_flag(command, args)?.is_some()
        && (target.is_empty() || config.is_self(&target))
    {
        local_sessions()?;
        selected = select_session(command, args)?;
        &selected[..]
    } else {
        args
    };
    // zerus-ucck P3a: a label named without a selection is resolved among
    // its candidates here, on the computer that holds the sessions.
    // Commands exempt from local sessions (hooks, session-action-status)
    // find their own session through `ids/`, and an archive ID already
    // selects exactly (#1437 review).
    if state::session_id::selected().is_none()
        && resolves_label_first(
            command,
            args,
            target.is_empty() || config.is_self(&target),
            platform::local_sessions(),
        )
    {
        resolve_label(command, args)?;
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
            print!("{}", help_text());
            println!("Encrypted workspace ABI: guarded-local-v1");
            println!("Desktop creation ABI: guarded-create-v1");
            println!("Account transfer ABI: revision-copy-v1");
            #[cfg(windows)]
            print!("{}\nMobile gateway ABI: windows-local-v1\n", crate::host::admin::HELP);
            return Ok(0);
        }
        "--version" | "version" => return crate::status::version(&config, &target, args),
        // Read-only on every machine; never routed as an agent launch.
        "status" => return crate::status::run(&config, &target, args, dry),
        // Text compiled into this hgs; a peer answers with its own.
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
        _ => {}
    }
    if args.first().is_some_and(|value| value.starts_with("zerus://")) {
        let local_only = args.len() == 2 && args[1] == "--local";
        if command != "inspect" || (args.len() != 1 && !local_only) || dry {
            return Err(Error::new(1, "Use hgs inspect '<session reference>' for read-only session references"));
        }
        let reference = crate::session_reference::Reference::parse(&args[0])?;
        let local_id = crate::swarm::local_node_id(&config).map_err(|e| Error::new(1,e.to_string()))?;
        // Like every other session command, a guarded inspect reads only the
        // computer it verified (#1376 review).
        if guarded && local_id != reference.machine {
            return Err(Error::new(1, "Session reference belongs to another computer; a guarded call acts only on this computer"));
        }
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
        // A local selector was resolved above. A peer resolves its own through
        // a gated `__dsh-ui-url`; a name alone never stands in for the ID.
        let mut rest = args.to_vec();
        let selector = match session_id_flag(command, args)? {
            Some((at, id)) => {
                rest.drain(at..at + 2);
                if !args[..at].iter().any(|arg| !arg.starts_with('-')) {
                    rest.insert(0, String::new());
                }
                Some(id)
            }
            None => None,
        };
        let name = match selector {
            Some(_) => rest[0].as_str(),
            None => require(&rest, 0, "usage: hgs native-ui <session> [--json]")?,
        };
        if rest.get(1).is_some_and(|v| v == "--json") && !dry {
            println!(
                "{}",
                serde_json::json!({"url":crate::native_ui::address(&config,&target,name,selector.as_deref())?})
            );
            return Ok(0);
        }
        return crate::native_ui::open(&config, &target, name, selector.as_deref(), dry);
    }
    if command == "account"
        && args.first().is_some_and(|v| v == "login")
        && args.get(1).is_some_and(|v| v == "native-dsh")
    {
        return crate::native_ui::open(&config, &target, "@account", None, dry);
    }
    if command == "a" && args.first().is_some_and(|v| v.starts_with("dsh/")) {
        // A DeepSeek attach is its native UI: a remote selector takes the same
        // gated `__dsh-ui-url` as native-ui (a local one was resolved above).
        let selector = session_id_flag(command, args)?.map(|(_, id)| id);
        return crate::native_ui::open(&config, &target, &args[0], selector.as_deref(), dry);
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
        "path-info" => crate::path_info::command(args, dry),
        "speech" => crate::speech::dispatch(&config, args, dry),
        "mobile" => crate::phones::dispatch(&config, args, dry),
        "__dsh-ui-url" => {
            let url = state::dsh::native_ui_url(require(args, 0, "Choose a native session")?)
                .map_err(|e| Error::new(1, e))?;
            println!("{}", json!({"url":url}));
            Ok(0)
        }
        "subagent" => {
            if args.len() != 3 || args[0] != "retire" || args[2] != "--json" || dry {
                return Err(Error::new(1,"usage: hgs subagent retire NAME --json (JSON stdin)"));
            }
            use std::io::Read;
            let mut bytes = Vec::new();
            std::io::stdin().lock().take(8193).read_to_end(&mut bytes)?;
            let result = state::subagent_lifecycle::retire(&args[1],&bytes).map_err(|e|Error::new(1,e))?;
            println!("{result}"); Ok(0)
        }
        "ls" => list(&config, args, dry),
        "watch" => crate::watch::run(&config, args, dry),
        "archives" => {
            let mobile = cfg!(windows) && args.last().map(String::as_str)==Some("--windows-mobile");
            let args = if mobile { &args[..args.len()-1] } else { args };
            let bounded = !mobile && args == ["--json","--bounded"];
            if (args != ["--json"] && args != ["lookup","--json"] && !bounded) || dry { return Err(Error::new(1,"usage: hgs archives [lookup] --json [--bounded] (JSON query on stdin)")); }
            let query = state::archive_catalog::read_query().map_err(|e| Error::new(1,e))?;
            let result = if args.first().map(String::as_str) == Some("lookup") {
                if mobile { state::archive_catalog::lookup_scoped(&query,true) } else { state::archive_catalog::lookup(&query) }
            } else if mobile { state::archive_catalog::page_scoped(&query,true) }
            else { state::archive_catalog::page_with(&query,bounded) }.map_err(|e| Error::new(1,e))?;
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
        "inspect" | "processes" | "worktrees" | "git-status" | "repository" | "dirs" | "send" | "send-now" | "session-action" | "session-action-status" | "terminal" | "history" | "history-projection" | "interrupt" | "clear-context" | "compact-context" | "answer" | "effort" | "settings" | "search" | "read" | "notifications" | "keep-warm"
        | "attachment" | "file-read" | "recovery" | "voice" | "schedules" | "graph" | "wait" | "parent" | "delivery" => {
            if command == "delivery" && dry { return Err(Error::new(1,"Delivery receipt reads do not support --dry-run")); }
            if command == "session-action-status" && dry { return Err(Error::new(1,"Lifecycle receipt reads do not support --dry-run")); }
            if command == "repository" && dry { return Err(Error::new(1,"Repository operations do not support --dry-run")); }
            if command == "wait" && dry { return Err(Error::new(1,"Wait operations do not support --dry-run")); }
            if command == "graph" && dry { return Err(Error::new(1,"Graph reads do not support --dry-run")); }
            if command == "dirs" && has(args, "--create") && dry {
                return Err(Error::new(1, "folder creation does not support --dry-run"));
            }
            if command == "worktrees" && args.first().is_some_and(|s| s == "create" || s == "cancel" || s == "remove" || s == "sweep" || s == "policy" && args.get(1).is_some_and(|s|s=="set")) && dry {
                return Err(Error::new(1, "worktree creation, removal and policy changes do not support --dry-run"));
            }
            if (matches!(command.as_str(), "processes" | "send" | "send-now" | "session-action" | "terminal" | "interrupt" | "clear-context" | "compact-context" | "answer" | "effort" | "settings" | "recovery" | "read" | "notifications" | "keep-warm" | "voice" | "schedules" | "parent") || (command=="attachment" && has(args,"--stage"))) && dry {
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
    let count = state::launch_identity::resume_count(
        require(args, 0, "--run needs a resume-argument count")?
    ).map_err(|error| Error::new(1,error))?;
    let n = count.value;
    if n >= args.len() - 1 {
        return Err(Error::new(
            1,
            "--run needs a base command after the resume arguments",
        ));
    }
    if env::var("HGS_SESSION").is_ok_and(|s| !s.is_empty())
        && env::var("HGS_TRACKING").as_deref() != Ok("0")
    {
        // The existing lifecycle receives its unchanged numeric grammar.
        // Required identities were verified before any bootstrap/native work.
        if count.required {
            let mut plain = args.to_vec();
            plain[0] = n.to_string();
            return state_command("run", &plain);
        }
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
            "--launch-id" | "--parent-link" | "--parent" => i += 1,
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
fn peer_json_archives(config: &Config, peer: &str, archives: bool) -> Value {
    let words = if archives { vec!["ls", "--local", "--json"] } else { vec!["ls", "--local", "--json", "--no-archives"] };
    let output = crate::remote_shell::hgs(config, peer, &words).map_err(|e| std::io::Error::other(e.message)).and_then(|remote| platform::ssh_output(
        crate::ssh_bytes::command(&remote)?
            .args(crate::machines::ssh_options(config, peer))
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=3",
                peer,
                &remote,
            ])
            .stderr(Stdio::piped()),
    ));
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
/// The help line a peer lists once it resolves `--session-id` (zerus-ucck P1).
pub(crate) const SELECTOR: &str = "session-id-selector-v1";

/// A help marker a peer lists: a whole line, or text within a line.
#[derive(Debug)]
pub(crate) enum Marker {
    Line(&'static str),
    Text(&'static str),
}

/// Every marker a peer must list before it receives `--session-id`, for the
/// command or for the one a guarded `--expect-machine` call wraps: what that
/// call needs already, then the selector. None without a selector.
fn selector_markers(command: &str, args: &[String]) -> Result<Option<Vec<Marker>>> {
    let (inner, rest, mut markers) = if command == "--expect-machine" {
        let inner = args.get(1).map(String::as_str).unwrap_or_default();
        let mut markers = vec![Marker::Text("guarded-create-v1")];
        if inner == "session-action-status" {
            markers.push(Marker::Line("lifecycle-receipt-status-v1"));
        }
        (inner, args.get(2..).unwrap_or_default(), markers)
    } else {
        (command, args, Vec::new())
    };
    let selector = session_id_flag(inner, rest)?.is_some();
    let reference = is_reference_call(inner, rest);
    if !selector && !reference {
        return Ok(None);
    }
    match inner {
        "notifications" => markers.push(Marker::Line("session-notifications-v1")),
        "keep-warm" => markers.push(Marker::Line("session-keep-warm-v1")),
        "hooks" => markers.push(Marker::Text("hooks-health-v1")),
        _ => {}
    }
    if selector {
        markers.push(Marker::Line(SELECTOR));
    }
    if reference {
        markers.push(Marker::Line(REFERENCE_SELECTOR));
    }
    Ok(Some(markers))
}

/// The POSIX script for a bounded archive page on a peer (zerus-080n8): an
/// older CLI rejects `--bounded`, so one that does not list the marker gets
/// the exact call, which it always served. `guarded` (an `--expect-machine`
/// call) first needs `guarded-create-v1`, as every guarded call does.
pub(crate) fn bounded_archive_script(args: &[String], guarded: bool) -> String {
    let exact: Vec<String> = args.iter().filter(|arg| *arg != "--bounded").cloned().collect();
    let page = format!(
        "if ~/.local/bin/hgs --help 2>/dev/null | grep -qEx '[[:space:]]*{}[[:space:]]*'; then exec ~/.local/bin/hgs {}; else exec ~/.local/bin/hgs {}; fi",
        state::archive_catalog::BOUNDED_MARKER, platform::join(args), platform::join(&exact)
    );
    platform::remote_sh(&if guarded {
        format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'guarded-create-v1'; then {page}; else echo 'hgs: update the selected computer to continue verified creation' >&2; exit 1; fi")
    } else {
        page
    })
}

/// A POSIX script that runs `hgs ARGS` on the peer only when the installed
/// CLI's help lists every marker; otherwise it asks for an update.
pub(crate) fn gated(markers: &[Marker], args: &[String]) -> String {
    let probes: Vec<String> = markers
        .iter()
        .map(|marker| match marker {
            Marker::Line(line) => format!(
                "~/.local/bin/hgs --help 2>/dev/null | grep -qEx '[[:space:]]*{line}[[:space:]]*'"
            ),
            Marker::Text(text) => format!("~/.local/bin/hgs --help 2>/dev/null | grep -qF '{text}'"),
        })
        .collect();
    platform::remote_sh(&format!(
        "if {}; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to select sessions by ID' >&2; exit 1; fi",
        probes.join(" && "),
        platform::join(args)
    ))
}

/// Commands whose first argument is a session, which `--session-id` may
/// select instead (zerus-ucck P1).
/// Session commands that also take an archived conversation (`--archive ID`),
/// which an archived session reference selects (zerus-ucck P1c).
const ARCHIVE_COMMANDS: &[&str] = &["processes", "reference", "fork", "kill", "rename", "resume"];

/// The help line a peer lists once it resolves session references on every
/// session command (zerus-ucck P1c).
pub(crate) const REFERENCE_SELECTOR: &str = "session-reference-selector-v1";

/// A session command, other than `inspect`, whose session is a reference.
fn is_reference_call(command: &str, args: &[String]) -> bool {
    command != "inspect"
        && SESSION_COMMANDS.contains(&command)
        && args.first().is_some_and(|value| value.starts_with("zerus://"))
}

/// `notify` and `paste` are the tray's and tmux's own entry points, and
/// `subagent retire` names its session second; they keep names only.
const SESSION_COMMANDS: &[&str] = &[
    "inspect", "reference", "processes", "send", "send-now", "history", "history-projection",
    "terminal", "session-action", "session-action-status", "compact-context", "clear-context",
    "interrupt", "attachment", "file-read", "answer", "read", "notifications", "keep-warm", "fork", "effort",
    "settings", "schedules", "voice", "hooks", "a", "kill", "rename", "archive", "terminate",
    "pause", "resume", "native-ui", "__dsh-ui-url", "parent",
];

/// The position and value of `--session-id` for a session command, refused
/// when repeated, empty or not a session ID. Other commands keep their own
/// meaning (`hgs task --session-id`, an agent's own flag after `--`).
fn session_id_flag(command: &str, args: &[String]) -> Result<Option<(usize, String)>> {
    if !SESSION_COMMANDS.contains(&command) {
        return Ok(None);
    }
    let options = args.iter().position(|arg| arg == "--").unwrap_or(args.len());
    let mut flags = args[..options].iter().enumerate().filter(|(_, arg)| *arg == "--session-id");
    let Some((at, _)) = flags.next() else {
        return Ok(None);
    };
    if flags.next().is_some() {
        return Err(Error::new(2, "--session-id may be given once"));
    }
    let id = args.get(at + 1).map(String::as_str).unwrap_or("");
    if !state::session_id::canonical(id) {
        return Err(Error::new(2, "--session-id needs a session ID (a lowercase UUID from hgs ls --json)"));
    }
    Ok(Some((at, id.to_owned())))
}

/// Whether dispatch resolves the label a session command names before the
/// command runs (zerus-ucck P3a). Only on the computer that holds the
/// sessions, with its local sessions enabled, and only for a plain label:
/// never a reference (`zerus://`), which routes or resolves itself, nor a
/// command that finds its own session or names an archive. Otherwise the
/// command's own refusal or routing answers first, as before label
/// resolution existed: a Windows computer without local sessions still
/// refuses the native DeepSeek UI and routes a peer's reference
/// (zerus-t2hd0).
fn resolves_label_first(command: &str, args: &[String], here: bool, local: bool) -> bool {
    SESSION_COMMANDS.contains(&command)
        && needs_local_sessions(command)
        && here
        && local
        && !has(args, "--archive")
        && args
            .first()
            .is_some_and(|first| !first.starts_with('-') && !first.starts_with("zerus://"))
}

/// zerus-ucck P3c: how each session command carries the identities it
/// expects, which select among the sessions that share the label it names.
/// Its guard arrives with its request on standard input, and its own check
/// selects (`state::expect`); until then every read of the label refuses.
const REQUEST_GUARDED: &[&str] = &[
    "send", "send-now", "history", "history-projection", "terminal", "session-action",
    "compact-context", "clear-context", "interrupt", "attachment", "file-read", "answer", "read",
    "notifications", "keep-warm", "effort", "settings", "schedules", "voice", "parent",
];
/// Its guard is on its command line (`--expected-run-id`, `--run`, `--run-id`,
/// `--expected-conversation-id`, `--conversation`).
#[cfg_attr(not(test), allow(dead_code))]
const FLAG_GUARDED: &[&str] = &["fork", "terminate", "resume", "processes", "reference", "a"];
/// It carries none: a label several sessions share is refused (exit 5).
#[cfg_attr(not(test), allow(dead_code))]
const UNGUARDED: &[&str] = &["inspect", "kill", "rename", "archive", "pause", "native-ui", "__dsh-ui-url"];
/// It finds its own session and is not resolved here: `hooks` through `ids/`
/// and its request's identities, `session-action-status` through its receipts.
#[cfg_attr(not(test), allow(dead_code))]
const SELF_RESOLVED: &[&str] = &["hooks", "session-action-status"];

/// The guard of `command` invoked with `args`: identities on its command line
/// select now; a command whose request carries them waits for it.
fn label_guard(command: &str, args: &[String]) -> state::LabelGuard {
    let value = |flags: &[&str]| {
        args.windows(2)
            .find(|pair| flags.contains(&pair[0].as_str()))
            .map(|pair| pair[1].clone())
            .unwrap_or_default()
    };
    let run = value(&["--expected-run-id", "--run", "--run-id"]);
    let conversation = value(&["--expected-conversation-id", "--conversation"]);
    if !run.is_empty() || !conversation.is_empty() {
        state::LabelGuard::Given { run, conversation }
    } else if REQUEST_GUARDED.contains(&command) {
        state::LabelGuard::Request
    } else {
        state::LabelGuard::None
    }
}

/// The label a session command names first, resolved (`state::resolve_label`):
/// the ambiguity report goes to standard output under `--json`, and its text is
/// the error, with exit status 5.
fn resolve_label(command: &str, args: &[String]) -> Result<()> {
    // Only the first argument names the session: a later word may be an
    // option's value, never a label.
    let Some(name) = args.first().filter(|arg| !arg.starts_with('-')) else {
        return Ok(());
    };
    local_sessions()?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if command=="effort"||command=="settings" {
        return state::resolve_effort_label(name,label_guard(command,args)).map_err(|refusal| {
            if let Some(value)=refusal.value.filter(|_|args.iter().any(|arg|arg=="--json")) {println!("{value}");}
            Error::new(refusal.code,refusal.text)
        });
    }
    state::resolve_label(name, label_guard(command, args)).map_err(|refusal| {
        if let Some(value) = refusal.value.filter(|_| args.iter().any(|arg| arg == "--json")) {
            println!("{value}");
        }
        Error::new(refusal.code, refusal.text)
    })
}

/// The arguments with `--session-id ID` replaced by the selected session's
/// current name. A name given before the flag must be that name; arguments
/// after it are the command's own (a message, a new name, a path).
fn select_session(command: &str, args: &[String]) -> Result<Vec<String>> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if command=="effort"||command=="settings" {return select_picker_session(command,args);}
    let Some((at, id)) = session_id_flag(command, args)? else {
        return Ok(args.to_vec());
    };
    let names = state::session_id::names_of(&id).map_err(|e| Error::new(1, e))?;
    let name = match names.as_slice() {
        [] => return Err(Error::new(1, format!("No live or saved session has ID {id}"))),
        [name] => name.clone(),
        _ => {
            return Err(Error::new(
                state::session_id::AMBIGUOUS,
                format!("Several sessions carry ID {id}: {}", names.join(", ")),
            ))
        }
    };
    // Resolution names the session; the command's own reads under its lock
    // must still find this ID on the record they act on.
    state::session_id::select(&name, &id);
    let mut rest = args.to_vec();
    rest.drain(at..at + 2);
    match args[..at].iter().find(|arg| !arg.starts_with('-')) {
        Some(given) if *given != name => Err(Error::new(
            1,
            format!("Session ID {id} is {name}, not {given}"),
        )),
        Some(_) => Ok(rest),
        None => {
            rest.insert(0, name);
            Ok(rest)
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn select_picker_session(command: &str, args: &[String]) -> Result<Vec<String>> {
    let Some((at, id)) = session_id_flag(command, args)? else {
        return Ok(args.to_vec());
    };
    let name =
        state::resolve_effort_id(&id).map_err(|refusal| Error::new(refusal.code, refusal.text))?;
    let mut rest = args.to_vec();
    rest.drain(at..at + 2);
    match args[..at].iter().find(|arg| !arg.starts_with('-')) {
        Some(given) if *given != name => Err(Error::new(
            1,
            format!("Session ID {id} is {name}, not {given}"),
        )),
        Some(_) => Ok(rest),
        None => {
            rest.insert(0, name);
            Ok(rest)
        }
    }
}

/// The terminal of the session `--session-id` selected under `session`, bound
/// before the first effect: its record, read under the state lock, still
/// carries the ID, and the name's immutable session ID runs that record's run.
/// None without a selection; an error when the name now runs anything else
/// (#1355 review). The caller acts on the immutable ID, never the name.
fn bound_terminal(session: &str) -> Result<Option<(String, String)>> {
    let Some(run) = state::session_id::selected_run(session).map_err(|e| Error::new(1, e))? else {
        return Ok(None);
    };
    let changed = || {
        let message = state::session_id::selected().map(|selection| selection.changed());
        Error::new(1, message.unwrap_or_else(|| format!("{session} changed; refresh and retry")))
    };
    let own = state::terminal_of(session).map_err(|e| Error::new(1, e))?;
    let info = terminal()
        .probe(Target::Name(&own), &[PaneField::SessionId, PaneField::RunMarker])
        .ok_or_else(changed)?;
    let fields = info.fields();
    if fields.len() != 2
        || !fields[0]
            .strip_prefix('$')
            .is_some_and(|id| !id.is_empty() && id.chars().all(|ch| ch.is_ascii_digit()))
    {
        return Err(changed());
    }
    if run.is_empty() || fields[1] != run {
        return Err(changed());
    }
    Ok(Some((fields[0].to_owned(), run)))
}

fn remote(
    config: &Config,
    target: &str,
    command: &str,
    args: &[String],
    global_dry: bool,
) -> Result<i32> {
    remote_via(config, target, command, args, global_dry, None)
}

/// The port Codex's browser sign-in waits on, on the signing computer.
const CALLBACK_PORT: u16 = 1455;
/// SSH options for a remote sign-in: this computer's loopback port 1455
/// forwarded to the peer's, on a connection of its own (never a shared
/// master, which would keep the port after sign-in), continuing when the
/// local port is busy.
const CALLBACK_FORWARD: [&str; 6] = ["-o", "ExitOnForwardFailure=no", "-o", "ControlPath=none", "-L", "1455:localhost:1455"];

/// Whether a peer call signs an account in (`account login`, guarded or not).
fn account_sign_in(command: &str, args: &[String]) -> bool {
    let rest = if command == "--expect-machine" {
        if args.get(1).map(String::as_str) != Some("account") {
            return false;
        }
        args.get(2..).unwrap_or_default()
    } else if command == "account" {
        args
    } else {
        return false;
    };
    rest.first().is_some_and(|operation| operation == "login")
}

/// `remote`, optionally as the one verified hop of a session reference: the
/// peer runs the command guarded by `--expect-machine MACHINE`, with the same
/// SSH options the command has without it (zerus-ucck P1c).
fn remote_via(
    config: &Config,
    target: &str,
    command: &str,
    args: &[String],
    global_dry: bool,
    machine: Option<&str>,
) -> Result<i32> {
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
                "ConnectTimeout=3".into(),
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
            let terminal=!has(&args,"-d")&&!matches!(actual,"dirs"|"path-info"|"worktrees"|"repository"|"session-action-status"|"delivery"|"--help"|"dsh"|"opencode");
            flags=vec!["-o".into(),"BatchMode=yes".into(),"-o".into(),"ConnectTimeout=3".into()];
            if terminal { flags.insert(0,"-t".into()); }
            let mut all=vec!["--expect-machine".into(),expected.into(),"--client".into(),config.host().into()];
            all.extend_from_slice(&args[1..]);all
        }
        "session-action-status" if machine.is_none() => return Err(Error::new(2,"A remote lifecycle receipt read requires --expect-machine ID before session-action-status")),
        "archives" | "session-action-status" | "kill" | "terminate" | "pause" | "inspect" | "reference" | "processes" | "worktrees" | "git-status" | "repository" | "dirs" | "project" | "archive" | "rename"
        | "send" | "send-now" | "session-action" | "terminal" | "history" | "history-projection" | "interrupt" | "clear-context" | "compact-context" | "answer" | "effort" | "settings" | "search" | "schedules" | "graph" | "wait" | "guide" | "dsh" | "opencode" | "attachment" | "file-read" | "recovery" | "swarm" | "read" | "notifications" | "keep-warm" | "speech" | "analytics" | "voice" | "hooks" | "parent" | "delivery" | "path-info" | "--help" => {
            flags = vec![
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "ConnectTimeout=3".into(),
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
    if account_sign_in(command, &args) {
        // Codex finishes a browser sign-in on the signing computer's
        // localhost:1455, which a browser on this computer cannot reach.
        // Forward this computer's port there, as OpenAI documents for
        // headless sign-in (T-tknwyr). A busy local port only warns, and
        // a connection of its own releases the forward when sign-in ends.
        flags.extend(CALLBACK_FORWARD.map(String::from));
        if !global_dry && !has(&args, "--dry-run") {
            eprintln!("hgs: Signing in on {target}. Open the sign-in link in a browser on this computer; this computer's localhost:{CALLBACK_PORT} is forwarded to {target} so the browser can finish.");
        }
    }
    flags.extend(crate::machines::ssh_options(config, target));
    let remote_args: Vec<String> = match machine {
        Some(machine) => ["--expect-machine".to_owned(), machine.to_owned()].into_iter().chain(remote_args).collect(),
        None => remote_args,
    };
    let wrapped_path_info = command == "--expect-machine" && args.get(1).is_some_and(|actual| actual == "path-info");
    let remote = if let Some(markers) = selector_markers(command, &args)? {
        // An older peer would read the flag as a session name; it must
        // advertise the selector, and what the command needs already, first.
        gated(&markers, &remote_args)
    } else if command == "path-info" || wrapped_path_info {
        // An older peer would read an unknown command as an agent launch, so
        // the actual command's marker gates it, wrapped by --expect-machine
        // too (zerus-3skhm). A path's ~ reaches the peer's hgs as written:
        // hgs expands ~/ itself and refuses ~user, which the peer's shell
        // would expand (zerus-ie12o).
        let words = remote_args.iter().map(|word| if word.starts_with('~') { format!("'{}'", word.replace('\'', "'\\''")) } else { platform::join(&[word]) }).collect::<Vec<_>>().join(" ");
        let guarded = if wrapped_path_info || machine.is_some() { "~/.local/bin/hgs --help 2>/dev/null | grep -qF 'guarded-create-v1' && " } else { "" };
        platform::remote_sh(&format!("if {guarded}~/.local/bin/hgs --help 2>/dev/null | grep -qF 'hgs [@host] path-info --path'; then exec ~/.local/bin/hgs {words}; else echo 'hgs: update the selected computer to check file paths' >&2; exit 1; fi"))
    } else if command == "--expect-machine" && args.get(1).is_some_and(|actual| actual=="session-action-status") {
        // Probe the same executable, without reading the JSON stream. An old
        // peer must never interpret this reader's name as a program to launch.
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'guarded-create-v1' && ~/.local/bin/hgs --help 2>/dev/null | grep -qEx '[[:space:]]*lifecycle-receipt-status-v1[[:space:]]*'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to read lifecycle receipts' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "--expect-machine" && args.get(1).is_some_and(|inner| inner == "archives") && has(&args, "--bounded") {
        // zerus-wzzr9: a machine-guarded archive page falls back the same way.
        bounded_archive_script(&remote_args, true)
    } else if command == "--expect-machine" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'guarded-create-v1'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to continue verified creation' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "subagent" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'Subagent retirement ABI: subagent-retire-v1'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to retire stale subagent records' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "repository" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'repository-clone-v1'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to clone repositories' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "wait" {
        // Old peers otherwise interpret an unknown command as an agent launch.
        // Probe the same installed executable without consuming JSON stdin.
        // These probes are POSIX scripts; a fish login shell runs them under sh.
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'hgs [@host] wait'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to track waits' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "delivery" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qEx '[[:space:]]*delivery-receipt-v1[[:space:]]*'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to read original send receipts' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "parent" {
        // An older peer would read an unknown command as an agent launch.
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qEx '[[:space:]]*session-parent-v1[[:space:]]*'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to change parent sessions' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "notifications" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qEx '[[:space:]]*session-notifications-v1[[:space:]]*'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to change notification preferences' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "archives" && has(&args, "--bounded") {
        bounded_archive_script(&remote_args, false)
    } else if command == "keep-warm" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qEx '[[:space:]]*session-keep-warm-v1[[:space:]]*'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to keep the prompt cache warm' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "hooks" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'hooks-health-v1'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to inspect hook health' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "analytics" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'hgs [@host] analytics --json'; then exec ~/.local/bin/hgs {} --read-only; else echo 'hgs: update the selected computer to read analytics' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if command == "watch" {
        // An older peer would read an unknown command as an agent launch.
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF '{}'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to stream events (watch-v1)' >&2; exit 1; fi",zerus_watch_protocol::HELP_MARKER,platform::join(&remote_args)))
    } else if command == "guide" {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qF 'hgs [@host] guide'; then exec ~/.local/bin/hgs {}; else echo 'hgs: the selected computer has no agent guide; check hgs @host -h' >&2; exit 1; fi",platform::join(&remote_args)))
    } else if matches!(command, "claude" | "codex" | "kimi" | "dsh" | "opencode") && args.iter().take_while(|s| s.as_str() != "--").any(|s| s == "--parent") {
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qEx '[[:space:]]*claimed-parent-v1[[:space:]]*'; then exec ~/.local/bin/hgs {}; else echo 'hgs: update the selected computer to claim a launch parent' >&2; exit 1; fi", platform::join(&remote_args)))
    } else if let Some(link) = (matches!(command, "claude" | "codex" | "kimi" | "dsh" | "opencode" | "fork") && machine.is_none())
        .then(state::parent_link::forwarded_argument)
        .flatten()
    {
        // The peer records this agent as the new session's parent only if it
        // advertises launch-parent-v1; an older peer launches without it.
        let mut with = remote_args.clone();
        let at = with.iter().position(|arg| arg == "--").unwrap_or(with.len());
        with.splice(at..at, ["--parent-link".to_owned(), link]);
        platform::remote_sh(&format!("if ~/.local/bin/hgs --help 2>/dev/null | grep -qEx '[[:space:]]*launch-parent-v1[[:space:]]*'; then exec ~/.local/bin/hgs {}; else exec ~/.local/bin/hgs {}; fi", platform::join(&with), platform::join(&remote_args)))
    } else { format!("~/.local/bin/hgs {}", platform::remote_join(&remote_args)) };
    if !global_dry && !has(&args, "--dry-run") && command == "ls" && has(&args, "--json") {
        println!("{}", peer_json_archives(config, target, !has(&args,"--no-archives")));
        return Ok(0);
    }
    let remote = if global_dry || has(&args, "--dry-run") { remote } else {
        let mut markers: Vec<(&str, bool)> = match command {
            "--expect-machine" if args.get(1).is_some_and(|v| v == "session-action-status") => vec![("guarded-create-v1", false), ("lifecycle-receipt-status-v1", true)],
            "--expect-machine" if args.get(1).is_some_and(|v| v == "delivery") => vec![("guarded-create-v1", false), ("delivery-receipt-v1", true)],
            "--expect-machine" if wrapped_path_info => vec![("guarded-create-v1", false), ("hgs [@host] path-info --path", false)],
            "--expect-machine" => vec![("guarded-create-v1", false)],
            "subagent" => vec![("Subagent retirement ABI: subagent-retire-v1", false)],
            "repository" => vec![("repository-clone-v1", false)],
            "delivery" => vec![("delivery-receipt-v1", true)],
            "wait" => vec![("hgs [@host] wait", false)],
            "notifications" => vec![("session-notifications-v1", true)],
            "keep-warm" => vec![("session-keep-warm-v1", true)],
            "hooks" => vec![("hooks-health-v1", false)],
            "analytics" => vec![("hgs [@host] analytics --json", false)],
            "path-info" => vec![("hgs [@host] path-info --path", false)],
            "watch" => vec![(zerus_watch_protocol::HELP_MARKER, false)],
            "guide" => vec![("hgs [@host] guide", false)],
            "claude" | "codex" | "kimi" | "dsh" | "opencode" if args.iter().take_while(|s| s.as_str() != "--").any(|s| s == "--parent") => vec![("claimed-parent-v1", true)],
            _ => vec![],
        };
        if let Some(selected) = selector_markers(command, &args)? {
            markers = selected.iter().map(|marker| match marker {
                Marker::Line(value) => (*value, true), Marker::Text(value) => (*value, false),
            }).collect();
        }
        if machine.is_some() { markers.push(("guarded-create-v1", false)); }
        let mut words = remote_args.clone();
        // A non-POSIX peer runs the words without a shell probe: send --bounded only to one that lists it.
        let archives = command == "archives" || (command == "--expect-machine" && args.get(1).is_some_and(|inner| inner == "archives"));
        if archives && has(&args, "--bounded") && crate::remote_shell::windows(config, target).unwrap_or(false)
            && !crate::remote_shell::supports(config, target, &[(state::archive_catalog::BOUNDED_MARKER, true)]).unwrap_or(false) {
            words.retain(|arg| arg != "--bounded");
        }
        if command == "analytics" { words.push("--read-only".into()); }
        crate::remote_shell::adapt(config, target, &words, &remote, &markers,
            "Update the selected computer before using this command; the required capability is missing")
            .map_err(|e| if e.code == 3 { Error::new(3, format!("{target} unreachable: {}", e.message)) } else { e })?
    };
    if global_dry || has(&args, "--dry-run") {
        if let Some(title) = tab {
            if config.tab {
                println!("tab {title} color={}", platform::tab_color(config, target));
            }
        }
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
    if remote.starts_with(crate::ssh_bytes::PREFIX) {
        let mut framed = crate::ssh_bytes::command(&remote)?;
        for (key, value) in ssh.get_envs() {
            match value { Some(value) => { framed.env(key, value); }, None => { framed.env_remove(key); } }
        }
        ssh = framed;
    }
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

pub(crate) fn local_json(config: &Config) -> Result<Value> { local_json_archives(config, true) }
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
        #[cfg(windows)]
        {
            raw["local_host"] = crate::host::admin::local_host(&config.state);
        }
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
        let Ok(remote) = crate::remote_shell::hgs(config, peer, &["ls", "--local"]) else {
            println!("  offline");
            continue;
        };
        let mut flags = crate::machines::ssh_options(config, peer);
        flags.extend(
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=3",
                peer,
                &remote,
            ]
            .into_iter()
            .map(str::to_owned),
        );
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
        return crate::native_ui::open(config, "", session, None, dry || has(args, "--dry-run"));
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
    // OpenCode has no terminal record: its adapter checks its own binding,
    // which carries the selected ID, and any supplied run.
    if state::opencode::exists(session) {
        return state::opencode::attach(session, run_id, dry).map_err(|error| Error::new(1, error));
    }
    if !existing {
        return attach(config, session, client, dry);
    }
    if dry {
        println!(
            "{}",
            terminal().describe(&DryRun::AttachExisting {
                session: &state::terminal_of(session).map_err(|e| Error::new(1, e))?,
                run_id,
            })
        );
        return Ok(0);
    }
    nested_guard(false, None)?;
    // A selected session binds to its verified run before any client or attach
    // effect, and a supplied --run-id must be that same run (#1355 review).
    if let Some((id, run)) = bound_terminal(session)? {
        if run_id.is_some_and(|expected| expected != run) {
            return Err(Error::new(1, crate::state::backend::RUN_CHANGED));
        }
        return attach_target(config, session, client, Target::Session(&id), Some(&run));
    }
    // Resolve exact name to an immutable tmux session ID before checking the run.
    // If it disappears later, attach fails instead of creating or selecting a new one.
    const ENDED: &str = "session is no longer running; resume it explicitly";
    // The session's own terminal (P3b), never another under its label.
    let own = state::terminal_of(session).map_err(|e| Error::new(1, e))?;
    let target = terminal()
        .probe(
            Target::Name(&own),
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
            terminal().describe(&DryRun::Attach {
                session: &state::terminal_of(session).map_err(|e| Error::new(1, e))?,
                client,
            })
        );
        return Ok(0);
    }
    nested_guard(false, None)?;
    terminal().attachable()?;
    // The session's own terminal (P3b); a resume may name a new one, so it
    // is read again after it.
    let own = |session: &str| state::terminal_of(session).map_err(|e| Error::new(1, e));
    if !session_exists(&own(session)?) && config.state.is_dir() {
        env::set_var("HGS_CLIENT", client);
        let rc = state_command("resume", &[session.into()])?;
        if rc != 0 {
            return Ok(rc);
        }
    }
    match bound_terminal(session)? {
        Some((id, run)) => attach_target(config, session, client, Target::Session(&id), Some(&run)),
        None => attach_target(config, session, client, Target::Name(&own(session)?), None),
    }
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
    let own = state::terminal_of(session).map_err(|e| Error::new(1, e))?;
    let target = terminal()
        .probe(Target::Name(&own), &[PaneField::SessionId])
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
        let own = state::terminal_of(session).map_err(|e| Error::new(1, e))?;
        println!("{}", terminal().describe(&DryRun::Kill { session: &own }));
        return Ok(0);
    }
    // The session's own terminal (P3b), never another under its label.
    let own = state::terminal_of(session).map_err(|e| Error::new(1, e))?;
    if session_exists(&own) {
        let rc = match bound_terminal(session)? {
            Some((id, _)) => {
                terminal()
                    .kill_session(Target::Session(&id))
                    .map_err(|error| Error::new(1, error))?;
                0
            }
            None => terminal().kill(&own)?,
        };
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
            "--parent-link" => {
                let link = require(args, index + 1, "--parent-link needs a link")?;
                state::parent_link::accept_forwarded(link).map_err(|e| Error::new(1, e))?;
                index += 1;
            }
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
        crate::native_ui::open(config, "", &target, None, false)
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
                "--effort" => { options.effort=Some(require(args,i+1,"--effort needs a model variant")?.into()); i+=1; }
                "--account" => {
                    options.account =
                        Some(require(args, i + 1, "--account needs a profile ID")?.into());
                    i += 1;
                }
                "--parent-link" => {
                    let link = require(args, i + 1, "--parent-link needs a link")?;
                    state::parent_link::accept_forwarded(link).map_err(|e| Error::new(1, e))?;
                    i += 1;
                }
                "--parent" => {
                    state::parent_link::accept_claimed(require(args, i + 1, "--parent needs a JSON identity")?).map_err(|e| Error::new(1, e))?;
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
            if options.resume || options.fresh {
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
/// Whether Claude has saved a conversation for `directory`. Claude names its
/// history folder after its own working directory as the system reports it
/// (process.cwd(): symlinks resolved, so /var/... is /private/var/... on
/// macOS), not after the configured path, so a project configured through a
/// symlink is looked up under its physical path as well (zerus-ngeih).
fn claude_history_exists(history_root: &Path, directory: &str) -> bool {
    let has = |directory: &str| {
        fs::read_dir(history_root.join(claude_history_folder(directory)))
            .ok()
            .is_some_and(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .any(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
            })
    };
    has(directory)
        || fs::canonicalize(directory)
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
            .is_some_and(|physical| physical != directory && has(&physical))
}

/// Claude Code 2.1.322's VL/Pe/sre rule: escape UTF-16 units, and above 200
/// units append the base-36 absolute signed wrapping hash of the original path.
fn claude_history_folder(directory: &str) -> String {
    let mut escaped = String::with_capacity(208);
    let mut length = 0;
    let mut hash = 0_i32;
    for unit in directory.encode_utf16() {
        if length < 200 {
            escaped.push(char::from_u32(u32::from(unit)).filter(char::is_ascii_alphanumeric).unwrap_or('-'));
        }
        length += 1;
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    if length > 200 {
        let mut value = hash.unsigned_abs();
        let mut digits = [b'0'; 7];
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b"0123456789abcdefghijklmnopqrstuvwxyz"[(value % 36) as usize];
            value /= 36;
            if value == 0 { break; }
        }
        escaped.push('-');
        escaped.extend(digits[at..].iter().copied().map(char::from));
    }
    escaped
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
    if state::parent_link::claimed_launch() && !matches!(command_name.as_str(), "claude" | "codex" | "kimi" | "dsh" | "opencode") {
        return Err(Error::new(1, "--parent is available for Claude, Codex, Kimi, DeepSeek and OpenCode launches"));
    }
    if command_name=="opencode" {
        if options.account.as_deref().is_some_and(|id|id!="native-opencode") {return Err(Error::new(1,"Choose native-opencode; separate OpenCode profiles are not supported"));}
        if options.resume || !options.extra.is_empty() || options.fresh || options.remote_control {return Err(Error::new(1,"Use saved HGS session names to resume OpenCode; select a model with --model provider/model"));}
        if options.effort.is_some() && options.model.is_none() {return Err(Error::new(1,"--effort needs --model"));}
        let project=if project=="/" {"root".into()}else{sanitize(&project)};
        let tag=if options.tag.is_empty(){"main"}else{&options.tag};let name=format!("opencode/{project}/{tag}");
        state::opencode::launch(&name,&directory,options.launch_id.as_deref(),options.model.as_deref(),options.effort.as_deref(),options.dry,options.new_only).map_err(|e|Error::new(1,e))?;
        if !options.dry {println!("OpenCode session ready: {name}\nOpen this session in Zerus.");}return Ok(0);
    }
    if options.model.is_some() || options.effort.is_some() {return Err(Error::new(1,"--model and --effort are OpenCode launch options; other agent flags go after --"));}
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
        state::dsh::launch(
            &name,
            &directory,
            options.launch_id.as_deref(),
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
    if state::parent_link::claimed_launch() && (existing || binding_exists(config, &session)?) {
        return Err(Error::new(1, "--parent applies only to new sessions; use hgs parent adopt for an existing session"));
    }
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
    let program = if command_name == "sh" && cfg!(windows) { base.len() } else { 1 };
    crate::project_launcher::apply(&directory, &command_name, options.bare, program, &mut base, &mut resume)
        .map_err(|error| Error::new(1, error))?;
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
        auto = claude_history_exists(Path::new(&history_root), &directory);
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
        let rc = state_command("resume", &state_args)?;
        return if rc == 0 && !options.detached && !options.dry {
            attach(config, &session, client, false)
        } else {
            Ok(rc)
        };
    }
    let tracked = matches!(command_name.as_str(), "claude" | "codex" | "kimi")
        && env::var("HGS_TRACKING").as_deref() != Ok("0");
    let allocation = tracked.then(state::launch_identity::Allocation::fresh);
    let helper = if tracked && !options.dry {
        Some(state::launch_identity::helper::Observed::probe(&executable))
    } else {
        None
    };
    let count = match (&helper, &allocation) {
        (Some(helper), Some(allocation)) => helper
            .count(allocation.session(), allocation.run(), if auto { resume.len() } else { 0 })
            .map_err(|error| Error::new(1, error))?,
        _ => if auto { resume.len().to_string() } else { "0".into() },
    };
    // The Unix login shell is the pane's root process. On Windows PowerShell
    // runs under `--run` as well: the host starts every root process as a new
    // process group, which ignores Ctrl+C together with everything it starts,
    // and only the supervisor turns Ctrl+C back on for its child (spec D11).
    let command = if command_name == "sh" && cfg!(unix) {
        base
    } else {
        #[cfg(unix)]
        let mut words = {
            let (program, flags) = crate::accounts::supervisor_shell(&shell);
            vec![
                program.into(),
                flags.into(),
                crate::accounts::supervisor_script(program, &supervisor_environment),
                executable.clone(),
            ]
        };
        // The supervisor starts directly, with the session's variables, which
        // carry the account (spec D8).
        #[cfg(windows)]
        let mut words = vec![executable.clone(), "--run".to_owned()];
        words.push(count);
        if auto {
            words.extend(resume);
        }
        words.extend(base);
        words
    };
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
    if tracked {
        spec.env.extend([
            format!("HGS_SESSION={session}"),
            // A new session's terminal is its label (zerus-ucck P3b).
            format!("HGS_TERMINAL_NAME={session}"),
            format!("HGS_AGENT={command_name}"),
            format!(
                "HGS_ACCOUNT_ID={}",
                options.account.as_deref().unwrap_or("")
            ),
            format!("HGS_STATE_DIR={}", config.state.display()),
            format!("HGS_EXECUTABLE={executable}"),
            // An explicit live parent claim, otherwise the process-proven
            // launcher; empty without either (zerus-sa9qn.2, zerus-cgl5w).
            format!("{}={}", state::parent_link::ENV, state::parent_link::launcher_value().map_err(|e| Error::new(1, e))?),
            "HGS_RESUME_SESSION_ID=".into(),
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
        let allocation = allocation.as_ref().expect("tracked launch allocation");
        let handoff = helper.as_ref().map_or_else(
            || allocation.environment(),
            |helper| helper.environment(allocation),
        );
        spec.env.extend(handoff.into_iter().map(|(key, value)| format!("{key}={value}")));
        for key in ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "KIMI_CODE_HOME"] {
            spec.env
                .push(format!("{key}={}", env::var(key).unwrap_or_default()));
        }
    }
    if options.detached {
        let rc = terminal().launch(&spec)?.code;
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
    let exit = terminal().launch(&spec)?;
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
    #[test]
    fn claude_history_uses_the_released_folder_cap_and_hash() {
        // Golden folder names from VL/Pe/sre in Claude Code 2.1.322. Each
        // case gets a separate history root so a cap-only implementation
        // cannot accidentally find the 200-character boundary case's file.
        let prefix = format!("-fixture-{}", "a".repeat(191));
        let cases = [
            (format!("/fixture/{}", "a".repeat(191)), prefix.clone()),
            (format!("/fixture/{}", "a".repeat(192)), format!("{prefix}-ble0s5")),
            (format!("/fixture/{}", "a".repeat(240)), format!("{prefix}-vis0vp")),
            (format!("/fixture/{}b", "a".repeat(240)), format!("{prefix}-h9g0ab")),
            (format!("/fixture/{}c", "a".repeat(240)), format!("{prefix}-h9g0aa")),
            (format!("/fixture/{}", "😀".repeat(100)), format!("-fixture-{}-ix5129", "-".repeat(191))),
        ];
        for (directory, folder) in cases {
            let history = tempfile::tempdir().unwrap();
            assert!(!super::claude_history_exists(history.path(), &directory));
            let path = history.path().join(folder);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("conversation.jsonl"), "{}\n").unwrap();
            assert!(super::claude_history_exists(history.path(), &directory), "history not found for {directory}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn claude_history_is_found_under_the_physical_path_of_a_symlinked_project() {
        // zerus-ngeih: Claude names its folder after its physical cwd.
        let root = tempfile::tempdir().unwrap();
        let physical = root.path().join("real project");
        std::fs::create_dir(&physical).unwrap();
        let linked = root.path().join("linked");
        std::os::unix::fs::symlink(&physical, &linked).unwrap();
        let physical = std::fs::canonicalize(&physical).unwrap().to_string_lossy().into_owned();
        let linked = linked.to_string_lossy().into_owned();
        let history = root.path().join("projects");
        let escape = |path: &str| path.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>();
        assert!(!super::claude_history_exists(&history, &linked));
        let folder = history.join(escape(&physical));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("notes.txt"), "").unwrap();
        assert!(!super::claude_history_exists(&history, &linked), "only a saved conversation counts");
        std::fs::write(folder.join("old.jsonl"), "{}\n").unwrap();
        assert!(super::claude_history_exists(&history, &linked));
        assert!(super::claude_history_exists(&history, &physical));
        // History recorded under the configured path itself still counts.
        let other = tempfile::tempdir().unwrap();
        let raw = other.path().to_string_lossy().into_owned();
        let folder = history.join(escape(&raw));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("old.jsonl"), "{}\n").unwrap();
        assert!(super::claude_history_exists(&history, &raw));
        assert!(!super::claude_history_exists(&history, &root.path().join("missing").to_string_lossy()));
    }
    #[cfg(unix)]
    #[test]
    fn a_bounded_archive_page_falls_back_on_a_peer_without_the_marker() {
        // zerus-080n8, zerus-wzzr9: plain and machine-guarded calls alike.
        let machine = "11111111-1111-4111-8111-111111111111";
        let root = tempfile::tempdir().unwrap();
        for guarded in [false, true] {
            let mut args: Vec<String> = if guarded { vec!["--expect-machine".into(), machine.into()] } else { Vec::new() };
            args.extend(["archives", "--json", "--bounded"].map(String::from));
            let script = bounded_archive_script(&args, guarded);
            for marker in [true, false] {
                let home = root.path().join(format!("{guarded}-{marker}"));
                let bin = home.join(".local/bin");
                std::fs::create_dir_all(&bin).unwrap();
                let help = if marker { "echo '       archive-bounded-pages-v1';" } else { "" };
                std::fs::write(bin.join("hgs"), format!("#!/bin/sh\nif [ \"$1\" = --help ]; then echo usage; echo '       guarded-create-v1'; {help} exit 0; fi\nprintf '%s\\n' \"$@\" > \"$HOME/called\"\n")).unwrap();
                std::fs::set_permissions(bin.join("hgs"), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
                let ran = std::process::Command::new("sh").arg("-c").arg(&script).env_clear().env("HOME", &home).env("PATH", "/usr/bin:/bin").output().unwrap();
                assert!(ran.status.success(), "{guarded} {marker}: {}", String::from_utf8_lossy(&ran.stderr));
                let called = std::fs::read_to_string(home.join("called")).unwrap();
                let expected: Vec<&str> = args.iter().map(String::as_str).filter(|arg| marker || *arg != "--bounded").collect();
                assert_eq!(called.split_whitespace().collect::<Vec<_>>(), expected, "{guarded} {marker}");
            }
        }
    }

    /// zerus-t2hd0: with local sessions off, as on a Windows computer without
    /// its session host, dispatch never resolves a label first, so each
    /// command answers for itself: `native-ui dsh/p` and `a dsh/p` with the
    /// native DeepSeek UI's refusal, `inspect` of a peer's reference by
    /// routing to it, and the other session commands with their own
    /// local-sessions refusal. A reference is never resolved as a label.
    #[test]
    fn label_resolution_never_answers_before_a_commands_own_refusal_or_routing() {
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let uri = "zerus://session?machine=11111111-2222-4333-8444-555555555555&host=peer&name=claude%2Fp&conversation=c1";
        for (command, rest) in [
            ("native-ui", args(&["dsh/p"])),
            ("a", args(&["dsh/p"])),
            ("inspect", args(&[uri])),
            ("inspect", args(&["claude/p"])),
            ("send", args(&["claude/p", "--json"])),
        ] {
            assert!(!super::resolves_label_first(command, &rest, true, false), "{command} {rest:?}");
        }
        // With local sessions on, a plain label resolves; a reference never.
        assert!(super::resolves_label_first("native-ui", &args(&["dsh/p"]), true, true));
        assert!(super::resolves_label_first("inspect", &args(&["claude/p"]), true, true));
        assert!(!super::resolves_label_first("inspect", &args(&[uri]), true, true));
        assert!(!super::resolves_label_first("inspect", &args(&[uri, "--local"]), true, true));
        // Another computer resolves its own labels.
        assert!(!super::resolves_label_first("inspect", &args(&["claude/p"]), false, true));
    }

    /// zerus-ucck P3c: every session command declares how it carries the
    /// identities that select among sessions sharing a label, exactly once;
    /// a new one fails here until it is classified.
    #[test]
    fn every_session_command_declares_its_guard() {
        for command in super::SESSION_COMMANDS {
            let lists = [super::REQUEST_GUARDED, super::FLAG_GUARDED, super::UNGUARDED, super::SELF_RESOLVED];
            let count = lists.iter().filter(|list| list.contains(command)).count();
            assert_eq!(count, 1, "{command} is classified {count} times");
        }
        for list in [super::REQUEST_GUARDED, super::FLAG_GUARDED, super::UNGUARDED, super::SELF_RESOLVED] {
            for command in list {
                assert!(super::SESSION_COMMANDS.contains(command), "{command} is not a session command");
            }
        }
    }

    /// zerus-ucck P3c: only a command classified as carrying its identities in
    /// its request defers a shared label; any other, a new unclassified reader
    /// included, refuses it at dispatch unless its command line carries them.
    #[test]
    fn an_unclassified_reader_refuses_a_shared_label() {
        use crate::state::LabelGuard;
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        assert!(matches!(super::label_guard("new-reader", &args(&["claude/p"])), LabelGuard::None));
        assert!(matches!(
            super::label_guard("new-reader", &args(&["claude/p", "--json"])),
            LabelGuard::None
        ));
        for command in super::REQUEST_GUARDED {
            assert!(matches!(super::label_guard(command, &args(&["claude/p"])), LabelGuard::Request), "{command}");
        }
        for command in super::FLAG_GUARDED.iter().chain(super::UNGUARDED).chain(super::SELF_RESOLVED) {
            assert!(matches!(super::label_guard(command, &args(&["claude/p"])), LabelGuard::None), "{command}");
        }
        assert!(matches!(
            super::label_guard("terminate", &args(&["claude/p", "--expected-run-id", "run-twin"])),
            LabelGuard::Given { ref run, ref conversation } if run == "run-twin" && conversation.is_empty()
        ));
    }

    use super::*;

    /// #1355 review: every command whose usage takes a session first accepts
    /// `--session-id`; only the tray's and tmux's own entry points do not.
    #[test]
    fn every_command_that_takes_a_session_first_accepts_a_session_id() {
        let pattern = regex::Regex::new(r"hgs (?:\[@host\] |@host )?(?:--expect-machine ID )?([a-z_-]+) <session>").unwrap();
        let mut usage: Vec<&str> = pattern.captures_iter(USAGE).map(|c| c.get(1).unwrap().as_str()).collect();
        usage.sort_unstable();
        usage.dedup();
        assert!(usage.len() >= 25, "{usage:?}");
        let internal = ["notify", "paste"];
        for command in usage.iter().filter(|command| !internal.contains(command)) {
            assert!(SESSION_COMMANDS.contains(command), "{command}");
        }
        // Forms the usage writes without `<session>` first.
        for command in ["kill", "rename", "archive", "terminate", "pause", "resume", "send-now", "native-ui", "__dsh-ui-url"] {
            assert!(SESSION_COMMANDS.contains(&command), "{command}");
        }
        for command in internal.iter().chain(&["task", "subagent", "ls", "search"]) {
            assert!(!SESSION_COMMANDS.contains(command), "{command}");
        }
    }

    /// #1355 review: a peer receives `--session-id` only after listing the
    /// selector and what the command, or the command a guarded call wraps,
    /// needs already.
    #[test]
    fn a_selector_needs_every_marker_its_call_needs() {
        let id = "3f2a9c1d-0b4e-4d6a-9c1d-2e3f4a5b6c7d";
        let args = |words: &[&str]| words.iter().map(|word| word.to_string()).collect::<Vec<_>>();
        let names = |markers: Vec<Marker>| -> Vec<&'static str> {
            markers.into_iter().map(|marker| match marker { Marker::Line(m) | Marker::Text(m) => m }).collect()
        };
        assert!(selector_markers("inspect", &args(&["codex/p/a"])).unwrap().is_none());
        assert_eq!(names(selector_markers("inspect", &args(&["--session-id", id])).unwrap().unwrap()), [SELECTOR]);
        assert_eq!(names(selector_markers("hooks", &args(&["--session-id", id, "--json"])).unwrap().unwrap()),
            ["hooks-health-v1", SELECTOR]);
        assert_eq!(names(selector_markers("notifications", &args(&["--session-id", id, "--json"])).unwrap().unwrap()),
            ["session-notifications-v1", SELECTOR]);
        let machine = "11111111-1111-4111-8111-111111111111";
        assert_eq!(names(selector_markers("--expect-machine", &args(&[machine, "inspect", "--session-id", id])).unwrap().unwrap()),
            ["guarded-create-v1", SELECTOR]);
        assert_eq!(names(selector_markers("--expect-machine", &args(&[machine, "session-action-status", "--session-id", id, "--json"])).unwrap().unwrap()),
            ["guarded-create-v1", "lifecycle-receipt-status-v1", SELECTOR]);
        assert!(selector_markers("--expect-machine", &args(&[machine, "inspect", "codex/p/a"])).unwrap().is_none());
        assert_eq!(selector_markers("--expect-machine", &args(&[machine, "inspect", "--session-id", "NOT"])).unwrap_err().code, 2);
        // zerus-ucck P1c: a reference needs the reference selector; inspect
        // keeps its existing read-only reference path.
        let reference = "zerus://session?machine=11111111-1111-4111-8111-111111111111&host=peer&name=codex%2Fp%2Fa&conversation=c";
        assert_eq!(names(selector_markers("send", &args(&[reference, "--json"])).unwrap().unwrap()), [REFERENCE_SELECTOR]);
        assert_eq!(names(selector_markers("hooks", &args(&[reference, "--json"])).unwrap().unwrap()),
            ["hooks-health-v1", REFERENCE_SELECTOR]);
        assert_eq!(names(selector_markers("--expect-machine", &args(&[machine, "send", reference, "--json"])).unwrap().unwrap()),
            ["guarded-create-v1", REFERENCE_SELECTOR]);
        assert!(selector_markers("inspect", &args(&[reference])).unwrap().is_none());
        // Everything the CLI forwards to a peer with a selector carries it.
        for command in SESSION_COMMANDS {
            assert!(selector_markers(command, &args(&["--session-id", id])).unwrap().is_some(), "{command}");
        }
    }

    #[test]
    fn repository_preparation_does_not_admit_session_commands_on_control_consoles() {
        for command in ["ls", "machine", "presence", "swarm", "project", "account",
            "speech", "analytics", "graph", "wait", "mobile", "hooks", "repository", "session-action-status", "subagent"] {
            assert!(!needs_local_sessions(command), "{command}");
        }
        for command in ["a", "kill", "send", "send-now", "inspect", "history",
            "history-projection", "processes", "terminal", "session-action", "interrupt",
            "clear-context", "compact-context", "answer", "effort", "settings", "recovery",
            "read", "notifications", "voice", "schedules", "attachment", "file-read",
            "claude", "codex", "kimi", "sh", "resume", "pause", "fork", "reference",
            "archive", "terminate", "dirs", "worktrees", "git-status", "__state", "__host",
            "repository-git-worker", "Repository", "repository-extra", ""] {
            assert!(needs_local_sessions(command), "{command}");
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
        let worktree_marker = zerus_client_core::desktop::creation_worktree::HELP_MARKER;
        assert!(USAGE.lines().any(|line| line.trim() == worktree_marker));
        let launch_marker = zerus_client_core::desktop::creation_launch::HELP_MARKER;
        assert!(USAGE.lines().any(|line| line.trim() == launch_marker));
        let message_marker = zerus_client_core::desktop::task_message::HELP_MARKER;
        assert!(USAGE.lines().any(|line| line.trim() == message_marker));
        let task_marker = zerus_client_core::desktop::task_start_reconcile::HELP_MARKER;
        assert!(USAGE.lines().any(|line| line.trim() == task_marker));
        let send_marker = zerus_client_core::desktop::deferred_send::HELP_MARKER;
        assert!(USAGE.lines().any(|line| line.trim() == send_marker));
        let answer_marker = zerus_client_core::desktop::deferred_answer::HELP_MARKER;
        assert!(USAGE.lines().any(|line| line.trim() == answer_marker));
        let phase_marker = zerus_client_core::desktop::deferred_answer::UPDATE_PHASE_MARKER;
        assert!(USAGE.lines().any(|line| line.trim() == phase_marker));
        let compact_marker = zerus_client_core::desktop::compact_to_send::HELP_MARKER;
        assert!(USAGE.lines().any(|line| line.trim() == compact_marker));
        let resume_marker = zerus_client_core::desktop::session_resume::HELP_MARKER;
        assert!(USAGE.lines().any(|line| line.trim() == resume_marker));
        let observe_marker = zerus_client_core::desktop::session_resume::OBSERVE_MARKER;
        assert!(USAGE.lines().any(|line| line.trim() == observe_marker));
        // Windows has no `hgs paste`, `hgs clip` or `hgs notify`, no Ctrl+V
        // that pulls a peer's image, and no POSIX creation-clone executor.
        // Its dry runs print the host's requests; every other line stays as is.
        let neutral = usage_text(false);
        assert!(!neutral.contains("tmux"), "{neutral}");
        assert!(!neutral.contains("Ctrl+V"), "{neutral}");
        assert!(!neutral.contains("worktrees program-"), "{neutral}");
        for marker in [zerus_client_core::worktree_programs::custody::HELP_MARKER, zerus_client_core::worktree_programs::presentation::HELP_MARKER, zerus_client_core::worktree_programs::stop::HELP_MARKER, zerus_client_core::worktree_programs::cleanup::HELP_MARKER] {
            assert!(USAGE.lines().any(|line| line.trim() == marker));
            assert!(!neutral.lines().any(|line| line.trim() == marker));
        }
        assert!(!neutral.lines().any(|line| line.trim() == clone_marker));
        assert!(!neutral.lines().any(|line| line.trim() == worktree_marker));
        assert!(!neutral.lines().any(|line| line.trim() == task_marker));
        assert!(!neutral.lines().any(|line| line.trim() == send_marker));
        assert!(!neutral.lines().any(|line| line.trim() == answer_marker));
        assert!(!neutral.lines().any(|line| line.trim() == phase_marker));
        assert!(!neutral.lines().any(|line| line.trim() == compact_marker));
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
                    "       Desktop creation worktree ABI: desktop-creation-worktree-v1\n       Desktop creation worktree receipt control: future-fence-v1\n",
                    ""
                )
                .replace(
                    "       Desktop creation launch ABI: desktop-creation-launch-v1\n",
                    ""
                )
                .replace(
                    "       Desktop task message ABI: desktop-task-message-v1\n",
                    ""
                )
                .replace(
                    "       Desktop task message references: desktop-task-message-reference-v1\n",
                    ""
                )
                .replace(
                    "       Desktop task message input review: desktop-task-message-input-review-v1\n",
                    ""
                )
                .replace(
                    "       Desktop deferred send ABI: desktop-deferred-send-v1\n",
                    ""
                )
                .replace(
                    "       Desktop deferred answer ABI: desktop-deferred-answer-v1\n",
                    ""
                )
                .replace(
                    "       Desktop deferred answer update phase: desktop-deferred-answer-update-phase-v1\n",
                    ""
                )
                .replace(
                    "       Desktop deferred compact to send ABI: desktop-deferred-compact-to-send-v1\n",
                    ""
                )
                .replace(
                    "       Desktop deferred session resume ABI: desktop-deferred-session-resume-v1\n",
                    ""
                )
                .replace(
                    "       Desktop deferred session resume Codex readiness: desktop-deferred-session-resume-codex-v1\n",
                    ""
                )
                .replace(
                    "       Desktop task start reconcile ABI: desktop-task-start-reconcile-v1\n",
                    ""
                )
                .replace("       hgs [@host] worktrees program-holder --json   bounded read-only native checkout custody (stdin)\n", "")
                .replace("       Worktree program custody ABI: worktree-program-custody-v1\n", "")
                .replace("       Worktree program presentation ABI: worktree-program-presentation-v1\n", "")
                .replace("       hgs worktrees program-stop --json   original native-confirmed program stop (private stdin)\n", "")
                .replace("       hgs worktrees program-stop-status --json   original result only, never replay (stdin)\n", "")
                .replace("       Worktree program stop ABI: worktree-program-stop-v1\n", "")
                .replace("       hgs worktrees program-cleanup --json   original live Remove continuation (private stdin)\n", "")
                .replace("       hgs worktrees program-cleanup-status --json   original removal result only (stdin)\n", "")
                .replace("       Worktree program cleanup ABI: worktree-program-cleanup-v1\n", "")
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
    fn only_linux_advertises_the_creation_worktree_and_task_message_executors() {
        let marker = zerus_client_core::desktop::creation_worktree::HELP_MARKER;
        let message = zerus_client_core::desktop::task_message::HELP_MARKER;
        let references = zerus_client_core::desktop::task_message::REFERENCE_HELP_MARKER;
        let review = zerus_client_core::desktop::task_message::review::HELP_MARKER;
        let send = zerus_client_core::desktop::deferred_send::HELP_MARKER;
        let answer = zerus_client_core::desktop::deferred_answer::HELP_MARKER;
        let phase = zerus_client_core::desktop::deferred_answer::UPDATE_PHASE_MARKER;
        let compact = zerus_client_core::desktop::compact_to_send::HELP_MARKER;
        let resume = zerus_client_core::desktop::session_resume::HELP_MARKER;
        let observe = zerus_client_core::desktop::session_resume::OBSERVE_MARKER;
        let clone = zerus_client_core::desktop::creation_clone::HELP_MARKER;
        assert_eq!(advertised(usage_text(true), true), USAGE);
        let mac = advertised(usage_text(true), false);
        assert!(!mac.lines().any(|line| line.trim() == marker));
        assert!(!mac.lines().any(|line| line.trim() == message));
        assert!(!mac.lines().any(|line| line.trim() == references));
        assert!(!mac.lines().any(|line| line.trim() == review));
        assert!(!mac.lines().any(|line| line.trim() == send));
        assert!(!mac.lines().any(|line| line.trim() == answer));
        assert!(!mac.lines().any(|line| line.trim() == phase));
        assert!(!mac.lines().any(|line| line.trim() == compact));
        assert!(!mac.lines().any(|line| line.trim() == resume));
        assert!(!mac.lines().any(|line| line.trim() == observe));
        assert!(mac.lines().any(|line| line.trim() == clone), "clone stays POSIX");
        assert_eq!(
            mac,
            USAGE
                .replace(
                    "       Desktop creation worktree ABI: desktop-creation-worktree-v1\n       Desktop creation worktree receipt control: future-fence-v1\n",
                    ""
                )
                .replace(
                    "       Desktop task message ABI: desktop-task-message-v1\n",
                    ""
                )
                .replace(
                    "       Desktop task message references: desktop-task-message-reference-v1\n",
                    ""
                )
                .replace(
                    "       Desktop task message input review: desktop-task-message-input-review-v1\n",
                    ""
                )
                .replace(
                    "       Desktop deferred send ABI: desktop-deferred-send-v1\n",
                    ""
                )
                .replace(
                    "       Desktop deferred answer ABI: desktop-deferred-answer-v1\n",
                    ""
                )
                .replace(
                    "       Desktop deferred answer update phase: desktop-deferred-answer-update-phase-v1\n",
                    ""
                )
                .replace(
                    "       Desktop deferred compact to send ABI: desktop-deferred-compact-to-send-v1\n",
                    ""
                )
                .replace(
                    "       Desktop deferred session resume ABI: desktop-deferred-session-resume-v1\n",
                    ""
                )
                .replace(
                    "       Desktop deferred session resume Codex readiness: desktop-deferred-session-resume-codex-v1\n",
                    ""
                )
        );
        for marker in [
            marker, message, references, review, send, answer, phase, compact, resume, observe,
        ] {
            assert_eq!(
                help_text().lines().any(|line| line.trim() == marker),
                cfg!(target_os = "linux")
            );
        }
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
