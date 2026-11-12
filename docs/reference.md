# Zerus reference

[Product overview](../README.md) | [Installation](installation.md)

This reference covers session lifecycle, desktop workflows and native integration
limits. Run `hgs -h` for the complete command syntax.

## CLI sessions

`hgs` starts a command in a named project tmux session and attaches locally or
through SSH. The session survives a closed terminal, an SSH interruption or a
sleeping client while its host remains running. Terminal windows, tabs and splits
belong to your terminal emulator; tmux provides the persistent session.

```sh
hgs claude sample-project           # attach if present; otherwise start Claude
hgs @mac claude sample-project      # the same operation over SSH
hgs ls                             # local and peer sessions
hgs status                         # version, sessions and services here and on peers
hgs version                        # this hgs version (also hgs --version)
hgs a claude/sample-project/review  # attach by full name
hgs kill claude/sample-project      # remove the hgs binding; keep native history
hgs pause --all                     # pause verified idle sessions
hgs resume --all                    # restore saved sessions in the background
hgs project ls                     # legacy local folder aliases
hgs claude sample-project -c        # continue the latest conversation in the folder
hgs --dry-run claude sample-project # show the tmux/SSH command
```

Pass an absolute folder path when no alias is configured. `--new` generates a
fresh session name; `--new -n review` uses your label and rejects an occupied name.
Arguments after `--` go to the agent or project wrapper. Arbitrary terminal commands
can run through `hgs`; native lifecycle and Activity functions require an adapter.

Inside a session's own terminal (tmux on Linux and macOS, the session host on
Windows), `hgs` never attaches or switches that terminal, so an agent starts other
sessions detached. `hgs claude /path/to/project --new -n review -d` starts the new
session beside the current one and prints `hgs: started NAME`, as `fork -d` and
`resume -d` do; `hgs guide launch` gives agents the full form with `--launch-id`.
A launch without `-d` and `hgs a` are refused there with exit 2 (`already inside
tmux session NAME; use --new -d or open a new tab`; Windows names no tmux). The new
session gets its own session and run identity, never the caller's. Its account
comes from the launch (`--account ID` or the computer's default for that agent);
the account profile that the calling session runs under is not passed on.

### Session references

**Copy session reference** in the desktop session window's **Session actions** menu copies a
`zerus://session?...` URI. Android offers the same action in the
session toolbar and menu. It contains the stable computer UUID, a native machine
name for routing, the session name and exact conversation identity; child and
archive references include their explicit identities. It contains no credentials.

Paste the reference into another conversation to identify its source. An agent
can run `hgs inspect '<reference>'` to read its Activity and recorded messages.
The CLI verifies the computer UUID, follows a renamed session by its conversation
ID, and rejects cleared, forgotten or ambiguous bindings. A conversation reference
survives resume into a new run; an unconfirmed first-message reference requires
the original run. A configured peer matching the machine name routes through SSH;
use `hgs @configured-alias inspect '<reference>'` when its alias differs.

A reference identifies a session; it authorizes nothing. Reading it with
`hgs inspect` never acts on the session. Any other session command does with it
exactly what it does with the session's name: `hgs send` sends input, and `hgs a`
may resume a saved session.
A reference never launches a new session and never addresses a session other
than the one it names.

Every other command whose first argument is a session also accepts a reference
there, for example `hgs pause '<reference>'` or `hgs send '<reference>' --json`.
It resolves the session the reference names, following a rename, and then acts
under the same checks as `--session-id`. Every record the command reads under
that name must still carry the reference's conversation (or its run, when it
names no conversation), and `kill` and `a` bind the terminal to the verified
run. An archived conversation's reference reaches the commands that take
`--archive` (`processes`, `reference`, `fork`, `kill`, `rename` and `resume`).
A child agent's reference stays read-only, for `hgs inspect`. A reference and
`--session-id` cannot be combined. A reference to another computer routes to its
configured host, or to `@host`, only after that peer's `hgs --help` lists
`session-reference-selector-v1`. It takes exactly one verified hop: the peer runs
the command guarded by the reference's computer UUID (`--expect-machine`), so a
peer that is another computer refuses it (`the selected SSH alias does not
match`), and no peer forwards it again. A guarded call itself never routes a
reference to another computer. A reference may also carry a `session` field,
the session ID, which is then checked as well. `hgs reference`, `hgs self` and
message envelopes do not add that field yet, because an older hgs refuses an
unknown reference field.

`hgs [@host] reference <session> --json [--archive ID] [--agent ID]` returns the
same URI and readable identity fields. Optional `--run ID --conversation ID`
guards ensure a copy action still addresses the selected conversation.

An agent obtains its own reference with `hgs self [--json]`. Terminal agents are
identified through their native process ancestry, never `HGS_SESSION` alone.
DeepSeek and OpenCode tool shells already receive their native conversation ID
as [`DSH_SESSION_ID`](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/shell/shell-env/README.md)
or [`OPENCODE_SESSION_ID`](https://github.com/anomalyco/opencode/blob/v2.2.7/packages/core/src/tool/plugin/shell.ts).
Without a proven terminal caller, `self` resolves that claim against the native
home and a live local binding. Its JSON reply distinguishes `identity_source`
`process` from `claimed`, includes the durable `session_id`, and supplies
`parent_identity` with the exact session, run and conversation for parent commands.
The native lookup uses a derived index with authoritative rereads; its cold
metadata scan is bounded to 5000 entries, 64 MiB and two seconds, and incomplete
or ambiguous coverage refuses. A missing, stale, archived or unavailable native
session refuses too. Current names follow renames. Native hosts need access to
the same Zerus state/config paths; new OpenCode services retain only those two
HGS path variables, never HGS session/run identity. `self` never routes to a peer
and refuses `--dry-run`. Claimed identity never enters send/graph attribution,
task ownership or permission proof.

`hgs [@host] guide [send|wait|launch|stop]` prints compact instructions for
agents that read, message, wait for, launch or stop other sessions. The text is
compiled into hgs and stays small because agents read it into their context;
topics are read only when needed. `@host` returns that peer's guide after
probing its installed help, so an older peer fails without launching anything.

Native conversation IDs appear only in **Details** on desktop and **Session
details → Overview** on Android, labeled **Session ID** with a dedicated copy
icon immediately after the exact identifier. The desktop icon uses the same
borderless style as header actions. The ID stays on one line whenever the pane
has room. The icon copies the ID as plain text, as does the **Copy session ID**
icon in the desktop session header, which copies without showing the ID;
**Copy session reference** is in the desktop session window's **Session actions** menu and copies
the portable URI. Successful text copies use the window's status confirmation
without a system notification on Linux or macOS. Before that ID is reported,
**Run ID** identifies the current
process run; it can change on resume. Child details also show their agent ID.
Search accepts complete IDs or
parts of conversation, run, archive and reported child IDs; a child match
identifies its parent conversation.
Machine, project and state filters still apply. Desktop ID results show the
matched identifier and expose its complete value in the tooltip.

The desktop session header keeps frequent actions as icons: open the session
folder, open a shell there, **Copy session ID** and **Clear session**. Its
**Session actions** menu shares the same checked handlers. A stopped session
hides the Clear session icon and offers
**Start fresh…** in the menu; an archived session shows **Restore session** in its
place. The session list context menu offers **Clear session…** for a live session
where a stopped one offers **Start fresh…**. For a session that is not open, it
becomes available once that session's own inspection confirms the conversation
listed for it; choosing it opens the session and asks the same confirmation.

### Session row and window menus

A session row's right-click menu keeps Mark needs attention, Mute, Compact/Expand
row, Open terminal, Copy connect command, Copy session ID, Clear, Rename,
Move to project and Terminate. Paused and archived rows keep the corresponding
available actions, such as Start fresh, Copy restore command and Forget.
Multi-selection keeps its batch menu.

Open terminal connects to the selected existing run in the external terminal
configured for this computer, including connections to remote computers. It uses
`hgs [@computer] a NAME --existing --run-id ID`, so a delayed terminal cannot
resume a stopped agent or connect to a replacement run under the same name.
An original live startup run can connect before its first native conversation
binding, including while it waits for startup trust. This does not resume it or
answer trust prompts. If its identity changes while the menu is open, reopen the
menu to use the current row.
Offline, paused, unidentified and unsupported adapters show a disabled reason;
DeepSeek's Native UI remains in Session actions. Copy session ID copies the
native conversation ID shown as Session ID in the header and Details, rather
than its name or run ID. Rows without this identity show a disabled reason.

The session window's top-right Session actions menu retains Expand all collapsed,
Open folder, Open shell in session folder, Pause, Fork, Set parent (and Detach),
Copy session name, Copy session reference and Copy folder path. Show running
commands is there when available. Their existing checks and confirmations apply.

Identifiers such as `zerus-ab12` are Beads task numbers, not session IDs. Desktop
message search finds their public mentions; Android session search finds mentions
in the available session metadata and previews. A task mention alone does not
establish a unique session identity.

### Static agent guidance

The static hint source currently covers tracked Claude Code and Codex main
conversations. Its native delivery contract still requires qualification on the
reviewed versions before adoption. Kimi, OpenCode and DeepSeek channels remain
separate work; older hgs peers inject nothing.

The hint points to `hgs guide` with fixed English text, independent of session
name, computer and hgs version. It uses the existing SessionStart hook context,
without changing user instruction flags, project launchers or saved native
arguments. It is not sent as a person's Activity message. New conversations and
previously unmarked conversations receive an attempt; repeated resumes suppress
another attempt. Clear and compaction replace context and request a new copy.
A fork inherits suppression only from the exact marked context it forks.
Renaming does not change the text or hint revision. Arbitrary commands and native
subagents receive no hint.

Configure optional injection in the hgs config file or caller environment:

```sh
HGS_AGENT_HINT=0                         # disable new hint injection globally
HGS_AGENT_HINT_OFF="native-codex work"   # exclude exact account IDs
```

Injection defaults to enabled. The global value accepts `0` or `1`; the exclusion
list is whitespace-separated and uses `native-claude` or `native-codex` for the
provider's own login. Environment values take precedence, including an explicitly
empty exclusion list. Invalid or unreadable hint policy disables only optional
injection with a bounded diagnostic; unrelated configuration validation is
unchanged. `hgs guide` and `hgs self` remain available when injection is disabled.
Opt-out does not remove context already retained by the provider.

Each fresh start, resume, recovery, archive restore and fork resolves the current
caller's effective hint policy again. Only those effective hint values enter the
new process environment, after login-shell startup on Unix. The hgs config path
is unchanged and policy is not stored in the saved launch recipe or binding.
This is a process snapshot: editing the file does not live-reload a running
native process. An `hgs` invocation inside that agent inherits the snapshot as
ordinary environment overrides, which can continue to shadow later file edits.
On Windows, an absent exclusion variable alongside the resolved global value
means an empty list; the hook does not supplement it from another default file.

The private conversation/revision mark suppresses retries; it does not prove
stdout delivery or native acceptance. An output failure reports unknown delivery
and is not automatically retried. There is no crash-atomic exactly-once promise.

<a id="recovery"></a>

## Pause, resume and reboot

```sh
hgs pause claude/sample-project/review
hgs pause --all
# Reboot the machine running the agents after checking the pause result.
hgs ls
hgs resume claude/sample-project/review
hgs resume --all
hgs a claude/sample-project/review
hgs @mac pause --all
```

Pause preserves an exact native conversation binding. It does not preserve open
network connections, child processes or an unsent native terminal draft. Supported
integrations include Claude Code, Codex, Kimi Code, the official DeepSeek Harness
and OpenCode. DeepSeek and OpenCode use their native engines instead of a
terminal-agent process. OpenCode sessions have no separate process to pause
([OpenCode controls and limits](opencode.md#controls-and-limits)).

For terminal agents, pause verifies the live pane, run and conversation identity,
native history, supported resume arguments and an idle state. It sends SIGTERM only
to the verified agent process, checking its PID and start time, and waits up to ten
seconds. It never kills the tmux server or escalates a failed pause to SIGKILL.
`pause --all` preflights every live session on the host: an untracked agent, busy
session or ordinary shell blocks the operation before the first signal. Once
stopping begins, some sessions can fail independently; inspect the nonzero result
before rebooting.

On Linux and macOS, **Terminate** also ends the agent's verified descendant
processes, including background jobs in separate Unix sessions. It temporarily
suspends the agent and its descendants while collecting a bounded, fresh tree,
pins every process to its birth identity, and confirms the complete stopped tree
before closing the exact terminal. After the terminal accepts teardown, it kills
the pinned agent and descendants and confirms their exit. The agent stays
suspended until it is killed, so shutdown handlers cannot fork new jobs after
verification. An incomplete check keeps the terminal; failed teardown restores
only processes suspended by this operation. A failed or unconfirmed stop does not
start worktree cleanup. Pause and native Stop keep their existing behavior.

Process names, shared process groups and the working folder never authorize
termination of other programs. Already orphaned processes whose session ownership
cannot be verified stay untouched; a surviving process using the checkout still
prevents its removal. Run Terminate from outside the target's own process tree.

Restore uses `claude --resume ID`, `codex resume ID`, `kimi --session ID` or the
supported native DeepSeek operation. It preserves the original folder, account
and project wrapper. There is no fallback to the latest conversation or a new
conversation when exact restore fails. `resume -d` and `resume --all` run in the
background; use `hgs a ...` when native startup needs input. A normal launch with a
saved name restores it; `--fresh` explicitly replaces its binding.
`hgs resume SESSION --fresh [-d] [--expected-run-id RUN]` starts a stopped Claude,
Codex or Kimi session with a new conversation from its own folder, account, safe
launch options and model, like `/clear` in a live agent. The replaced conversation
stays in native history and readable in Activity above a **Session cleared**
boundary; a running agent uses **Clear session** instead.

You can also resume native history created outside `hgs`:

```sh
hgs codex -c 01a0e955-b037-73f0-bc6c-36622a8c27b7
hgs codex /path/to/project --resume=01a0e955-b037-73f0-bc6c-36622a8c27b7 -n review
```

After `-c`, IDs are recognized as UUIDs; Kimi also accepts `session_UUID`. Without
an ID, `-c` continues the latest conversation in the project. An already tracked
conversation keeps its session name; another gets `resume-ID` unless `-n` is set.
An occupied name bound to another conversation is an error. Native history must
remain on the selected host: project synchronization does not move conversations.

Before the agent confirms its conversation and creates native history, pause is
unavailable. Codex and Kimi may defer creation until the first message. Codex can
also defer `SessionStart` after resume; Activity may send the first resumed message,
pause or clear only after verifying the exact process, matching history and empty
native input. Codex reports such a clear with the next message's `SessionStart`,
which then names the new conversation instead of the restored one. A sent first
message or an archive restore still waits for that confirmation.
Unknown flags or noninteractive modes such as `codex exec` are not guessed.
The original positional prompt is never replayed during resume.
For Kimi Code whose help advertises a non-interactive `--prompt`, Zerus removes
the initial message from native arguments and the saved resume recipe, then
submits it through the ordinary guarded send path for the exact launched run.
Folder trust and setup remain manual. `inspect` includes `launch_prompt` with
a preview of at most 200 characters plus an ellipsis, request ID and status.
The full text stays only in the launching supervisor until ordinary guarded
submission stores its input receipt. Errors are limited to 512 characters plus
an ellipsis. An undelivered message is cancelled when its child exits or shuts
down. After an attempted submission there is no automatic retry, including on
refusal or an uncertain delivery. Review
the preview and ordinary input receipt before sending it manually.
Capability detection and native spawn use the same resolved executable after
account environment setup. A custom `run_kimi.sh` keeps its own argument
contract and is never invoked a second time for help; `--bare` selects the
native Kimi launch behavior described here.

A launch interrupted before conversation confirmation is not a Saved session.
Repeat the same command without `--fresh`; the diagnostic attempt is preserved in
`attempts/`, and native history is retained. Old untracked sessions cannot be safely
paused: launch the intended conversation with its explicit native ID to bind it.

### Tracking and durable state

Every session record has a durable `session_id`, a UUID assigned when the
session is first created. Every write of this hgs keeps it: resume, Clear and
fresh starts, rename, pause, archive and restore. A fork is another session
with its own. A record written by an older hgs gains one the next time this
hgs writes any of its records, including archives.

The ID is stable across writes of this hgs version and later ones. An older
hgs, after a rollback or in a mixed fleet, rebuilds a record on resume
without this field; the next write here then assigns a new ID, once.
Consumers therefore treat `session_id` as a hint and fall back to the run and
conversation identities.
The agent's environment carries it as `HGS_SESSION_ID`, beside `HGS_SESSION`
(the name at launch) and `HGS_RUN_ID`, and `hgs ls --json` rows and
inspections include it. A hook first resolves the agent's `HGS_SESSION_ID`
through the ID index, under whatever label the session carries now. Failing
that, it prefers the record that carries the ID; when none does, for example
after an older hgs rewrote the record, the run identity alone decides, as
before.

A session this hgs creates keeps its history under its durable ID. Its
Activity, read marks and notification preferences are keyed by
`journal_name`, which is `session:<session_id>`, and `launch_label` keeps the
label it was launched with. Rename, resume, Clear, Start fresh and restore
keep both. A later write here can give the session a new ID after an older hgs
dropped it; the journal stays where it is. A `--fresh` launch over a saved,
confirmed conversation of the label is a new session with its own journal; only
a retry of an unconfirmed attempt continues that attempt's journal. A session created
before keeps the history of its label, pinned at its first rename. hgs v0.47.0
and later read `journal_name` and keep it on every one of those paths.

A command whose first argument is a session also accepts `--session-id ID`
instead, for example `hgs inspect --session-id ID` or `hgs rename --session-id
ID NEW-NAME`. A name given before the flag must be that session's current
name; arguments after it are the command's own. The value must be the
lowercase UUID shown by `hgs ls --json`, given once. Resolution reads the
verified index `ids/<session_id>.json` under the state directory, which names
the session's current label. A missing or stale entry costs one scan of the
records, which repairs it when the state lock is free. Hooks never repair, and
an index this process cannot write is left alone. An older hgs ignores the
index. Resolution only
finds the session's name: every record the command then reads under that name, live or
archived, must still carry the ID, so a name another session took meanwhile
fails with "Session ID ID no longer names NAME" instead of acting on it. `kill`
and `a` bind the terminal before their first effect: the name's immutable
terminal session must run the run that the verified record carries, and they
then act on that terminal session, never on the name. A `--run-id` given to
`a --existing` must name that same run. An OpenCode session's attach goes to
its own adapter, which checks its binding and run the same way.
`hgs task` keeps its own `--session-id`, `notify` and `paste` stay the tray's
and tmux's own entry points, and an agent's own flag after `--` is untouched.
With `@host` the flag reaches a peer only once its `hgs --help` lists
`session-id-selector-v1` and every marker the call needs already, including a
guarded `--expect-machine` call that wraps the command and `native-ui`, whose
peer resolves the ID itself; an older peer is refused with "update the
selected computer". Names stay unique for now. When several sessions later
share a name, commands exit with status 5 and list the candidates; see
`docs/superpowers/specs/2026-11-04-session-names-as-labels.md`. Because an
older hgs can change a session's ID once, a client whose `--session-id` is no
longer found treats the session as changed and never retries the call: its next
action uses the refreshed listing, with the name and its expected run and
conversation identities when the row lists no ID.

A session's terminal has its own name, kept in the record as `terminal_name`
(zerus-ucck P3b). A launch gives the terminal the session's label when no
terminal session has that name, so `tmux attach -t LABEL` reaches the session
as before. A record an older hgs wrote has no `terminal_name`; its terminal is
its label. A tmux session that hgs never started, such as one opened by hand,
may already hold the label of a saved session. A resume or Start fresh then
names the session's terminal `LABEL~<first 8 characters of its session ID>`,
or `LABEL~<session ID>` when that is taken too, and refuses when both are.
Earlier hgs skipped such a resume as already live. In that case `tmux attach -t
LABEL` reaches the other tmux session, while `hgs LABEL`, `hgs a LABEL`, `hgs
kill LABEL`, pause, terminate and every other command act on the session's
own terminal, after checking its pane and run as before; none of them touches
the other tmux session. `hgs ls` lists the session under its label. A rename
moves a live session's terminal to the new name and records it; when the move
fails, nothing changes. A rename of a stopped or paused session clears its
terminal name, so its next launch names one anew. A session is live only in
its own terminal, with its pane and run, or while a launch of its own label
holds that terminal; a tmux session another launch started under its terminal
name never makes it live and never hides it from `hgs ls`. When hgs cannot
read a terminal's environment to tell whose launch it is, it refuses and
starts nothing. A new launch, fork, rename or restore still refuses a label
that any tmux session holds. A build older than this one finds no pane for a
session whose terminal is named otherwise, shows it as not live and drops
`terminal_name` when it rewrites the record.

When several sessions share a label (zerus-ucck P3c), a command picks one by
the identities it carries. These come from its command line
(`--expected-run-id`, `--run`, `--run-id`, `--expected-conversation-id`,
`--conversation`) or from its JSON request (`expected_run_id` and
`expected_conversation_id`; `run_id` and `conversation_id` for `hgs read` and
`hgs notifications`).
- Exactly one session carrying them is the one, and every later read in that
  command still checks both identities against it.
- None carrying them is refused as a changed session (exit status 1). The
  command then reads none of the sessions under that label.
- With several, or with no identities at all, the command exits with status 5
  and lists the candidates (as JSON under `--json`). This holds for `inspect`,
  `pause`, `rename`, `archive`, `kill` and others that take no identities.
- `--session-id` and references always select exactly.
- `pause --all` and `resume --all` refuse a label that several sessions share;
  act on each with `--session-id`.

When a session leaves, hgs releases its own Git worktree lock. A session
leaves by an archive, by `terminate` with or without `--forget`, or by
forgetting its archive. Its own lock is one whose reason is exactly
`zerus:<session ID>`, on the checkout Zerus created for that session.
- hgs never touches a person's own lock, another session's, or any lock on a
  folder Zerus did not create.
- The lock stays in three cases:
  - another live or saved session uses the checkout;
  - the checkout's Zerus ownership cannot be verified;
  - the sessions that might use it cannot all be read.

  hgs then says on standard error why it kept the lock, and the archive
  stands.
- The release never forces Git. It sets the lock aside and checks its text,
  and removes it only when the text is exactly that session's. Anything else
  is put back, never over a lock that appeared meanwhile. A lock left aside by
  a crash is put back by the next hgs command.

Nothing in this version creates such a lock.

The desktop client adds `--session-id` only to explicit actions: send, send
now, answer, queued-message removal, clear and compact, interrupt, model
settings, pause, resume, start fresh, rename, fork, archive, terminate, kill
and forget. It does so when the listed row for the action's exact run carries a
session ID and that computer's `hgs --help` lists `session-id-selector-v1`; the
answer is kept for ten minutes. Reads, polls, history, status, watch, terminal
input and read marks keep the name until a label index exists. Confirmed
delayed and worktree-cleanup end actions keep their scoped envelope, which
already names the exact run and conversation. A refused ID shows as a changed
session, but only when the CLI refused it while resolving it: another name, no
session or several sessions. A later read's "no longer names", which can
follow an effect, keeps the action's own result.

`hgs session-action-status SESSION --json` reads one original lifecycle receipt.
Supply the original `session-action` JSON unchanged, including its `request_id`,
`action`, `expected_run_id`, `expected_conversation_id` and any original action
arguments. The request is limited to 8 KiB; the existing private receipt is
limited to 64 KiB. Its request digest, identities and result target must match.
The reader opens no writer or lock, initializes no missing state, and leaves the
receipt unchanged. It omits private native diagnostics.

A stored `claimed` receipt is reported as `uncertain`; a later fresh read can
observe the original writer's completed receipt. Missing evidence reports
`unavailable`. Neither result proves that nothing ran or authorizes a retry.
Malformed, unsupported or unsafe receipts are refused. This API covers only
`session-action`, including Archive, Terminate and Forget; `clear-context` has
separate receipts and is not accepted here.

For a peer, use `hgs @example-peer --expect-machine MACHINE_UUID
session-action-status SESSION --json`. The selected executable must advertise
`lifecycle-receipt-status-v1` and the existing machine guard before JSON is
forwarded. Its existing private Swarm catalog supplies the machine identity
without initialization, locking or saving. An unguarded remote receipt read is
refused. This reader is a recovery prerequisite; it does not enable durable
delivery of pending Undo actions when Zerus quits. The independently reviewable
[pure shared journal model](desktop-quit-journal.md) also leaves current Quit
behavior unchanged; the actual store and detached delivery remain separately gated.

The proposed [native quit-store design](desktop-quit-store-design.md) uses a
persistent per-OS-user location derived from the effective UID's passwd home,
never from `$HOME`: `<pw_dir>/.local/state/hgs/quit-v1/` on Linux and
`<pw_dir>/Library/Application Support/hgs/quit-v1/` on macOS. Before any production
anchor open, create or stat, an isolated launch must refuse durable delivery:
HOME differs from that OS home, XDG state/data/config points away from its default,
HGS_STATE_DIR or HGS_CONFIG_DIR was explicitly set, an install prefix is explicit,
CLI selection (`ZERUS_CLIENT_HGS` or `--hgs`) or fleet routing (`HGS_SELF`,
`HGS_PEERS`, `HGS_SSH`) is explicit, or a test/replay/headless marker is present.
Empty, non-UTF8 and default-looking explicit selectors also refuse. Native origin
evidence must precede Config::load's default HGS_STATE_DIR injection and the GUI's
`--hgs` conversion; later mutations cannot erase an original override. Such
launches keep drop-on-Quit. The proposed future caller uses a freshly selected,
translated note in the group's existing Undo toast at registration; a refusal
first discovered at Quit is diagnostic only, with closed codes and counts.
The initial strict fence also excludes the maintained Mac LaunchAgent's `--hgs`
launch and Linux launches passing it. Exact selected executable/configuration
origin must be qualified on both maintained launch paths before delivery is
enabled anywhere; this is tracked separately as `zerus-5wmh.66.2.34.3.5`.
Tests use typed fixture scopes and decoy anchors; they never probe the owner's
real anchor. Missing or tampered existing state requires explicit operator
recovery, with unresolved charges retained. These are design requirements; the
current runtime has no durable quit-store caller. Windows stays fenced.
The proposed cleanup restriction also needs a native Git worktree lock before
either version-2 ownership write: private legacy CLI controls showed that
version-2 refusal alone does not fence fresh old manual removal. Worktrees retains
the existing Blocked/locked reason. Exact new review/release semantics and the full
legacy caller, partial-cut and crash/restart matrix remain separate gates; no
runtime hold or unlock is implemented. Hold installation and delivery must not
ship before the reviewed release surface is available in the same or an earlier
build.

A private actual V1-hgs/native-Git race also showed that Git 2.56 can remove a
checkout, its V2 marker and a newly installed native lock after its already-issued
remover passed the absent-lock check. A Git lock therefore supplies no drain
proof. The proposed hold now requires verified native quiescence **and exclusion
through publication before any hold write**, across old configurations and paused
callers. Kernel birth/cwd/open-file evidence must be complete; hidden/unreadable
or omitted observations are UNVERIFIED. Existing process helpers and repeated
empty snapshots do not provide atomic exclusion, and no qualifying mechanism is
available yet. If that proof cannot be established, publish no hold or durable
delivery for the worktree and keep today's drop-on-Quit with the registration-time
Undo note and Quit-time diagnostic boundary above. Today's Tauri client has no
visible Quit notice; current toasts and the bounded local stderr drop line remain
unchanged until the qualified caller lands. No new confirmation, exit delay or
next-start notice is introduced for pre-hold refusal. After any partial hold,
retain denial/manual recovery instead of
rolling it back. No signals, restarts, process-name assumptions, old consent or
unlock/downgrade repairs close the race. See the design's exact negative evidence
and still-unprotectable legacy windows; R3, the full matrix and the shipped
release-surface gate remain unchanged. This is documentation only.

The proposed store and capture spool are excluded from all exports, diagnostics
bundles and Private analytics aggregate export and are never uploaded. Logging
and operator recovery output contain bounded counts, reason codes and digests,
never stored session names, paths or targets. Installers and updater flows must
preserve admitted helpers/children without signals or indefinite waits, and a
replacement GUI must respect their still-charged global generation.


`hgs hooks --account native-claude --json` inspects the account's hook
registration without running the agent. Use another exact account ID for an
isolated profile. The report contains provider/account identifiers, configuration
and path fingerprints, expected/present/missing/duplicate event names and cached version
compatibility, never settings, commands, prompts or credentials.
`unrecognised_tracking` is true when an entry runs `PROGRAM __state hook` or
`PROGRAM __state answer-channel` for an unrecorded program whose basename
contains `hgs` (case insensitive), but is neither an hgs basename nor the current
runner. The name is ambiguity evidence only, never ownership. Unrelated programs
using these arguments remain personal hooks and do not block repair; the report
never names them. This flag is only in the account
report; `hgs hooks SESSION --json` keeps its hooks-health-v1 fields. DeepSeek and
OpenCode report `not_applicable`: they use their native APIs instead of tracking
hooks. An unavailable or changed executable-version cache means compatibility
`unknown`; inspection never runs `--version` or refreshes that cache.

`hgs hooks SESSION --json` accepts a JSON object on stdin containing
`request_id`, `expected_run_id` and `expected_conversation_id`. Only that exact
bound conversation is inspected. Its `observed` field counts known main-agent
callbacks recorded after this feature was enabled in that run/conversation;
older bindings or silence report `unknown`, not broken hooks. The summary is
bounded by the known event names and never scans native history. Registration,
observed callbacks and provider-effective settings/trust are different evidence:
`effective` and `trust` remain explicitly `unknown` in a file inspection. Project
settings, policy, native trust or a running agent's already-loaded settings can
make its effective hooks differ. Attaching does not retrofit them.

Explicit configuration repair is native-only:
`hgs hooks --account ID --repair --json`. Its stdin must contain a fresh UUID
`request_id`, `expected_provider`, `expected_account_id`,
`expected_config_sha256` and `expected_path_id` copied from the inspected account
report. Repair refuses changed content, account/path/symlink identity, malformed
or oversized files and unknown/incompatible native-version evidence. It also
refuses, without writing, while `unrecognised_tracking` is true: run it with
`HGS_EXECUTABLE` set to that wrapper, or use a supervised launch with that
runner to record it. Otherwise review or remove the ambiguous entry manually.
Repair migrates recorded tracking/channel entries to the current runner and merges
missing HGS tracking registrations under the existing hook lock and atomic backup
rules, preserving personal settings/hooks and trusted legacy commands. It never
trusts hooks, enables/widens answer channels, changes permissions, restarts a provider or retrofits running
agents. If compatibility is unknown, explicit repair is unavailable. The current cache is
normally populated for Claude's version-qualified hooks; Codex and Kimi can remain
unknown even after launch. Their normal supervised launch retains the existing
hook installer, but this diagnostic never invents version evidence. Review the returned configuration
and any conditional matchers rather than treating a successful file edit as
proof that hooks are effective. Existing partial Kimi blocks require manual review
or a supervised launch: explicit repair refuses to rewrite them lossily.

Complete account-wide Kimi tracking registrations, including legacy Python
compatibility commands, are reused without adding another managed block. If
that block was already added and contains only the same HGS callbacks and
options, launch or compatible explicit repair removes the redundant block while
retaining the original commands and personal settings/hooks. Different options
or personal entries prevent removal. This edits registration only; a running
provider's already-loaded settings are not retrofitted.

Activity recognizes duplicate Kimi callbacks by their native turn ID within the
same run and conversation. Equal captions in separate native turns or cleared
conversations remain separate messages.
For older attachment callbacks that lack that ID, a complete bounded native
history and a unique exact staged-file input can associate their rich file card
with the one native turn. The confirmed local delivery can supply the receipt
before its durable inspection metadata arrives; both copies share one delivery
ID. Incomplete or ambiguous evidence never authorizes this native-turn
association. The original journal is retained.

The additive mobile operation `hooks_status` exposes only the exact-session
read, and only when the selected CLI advertises `hooks-health-v1`. Both outbound
connectors preserve the request/run/conversation identity. The relay and gateway
refuse account-wide, repair, archive and child extensions; no mobile hook write
operation or client repair UI is advertised. Diagnostic receipts have short
read-only retention and never authorize replay of a mutation.


At launch, `hgs` adds handlers to Claude `settings.json`, Codex `hooks.json` and
Kimi `config.toml`, preserving other hooks and making a `.pre-hgs` backup before the
first edit. It respects `CLAUDE_CONFIG_DIR`, `CODEX_HOME` and `KIMI_CODE_HOME`.
Handlers do nothing outside an `hgs` run. `HGS_TRACKING=0` disables registration for
new launches. Native hook trust still requires an explicit user choice in Codex.
Claude Code's `PostModelSwitch` is registered only once `claude --version` reports
2.1.296 or later: older versions stop every start at a settings warning for a hook
event they do not know. Until then each launch reads the version again; a version
that cannot be read gets no such event. A launch through a project `run_claude.sh`
never reads it, because a launcher runs only under supervision. Once registered,
the version is read again only when the `claude` executable changes; a version older
than 2.1.296 then has hgs remove its own `PostModelSwitch` entries before a tracked
start, dropping the event only when nothing else is left in it; started outside hgs
or with `HGS_TRACKING=0` first, it still warns until then. The user's own hooks for
that event stay, and older versions still warn about those. An unreadable version or
a launcher keeps the entry, as does a wrapper script that starts another Claude Code
without itself changing: delete the entry from its `settings.json` by hand there.
`PreModelSwitch` is never registered.

Claude Code can offer once, at startup or under a finished reply, to make auto
mode the default permission mode when the user settings name another default.
Activity shows the offer with both native choices; Zerus never answers or hides
it, and only the choice made in Activity or Terminal changes that setting.

Claude Code reports its context window only to a status line command. For a tracked
Unix start of the `claude` CLI that reports 2.1.296 or later (the version read as
for `PostModelSwitch`), `hgs` adds `--settings` with its own `statusLine`,
`hgs __state statusline`, to the agent's command line; no settings file changes and
the saved launch arguments stay as typed. The command keeps the run's latest
`session_id`, `model.id` and `context_window.context_window_size` from Claude's
input in `context-telemetry/` of the state folder, one file per run; a new run
removes those not updated for 30 days. It then runs the status line Claude would otherwise use:
the `statusLine` of the user settings file (`CLAUDE_CONFIG_DIR` or `~/.claude`),
then the session folder's `.claude/settings.json` and `.claude/settings.local.json`,
each later file's fields over the earlier ones. It runs that command with
`/bin/sh -c`, Claude's input, environment and folder, prints its output and exits
with its status, so Claude shows the user's status line as before and a change to
it applies at once. Its `padding`, `refreshInterval` and `hideVimModeIndicator` are
copied at launch. Without a user status line nothing is printed; Claude 2.1.296
then keeps one empty row below the prompt. Claude keeps its own status line, and
the session reports no window, on Windows, through a project launcher, with an older
or unreadable version, when the launch passes `--settings`, `--setting-sources`,
`--project-config-root`, `--managed-settings` or `--worktree` (`-w`), when managed
settings exist (`/etc/claude-code` or
`/Library/Application Support/ClaudeCode`, or `CLAUDE_CODE_MANAGED_SETTINGS_PATH`),
when a settings file cannot be read or its status line is not a command, and when
the session folder lies below another repository root (an enclosing repository, or
the repository of a linked worktree such as a generated one) and the local settings
of that root or of the folder set one: Claude reads one of the two by ownership checks.

Tracked Codex needs `--no-daemon`; the integration has been verified with 0.160.0,
while 0.155.1 lacks that required flag.

New hooks call `hgs __state hook`. Existing trusted Python commands are retained:
`hgs_state.py` forwards them to the Rust CLI, so keep Python for legacy hooks.
hgs recognises its own entries by the program they run: a path named `hgs` or
`hgs.exe`, a legacy `hgs_state.py`, exactly the current `HGS_EXECUTABLE`,
which a wrapper may name differently, or a program hgs recorded for that
settings file. A later launch therefore adds no duplicate registrations.
After a launch or explicit repair leaves an hgs entry that runs the current
program, hgs records that exact program for the resolved settings file in
private state (`hook-owners/` below the state directory, up to 8 programs of up
to 4 KiB each, written atomically under the settings lock). Health and repair
from a shell without that `HGS_EXECUTABLE` therefore still recognise the
entries a wrapper wrote. When the current runner changes (including a different
`HGS_EXECUTABLE`), the next install or compatible explicit repair rewrites those
exact recorded tracking/channel entries to the current runner and records it.
Shell migration accepts only the exact standalone form hgs writes; composed
commands, comments and alternative quoting remain byte-for-byte unchanged.
Other entry fields, personal hooks and trusted legacy commands are preserved.
An unrecorded old wrapper is never silently claimed or deleted: use the exact
wrapper as `HGS_EXECUTABLE` for a repair or supervised launch to record it first.
Nothing is recorded for a program hgs did not write.
Earlier versions added another group for a wrapper at each launch. Exact copies
of such a group are removed once, and Codex may ask to review that change. A
group that holds any personal hook is kept as it is, and a command for another
program is never treated as hgs's.
Codex can request **Hooks need review** before both a new session and a saved
conversation's resume or archive restore. Activity shows that exact native
decision; messages remain blocked while it is open. Only the person's explicit
choice trusts or skips hooks, and Codex persists that choice. A pending message
receipt is never retried by recognizing or answering the card. Normal supervised
startup preserves existing tracking definitions; an unchanged registration does
not require trust again merely because the Zerus binary was updated. New or
changed definitions can require a new native review.
The CLI and session core are Rust; SQLite is bundled and requires a C compiler at
build time. Existing session bindings and provider history remain compatible.

Session records live in `${XDG_STATE_HOME:-~/.local/state}/hgs/sessions`, or
`HGS_STATE_DIR`. Writes are atomic and synchronized to disk. Records contain the
command and arguments, folder, conversation ID and run identity; environment
secrets are not saved. A secret passed directly as an argument is still an argument
and can appear in the private record. Native agents retain their full history.
Late events from a previous run and child-agent events cannot rebind the parent.

`runtime_state`, `process_state` and `conversation_state` distinguish the tmux
terminal, agent process and native conversation. `SessionEnd` alone does not prove
process exit. A live process without verified activity shows **Status unknown**;
**Agent exited** means the process ended while its tmux terminal still exists.

### Child sessions

A new session that a live tracked agent launches on the same computer is
that agent's child. Terminal-agent forks also record their launching caller. The launching `hgs` proves its caller by native process
ancestry, as the process-proven mode of `hgs self` does, and passes it to the new session as
`HGS_PARENT`; it always sets the variable, empty when no live agent owns the
launch, so an inherited or typed value never reaches the record. The new
session's first record keeps it as `parent`: the parent's machine UUID and
durable `session_id`, its name, conversation and run as hints, `source`
(`launch`, `adopt`, `manual`, `claimed` or `forwarded`) and the time. A resume never adopts its launcher. Resume,
Clear, fresh starts, rename, pause, archive and restore keep the link, as they
keep `session_id`; an older `hgs` that rebuilds the record drops it. Claude,
Codex, Kimi, DeepSeek and OpenCode sessions can be children and manually chosen
parents. New DeepSeek and OpenCode bindings record the proven launcher directly,
before native creation; they never trust an inherited `HGS_PARENT` value.
DeepSeek and OpenCode agents cannot prove process ownership from their shared
host. They can explicitly claim a parent identity: `adopt` may also name
`parent` with an exact `session_id`, `run_id` and `conversation_id` (optional
name hint). The parent must freshly match a live session on this computer;
the recorded source is `claimed`, distinct from process-proven `adopt`.
The same choice is accepted by new `hgs claude/codex/kimi/dsh/opencode` launches
as `--parent JSON`, for example `--parent "$(hgs self --json | jq -c .parent_identity)"`.
This explicit choice overrides automatic launcher detection, must still match
at launch, and never reparents an existing/resumed session. Use `hgs parent`
for an existing session. The installed help marker is `claimed-parent-v1`;
explicit peer launches require it. The selected computer checks the claim
against its own sessions, so the claimed parent must be live there: a claim
copied from an agent on another computer (`hgs @peer dsh … --parent "$(hgs self
…)"`) is refused and nothing is recorded. `hgs self` never routes to a peer.
A DeepSeek or OpenCode parent must confirm it is live within 2 seconds; a host
that has not answered by then is refused as busy, never counted as live.
Claimed links are presentation/attention metadata like manual links and grant
no operation or permission authority.
A launch or fork on another computer (`hgs @peer claude …`, `hgs @peer fork …`)
run by a proven agent passes that agent's link to the peer as
`--parent-link`, hex-encoded JSON, only when the peer's `hgs --help` lists
`launch-parent-v1`; an older peer launches without it. The peer accepts only a
well-formed launch link naming another computer, records it with the
`forwarded` source and uses it only when no local agent owns the launch. It
cannot verify a parent on another computer, so its listing always shows a
forwarded parent as `missing`, even when its IDs match a session there, and the
child is never supervised. A cycle check stops at a forwarded parent.

`hgs [@host] parent SESSION --json` reads JSON stdin with
`expected_run_id`, `expected_conversation_id` and `action`: `read`, `adopt`,
`set` or `clear`, plus optional `replace`. `set` names `parent` with the chosen
session's `session_id` and, as the desktop listed them, its `name`,
`conversation_id` and `run_id`; the host reads that session on its own computer
the same bounded way, and refuses an archived, changed or missing parent, the
session itself and a cycle. `hgs --help` lists `session-parent-v1`; a call to a
peer without it is refused before it reaches the peer. `adopt` makes the calling agent, proven the
same way, the session's parent. It refuses a caller that no live agent owns, a
session adopting itself, archived sessions, a target that already leads the
caller on this computer (a cycle), and a session with another parent unless
`replace` is true. The cycle check follows at most eight of the caller's
parents through the session ID index, reading each record bounded, as a
regular file only, never following a final link or waiting on a FIFO; a
longer chain or an unreadable parent is refused rather than assumed acyclic. Coordinators use it for workers they direct but did not
launch. The answer names the link now in effect and the previous one. Links
are presentation and attention routing only: they never authorize a send,
answer, approval, stop or removal, and never change project membership.
`hgs wait` dependencies, native subagents, names and folders never create one.

On the desktop the session menu offers **Set parent…** (a searchable list of
the computer's other Claude, Codex, Kimi, DeepSeek and OpenCode sessions, never
the session's own descendants) and, for a child, **Detach from PARENT…**. Dragging a session card
onto the middle of another card on the same computer frames it and asks to
make the dragged session its child; the upper and lower parts of a card still
reorder. Every change is confirmed in a dialog, which names a parent it
replaces, before the desktop calls `hgs parent`.

`hgs ls --json` rows carry `parent` with `state` from the same listing: `live`
(running, its agent not exited), `saved` (paused, stopped or exited),
`archived`, or `missing` when this computer lists no such session (gone, or on
another computer). Neither the listing nor the launch waits on the shared
catalog: the link's machine identity is read once a caller is proven, without
the catalog lock, and a launch without a readable identity records no parent.

The desktop session list nests a child under its parent when both are shown in
the same project group, 26 px per level up to three levels, with a connector
from the parent above. The parent's footer shows **Team** with working and all
child sessions and native subagents together, working of all (`2 of 5`); this
one toggle shows or hides both (owner, 2026-11-07, zerus-sa9qn.16), and Left
and Right do the same from the keyboard, Left then moving from a child to its
parent. A native subagent row looks like a compact session row (the agent's
tile, its parent's account mark, its name and status, the summary as its
tooltip) and never expands; the expand button's place stays empty, so its
status lines up with the rows around it (owner, 2026-11-08). This
device remembers a collapsed parent across relaunches by its computer's machine
UUID and session ID (`sessions/collapsedTeams`, at most 2048 parents, the oldest
forgotten first), so a rename, resume or restart keeps the choice; a parent
without those IDs keeps it for this window only. A child row's compact or full
form is remembered like any row's. Nesting stops after eight levels; a deeper child starts at top
level again. A child whose parent is not shown above it stays at top level with
**from PARENT**, which opens the parent when it is listed. A parent is matched by
its computer's machine UUID and session ID, or by conversation for a parent an
older `hgs` listed without one; names never match. A loop never hides a card.
Cards show a large agent tile at the left: the agent's mark from the owner's
design prototype (Claude, Codex, DeepSeek, Kimi and the shell; OpenCode keeps
its glyph) on a bordered tile, 34 px on cards and 22 px on compact rows. The
tile uses the skin tokens `list.agent-tile` and `list.agent-tile-border`: the
prototype's near-black tile with a thin outline in the dark skin, and a white
tile with a firmer outline in the light skin, where it gives the colored marks
their best contrast. The artwork loads lazily, outside the entry bundle; until it arrives the
tile shows the provider glyph. A compact parent row takes a second line that
sums up its child sessions: working of all, **Needs you**, **Asking** and
**Errors** counts, with the same collapse toggle; childless compact rows stay
one line. On rows narrower than about 340 px the counters show their numbers
only and the toggle keeps its place; the toggle's label and the row's
accessible description keep every count in words. Rows stay transparent and
paint a surface only on hover and selection. A selected card uses the skin's
green-tinted card surface and an accent border, as in the owner's prototype,
with attention, unread, draft and error edges drawn on top; while the list has
keyboard focus the selected card also gets an accent ring.

Supervised children draw no attention for requests their parent can handle.
A child is supervised when its own computer lists its parent as `live`; rows
with a parent then also carry `human_request`, true while the child holds a
request only the person can answer: a tool or permission approval, its own or
a native subagent's, a folder or Codex hooks trust prompt, or signing in (an
authentication failure or **Sign in required**). A supervised child is quiet
only when its host reports `human_request: false` and the row itself shows no
such approval or sign-in; a host that does not report it keeps every request
visible. A quiet child behaves like a muted session: no system notification, no rail, tray, menu bar or project header
count, and gray badges and edge, while its status, Activity, unread reply,
questions and approvals stay as they are and it remains in the Active and
Needs attention filters. Nothing is marked read. Review later, the person's
own mark, still counts. A person-only request notifies as for any session.
When the parent is paused, stopped, exited, archived, not listed on the
child's computer (including a parent on another computer) or gone, the child
is ordinary again: a pending request that waited quietly is announced once,
while replies seen meanwhile are not replayed. A supervised child shows a dim
arrow turning down and right before its title, meaning its parent above
handles it (its tooltip and accessible name say which session supervises it;
owner, 2026-11-07, zerus-sa9qn.16, replacing the crossed bell); the parent's **Team**
toggle adds **Asking** (quiet requests waiting for it) and **Needs you**
(person-only requests), which never raise the parent's own attention.
`client/core/tests/fixtures/supervised-attention.json` lists the cases the
desktop projects and Android and iOS check.

A supervised child's blocking question also goes to its parent session, once
(zerus-sa9qn.8, owner approval of 2026-11-12). This is an owner-approved,
service-originated route, not the person's consent: the durable desktop
service on the child's own computer originates one guarded `hgs send` to the
live parent with no per-message confirmation, and nothing else. It never
answers, selects or approves anything, never builds a UI permit and never
changes who is alerted: the child stays quiet exactly as above, and every
person-only request still notifies. The service watches its own computer's
rows on every catalog event, whatever the notification settings. A row is an
edge when it is a running Claude child waiting for input (`phase: input`)
whose parent is `live`, not forwarded, and whose host reports
`human_request: false`; a new `attention_signature` is a new edge, and a
question already pending at startup counts once. After about 3 seconds the
service reads the child with one bounded `hgs inspect NAME --after
9223372036854775807 --skip-processes` and its `hgs reference`, at most one
read per child and two in all, each with a 5-second timeout, from a
deduplicated queue of at most 64 edges; it reads again immediately before
sending. Only Claude's own AskUserQuestion card routes: source `hook`, the
main agent, not optional, no `answer_delivery`, `can_answer: true` and a full
identity. Claude Code's own prompts (`claude_native_question`), every
synthetic native card, approvals, trust and sign-in never route. Codex asks
blocking questions only through native prompts and its async questions are
optional, so a Codex child has nothing to route in v1; Kimi, DeepSeek and
OpenCode questions are a follow-up (zerus-sa9qn.34).

The parent's own current row on the same computer, matched by the link's
session ID, must be running a terminal agent (Claude, Codex or Kimi); the send
names its current run and conversation. The deferred-send actor runs the one
fixed `hgs send PARENT --json` as its `route_question` kind, with the usual
guarded checks (exact run and conversation, native input state; a draft or
busy layout refuses). Its request ID is a name-based UUID of the question
identity (RFC 9562 version 8 from SHA-256: machine, child session, run,
conversation, question ID and hash), so it is idempotent across restarts. The
route ledger reserves each identity in the same transaction as the actor's
original and settles it from the actor's receipt only: `submitted` (the CLI's
own receipt), `refused` (before any child ran, an invalid text, or more than
20 routed questions for that parent in the last hour) or `unknown`
(everything else, including an `hgs send` refusal, which leaves no receipt).
Nothing is retried; an unknown result is never replayed, and a routed or
refused identity never routes again. The text is a fixed English template of
at most 2 KiB: the header `[Zerus child question]`, the child's name and
reference (the name single-quoted; a child whose name holds anything but
ASCII letters, digits, `.`, `_` and `-` in three segments is never routed and
is recorded `refused` as `question_route_invalid`), the question kind and option count, at most 300 characters of the
first question as one quoted line labelled as the child's data, `hgs inspect
'<reference>'` for the full question, the exact `hgs answer CHILD --json`
fields (run, conversation, question ID and hash) and "If only the person can
decide, say so in your reply." It carries no ticket, wait or other authority.
v1 routes on Linux only; macOS follows the deferred-send actor's macOS child
seam and Windows keeps today's behaviour. There is no opt-out in this slice
(zerus-sa9qn.32) and no Activity outcome line yet (zerus-sa9qn.33). The
service advertises `Desktop question route: route-questions-v1` in `hgs
--help`; an older service does not route.

Android (zerus-sa9qn.4) and iOS (zerus-sa9qn.5) apply the same contract, iOS
in ZerusKit `ChildSessions` and the session card: child cards nest under a
parent shown in the same project group, 16 dp per level up to three levels
with a connector, and a child without a shown parent stays at top level with
**from PARENT**. A parent's counter is the same single toggle for its shown
subagents and child sessions (`2 of 5`, gray **Asking**, amber **Needs you**),
expanded by default; the phone remembers a collapsed parent across relaunches
by machine UUID and session ID, at most 2048 (iOS keeps them in its sealed
`collapsed_teams` store). A quiet child gets the mute
treatment on the phone: no system notification, gray badges and reply count,
and the same arrow before its title; a request that waited quietly notifies
once when the parent is gone, and a completion seen meanwhile is not replayed.
Cards show the account mark (circle, letter and label) resolved by the row's
computer and recorded `account_id` against that computer's account catalog,
in the color that computer keeps for the account (the row's `account_mark`),
else the label's default palette color. See
`docs/superpowers/specs/2026-11-07-account-labels-child-sessions-design.md`.

### Rename, Saved and Archive

**Rename…** changes the label after the project name without replacing the process
or working folder. Tracking and journal history follow the new name; attached
terminals receive updated titles. Archive renaming targets only the selected ID.

```sh
hgs rename codex/project-docs/review codex/project-docs/database-review
hgs @mac rename claude/sample-project/review claude/sample-project/deploy
hgs rename codex/project-docs/review codex/project-docs/database-review --archive ID
```

Live and Saved names must be free. Archive names may repeat. Labels accept Unicode
and internal spaces, but reject `/`, `:`, `.`, control characters and surrounding
whitespace. `--dry-run` validates without mutation; `HGS_TAB=0` disables title updates.

**Saved sessions** are unfinished conversations paused explicitly or left after
process interruption, tmux loss or reboot. **Archive** contains completed
conversations. For automatic archiving, a tracked run needs both native main
conversation completion and a successful process exit without evidence of pause
or an external signal. `SessionEnd`, an SSH disconnect or a completed reply alone
is insufficient. Older ambiguous records are not reclassified automatically.

```sh
hgs archive codex/my-project/review                 # stopped sessions only
hgs ls --local --json
hgs inspect codex/my-project/review --archive ID
hgs resume codex/my-project/review --archive ID -d
hgs kill codex/my-project/review --archive ID        # forget only this binding
```

Archives live in `archives/`, each with an independent ID. A new launch with the
same name preserves the archive. Restore refuses an occupied live or Saved name
and removes an archive binding only after the native agent confirms the expected
conversation. Failed restore keeps it. **Forget archive** preserves native history;
`resume --all` excludes archives.

Desktop Restore keeps the opened conversation and its message draft when the
resumed row confirms the archived conversation ID. Text, attachments and editor
selection move to the live draft key; machine/project/search scopes stay in
place. An existing live draft is never overwritten: the archive draft remains
in Saved drafts. A later user selection is not replaced when Restore completes.

### Browse Archive by date

Desktop and Android filter **Archived date**, the time a conversation moved into
Archive, independently of its creation or last message. Choose All dates, Today,
7/30 calendar days, or an inclusive From/Through range. Days use the viewing
device's local time zone, including daylight-saving changes. Pages request the
most recently archived instances first; the desktop retains saved row order within
each project. **Refresh** starts a fresh view. **Older sessions** and **Newer
sessions** replace the current page, retaining at most 50 rows per selected online
computer. Only two cursor anchors per computer are kept, without a growing page
or cursor history; every matching record remains reachable in either direction.
The compact period button opens quick ranges and a custom From/Through picker.
Quick ranges fill the From/Through draft and keep the picker open, so either date
can be adjusted before **Apply range**. Only Apply changes the active date filter;
opening a picker, choosing a preset, Cancel, Escape or dismissing the panel never
fetches a page. **All dates** shows **Any date** for both bounds and applies no
date limits. Calendar inputs follow the app's desktop theme.
The navigation row shows the current range for one computer or the loaded/matching
count across computers. Hover the desktop count or expand the mobile count for
each computer's exact page range. Older/Newer arrows sit beside this summary;
their accessible labels describe the direction. Refresh uses the adjacent icon.
The Archive badge counts rows in the current project/machine scope, with `+` when
the current window does not contain the whole archive. An unavailable
computer keeps its own error and retry state; changing dates, search or machines
discards responses belonging to the previous view.

Upgraded desktop and connector polls use `hgs ls --json --local --no-archives`.
For older Android clients, the connector attaches a cached recent window of at
most 50 archived rows/128 KiB and explicit `mobile_archive` loaded/total/partial
metadata. It refreshes that window only when the native directory revision
changes or five minutes expire. Native identity checks and the existing fresh
snapshot gate remain in force for that window. Updated Android ignores this
compatibility window when page operations are supported; an old relay can still
show it while the system updates. Full Archive browsing needs the updated client.
Archive rows carry the current host-owned `notification_preferences`, using the
original journal and native conversation identity. Both connectors retain the
complete preference or explicit unknown value in their bounded previews; a
known relay conversation never silently loses its preference revision when it
is archived. Corrupt host preferences remain unknown. Native pages keep their
250,000-byte summary budget; one larger row is returned alone if it fits
768 KiB, within the 1 MiB response bound, so a valid 384 KiB preference can still
be read. The compatibility preview remains capped at 128 KiB and omits an
oversized row.
The legacy `ls --json` default remains compatible. The native, read-only ABI is:

```sh
printf '%s' '{"from":0,"until":253402300800,"limit":50,"cursor":"","query":""}' | hgs archives --json
```

`from` is inclusive and `until` exclusive Unix seconds (integers, nonnegative);
`limit` is 1–50. All fields are optional. `query` is literal case-insensitive
archive metadata search (names, IDs, provider, folder and title), at most 256
characters; this is distinct from the desktop's global public-message search.
The result contains `version:1`, `sessions`, `total`, `next_cursor`, the
additive `previous_cursor` and zero-based `position`, and the echoed
range/query. A host that counts only up to a cap may add `total_capped: true`;
the desktop then shows the total as "N+" and accepts a window that starts past
it. `hgs archives --json --bounded` (help marker `archive-bounded-pages-v1`)
asks for such bounded pages: the total and the position count at most 10,000
matches (`total_capped`, `position_capped`), the empty query included, and each
scan looks at most at 25,000 catalog rows in date order. A page whose scan
reaches that budget before its limit of matches is `partial`: it may be short or
empty, `searched` gives the rows it looked at, and its cursor continues from the
last row it looked at, in either direction. Without `--bounded` a page is
exactly as before; the desktop sends the flag only when its CLI lists the
marker, and a peer whose CLI does not list it answers the exact page. Pass
either opaque cursor with the same range/query to move older or newer. Both
directions use date/ID keysets; no offset scan or retained client cursor stack
is required. Ordering uses `(archived_at, archive_id)` descending, avoiding
offset duplicates when an earlier page changes. Newer completions appear after
Refresh. Deleted or restored instances disappear; all actions still recheck
exact native identity. Cursors belong to one computer/catalog and cannot be
reused after rebuilding it.

A rebuildable `archive-catalog-v1.sqlite3` indexes metadata beside the durable
state. Archive JSON files remain authoritative. Its first use indexes existing
records; unchanged polls/pages reuse it, and atomic archive writes, rename,
restore or forget update the directory generation. Only requested rows open
conversation summaries, outside the state writer lock. Browsing never starts an
agent or changes read marks. New archive pages use the additive mobile
`archive_list` capability; older computers retain their previously loaded archive
view and need an updated CLI/connector/relay for paging. Archive IDs are admitted
from validated pages for one hour (bounded to 2,000 recently paged instances),
while native inspection and lifecycle actions still verify name/archive/run.
An already-open older archive also survives a connector restart or cache expiry:
`hgs archives lookup --json` accepts only `name` and `archive_id`, reads that one
authoritative record without scanning the catalog or transcript, and returns its
name/archive/run/conversation identity. The connector verifies this response and
any requested run/conversation before admitting an uncached target. Failed or
mismatched lookups grant no admission; lifecycle operations keep their native
fresh checks. This internal read ABI adds no relay mutation or phone operation.

Desktop bulk removal is explicitly **Remove loaded archived sessions** when
paging is enabled. Its confirmation names/counts those loaded instances in the
current date/machine scope; unloaded entries are kept. Local user grouping follows
the exact observed run even when its reusable session label later changes owner.

## Project wrappers

On Linux and macOS an executable `run_COMMAND.sh` in the project folder runs
inside the tmux session. On Windows `run_COMMAND.ps1` runs through the Windows
session host, with the existing PowerShell selection: a canonical native
`pwsh.exe`, then Windows PowerShell. A Store alias that cannot be
resolved to a native executable is not selected. `--bare` bypasses the wrapper.
Use the script to prepare the environment and launch the native agent.

Unix keeps its existing `.sh` behavior and does not automatically select `.ps1`.
A project can provide both platform-specific wrappers. No PowerShell dependency
is introduced on Linux or macOS.

The Windows wrapper runs with `-NoLogo`, `-NoProfile` and process-scoped
`-ExecutionPolicy Bypass`. Zerus does not change machine policy or load a user
profile. Saved recipes retain the wrapper and its arguments across resume,
fork, recovery and Start fresh. At spawn, a fixed encoded bootstrap transfers
the script path and argument array as data, then clears that private data before
invoking the script. This avoids PowerShell `-File` splitting a literal argument
such as `-name:value`; empty, quoted and Unicode strings reach `$args` intact.
The wrapper remains responsible for forwarding those arguments to its agent and
returning its exit code. A new Claude profile whose tracking-hook form is not
known must first initialize native tracking with `--bare`, or explicitly run
untracked with `HGS_TRACKING=0`. Zerus does not probe the PowerShell interpreter
as Claude or invoke a project script a second time for its version.
Windows PowerShell 5.1 has
[native argument forwarding limitations](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_parsing?view=powershell-5.1)
for empty strings and embedded quotes. [PowerShell 7.3+ improves native argument
passing](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_parsing?view=powershell-7.6); its Windows mode still uses legacy rules for batch/npm shims. Prefer a
native agent executable when forwarding complex arguments.

PowerShell stays the supervised process while its native agent is a child.
Existing native process/confirmation guards still apply. In particular, a
Codex resume/fork may need its first message in Terminal until native startup
confirms the conversation; a wrapper is not evidence of that confirmation.
For a plain PowerShell shell on Windows use `hgs sh PROJECT --new -d`; this is
already supported and is untracked, like an ordinary Unix shell session.

```sh
hgs codex sample-project                 # run_codex.sh starts a new conversation
hgs codex sample-project -c              # run_codex.sh resume --last
hgs codex sample-project -- resume ID     # continue this exact conversation
hgs codex sample-project --bare          # bypass the wrapper
```

Explicit arguments after `--` suppress automatic Codex `resume --last` insertion,
even with `-c`. Use `-- resume --last --model MODEL` when needed. Claude and Kimi
forward `-c` to their wrapper. Existing sessions attach without rerunning the
wrapper. Interactive setup needs an attached launch; detached mode assumes the
required environment is already available. Never put private keys in wrappers.
Tracking never runs `run_codex.sh` to detect Codex capabilities; it runs only as
the launch and always receives `--no-daemon`, so it must start a Codex release with
that flag. `--bare` checks the native Codex and omits the flag for an older release.

<a id="desktop"></a>

## Desktop workspace

The tray's **Open Zerus**, a left click or `hgs-tray --sessions` opens the workspace.
On macOS, use `~/Applications/hgs-tray.app/Contents/MacOS/hgs-tray --sessions`.
On Windows, starting `hgs-tray.exe` without options opens it too; `--background`
starts in the tray only ([Windows](#windows)).
Repeated launch raises the existing window. Closing it leaves the tray and agents
running. The executable, bundle path, bundle ID and service names remain `hgs-tray`
for compatibility. Windows, notifications, the tray, `--version` and the Linux launcher
show Zerus. On macOS, a localized display name shows Zerus in the Dock, Finder,
Spotlight and System Settings for the unchanged `hgs-tray.app` bundle.

Settings → Mobile connection also lists and pairs [phones](#machines-and-accounts). Settings → Mobile connection → Phone notifications sets when the phone notifies: immediately, as soon as you are away from every Zerus computer, or after 5, 10, 15 or 30 minutes away. The choice is shared through the swarm catalog; computers this one synchronizes with directly whose hgs is too old to receive it are named there.

The tray has two counters: **active** sessions (the session list's Active filter:
working or Background, Scheduled or Waiting, or needing attention, a subagent
that asks included) and **Needs attention**. Attention includes input, approvals,
errors and unread main-agent replies. An offline host adds only its Review later
sessions to either counter; a dot marks incomplete data. The tooltip keeps the
total number of sessions.
Linux application badges and the macOS Dock show the attention count, hiding zero.

Zerus Next's tray menu (on Linux, macOS and Windows) has:
- **Open Zerus**, and an update's entries when there is one;
- one line per computer: how many sessions it has (`12 here`, `5 on mac`), or
  that it is unreachable or still polling;
- **Quit**.

It lists no sessions: open them from the workspace. It has no **Open in**
either. New sessions and folder shells still open the way `open-mode=` in
`tray.conf`, or a mode chosen earlier, says ([external terminal
modes](#external-terminal-modes)).

The session menu's **Mute** disables notifications for that conversation on all
devices and removes it from their notification counters and bells. The session's
host stores the preference; other desktops and phones receive it in their next
host snapshot. A crossed-out bell and neutral gray or white attention and unread
badges identify a muted session. The preference survives restart, rename and
resume of the same conversation. Muting preserves the agent's actual state,
unread replies, questions, approvals and review reminders. The menu requires an
online host with shared notification preferences and a configured local `hgs`
that advertises `session-notifications-v1`; older hosts show an explanation
instead of a separate local-only preference. A pending or failed change never
replaces the last confirmed host value.
If a host later becomes unavailable or stops reporting this feature, its last
confirmed muted display is retained and labelled as such; it does not enable a
new change. Concurrent changes use the host's revision and require a fresh
snapshot after a conflict. Unmute records the host's current notification
identities, so a device that missed the muted interval does not announce those
old events when it reconnects. This does not mark the underlying work as read.
**Unmute** restores future notifications without replaying events seen while
muted or acknowledging pending work. Muted sessions remain available in the
Needs attention filter.

The desktop service accepts notification clicks only for a live, unblocked
attention request on Linux, macOS and Windows. After a read, mute or resolution
retires that request, a later click is ignored even if the operating system has
not yet removed the banner.

Notifications open Activity for the exact requesting run and conversation,
including remote or renamed sessions. Polling and restarts do not repeat the same
request; resolved notifications are removed. Main-agent reply banners wait until
the session is idle and has no reported active children. While native agent work,
live background commands or active subagents continue, intermediate replies remain
unread in Activity but show no new-reply badge or reply-only attention count on
desktop or Android. The pending reply becomes visible when work finishes; this
never marks it as read. Explicit questions, approvals, errors and review reminders
remain actionable. Optional Codex questions, which the agent continues without,
are not attention: on the desktop, Android and iOS they change neither the
session's status nor the Needs attention filter, counters or badges. Listings report them separately
as [`optional_questions`](#optional-questions-in-listings). Settings → Sessions →
**Notify about optional questions**, off by default, adds an *Optional question*
banner once for each new question. Questions already pending when it is turned on,
or first seen while Zerus is focused or the session needs action, are only
recorded; a stop, resume or temporarily unknown count announces nothing again. The
banner opens the conversation's Activity. It is withdrawn when its questions are
answered or skipped, the session is marked as read here or on another device, or
the setting is turned off; delivered Windows messages remain but no longer open a
session. Routine replies from a turn the
agent scheduled itself (Kimi `CronCreate`) are not new replies while a recurring
job remains: they appear in Activity without an unread mark, badge or banner. The
reply after the last job ends, a one-shot reminder and any reply to the person's
prompt notify as usual. Explicit input and approval
requests still notify immediately. Ordinary child results and errors stay with
the parent. While Zerus has application focus, session banners are suppressed and
are not replayed when focus leaves. Unread replies and attention requests remain
visible in the interface, including requests from other sessions and machines.
Delivery failures show a reason and retry after a minute while still relevant.
macOS uses Notification Center and needs its system permission; Focus and OS
notification settings still apply. Notification Center looks up the icon by bundle
ID, so a stale per-user IconServices entry can keep an earlier icon after updates
while Finder and the Dock show the current one. Remove
`$(getconf DARWIN_USER_CACHE_DIR)com.apple.iconservices/store.index`, then run
`killall iconservicesagent NotificationCenter`; both restart on demand.

While Zerus runs, even with its window closed, it records keyboard/mouse idle time
and screen lock for phone notifications. macOS uses CoreGraphics without a
permission prompt. Linux uses the desktop's screen-saver and idle interfaces (KDE
Plasma, GNOME and other compositors with Wayland idle notifications). When none
is available only input in Zerus windows counts, which can only cause extra phone
notifications.

**Sessions**, **Projects**, **Machines** and **Accounts** share the navigation rail;
click the logo for **Overview**. Overview shows attention, work in progress,
online machines, CPU, memory and load, plus account limits. Counts open the matching
filter, sessions open Activity, and machine settings lead to Machines. Native
account quotas are grouped by verified identity instead of adding machine usage.

**About** is the final Settings section, after **Voice input**.

**Settings → Voice input** selects the speech computer, its recognition profile
and the local microphone. The speech computer is independent of the agent's
computer: a laptop can record locally and use a configured GPU peer over SSH.
The provider manager creates, edits, tests and removes profiles on the selected
processing computer. Choose **Local GigaAM** (model and CPU/GPU) or an
**OpenAI-compatible server** (API base URL, model, language and optional API key).
An empty language lets the server detect it; an explicit language uses a two-letter
code such as `ru`. Keys remain on the processing computer and are never shown
again; Keep, Replace and Remove are explicit choices. Test checks the saved
profile with synthetic audio and cannot send input to an agent. Removing a
profile requires confirmation. Choosing a default is separate from browsing or
editing profiles. Configuration is available even when inference is not running.
See [provider management](speech-providers.md) and
[host installation](../services/speech/README.md). Local GigaAM starts with the
Russian `v3_e2e_rnnt` profile. Saving or selecting a profile does not install or
download a model and never falls back to another provider.

On desktop, click **Voice** in the message toolbar to start recording. The icon
immediately to its right switches voice/video mode without starting capture;
it stays disabled while opening, recording or retaining a preview. Recording
continues after the click. Click **Send** to send, or **Stop and preview** to
review it with **Play**, **Discard** or **Send**. Enable **Preview before sending**
in Voice input settings to show **Stop** instead of **Send** while recording
audio or video. The setting is local to this device and applies to the next
recording; existing drafts and deliveries are unchanged. Audio recordings
are limited to two minutes and remain separate from the typed draft. Capture
opens the local microphone without waiting for speech computers or providers.
Provider discovery runs in the background. If it fails, the recording remains
available for playback; refresh providers in the preview before sending.
An unavailable selected provider stays selected until you explicitly change it.
A retained recording also pins a versioned provider's configuration revision;
editing that profile requires explicit reselection before sending the recording.
During capture, the typed draft is preserved and read-only, and ordinary message
submission and attachment changes are blocked. History remains readable.
**Space** or **Escape** stops recording into the retained preview; neither sends
nor discards it. Recording takes priority over Escape's agent-interruption
action. Holding either key stops capture only once, and editing resumes after
capture ends unless another delivery lock applies. Android supports the same
controls with a hardware keyboard; its touch Stop control remains available.
On Android, an empty editor with no attachments shows a microphone in the Send
position. Tap to switch voice/video, hold to record, release to send,
slide left to cancel or up to lock. Stop a locked recording to keep a preview.
The recording row overlays the retained editor and shows a red indicator, elapsed time and **Slide to cancel**;
the hint moves and fades as you slide left. Short haptic feedback accompanies
confirmed capture start and explicit stop/cancel, respecting Android settings.
Text or attachments restore the ordinary Send button. Sending requires a
confirmed provider for the exact conversation; delayed discovery keeps a preview
until an explicit Send.
After granting microphone permission, press again to start recording.
The preview lists speech profiles reachable from the conversation computer. Explicit Send
stores the original there, starts recognition and delivers recognized text to
the pinned agent conversation. Video is limited to 60 seconds and 10 MiB. Its MP4 and extracted WAV share one
message identity; the agent receives recognized speech. A tap starts circular
video playback. One history card retains the playable original,
expandable transcript and processing or delivery errors across devices. Its
circular play/pause control sits beside the recording's amplitude waveform;
the played portion is highlighted, and selecting a point on the waveform seeks
within the recording and keeps paused audio paused. The time display follows
playback. Older recordings gain their waveform in the background without
recognition or delivery being repeated.
Clients recover interrupted requests by the original message UUID without blindly
uploading or sending again. Hiding the composer stops unsubmitted capture.
See [persistent voice messages](voice-messages.md) for storage and delivery details.
Voice settings also show the selected computer's
90-day counters. Desktop Settings > Private analytics and Android Settings > Computer analytics
show bounded per-computer and fleet summaries with explicit coverage. Agent collection
starts disabled; local desktop controls distinguish enablement, retention and purge.
See [analytics](analytics.md) for the private storage and read contracts.

If microphone access is denied, Voice shows a permission explanation with
**Open microphone settings** on macOS and Windows. On macOS this opens
**System Settings → Privacy & Security → Microphone**. Allow Zerus, return to
the conversation and press **Voice** again; opening settings does not start a
recording. If settings cannot open, the dialog keeps the manual navigation path.

Session rows show provider, machine, current action, model, effort and Git context.
Double-click a session row to toggle between the full card and a compact row
showing only its name, status and agent type. Right-click **Compact session row**
is also available. Compact rows keep their expand icon; it or the Right key
restores the full row. The row remains selectable and keeps its normal context actions. This device
remembers the preference for that conversation across restarts. **Expand all
collapsed** in the Sessions menu restores every compact row, including sessions
hidden by the current filters. Row density never compacts the agent's context
or changes its runtime state. Settings > Sessions > **Start new child sessions
as compact rows** (on by default) makes the row of a session that another
session launches or adopts start compact. "New" means a parent link recorded
after this desktop last looked: the device remembers, for each computer whose
clock stamped the links, the latest link time it handled and the children
handled in that second (`sessions/childRowsSeen`). A computer seen for the first
time only records its current children, so existing rows never change, and a
row the person expands afterwards stays expanded across restarts. Turned off,
new child rows keep the full card. A muted session keeps its mute icon in either size, right after its name; a
supervised child shows its arrow before the name whether muted or not
(owner, 2026-11-08).
The folder/branch line also shows native outline Git badges: a diff with the
number of changed/new files, upload/download with outgoing/incoming commit counts,
and a green cloud check for a clean checkout matching its verified upstream.
Changes include staged, unstaged and untracked paths, counting each path once.
Merge conflicts use a red diff badge. Local changes and outgoing commits can
appear together; counts above 99 show `99+`, with exact values on hover.
Hover over the badges for the checkout, branch, upstream, counts and remote check
time. These describe the current checkout shared by sessions in the same folder,
not ownership of an individual feature or an archived conversation.

Git reads run separately from agent/fleet polling, with at most two concurrent
requests. Local evidence refreshes about every 15 seconds and expires after 30;
read-only remote verification is cached for at most 60 seconds. A changed tracking
OID or remote endpoint invalidates that verification. Nothing fetches, commits or
pushes automatically. A muted cloud question mark means verification is unavailable,
expired, detached or lacks a remote upstream. Outgoing/incoming counts become muted
when only the local tracking ref is known; a changed remote asks you to fetch.
Ordinary non-Git folders show no badge. Offline and changed checkout evidence
cannot show a green check.

**Ready** means a reply ended, not that a long-running task is complete. An idle
agent with confirmed live shell commands shows **Background** in muted blue, with
the command preview and elapsed time. Open **Processes** to follow output or inspect
the running commands. The session remains in **Active** and still accepts new
messages. Green **Working** indicates native agent work. Input/approval requests
take precedence; ordinary agent work takes precedence over background work,
followed by Scheduled and Ready. Offline and
historical/unconfirmed process records never establish background work. The CLI's
additive `process_work` summary uses current owned OS trees (one process-table
sample per fleet listing) or the native DeepSeek job registry; it does not scan
native transcripts on each fleet poll.

Subagent rows and the Inspector use **Recorded** for offline, archived, paused,
stopped or unverifiable parent sessions. Recorded children contribute no current
work to the displayed counts; their saved states are retained. A current child's
explicit input or approval request still takes precedence over Working.

Working and compacting timers use native start evidence and update locally without extra
host requests. An idle agent with pending jobs it scheduled for itself (Kimi
`CronCreate`) is **Scheduled**: its badge and detail show the next run time
instead of a timer, or "due" once that time has passed. A question, approval or
error still takes precedence, and work shows as Working. Theme, density, animation and machine/project label appearance are
local preferences. **Settings → Appearance → Content scale** adjusts Activity,
questions, the message field and Terminal from 75% to 200% in 5% steps, with 100%
as the default; the session list, the side panel, the session header and its tabs
keep their size.

Drag the small handle above the message toolbar upward to enlarge the message
field, or downward to shrink it. Double-click the handle to restore the default
height. The height is a local preference shared by sessions and retained across
restarts; smaller windows temporarily limit it so the conversation stays visible.
With the handle focused, Up/Down resize it and Home restores the default.

Settings and dialog selection fields use the same themed dropdown. The list
uses Zerus menu colors in light and dark skins, marks the current value and
scrolls within the window. Arrows skip disabled choices; Enter or Space opens
the list and confirms a choice, Escape closes it without changing the value,
and typing finds choices by prefix. Closed arrows change the value directly,
as with the former native selection fields. Tab confirms the highlighted choice
and continues to the next control. The popup returns focus to its trigger on
selection or Escape, including inside modal dialogs.

**Settings → Appearance → Activity width** keeps Activity readable in a wide or
fullscreen window: the transcript, its queued input, questions and the message
field share one centered column of at most 900 px by default (480–2400 px). The
width is measured at 100% and grows with content scale, so lines keep their
length. The scroll bar stays at the pane edge and the side margins scroll the
transcript. Drag either column edge in Activity to resize the column
symmetrically, or double-click an edge to restore 900 px. **Full-width Activity**,
in Settings and in Activity's context menu, fills the whole pane; **Reset Activity
width** in the same menu restores the default column. Narrower panes use their
full width, and Terminal, Processes and Native UI are not affected. A reset icon
appears beside an Appearance slider's value once it differs from the default and
restores that default.
**Settings → Appearance → Usage limit indicator** sets how the account usage in the
status bar and on Overview account cards shows each limit window. The default, **Usage and elapsed time bars**, draws usage above a gray
bar for the elapsed part of the window; a longer usage bar means a faster than
even pace. **Pace marker** shows elapsed time as a mark on the usage bar, and
usage beyond the mark takes the warning color. **Pace marker and reset
countdown** adds an hourglass with the time until reset, such as `26h` or `6d`;
compact Overview cards omit the countdown. Colors follow the average pace since
the window started: green when the limit lasts until reset, orange when it would
run out in the second half of the remaining time or 90% is used, and red when it
would run out sooner or is exhausted. The first 8% of a window, and windows
without a reported length or reset time, keep the fixed 70%/90% thresholds. The
tooltip adds the time until reset and the pace forecast.
**Settings → Appearance → Keep Zerus above other windows** keeps the Zerus window
over other applications on X11, macOS and KDE Plasma 6 on Wayland. On KDE Wayland,
Zerus uses temporary KWin scripts scoped to its own window and confirms the
compositor's state; it installs no window rules. The pin above Settings on the
left rail switches the same option, and both controls follow changes made in
KDE's window menu. The unpinned icon is a muted tilted outline; the pinned icon
is upright, filled and gold, with a small line beneath it. The icon follows the
confirmed window state and retains its color across theme changes. Hover only
highlights the button background; its tooltip names the next action.
KWin rules can override the preference; Zerus explains a
refused change. Other Wayland desktops require their own integration and show
the option disabled with a window-menu/rules workaround. Files and terminals
opened from Zerus may appear behind it. Missing Git metadata does not turn
a folder into a repository.

The project button beside the machine chooser focuses Sessions on one or more
logical projects. Check projects to apply changes immediately; selected projects
appear as removable chips. **All projects** clears this restriction. Right-click
a project band for **Filter this project**, which replaces the selection with
that project alone. Clear it using its chip or **All projects** in the chooser.
Multiple projects selected in the chooser form a union,
narrowed by the machine, state, folder and search filters. Selection stays local
to the open workspace while navigating sections; an explicit session link or
Overview scope opens its requested context. Renaming a project retains selection,
and new projects do not join an explicit selection automatically.

The project chooser shows parent-session counts for the current state, machine
and folder scope before project selection or text search, including zero counts.
Its open checkboxes and selection stay in place during polling. A removed selected
project remains as **Unavailable project** until removed from the filter.

The account button beside the project chooser selects one or more specific
accounts, rather than agent types. Its names and verified identity groups follow
Accounts: one verified account on several machines is one choice, and the machine
filter remains independent. Different accounts with the same label remain separate. Choices retain
the exact machine-local profile bindings selected at that moment. A later
catalog refresh or a newly discovered binding never expands that selection
silently. Selected accounts appear as removable chips; **All accounts** clears
the account restriction. Account selection intersects the project, machine,
state, folder and search scopes, including message search and loaded archives.
Chooser counts use the other scopes before account selection or text search.
An unavailable selected account stays selected until explicitly removed.
The chooser uses the same published, current profile catalog as Accounts;
removed profiles and historical session-only bindings do not create extra choices.
Sessions without a recorded account remain distinguishable from known accounts
and shell sessions; they never borrow the current default account.

**Details** shows the session's bound account, its profile ID and recorded agent
home when available. Account catalogs and identities are warmed at startup and
shared with Accounts. Opening the chooser refreshes the catalogs, reusing cached
identity checks. Newly loaded choices appear without reopening an empty chooser.
These reads do not sign in or change any running session.

Changing the state, machine, project or account filter only changes the session list.
The open conversation and its draft stay in place even when no row matches;
its row is selected again when it returns to the list. Click another session
to open it.

When the open session is terminated or removed, the conversation pane returns
to **Select a session**. Polling, new sessions and filter changes keep this
selection empty until you choose a session. Unsent drafts remain in **Saved drafts**.

A desktop draft belongs to its session, not to the session's name. Since
zerus-ucck P2c, a live or saved session that lists a durable session ID keeps
its message, subagent, question-answer and voice drafts under that ID, so a
draft stays with its session across a rename. A draft saved under the name
before moves to the ID once, when its session opens, and never over a draft
already there. Archived sessions and sessions without an ID keep their drafts
under their names. **Saved drafts** shows such a draft by the name its session
had when the draft was saved, and **Go to session** finds the session by its ID.
After a downgrade to a desktop build that reads only names, that build opens
the session with an empty message editor. The draft stays under the session ID,
because the older build writes only the names it uses itself and never deletes
it. That build lists it in **Saved drafts** as `id:<session ID>`, without a
draft mark on the card. A draft typed in the older build stays under the name.
Back on this build, the composer shows the draft kept under the session ID, and
the one written under the name stays in **Saved drafts**.

Read marks waiting for a host follow the same rule. A mark is queued under the
session ID the listing reports for its run, so two sessions that share a name
keep separate marks; it is still delivered with `hgs read NAME`, guarded by run
and conversation. A **Needs attention** or review-later mark of a session
without a conversation is kept under its session ID, and one kept under the
name moves to it.

The **Active** filter combines sessions currently working or Scheduled, those in
**Needs attention**, and sessions with unsent text or attachment drafts, counting
each session once. Desktop and Android use the same filter IDs and membership
rules; drafts remain local to each device. Drafts for visible subagents also keep
their parent session active. Drafts and explicit review-later reminders remain
active offline; stale work alone does not. Archives stay in Archive. Active also
applies to desktop content search, and the Dashboard's Active counter opens this
same filter. Creating, clearing or sending a draft updates the filter and count.
Cards with an unsent draft have a blue edge in the Draft color, including the
open conversation, offline sessions and Saved drafts inspection. Collapsed tiles use
the same color around their frame. Attention and errors retain their warning
colors; working indicators and unread identity remain unchanged.

The **+** in the session list header opens **New session**, like the one in the
side rail. The button beside **⋯** collapses the list into a strip. Every project
band and session row keeps its height, so each session
becomes a square tile: status and provider icons, the session name on up to two
lines and a short state such as the working time. Instead of the attention
edge, a tile's frame takes the color of its state (working, needing an answer,
error, paused, draft or a new reply) with a faint glow inward. Each band shows
the project name and its most urgent counter. Above the tiles and as wide as
them, the strip keeps a search button and two filters: the active one and the
most urgent other one (sessions needing an answer, otherwise active sessions,
otherwise all). Rows open on click and name their session in a tooltip;
tiles do not open a session's subagents. Expanding grows the same rows back
into cards. The strip's search button opens the full list over the conversation
without resizing it; Escape or a click outside returns to the strip, and the pin
docks the list again. While the pointer rests on the
strip, the full list opens over the conversation; turn this off with **Settings →
Sessions → Expand the collapsed session list on hover**. The collapsed state and
the docked width are local preferences.

Search names, IDs, folders, branches and models immediately. From two characters,
content search also reads public Codex, Claude and Kimi messages and saved tool
events on each host, and DeepSeek messages through a running native host
([DeepSeek](#deepseek)). Project, machine and state filters narrow results; the all-sessions
view includes archives. Results open the recorded message; **Back to latest
activity** returns to the live timeline. **Limited history searched**, **Some
history unavailable** and excerpt labels describe incomplete coverage. Internal
reasoning and system instructions are excluded.

**New reply** stays unread until its latest result is visible in an active window
or an attached Terminal at the bottom. Background windows and old search results
do not clear it. Read state is kept on the session's host: a reply or request
read on any desktop or phone is read everywhere after the next poll and its
notification is withdrawn; it survives rename and resume. Hosts with an older
`hgs` keep read state on each device. Viewing never answers an approval. **Mark
as read** clears the current reminder without answering the agent; **Mark needs
attention** creates a local review reminder.
Opening the conversation does not clear an explicit review reminder.
**Mark all as read** (right-click the Sessions icon in the rail, the icon beside
Saved drafts, or the project and machine action menus) marks current replies on
every machine as read, including hidden sessions. Sessions marked as needing
attention keep their reply unread; pending approvals, questions and errors stay
highlighted.

Ctrl/Command-click selects multiple sessions while preserving the open conversation
and draft. Shift-click selects the visible range from the last selection anchor;
Ctrl/Command+Shift-click adds that range to the selection. Shift with Up/Down or
Home/End selects ranges by keyboard, using the current filtered row order and
skipping hidden rows, project headers, subagents and pending launches. Anchors
follow session identities across refreshes; a hidden or removed anchor falls back
to the visible open row, or to the clicked row when none is visible.
Batch actions include local **Compact session rows** and **Expand session rows**
icons, read/review later, pause, resume, archive, project move, termination and
forgetting. Both density actions are available for a mixed selection and apply
only to rows needing that change, including offline rows. Applicability is shown
per selection. Escape or the selection close button
clears selection. Context menus and shortcuts preserve the target identity.
**Clear archive…** requires confirmation, preserves offline records and excludes
new records created after the confirmation snapshot; native history remains.

Enter opens the selected live session and Escape closes the current transient UI;
it never closes the workspace. An archive needs explicit **Restore**; Enter does
not start it. The other keyboard commands are listed below.

<a id="keyboard"></a>

### Keyboard

**Mod** is Ctrl on Linux and Windows and Command (⌘) on macOS. **Control** is the
physical Control key everywhere; macOS keeps Command-Tab and Command-` for its
application and window switchers.

| Command | Linux / Windows | macOS |
|---|---|---|
| Overview, Sessions, Projects, Machines, Accounts | Ctrl+1 … Ctrl+5 | ⌘1 … ⌘5 |
| Settings | Ctrl+, | ⌘, |
| Go to… (sessions, projects, accounts, machines, commands) | Ctrl+K | ⌘K |
| Keyboard shortcuts | F1, Ctrl+/ | F1, ⌘/ |
| Next / previous session | Alt+↓ / Alt+↑, Ctrl+PgDn / Ctrl+PgUp | ⌥↓ / ⌥↑, ⌘⌥↓ / ⌘⌥↑ |
| Next / previous session that needs attention | Alt+Shift+↓ / ↑ | ⌥⇧↓ / ↑ |
| New session | Ctrl+N | ⌘N |
| Rename session | F2 | F2 |
| Search sessions | Ctrl+F | ⌘F |
| Refresh | Ctrl+R, F5 | ⌘R, F5 |
| Next / previous tab (Activity, Terminal, Native UI, Processes, Task View) | Ctrl+Tab / Ctrl+Shift+Tab | ⌃Tab / ⌃⇧Tab |
| Switch between Activity and Terminal | Ctrl+` | ⌃` |
| Show or hide the side panel | Ctrl+I | ⌘I |
| Next / previous area (list, conversation, message field, side panel) | F6 / Shift+F6 | F6 / ⇧F6 |
| Send message (also Enter) | Ctrl+Enter | ⌘↩ |
| Send queued messages now | Ctrl+Shift+Enter | ⌘⇧↩ |
| Send and push now | Ctrl+X, Ctrl+S | ⌃X, ⌃S |
| Increase, decrease, reset content scale | Ctrl+= / Ctrl+- / Ctrl+0 | ⌘= / ⌘- / ⌘0 |

Session navigation moves through the visible list, or the search results while
they are shown, and skips project headers and subagents; it does not wrap.
Attention navigation moves among the sessions of the same list that match the
**Needs attention** filter, wraps around and opens a collapsed project. Tab
cycling skips hidden tabs. From Terminal, Ctrl+` returns to Activity with the
cursor in the message field.

**Send queued messages now** runs the agent's own send-now action for the queue
shown in Activity, exactly like its **Send now** button; it never sends the text
being typed. If the earlier queue is no longer visible, nothing is sent: Activity
refreshes and a transient error asks you to check Terminal or the current queue
before retrying. **Send and push now** sends the typed message and, once the agent
shows it in its queue, sends that queue now. With an empty message field it is
the same as Send queued messages now. If the message does not appear in the
queue within 20 seconds, for example because the agent took it directly or a
dialog such as the cold cache warning is still open, or the session's run or
conversation changes, the push is dropped with a notice; the message is never
resent. Selecting another session drops the push at once, and a new push
replaces one that is still waiting. A message with attachments but no text is
sent without a push, because the queue cannot be matched without text. Without
a selection, Ctrl+X starts this sequence; with a selection it still cuts. As in
every Qt key sequence, a key other than Ctrl+S after Ctrl+X cancels the
sequence and is not typed.

**Terminal keeps the agent's keys.** While Terminal has focus on Linux and
Windows, a key reaches Zerus only when a command uses it and it is Ctrl or
Ctrl+Shift with a digit, Tab, PgUp, PgDn, ` or comma, or Ctrl+Shift+Enter.
Ctrl+letters (Ctrl+C, Ctrl+R, the agent's own Ctrl+X Ctrl+S), Alt chords,
Escape, Enter and function keys go to the agent. On macOS every Command chord
except ⌘C and ⌘V reaches Zerus, and of the Control chords only ⌃Tab, ⌃⇧Tab, ⌃`
and ⌃⇧`.

**Text fields keep their editing keys.** In the message field and search, a
command's key wins over the field except for copy, cut, paste, undo, redo,
select all, word and line movement and deletion (Ctrl, Command or Alt with
arrows, Home, End, Backspace or Delete), and keys without Ctrl, Command or Alt.
Function keys are not editing keys: F1, F2, F5 and F6 reach Zerus without a
modifier. So Alt+↑/↓ switches sessions even while typing, and on Linux Ctrl+K
opens Go to instead of deleting to the end of the line. A focused drop-down
list keeps Alt+↑/↓ and F4 to open it, and the project list on the Projects page
keeps Alt+↑/↓ to reorder projects.

**Go to** (Ctrl+K) searches sessions (not archived), projects, accounts,
machines, sections and commands by name; start the query with `>` to list only
sections and commands. With an empty query, sessions opened recently come
first, then the others in the order of the session list. Enter opens the
result; Escape or the Go to chord again closes the popup. A session opens
under the current filter when it is visible there; otherwise the filter
changes to All sessions. A project selects its first session, or opens
Projects with that project selected when it has none.

**Settings → Keyboard** changes any command's chords (up to two per command,
each a key or a two-key sequence), marks the chords that also work in
Terminal, and resets one command or all of them. Keys used for typing,
navigation, text editing, context menus and Terminal copy/paste cannot be
assigned. A chord already used by another command can be reassigned after
confirmation. Changes apply at once and are stored on this device.
**Keyboard shortcuts** (F1) lists the current chords and these fixed keys:
Enter and Shift+Enter in the message field, Escape, Tab and Shift+Tab, Tab to take
a suggestion, list navigation keys, Menu and Shift+F10, Ctrl/Command-click,
Space and Escape during voice capture, Ctrl+Shift+C/V (⌘C/⌘V on macOS) and
Shift+PgUp/PgDn in Terminal, and Alt+↑/↓ in the project list on the Projects page.

<a id="activity"></a>

## Activity, messages and native controls

Activity shows recorded public messages and tool events rather than a full copy
of native history. User messages preserve literal text, Markdown markers and
line breaks; agent replies render Markdown. The copy button at the right of a
reply's header puts its Markdown source on the clipboard; a text selection still
copies the visible text. Claude's recorded thinking appears in
full as a quieter **Thinking** card: the same Markdown at the same size, without
the card fill and with dimmer text. Adjacent tool events collapse into groups.
Reading older history preserves scroll and selection; **Jump to latest** returns
to live events without resizing the timeline. Its double-down-arrow icon sits beside
Voice in the composer toolbar, outside the messages; the tooltip includes the
number of new events. Subagents use the same icon in their own toolbar.
Pressing Enter on a session or subagent row opens Activity and performs the same
jump. Double-clicking a subagent row also jumps to latest; double-clicking a
session row toggles its compact presentation.
Connect commands remain available through **Copy connect command** in the session
menu. Claude hands finished
background work and other sessions' messages back as a new turn. Activity never
shows them as your message: a background task becomes a violet **Background
task** notice, a subagent's final report a **Subagent report** with one summary
line and **Show report** for the full Markdown, and a message from another
Claude session a **Message from** notice with its full text. Search and the
session's last request ignore all three.

The owner-only `events.sqlite3` journal sits with session records on the agent host
and retains about 50,000 events per machine. Excerpts can contain project data.
Pause/resume continues the same conversation journal. Selected Activity normally
polls every 2.5 seconds while visible; host lists poll every five seconds, and the
tray keeps its background polling when the workspace is closed.

Kimi replies and intermediate public messages come from the exact conversation's
`agents/main/wire.jsonl` when Stop lacks text. Transcript reads are bounded to an
8 MiB tail, 50 replies and 32,000 characters per reply. Late copies do not duplicate
cards. Nested `codex exec` reviews register as separate children, including
`--ephemeral` runs without a transcript. Detection uses the rollout's `source:
exec` or the native caller's ancestry to the verified live parent process.
Child events preserve the parent's conversation, model and history; ephemeral
children show their recorded request, tools and result. Unknown inherited
SessionStart events cannot replace an established parent. `/new` and `/clear`
still change the main native conversation.
Claude's background service runs jobs in processes that inherited the run token
of whichever session started the service. A hook names its Claude process and
conversation (`CLAUDE_PID`, `CLAUDE_CODE_SESSION_ID`); an event from a Claude
process that is neither the session's agent nor runs under it never changes that
session by its token. A background job belongs to the live session whose agent
window moved it there, as Claude's session registry records (`jobId`,
`parkedJobId` in `sessions/<pid>.json`). The session follows the job's
conversation without a clear when its transcript ends with Claude's
`continued-in` mark for it; a new conversation in the job clears it as `/clear`
does. Events of jobs that no live session holds are not recorded. A command
such a job runs, such as `hgs task` or `hgs self`, acts as that session while
the job is on its conversation.

### Sending and attachments

Enter sends, Shift-Enter inserts a line and Escape releases focus while keeping
the draft, unless a turn is working. Paste images, drag local files or use the
attachment button. Attachments collect in one chip in the fixed-height toolbar
above the message field, next to session notices (cache, account limit,
recovery) and the context counter; its popover previews, opens and removes them. Labels such as `[Image #1]` and `[File #2]` preserve
attachment placement within the text; removing one does not renumber the rest. Text, cursor/selection and attachment
bytes are saved locally as they change, separately for each machine, session and
subagent. They survive GUI crashes, restarts, unavailable machines and ended
sessions, even if the original attachment file is gone. Storage is a private
directory next to the desktop's Qt settings file (`<settings-file>.drafts`),
outside P2P configuration sync.

Pasting an image in Terminal uses the same draft attachment path and opens
Activity so you can review it and send your message. It never sends automatically
or writes image data to the terminal. If the message field is not ready to accept
attachments, Terminal shows an Open Activity action and asks you to paste again
when it is ready. Text-only terminal pastes keep their normal behavior.

In Terminal, Ctrl+Shift+C (⌘C on macOS) copies the selection exactly as shown,
and Ctrl+Shift+V or Shift+Insert (⌘V) pastes; Ctrl+C and Ctrl+V stay the agent's
keys. Right-click opens **Copy selection** and **Paste**. While the agent reads
the mouse, a right-click goes to the agent; hold Shift for the menu.

Use **Reference an existing file** (the link button beside Attach, or its Files
menu in a narrow column) when the file
already exists on the agent's computer, including very large files. Locally,
choose a file or enter its absolute path (or a `~/` path); Zerus checks that it
names an existing file. If that check cannot answer within a few seconds, for
example on an unresponsive network mount, the dialog says so and offers Retry.
For a remote session, enter an absolute path (including Windows drive or UNC
paths on a Windows computer) or `~/` path on the named remote computer. When
that computer's hgs has `path-info`, Insert asks it whether a file is there
(one metadata read, no content) and refuses a folder or a missing path; if it
does not answer in time, the dialog offers Retry, and closing the dialog ends
the call. On a computer with an older hgs, only the path's form is checked, as
the dialog then says. A file on your desktop is not uploaded or made accessible
to that computer. Zerus inserts an editable, quoted path into
the draft and saves it as ordinary message text. Review it and press Send when
ready. This action does not read, copy or Base64-encode the file and has no file
size limit. It refers to the existing file, whose contents may change; the agent
decides how to use it. Ordinary attachment uploads retain their existing limits.

Session refresh, connection and recovery errors appear on one line beside the
attachment button, with the complete diagnostic in its tooltip and an explicit
session refresh action. They do not move the Activity or Terminal tabs. A blocking
session error makes the message field read-only and muted while preserving its
draft, focus and selection; informational notices leave editing available. When
the composer is hidden, the same diagnostic uses the existing workspace footer.
Other operation notices retain their priority there.

An uncertain draft holding a deferred-input identity shows a quiet **This text
may already have been delivered** hint when its exact text appears in the loaded
history of that run and conversation. The hint is derived from the existing
bounded history window and disappears when it no longer matches, including on
edit or Allow retry. It changes no draft, checkpoint or uncertainty state: text
equality is never request-ID receipt evidence and never clears or retries input.

Reads from the selected remote machine show a yellow loading circle beside the
session name only after two seconds. It remains visible while an actual read is
pending and clears when that read completes or fails. Switching sessions shows
only reads belonging to the newly selected session or machine. Local reads do not
show this indicator, and its appearance does not change the header layout.

Ordinary text, cursor and question-form edits save through one ordered background
writer. Waiting edits coalesce to the latest snapshot per draft, so typing does
not wait for disk commits. The editor shows **Saving draft** until its latest
checkpoint completes; a crash before that checkpoint can lose pending edits.
Graceful close drains outstanding writes. Sending, renaming and delivery-state
changes use durable barriers, and an older queued save cannot replace a later
checkpoint or resurrect a confirmed sent message. Attachment snapshots remain
immutable. Save failures remain visible and block sending until persistence
succeeds.

**Settings → Automatic approvals** provides a separate dangerous opt-in, off by
default. Enabling it explicitly allows reachable sessions to run tools without
individual review, including destructive commands and paid usage. It works
while this Zerus process is running, including with its window hidden, and does
not change another desktop's preference or the agents' permanent permissions.
Resuming after an expired cache may process the full conversation again. While
enabled, the normal peer polling interval is capped at five seconds so remote
permission prompts need not wait for the ordinary minute-long refresh.
Only verified, answerable one-time Claude, native OpenCode and native DeepSeek
tool permissions qualify. Ordinary questions, startup trust, incomplete panels and
persistent permission changes remain manual. Existing native identity and
delivery guards still apply.

Below the switch, **Categories** choose which of those requests are approved
automatically; a request in a category that is off waits for the person. The
categories apply only while the switch is on and are stored per computer as
`automaticApprovals/category/<id>`, beside the switch. Every category except
Paid usage is on by default, and an enabled switch from before categories
keeps that default. A request's category comes only from the tool name in its
verified native question: Claude's permission hook `tool_name` (the card's
header), OpenCode 2's permission `action` (the card's `permission_action`) and
the DeepSeek tool in Zerus's own `Allow <tool>?` approval. Matching is exact;
commands, paths and other text are never parsed to choose a category.

| Category (ID) | Claude | OpenCode 2 | DeepSeek |
|---|---|---|---|
| Read files and search (`read`) | Read, Glob, Grep, LS, LSP, NotebookRead | read, glob, grep, list, lsp | read, read_image, glob, grep, terminal_list, terminal_read, job_list, job_output |
| Edit files (`edit`) | Edit, Write, MultiEdit, NotebookEdit, EnterWorktree | edit | edit, write, apply_patch, str_replace_editor |
| Shell commands (`shell`) | Bash, PowerShell, Monitor | shell, execute | bash, pwsh, terminal_open, terminal_send, terminal_signal, terminal_close, job_kill |
| Web pages (`web`) | WebFetch | webfetch | web_fetch |
| MCP tools (`mcp`) | `mcp__<server>__<tool>`, ListMcpResourcesTool, ReadMcpResourceTool | none | `mcp__<server>__<tool>` |
| Subagents and skills (`agents`) | Agent, Task, Skill, Workflow | subagent, skill | skill, subagent, workflow, spawn_teammate, send_message, interrupt_agent, wait_agent |
| Other tools (`other`) | any other tool | any other action | any other tool |
| Paid usage (`paid`) | WebSearch | websearch | web_search |

Shell commands are one category: Zerus does not tell safe commands from
destructive ones. OpenCode names MCP tools `<server>_<tool>` without a prefix,
so any underscored OpenCode action (such as `external_directory`) is Other, as
is a card from an older CLI without `permission_action`. Other tools also
covers a tool name that cannot be read; it may include paid tools. Paid usage
always wins: a paid tool, OpenCode's image generation
(`experimental_image_generate`, Other by name) and any request while the
prompt cache is cold, which the agent reports or whose estimated lifetime has
ended, also need Paid usage, whatever else they match. Answering after the
cache expired processes the whole conversation again; an unknown cache state
is not treated as cold. Usage-limit, extra-usage and credit prompts are not
one-time tool permissions and are never answered automatically. The worker
checks the categories when it selects a request, again just before the
journal reservation, which records them, and again before delivery. Each check
uses the current time with the cache state of the selecting inspection, so a
warm cache that expires meanwhile needs Paid usage too. A category turned off
or a cache expired before delivery finishes the reserved attempt as cancelled
without sending anything. With `ZERUS_CLIENT_DESKTOP_SERVICE=1` the background service owns the
categories with the switch, in its durable policy (all except Paid usage for
an existing policy). The page then changes them only through the service's
authenticated route: a category change names the complete set and the
switch's committed value, so it never turns approvals on or off. Like turning
the switch off, a change that approves nothing more (removing a category, or
any change while approvals are off) is accepted while the background workers
are unavailable; adding a category while approvals are on needs them ready.
Each change is an exact, idempotent receipt and cancels approvals in flight.

Zerus Next uses its live event-stream catalog for the same bounded discovery;
it does not add a peer poller. Its opt-in is device-local and is never seeded
from Qt or imported by migration. Confirm it separately in Next. Cached,
offline or unidentified computers cannot admit an approval; each candidate is
freshly inspected, and its exact identities are checked again before the
existing native Answer lane runs under the journal's original request UUID.

The same Settings page contains this computer's private approval journal, naming
the computer, session, time, question, approval categories, selected answer and
confirmed outcome.
Each attempt is durably reserved before native input. Failed, cancelled and
unconfirmed attempts remain visible; an unconfirmed delivery is never retried
automatically, including after a restart. If the journal cannot be saved,
automatic approvals pause without sending new answers. Turning the option off
stops new approvals; requests already sent may still finish.

The journal stores full questions only in the private local
`approval-audit/journal.sqlite3` below the existing `HGS_STATE_DIR`. It is not
part of P2P settings sync. Pages contain at most 50 summaries; exact computer and
session filters use indexes, and Older/Newer navigation uses stable record-ID
cursors rather than offsets or complete-table counts. Full question text loads
only for the selected entry. Background discovery scans at most 512 sessions per
tick, admits at most two concurrent targets and bounds its cooldown cache to
4096 entries, independent of total session or journal counts.

`hgs approval-log begin|finish|list|detail` is the local desktop journal API
(JSON stdin). `begin` selects only a supported one-time grant and returns a
durable request UUID; an existing reservation never permits another delivery.
`list` supports exact `host`/`session` filters and `before` or `after` record IDs.
`detail` reads one entry. The command itself never sends native input.

**Saved drafts** below the session list lets you preview, copy or restore drafts
from ended or missing sessions into the current session. It lists the most
recently edited drafts first; each row names the draft's session and computer
(and whether it is a subagent message, question answer or voice message), with
its time and first line. **Go to session** opens the session that owns the
selected draft, or its subagent, while that session is in the session list.
Double-clicking a draft opens the same owning session; unavailable sessions
remain in the draft browser for preview, copying or explicit restoration.
Rows reuse the Sessions cards, including provider and computer badges, with
the last-edited date beside the model footer. Long fields stay within the card;
rows have no hover tooltip because the selected draft's full text appears in
the preview below. Both desktop themes use the workspace colors. Metadata uses
separate fields or slashes, without dot separators.
Copying keeps the dialog open; restoring never sends anything and replacing an
existing draft requires confirmation. A source draft remains available after
copying it to another session.

Independent sessions can explicitly declare **Waiting for acceptance** and
**Waiting for result** through `hgs wait`. The idle card names the producer and
computer and shows elapsed time; the session stays in Active. Native work and
attention take precedence, while reply-only completion badges wait. Settlement requires the scoped reply ticket
and, for a result, a confirmed delivery receipt in the exact waiting conversation.
The durable journal has bounded previews and stable pages. See
[Agent coordination](agent-coordination.md).
Confirmed sends clear the outgoing draft; failures retain it. After an interrupted
or uncertain send, check Activity or Terminal before explicitly allowing another
send. Saving failures are shown in the editor and retried locally; sending starts
only after its recovery snapshot has been saved.

Delivery verifies the exact run, process, conversation and native input state.
A busy Claude or Codex session can accept a message through its native queue
when its empty composer is recognized. An unknown busy layout or a draft refuses
with `agent busy` before submission; wait or inspect Terminal rather than typing
through that refusal. Deliberate send refusals keep only the latest private
`input_diagnostics/codex.json` or `claude.json` under the session state directory.
Each sample is at most 16 KiB: cursor coordinates, bounded cursor-row text and
structural metadata for at most eight nearby rows. Text from other rows and the
attempted message is omitted. Listing sessions does not create samples.
For Claude and Kimi messages ending in a literal backslash, the paste includes a
final newline so the single Enter submits the unchanged message instead of
activating the agent's backslash-Enter line continuation. This is verified
against native history; no second Enter is sent as an automatic retry.
Codex messages ending in an `@mention` likewise include a final pasted newline
to move the cursor outside file/mention completion before the single Enter.
Codex trims that newline on submission; the message and receipt retain the
literal mention, including a recipient that matches an existing filename.
Confirmed receipts are idempotent by request ID. An uncertain result is never
retried automatically; inspect the native UI before submitting again. A session
change or stale request cannot send into another conversation. Busy-session queue
controls follow the native agent's capabilities; unsupported operations remain
in Terminal or Native UI. Codex ordinary deferred follow-ups remain read-only
unless its native UI offers the supported promotion operation.

**Stop** interrupts the current turn and cancels recovery without terminating the
session; Escape anywhere in the workspace except Terminal does the same while
the selected session's turn is working, after first clearing a search being
typed. While recording voice, Escape stops the recording instead and preserves
its preview. Once the turn is interrupted, its prompt returns to an empty message
field for editing; a started draft is kept. Claude, Codex and Kimi use verified native
Escape; Claude sends no event for it, so HGS records the interruption once
Claude shows it. Stopped before its first reply, Claude rewinds the turn and
puts the prompt back into its own input instead; HGS then moves exactly that
text from Terminal to the message field (Ctrl+Y in Terminal brings it back) and
leaves any other input untouched. Supported DeepSeek hosts use native cancel.
Native queue behavior still applies. When Codex lists pending steers ("press esc
to interrupt and send immediately"), Escape submits them at once and any queued
follow-ups after that turn. Ordinary Codex follow-ups queued alone are not sent:
Codex interrupts the turn and returns them to its input in Terminal as one
unsent draft. Activity can send again once that input is empty, so submit, edit
or clear the draft in Terminal first. HGS notes the follow-ups the Codex panel
lists when Stop sends Escape; while Codex reports that turn interrupted and its
input holds exactly them, Activity says the queued follow-ups are back in Codex's
input. An edited draft, any other draft and follow-ups Stop did not see listed
keep the general notice to return Terminal to an empty prompt.
Uncertain interruption is not repeated. Stop notices name the original session
and computer even if you navigate away. If the turn has already ended or is
waiting for input, no interruption is sent and no action is needed; Zerus refreshes
the status and briefly shows that outcome. Other failures retain the diagnostic
and ask you to refresh status or check Terminal before trying again. The status
bar's **Dismiss notification** button clears a notice without changing the session.

When Claude suggests your next message after a turn, Activity shows it as the
empty message field's placeholder, as Claude's terminal does; Tab inserts it.
Sending a message, a new turn or a new conversation removes the suggestion.
Claude writes it with an internal agent, which is neither listed as a subagent
nor shown in Activity. Claude's other internal agents, such as the ones that
summarise a running subagent's progress, report without starting as a subagent
or naming an agent type; they are not listed either and never keep a finished
session Working.

File links retain their visible path and source position. Relative references
resolve against that session's folder. On Windows, references may also name a drive
path (`C:\work\a.cpp:12`, `C:/work/a.cpp` or `file:///C:/work/a.cpp`); a share
(`\\server\share\…`) never opens. The file dialog shows and copies a path on this
Windows computer with backslashes (`C:\work\a.cpp`); remote machines keep their
`/` separators. Local references can open the file or its parent folder; remote
references identify the host and offer **Download…**, **Download and open** and
an SSH terminal. **Download…** asks where to save. **Download and open** saves
automatically in the local Downloads folder under `Zerus Files`, then opens that
copy. A completed, unchanged copy recorded for that exact machine and remote path
is reused without another transfer, including after restarting Zerus. The dialog
then offers **Open local copy** and **Show file** (selected in Finder or the file
manager), even when the remote machine is offline. **Download again** explicitly
fetches an updated copy; opening a saved copy does not check whether the remote
original has changed. Missing or edited cached files are not reused. Files saved
by older versions without a recorded source are not guessed from their names.
Fresh downloads preserve existing names with numbered suffixes such as
`report (1).pdf`; simultaneous requests for the same source in one window share
one transfer.
Downloads use that machine's configured SSH profile and SFTP, stream directly to
disk and publish the destination only after a successful transfer. Errors remain
in the dialog; a saved copy can be opened with **Open local copy**. Offline or
unresolved relative references explain why downloading is unavailable. The
download continues if its dialog is closed, with a completion or error notice;
an explicitly requested open still happens after successful saving. The
120-second transfer deadline leaves an existing destination unchanged on failure.

**Settings → Files** controls the local downloaded-copy cache. **Never clear** is
the default. An optional size limit removes the least recently used completed
copies created by **Download and open**, without touching manual downloads, unrelated files or
copies whose contents were edited. An internal index tracks ownership; files
that have been moved or whose contents changed are no longer cleanup candidates.
Transfers in progress and recently used or completed copies are protected for
one minute so the default application can consume them. Usage may briefly exceed the limit, including a
single oversized file; periodic cleanup runs while Zerus is open and on its next
start. Enabling or reducing a limit requires confirmation. Cleanup and inventory
run outside the UI thread.

`hgs @host download --path REMOTE_PATH --output LOCAL_FILE` provides the same
transfer from the CLI, including empty files and paths starting with `~/`.
Display alone downloads or launches
nothing. Copying the path keeps the dialog open so you can copy first and then
choose an opening action. Desktop dispatch happens after the reference dialog is
destroyed so Wayland activation does not depend on a disappearing window.

Zerus opens files the way a browser treats downloads. Conversation attachments,
draft attachments, downloaded copies, local file references and their folders
open in their default application. Anything that would run a program is selected
in the file manager instead, with a notice that says why. That includes:
- file types that run programs, scripts or installers, such as `.exe`, `.bat`,
  `.ps1`, `.url`, `.app`, `.command`, `.pkg`, `.dmg`, `.desktop`, `.sh`, `.jar`
  and `.py`, plus types Windows itself rates as dangerous;
- macOS bundles;
- on Windows, names without an extension;
- on Linux and macOS, executable files without one, and programs or `#!/…`
  scripts recognized by their content.

A link is judged by its target. On Linux, when no file manager answers, Zerus
opens the folder instead of selecting the file.

Files from other machines carry the system's downloaded-file marker. These are
attachment copies opened from Activity, and every completed download as soon as
it is saved. The marker is Mark of the Web for the Internet zone on Windows and
quarantine on macOS, which keeps an earlier approval. SmartScreen, Office
Protected View and Gatekeeper therefore still apply when such a file is opened
later. Windows skips the marker when policy disables zone information. Linux has
no such marker, and cached attachment copies there are never executable.

### Questions and approvals

Activity exposes supported native questions with their full visible disclosure,
options, queue position and request time. It never submits a highlighted option
without a user action. Answers are scoped to the unchanged run, process,
conversation and request hash; uncertain input is not automatically retried.
Unsent freeform answers, choices and the current question page are also saved
locally. They restore only for the same machine/session, run, conversation and
question hash. Interrupted submissions remain locked until reviewed; confirmed
queued answers keep their submitted state after a GUI restart.

Questions the agent wrote render its Markdown like replies in Activity: the
prompt, details and option descriptions, at the card's text sizes. A link opens
only when clicked: web links in the default browser and file references in the
file dialog. Other link targets, raw HTML and images remain text. Approval and
trust disclosures and panels read from Terminal, such as folder trust and resume
prompts, are shown exactly as reported.

Question cards fit the current question's content, up to 600 px or 60% of the
Activity area's height, whichever is smaller. Longer prompts and options scroll
inside the card; freeform answer fields and actions stay visible. Short questions
remain compact, and resizing preserves answers and space to read Activity.

Explicit **Submit** on a supported Claude question replaces its native Other
draft with the form answer, including clearing it for a preset-only answer.
Polling and inspection never edit native drafts. The exact form payload and
matching native acknowledgement determine delivery; uncertain attempts are
never replayed automatically.

With Claude Code 2.1.296 or later on Linux, macOS and Windows, hgs also
registers its own `PermissionRequest` hook for AskUserQuestion and tool approvals,
`hgs __state answer-channel`, beside its usual hooks. The entry an earlier hgs
registered for AskUserQuestion alone is widened in place.
- It waits next to Claude's question panel and is bound to the one exact
  pending question. **Submit** hands the answer to Claude through this hook
  instead of the terminal, so the question can be answered at any terminal
  size. `hgs answer` receipts then report `transport` `claude_hook`.
- It answers only for the saved conversation, and one hook process owns each
  question.
- If the hook does not take an answer within three seconds, delivery falls back
  to the verified Terminal path (`claude_tui`).
- Once the hook has taken an answer, that answer is never sent another way. If
  Claude does not confirm it, delivery is uncertain, the card stays locked and
  further answers are refused until the question is resolved in Terminal.
- An answer, decline or cancellation in Terminal ends the hook, and so do a run
  change and its one-day deadline.
- Sessions started before the hook was registered and other agents keep
  Terminal delivery. On Windows the hook runs only in Claude Code's exec form;
  settings that still use the shell form, written for an older Claude Code,
  keep Terminal delivery. Tool approvals through this hook are described below.

A disabled **Submit** always shows why, including while a saved answer is still
loading.
Desktop question submissions, including skips and approval choices, require
**Submit with full context** confirmation when the prompt cache is cold or its
estimated lifetime has expired. Resuming with the full conversation can increase
token usage, cost or subscription usage. **Cancel** keeps the form answers.
Compaction or clearing can invalidate the pending question, so this confirmation
offers neither action. A changed question, session or draft cancels consent.
Kimi's native cold-cache chooser already provides the explicit cost and context
choices. Its selected row is submitted directly, without a second full-context
confirmation; the same identity and durable delivery checks still apply.
App-owned desktop and Android dialog action rows place **Cancel**, **Close** or
the equivalent dismissal at the far right, after the available actions. Long
Android rows wrap, keeping dismissal last and visible with larger text. Destructive
and full-context desktop confirmations default to Cancel; Android dialogs focus
their dismissal after the dialog window gains focus. Enter on a valid desktop
editing form may save or create; Escape/Back dismisses without submitting.
Accepted operations that cannot safely be abandoned keep their existing pending
locks; closing a review never repeats an uncertain command. Destructive buttons
name their action and the dialog identifies its original target. System file,
permission and installation prompts follow the operating system's own layout.

Unsupported or truncated native prompts remain in Terminal.
Claude wraps prompts, options, review answers, key hints and Bash commands to
the terminal width and splits words longer than a row. Zerus matches the wrapped
rows against the exact request at any width; only a clipped panel needs a
larger Terminal.

Claude keeps no scrollback for a question or approval panel taller than its
pane, so a long question or command can lose its title and beginning
everywhere. Kimi question dialogs and Kimi 2.1's startup folder trust prompt
can lose their title the same way when their disclosure is long. Kimi 2.2
pages its trust disclosure, or asks for a larger terminal, when its pane is
short; Zerus offers the trust choice only once the whole disclosure shows on
one page, and never chooses for you. A session no client views keeps tmux's
default 80x24 size, and so does a pane of the Windows session host. While a
recorded question or permission request waits and such a pane shows a clipped
panel, Zerus makes that pane taller, doubling its height up to 192 rows without
changing its width, until the agent redraws the complete panel. The exact
request match still decides; the next client that attaches sets the size
again. A pane a client views keeps that client's size, so its clipped panel
stays in Terminal, as it does under a Windows session host older than this
behavior. Only a question listing, such as `hgs inspect`, grows a pane, and
never while an answer to that run drives it: both hold the run's answer lock.

Claude tool approvals keep the session at **Needs approval** and appear in
Activity while Claude shows its chooser. When the complete panel matches the
agent's request, such as the Bash command, file name or fetched host, Activity
lists Claude's options verbatim after a short review delay. Options that reach
beyond this request, such as always allowing, blocking, session-wide grants or
mode switches, are marked and need a second **Confirm** click. Choosing a **No**
option ends Claude's turn as in Terminal; the composer reopens after Claude's
prompt is verified. A clipped or unrecognized panel, a mismatched request and
plan approval stay visible with **Open Terminal**. Claude reports no answer
given in Terminal, so Zerus settles it from the screen alone whenever a status
listing, `hgs inspect` or `hgs read` looks at the session: Claude's
interruption ending the turn records **No** or Esc, and the approved Bash
command shown running with its elapsed time records **Yes**. Until Terminal
shows either, and for an approved tool that is not a Bash command, the session
stays at **Needs approval** until Claude's next event. Only a session waiting
for such a request reads its terminal for this.

While hgs's `PermissionRequest` hook waits beside a main-agent approval, the
card answers through it, so a clipped panel no longer needs a larger Terminal.
- The hook is bound to one exact request: the saved conversation, and the one
  active tool call whose tool and input equal the request when the hook
  starts. Without exactly one such call, the request is answered on screen as
  above; a later identical call is another request.
- A paused session, a provider error, or a terminal that no longer belongs to
  the agent never lets a decision go through the hook.
- Claude sends the hook no signal for an answer in Terminal, so the card and
  the hook act only while Terminal still shows a chooser. A complete panel must
  be this request.
- The card always shows the whole request the hook answers: the Bash command,
  its description and any other field, or the whole input of another tool.
- Claude's own options appear only when a complete panel shows exactly that
  operation. That means the main agent's Bash command, with its description if
  any and no other field, and no other active Bash call. A file name or host
  proves too little. For example, a Read of another file with the same name may
  be queued in front of this one.
- Any other complete panel is answered on the screen as above, acting on what
  it shows. A clipped panel gives **Yes** and **No**. An unreadable terminal
  gives no hook card.
- A visible safety warning of any kind must belong to this request's command,
  through a proven panel or the visible end of the command in a clipped one.
  Otherwise there is no hook card.
- **Yes** allows this one tool call without changing any permission setting.
  **No** ends Claude's turn as in Terminal. **Tell Claude what to do instead**
  denies the call with your message, and Claude continues without it.
- Options that reach beyond this request go only through the verified panel,
  with the second **Confirm**. A **Yes** or **No** that the hook does not take
  within three seconds also goes through a verified panel; feedback it does not
  take is not sent.
- A decision the hook has taken is never sent again. Until Claude confirms it,
  the card stays locked and is answered in Terminal.
- `hgs answer` receipts report `transport` `claude_hook` or `claude_tui`.
- Automatic approvals use the hook only for a card whose complete Bash panel
  shows exactly that operation, with the same one-time choice.
- Subagent and workflow requests keep the screen path. Claude 2.1.296 runs
  subagents in the background and shows their requests outside the main screen.

Claude's Bash safety confirmations, including warnings about possibly empty
paths in `rm` commands, also appear in Activity with the native warning text.
A visible safety confirmation makes the session **Needs approval** even if
its permission hook was not recorded, and ordinary message sending waits for
the decision. Without a matching permission request, or when the full command
stays clipped and no permission hook waits for it, the card offers **Open
Terminal**. With a complete matching request,
Activity can submit the native **Yes** or **No** choice. Claude's unattended
denial countdown stays visible without replacing the request as it ticks.
Desktop keeps the card's scroll, chosen option and review delay through these
countdown updates. A changed command, warning or set of choices requires a new
review.

Bash approvals from Claude's subagents and workflows use the same Activity
choices when one recorded child permission request matches the complete visible
command. The answer is bound to that child request as well as the parent run and
conversation; a repeated command is a new request. Identical pending requests
from multiple children, missing hook evidence and clipped commands remain
read-only. Other child tool approvals still require Terminal. A child's answer
never interrupts the parent or clears a sibling's request; completion follows
that child's own native events. Parent progress alone does not settle child
permissions. Pending evidence is bounded without evicting an ambiguous request;
if its limit is exceeded, approvals remain in Terminal until the next native
session starts. Read-only native choices are visibly disabled, with **Open
Terminal** as their action.

Kimi plan approval (ExitPlanMode) keeps the session at **Needs approval** and
shows the complete plan from Kimi's own request in Activity, rendered as Markdown,
with Kimi's choices: **Approve** (or the two or three approaches the agent
proposed), **Reject** and **Revise** with a one-line feedback field. A plan longer
than 8,000 characters is shortened and names its saved plan file. Zerus answers
only while Terminal shows exactly that chooser, using its number key and, for
Revise, pasting the feedback before Enter; Kimi's recorded decision, including
the exact feedback, confirms delivery. A clipped or different chooser, or one
already in Revise feedback mode, stays read-only with **Open Terminal**.

Kimi CreateGoal approval shows the full objective and completion criterion literally,
with Kimi's own mode choices for Always Ask or Ask When Needed. **Switch to Never
Ask and start** and a switch between Always Ask and Ask When Needed require a
separate confirmation because they change permissions beyond this request.
**Do not start** cancels the tool without creating a goal or changing the mode.
Zerus answers only the exact native goal panel after the same complete wire and
process checks as other Kimi approvals; Kimi's recorded decision and mode label
confirm delivery. A different, clipped or unverifiable panel stays read-only.

Other Kimi tool approvals also appear in Activity, with the complete request from
Kimi's wire. Kimi's hooks cannot approve, so Zerus answers only in Kimi's own
panel, and only for a Bash command: the panel must draw that exact command,
working directory, description and danger warning row by row, wrapped as Kimi
wraps it, and the complete history of every agent of the conversation may hold
no other unresolved Kimi request, however old: nothing proves that Kimi no
longer shows one. A history longer than 8 MiB (4 MiB for another agent), a
record Kimi is still writing, a line that is not a JSON object with one string
type, an interaction record without its ID or of a kind Kimi does not write,
or a repeated request ID keeps the approval read-only. Types and IDs are
compared as decoded JSON. Every key and pasted feedback line first finds the same
unresolved request, with its active tool call, in the same run, process and
pane. The
choices are Kimi's: **Approve once**, **Approve for this session**, **Reject** and
**Reject with feedback**. The session choice asks for confirmation: Kimi then
stops asking about that exact command and also approves every other request
already waiting with the same action, which shows only a command's first 50
characters. Kimi's recorded decision confirms delivery. Other tools, commands
Zerus cannot draw exactly, a narrow or clipped panel, and an unverifiable history
stay read-only with **Open Terminal**. Automatic approvals never answer Kimi.

Claude Code can also ask its own questions without an agent tool call, such as
whether to switch models after a safeguard stop. Activity shows the question and
its options verbatim with **Open Terminal** while Terminal still shows them: no
tool result can acknowledge an answer, so Zerus never chooses one of these
options. When Claude waits for input and no request explains it, Activity shows
the chooser visible at the bottom of Terminal, such as **Session paused**, in
the same read-only way. A new prompt ends any earlier request.

Supported startup prompts include Claude workspace trust and auto-mode consent,
Codex folder access, **Hooks need review** and **Update available**, and Kimi
folder/MCP trust. These can appear before a conversation exists, but only with
verified startup identity.
A recognized panel must show its native title, folder, exact choices and key
hint. Its explanation is shown verbatim and pinned to the answer, so changed
native wording stays answerable: Claude workspace trust preserves its full
permissions warning, Kimi includes the visible MCP commands, and Claude's
auto-mode and resume panels keep their explanation and cost warning. Zerus adds
descriptions only for reviewed text. In a Git subdirectory or worktree, Codex
folder access names the repository root that Codex will trust. Claude wraps a
long workspace path across rows; the joined rows must still equal the launch
folder exactly.
Codex 0.167 can offer **Back to Agent Command Center** instead of **Quit**.
Back cancels opening the folder without saving trust and keeps Codex running;
Quit exits the original Codex process. Both require an explicit user choice.
Trust persistence belongs to the native agent.
For a new Claude run on Linux or macOS, the original `hgs` supervisor confirms
the complete first-run text-style panel automatically when both the native focus
and current-selection mark name **Auto (match terminal)** or **Dark mode**. It
preserves that selection and sends Enter once after checking the stable panel
and the original process. An uncertain result never causes another Enter.
Recognized manual theme choices, including Light mode and Dark mode with ANSI
colors, appear in Activity with **Open Terminal**. Windows and runs whose original
supervisor predates this feature keep the read-only theme card. Login, workspace
trust, auto-mode consent and permission questions still require an explicit choice.
Already-running Claude and Kimi sessions can retain the exact Bitwarden launcher
notices above the complete workspace-trust panel; Claude also accepts its
Keychain notice. These notices do not change the request identity or appear in
its disclosure.
Any other chooser before a conversation exists, such as a renamed option, an
unreviewed update command, a panel for another folder or unrelated output above
it, appears read-only with its visible text and **Open Terminal**. Zerus never
answers it, and messages wait until it is answered in Terminal.

Before the first turn, Claude's auto-mode prompt accepts absolute workspace paths
and native `~/…` abbreviations that match the verified launch directory, also
after the initial conversation is bound. Under a finished or stopped reply, the
bound conversation and that turn identify it instead. The offer must be the
complete panel at the bottom of Terminal: panel text quoted in the conversation is
never answered, and messages wait until the offer is answered.
Claude's **Resume this conversation?** chooser also appears in Activity when a
long conversation has gone cold, including between turns in a resident session.
The card preserves the inactivity, context size and five-hour usage warning.
**Resume** and **Start a new conversation** require an explicit choice; Zerus
never dismisses this warning automatically. If the native unsent draft has focus,
pick a row in Terminal first. Replies verify the exact run, conversation and full
panel before each key, and uncertain delivery is never replayed.
For Codex hooks, **Review hooks** opens the native browser in Terminal;
**Trust all and continue** and **Continue without trusting** are explicit choices.
Review input with an uncertain result is never retried automatically.
Codex's **Update available** picker shows both versions and the exact update
command. **Update now** switches to Terminal while Codex runs that command; Codex
then exits and the session needs an explicit resume. Codex leaves the picker and
input mode at once and prints `Updating Codex via` with the command before running
it. That announcement for the exact command, or Codex exiting, confirms the choice
within 15 seconds, however long the installer takes. Zerus does not wait for the
installer: the `hgs answer` receipt's `codex_update` is `installing` while Codex
runs it, or `exited`. Without either confirmation, delivery stays uncertain and
Enter is never sent again. **Skip** and **Skip until next version** continue
startup, and Codex records the dismissal itself. Only reviewed npm, Bun, Vite+,
pnpm, Homebrew and standalone Unix commands are recognized; other or clipped
commands stay in Terminal.

After **Update now**, the desktop shows a notice for that state: Codex is still
installing its update, or it has exited and the session needs a resume. An
older host's receipt has no `codex_update`; the desktop then only switches to
Terminal.

Codex's **Approaching rate limits** chooser also appears in Activity and on the
phone after a turn. Its reviewed **Switch to gpt-6-luna**, **Keep current model**
and **Keep current model (never show again)** choices use the exact guarded native
answer path. Zerus never selects one automatically. A changed or incomplete
chooser stays **Needs input** with an **Open Terminal** hint; it never authorizes
a model change or an ordinary message. Each answer pins the process, run,
conversation and complete panel before every key, and uncertain delivery is never
replayed.

After a resume, Codex asks **Resume paused goal?** when the conversation's goal
is paused, stalled or limited by usage. Activity shows the goal from Codex's goal
database once the centered dialog over the transcript shows the same goal and
both choices; the session waits for this choice instead of Restoring
conversation. **Resume goal** marks the goal active, and Codex continues it by
itself when idle. **Leave paused** keeps it paused and the conversation waits
for your message. A dialog too narrow to read, or for another goal, stays in
Terminal.

Optional Codex questions preserve the regular composer and support **Skip**.
Codex closes unanswered optional questions when their native turn completes,
is interrupted, or accepts an ordinary follow-up. Zerus removes those cards
and their question badge without recording a user answer. Continued tool work
alone does not close a question. Questions belonging to a native child remain
with that child's parent; they are not requests for the person.
When Codex's own question panel is open, Zerus answers or skips there. Codex's
animated working status above the panel does not block an answer; any change
to the panel itself does. An open panel taller than its pane keeps its title
and scrolls the answers, naming the visible row in its key hints (`option 1/6`).
In a pane no client views, a question listing grows the pane as it does for
Claude until the whole list shows; a title taller than the pane stays in
Terminal. A long answer wraps in Codex's Other field, and Zerus compares that
field, wrapped rows joined, with the text it pasted.
Queue arrows navigate optional requests from newest to oldest while retaining
answers and the current multipart section. A required approval temporarily takes
priority. Confirmed answers appear as **You (answer)** in Activity. Codex optional
question replies show the question and your answer in separate sections,
preserving the literal answer text.

If an optional answer enters Codex's native queue, the question stays visible as
**Submitted / Awaiting agent** with the original answer locked. Submission means
the verified terminal received the answer; it does not mean Codex has recorded
it yet. This state survives polling and GUI restarts. The card disappears after
the exact native question reply appears in the conversation. If Codex closes
the question first, this exact submitted answer stays as a read-only delivery
receipt until acknowledgement; it is not an outstanding request for input.
Zerus never resends
the answer or promotes the queue automatically; **Send now** remains an explicit
native queue action. Delivery without confirmed submission still requires
checking Terminal before retrying.

### Models, effort, context, goals and subagents

The Activity model selector changes supported per-session model/effort settings,
leaving account defaults intact. Codex/Kimi options come from the selected
account's native catalog. Before the first message, verified empty native input
can allow selection without creating a conversation. Startup prompts, drafts,
unconfirmed resume and an already submitted first message block that operation.
Unverified states fall back to Terminal with an explanation.
A choice that the agent's status line already shows is confirmed without typing
anything into Terminal. A change that fails after the agent visibly read every
typed key and returned to its empty prompt leaves nothing unconfirmed, so another
change or a message can follow, including a new session's first message. When the
agent has not reacted to a typed key, further changes and messages wait for its
next native event: finish or cancel the change in Terminal and send a message there.
Codex Ultra's double-chevron composer remains an ordinary input field: a
confirmed session-only change permits another effort choice, messaging and
context clearing with the same identity and draft checks. Earlier unconfirmed
attempts retain their original receipts and are never replayed automatically.

Codex model choices use the selected account's `models_cache.json` and sort by
ascending signed 32-bit `priority`, preserving source order for ties, as in
[Codex's native picker catalog](https://github.com/openai/codex/blob/rust-v0.162.0/codex-rs/models-manager/src/manager.rs#L170).
Missing or invalid priorities follow ranked entries in their original order;
a legacy catalog without priorities keeps its source order. This shared catalog
order applies to both desktop and Android without changing the current model.

Claude options are the models its `/model` picker lists, including **More models**,
from the catalog Claude Code caches in the account's profile. Zerus expands a
section only when the native picker advertises that action, selects the model
and effort, and applies them with its session-only choice (`s`). Enter on a model
row saves new defaults; Zerus never uses that action or the typed `/model NAME`
and `/effort LEVEL` shortcuts for a session change. An unconfirmed selection
closes the recognized model picker with Escape while its original input authority
is still valid. The change is confirmed by Claude's receipt in the exact conversation transcript and by
its effort indicator when shown. In a conversation that already has replies, Claude
warns that the next message re-reads the full history and uses more tokens: the
selector states this before **Apply**, and Zerus then accepts only that built-in
notice. A confirmation requested by a `PreModelSwitch` hook is declined and
reported, as is a picker without the session-only choice; use Terminal for those.
With Claude Code 2.1.296 or later, its `PostModelSwitch` hook names the exact model
of every switch at once: `/model` in Terminal, a change from Zerus, an automatic
switch or a resume. The selector shows that model until the transcript records a
later turn or `/model` receipt; the switch itself adds no Activity entry.
A stopped Claude session resumes with a choice made in Zerus through `--model` and
`--effort`; otherwise it starts with the account's settings.

Busy-session choices, and choices made while the terminal shows a chooser, remain
**Pending** until the agent is ready, including after navigation away. If a
chooser appears while the desktop applies a choice, the choice stays **Pending**
and the desktop tries once each time the session becomes ready again. A failed
or refused application, a new run and a new conversation end these attempts. A
message sent before application uses the current model.
**Save for resume** stores a stopped session's choice without starting it. A stale
queued setting cannot replace a newer choice from another device.
If a message or another client already consumed or replaced it, Zerus refreshes
the current settings without retrying the stale request or showing a failure.
`hgs [@host] settings SESSION --json` exposes the same scoped operation.

Native clear/compact operations are capability-gated and require exact identity.
When they are unavailable, `hgs inspect` reports why in `clear_context_reason` and
`compact_context_reason`, empty while the action is available. A busy session names
what keeps it from Ready: the main agent's turn and tools, and each working subagent
with its current tool, up to three, then a count. The actions follow the status the
session list shows, so Kimi profile groups still working after the main turn ended
keep them unavailable and are named by profile, such as `2 “code” subagents`.
Applying settings and forking a running session wait for the same Ready, and a
profile hook entry that Kimi's native subagent catalog has replaced holds back
none of them. A chooser recognized only on the terminal, such as Kimi's context
choice after an idle period, keeps clearing, compaction, forks and effort changes
unavailable until it is answered; `fork_reason` and `effort_change_reason` give the
same reason as `clear_context_reason`. A model settings change made meanwhile is
saved as **Pending** (`settings_apply_when` is `ready`), and no key reaches the
chooser.
The desktop shows the reason as the tooltip of the disabled **Clear context**,
**Clear session** and **Compact and continue** actions, and as a line in the
cold-cache confirmation.
Hosts from before these fields leave the actions disabled without a reason.
**Clear session** requires confirmation naming the session and host, preserves
unsent drafts and rechecks identity. While Claude shows an approval, question or
another chooser, **Clear session** asks for a separate **Dismiss and clear**
confirmation: Zerus presses Esc once, as in Terminal, so an approval is denied and
a question stays unanswered. It runs `/clear` only after the chooser closed and the
empty native prompt is verified, and never repeats an uncertain attempt. Startup
choices such as resume, permission mode and folder trust are answered in Activity
instead. Claude reports no hook when **Stop** ends its turn, so the session stays
**Interrupted** until its next message. **Clear session** is available there once
no tool or subagent still works and the empty native prompt is verified, as for
that message; a native draft keeps it unavailable and is named as the reason.
Compaction still waits for Ready. DeepSeek context reset is unavailable until
its native Session Controller provides the supported operation. A confirmed clear
adds an amber **Session cleared** boundary to Activity and keeps the earlier local
timeline above it: the agent no longer sees that history, but you can still read
and search it. Resuming or opening another conversation starts a new timeline.
Codex can defer its new-session hook until the next message; its verified empty
native reset panel also confirms the clear, including `/clear` in Terminal. Only
the header directly above the empty composer counts: the same header above an
earlier message and its answer does not. Old
context and prompt-cache values are hidden until the new conversation reports
native usage. The saved native identity changes only on the real hook, and an
unconfirmed command does not reset counters.
A resumed Codex session can also confirm a Terminal clear when its main end
precedes that hook; the end alone never confirms a reset. Zerus keeps the process
and run and observes the real successor without replaying the clear or message.
A stopped session offers **Start fresh…** instead, with the same confirmation, and
keeps **Send** available: it offers **Resume and send** with the full saved context,
**Resume, compact and send**, or **Start fresh and send**. Zerus Next names the
same choices **Resume**, **Compact and resume** and **Start fresh** under its
"Start the session to send?" title, with **Cancel** last as the default; its
dialog actions wrap onto further rows rather than leaving a narrow dialog. The compact option waits
for the restored conversation to accept input, then runs native compaction and
delivers the unchanged message only after confirmed success. A failed start, an
unexpected conversation or three minutes without readiness cancel that send and
keep the draft.
**Compact and continue** sends only after confirmed successful compaction with the same
unedited draft; errors, cancellation or identity/draft changes cancel submission.
While awaiting startup or compaction, **Waiting to send…** keeps the accepted text
and attachments visible and locks edits. The field still supports selection and
copy. **Cancel send** restores editing and keeps the complete draft; it cancels
the automatic message, while startup or compaction continues. **Stop** interrupts
the agent. A GUI restart keeps the draft without resuming automatic submission.
Switching dialogs, changing filters or closing the workspace keeps accepted sends
pending on their original sessions. They continue in the background, verify the
same run/conversation and compaction request, and never submit a second copy.
Context metrics use native telemetry, without guessing a capacity from a model name.
Claude's window comes from its status line (see
[Tracking and durable state](#tracking-and-durable-state)): `session_usage.context`
gains `limit`, `limit_source` `native_statusline`, `limit_model` and
`limit_observed_at` only while the latest report belongs to the session's run and
conversation and names the model that the inspection reports. After a model change
the window stays hidden until Claude reports the new model; after a clear Claude
2.1.296 reports at its next update, usually the next reply, and a resumed run
reports when it starts. Compaction changes the used context, not the window.
Claude's recorded compaction boundary replaces the earlier context size with its
reported summary size. It invalidates the earlier prompt cache estimate; a new
warm countdown requires fresh native usage with a reported TTL.

**Tasks**, **Plan & agents**, **Details** and **Worktrees** occupy the resizable inspector.
Its width, tab and open state are local; toggling it preserves Activity, drafts and
Terminal attachment. Plan & agents shows the agent's own plan, not the project's
Zerus tasks. Successful native task/plan tools supply recorded pending,
in-progress and completed items. Tool failures and Stop do not complete tasks.
There are up to 100 items per list and 65 lists, with truncation disclosed.
Old data shows **Last recorded**; unavailable history is not reconstructed.

Goals remain independent of turn state. Codex uses `goals_1.sqlite`, Claude uses
`goal_status` in the exact transcript, and Kimi uses native `goal.create/update/clear`
wire events. DeepSeek exposes its native phase and continuation permission.
Only recorded usage and budgets are shown; missing data is never inferred from a
prompt. Ready does not complete a goal.

Subagents open their own Activity and return through **Main activity**, preserving
the parent's draft. Individual child IDs and profile groups are distinguished;
unknown counts remain unknown. An unknown child never keeps its parent from Ready.
A Claude child that stops on a provider error, such as a usage limit, gets no
SubagentStop: its StopFailure, or a `failed` entry for its current start in the
workflow journal with no newer child event, settles it as **Error** with that
message, and a later event of the same child resumes it. Ordinary child
results/errors stay out of the attention count, while explicit input/approval
requests remain visible. DeepSeek
continuable children can receive messages through the native API with parent
verification; other adapters and one-shot children may expose history only.
The expanded session tree hides children whose latest reported turn is finished
or failed by default. Session and Overview cards then show only the active count, omitting
an empty indicator when no active or uncertain children remain.
**Settings → Sessions → Show completed subagents** restores completed rows and
active/total card counts. Unknown states, paused agents and input/approval
requests remain visible. The **Plan & agents** inspector counts the full
reported roster (active/total) but keeps only working agents, those waiting for
an answer and the one open in Activity in view; the others fold behind
**Finished: N** and unfold 50 at a time, so a session with many subagents
renders and scrolls a bounded list (owner, 2026-11-08, T-ug5hxx). Each session
starts folded. `client/scripts/inspector-scroll-smoke.html` measures that
scroll in WebKitGTK under a private Xvfb.
An open child's Activity and draft stay in place when its turn finishes. A child's
draft keeps its row visible after returning to main Activity. Finished turns are
not proof that the agent's task or process has ended.

Cross-provider children (for example, Codex launched from Claude) bind their
original PID and native birth identity to the parent's run and conversation at
the verified hook caller. If that original exits without a completion event,
the child shows **Ended** or **Unknown**, never **Working**. Missing or unreadable
process identity remains unknown. Explicit pending input/approval stays visible:
process uncertainty suppresses Working, not attention. Neither a recycled PID nor an old journal
start establishes current work. Recorded native jobs of such a child become
unknown unless their own separately verified original process is still live.
Other confirmed live jobs keep their ordinary **Background** status. Offline
and historical rows establish no current work. Process observations do not
invent a Stop, successful result, unread reply, or native execution permission.

`hgs [@host] subagent retire SESSION --json` explicitly retires one stale child
record without starting, stopping or messaging its agent. Inspect the current
parent and child first, then submit an exact-generation request:

```sh
hgs subagent retire claude/example/review --json <<'JSON'
{"request_id":"11111111-1111-4111-8111-111111111111","expected_run_id":"22222222-2222-4222-8222-222222222222","expected_conversation_id":"33333333-3333-4333-8333-333333333333","agent_id":"external:codex:44444444-4444-4444-8444-444444444444","expected_updated":1700000000.5}
JSON
```

The command refuses a verified live original, current native-child work,
changed run/conversation/child timestamp, or a profile group. An unbound legacy
or unavailable identity can be retired explicitly as record housekeeping, not
as proof of process exit or successful completion. Requests are at most 8 KiB,
the selected manifest read is capped at 16 MiB, the writer-lock admission waits
at most two seconds, and the parent keeps at most 64 immutable retirement
receipts. Repeating the same UUID and payload reads its original receipt; it
never retires a later start. A fresh verified SessionStart may resume that child.
An intermediate hook can also recover a mistaken retirement when its native
caller verifies the same original process, or binds a previously unbound legacy
record. A different recorded birth still requires a fresh SessionStart.
Process checking admits at most 64 external identities per read; excess or
unverifiable entries remain unknown. Older peers require the
`subagent-retire-v1` help capability and refuse this command until updated.

Claude child tracking reconciles hooks with exact native history, including
workflow children. A final assistant `end_turn`, a successful `toolEndsTurn`
result bound to its exact owned tool call, or a final **Request interrupted by
user** row can settle a turn even when `SubagentStop` is missing. Its **for tool
use** variant settles only when it follows the owned error results of the
stopped calls; an unbound marker changes nothing. A tool-use interruption that an
earlier hgs version recorded as new child work settles from the same native row
unless a newer hook arrived. Matching
workflow `started`/`result` attempts independently confirm the same reply.
Later native child activity can recover a missing start; newer hooks take
precedence. Silence and a completed parent never prove child completion.
Reconciliation preserves existing reply and read identities. `hgs inspect`
reports each child's `tracking_evidence`: source, event kind, event time,
native row identity and, when verified, workflow result confirmation. Missing,
partial or ambiguous native evidence leaves the last reported state intact.

Codex child tracking verifies the child and parent IDs in the account-owned
rollout, then reconciles typed turn starts, successful completions and explicit
interruptions. An interruption without a turn ID requires a matching start in
the bounded native history. Kimi's version 2 session catalog identifies direct
children individually, including several children using the same profile;
their own wire turn events establish current work or completion. Older,
delayed or ambiguous catalogs retain profile-hook counts. A removed catalog
entry becomes unknown rather than a fabricated completed result.

DeepSeek uses each child's exact native API entry within one host generation.
A child never inherits its parent's running status. Missing, contradictory or
cross-generation observations remain unknown; desktop and Android counters
retain the `+` uncertainty marker. OpenCode child sessions are not shown in this
version ([OpenCode controls](opencode.md#controls-and-limits)). Inspection reports native
tracking evidence without changing reply/read identities or restarting agents.

<a id="fork-session"></a>

**Fork session…** creates an independent native conversation in the same folder,
next to the original session in its group. Files remain shared. The CLI is
`hgs [@host] fork SESSION [-n NAME] [-d] [--archive ID]`; support depends on the agent.
DeepSeek uses the native fork of a supported resident host
([DeepSeek](#deepseek)). A session name is a label: an archived session never
blocks a new session or a fork from its name. Without `-n` the fork is named
`TAG-fork`; a name a live or saved session already uses gets the first free `-2`,
`-3`… instead of failing, with or without `-n`. The fork dialog shows the name the
fork will get, and the new session is recognized by its fork parent, never by a
name another session took meanwhile. Inspection reports `fork_parent_id` and
`fork_source_name` as null for a session that is not a fork; only an hgs that
predates these fields is trusted for the exact name. A failed inspection never
adopts a session and is retried, at most three times, as the list changes. A
fork the desktop has not recognized within two minutes is reported as not
confirmed and Fork is available again; the session may still be starting.
An hgs older than this behaviour still refuses a name that only an archived
session holds; update that computer or choose another name.

A fork starts in its source session's project, including a fork of an archived
session. Once the fork's binding exists, the CLI writes that membership through
the same catalog owner as other project changes, for the desktop action,
`hgs fork` and DeepSeek forks alike, and never moves other rows. A source in Inbox,
in a project that no longer exists or with an unresolved project choice gives the
fork an explicit Inbox membership. A source without a membership leaves the fork
without one, so both follow the same default project. A fork name that already
has a membership keeps it. Without a catalog, or if the catalog stays busy for
3 seconds, the fork still starts in the default project. That wait happens after
the fork's binding is saved, without holding the session state or DeepSeek
binding locks, and the membership is written only while the fork's binding is
still the same run and conversation. The desktop then places
the fork right after its original and opens it, once its inspection names the
source conversation as its parent, even before a Codex fork has a conversation ID
of its own. With an hgs that predates the parent field it waits for that ID.

A fork's Activity first shows the history it began from, read-only: each source
session's rows up to the moment of the fork, under an "Inherited from" label
naming that session, then a "Forked here" marker and the fork's own activity.
A fork records its sources when it starts (up to four forks back, so a fork of a
fork shows both); a fork made before this records nothing, so its first
inspection finds its source's rows up to its creation time and before its own
first row, and saves that. Each Activity page reads at most one page from each
of these ranges, however long a source is. Inherited rows are muted and never act as the
fork's own: they open no attachments, and search finds them only in their source.
A provider that copies the history into the fork's own transcript adds nothing
twice, because the fork's messages dated before its creation are not shown. The
source session is never changed. Rows the journal no longer keeps are not shown.
The phone history (`hgs history`) shows the same inherited rows as additive
`Inherited` rows that never count as unread; see `services/mobile/README.md`.

Codex may defer `SessionStart` until the fork's first message. Activity enables
Send once Zerus verifies that the supervised process owns an independent child
history linked to the source conversation and its native composer is ready.
The child ID is confirmed by the agent's own event; the source session stays intact.
On Windows, verification searches only the selected account's history and requires
the same supervised process to hold exactly one matching child history open.
Incomplete, unreadable, ambiguous or exhausted verification keeps Send unavailable.

<a id="projects"></a>

### Keep cache warm

A session can keep its agent's provider prompt cache warm for 1 to 7 hours or
always, so the next message after a pause reads the cached conversation instead
of rebuilding it. It is off by default and lives with the session on the
computer that runs the agent, so every desktop and phone sees the same setting.

While it is on, the [recovery worker](#automatic-recovery) on that computer
sends the idle agent one short message, exactly
`[Zerus keep-warm] Reply with exactly OK and do nothing else.`, at 80% of the
cache lifetime after the last turn ended: 48 minutes for a 1-hour cache and
24 minutes for a 30-minute cache. The margin keeps pings ahead of Claude Code's
own idle compaction at 90% of the TTL. A ping only extends a warm cache: when a
window was missed (the computer slept, the agent stayed busy until expiry) or
the cache is within 30 seconds of its estimated expiry, nothing is sent and
keep-warm resumes after the next real turn. The cache clock belongs to the
current conversation: after **Clear** the setting stays on, and the first ping
waits for a finished turn of the new conversation. The deadline is on the agent
computer's clock; at the deadline the worker turns the setting off. A rename,
including a legacy-label rename, keeps the setting.

| Agent | Lifetime used | Offered |
|---|---|---|
| Claude Code | the TTL its last request wrote in native usage: 1 hour on a subscription within its usage, 5 minutes on an API key or usage credits | 1 hour only |
| Codex | OpenAI's 30-minute default for GPT-5.6 and later; Codex sends no TTL | GPT-5.6 and later |
| Kimi Code | Kimi Code's own `estimated_cache_duration` (60 minutes for `kimi-for-coding`, 30 minutes for `k3` models), read from its home | models with at least 15 minutes |
| DeepSeek, OpenCode | DeepSeek's disk cache lasts hours without pings; others report none | no |

Shorter caches are not offered: a 5-minute cache would need about 15 pings an
hour, each a cached read of the whole context, costing about as much as
rebuilding the cache once. Claude Code's `CLAUDE_CODE_PROMPT_CACHE_TTL=1h` or
`promptCacheTtl` setting requests the 1-hour cache on API-key accounts at a 2x
write price. Each ping uses the account's usage: a cached read of the whole
context plus about 40 new tokens, which stay in the conversation.

A ping goes only to the exact verified, live agent process with the expected
run and conversation when it is settled: no turn, tool or child is running, no
live background or scheduled job, no automatic recovery episode, no unconfirmed
or queued input, no pending launch message, question, approval, trust, resume,
usage-limit or cache-expiry choice, no pending model or effort setting, an
empty recognized composer, no terminal input in the last two minutes (a terminal
that cannot report input times must have no attached client; unknown activity
never admits a ping), and the session is not paused, archived or terminated.
The last check before the first typed byte repeats these rules and the
30-second cache margin, so a slow stage never types into a cold cache. It uses the
guarded send path with its own origin and request ID, bound to the setting
revision it was decided for (a change in between cancels it): a refusal before any input
waits for the next round, and an unknown delivery is never sent again; keep-warm
then waits for the next real turn. Rounds run every 15 seconds and never
overlap, so two desktops or a manual `hgs recovery tick` cannot send two pings
for one window.

A ping is not the person's message. Its prompt is journaled with
`origin: "keep_warm"` and every event of its turn with
`turn_origin: "keep_warm"`. Its Stop is never the session's reply, so it adds no
unread badge, notification or read-mark change; listings and search keep the
previous prompt, last message and last event time. A message the person sends
or queues while a ping is being typed or running makes that turn theirs; the
ping's own prompt arriving later is then shown as ordinary text. A ping turn that
fails, for example on a usage limit, never starts automatic recovery, even when
an earlier finished recovery episode is kept: keep-warm stops
until the next real turn, and the provider failure stays visible as usual.

In the desktop app, **Details** shows **Keep cache warm** in the **Session**
card of every live, paused or stopped session. Its switch (accessible name "Keep
cache warm") turns it on for the chosen duration; while it is on, a slider picks
1 to 7 hours or Always (its last position, shown as an infinity sign in a
fixed-width value so the slider never moves; its accessible value is "Always")
and the row shows the remaining time, such as **2 h 14 m left**, and what
keep-warm is doing: the next ping, **Warming the cache now**, a cold cache or
the host's reason. Its tooltip states the cache lifetime and its source, the
ping interval, the pings per hour and that each ping is a cached read of the
whole context. A change shows at once and the controls stay enabled and in place
while the computer confirms it; a choice made meanwhile is sent next with the
confirmed revision (the newest one wins), and a refusal returns the row to the
confirmed state. The slider rounds the time left up to whole hours but ignores
the first five minutes past an hour, so a fresh choice keeps its position
although the host's clock may run slightly ahead. When the agent or account is
not eligible the switch is disabled and the host's reason is shown under it; a
computer whose `hgs` lacks `session-keep-warm-v1` shows an update hint. While it
is on, the session header also shows it on its chip row beside the model: a bolt
with the time left, such as **2 h 14 m**, or an infinity sign for Always, at the
chip row's height so the header never grows. Its tooltip gives the same state
and lines as the Details row, and a click opens **Details** at the control. It
reads only the listing, the followed session's Details summary and a change
confirmed in this window. While a ping runs, the session reads **Ready** with
**Warming cache** and counts in neither Active nor Working. Activity folds the
ping, its reply and its Stop into one dim **Cache kept warm** row that expands
to show both.

Android follows the same status rule: while a ping runs the session reads
**Warming cache**, is neither Working nor Active, and holds no reply in progress,
so an earlier unread reply keeps its badge. The shared session-filter contract
(`mobile/android/app/src/test/resources/session-filter-contract.json`) pins this
for desktop core, the desktop frontend and Android. The phone's session Details
show **Keep cache warm** read only (Off, On always, or the remaining time) and
one status line: the next ping, **Warming the cache now**, a cold cache or the
computer's reason, including why an agent is not eligible. Changing the setting
from the phone needs a relay operation and is not available yet.

```sh
hgs [@host] keep-warm SESSION --json
```

The request on standard input carries `expected_run_id`,
`expected_conversation_id`, `expected_revision`, `enabled` and, when enabling,
`hours` (1 to 7) or `always: true`. A stale identity, an older revision
(`conflict`), more than 64 enabled sessions on one computer (`capacity`) or an
ineligible agent (`unsupported`, with `reason`) are refused. Enabling starts the
recovery worker as a policy change does. `hgs --help` lists
`session-keep-warm-v1`; an older peer refuses the command.

Listing rows carry `keep_warm: {enabled, until, revision, state, next_at,
reason, turn_active}`; `until` is null for Always and `state` is `off`,
`waiting`, `warming`, `cold`, `uncertain`, `stopped`, `paused` or
`unsupported`. `inspect` and the command response add `supported`,
`reason_code`, `ttl_seconds`, `ttl_source` (`native_usage`, `provider_default`,
`kimi_config`, `kimi_table`), `interval_seconds` and `cache_expires_at`.
The setting is `keep-warm.json` in the state directory, keyed only by session
IDs or label keys; the worker's state of each session is under
`recovery/keep-warm/`. Both are read as bounded regular files: a special file is
refused without blocking, and a worker state that is unreadable or not one
the worker writes holds keep-warm as uncertain until the next real turn. A
legacy-label rename carries the worker state before the setting, stops when it
cannot tell whether a state exists (permission denied is not absence), and
repairs an interrupted move when the rename resumes. A session record that exists
but cannot be read skips the round; only a session that is gone turns the
setting off, and turning it off keeps an uncertain-delivery fence. See the
[design note](superpowers/specs/2026-11-07-keep-cache-warm-design.md).

## Projects and P2P synchronization

A project has a stable ID, name, color, folders on multiple computers and session
membership. Drag sessions between projects without moving folders or replacing
agent processes. Drag headers, use the context menu or Alt-Up/Down to reorder
projects. The project order is shared: every computer in the swarm and the
Android app show the same order, and the latest change from any desktop replaces
the others without a review. Until someone reorders, each desktop keeps its own
order and Android lists projects by ID. Empty-project visibility, collapse state,
session order within a project and default project are local.
**All swarm projects** explicitly shows the full shared catalog.

A newly discovered session appears in the local default project until a shared
assignment arrives. Discovery alone does not publish that fallback as a project
assignment, so another desktop can launch the session into its chosen project
without racing an automatic Inbox assignment. Launching or explicitly moving a
session publishes its project choice, including a deliberate move to Inbox.
Update the desktops that observe sessions to receive this behavior; existing
catalog assignments and unresolved conflicts remain available for review. When ID grouping is available, it uses a separate bounded causal membership record for each computer and session ID. Explicit unassignment and conflicting project choices are records, not permission to fall back to a shared label. Distinct concurrent project choices stay reviewable; a later explicit choice consumes the observed heads. Only peers advertising `session_id_memberships_v1` exchange these records. Older builds preserve the separate file and keep their existing name/twin protocol, but cannot change ID grouping.

Folder entries contain a machine, path and optional label. Browse works for local
and remote folders, including ordinary non-Git directories. The desktop folder
browser offers **New folder** on the selected remote computer. Enter only the
child folder name directly in the list and press **Create** or Enter; Escape
cancels the inline entry. The list context menu and Ctrl+Shift+N (Command+Shift+N
on macOS) open the same entry. The **Dialog…** button beside **New folder** opens a separate
name-only dialog with the parent shown above it. A missing path entered in the
browser supplies that form's parent and initial name; the parent must already
exist. Once
creation is confirmed, choose **Use this folder** to select it for the project
or session. Browsing and cancelling the creation form do not create anything,
and existing paths are never replaced. This requires an updated `hgs` on the
selected computer; older peers can still browse existing folders. If creation
cannot be confirmed, the browser checks that exact path without repeating
creation. An available folder can be selected immediately, including an empty
folder or one that already existed; an unsuccessful check retains the creation
error and **Go** remains available. Creating a folder does not initialize Git
or start an agent.

The CLI equivalent is `hgs [@host] dirs --create <path> [--request-id UUID]`.
It creates one directory and returns the canonical path plus the exact request
identity as JSON. It never creates missing parents or repeats an uncertain
operation. Local creation rejects `--dry-run`; remote dry runs only print the
transport command.

`hgs [@host] path-info --path <path>` answers whether a path exists on that
computer, with one metadata read: no content is read and nothing changes. The
path is absolute (a Windows drive or UNC path on Windows) or starts with `~/`
for the home folder. The reply is one line of JSON, versioned and closed:
`{"schema": 1, "exists": true, "kind": "file", "size": 1234}`, where `kind` is
`file`, `directory` or `other` and `size` is given for a file only. A path that
does not exist reads `exists: false`; one this account cannot inspect reads
`exists: null`, with `kind` and `size` null. A relative path or one with control
characters exits 1; a malformed call exits 2. A peer call runs only when the
peer's `hgs --help` names the command, as an older hgs would read an unknown
command as a session launch; the path's `~` reaches the peer's hgs as written.

The desktop folder
list starts in alphabetical name order, ignoring case; column headings change
the sort. Sorting preserves the selected folder and does not reorder the shared
catalog. The divider below the list adjusts the folder/worktree heights and
remembers that choice locally; additional window height goes to the folders.
Worktree actions share its heading row, leaving more space for the folder list.
Right-click a folder and choose **Move to** or **Copy to**, then the destination
project. Move transfers its catalog link; Copy keeps the original and adds a link
to the other project. The computer, path and label are preserved, including for
offline computers. If the destination already has that folder, its existing link
is reused. Files and session project assignments stay in place. These catalog
changes use the normal Swarm synchronization.
An untouched **Inbox** selection in New session follows an unambiguous project
whose folder is confirmed on the selected computer. Desktop, Android and iOS
use exact canonical folder membership or a complete, current worktree catalog;
folder names and path prefixes do not assign projects. Explicit project choices,
including explicitly choosing Inbox, win. Multiple matching projects or missing
verification keep the current selection. An untouched Inbox stays a local fallback,
so later native project discovery cannot be overwritten by launch bookkeeping.
Older saved desktop launch intents retain their original placement semantics.
Desktop also checks existing saved folders on that exact computer against the
CLI-confirmed canonical path, including local Windows drive and UNC folders
that remain outside the POSIX shared catalog. It does not resolve saved aliases
or infer from a parent folder. A native project hint must name the same known
Swarm catalog; a foreign hint never selects a same-ID local project.
A new tracked CLI session also infers its initial project from that computer's
existing catalog; resume, adoption,
manual assignments and explicit unassignment are never reassigned. A fork
takes its source's project instead ([Fork session…](#fork-session)). This optional
metadata step does not initialize a catalog, join a swarm or create a repository.
A new session that a live tracked agent on the same computer launches (its
parent link proven from process ancestry) starts in that parent's project,
for Claude, Codex, Kimi, DeepSeek and OpenCode alike. The parent's project wins
over folder inference, so a session tree stays together. A parent whose project
no longer exists gives the child an explicit Inbox membership. A parent in Inbox,
without a membership or with an unresolved project choice leaves the child to
folder inference, as does a claimed, adopted or forwarded parent. The membership
is written like a fork's: after the child's binding is saved, retrying a busy
catalog for up to 3 seconds without holding the session state or provider
binding locks, and only while the child's binding is still the same run and
conversation. A child name that already has a membership keeps it. Existing
sessions, including children placed before this rule, are never moved.
The general **New session** action inherits the selected session's project,
computer and matching project folder. For a linked worktree, a verified catalog
selects the main checkout instead. Choosing another folder, project or computer
cancels this automatic selection; explicit folder and worktree launch actions
keep their requested path.
Project switches in **New session** keep the machine and select that project's
folder. Only a path explicitly chosen through **Browse…** stays pinned per machine
within that dialog;
closing it clears this temporary choice. Selecting a listed folder unpins it.
A folder outside the project shows a warning and is added only after a successful
launch with the matching launch ID. Cancellation and launch failure add nothing.
A worktree of a project folder on that computer is not outside the project, so
starting there adds no folder; **Choose existing…** offers it again. When an
automatic worktree is generated from an outside folder, the session starts in
the worktree and the outside folder, not the worktree, joins the project.

New launches immediately show **Starting…** until the matching session appears.
Controls stay disabled before confirmation. Errors remove the placeholder and
show a reason; **Hide launch status** hides it while preserving a late session's
project placement. A session that ends before it appears and leaves no record,
such as an agent that cannot start after the launch reported it started, also
removes the placeholder: a notice shows its exit code and last lines, as its
computer reports them in `ended_launches` ([JSON and external
clients](#json-and-external-clients)), and stays when the launch command
reports afterwards; a hidden launch status ends silently but still shows a
later launch error.
Existing names and unrelated sessions are never repurposed.

On Unix, agent launch and resume use explicit POSIX or fish argument syntax for
known login shells. Other configured shell types use `/bin/sh -lc`, with the
inherited environment and POSIX login profile; their own rc files are not loaded
or interpreted as another language. tmux executes the argument vector directly,
including on computers whose default shell is not POSIX. Account selection and
agent hints are reapplied after shell startup.

**Open terminal window** is off by default and remembered separately from the tray
open mode; DeepSeek uses its embedded native interface.

To join a swarm, configure SSH access and open Zerus on the initial machines so
existing projects import once. Projects shows **Connect project catalogs…** when
an accessible computer has no project-sync connection. Session access over SSH
alone does not connect project catalogs: the guide opens **Connect computer**
with the missing computer selected. Choose **Preview connection**, review project
mapping and **Join and merge projects**. Nothing is paired or joined by opening
the page. The same flow remains under **Projects → Swarm… → Connect computer**.
Names alone do not merge projects. Existing swarm membership wins by default;
turn off that preference to compare conflicting assignments. HGS backs up the
catalog before a merge. **Connect this member** adds a route to an existing member.

There is no permanent leader: each member stores the shared catalog and can relay
changes. One configured SSH connection is enough for exchange in both directions;
the other side needs no reverse SSH connection. Learning a machine ID through the
catalog does not create local transport access to it.

Shared fields are machine IDs/names, project IDs/names/colors, folder locations,
session membership and the phone notification timing. SSH hosts, users, ports,
keys, proxies, credentials, transport bindings, local order, filters,
collapse/appearance and the default project stay local. File contents and agent
history are not copied. Offline hosts do not hide projects with locally configured
access.

The catalog and local bindings live in `~/.config/hgs/swarm/catalog.json`; do not
copy that complete file between machines because it contains node identity and
local transport bindings. `hgs swarm export` exports only shared fields. Original
Zerus settings stay in `workspace/organizationBeforeSwarm`; import/merge backups
use `~/.config/hgs/swarm/backup-*.json`.

A session's project is shared by its label and, since zerus-ucck P2c, also by
its durable session ID. Every writer of a membership on the session's own
computer, `swarm apply` included, adds an ID twin: the session ID and the
exact label membership operation it mirrors. Twins live beside the catalog in
`swarm/session-id-twins.json`, never in `catalog.json`, so an older hgs on the
same computer, and every peer that does not list the `session_id` kind, never
reads one; only members that list the kind exchange them. `swarm get` lists
the ID form as `projects[].session_ids` ("machine\nsession ID") only while the
operation a twin mirrors is still the selected label membership, so any later
move by label, from any hgs or desktop, hides it. `swarm get` also reports
`membership_by_id {ready, missing}`. It is ready when every machine of the
catalog is this computer or a peer whose last sync listed the kind; any other
machine fails closed. Desktops and the Rust mobile connector read the ID form
only when it is ready. The connector keeps sessions with the same label separate
by ID; a missing or invalid session ID never falls back to that label's project.
With a legacy catalog, only unique labels receive label-based membership; twins
stay unassigned rather than sharing a guessed project. The label form is still
written for compatibility. `swarm export --kinds`
takes the kinds its requester parses on stdin; without them it exports only
the kinds hgs v0.47.0 parses.

Primary project placement is stored separately in the bounded causal
`swarm/session-id-memberships.json`, exchanged only with peers advertising
`session_id_memberships_v1`. It preserves concurrent concrete placements for
review; a concrete project beats concurrent default placement, and a later
explicit Inbox/unassignment wins. Valid twins are migrated under the catalog
lock without replacing an existing ID record. An exact primary null/conflict
means unassigned; only an absent ID record allows name membership fallback.
Unreadable primary data stays unavailable and is not repaired automatically.
Older builds preserve this file but cannot edit its ID grouping.

The `session-labels-v1` transition gate requires fresh positives from this
computer, every configured peer and every catalog machine. An unaddressed,
unknown or unreachable member keeps it closed. `hgs swarm session-labels-gate
--json` reads the bounded ten-minute cache without starting a worker or making
connections. Each hello retires the previous observation; ordinary hello never
waits for a connector, and the existing sync worker performs that bounded query
separately. The answering CLI must report both ID membership readiness and its
complete ID routing capability; the earlier observation-only build cannot open
the gate. Configuration, route, membership or connector changes invalidate the
answer. The cache grants no native authority and never weakens exact selector,
original receipt or no-replay checks.

While the gate is open, `hgs reference`, `hgs self` and sender envelopes include
the stored canonical session ID. Older parsers continue to receive the earlier
reference form while it is closed. This capability leaf does not yet enable
new duplicate-label writers.

The session-label transition gate's supported connector inventory consists of
the maintained installer's connector using the canonical mobile configuration
(`core::mobile::config_path()`). On Linux its user unit is
`zerus-mobile-connector.service`; other platforms include an equivalent only
where their installer creates one. An existing configuration or installer unit
requires a fresh positive capability reply from the actual running connector,
bound to its generation, configuration and machine. An installed executable's
help cannot substitute for that reply; an old or absent responder keeps the gate
closed. With neither the canonical configuration nor an installer unit, the node
has no supported connector. Hand-started connectors with custom configuration
paths and legacy services outside the installer must be stopped or upgraded
before labels repeat. They are not inferred from process names. Deploying a live
connector remains an explicit owner-approved operation.

On a computer without a Qt settings store, the first `hgs swarm get` or initial
`organization` watch subscription may initialize an empty catalog. It derives
no projects, folder links or session assignments from names. The client still
sends only explicit edits; placement and concurrent field versions stay in hgs
and the replica. An existing uninitialized catalog may also bootstrap; every
existing operation, peer, binding and machine-name entry is preserved. Steady-state
organization watch reads remain lock-free and do not save. Only the first read
of an explicit organization subscription may try the bootstrap; a present or
undecidable Qt probe is retried only on a new subscription.

The presence check is conservative and never parses Qt settings. Any existing,
malformed, unreadable or unexpected store counts as possibly holding an
organization, as does a probe error. The locations are
`$XDG_CONFIG_HOME/hgdev/hgs-tray.conf` (default `~/.config`) on Linux,
`%APPDATA%/hgdev/hgs-tray.ini` on Windows, and
`~/Library/Preferences/com.hgdev.hgs-tray.plist` on macOS. Zerus.app retains that
macOS preferences domain after switching clients. A computer with such a store
keeps the existing Qt import path; neither trigger bootstraps it or writes a
pending-import marker. Watch hello still obtains its stable machine identity
and may create an uninitialized catalog before the organization subscription.

`hgs swarm get` keeps its ordinary locked load and save, including self bindings,
the machine-name refresh and a stable persisted node identity even when a Qt
store exists and the catalog is still uninitialized. The bootstrap is the only
new behavior, evaluated under that same lock when `imported` is false and Qt
settings are definitely absent. Get and the initial organization subscription
share this idempotent operation, so concurrent first reads initialize once.

An automatic bootstrap writes a node-bound `gui-import-pending.json` beside the
catalog before saving the legacy `imported` flag. A later Qt first start, using
the current hgs, can still call `swarm initialize` to import its local
organization. While that marker is pending, initialize fills only missing
fields, preserving existing values, deletions and concurrent versions. It saves
the catalog before clearing the marker, so a crash can safely retry that
missing-field import. An already imported catalog without a matching marker
keeps the existing initialize no-op. The catalog and peer wire formats are
unchanged; older hgs versions can read the catalog but do not perform the pending
GUI import. Qt's settings store is never changed by the bootstrap or import.
A desktop whose saved Qt organization cannot be read, while Qt's store is
present, runs the same bootstrap through `hgs swarm initialize --g3-bootstrap
--json` with stdin `{"version":1,"expected_node_id":NODE}`. It refuses another
node and a foreign or unreadable marker, leaves an initialized catalog as it is,
and reports `g3_bootstrap` in the returned snapshot. Projects are then usable,
the desktop keeps showing the refusal, and a later readable organization still
fills only missing fields. A Qt store read completely without a saved
organization is a first use instead: the desktop imports the default project
through `--g3`, as for an absent store.
An unreadable, malformed or unsupported pending marker prevents bootstrap but
does not fail get or the initial organization read: they return the existing
uninitialized state and leave the marker untouched. A stderr note is emitted
once per process. Explicit initialize still refuses that unknown import state.

The independent `hgs-swarm` service (`com.hgdev.hgs-swarm` on macOS) makes no network
requests without members. Local changes normally trigger exchange within three
seconds; unchanged peers are checked every 30 seconds, backing off to five minutes
on failure. Zerus reads the local catalog every five seconds without SSH. Offline
members catch up later. Independent fields merge; conflicts remain in the Swarm
review UI. A session's missing/Inbox placement is a fallback: a concurrent choice
of one known live concrete project wins without review, and identical concrete
choices coalesce. A newer explicit move to Inbox or unassignment still wins by
causal identity. Different concrete projects and unknown/deleted project targets
remain reviewable; project mappings are applied before join comparisons. Local
passive observations never publish membership, including during import. All
underlying versions are retained, so the next explicit choice consumes every
observed candidate. Project/folder deletion records still prevent resurrection;
this version does not prune old journal records.

The worker records its version and process in `~/.config/hgs/swarm/worker.json`
and exits when an upgrade replaces its executable, so `Restart=always` or launchd
`KeepAlive` starts the new version. A worker started before this mechanism keeps
running after an upgrade and cannot read a catalog that holds the phone
notification timing. `hgs swarm get`, `preference`, `sync`, `join` and inbound
`exchange` restart such a worker, or one from an older version:
`systemctl --user try-restart hgs-swarm.service`, or
`launchctl kickstart -k gui/$UID/com.hgdev.hgs-swarm` on macOS. They try at most
once every ten minutes and only for the default configuration directory, never
under `HGS_CONFIG_DIR`, and never touch agents, tmux servers, DeepSeek hosts or
`hgs-recovery`. `hgs swarm get` reports `worker: {running, current}`; when a
running worker stays outdated, Settings → Mobile connection asks you to restart the
sync service. Windows has no service manager for the worker; the desktop app runs
it there ([Windows](#windows)).

**Swarm → Conflicts** explains the affected session, archived session, project or
folder. Compare the values and the computers that saved them: projects, project
names and colors appear as the project labels and computers as the machine badges
used elsewhere, with **this computer** beside this desktop's badge. A computer the
catalog does not list stays text with its identifier. **Shown now** marks
the temporary display choice; it does not mean the conflict has been resolved.
Select a row to preview the result, then use **Keep this project** or the matching
action. Project assignments change catalog placement without restarting agents
or removing conversations. Folder/project removals affect the shared catalog,
leave files and sessions intact, and require confirmation. The decision is shared
with connected computers and reaches offline members when they reconnect. Closing
the window postpones the decision. New conflicting versions clear the selection
and require another review, including if they arrive during a confirmation.

```sh
hgs swarm get
hgs swarm preview mac
hgs swarm sync
hgs swarm disconnect mac
```

Disconnect stops outbound exchange through that connection while retaining the
catalog and session access. It does not deny inbound exchange from an existing
member; SSH controls transport access. `HGS_SWARM_SERVICE=0 ./install.sh` skips
worker installation.

### Empty-peer adoption

`hgs swarm adoption-preview PEER --json` checks whether the configured empty
computer can adopt the local catalog without opening its desktop UI. The
destination accepts either an untouched store (`imported=false`, no operations
or pending marker) or the narrow first-read bootstrap: a valid same-node v1
`gui-import-pending.json`, only the current self machine-name operation, the
initial empty-peer membership and empty/missing sidecars. Peers, other product
history, foreign or unreadable data, an existing drive store, or a
present/unreadable Qt store refuses. The pending marker is preserved byte for
byte, and the later missing-only GUI import retains adopted values, tombstones
and concurrent heads.

`hgs swarm adopt PEER --json` performs the explicitly previewed original
request. It copies the complete catalog, primary session memberships and twins,
including every raw operation and parent. Canonical full-body hashes are bound
to raw file presence (`missing`, `valid_empty` or `nonempty`), swarm identity and
pre/post receipts. An absent file's zero bytes are not a valid empty JSON
component. The aggregate request is at most 32 MiB including every component;
preview, status and mutation receipts are at most 64 KiB. Disk read ceilings do
not widen wire limits. Journals and the registry are bounded and directly
indexed by original UUID; one adoption is active per catalog.

The destination keeps its own node and self bindings and gets no reverse peer
or source-local binding. Only the source adds the configured destination peer
and calls `sync_peer` for that peer, preserving existing peers and unrelated
synchronization. Without real reverse enrollment, drive-folder import remains
unsupported and the drive component stays `Uninitialized`; preview and
settlement disclose this rather than claiming every component is shared.

`hgs swarm adoption-status PEER --json` returns the bounded original phase and
receipts without repairing anything. Main, primary and twins commit separately
under the destination catalog lock, with write-ahead phases and fresh-read
receipts. The durable `adoption-pending.json` guard blocks capable ordinary
loaders and savers after every partial phase, including before directory/lock
creation. It is removed with a parent barrier only after all components verify;
`remote_committed` follows that removal. Ordinary loaders and savers also
refuse a bounded registry whose role/phase pairs are invalid, whose unfinished
request is not the active one, whose active entry is not in an active phase, or
whose active request has no matching durable journal (one direct read by UUID).
A crash or unknown result before the terminal receipt remains partial/unknown
and read-only. The lock-free organization read and its watch keep showing the
main catalog but add `adoption` (`state` `partial`, `pending` or `unknown`,
`ordinary_writes`, original `request_id`, `role` and `phase`) whenever a guard
exists or a request is active, and follow the guard and registry files; with
neither, the projection is unchanged. Older binaries are not
claimed to honor the guard.

Exactly two continuations exist, and only within the original live CLI call: a
durable original `refused_not_started` with zero product effects plus fresh
identical proof, or a terminal `remote_committed` with an explicitly unstarted
local source-membership phase. Both require the original native owner,
request/body, configuration, source preimages, per-UUID lease, and actual child
exit plus both physical pipe EOFs within the original 25-second deadline. A
receipt, idle output, byte count, or serialized drain claim is not drain proof.
A distinct or restarted CLI has only original-status and fully validated
committed-receipt lookup; it cannot continue mutation. A lost reply, changed
context, active or unknown writer, restarted owner, missing receipt, or stranded
guard is `Partial`/`Unknown` and status-only. No new UUID, remote replay after
an unknown result, duplicate cleanup, or third destination repair is permitted.
The native dialog binds the original to independently observed Live machine
cards for this computer and the destination (computer, host and machine UUID
equal to the preview node) and refuses a stopped hub. It rechecks those cards,
the original generation, configuration, ticket and resource custody at the
queued invocation barrier, at each preflight result, at the mutation's drained
result and before native settlement. A refusal is sticky: a restored card or
configuration never revives that original. Its later review action is
read-only, not another adoption call.

Each original remote call keeps the same ssh child (and its Windows job),
request writer and both pipe readers until they physically settle. After any
failure the child may exit by itself until the original deadline; then only
that owned child is stopped and its exit confirmed by wait. No process group,
descendant or name-based kill is used, and a PID whose status is no longer
trusted is never signalled. The CLI then waits for the writer and both pipe
EOFs before it reports the call as unknown, so a descendant that still holds a
pipe keeps the call, and its native accounting, retained. A call whose exit
cannot be confirmed stays pinned and admits no other call in that process. The
adoption ssh passes `-o ControlPath=none` ahead of the configured connection
options, so a user's `ControlMaster`/`ControlPersist` setting cannot start a
background mux master that inherits stderr. Its uncached remote-shell discovery
probe does the same. Every other remote operation's discovery probe keeps the
configured connection sharing unchanged. The desktop runs the call beside
its organization worker: other organization work continues, the dialog can be
closed and reopened onto the same original, and after the original deadline it
reports that the original is still settling with an unknown result. Only new
adoption actions wait, and no failure or timeout settles or replays it.

The desktop runs only its three fixed adoption calls (`adoption-preview`,
`adopt` and `adoption-status`) with the CLI as the leader of a private process
group it creates for that one call; its local reads keep the ordinary runner.
It observes the leader's exit with `waitid(WNOWAIT)`, so the unreaped leader pins
the group, and only then, or on its own timeout, sends `SIGKILL` to that exact
group and reaps the leader. It never signals after reaping, and a shared group,
a process scan or a name or path match is never an authority to signal. So when
anything kills the CLI, the original ssh and its in-group descendants (such as
a ProxyCommand) are stopped and reaped before the dialog settles. The accounted
resources stay held until the leader is reaped and both of its pipes reach EOF.
The result stays unknown and status-only, the ticket stays held, and nothing is
retried or continued. Windows keeps its per-child kill-on-close jobs.
Processes that leave the group (setsid or a process group of their own) stay
outside it and are never signalled by it. Two boundaries remain: if the
desktop has crashed, a later kill of the CLI is not accounted by anyone; and
after a desktop restart the dialog cannot reopen the original. It can still
review it read-only: when the organization read discloses an adoption request
and the dialog holds no original of its own, **Review original status** reads
this computer's journal for that request (`hgs swarm adoption-status --json`)
and, for a source original whose recorded peer is configured here, the
destination's status. Nothing creates a ticket, continues the original or
sends a mutation, and a mismatched request or an unconfigured peer refuses
before any remote call.

Adoption-specific raw JSON rejects duplicate object keys recursively before a
typed map is constructed. The source catalog and sidecars must match their
declared source and source-node machine operation. The original destination is
validated independently against its own empty preimage, whose swarm may differ
from the source. The complete main/primary/twins candidate, cross-component
closure, aggregate operations, parent edges and encoded wire size are checked
before any journal, guard or product effect. Primary counts come from the
membership view validated with the main catalog's machine redirects, so a valid
causal edge from an old machine key to its canonical child is accepted. Terminal and settlement receipts
must match the full original request and all component preimages and postimages;
a same-UUID or hash-shaped response is insufficient. `guard_cleared` is durable
before `remote_committed`; a crash between them remains manual/status-only.

### Phone notification timing

`hgs swarm preference get --json` prints `{"mobile_delivery": "away"}`;
`hgs swarm preference set mobile_delivery away_10 --json` changes it for every
member. Values are `immediate`, `away` (default), `away_5`, `away_10`, `away_15`
and `away_30`. Concurrent changes settle on one value without a review; the next
change from any computer or the phone replaces them. A machine joining a swarm
adopts the swarm's existing choice. Without a swarm the local catalog keeps it the
same way.

Members running an older hgs keep synchronizing projects, folders and sessions but
do not receive this choice or the shared project order (they keep their own
order); Settings → Mobile connection names the computers this
one synchronizes with directly until they are updated. A member reached only
through another computer is named in Settings on the computer that connects to it.
An older machine cannot preview or join a swarm whose catalog already contains the
choice; update it first.

An older member does not relay the choice either: a machine that synchronizes only
through it receives the choice after that member is updated. Each synchronization
with an older member exchanges the full inventory, because its catalog never
matches a catalog that holds the choice.

Once a delivery choice exists, an older hgs on the same machine cannot read the
swarm catalog, and leaves it untouched, until hgs is updated again.

### Legacy CLI folder aliases

`~/.config/hgs/projects.local` retains `NAME=FOLDER` aliases for CLI compatibility.
The old infrastructure-owned `~/.config/hgs/projects` is no longer read. Aliases
import into Zerus once per machine; later project edits belong in Projects. An
alias whose folder that machine cannot take (not an absolute path in its form)
starts no project. Projects names it, and the machine imports again at the next
start of Zerus.

```sh
hgs project ls --json
hgs project add infra ~/work/infra
hgs project set infra ~/work/other
hgs project rm infra
```

Alias names reject spaces, `/` and `=`; logical project names are not restricted
that way. Absolute paths remain available directly from the CLI.

### Cloning a repository

In desktop **New session**, choose **Working folder** for an existing checkout or
ordinary folder, or **Git repository** to prepare a new checkout. Project,
computer, agent, account and session name stay in this one form. The Git source
accepts an SSH or HTTPS URL from GitHub or another Git server. **Clone to** shows
the suggested complete destination; edit it to rename the target folder or use
**Browse…** to choose its parent. A manual destination stays when the URL changes.
Parent folders are remembered separately for each computer.
Switching sources keeps the shared controls and action buttons in place.

Select an existing logical project, Inbox, or **New project…**, then use the
single **Clone and start** action. Progress, access errors and recovery controls
move to a selectable creation card in Sessions as soon as the form closes.
Switching source modes or opening the form never starts a clone.
**Projects → Clone repository…** offers standalone preparation without starting
an agent.

**Clone and start** checks the selected computer's installed CLI, Git, parent
folder, destination and repository access before cloning. Access uses that
computer's existing SSH configuration and Git credential helpers; passwords and
tokens embedded in repository addresses are refused. Configure missing access on
that computer, then try again. These checks run in the background and create no
checkout. Cloning begins only after explicit confirmation. Existing destinations,
including empty folders and symlinks, are preserved: choose another name or use
**Browse…** to select an existing checkout.

A confirmed clone joins the selected project even if the agent cannot start.
Its confirmed project choice, including Inbox, takes precedence over automatic
folder-based project detection.
The normal **Auto** / **None** worktree choice applies to the new checkout. Failed
launches retain the checkout for another session. Ordinary folders remain
supported without Git initialization.

Cloning has a ten-minute Git deadline and never automatically retries an
uncertain operation. Private native receipts and local pending-request metadata
retain the original identity across dialog or application restarts. **Check
result** reads that original receipt; a recovered checkout requires explicit
**Continue setup** on its creation card. Standalone preparation retains its
**Use checkout** action. Failed or partial checkouts
are kept for inspection. A timeout or lost connection is not permission to remove
files or run the clone again. **Choose another destination…** leaves the original
checkout and native receipt in place and lets you prepare a different clone.

The CLI is `hgs [@host] repository check --url URL --parent PATH --name NAME
--json`, followed by `repository clone` with the same fields and an explicit
`--request-id UUID`. `hgs [@host] repository status --request-id UUID --json`
reads its durable receipt. Reusing a request ID with identical fields returns
the original result without repeating Git; different fields are refused.
Remote acquisition requires the exact selected CLI's `repository-clone-v1`
capability. Older peers continue to support ordinary folder selection.

### Session creation progress

The final **Start session** / **Clone and start** action immediately publishes a
selectable creation card and closes the form. An ordered background writer saves
the captured choices before any clone, worktree or agent starts. The initial
**Saving request…** stage is visible even while storage or the selected computer
is slow. A save failure keeps the card and dispatches no next step.

Select the card to see its current stage, elapsed time, bounded diagnostics and
retained checkout/branch. Stages distinguish folder and repository checks,
cloning, worktree creation, checkout verification, agent startup and confirmation
of the real session. After 30 seconds without a stage update, the detail view
explains that preparation is taking longer than usual. It never invents a
percentage. Failed and uncertain creations remain available for review.

Creation cards appear in All and Active, with separate creation counts and the
same project, machine, account, folder and text scopes. The newly submitted card
is temporarily revealed without changing those scopes; **Return to filters**
ends that reveal. The clock button opens bounded pages of creation history,
including hidden cards. **Hide status** changes presentation only. Moving to
another conversation leaves preparation running and preserves its draft.

**Stop creating** fences later preparation and launch. Modern worktree receipts
can cancel only their own Git process group/job. Clone operations and older
worktree helpers offer **Stop after preparation**. Files and branches are always
kept. Once launch dispatch is reserved, check its result; use normal confirmed
session termination after an actual session appears.

When a worktree admission is refused, the card shows that refusal and its
reason rather than treating it as a transport failure. A fresh attempt may
reuse the creation's scheduling slot only after its exact predecessor failed
without effects and all original resources drained; the old receipt remains
available under its original request ID.

For an older missing worktree request, **Stop creating** can durably fence
further starts while leaving the earlier outcome unknown. The card says both
facts, retains the original request and destination for review, and never offers
**Continue setup** for that UUID. **Recreate…** opens separately reviewed choices
for a new creation and new request; **Hide status** dismisses the card without
erasing its retained result. Missing, replaced or unverifiable receipt storage
still refuses a fence. A fence alone never proves that an earlier worker,
checkout or native resource has settled.

**Check result** observes the original clone/worktree request, reads the original
service launch receipt, or refreshes session evidence. An unknown outcome never
permits another creation command. After a
restart, unfinished cards require explicit **Continue setup**; recovered
preparation is revalidated before startup. A definitely ended startup can retry
only startup with a new launch identity. **Edit choices** opens a reviewed new
request while preserving the original attempt and resources.

A failed repository verification after a successful worktree receipt keeps the
already-created checkout, branch and original receipt. Its message distinguishes
that read failure from creation failure. One bounded private
`last_catalog_failure` records the read stage, sanitized state/detail and typed
staleness, completeness and sample time (null means unavailable), and remains
after later continuation. A catalog timeout does not establish repository
corruption. **Continue setup** revalidates the retained checkout and can then
start the session; it is not a read-only check and never repeats an unknown
original effect.

A matching service receipt with a fully drained, unsuccessful launch-command
exit shows **Creation failed** and its reason. Its dispatched launch identity
is kept: **Continue setup** never repeats it. **Edit choices** opens a separate,
reviewed creation with a new identity. Missing, unreadable, mismatched or Unknown
receipts stay **Creation result unknown**, with **Check result** and **Hide status**.
Failure and unknown-result notices close on the normal toast timer; their full
details remain on the creation card. A failure first read after opening the
window stays quiet only if its last recorded state change predates this window's
start, even when the creation itself is much older. Older cards without that
timestamp use their creation time.

Only the exact native launch identity, computer, name and agent replace a card
with a real session. Command success alone remains **Confirming session…**; a
late transport error cannot downgrade a proven session. Native early-exit
evidence keeps its reason and bounded output in the creation card. Project
placement still follows its existing acquisition/launch success boundary.

When supported by the selected CLI, `dirs --creation` confirms its stable
computer UUID. Subsequent preparation and launch commands use
`--expect-machine UUID`; an SSH alias that now points to another computer fails
before mutation. Older helpers keep one-shot preparation with conservative
recovery. Creation has at most four active preparations and 32 unfinished
requests; hidden resolved history is pruned locally, never unresolved requests.
The private versioned journal is bounded to 128 records / 8 MiB. API v1 and mobile
action journals retain their existing contracts.

The native operation pins the parent and new destination through Git and checks
their identities before accepting the result. Unix Git starts in the held
destination directory, so replacing its pathname cannot redirect checkout writes.
An unpredictable private staging folder is pinned before atomic publication of
the destination name; its fallback `staging_path` is reserved in the receipt before
creation. A failed publication can leave that staging folder for inspection.
Windows creates and holds the new directory atomically and retains its ancestors
without write/delete sharing; a conflicting handle or reparse point fails closed.
Zerus never removes a replaced or partially populated directory.

On Unix, a separate native worker owns Git's deadline and watches a wrapper-owned
pipe. Ending the GUI/SSH wrapper closes that pipe and cancels the worker's own Git
process group, including its descendants. The original receipt remains
unconfirmed after abrupt wrapper termination; status and duplicate requests never
restart the clone. Windows retains the existing kill-on-close job containment.
The complete encoded receipt is limited to 16 KiB; large Git errors are truncated
within that envelope, and status and duplicate requests return the same saved
result. Local paths, including Windows drive, UNC and device paths, are refused.
A single-letter SSH alias must use an explicit address such as `ssh://c/repo`.

### Worktrees and ordinary folders

New session keeps **Auto** available when the selected computer confirms the
working checkout, even if the repository has more checkouts than its bounded
catalog can list. Missing, bare, stale or unconfirmed folders remain ineligible.
Opening the form or choosing a folder creates no worktree; creation starts only
after **Start session**.

Session cards show a checkout line of the form `folder / branch`, such as
`zerus / work-1a2b`, preceded by a small Git branch icon; **detached HEAD**
replaces a missing branch. A linked worktree names its main checkout folder
rather than its own generated folder. Ordinary folders without Git metadata
show only their name, preceded by a folder icon. The opened session's header and
**Details** show the same line, marked `· worktree` for a linked worktree; the
full folder path stays in the header tooltip and **Details**.

A running Codex session with pending optional questions, ones the agent keeps
working without, shows an amber question-mark chip with their count (`99+`
above 99) right after its status badge, or after a separate new-reply badge, on
its Sessions and Overview cards; a collapsed strip tile shows a small amber `?`
mark. Its tooltip and the card's accessible description say how many
are waiting. The chip shares the amber of attention but never changes the
session's status, the
Needs attention filter, counters or badges, and it is hidden for paused,
stopped, archived or offline sessions and while the count is unknown. Open the
session to answer or skip the question in Activity.

Existing Git worktrees appear in Projects and the session inspector. Clicking a
checkout filters Sessions while preserving the open conversation and draft.
In **New session**, **Auto** creates a generated `work-…` branch from `HEAD` and a
sibling worktree when **Start session** is pressed, then starts there. Its path is
shown before launch. Opening the form and checking folders never create anything.
**None** starts directly in the selected working folder; ordinary non-Git folders
use this mode and never initialize Git. The adjacent options menu offers
**Choose existing…** and **Create manually…**. Choosing an existing checkout keeps
its path, machine, project and account. Explicit worktree launch actions also use
that checkout directly. Bare or unavailable copies cannot launch; ordinary Browse
remains available even without Git or after metadata errors. The project gear
opens folder configuration, and each agent choice includes its provider icon.
The folder status area stays empty unless it has a warning or an error.

**Create manually…** in this form, or **New worktree…** in Projects, lets you
choose a new branch, base ref
(default `HEAD`) and unused destination under an existing parent directory. The
CLI is `hgs [@host] worktrees create --path /repo --branch feature/task
--destination /repo-task --base HEAD --json`. Zerus also verifies `--common-dir`
and a request ID. Creation uses normal `git worktree add -b`, including configured
checkout hooks, with no force, fetch or deletion. The ref is resolved before
creation. When the base is a local branch with a remote upstream, such as `HEAD`
on `main` tracking `origin/main`, the new branch inherits that upstream so its
session card can verify publication; Git's default `simple` push mode still
refuses a bare `git push` to the differently named branch. Commits, tags,
detached `HEAD`, local upstreams and `push.default=upstream` or `tracking` leave
the new branch without an upstream. The five-minute operation can leave partial results after failure;
inspect the destination before retrying. Stopping session preparation preserves
the created folder and branch. Closing or archiving a session never removes them.
Automatic creation requires a fresh verified repository on the selected computer.
The creation card starts the agent only after its matching result and fresh
checkout checks, and never repeats an unconfirmed request. Its destination remains
available for inspection or an explicit direct-folder launch.

The opt-in `worktrees create --receipt --request-id UUID` contract reserves a
private original request before Git. Identical duplicates return that result;
different arguments refuse. `worktrees status --request-id UUID --json` reads it,
including fresh ownership verification for a created checkout. An interrupted
reservation becomes unconfirmed and is never replayed. `worktrees cancel
--request-id UUID --json` requests shutdown of that operation alone; a running
receipt is not proof of exit. A final failed receipt with `effects_started:false`
permits an explicit new attempt after checks. Failures after checkout begins keep
the request and resources for inspection. Existing calls without `--receipt`
and mobile API v1 remain compatible.

Catalog reads are on demand, with a 30-second repository cache; `--refresh`
bypasses it. Identity is machine plus canonical Git common directory, not equal
paths or repository names. Limits are three seconds, 1 MiB of Git output and
512 worktrees. Errors distinguish `not_repo`, `git_unavailable`, `metadata_error`,
`timeout`, `too_large` and `folder_unavailable`; previous data is explicitly stale.
The UI caches at most 64 snapshots and 8 MiB. Cleanup is described in
[the cleanup design](superpowers/specs/2026-10-10-worktree-cleanup-design.md).

### Cleaning up worktrees

**Worktrees…** in the Projects heading opens a cleanup workspace across visible
projects and computers. Repositories linked from several projects or subfolders
appear once per computer; filters preserve exact project and machine identities.
Four groups separate **Cleanup candidates**, **Review work**, **In use** and
**Not checked**. Main checkouts and ordinary folders are hidden by default.
Search, sort and refresh share one compact toolbar. **Filters** opens project,
computer and state controls and the main-checkout toggle; its count shows active
filters without taking height from the workspace. Folder names are emphasized,
while a separate muted row with a Git branch icon identifies the branch.
For a single-computer scope, the coverage line names that computer and the
redundant computer column is hidden.
Search matches folder, branch, project and related session names; sorting offers
largest folders, oldest Git activity or folder names. Sizes are apparent file
sizes, not a promise of physical disk space reclaimed; incomplete measurements
are marked as lower bounds.

Catalog reads run two at a time, and repository reviews run sequentially in the
background. Coverage reports checked/known worktrees and incomplete catalogs.
After the first bounded repository review, unchecked known rows are checked
individually, including rows beyond the native 64-row review limit. Catalogs
remain bounded at 512 entries; this is not a scan of every folder on a computer.
Offline snapshots remain readable and selectable for review; removal stays
unavailable until the owning computer supplies a fresh check.
Browsing data is retained for one hour, across closing and reopening the manager.
The coverage line shows when catalogs were updated; **Refresh** rereads them and
clears the selected cleanup candidates. The open manager refreshes hourly, and
worktree creation/removal invalidates the affected computer/repository cache.
External Git changes can be picked up with **Refresh**. **Select candidates** uses
checks less than five minutes old. Selecting rows is independent of deletion
eligibility: Ctrl/Cmd-click, Shift-click and checkboxes select multiple folders in
any group. **Review cleanup** always checks the selected targets afresh; native
removal repeats those checks under the repository and session locks. A cached
browsing verdict never authorizes deletion.

Selecting a folder loads its files and commits after a short navigation debounce.
The inspector distinguishes loading, an empty list and a failed read with retry.
Its refresh button rereads the selected folder, including cached file previews.
Changed/untracked file names, tracked diffs and up to
30 original commits absent from the base history are available. Inspection and
file previews are cached in bounded application memory for an hour, never written
to the settings file. The base comparison is a
two-way diff and can include later changes on the base. **Local files** previews
ignored entries and their contents; **Sessions** opens the exact related
conversation on its owning computer. Reads are bounded (200 changed files or
directory entries per page and 128 KiB per text preview), with truncation shown. **Browse all files** and **Next** continue beyond
the largest ignored entries or the first directory page. Binary
files are identified, and symlinks are not followed. Git refs are local snapshots;
inspection never fetches. No-base and failed checks are reported explicitly.

The inspector keeps its longer metadata under **Details**. Local file names use
the available column width; browsing and pagination share one small navigation
row. The empty text preview stays hidden until a file is opened, and both the
file list and the opened preview keep usable, resizable space. Related sessions
have session icons and retain their exact conversation targets.

**Select candidates** selects only currently visible, verified cleanup candidates.
Folders with local files outside Git require individual selection, even when
Git reports them clean and merged. **Review cleanup** shows the exact selected
folders across repositories and computers, branch handling and why any folder
must stay. Folders outside the candidates group can be selected and removed too.
Local data needs a separate explicit checkbox; deletion checks its reviewed
metadata fingerprint again. A missing folder only loses its Git registration.
After complete success the dialog closes and the overview shows the result.
Partial results and errors remain open; uncertain replies are not replayed. **Stop after this folder**
lets the current operation finish and leaves the remaining folders untouched.

Folders a removal left behind (see partial removal below) appear first, in a
separate **Partially removed** group shown only when there are any. A repository
with partial-removal records is reviewed even when no linked worktree remains,
and each computer's vanished-repository leftovers come from one bounded
`hgs worktrees leftovers` call; there is no per-row call. These rows never join a
cleanup selection. The panel shows the remaining size (a lower bound when the
listing was incomplete), whether the branch is kept, moved or unchecked, why the
removal stopped, and the sessions and programs that use the folder. It offers no
file browsing. **Keep folder…** asks for a simple confirmation and forgets only
the record. **Delete remaining files…** opens an explicit confirmation whose
default focus is Cancel: it states that the files cannot be restored and that
the branch is kept, and shows the unchecked **Delete even if some running
programs cannot be checked** box only when the review offered that acceptance.
Each confirmation sends the fingerprint of the review on screen exactly once.
The outcome stays on the row until a fresh review: **Files deleted**, **Folder
kept**, **Record already gone** (another action may have resolved it, so the
row asks for a fresh review instead of claiming the files stayed), **Nothing
changed** with the reason, or **Result unknown** with **Review again**. Nothing is retried automatically. A row whose repository is gone shows
**Repository missing** and offers Keep folder only.

Integration evidence distinguishes ancestor commits, identical file trees and
cherry-pick-equivalent patches. Equivalent content is useful when reviewing
squash/cherry-pick workflows, but only Git ancestry permits optional branch
removal. Original branches otherwise remain available.

Manual removal is explicit and is reviewed on the computer that owns the
checkout. **Clean up…** in Projects or the session inspector lists the linked
worktrees of the repository as **Ready** (preselected), **Review** (removable but
with notes such as local ignored files, unmerged commits, archived sessions or activity in the last
24 hours), **Missing** (the folder is gone; only Git's record is forgotten) or
**Blocked**. The button shows the number of ready and missing worktrees; that
review runs at most every five minutes while the panel is visible and never
removes anything. A worktree row's menu offers **Remove worktree…** or
**Forget missing worktree…** for that checkout alone.

A worktree is protected when it is the main checkout or bare, is locked,
contains another worktree or Git repository, has initialized
submodules, has a detached commit that no branch, tag or remote branch contains,
or is used by a process of this user or by a running, paused or saved Zerus
session (archive the sessions first). Git's own removal would silently delete a
nested worktree inside an ignored folder; Zerus refuses it. An incomplete local
file inventory, unreadable session bindings or special files also block removal.
Uncommitted and untracked files require a separate **Permanently discard**
confirmation in the manager. That consent is bound to the exact file contents
and staged Git blobs (bounded to 64 MiB and two seconds), and does not override
any other protection. A changed or incomplete proof requires a new review.

Linux can deny access to a protected process's current folder or executable,
including desktop services. A complete inventory containing only access denials
allows explicit manual acceptance: **Remove even if some running programs cannot
be checked**. This accepts uncertainty; it does not prove those programs are
unrelated. The acceptance lasts five minutes on the owning computer and is bound
to the exact checkout review, repository and ownership. Unrelated protected
processes starting, exiting or changing PID do not invalidate it. Removal still
requires a fresh complete inventory and refuses known programs or sessions using
the folder, changed files or ownership, and all other protections. An incomplete
inventory remains blocked. Automatic cleanup never uses this exception. Process
names, PIDs and access details remain available under **Details**.

The original native **Terminate** with **Remove working folder** also ignores
actual permission-denied cwd or open-file reads after that exact termination
succeeds, for its verified generated checkout and unchanged launch ownership.
Those unreadable programs are neither listed nor signalled by that cleanup.
Linux and macOS inspect open files as well as current folders; a readable path
inside the checkout still keeps it, even if another read on the same process is
denied. The owning CLI repeats the complete bounded classification before
removal. Other errors, incomplete coverage, changed identities or ownership,
known use, sessions and file protections still refuse removal. Generic background
maintenance remains conservative; retained results never reconstruct this
original confirmation or retry an unknown removal.

For a local Linux or macOS native confirmation, a selected CLI that supports
`worktree-program-stop-v1` and `worktree-program-cleanup-v1` can also stop the
programs proven to use that generated checkout. Before **Stop and remove folder**
is confirmed, the dialog shows a count and warning for other programs, with an
expandable list, including editors and terminals the person opened there. Only
the selected session's own agent and wrapper can be omitted from this initial
display, when their exact record binding, native birth identities and direct
parent relationship remain verified across discovery; uncertain rows stay visible.
This presentation never filters the complete native execution manifest. The
separate Force confirmation always shows its complete freshly checked survivor
list expanded. Only the original process identities are eligible: immediately before each signal, Zerus checks
their native birth/pin and current cwd or open-file membership in the same held
checkout. Newly discovered or replaced programs require fresh review. Programs
needed to verify and settle the operation, and their protected ancestors, are
never signalled. Pause, native Stop, manual cleanup and background maintenance
retain their existing scope.

These native helpers resolve the existing configured session-state root once
at command configuration load, including aliases selected through `HGS_STATE_DIR`,
`XDG_STATE_HOME`, or a symlinked home or `~/.local`. All phase reads use that
same physical root. The configuration identity is unchanged; registrations and
their original argv keep their existing spelling. Journal descendants and the
checkout walk still refuse symlinks. A missing, unreadable or non-directory
state root is refused with its configured path; inspection does not create it
or choose a fallback profile.

The original Terminate UUID and exact successful archive receipt precede a
separate, durably reserved program-stop UUID. Normal stop sends TERM with a
15-second phase bound and never escalates automatically. If process metadata
is temporarily unavailable after signalling, read-only exit observation stays
within that original phase; only a later exact native pin observation establishes
exit. Uncertainty at the bound remains Unknown. If verified original programs
survive, **Force stop…** opens a new native confirmation listing the
currently surviving originals, with a new UUID. It becomes available only after
the previous mutating helper, descendants, pipes and loans physically settle.
Missing or uncertain outcomes cannot create a Force offer. The original selected
CLI, configuration, environment, Actions claim and checkout/ancestor handles
remain held across both confirmations, within one original five-minute custody
bound. Neither a deadline nor a returned status proves drain.

After a known Complete stop and actual mutation retirement, that same live
Remove continuation may open one final cleanup phase, with its own UUID. It
reuses the guarded cleanup engine: fresh file, ownership, session and known-use
checks still apply; the branch and archive restore identity stay. Only the
original read-only holder's one proven root descriptor is omitted from process
use accounting. Another descriptor, cwd use or any other process still counts.
The holder remains charged independently; it has no stop or deletion verb.
Cleanup returns Removed, Kept or Unknown once. A lost or partial deletion stays
Unknown, with original-ID status lookup only; a restart, reconnect or a new UUID
cannot reconstruct the continuation or repeat it. The new fixed stdio helper
commands accept only their current canonical native parent's original-child
proof, never renderer, manual or background requests. If the selected route
cannot support this pipeline, removal remains available through the existing
separately guarded folder review rather than an alternate CLI selection.

A checkout with uncommitted or untracked files can also have such access
denials. Its review then offers both consents (`discardable_with_process_consent`
beside the discard and process fingerprints, while `discardable` stays false for
older clients). Removal needs both: **Permanently discard** and **Remove even if
some running programs cannot be checked**. Each consent alone is refused, and
every other protection still wins.

A refusal before deletion shows **Folder kept**. **Check again** refreshes only
remaining folders and clears the prior local-file, discard and process choices;
removal needs a new explicit confirmation. File inspection remains available.
Successfully removed targets are never resubmitted. A lost response, an older CLI
without a verified outcome, or an error after Git may have started shows **Result
unknown** and never enables another removal in that operation. Files can still be
inspected. The CLI preserves nonzero failure exits and stderr while adding a
request-bound JSON outcome for clients that request `--json`.

Git stops tracking a worktree before it deletes the files, so a removal can fail
halfway, for example on a readonly ignored folder: Git forgets the worktree while
some files remain. Before every `git worktree remove`, the owning computer
reserves a private removal record. A confirmed outcome drops it, and an
interruption or failure keeps it. A folder that Git no longer tracks but that
still exists then appears in the repository review (and in a review targeted at
that worktree, such as the cleanup after Terminate) as a **leftover**
(`worktree-leftovers-v1`), with a fresh bounded listing of the remaining files,
the retained branch and current processes or sessions. Size, manifest and deletion
blockers share one no-follow walk, bounded by two seconds and 200,000 entries,
including queued children. Unreadable or protected subtrees and exhausted bounds
leave `size_complete` false and refuse deletion. The cleanup after
Terminate keeps such a folder with "Git no longer tracks this folder, but files
remain" instead of an unverified-folder error. A targeted review matches the
worktree's whole path, as given or resolved through links, never a prefix. The
targeted reviews behind `hgs worktrees inspect` and `remove` therefore also
resolve that worktree's own record under the rules below, so inspection, otherwise
read-only, may drop a record whose folder is gone or that Git tracks again. A missing Git
registration is never treated as proof of deletion, and the removal is never
retried. **Keep folder, forget record** (`hgs worktrees keep-folder`) checks the
reviewed leftover again under the repository lock and drops only its record;
the folder, its files and the branch stay. With `--json`, a failure prints a
request-bound outcome (`worktree-leftover-keep-v1`): `kept` before the record is
touched, `unconfirmed` once forgetting it may have started. A client reports
**Nothing changed** only for `kept`; an unreachable computer, a crash or any
other reply is **Result unknown**. **Delete remaining files**
(`hgs worktrees delete-leftover`, `worktree-leftover-delete-v1`) is disabled on
macOS pending a verified native guard: XNU may unlink ordinary `._*` files while
retrying directory removal after an unreviewed folder replaces the checked name.
A fresh `._*` listing cannot close that race. Review and **Keep folder** remain
available. The desktop explains the refusal and offers **Open in Finder** for a
local folder; for a remote Mac, open the displayed folder in Finder on that Mac.
The CLI refuses before changing permissions, unlinking files or reserving an
attempt and returns the existing request-bound `kept` / outcome-version 1 reply.
The shared directory-removal wrapper also refuses both nested and root removals
on macOS. A future authorized native window must run the ignored
`state::worktree_cleanup::tests::macos_late_swap_preserves_appledouble_payload_at_root_and_nested_names`
probe with `--exact --ignored` before re-enablement is considered.

On Linux, deleting the remaining files needs an
explicit confirmation and the fingerprint of the fresh review the owner saw. It
checks everything again under the repository and session locks and refuses
saved or running sessions, other running programs, nested Git repositories,
special files, folders on another filesystem or owned by another user, and an
incomplete listing. A failed saved-session inventory (including an unreadable or
malformed binding) is an explicit blocker, never evidence that no session uses
the folder. The process-only manual exception cannot override it.
The narrow manual exception for programs whose folders
cannot be read works as it does for worktree removal: it is bound to the exact
leftover path and its fresh-review fingerprint and expires after five minutes,
the known-use, session, ownership and file protections still apply, and
automatic cleanup never uses it. Deletion never follows links and never leaves
the reviewed mount. Each folder is opened without following links and checked
through its open descriptor for owner, device and mount (the Linux mount ID, or
the macOS filesystem ID and mount point), so a link, a mount or a same-device bind
mount that appears after the review stops the deletion before anything in it
changes. A filesystem whose mount cannot be identified is refused. The reviewed
folder itself is part of the confirmed fingerprint (device, inode and mount): it
is reopened through its parent without following links (folders above it need
only search permission: Linux opens them as paths and macOS for search), and
completion is confirmed through that parent. The reviewed root stays open and
its name is checked against that inode again immediately before final removal.
This rejects an observable replacement and prevents reuse of its inode number.
The check and removal are separate: neither [Linux `unlinkat`](https://man7.org/linux/man-pages/man2/unlink.2.html)
nor [macOS `unlinkat`](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/vfs/vfs_syscalls.c)
provides an expected-inode argument or directory removal through the open root
itself. An empty directory swapped into the name between the check and removal
can still be removed. No userspace traversal enters that replacement. On Linux,
directory removal refuses a nonempty replacement. On macOS, [XNU may clean up AppleDouble entries and retry directory removal](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/vfs/vfs_syscalls.c#L10670)
after `ENOTEMPTY`: its cleanup
can unlink ordinary files whose names start with `._`, such as `._payload`.
A late macOS replacement therefore has no empty-only guarantee; its unreviewed
`._*` files may be removed by the kernel. Renaming the path into quarantine
would instead move an unreviewed folder and cannot guarantee race-free
restoration, so it does not supply the missing inode-bound primitive. Only after those checks does deletion grant the
owner write access to their own folders inside the reviewed tree, which is what
blocked Git; it never touches the branch. If it
stops partway, the record stays with the reason and the deletion is never
retried; review the folder again. Each confirmation is consumed by a durable,
unique attempt reference before filesystem changes begin. If that reference
cannot be saved, deletion refuses without changing files. Even if no file
changed and saving the stopped outcome fails, the old confirmation is no longer
valid; another attempt needs a fresh review. Deleting leftover files is currently
available on Linux only. A record resolves by itself once its
folder is gone, or once a fresh worktree list taken under the repository lock shows
that Git tracks the worktree again. On Linux and macOS the Git that removes a
worktree inherits the repository lock (an flock belongs to the open file, and only
that Git receives it), and hgs releases the lock by closing it, never by an explicit
unlock. The lock therefore stays taken while that Git, or anything it started, still
runs, even after hgs is killed, and no review resolves the record in that time. On
Windows the lock ends with hgs. If worktree operations keep reporting that another
one is running (for example because a custom `git` wrapper left a helper behind),
`fuser -v` or `lsof` on the repository's `worktree-cache/repo-*.lock` file in the
hgs state folder shows which process still holds it. A leftover whose repository is gone is listed
by `hgs worktrees leftovers` and can still be kept; a record whose repository and
folder are both gone resolves.

A missing folder or repository counts as gone only on the filesystem that held it.
Each reservation records that filesystem in a separate private table that older
CLIs never read: on Linux the device and the `statfs` filesystem ID (derived from
the filesystem UUID on ext4, btrfs and xfs), on macOS the filesystem ID and mount
point. A missing path is gone only when the nearest existing folder above it is on
that same filesystem. A volume that is unmounted, replaced by another one at the
same place, or that cannot be identified (for example a zero filesystem ID, as on
some tmpfs and network filesystems) leaves the record uncertain: it stays visible
and can be kept by hand, and it never resolves by itself. So does a folder or
repository that was already missing when its removal was reserved, since no
filesystem could be recorded for it. On btrfs the device is a per-subvolume number that can change
across remounts and reboots; such records also stay uncertain until kept. A
block-level clone of a filesystem shares its ID, but mounted at the same place it
normally has another device, so it is not taken for the original. A bind mount
whose source is on the same filesystem as the folder above it is the exception:
once it is unmounted, the missing path is on that same filesystem and counts as
gone. Records written by an older CLI have no volume row and keep the plain
missing-path check, as does every record on Windows. One review or listing reports at most 32 leftovers
within 1 MiB and marks the rest with `leftovers_partial` (review) or `partial`
(listing); they stay recorded for a later review. Verified resolutions of vanished
records stay committed even if the remaining unresolved records still prevent
a new removal at the 512-record bound. The folder catalog
(`hgs worktrees --path`) reports a repository's record count as `leftovers`, read
through its index on every call, so a reader reviews that repository even when no
linked worktree is left. At most 512 unresolved records
are kept per computer; at that bound new removals are refused with a visible
reason, and no record is evicted. Automatic cleanup never acts on these records.

Confirmation lists the folders, the ignored data deleted with them (such as build
output) and any branches that will be deleted. Each removal then repeats the
review under the repository lock used by creation, compares the reviewed
fingerprint and runs `git worktree remove`, one worktree at a time. It uses
`--force` only for that explicit content-bound discard, never automatically.
**Also delete merged branches** (off by default, remembered locally) uses
`git branch -d` only for branches merged into the main checkout's branch or
`origin/HEAD`; other branches are kept. A failure is reported for that worktree
and never retried automatically.

After **Terminate**, explicit **Move to archive**, or a confirmed clean native
exit, the owning CLI automatically removes only verified Zerus-created
checkouts that are clean, have no local ignored data, have no remaining session
or process, and are proven integrated by ancestry, identical tree or equivalent
patches. It never fetches or deletes the branch. Ownership requires both a
private creation receipt and the matching nonce in Git's per-worktree metadata;
older or externally created worktrees remain manual. Folder/branch naming is
not ownership evidence. Automatic cleanup has bounded checks and never changes
a successful archive into a failed termination. A lost removal acknowledgement
is reported as unconfirmed, with no automatic replay.

**Settings > Worktrees** controls cleanup on the selected computer across all
its projects. Each computer keeps its own policy. The master switch and the
three independent rules default to on:

- Clean an integrated managed worktree after its last session is archived or
  terminated.
- Periodically clean unused integrated managed worktrees after **24 hours** of
  Git inactivity by default (configurable from 1 to 8760 hours). This also covers
  work integrated after archival and worktrees created without a session.
- Forget a missing managed checkout only after verifying its retained Git
  registration, ownership nonce and absence of session references.

Periodic maintenance runs with the existing host recovery service, independently
of whether automatic provider-error recovery is enabled or the GUI is open. It
imports at most 64 old ownership receipts and checks at most one queued worktree
per 30-second round. An indexed private queue schedules retained worktrees for a
later hourly check; it does not rescan project disks or journals. Archive metadata
checks verify compact reference metadata against the complete native archive-file
roster and exact file versions, at most 5000 entries and 32 MiB within their
deadline. Ordinary archive catalog migration/reconciliation builds this metadata;
archive writes, rewrites and removals maintain it. Its ordinary reconciliation
stamp is separate from the search stamp, so an older CLI's search refresh cannot
hide pending reference maintenance. Warm pages still use only these stamps.
Missing, stale or corrupt coverage keeps the folder; cleanup never rebuilds it
or interprets an omitted row as no reference.
Before deletion, an indexed checkout query reads the relevant authoritative
archive bodies within the same bounds and durably prepares Restore for every
compatible reference, including older archives without an ownership field.
Unrelated archive bodies and conversation journals do not consume this check's
byte budget. Reference coverage is independent of search/label filtering, and an
interrupted initial migration cannot publish complete coverage. Branch, head and
ownership checks remain independent. Old removed receipts do not repopulate the
queue once their Git metadata is gone.

Reference reads bind a regular native catalog object before SQLite runs; its
private read-only reader never reopens mutable paths or accesses SQLite sidecars.
Unexpected sidecars or replacement refuse coverage within the original budget.
Archive write hooks publish bounded per-ID native metadata updates without
opening SQLite. Reads verify those updates against the current archive objects;
ordinary catalog reconciliation absorbs them. Missing or corrupt updates never
substitute for a stale base row.

All automatic rules use fresh native checks under the repository/session locks.
They preserve branches, changed/untracked files and **all ignored local data**,
including build caches. Running executables inside the checkout protect it even
when their current directory is elsewhere. An incomplete or inaccessible process
inventory also keeps the checkout. These checks cover the owning user's processes;
Windows currently retains automatic candidates because process folders cannot be
verified there. Cleanup reserves its removal intent
durably before invoking Git. An interrupted or unconfirmed attempt stays for
manual review across worker restarts; a periodic sweep cannot replay an uncertain
archive-triggered deletion. External or old worktrees without matching ownership
receipts require explicit review in Projects. Recent activity or a folder/branch
name alone never establishes that a worktree is disposable.

Archive cleanup reports also carry a versioned exact archive identity and the
launch-ownership verdict. When it fits the bounded report, `review` retains the
structured row from the original removal check, including the total process
count before sample limits. Missing rows stay missing. This evidence supports
counts-only notices and retained review; displaying it never starts another
check or removal. Restore still requires the existing fresh exact-archive guards.

Read the host policy with `hgs [@host] worktrees policy get --json`; save the full
reviewed policy from JSON stdin with `worktrees policy set --json`. The revision
must still match. Existing policy files and writes require every versioned field;
incomplete or invalid policies keep folders instead of enabling defaults.
`hgs [@host] worktrees sweep --json` performs one bounded automatic
step and respects the same policy and protections. Reads never start a sweep;
creation, removal, sweep and policy writes reject `--dry-run`.

The desktop reports successful automatic cleanup, or keeps **Folder kept:
<folder>** after Terminate/Archive (**Cleanup unconfirmed:** when the outcome is
unconfirmed). This global reminder names the retained checkout; its tooltip and
accessible description identify the source session, computer, full folder and
reason. It can remain while another session is selected and does not expire
after a minute. The action is hidden while another known session or a new run
uses the folder. Review still checks the current files and requires explicit
confirmation before removal. Archives keep the exact branch/head needed to
recreate an automatically removed checkout on explicit **Restore**; a changed
branch or occupied destination is refused. Browsing and dry runs never recreate
a folder. Plain Pause never cleans up.

Desktop **Terminate** also offers **Remove the unused worktree after stopping**,
including for a selection of sessions. The choice is remembered on this desktop.
The choice is bound to the checkout ownership recorded at that session's launch.
A replacement checkout at the same path or branch is kept for manual review;
older sessions without a launch ownership record also require review.
For a retained **Not created by Zerus** linked worktree, **Remove anyway…**
opens a separate explicit manual confirmation. It shows the native-selected path,
computer, kept branch, size, changed/untracked counts and local data, and requires
its matching discard consents. Fresh session and program checks still apply;
known occupants must stop first (use the existing Force flow when offered).
Unreadable program paths retain the existing five-minute manual acceptance.
This action never removes the primary checkout or an ordinary folder, never
makes an unowned folder eligible for automatic cleanup, and never retries an
unknown removal. The branch and archive identities remain; recreating an unowned
working folder for Restore remains an explicit manual step. Fresh
removal fingerprints include the ownership nonce, so replacing a checkout after
review cannot inherit its removal consent.

On Linux and macOS, `worktrees programs --json` provides read-only native
discovery for an original live session and its verified launch-owned checkout.
Its closed stdin object contains `capture`; the bounded reply binds the exact
capture, configuration, boot and checkout directory object to the complete list
of observed program PIDs and native birth identities. This is data, never a stop
permit. The command sends no signals and changes no cleanup or Force authority.
It refuses an incomplete or changing census, changed ownership, ambiguous hard
links, nested mounts, or a custodian using the checkout. The discovery budget is
five seconds, 64 programs, 65,536 filesystem objects and 8 MiB of retained relative
names; the wire reply is capped at 64 KiB and expires after five minutes. Actual
permission denials are omitted only from this explicit-Remove discovery; a
readable cwd or open file still establishes membership. The existing generic
worktree review and background cleanup keep their conservative process policy.

On macOS these reads include each thread's own working directory, not just
the process directory. The native thread-path API selects by user TSD value
(`cthread_self`), not a stable thread ID, and returns the first match. Every
unique value, including zero, is read. Any repeated value, zero or nonzero,
leaves the observation unsettled: at most four attempts within the existing
deadline, then incomplete coverage, never an empty census. Raw Mach threads
can share zero while holding different working directories; the thread-ID64
API supplies no path to resolve that ambiguity. No TSD mutation, suspension
or elevated task port is used. The accepted residual is a live thread changing
its TSD between listing and read: an ESRCH can be taken as an exited thread,
or a reused value can address a different thread. Process birth checks do not
close that per-thread window. This follows the pinned [XNU thread-path
implementation](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/kern/proc_info.c)
and the coordinator's zerus-q0cd5 addressability ruling. Private fixture PID
tests qualify their own processes only; a full-host diagnostic reporting a
limit remains incomplete and does not qualify full-host Remove.
Native vnode evidence is retained independently of its pathname: XNU may
return a filled vnode while path reconstruction fails. Exact object membership
still applies, while the generic sampler refuses an empty or relative path.
A path without interpretable native vnode metadata is unsettled. The native
zeroed reported-no-vnode response is accepted only as that observation: XNU
uses it both for no own cwd and for a failed vnode acquisition during churn,
which the API cannot distinguish. This additional kernel-acquisition residual
does not prove perpetual absence or exempt aliased thread handles.

After the original confirmation closes, cleanup runs in the background only
following exact successful termination. A compact workspace notice shows progress,
then **Working folder removed**, **Working folder kept** with a short reason, or
an unknown result. **Check again**, **View files**, **Details** and **Keep** provide
explicit next actions. Review opens only when requested and does not block the
rest of Zerus. Pending work and retained results are bounded to 64 folders, and
background batches run sequentially. Closing the review does not cancel an
accepted removal. The notice remains visible through the confirmed action and
the actual folder-checking/removal phases. Result notices use the normal notice
timeout; dismissing one does not discard the retained result. The status bar's
working-folder entry still opens its review. While a folder is being checked,
removed or restored, the status bar also shows **Background tasks** with a count.
Its list names each folder and its current state, and offers the same review.
It adds no cancel or retry, and goes away only when the work has really ended.
A kept or unknown result stays on the working-folder entry while other folders run.
Review groups results for the same folder and lists their original sessions.
Blockers name the current sessions (with **Open session**) and programs with PIDs;
full reasons remain readable. **Clear settled results** clears settled kept
folders, including folders retained for another session or external program.
**Dismiss** removes one folder from this review; unknown results must be dismissed
individually and retain their native no-replay fence. Busy folders, live Force
offers and unfinished program stops cannot be cleared or dismissed. Closing the
review only closes it; it never clears results or releases an action.

The original folder is checked on its owning computer. A verified Zerus-created,
clean, integrated checkout can be removed with tool-declared generated caches
without another confirmation. Supported cache proofs include Cargo's cache-directory
tag, source-backed Python bytecode, package-manager metadata, Gradle build metadata
and a CMake build bound to this checkout. A directory name alone does not establish
disposable data. Other local files, source changes, external checkouts and uncertain
process use keep the folder for explicit review. The branch and conversation are
retained, and managed archive restoration records the exact retained head before
removal. An already completed native cleanup is reported directly; an uncertain
native cleanup is never restarted by the desktop. Cancellation, failed termination
and unconfirmed removal never trigger a retry. Background maintenance keeps its
existing policy and preserves all ignored data.

The cleanup review always shows integration evidence, even when local files
remain. Git integration uses local refs, not a fresh remote check. The selected
folder explains unsaved changes, local data loss and why automatic cleanup kept
it. **Inspect files and commits…** opens the bounded file browser, including
ignored files; a cache-like filename alone is not proof that data is disposable.
**Keep folders** makes no removal. Local-data and unsaved-work deletion require
separate consent. Manual removal keeps conversations and keeps branches by
default; restoring an archive may require recreating its working folder.

The CLI is `hgs [@host] worktrees review --path PATH
[--worktree WORKTREE] --json` and `hgs [@host] worktrees remove --path WORKTREE
--common-dir DIR --fingerprint HASH [--ignored-fingerprint HASH]
[--discard-fingerprint HASH] [--process-fingerprint HASH]
[--preserve-archive-restore] [--delete-branch]
--json`. Explicit inspection is `hgs [@host] worktrees inspect --path WORKTREE
--common-dir DIR [--file RELATIVE_PATH [--offset N]] --json`. Machines with an
older `hgs` must be updated before cleanup is available there.

## Project tasks

`hgs task` keeps a project's task list: prompts that wait in Backlog or To do
until someone sends them to a session. Every computer of the swarm holds every
task and works with them offline. Every command prints
JSON. Tasks belong to a catalog project, named by its ID or name; a task is
`T-` plus six characters, and `PROJECT/T-xxxxxx` names it anywhere.

```sh
hgs task add --project orchard [--backlog] [--target codex/api|any [--host ALIAS]] [--position N] < prompt.txt
hgs task add --project orchard --title "Task title" --request-id UUID < description.txt
hgs task rename orchard/T-a1b2c3 < title.txt
hgs task list --project orchard [--stage todo]
hgs task list --project orchard --stage todo --limit 50 [--after CURSOR|--before CURSOR] [--if-revision REVISION] [--session ID [--host ALIAS]]
hgs task get orchard/T-a1b2c3
hgs task get TASK_UUID --project orchard --history-limit 80 [--history-before CURSOR] [--attachments metadata]
hgs task attachment TASK_UUID --project orchard --file 1 [--entry ENTRY_ID]
hgs task send orchard/T-a1b2c3 [--session codex/api [--host ALIAS]]
hgs task handoff orchard/T-a1b2c3 --session codex/worker [--host ALIAS]
hgs task start orchard/T-a1b2c3
hgs task begin-attempt orchard/T-a1b2c3 --attempt ORIGINAL_UUID [--json]
hgs task control plan|execute|status --json
hgs task report orchard/T-a1b2c3 [--progress|--ready] [--considered REV] < summary.txt
hgs task report-attempt orchard/T-a1b2c3 --attempt 00000000-0000-4000-8000-000000000001 [--progress|--ready] [--considered REV] < summary.txt
hgs task cancel orchard/T-a1b2c3 [--reason "Wrong approach"]
hgs task stage orchard/T-a1b2c3 backlog|todo|done
hgs task move orchard/T-a1b2c3 --top|--before REF|--after REF
hgs task edit orchard/T-a1b2c3 < prompt.txt
hgs task comment orchard/T-a1b2c3 [--content-json] < comment.txt
hgs task target orchard/T-a1b2c3 codex/api|any [--host ALIAS]
hgs task target orchard/T-a1b2c3 new-session|subagent --parent codex/api [--host ALIAS] [--session-id ID]
hgs task worker orchard/T-a1b2c3 --subagent CHILD_ID --request-id SENT_REQUEST_ID
hgs task settings --project orchard [--set < settings.json]
hgs task cutover --project orchard [--hold on|off [--confirm]]
```

`begin-attempt` and `task control plan|execute|status` are currently inactive
source interfaces (`zerus-cg5h.4.7`). Valid requests return exit 1,
`code: task_controls_inactive` and `attempted: false`, before opening task or
desktop stores. The `task-control-v1` help marker is absent while the gate is
off. Existing prompts, send/handoff/reopen/direct-start/report behavior and
plain `start` are unchanged. Native wiring and queue activation are later
reviewed steps; a new CLI on disk does not qualify a resident adapter.

The control interface takes strict schema-1 JSON on stdin, at most 16 KiB. Plan
names the original task/attempt or, for Stop release, the exact executor and
explicitly observed Stop IDs. Execute accepts the original plan, with exact
task version, slot revision, caller scope, configuration/store/image and loaded
adapter guards; these labels are data, not authority. The owning runtime must
prove them again before any effect. Status names an original request UUID or
uses an executor sequence cursor with a limit of 1–128. Unknown fields,
duplicate fields/options, noncanonical IDs and oversized requests refuse.

After activation, a fresh managed send/handoff/reopen supplies exactly one
pinned `begin-attempt REF --attempt UUID` acknowledgement, replacing plain
start. Feedback carries the existing report origin and requests no second
acknowledgement. A semantic begin requires the immutable attempt to be current,
delivered and owned by the exact executor; it cannot acknowledge a newer
generation, clear a Pause or bind a native turn by itself. Legacy and direct
start policy, structured report authority and considered revisions stay intact.
The existing desktop Start plan is unchanged in this wire leaf; its final
managed/legacy acknowledgement mode and startup-run reservation belong to the
native-wiring check-in before activation.

Plain `task get` retains the complete journal for existing clients. The optional
`--history-limit` (1–80) returns only one chronological history page, the canonical
task document, `log_total`, `start_confirmed`, `history_schema: 1` and
`history_next`. Pass that opaque cursor as `--history-before` for the previous
page; a null cursor marks the beginning. Use the task UUID from the list to avoid
a project-wide short-ID lookup. Cursors bind the project, task and immutable
history entry, so new records and receipt refinements preserve an older page.
Omit the cursor to return to the latest page. Pages come from the project's
local task index, which keeps the histories of the 16 most recently read tasks
and reads a task's history again only after that task changes. The requested
task is held to the same rules as a page's rows: a suspect one is read from its
document first. The index is never replicated and does not change the task
document format. A task whose history
the index cannot give is read from its document, with the same page.
`--attachments metadata` returns each attachment of the task and of its entries
as `name`, `mime`, `reference`, `size` and the `sha256` of its bytes instead of
the file, and gives each entry an opaque `id`. `hgs task attachment` returns one
file with its bytes (`data_base64`): `--file N` counts from 1 among the task's
own attachments, or among an entry's with `--entry` and that entry's `id`.
Task list rows also include a bounded `history_revision` token. A journal-only
note or delivery receipt changes it, so the existing task stream can refresh the
selected inspector without transferring the journal or adding a polling loop.

Task views (`task get`, history pages and list rows) include an `execution`
object for a task that is In progress or recorded execution attempts:
`requirement` (the task's requirement revision, from 1; each edit of its prompt or attachments counts the next one, a rename or comment does not), `attempts`, `current`
and, when two attempts claim the same generation, `conflict: true`, which
nothing resolves automatically. `current` is the attempt the task's execution is
about: its `id` and `generation`, `origin` (`send`, `start`, `handoff` or
`reopen`), `executor`, `request_id`, the `requirement` it executes, `phase`
(`submitted`, `unknown`, `running`, `paused`, `ready`, `cancelled`, or
`unconfirmed` when nothing shows who executes it), `at`, and `began_at` and
`progress_at` when known. History written before attempts were recorded reads
as `legacy: true` with generation 0, from its latest boundary alone: a send,
handoff, reopen or direct start. A start by that boundary's session before the
execution ends (a result report that moved it, a cancellation or a stage move)
acknowledges it; a stale report or progress ends nothing. Any other start is a
boundary of its own, and an earlier execution never lends its origin,
time or receipt to a later one. A boundary written for a name-only session
(older history) names the session holding the task only through the binding
Zerus records when it binds that exact key to the holder's verified run, on the
same computer, before any change; the task keeps it until its next send or
start. Only that run is then the executor: a name alone never names one, so
another run that holds the name is not, and a holder an older build bound
without recording it reads as a conflict. When merged replicas leave the latest boundary with another session
than the one holding the task, `conflict` is set. Each attempt freezes the history
entries of the task's executions so far (`sent`, `resumed`); a send or reopen
it settles names its request. An execution entry neither frozen nor named, such
as a send an older build recorded concurrently, is listed in `unexplained`, sets
`conflict` and holds any new delivery until the person reviews that exact entry.
No start, child or result is invented. A resume settles only the pauses it observed, so a
pause that no resume observed keeps the attempt paused. A session's
Stop, idle time, pause or elapsed time is never an attempt event and never
moves a task. Attempts are kept beside the history under task schema 7, which a
reader that does not know them shows read-only: that includes the builds that
already read schema 6 but know no floor. The stored schema is one merged
value, so an older replica's concurrent upgrade can leave a lower one; this
version raises it again for a task with attempts in every write and every live
import, and before a task is read for an inventory, so the raised value is what
other replicas receive. That repair is a new change, not proof that an older
replica did nothing before it. A task can also take a floor before its first
attempt: a cutover record that keeps schema 7 the same way, so a reader that
does not know attempts or the floor refuses to write it, while every path that would start
it without an attempt refuses it as cutting over. A floor this version cannot
read still counts. This version reads attempts, guards reports against the
current execution, and records attempts and floors through the attempt writer
(below).

The attempt writer (zerus-cg5h.4 S2b.2 C3 activation) is how every native task
input now begins: the desktop's Start (`start-plan`/`start-deliver`),
`task send`, `task handoff` and a reopen (`task message --mode reopen`). It
runs on the computer whose session executes the task, which holds the input's
receipts, within one 15-second budget counted from the command's entry:
1. The request is frozen first: ONE request ID and the exact native request.
2. With no lock held, the fence admits it: the task's certificate with no
   network; otherwise, for a task with no floor yet, its first floor, which
   is written only when the project's bootstrap gate is positively satisfied
   (every eligible direct member recorded current, with a complete exchange,
   the phase known, and the hold off); then a fresh proof.
3. In the fence's final admission, and only there, the frozen request is
   checked again against the task as admitted (its prompt, attachments,
   queued messages, requirement, target, executor and settings): any change
   refuses it, and nothing is reserved. Otherwise its attempt is reserved
   under that ID and saved.
4. Only then, with every lock released, the input is submitted once. Its own
   submitted receipt settles the attempt, and the settlement never overwrites
   a later stage or assignment.

The outcome is explicit: `submitted`; `refused` (the request changed, or the
input was refused before anything was offered to the terminal, which settles
the attempt as not admitted; a new action with a new ID may send it);
`cutting_over` (the fence, the hold or unknown bootstrap evidence; nothing was
reserved or sent); or `uncertain` (the input may have arrived; it is never sent
again). Repeating a request ID, a restart or `--recover-only` only reads that
request's receipt and never submits, even when no receipt exists. Only a task
positively without floor or attempt, whose bootstrap gate is positively open
(a member not yet known current, or the person's hold), keeps the legacy route,
checked again there; a task that may hold a floor or attempt is never sent
that way. A reopen of a managed task keeps the person's text as a discussion
note bound to its attempt's ID, saved together with that attempt; the note is
not queued feedback and is never delivered by itself, and only the receipt
moves the task back to In progress. Feedback to a task In progress
(`--mode send`) stays an ordinary message.

Plain `task list` still returns every task for existing clients. With `--stage`
and `--limit` (1–200) it returns one page of that stage in the same order and
with the same rows, except that a prompt longer than 2 KiB is cut at a
character boundary and marked `text_truncated: true` (`task get` returns all of
it). The reply adds `page` (`schema: 1`, `stage`, `limit`, the zero-based
`position` of its first row, the stage's `total`, and the opaque `next` and
`previous` cursors), `counts` for every stage, `damaged` (at most 20) with
`damaged_total`, and a `revision`. Pass a cursor as `--after` or `--before`; it
binds the project and stage and names a row, so tasks added or moved elsewhere
never shift the page after it. A page past either end returns the nearest whole
page, and only an empty stage has no rows. `--if-revision` with the last
`revision` answers `{"unchanged": true, "revision": ...}` without rows when
nothing changed, after checking that page's rows against their files.

`--session ID` (`launch_id:...` or `run_id:...`, with `--host` for a session on
another computer) pages one session's view of the stage: the tasks its
identity owns, the session of a task in progress, in review or done, or the
target of a queued one (a sub-agent or new-session target counts for its
parent), plus To do and Backlog tasks for any session. A session's name never
matches: two sessions with the same name keep separate views. Rows, order,
cursors and `revision` are those of the unfiltered stage; `position`, `total`
and `counts` count the view, `damaged` stays the project's, and the reply adds
`session` (`host`, `session_id`). A cursor names a row, so tasks added to the
view before it never shift the page after it. The view reads two bounded
ranges of the index (the owner's rows and the unassigned rows), never every
row of the stage.

Pages come from a local index of task headers in the project's replica folder
(`task-index-v2.sqlite3`): a disposable cache, never replicated, rebuilt
silently from the task documents when it is missing, damaged or of another
version, and never a reason for a read to fail. An older hgs that shares the
state folder keeps its own `task-index-v1.sqlite3`, which this one never
opens, migrates or removes, so the two never rebuild each other's index. Each save notes the task in the
project's change journal (`changes`, a bounded hint that is only ever replaced,
never written through), so a read decodes only changed tasks. Every task
file's stamp (length, modification time, inode) is also checked at least every
30 seconds for writers without the journal: one listing of the task folder
gives the files to stamp (a file it does not list is missing, and a file
written after it is found by the next check), and only a task whose stamps
changed is read again. A row is suspect when it was read
from files modified within the last 2 seconds, before the journal last became
unknown, more than 10 minutes ago, or at a time after now (a clock set back).
A page reads its suspect rows from their documents again before it returns
them, and each read of the index reads up to 256 others again, those read
longest ago first, so every row, and with it every stage count, is read again
within 10 minutes plus ceil(tasks / 256) reads. The index holds at most 256
MiB whatever its page size; a larger one is built again. Short-ID lookups,
list places and the agent start limit use the same index. Adding, moving and
staging a task place it from the index too, without reading every task, while
the index is certified current (no row change by another build pending under
the repair epoch) and holds no damaged task: every task file's stamp is
checked first, as the periodic full check does (so a task a writer without the
journal added or moved is read again), and the two neighbouring tasks are read
from their documents to confirm their stage and rank. Otherwise, or when the stage needs its ranks
renumbered, the whole write reads the documents as before.

Send pastes the task prompt into its session through the same input path as
`hgs send`; the task becomes In progress, and Zerus does not
check that the agent received it. A managed dispatch pins its exact
`hgs task report-attempt REF --attempt UUID` command before the native request
is hashed. Its result can move that execution to In review; an explicitly
legacy execution uses ordinary `hgs task report`. Cancel moves a task In
progress to In review and pastes the cancel message without stopping the turn;
its reason is kept as a comment and never pasted. A task this computer already
shows In progress is not sent again.

Both report commands add the report to history. For a managed execution an
agent uses `hgs task report-attempt REF --attempt UUID`, with the UUID supplied
by its original dispatch or start acknowledgement. The named immutable attempt
must be current, its generation and verified executor must match, and the
existing conflict, phase and unknown-delivery guards still apply. A result
(default or `--ready`) can then move In progress to In review; `--progress`
records progress without moving the task. The report retains its declared
origin as optional `reported_attempt` log metadata. A stale origin records
`stale: "other_attempt"` and creates no Ready/Progress fact for the current
attempt. A different executor, a conflicting execution and unconfirmed delivery
retain their existing guards. Explicit human reports retain their existing
rules. `--considered REV` still only records the positive requirement revision;
this change does not gate actionable-clarification acknowledgement.

A plain report by a managed agent has no origin proof. It succeeds as a history
write (exit 0) with `history_saved: true`, `outcome: "not_applied"` and
`moved_to_review: false`. Text and JSON say that the task did not move and refer
to the command in the original task prompt or a person's acceptance/resend.
They do not supply the current attempt UUID or a newly retargeted command.
Never replace an original UUID with one from a later task lookup or retry the
history write as a duplicate. The new action is deliberate: older CLIs reject
`report-attempt`, while older `report` parsers can ignore an unknown option.
The CLI help marker `report-attempt-v1` lets coordinators check support.

Direct `add --start`, `start REF` and an active task's start acknowledgement
return `reporting`, `report_command` and, for an actual managed attempt,
`report_attempt` in JSON. The text prints the same pinned command. A positively
unfloored legacy direct start creates no attempt: it explicitly reports
`reporting: "legacy"`, prints the ordinary command and invents no UUID.
Creating managed attempts for every first direct start is separate work; this
change activates no new C3 admission or floor path. Legacy plain reports keep
the documented pinned-agent rule for never-sent queued work. A floor or a
recorded attempt never authorizes an implicit legacy start.

If managed history remains but the current execution falls back to legacy
(for example, after its first managed handoff is refused), a successful start
acknowledgement returns `reporting: "history_only"` and an explicit `report_hint`,
without `report_command` or `report_attempt`. It creates no attempt UUID and
explains that reports stay in history without moving the task. The person can
accept the result or resend the task as a new attempt.

This is an updated-writer boundary, with no task/exchange schema, event shape,
C3 or floor change. Older writer binaries still apply their older unscoped
policy. Prompts for attempts started before this change have no pinned command;
explicit human acceptance or a resend creating a new attempt covers them.
There is no silent upgrade or latest-attempt inference. Older readers may
ignore the optional report metadata while retaining it in the Loro document.

Run inside a session (`HGS_SESSION` and
`HGS_RUN_ID`), `hgs task` acts as that session's agent: `add` files the task
for that session, and the project settings can keep agents to their own
session or limit how many tasks they start per hour. `settings --set` reads a
JSON object and changes only the fields it names.

The desktop cancellation dialog uses `hgs task cancel-execution REF --project P --json`,
with schema 1 JSON on stdin: `dispatch_id`, `stage_at`, executor `machine` and `session_id`,
and `reason`. It checks the exact active execution while holding the project lock,
including after forwarding or offline fallback, and returns a compact task plus
`cancel_message`. Peers without this command refuse it; no legacy cancellation
fallback is attempted. `hgs task cancel` retains its existing full response.

The desktop's explicit Start for an existing executor uses two schema 1 JSON
commands: `hgs task start-plan REF --project P --json` prepares an immutable
native request without sending it, and `hgs task start-deliver REF --project P
--json` delivers that exact plan. Preparation binds a fresh request UUID, the
task's `stage_at` and target, and the selected host, session name, stable session
ID, run ID and conversation ID. Delivery goes through the attempt writer: it
rechecks the plan against the task and executor in the fence's final admission
before entering the existing native input path. Peers without these commands
refuse them; there is no fallback to an unguarded send.

Before delivery, the client checkpoints the complete original plan in its
private task outbox, separate from discussion-message recovery. A restored or
uncertain Start is never delivered again. **Review task start** reads that local
record; its explicit release first calls `start-deliver --recover-only` with the
original plan. This only reads the native receipt and settles an acknowledged
dispatch. A found receipt keeps the task started; otherwise the reviewed record
can be released so a later, separate Start has a fresh request UUID. A changed
record or failed check keeps the record. Release never stops an executor or
sends any input, and there is no additional polling loop.

Task links retain the computer and immutable launch identity (a run identity for
older bindings). The session name is a display and routing hint: renaming changes
the shown label and keeps current-session emphasis and filtering. A different
session reusing the old name does not inherit the task. Name-only older tasks can
use a uniquely retained rename alias; an unresolved identity cannot receive input.

An agent recording a direct request it is already doing uses `add --start`, or
`start REF` for a queued task. This records In progress without sending the same
instruction into its conversation again. For a dispatched active task, its exact current agent can acknowledge `start` once without a resend or a new attempt. Plain `add` remains a queue operation.
A report by the exact pinned agent also moves its unsent queued task to In review;
a report from another session remains a history note. `handoff` is explicit: only
the current agent owner (or the person) can transfer an active task, and both its
pinned target and working session change only after confirmed native submission.
Failed or uncertain delivery never authorizes an automatic retry.

The central Task View shows a canonical description document followed by
Activity-style discussion. The task menu opens the result and audit history;
Review foregrounds the result on the main page. Explicit **Edit description**
uses Save/Cancel; ordinary messages never rewrite the document. Task input uses
Activity's centered column, composer skin, attachments, voice preview and private
durable drafts keyed by project and task UUID. Lifecycle controls are separate
from message submission. The compact executor field distinguishes planned,
current and last execution. Clicking planned assignment changes only that field.
Its choices are the main session, a subagent, a new session, searchable existing
sessions and Any session. Choosing
an executor plan stores intent without starting anything. **Start task** for a
new session opens the standard session dialog; after its explicit launch, delivery
requires that exact new launch ID, computer, project, and unchanged parent
computer/session identity. A private record is reserved before task delivery;
restored reserved or uncertain deliveries are never retried automatically. Use
**Review executor launch** to inspect a failed or uncertain launch. Explicitly
releasing a reviewed record neither stops an agent nor sends the task again.

`hgs task message REF --request-id UUID --mode queue|send|reopen|note
--expect-dispatch ID --content-json` is a distinct discussion operation. `get`
returns the current `dispatch_id` (a native send receipt or an explicit direct-work
acknowledgement). Queue messages wait for the next explicit Start; their original
IDs, text and files are included in that dispatch, within the native text/media
limits. Exceeding those limits refuses the dispatch rather than dropping context.
Notes remain saved without delivery. In-progress feedback addresses the immutable
owner and captures its current run/conversation before transport. A native dispatch
receipt, when present, must match that live run and any recorded conversation.
Review/Done feedback explicitly uses reopen; only a confirmed submission returns
the task to In progress. Submission does not establish model consumption.

The replicated schema-five outbox is reserved durably before native input. A
repeated request verifies the original content, mode and execution and reads the
receipt; it never retries a reserved native send, even if no receipt survived.
SSH failures retain the desktop's original request for **Recover saved comment**.
That check uses `--recover-only` and cannot initiate a missing send. If no
reservation can be found, it retains the original comment in the discussion with
an unconfirmed outcome and no native submission authority. A different new
comment first completes that read-only recovery, then reserves its own identity.
An executor precondition refusal saves the comment with a failed delivery, leaves
the task's stage unchanged, and does not submit native input. Original
payloads are private per-task files read and written off the UI thread; settings
contain only small markers. Explicitly releasing a local recovery record neither
cancels nor resends native input.
Handoff cannot retarget a reserved message. Failed and unknown outcomes remain
visible in the same discussion. This is transport attribution, not a distributed
lease, per-task pause or a revision scheduler. Legacy direct-work acknowledgements
have an immutable owner and acknowledgement anchor, but no historical native-run
receipt; a message captures that owner's current verified run explicitly.

A subagent plan dispatches a creation request to its coordinator only on Start.
The coordinator creates the native child and records `task worker` with the
current durable `sent` request ID. Registration verifies its live parent run and
conversation and the child's presence in that native roster. The card displays
“child unconfirmed” until registration; the parent remains coordinator and task
reporter. This is explicit attribution, not the full per-worker scheduler or an
implementation of per-task Pause.

Descriptions and immutable comments support up to eight attachments per message,
10 MiB per file, with a cumulative 20 MiB across the task's current description
and its comment history. Bytes replicate with the document. Description files
travel through the ordinary native attachment path when the task is sent;
comments remain in task history and are not automatically delivered as new native
input. CLI `add`, `edit` and `comment` accept `--content-json` with
`{"text":"Inspect [Image #1]","attachments":[{"name":"screen.png","mime":"image/png","reference":"[Image #1]","data_base64":"..."}]}`
on stdin. Omit `attachments` to keep description files while editing, or pass an
empty array to remove them. Listings contain file metadata and omit history;
`get` includes the retained bytes.

The desktop right-hand Tasks board has a New task (+) button. Enter a title and
confirm it to create a shared To do card (Backlog in that list), then fill its
description in the central Task View. Subsequent input adds comments; Edit
description explicitly returns the same composer to the description draft.
Rename changes only the title. Creation and discussion never start an executor.
Task View retains separate drafts from Activity, supports description and comment
attachments, and offers voice recording with preview and transcription into the
draft before the person sends it. Task comments remain history-only.

Named cards use `--title`; description stdin may be empty. `--request-id` reserves
the card UUID and binds the original creation input. Repeating that input recovers
the existing card without reverting later edits or lifecycle changes. Reusing the
UUID for a different creation is refused. A title is one nonblank line, at most
512 UTF-8 bytes. Legacy cards keep their first description line as the displayed
title until an explicit Rename gives them an independent one.

Immutable session identities and description media use task schema 2. Comment
media, executor plans and native worker bindings require task schema 3. Independent
titles and title-only creation require task schema 4. Durable discussion identities
and delivery reservations require task schema 5. A later edit never downgrades that schema. Older readers and the current mobile ABI show
tasks newer than schema two read-only; named cards keep a visible title in
the existing text field. Project documents and the paged exchange protocol
remain schema 1. Mobile picker/preview and executor-plan presentation are separate
work; host-side mobile sends keep the complete description attachment payload.

`--host ALIAS` names a session on another computer of the swarm that this
computer verified itself. A send or cancel for such a session runs on that
computer: it gets the project's tasks first, refuses a task its own copy shows
In progress, pastes and records there, and its copy comes back (`synced` is
false when it could not; the next sync brings it). When it cannot
be reached, or its Zerus is too old to share tasks, a send is refused with the
reason and nothing changes, while a cancel goes through here: the task moves
to In review, its session gets no cancel message, and that computer's copy
follows when it syncs. Listings
name the computer of every session reference (the target, the session, the
author and each history entry's session and actor) as `host`: empty for this
one, the alias of a verified member, or null. Windows keeps no verified
bindings yet, so there `--host` is refused and other computers read null.

Tasks are Loro documents below `HGS_STATE_DIR/replica`, one directory per
project; a task file that fails to load is listed under `damaged` and left in
place until another computer of the swarm sends its copy, which replaces it;
a copy of the damaged file stays beside it as `<file>.damaged-<time>`. A task
this computer saved with attempts or a floor gets a mark, `<uuid>.managed`,
naming the version this computer last saved and the exact first block of
the updates file after the snapshot, sealed by a digest of its whole content
so that a mark changed in any way cannot be read. The mark is rewritten before every save
of such a task makes it durable (one small extra write per save), and every
fold opens a new updates file with the operations at the document's
frontiers, which every copy already holds, so it adds no change and differs
from fold to fold. A copy whose files hold less than its mark records (they
were cut short, a save did not finish, or an older Zerus wrote them so), or
whose mark cannot be read, is damaged, not read without those changes:
nothing sends, writes or folds it, not even a copy read before, until a person
reviews it or a copy that covers the mark arrives. Files that hold more than the mark records (an older
Zerus folded or appended) raise the mark to what they hold when the task is
read, before anything lists, inventories or sends it. A task with only its
mark left is still listed under `damaged`. Saves of a task go through one
store in turn: a copy read before a later save of the same task is merged
into what is saved, never written over it, and the mark never moves back. Its damaged copy is replaced only when the mark is readable and
still describes the files (an older Zerus that folded or rewrote them, or a
mark restored from before a later fold, leaves it stale), every
update saved here applies, and the other copy merged with them holds that
version, so no attempt fact is lost. A copy without a mark is replaced only
when every update saved here applies and the result shows no attempts or
floor. Otherwise the copy is kept: it stays under `damaged`, nothing is sent
for it, and each copy another computer or a phone sends is refused and named
in that exchange's refusals, not kept for later. A person reviews it: moving
both damaged files (`<uuid>.loro` and `<uuid>.updates`) away keeps it refused
while the mark remains, and removing the mark as well accepts the next copy
sent, which may lack attempts. Any
entry at the mark's path counts as a mark, even one that cannot be read.

For an explicit local review, `hgs task review-copy --project P --uuid UUID
--json` inspects only that exact kept damaged copy. It returns a fingerprint;
`hgs task review-copy --project P --uuid UUID --confirm FINGERPRINT --json`
confirms moving its `.loro` and `.updates` into a private sibling
`<uuid>.review-<UUID>` directory and removing its `.managed` mark last. Only a
person can confirm. A healthy copy, changed fingerprint, link, special file,
missing store or unexpected project identity is refused. Missing task files are
explicitly part of the review; a mark-only copy can be reviewed. The store lock
wait is bounded to two seconds, and inspection reads at most 64 MiB across the
three files with a five-second inspection budget. Other tasks are not read or
changed. The next incoming copy uses the existing importer; review sends and
starts nothing. The files moved aside remain available for manual examination.

In desktop Tasks, the damaged-files notice shows at most five kept-copy review
cards, with project, known title and a copyable exact UUID. **Review local copy…**
opens a confirmation naming the possible loss of attempt history and unsent
local edits; **Cancel** has default focus. The card retains moved, refused with
reason, or unknown outcomes. Unknown is never automatically retried; inspect
the files manually. **Dismiss** removes a moved outcome from the view only.

What this recovery supports, alone or combined: running an older Zerus
after a rollback (its importer may fold, append or rewrite the task files as
it always did), a crash at any point, damaged or truncated files, and an
unreadable or dangling mark; in each it stays correct or keeps the copy. It
also supports restoring or copying the whole replica root as one consistent
older state, an ordinary rollback of local data: other computers supply the
newer changes again. It does not support replacing, restoring or copying
individual files under the replica root (a task's snapshot, updates or mark)
from a backup or another computer while the rest stays newer, by any means,
including file-level backup and sync tools: such a partial restore can put
back an older pair of files that agree with each other, which nothing here
can tell from a genuine one, and recovery may then accept another copy and
lose what was written after that backup. Restore the replica root only as a
whole, never copy task files back one by one, and keep file-level sync tools
away from it. After restoring a replica root from a backup or copying it to
another computer, delete its `store-id` file so a new one is created: other
computers then see a changed store, and the cutover proof (which binds each
member's verified computer and store) stays closed for it until a fresh,
complete inventory with it. Zerus creates `store-id` only where nothing is
(or an empty file an interrupted first creation left); one it cannot read
without waiting, that is not a regular file, or that holds anything but one
canonical identity is refused and kept, never replaced, until a person
removes it. A project folder is listed only by its own `project-id`: a
regular file holding the valid ID the folder is named after.

The damaged entries of `task list` (and its pages) say what is known here,
read only: `kept: true` with `kept_reason` (`marked` for a copy cut over
here, `unreadable_local_data` for torn or unreadable local updates) for a copy
kept for a person's review whatever copy arrives, or `recovery:
"depends_on_incoming_copy"` when what happens is decided against each copy
that arrives; the listing never says a replacement is safe. A refusal names
the decision against that copy: `kept` with `marked`, `attempt_evidence`,
`pending` or `unreadable_local_data`, in `hgs` output, exchange refusals and
the phone's `refused_there`.

The swarm worker
replicates them with the catalog: after each catalog
sync it compares every project's digest with the member's and exchanges only
the changes each side lacks, in pages of at most 1 MiB, and a task changed
here goes out within seconds. Changes made on several computers at once merge.
A member whose Zerus does not share tasks keeps syncing the catalog and is
named in the Swarm settings. A hello's replica part also carries
`task_schema`, the highest task schema that Zerus writes (7, which reads the
floor; absent in an older one), `store`, the random identity of its replica
root (`replica/store-id`, created once under a lock), and for each project
it read how many tasks do not load (`damaged`, 0 included) and which it left
out this time (`omitted`, busy or unreadable). This computer keeps what each member's last
verified hello showed (its computer, replica protocol, task schema and store)
and how each project's last exchange with it ended in
`replica-members.json` beside the catalog, information only. Each record is
bound to that exact observation: a changed store, protocol or schema retires
the completions recorded under the old one, an outcome that arrives for an
observation that is no longer the member's is refused, and a member's record
goes at the membership change itself. Every `replica-inventory` and
`replica-exchange` reply names the store that answered, read as it answers: a
reply from another store than the hello showed (its replica root was replaced
since) ends that exchange as `changed_identity`, and one that names no store in
a form this reads still exchanges documents but is `malformed`. A project is `complete` only when both
sides' whole inventories were exchanged: every page, nothing refused, and no
task damaged or left out on either side (equal digests of the readable tasks
alone are an exchanged subset, not a complete project). Otherwise it names a
reason: `unreachable` (also for every project known here when the member's
hello fails), `busy` (also for every project known here when this computer's
own pass cannot read its projects), `refused_documents`, `page_budget`, `changed_identity`,
`damaged_tasks`, `omitted` or `malformed`. Absence or unreadability is never
evidence of success: a project map, damaged count, refusal list or import
count that is missing or in a form this does not read is `malformed`, never
taken for an empty one. An exchange reply whose pages do not read stops the
exchange; a reply whose evidence does not read (a member before this one sends
no damaged counts) still exchanges documents and stays `malformed`. Every
project this computer stores or knows alive gets an outcome on each sync.

`task cutover --project P` shows the project's cutover to execution attempts
as this computer knows it, read only and bounded by its members: `phase`
(`not_cut_over` while no task here has a floor or attempts, `cutting_over`
while a member keeps it open, `active` otherwise, `unknown` past its count's
budget), `hold`, `cut_over_tasks`,
`last_cutover_write`, every eligible member with its advertised task schema,
store, when it was seen and its last inventory of the project, and `closed`:
the first reason it is open as `{member, project, reason, at}` (`unseen`,
`protocol` for another or no replica protocol, `task_schema` for a member below
7, `store` for a missing or invalid store identity, `no_inventory`, an
incomplete exchange's reason or `incomplete`, or `inventory_before_cutover`),
with `closed_text` as prose. Anything not known as complete keeps it open. The
count of cut-over tasks has a strict budget (entries and time): past it the
phase is `unknown` and the reason `scan_budget`. Eligible members are the
project's swarm members that may hold its documents: every verified member
except one whose verified hello has no replica key at all; a replica part
this cannot read keeps the member eligible and the cutover open as an
unknown protocol. It grants nothing and dispatches
nothing. `--hold on` sets the project's cutover hold, a replicated value that
names who set it and when (concurrent changes: the last writer wins). While on,
no task of the project takes a new floor or a first attempt (the attempt
writer refuses them as cutting over); it never lowers anything, and a
task that never cut over keeps the legacy Start. Only a person sets or clears it,
and clearing it takes `--hold off --confirm`. A hold that cannot be read is
on. Times are in milliseconds.

`task cutover --project P --task T --check` reads the task's writer universe
for the fence that the attempt writer uses (zerus-cg5h.4 S2b.2 C3), within
one 15-second budget counted once from the command's entry. This computer's
identity, the project's name and lock, every call (from its spawn to its
parsed answer) and the result all count against it, and nothing is reported
open after it. The check needs `--project`. It covers every node that may hold a task
of the swarm, not only this computer's peers:
- this computer;
- every catalog machine not redirected away (each machine that ever joined
  writes its name there);
- every node reached over verified peer links.

A node's catalog name is only something to cover. Each node must answer a
fresh hello over its own route, and that hello's node is checked at every hop.
A hello's replica part lists the node's verified direct peers (`peers`) and its
`fence` capability. `swarm replica-relay` forwards one hello, or another relay,
only to the relaying member's own verified alias for the next node. It passes
on what is left of the deadline and the hops, and answers with the reply
checked to come from that node. Its budget starts when the request arrives:
its catalog lock, its store identity and the forwarded call all count
against it, and it never answers after it.

The check reports:
- `universe`: this computer's node, store and replica root, and every node with
  its route and what its hello showed;
- `incarnation`: a digest of all that, which changes when any of it does (a
  replaced store, a new route or node, or any enrolment or disconnection:
  each computer keeps `swarm/membership.json`, an epoch and a generation that
  rises, durably before the catalog publishes the change, whenever its peers
  change, including a change an older build made, which the next read
  records);
- `closed`: the first node that keeps the fence closed, and why:
  - `unreachable`, `unverified` (another computer answered), or `unroutable`
    (a catalog machine that no verified route reaches);
  - `protocol`, `task_schema` below 7, `store`, or `fence_unsupported`;
  - `unavailable` (the node's own replica cannot be read now: its hello sends
    an explicit unavailable part, and it stays a writer);
  - `busy` (the catalog lock was not free within the budget), `deadline`,
    `discovery_bound` (a node still unseen beyond 16 hops), or
    `too_many_nodes` (more than 256 candidates, from the catalog, every
    visited node's writers and the nodes found over peers, counted as they
    are added). A partial universe is never certified.
- `universe_closed`: the first node that keeps the universe closed, alone;
- `proof`, when the fence is proved: the common `version`, the `rounds` it
  took, every node with the store it answered from, and `admissible: true`
  once the final admission passed (C3c2).

A node whose hello has no replica key, as an older build sends, shares no
tasks, so it alone is left out. A node whose replica cannot be read sends a
present part instead, and is never left out.
The nodes behind it are not left out: with no other route they close the fence
as `unroutable`. The check holds no lock across the network, creates no floor
and no attempt, and grants nothing.

The proof (C3c2) runs over that universe. Each round sends every node, over
its verified route, `swarm replica-task` for the task. That request carries
this computer's version (a canonical map: decimal peers and positive counts,
compared as the changes they name, never as encoded bytes) and the updates the
node lacks. The node then:
- imports them;
- runs the store's locked schema repair;
- saves durably;
- reads the task again from its files;
- answers with that read: its version, whether its state is every change it
  holds, its raw stored schema, whether it holds a valid floor, and the updates
  this computer lacks.

A node answers only from the store its fresh hello over that route showed;
any other store closes the proof as `changed_identity`. The fence is proved
when, in ONE round, every node answers exactly this computer's version, with
nothing more to send. That version must hold a valid floor and a RAW stored
schema of at least 7 (not the raised one a cut-over task reports), and this
computer's copy must still hold it when read again afterwards. Answers for
different versions are never combined. At most three rounds run within the
budget. The universe is then read again and must be the same incarnation.

The final admission takes the catalog lock, the project lock and the store's
admission, in that order and within the budget. Under those locks it reads
again:
- this computer's membership view, its store and its replica root;
- the hold, for a task that records no attempt yet;
- the task: holding the proven version, its floor and its raw stored schema.

Only then would a reservation run; the check runs none. Closed reasons add:
`no_floor`, `schema`, `damaged`, `not_converged`, `changed_identity`,
`membership_changed`, `changed_version`, `held`, `malformed`, `busy` and
`deadline`. It proves the schema barrier only: not exclusive delivery, not
consent.

A proof that passed its final admission can be kept as the task's
certificate (C3e): `<project store>/fence/<task>.json`, inside the replica
root, so a replaced root loses it. The record holds:
- the proven version;
- this computer's membership view (peers, catalog machines, membership
  generation), store, root and configuration identity (the Location identity
  of its state and configuration folders);
- every node with the store it answered from;
- a digest of all of it.

A later operation reuses it with no network, in the same final admission
(the catalog lock, the project lock, the store's admission). That admission
reads everything the record binds again:
- the record itself, for this task, project and swarm. It is read without
  waiting: only a regular file of at most 1 MiB, never a link, FIFO, device
  or folder (`certificate_not_regular`, `certificate_oversized`). It must be
  exactly as written (its digest) and well-formed: a non-empty canonical
  version, and every certified node a canonical identity other than this
  computer's, with a canonical store;
- the view, so any enrolment, disconnection or new catalog machine refuses;
- the store, the root and the configuration identity, so another
  configuration sharing this state never inherits it;
- every direct member, from the swarm's member records, read the same way:
  - a certified member must be recorded with the store it answered from;
  - an uncertified member must be recorded with no replica;
  - a member without a record of its current enrolment refuses
    (`member_unobserved`), and so does a damaged record file
    (`members_unreadable`). The records only refuse; their absence is never
    taken as "nothing changed";
  - a certified member must positively show replica protocol 1 and task
    schema 7 or later: another protocol, an older schema or none refuses
    (`member_downgraded`), so a direct member rolled back below the
    floor-aware build stops the reuse at its next recorded hello;
- the hold, for a first attempt;
- the task: holding the certified version, with its floor and raw stored
  schema.

Anything changed refuses it with its reason, and the caller proves the
fence afresh. Only the attempt writer writes a certificate, and never one
that its reuse would refuse: inside the proof's final admission, under the
catalog lock that every member record write takes, each direct member must
already agree with the proof by the same rules, or nothing is written. So
the writer first records each direct member's verified hello (the swarm's
replica pass does). With that setup a fresh proof's certificate is reused at
once and on every later attempt, until something it binds changes. `--check` reports
`certificate` (`valid`, or the first `reason`) and never writes one. A
certificate is a schema-barrier record only: never consent and never send
authority.

Cutover boundary (owner decision, 2026-11-05): once a project's tasks cut
over, the rollback floor for every computer in that swarm is the first
floor-aware build. That is the minimum managed-store, writer and importer
implementation, on every import route, including a phone's answering hgs.
Running an older CLI or desktop there is unsupported: an older build's
damaged-task recovery, or a truncated update tail, can bring back a pre-floor
copy that it would then write. On an activated swarm that floor is the first
release with the membership incarnation of C3c1 R3 (in practice 0.48.0);
unreleased intermediate builds are unsupported too.

Phones reach a project's tasks through the mobile relay and the Rust mobile
connector, which uses two commands of its own. `hgs task digests --json` prints
each project's digest of what a phone holds (the project document and every
task that is not Done), which the connector adds to its heartbeat;
`hgs task exchange --json` reads a phone's request on stdin, imports the
phone's changes and answers a page of what the phone lacks, at most 512 KiB of
updates, with a larger task in parts. A task the phone holds keeps coming to it
after it becomes Done. Opening a missing Done task requests its UUID with an
empty version and receives a Loro shallow snapshot at its current frontiers:
all current fields, logical history, attachments, schema floor and execution
facts, without the superseded CRDT edit operations. Live tasks, nonempty-version
deltas and ordinary desktop replication keep their existing encoding; opening
an already cached Done task does not rewrite it. The exchange remains schema 1,
with the same required answer fields and existing bounded parts and hashes.
Task-capable Android and iOS builds have used Loro 1.16.2 throughout; iOS
0.1.0 (849) predates Tasks and never requests this operation.

A change from another computer can depend on operations before a phone copy's
shallow root. That merge is explicitly refused, keeping the local files. The
phone makes at most one fresh empty-version fetch for that task per exchange.
It replaces a healthy shallow copy only if the fresh state covers every
operation in the latest saved local version, checked and saved under one store
admission. Otherwise its unsent edits stay in place for a person's review;
neither a refusal nor a failed fetch clears them. Later comments and a desktop
reopen back to To do continue through the same exchange.

A request may cover a range of task UUIDs, so a large
inventory goes in parts; a refusal prints `{"schema": 1, "ok": false, "error":
…}` with exit status 1. Both read what the phone holds from the project's task
index, which keeps each task's version and the project document's version, so
a call reads again only what the change journal or the periodic full check
finds changed, and neither rewrites the store's inventory cache; when the index
cannot answer, they read the documents as before, with the same digest. The
index reads a task as the inventory does: a task whose execution schema an
older replica's merge lowered is repaired and saved first, so a phone receives
the repair; an index whose rows a build without that rule read reads every
row again once, and after such a build sharing the index stores rows, the
index answers only once every row was read here again (in its bounded batches,
whatever the clocks); until then both read the documents. The phone's Send now and Cancel run on the session's
computer as `hgs task mobile-send --json` and `mobile-cancel --json`, which do
what `send` and `cancel` do and answer only the task's stage (a refusal says
`"uncertain": true` when the prompt was pasted but could not be recorded);
`hgs task mobile-done --json` answers a page of at most 50 Done tasks, newest
first, with how many there are and how many from the last 7 days. It chooses
the page from the task index while the index may place tasks (see the task
index below) and reads only the page's tasks; otherwise it reads every task as
before. See
`services/mobile/relay/README.md`.

The phone apps edit only tasks at schema 2 or below, and none of their edits
changes the stored schema. The exchange imports a phone's changes and raises a
task that records attempts back to the execution schema before it answers, so
a phone that receives a task's attempts receives that schema with them and
shows the task read-only from then on. Its earlier offline edits merge as
ordinary concurrent edits; they add no attempt fact and never stand for a
delivery, a refusal or a cancellation. Send now and Cancel from a phone run the
same checks as `send` and `cancel`, including the hold of an attempt whose
delivery is unknown.

Agents use the same `hgs task` commands in their own shell. A command run inside
an agent session acts as that session when it proves it as `hgs self` does (the
run is live here and the command descends from the agent; renames are
followed) and is refused when its environment names a session it cannot prove.
It works on the session's catalog project unless a reference or `--project`
names another. A change an agent makes prints one line saying what happened,
for example `orchard/T-a1b2c3 sent to codex/api.`; `--json` prints the task, and
reads print JSON. Claude Code and Codex sessions read the instructions for the
task list from Zerus's SessionStart hook; `hgs guide tasks` lists the commands.
There is no MCP server; sessions without bypassed permissions are not covered
(Claude Code asks first, Codex's sandbox blocks the task store and SSH).

In the desktop, the right **Tasks** inspector is the project board. There is no
Task Board tab beside Activity. The first filter, **Active**, combines In review
and In progress without changing either workflow stage. Individual stage tiles
(In review, In progress, To do, Backlog, Done) remain available; the funnel keeps
this session's tasks and Any
session tasks it can take, and the gear opens project settings. Narrow filters
keep icons and counts; wider filters include captions. The plus replaces the
count label with a title field in the same header: Enter creates the named card
and opens its description for editing. Escape closes the field and keeps its
local draft. Adding a card does not move the filters or list viewport. The chosen
side-panel width is local and survives hiding, resizing and application restart.
Double-click its divider to reset to 400 logical pixels, subject to available
space and the panel header minimum.

Clicking a card opens the reusable central **Task View**, preserving the Activity
conversation and its input draft. Click the task heading to rename it in place,
or choose Rename in the card menu. Enter or leaving the heading saves; Escape
cancels. A refused rename retains the edit. Description is a document followed by
the discussion; editing it is an explicit Save/Cancel operation available in To
do and Backlog. The assignment control changes the executor plan separately.
Workflow commands remain separate from unsaved description edits. Result and
History open through the task menu. Reports are not proof of a merge or install.

The Task View uses Activity's Markdown renderer, content width and rich input,
including attachment previews, voice drafts and per-task local recovery. The
input explicitly names whether it edits the description, saves a note, queues
context for Start, sends feedback, or requests changes. Comments stay in the
discussion with their delivery status; a refused executor check saves the comment
without sending it or reopening work. Recovery never replays unknown input.
Selection retains the previous painted document until the exact selected reply
arrives. Earlier history pages load in the same view; Latest returns to the tail.
The GUI retains at most 100 listed tasks and 80 selected-task history entries.
Legacy CLI reads still transfer the full journal; indexed backend reads remain a
separate tracked change.

Rows in every writable stage can be reordered with an insertion marker and edge
scrolling. This does not change execution or interrupt work. To do and Backlog
also accept stage and assignment drops. Right-click a card for its actions.
Compact cards expose Start task for To do/Backlog, Accept for Review and Back to
To do for Done. Task pause is disabled until independent execution accounting
exists. Executor labels and Open Activity follow retained immutable session
identity across renames. An unconfirmed start is labelled explicitly. The board
and selected Task View share one polling controller. Full execution and result
milestones remain specified in
`superpowers/specs/2026-10-26-activity-task-execution-design.md`.

## Machines and accounts

**Machines** edits local SSH aliases and overrides in
`~/.config/hgs/machines.local.json`, leaving the shared `config` intact. Blank
fields inherit OpenSSH configuration. Effective values come from `ssh -G`
without connecting; `hgs machine resolve ALIAS` exposes the same result.
**Test connection** checks SSH and hgs/tmux versions. **Open SSH terminal** supports
interactive host-key verification. **Install hgs** builds the CLI remotely from
a selected checkout while preserving existing state. Unsaved machine drafts do
not change configuration; **Discard draft** removes the draft explicitly.
Zerus lists a machine's sessions and connection state under its alias, even when
that machine calls itself something else (its `HGS_SELF` or host name).

**Connection status** shows the selected machine's connection and, while it is
not connected, the time to its next automatic attempt. The time comes from the
connection supervisor that actually waits (the background service's, or the
app's own without it): each wait publishes the deadline that ends it, measured
on that process's monotonic clock and anchored once per wait in the view, so
status updates never restart or extend a countdown. **Reconnect now** ends only
that machine's current wait in its existing supervisor; the catalog stays as
last known, follows and source identity are kept, repeated clicks join the
running attempt, and nothing restarts the background service, a source or an
agent. Disabled and removed machines have no supervisor and no button.

**Reconnect schedule** sets this computer's delays for every machine: the first,
second and third retry, then every later one, each 1 to 3600 whole seconds and
never shorter than the one before. The defaults are 1 s, 2 s, 5 s, then every
10 s, so a computer that starts later is found within seconds. A change applies
to each machine's next wait. `hgs machine retry-delays [--set FIRST SECOND THIRD
LATER | --reset]` reads or writes `~/.config/hgs/reconnect.local.json`
(`{"version":1,"delays_seconds":[...]}`) under the machines lock; `hgs machine
ls` reports the same as `retry_delays`. A missing file means the defaults. An
unreadable or invalid file is kept as it is and reported: readers use the
defaults, a running background service keeps the last schedule it read (one
that starts with such a file uses the defaults and keeps running), and only
`--reset` replaces it. The background service reads the file when it starts and
again on the desktop's request after a change, so the schedule stays in effect
without the desktop.

**Phones**, below the machines and in Settings → Mobile connection, lists the
phones paired with every workspace this computer reaches: through its own mobile
connection, or through one enabled peer that has one (one hop, like phones reach
peers). Each phone shows its platform, when it was paired, its last activity
(recorded at most every five minutes) and the computer whose invitation it used;
older pairings show "—". **Pair a phone…** shows a single-use QR code with the
server and code for manual entry; it expires after ten minutes and closing the
dialog discards it. Opening **Pair a phone…** refreshes the available workspaces
before requesting one invitation, including immediately after connecting a workspace.
The QR code grows for longer encrypted invitations; **Enter the code manually**
reveals the full selectable code in a bounded field. Scan it in Zerus on the phone
through Machines → Pair another
workspace. **Revoke…** asks for confirmation, then removes that phone's
credential, queued requests and push registration; the phone loses every computer
of the workspace at once. A computer whose hgs predates phone management is named
and never asked. The same operations are available as
`hgs [@host] mobile phones|invite|revoke-phone <id> --json`; a computer without
a mobile connection answers `{"configured": false}`. Requests go to the relay
through the system `curl` with the computer credential on its standard input;
curl ignores the user's `.curlrc` and environment proxies and never follows
redirects. The desktop passes `--workspace ID --server URL` from the list it
shows, so a gateway whose mobile connection changed meanwhile refuses to invite
or revoke for another relay or workspace, and a code for another workspace is
never shown. A revocation that fails or cannot be confirmed stays reported until
**Refresh**; it is never retried automatically.

For an encrypted workspace these commands run the mobile connector. The desktop
runs them on its own computer with the exact connector its status probe uses,
in `ZERUS_CLIENT_MOBILE_CONNECTOR_PATH`: on Windows the one packaged beside the
app first, elsewhere the one beside the selected hgs, then beside the app, then
on PATH. The CLI runs that program and never falls back from an empty, relative,
missing or unusable selection to its own sibling or PATH. Without a selection
the CLI keeps its order: the connector beside the running hgs, then PATH. The
desktop never sends its local selection to another computer.

On Windows, these phone-management commands pass a nonempty `CURL_CA_BUNDLE`
path explicitly to curl's `--cacert`, so Schannel verifies the relay using that
PEM CA bundle. If the variable is absent or empty, curl keeps its default
trust behavior. An invalid bundle or failed certificate verification refuses
the request; there is no fallback to an unverified connection.

Multiple machine filters narrow both sessions and content search. Label color,
Bright/Soft mode and appearance previews are local. Color pickers cancel on an
outside click or Escape; only explicit confirmation saves a color. Project
color changes remain staged until **Apply**. **All machines** clears the
restriction. A temporary connectivity error retains the last snapshot and makes
unavailable controls explicit.

The machine chooser shows each computer's session count, including zero, and an
All machines total. Counts follow the current session state, project and folder
scope before machine selection or text search; child rows and unconfirmed launch
placeholders do not add sessions. An open chooser updates counts without replacing
its checkboxes or changing the selection. Offline computers retain their last
known session inventory; live-work filters still require current work evidence.

<a id="accounts"></a>

### Accounts

In **Accounts → Model and effort…**, save the model and reasoning effort for new
conversations with the selected account. One account has one model and effort
(owner, 2026-11-07, zerus-f0uz8): saving applies on every computer that has the
account, the selected one first, and lists where it saved and where it failed,
such as an offline computer. When the computers differ, the account shows
**Differs between computers** with **Use everywhere**, which saves the selected
computer's choice on all of them. Codex, Claude and DeepSeek offer
only models and effort levels from the selected computer's native catalog; each
other computer checks the choice against its own catalog. Open the agent
once and refresh Accounts if its catalog is empty. **Provider settings** removes
the Zerus override; the dialog also shows readable native account defaults.
Kimi supports a launch model choice; its effort remains controlled by Kimi's
native settings. OpenCode continues to manage its defaults natively.

The native DeepSeek account (`native-dsh`, zerus-v7tqa) saves a native route
`provider/model` with exactly one slash, such as
`deepseek-official/deepseek-flash` (shown as **DeepSeek-V4.1-Flash
(DeepSeek)**), and an effort of `off`, `low`, `high` or `max` that the chosen
model's catalog entry offers. Its catalog and native default
(`agent-default-model`) come from the running DeepSeek host's read-only catalog
method; Accounts never starts a host, so start or resume a DeepSeek session and
refresh when the list is empty. Zerus applies the choice to each session it
creates, from `hgs dsh FOLDER --new` and from New session, as described in
[Official DeepSeek Harness](#official-deepseek-harness). An older `hgs` refuses
an account catalog that holds a DeepSeek choice ("Invalid account model
defaults"), which stops its launches on that computer: clear the DeepSeek
choice (`--model ''`) before installing an older CLI build.
Existing conversations, saved resumes and forks keep their own choices. Explicit
native launch options after `--` take precedence. Nothing rewrites native account
configuration or changes a running agent.

The CLI is `hgs [@host] account model-settings ID` to inspect the catalog and
`hgs [@host] account model-defaults ID --model MODEL --effort LEVEL --revision REV`
to save a choice. Omit effort to inherit the selected model's provider default;
pass `--model ''` to return to provider settings. Revisions prevent overwriting
another account edit made while the dialog was open.

Model-default entries are read independently. An unknown provider, unfamiliar
field shape or model/effort value this build cannot validate is preserved
verbatim and unused, so it cannot block other accounts or launches.
`account ls` and `model-settings` report `model_defaults_status` as `inherited`,
`saved` or `ignored`, with `model_defaults_reason` for an ignored choice.
Saving a choice still checks the selected account's native catalog and supported
values; setting or clearing one choice preserves all other entries unchanged.
Invalid JSON still refuses the catalog. This compatibility applies only to
builds with the lenient reader: older builds can still reject a newer saved
choice, so clear that choice (`--model ''`) before rolling back to them.

Without a choice, a Claude, Codex or Kimi account bypasses permissions (owner,
2026-11-07, T-6hcfnu): each provider's own bypass switch is added at launch
(`--permission-mode bypassPermissions`,
`--dangerously-bypass-approvals-and-sandbox`, `--auto`), unless the launch
names its own mode after `--`. The account shows **Zerus default**, and New
session shows the selected account's effective mode. **Change…** saves an
explicit choice, **Bypass permissions** or **Provider settings** (the
provider's own ask, plan or default mode), which always wins; `hgs account
permissions ID --mode default` returns to the default. The CLI lists
`permission_mode_source` (`default` or `zerus`). OpenCode and DeepSeek keep their
native configuration. Choices saved before this default existed as provider
settings were stored as no choice, so they now read as the default. Zerus's
automatic tool approvals are a separate opt-in and stay off.

OpenCode's Accounts permission line is a read-only account configuration summary.
It reads `opencode.json` then `opencode.jsonc` in the native account's configuration
directory (`OPENCODE_CONFIG_DIR`, otherwise `XDG_CONFIG_HOME/opencode` or
`~/.config/opencode`), an absolute `OPENCODE_CONFIG` file, then the platform's
system configuration directory. Reads are limited to 1 MiB per file and 4 MiB in
total. JSONC comments/trailing commas and both current `permissions` arrays and
legacy `permission` rules are recognized. Without rules, the line shows
**Allow tools**, OpenCode's [native default](https://opencode.ai/docs/permissions/#defaults).
Ordered exceptions show **Custom rules**. Unreadable, malformed, oversized,
unexpanded agent/policy configuration and deployed macOS managed preferences stay
**Unverified**. Project, agent and session rules can differ. This never writes
OpenCode configuration or changes a launch or running session.

The Zerus permission mode (**Permissions: … Change…**) is likewise shared by every
computer with the account by default, with the same per-computer results and
**Use everywhere** mark. **Keep per computer…** switches the account to
per-computer permissions, for example bypass on one computer only: then
**Change…** saves only on the selected computer, the row names it, and
**Share across computers…** switches back. The account says which mode it is in.
Each computer keeps the choice as `permission_scope` in its `account ls` rows
(`account` or `computer`, from `account-scopes.json` beside `accounts.json`,
which older hgs builds never read); any computer that keeps permissions per
computer makes the whole account per computer, so a change never spreads by
accident. `hgs [@host] account permission-scope ID --scope account|computer
[--revision REV]` sets it on one computer; `account ls` reports
`permission_scope_supported: true` where it is available. An unreadable scope
record reads as per computer. The scope is outside the catalog revision, so a
shared permission change checks it again when it is saved: if the account is
now per computer, by the desktop's current view of every member or by a fresh
read of any computer it writes, nothing is saved; each write also passes
`--expect-scope account`, which that computer checks under its catalog lock.
**Add to machine…** copies the selected computer's model and effort, and its
permission mode while the account still shares it, to the new profile as
explicit values, provider settings included, so a restored profile drops older
overrides. A computer whose catalog refuses one names it in the result, and
the account shows the difference.

Accounts collects provider profiles from connected machines. Verified provider
account ID, or email with organization, groups the same account across hosts;
matching display names alone do not. Providers, organizations and conflicting
IDs remain separate. Local results appear first; remote profiles arrive
incrementally. Known rows and selection survive refresh, failure and offline
hosts. Limits use one current account snapshot, never the sum of machine usage.

On Android, the refresh status below the toolbar remains readable after a quick
sync. **Refresh details** lists each workspace's current read/apply stage, last
received report and failures; retained sessions and counters stay visible when
a read fails. Phone sync reads machine reports, while providers refresh their
usage independently. Account cards show the machine's provider refresh state,
saved/current report and provider update time, including after phone sync ends.
An unavailable quota report or an expired short-lived Claude usage token does
not mean the native account is signed out. Explicit native sign-in status takes
precedence; older authentication problems are labelled **Last reported** when
their report is stale or offline. Viewing accounts never refreshes Claude's
credentials on Zerus's behalf.

Claude usage refreshes share a cache on the account's owning computer. Account
views and the selected session footer normally request usage every five minutes;
their clocks, manual Refresh and requests from other GUI clients share a five-minute
cache interval per account. Account-list reads (every 30 seconds in
the desktop) use saved reports and never contact the usage endpoint. Arch and
Mac clients viewing the same owning computer therefore share its cooldown.
Verified native account UUIDs share a cache across profiles on that computer;
otherwise the selected profile home and credential identity keep reports separate.
Every Claude inspection first reads current native sign-in status, even during
a cooldown. Cache keys bind that observed email, organization identity and plan,
plus any stable credential account identifiers. Token values, expiry, scope and
tier changes do not reset the same account's cooldown. A different account never
inherits the prior account's windows or wait; an unconfirmed native identity
cannot adopt a previous report. Cache and lock files must be private regular
files; links and special files are refused without waiting for a writer.

On HTTP 429, the last successful windows and their original sample time remain
visible, labelled **rate limited** with their age and next retry time. Refresh
also respects `Retry-After` (seconds or an HTTP date); an absent or unreadable
header waits five minutes. A 429 uses a minimum 60-second cooldown even when the
server requests less; it does not require waiting the healthy five-minute interval.
No token refresh or agent message obtains these
limits. The private cache keeps at most 256 account snapshots, four known Claude
windows each and no tokens or request history. Active cooldowns are never evicted
to admit another account. Only expired entries can be replaced at capacity.

**Account marks.** Desktop session cards mark the session's recorded account
with a colored circle holding the first letter or digit of the account's label:
on collapsed rows before the title, and on expanded cards with the label in the
footer. **Color and mark…**, beside **Rename…** in the account's details, chooses
a color and an optional emoji. They are kept with the account on each of its
computers (zerus-sa9qn.6), so every desktop and phone shows the same mark:
`hgs [@host] account mark ID [--color #RRGGBB|none] [--emoji EMOJI|none]
[--if-unset] [--revision REV]` saves them in `account-marks.json` beside
`accounts.json` (64 KiB at most, private; a change that would exceed that
changes nothing), which older `hgs` builds never read because they parse
`accounts.json` strictly. `account ls` then reports
`marks_supported: true` and each row's `mark` (`color` and `emoji`, null when
unset); each `hgs ls` session row carries its recorded account's
`account_mark` when one is set, which reaches phones with the listing. A
malformed file shows no marks and refuses writes until it is repaired or
removed; unknown fields of a newer `hgs` are kept. The desktop saves a mark on
every reachable computer of the account. A computer with an older `hgs` keeps
it as this desktop's preference for that member profile (`accounts/color/` and
`accounts/mark/`), as before. Once, the desktop moves those preferences to the
computers that keep marks with `--if-unset`, which fills only what a computer
has no value for, then forgets its own: if two desktops disagree, the first one
to move its value wins. A computer that is offline or busy is tried again on its
own, after a minute and then less often, up to every 15 minutes. Without a choice the color is a stable palette slot
derived from the label, so every desktop shows the same default. Settings > Sessions >
**Account marks** shows emoji instead of letters for accounts that have one.
**Show account labels on session cards** (on by default) turns the marks off on
every card, and an account's **Color and mark…** dialog can keep its own mark
off session cards (**Do not show this account's label on session cards**,
stored per member profile as `accounts/hide/`; the account's details then say
**Hidden on session cards**). Hiding is presentation only: labels, colors and
account bindings stay as they are. A
session without a recorded account binding, or whose profile is not in the loaded
catalog, shows no mark. The session list reads each live computer's account
catalog once, without profile inspects, to resolve labels.

**Add account** creates a separate profile on the selected host and starts native
sign-in. Default/native profiles retain existing authorization. Its agents use New
session's rows: a large colored provider logo, the agent and what adding does.
A removed profile can be added again with its saved sign-in. Choosing another agent
never resizes the dialog or moves its fields: **Account** and **Account name** stay
in place and are disabled when they do not apply, and the hint keeps room for its
longest text. OpenCode has one native configuration per machine: **Rename…**
changes only its name in Zerus, **Remove from…** takes it out of Accounts and New
session there, keeping its configuration, credentials and sessions, and Add
account brings it back. Its row is unavailable while it is listed or not
installed. **New session**
shows one list of **Account and agent** pairs for the chosen computer on desktop
(including macOS) and Android. A large colored provider logo spans two text lines: the
account email and compact cached limits, or **limits n/a**. Desktop reuses the
status footer's compact meter widget, including the selected usage indicator;
Android shows the same usage/elapsed rails and pace colors. Default labels, profile
IDs and quota error details are omitted from these rows. The account list uses
the same provider identity and organization grouping as **Agent accounts**, with
bindings only from the selected computer. Aliases of one account share a row;
conflicting account IDs, organizations and unidentified profiles stay separate.
The selected, remembered or default local profile remains the exact launch binding.
Rows show cached limits at once. On desktop, opening the chooser also asks for
current limits of the chosen computer's accounts, except those checked within
the last five minutes, and **Refresh** asks for all of them again. Replies update
rows in place: rows group once per loaded catalog, and the list keeps its rows,
selection and scroll position. Changing a selection never probes providers.
A profile's credential fingerprint follows its signed-in account, so token
refreshes (Kimi's every 15 minutes) and Claude Code's settings writes keep cached
status and limits; sign-out or another account discards them. **Run shell** is a single desktop
separate choice. The last successfully launched agent and account per computer
remain remembered; without that agent on the computer, the first agent is chosen.
Focusing the list while accounts load never chooses **Run shell**. **Make default on…** sets
the default for new sessions; explicit `--account` wins, and attach, resume and fork
retain the original profile. Ordinary agent commands outside `hgs` keep their
native sign-in. Use `hgs [@host] account default native-claude` to restore the native
Claude default; removing a profile also removes its default assignment.

```sh
hgs account ls
hgs @mac account add work --provider codex --label Work
hgs @mac account login work
hgs @mac codex /path/to/project --new --account work
hgs account copy native-codex --from @local --to mac --as codex-work --label Work
```

`account ls` advertises `setup_revision: true` when terminal setup accepts an
optional `--revision REV` on `account login`, `account install` and `account start`.
That check runs when the terminal command executes, before provider work. Calls
without the option retain their existing behavior. Next uses the capability,
exact computer identity and fresh catalog snapshot; unsupported or uncertain
setup is never replayed.

Signing in on another computer (`hgs @mac account login work`, or **Sign in** for
an account there) runs that computer's native sign-in in a terminal here. Codex
finishes its browser sign-in on the signing computer's `localhost:1455`, which a
browser on this computer could not reach, so the SSH connection forwards this
computer's port 1455 there (`-L 1455:localhost:1455`). Open the sign-in link
Codex prints in a browser on this computer. The connection has its own SSH
session, never a shared master, so the forward ends with the sign-in. If port 1455
is already in use here, for example by another Codex sign-in, SSH only warns, and
the browser cannot finish until that sign-in ends.

`accounts.json` stores metadata and selected permission modes; authorization stays
in the profile's own directory. Managed profiles clear inherited API tokens;
native defaults keep the user's normal environment. Tokens are not displayed in
UI, and the mere presence of a file does not prove valid sign-in. Claude managed
profiles support claude.ai login; shared Claude Console authorization is not
isolated by this mechanism. Completed native sign-in finalizes profile setup
without altering permissions or workspace trust.

Managed Claude profiles inherit behavioral settings from `~/.claude/settings.json`,
including permission default, model and effort, at launch/resume. Profile overrides
remain. Auth, API keys, provider endpoints, directory overrides and other hooks are
not copied. OAuth, token renewal, limits and connectors stay native.

Managed Codex, Claude and Kimi profiles also use the user's MCP servers. Each
launch/resume merges `mcp_servers` from `~/.codex/config.toml`, `mcpServers` from
`~/.claude.json` or `~/.kimi-code/mcp.json` into the profile, including their
headers. A server that the profile added, changed or removed keeps its own state;
the profile's `.hgs-inherited-mcp.json` records only digests of what was inherited.
Provider sign-in, hooks and other settings are not shared this way.

**Permissions → Change…** sets provider defaults or bypass for future launches
and resume on that host. Explicit launch flags take precedence. Current agents
continue unchanged; account labels describe launch settings, not every running
session's effective permissions. Bypass maps to Codex full access without sandbox,
Claude `bypassPermissions`, Kimi Never Ask/`--auto` and DeepSeek's native
`danger-full-access`. The CLI is `hgs [@host] account permissions ID --mode
provider|bypass`. Permissions do not accompany copied sign-in credentials.
Resident legacy DeepSeek bridges also support this mode through the official
authenticated web API, without restarting the host or changing other sessions.
Capability checks precede new binding creation. If permission setup is not
confirmed, sending stays disabled; explicitly resume the same session to verify
and finish setup. A confirmed native preset is not applied again after an
uncertain acknowledgement. Already-running sessions keep their existing mode.

**Copy existing sign-in over SSH** copies Codex/Kimi credentials and necessary
provider/model settings into a new profile, without replacing another one or
copying history, tools or hooks. Claude's system Keychain cannot be transferred:
sign in on the destination. Providers can require new sign-in even after copying.
**Remove from…** requires confirmation naming the account and machine. Native
history and active session ownership remain separate from catalog removal.

On macOS, HGS does not unlock Keychain. The per-user
`com.hgdev.hgs.user-session` service allows a same-UID HGS process started through
SSH/tmux to join the existing graphical security session before Claude login,
inspection or launch. It passes system session authority, not passwords or tokens,
starts on demand and exits after a minute idle. A logged-in desktop session is
required; screen lock differs from logout. If Keychain is actually locked in that
session, unlock it there. Authentication diagnostics distinguish these cases.

<a id="local-models"></a>

## Local models and OpenCode

Ollama runs model inference on the chosen agent computer. The native harness
owns tools, permissions and conversation state. Codex can use a local Ollama
model with its existing adapter; see the [qualified local-model example](local-models.md).

Choose **OpenCode** in New session to use OpenCode 2 through its shared
background service. Select an offered `provider/model` (keyless free models are
listed when the service offers them) and, where the model has variants, an
effort, or retain the default model. Supported: text and voice conversations,
queued messages, Stop, native questions and permissions, status and history,
model and effort change, rename and forgetting the Zerus binding. The desktop
Terminal tab opens OpenCode's native TUI on the existing service and conversation,
also available as `hgs a NAME --existing --run-id RUN`. Disconnecting closes that
client; the service and conversation keep running. Opening Terminal never starts
or replaces a service and disables the attach client's automatic updates.
This interactive capability is separate from pane snapshots: mobile Terminal
snapshots remain unavailable. Pause/resume, attachments, compaction, fork and
child sessions are unavailable.
See [OpenCode versions, service and controls](opencode.md).

From the CLI, `hgs [@host] opencode DIRECTORY --new -n TAG --model PROVIDER/MODEL`
starts a session. `--effort VARIANT` needs `--model` and must be one of that
model's `effort_options` (listed by `hgs account ls`); when omitted, the
model's default is used. A launch starts the shared service when none is
running, and `hgs [@host] account start native-opencode` starts it explicitly
(**Start OpenCode**). Inspection, listings and sending never start it. OpenCode's
official user install, `~/.opencode/bin`, is discoverable from GUI, service and
SSH launches; an `opencode` found earlier on PATH keeps precedence.

`hgs [@host] account favorite native-opencode --model PROVIDER/MODEL --favorite on|off --request-id UUID` stars a model on that computer (at most 64; stored in `opencode-favorites.json` beside the account catalog). A retried request identity returns its first change, with today's favorites list.

`hgs [@host] account models native-opencode [--location DIR] --json` lists every available model of that computer and folder (connected providers only, at most 10,000 models and 4 MiB; `models_truncated` reports a cut). It never starts OpenCode; with the service stopped it reports `not_running` and, for the home folder, the last known short list.

<a id="deepseek"></a>

## Official DeepSeek Harness

The native adapter accepts exactly the reviewed `@deepseek-ai/dsh` releases
`0.2.1-rc.2`, `0.2.1-rc.5` and `0.2.2-rc.1`. Install a reviewed release on the agent host,
sign in and choose **DeepSeek Harness** in New session:

```sh
npm install -g @deepseek-ai/dsh@0.2.2-rc.1
hgs account login native-dsh
hgs dsh /path/to/project --new -n work
```

Zerus launches the official native engine and its tools, access rules and history.
A versioned adapter connects its Session Controller through a private Unix socket.
The official web host uses a separate `hgs` profile through `--patch`; sign-in and
secrets stay in `DSH_HOME`. The web endpoint is loopback-only; remote access uses
SSH. `~/.npm-global/bin` is discoverable from service and SSH launches.

If the adapter endpoint disappears while the recorded original host is still
alive, Zerus refuses another launch and leaves the original running. New
launches record the kernel boot identity along with the native PID and birth,
so an older boot cannot identify a reused PID and birth ticks as its live host.
A dead original or a verified reused PID permits normal startup. Fresh legacy
PID-file evidence still protects a live or unverifiable process.

Before spawning, Zerus records the exact launching process and a preparation
boundary. An ended launcher is recoverable on the same boot only before the
possible-spawn boundary and with unchanged PID-file evidence. Once a host might
have started, its missing PID file or ended launcher cannot authorize another
start. The error directs you to the existing native UI; save any running work
before restarting the computer if a possible start remains unconfirmed. A later
boot retires that old attempt while retaining checks for newer legacy evidence.
Known spawn refusal restores the previous identity proof; uncertain starts do
not roll it back. Incomplete or older starting records without a boot/launcher
identity remain unverifiable, rather than being assigned a synthetic identity.
A healthy resident adapter reconnects without a restart or record rewrite.
These checks never stop a host or reload its bridge.

Reviewed adoption of `0.2.1-rc.5` (2026-10-29, `zerus-r7d4`): the rc.2-only
startup gate rejected rc.5 before starting the host. Comparison of the official
npm packages found the launcher's code, boot/profile handling, web listener,
browser authentication and Session Controller runtime unchanged. In particular,
`--profile hgs`, `--from-default-profile web`, `--patch`, `--no-open`,
`--host 127.0.0.1` and `--port 0` retain their meanings. The web host still
exchanges its loopback launch token for an authority-bound cookie with HTTP 303.
The adapter's private socket, protocol 1, per-host random generation and native
methods are unchanged. Its ping version remains `0.2.1-rc.2`: that identifies
the resident adapter, independently of the installed harness. Existing rc.2
hosts remain accepted without a restart or extension reload.

Reviewed adoption of `0.2.2-rc.1` (2026-11-05, `zerus-o8ftr`): model selection
moved from the controller's selection map into each native Agent's model object.
The adapter uses its pure `current()` read and session-local durable `select()`
write. It still serializes settings through the same native image-admission
chain; it never calls the public model command that also saves a profile default.
The new model interface is checked before the native Agent setup is published,
and a partial or missing interface refuses. The rc.2/rc.5 selection path, resident
adapter identity and existing-host reuse remain unchanged. An owning host is
never hot-reloaded to adopt a new bridge. CI continues to resolve and verify one
official latest release, with no historical fallback.

Ordinary forks in `0.2.2-rc.1` no longer name their parent in the native header. Zerus
pins a completed source event boundary before forking and verifies the exact
inherited prefix and native seed marker before accepting the new identity.
The original parent-header check remains for rc.2/rc.5. Schedule tasks are now
Host-wide: Zerus still lists and edits only a task whose explicit destination is
the selected session, checks the original record and uses the native queued
`expectedUpdatedAt` comparison. Creator/editor membership does not grant access
through another session. Delivery-history pages omit other destinations.

rc.5 adds a UI entry-customization plugin and changes conversation, settings,
analytics and model-list UI. It shares model-listing/cache code between account
and API-key providers; the default official API-key route now follows `/models`,
and the bundled official models advertise a 1,048,576-token context window. MCP
resource listing returns an empty list when the server has no resource
capability. These changes preserve the controller operations used by Zerus;
isolated contracts exercise both reviewed releases with a local keyless model,
including native startup, browser authentication and account-key handling.
Package URLs and SHA-512 integrities are recorded under `dsh` in
`scripts/ci/agent-compatibility.json`. That is review history: CI still resolves
one official latest release, with no fallback. An unknown version still refuses
startup pending review.

rc.5 does not resolve the remaining dependency advisories in `zerus-s7nn`:
its dependency tree still contains `@modelcontextprotocol/client` 2.2.0
([GHSA-6prh-2h8m-c8cw](https://github.com/advisories/GHSA-6prh-2h8m-c8cw),
fixed in 2.3.0) and `fflate` 0.8.2 nested under `@deepseek-ai/libreoffice-kit`
([GHSA-px8p-9vwx-vf98](https://github.com/advisories/GHSA-px8p-9vwx-vf98),
fixed in 0.8.3). The separate top-level `fflate` 0.8.3 does not replace that
nested copy. This adoption introduces no dependency overrides.

A new session waits for its first message. API-key `deepseek-official` connections
work independently of account sign-in. If `deepseek-account` needs login, Activity
shows **Sign in** and keeps the draft until native authorization finishes.

An API-key account shows its balance in place of limit windows. When the key
comes from the native credentials file (`DSH_HOME/.credentials.yaml`), `hgs account
inspect native-dsh` reads the official `GET https://api.deepseek.com/user/balance`
at the usual account-status refresh, with no additional polling. The private
Node.js reader parses the key with the installed harness and sends it only to
`api.deepseek.com` as a bearer token: redirects are refused, the request times out
after 5 seconds and the reply is bounded to 64 KiB. The key is never logged,
cached or returned. Per currency (USD or CNY), the inspection reports a `Total`
balance and its `Topped up` and `Granted` parts as `balances` rows, plus
`balance_available: false` when DeepSeek says the balance no longer covers API
calls. The status stays `configured`. Accounts, Overview, New session and the
session footer show the Total as a bare amount such as `$6.20` (`¥` for CNY;
another currency keeps its code). Its tooltip and accessible label name it in
full, for example `Balance: 6.20 USD` with the Topped up and Granted parts. The
account's usage panel lists all three amounts and warns about an insufficient
balance. A key from the launch environment, an
unreachable API or a rejected key leaves `configured` without a balance. A
DeepSeek account sign-in keeps its `Wallet` and `Bonus` balances from the native
web host. DeepSeek publishes no usage or cost endpoint for API keys.

Each refresh with an API-key balance also records that reading in a private
journal (`account-spend/` in the hgs configuration directory), kept per account
profile and key identity. It makes no additional network requests. The usage
panel shows **Spent today** and **Spent this month** across the whole account
(every key, tool and computer), computed from balance decreases since the first
reading:
- An interval in which the topped-up balance rose contains a top-up, so it is
  left out.
- A decrease in the granted balance counts as spend but is named separately,
  because an expiring grant looks the same.
- An interval without readings is counted on the day it ends, and the panel
  names the previous reading.
- A currency missing from one reading keeps its previous baseline, so its
  spend counts from its own last reading.

The journal keeps the last reading and up to 400 days of daily totals. Days
follow the computer's local time.

The panel also lists requests and cache-read, uncached-input and output tokens
per model. It shows today, this month and each of the last 31 days, from this
computer's native session logs (`DSH_HOME/sessions`). hgs only reads these logs:
- A private index (`account-usage/`) remembers each log's identity, size,
  modification time and last complete Zstandard frame. A refresh reads only
  appended frames, newest logs first, within 32 MiB and 2.5 seconds. That budget
  also covers the identity check (up to 4 KiB per new or changed log). The rest
  follows on later refreshes, and meanwhile the panel says totals may grow. A
  log whose check waits keeps its previous counts.
- A malformed or unreadable log is set aside until it changes. The panel says
  how many logs could not be read and are not counted.
- A forked session counts only after its inherited prefix (`session/end-seed`
  with `inherited: true`). Only each session's current format generation
  counts. A log is read again from its start when it is replaced or shortened,
  rewritten without growing, or rewritten in its first 4 KiB or in the 4 KiB
  before the counted offset.
- Logs untouched for 31 days are not opened.

Other computers and tools using the same key are not included. No cost is
estimated from token counts, because peak hours, holidays and price changes make
such estimates brittle; the balance journal records real spend.

**Native UI** opens the official web interface and the host workspace's native
session list, including sessions created through Zerus. Native cookie exchange
supplies authentication, and ports/tunnels are discovered automatically. The
header always opens it in your browser. On Linux, the optional embedded view
shows it as a tab beside Activity: navigation preserves its screen and draft,
embedded cookies and cache are memory-only, and questions and approvals for
Zerus-managed sessions remain in Activity (**Respond in Activity** points there
when needed). The embedded view is the plugin
`<prefix>/lib/hgs-tray/plugins/libhgs-tray-native-ui.so` (`/usr/lib/...` from
Arch packages, `~/.local/lib/...` from `tray/install.sh`), which alone needs Qt
WebEngine (`qt6-webengine` is an optional dependency on Arch). The GUI loads it
when a DeepSeek session is first shown. Without it, or on macOS, there is no
Native UI tab, and Sign in, Open terminal and the other Native UI actions open
the browser instead; Zerus on Windows does not offer the native UI
([Windows](installation.md#windows)). Builds choose with the CMake option
`HGS_NATIVE_UI_PLUGIN` (`AUTO` by default: built when Qt WebEngine is found).
`hgs native-ui SESSION --json` returns a private connection URL containing a token;
keep that output private.

Activity supports creation, messages/images, busy-session submission, questions,
approvals, per-session model/effort, pause/resume, rename, goals, todos and child
views. Children are continuable and run independently of the turn that created
them: the engine records a child's native identity when it creates the child,
and stopping the parent ends the parent's turn while the child keeps working. A
new child view stays empty until the child's first step. A one-shot child stored
by 0.2.0-rc.2 and cancelled before its first step stays listed with an empty
view. A child without a valid identity reports native corruption. A
child view shows the child's own model, which reading never changes. Request
receipts prevent duplicate submission. Uncertain delivery requires inspection.
Model changes do not replace profile defaults.

Account launch defaults (zerus-v7tqa): when the DeepSeek account has a saved
model and effort, Zerus applies them to a session it has just created, after
creation and permission setup and before any message, through the same scoped
`settings` method as a per-session change. It binds the exact session identity
and host generation, so a resident host keeps its adapter and is never reloaded;
a host that changed in between is not asked, because that call would resume the
session. The binding records the outcome as `account_defaults` with `status`
`applied`, `failed` (the host never applied it) or `uncertain` (the reply was
lost, so it may have applied), and `hgs inspect SESSION` reports it. A failed or
uncertain application never fails the launch or blocks the session: `hgs`
reports it on standard error once and never retries it, and the session keeps
the native default. The desktop marks such a session's model and effort
selector with a warning sign and explains the outcome, the account's choice and
the reason in its **Model and effort** dialog (zerus-fw3g5); an applied or still
pending outcome shows nothing, and the dialog never re-applies the choice. Relaunching an existing name or resuming a session never
applies account defaults, and the native profile default stays unchanged.
Without an account choice, a new session starts on the native default
including its configured effort; a session's own recorded selection still wins
when it resumes. A settings change that resumes a paused session applies the
account permission mode, as **Resume** does (hosts started by this version).
A child keeps the model it
inherits and takes messages only through its parent; the adapter refuses model
changes, messages, compaction and interrupts addressed to the child itself.
Stopping one session does not terminate neighboring agents or the shared host.
Closing Zerus leaves the host running; Forget retains native history. Linux user
systemd runs the host in an independent scope so GUI service restart cannot kill
the engine.

**Fork session…** uses the official Session Controller fork. It copies the
conversation through its latest completed turn into a new native session of the
same profile and folder (including its permission preset),
named after the fork, and binds the exact session ID the engine returns; the
source keeps its identity and history. The engine, not Zerus, holds the new live
agent until its host next starts: Zerus sends its messages, while its questions,
approvals and Pause stay in Native UI until then. A fork needs a completed reply.
If the reply is lost, forking again under the same name adopts the result from the
same host; after a host change Zerus refuses to repeat it. A native failure after
creation can leave an unbound session in Native UI.

Content search covers bound DeepSeek sessions through the native store's
literal, case-insensitive event scan, which works without the optional full-text
index. Results contain the person's and the agent's text only; injected context
and reasoning are excluded. Searching never starts, resumes or reloads a host.
Hosts are asked at once, each scanning for at most 5 seconds; a stopped host, an
older adapter or a host that does not answer within 7 seconds reports that
history as unavailable.

The official Schedule plugin (**Automation tasks** in Native UI Plugins) is
listed and managed per exact session. The reviewed web bundles include it,
and the profile Zerus creates layers that bundle, so it is on unless the
profile's own patch disables it:

```sh
hgs schedules dsh/project/work            # tasks of this session as JSON
echo '{"request_id":"UUID","expected_run_id":"RUN","expected_conversation_id":"session-ID",
  "expected_generation":"GENERATION","action":"update","id":"schedule-ID",
  "expected":{...record from the list...},"title":"New name"}' | hgs schedules dsh/project/work --json
```

`action` is `delete`, `update` (the native compare-and-set; `title`, `prompt`
and the native `change` timing object) or `history` (`limit` 1-100, optional
`before`). Results keep native codes such as `schedule_conflict` and
`schedule_not_found`. None of these activate the session. Without the plugin the
command refuses and listings omit `schedules`.

Account transfer stays in Native UI. Native context reset awaits a supported
Session Controller operation. Resident adapters advertise each operation; one
started by an older Zerus keeps fork and schedules in Native UI and its history
unavailable to search until its next ordinary start. Other harness versions need
contract verification; updates never restart live hosts or agents automatically.
`python3 tests/test_dsh_native.py -v` exercises the official engine with a local
keyless model, covering history, lifecycle, questions, settings, fork, search,
schedules and browser login.

## Automatic recovery

**Settings → Automatic recovery** shares rules for temporary overload, rate limits,
session usage limits and network failures. Automation is off by default. Each class has its own delays:
`15, 30, 60, 300, -1` stops after four attempts; `15, 30, 60, 0` repeats the previous
60-second delay indefinitely after the first two attempts; `-1` alone does nothing.
Small jitter is applied. Class counters persist within an episode when errors change.
Temporary overload covers HTTP 500, 502–504 and 529, model capacity errors and the
520–524 gateway failures that Cloudflare reports for an unreachable or timed-out
provider, often without a response body.
Kimi's explicit HTTP 429 message about its rolling 5-hour window uses the
**Rate limit** schedule, including when the message calls it a quota and offers
an upgrade. It provides no reset timestamp, so recovery does not invent one.
Monthly quota, billing and sign-in failures still require manual action.

**Session usage limit** is separate from a temporary rate limit. Its default mode,
**Wait for reset**, schedules a continuation at the provider's reset time, using
the failure's date and reported timezone. The toolbar shows that reset time.
If the deadline is unknown, ambiguous or was already reached when the turn failed, recovery needs attention
instead of starting short retries. **Retry on a schedule** uses the same delay
syntax as other classes: `300, 0` retries about every five minutes until stopped,
even before the reported reset. Explicit native Retry-After deadlines still apply.
Existing shared policies keep their settings and gain the reset mode by default.

Native agent retries finish first. Claude, Codex and Kimi receive an explicitly
marked `[HGS automatic recovery]` continuation in the same conversation; HGS does
not replay the original request or tool command. The newer DeepSeek adapter retries
the failed model request; an older host uses continuation messages. Sign-in,
balance, other quota and context exhaustion require manual action. Questions, new input,
Stop, session termination and identity changes cancel waiting; a native draft
blocks delivery. Unknown providers do not receive automatic input.

A continuation goes only to an agent that the failure stopped. When the agent
keeps working after a provider error that followed an accepted continuation, the
episode returns to **Recovery in progress** with the same attempt count: nothing
further is scheduled or delivered while it works, and its answer completes the
episode. An account has one recovery request in flight at a time, until the agent
accepts the continuation and makes progress, so a long working turn does not hold
the account's other sessions at **Waiting to retry**. From that progress the job
records `resumed_at`, and desktop and mobile show the turn as ordinary work rather
than **Recovery in progress**. The episode stays open until the turn ends, so a
further provider failure in that turn reschedules on the same attempt budget.

Provider failures replace Working or Compacting with a specific state such as
**Usage limit reached**, **Model at capacity** or **Sign-in failed** and appear
under **Needs attention**. Activity explains the stopped turn and offers
**Open Terminal** and, for exhausted quota, **Refresh usage**. Restore account
access or wait for the reset, then explicitly send a message to continue. Drafts
are kept; refreshing usage never retries a turn. Account percentages alone do
not change a running agent's state. Failed refreshes and offline snapshots are
labeled as last reported usage.

Claude's **What do you want to do?** usage-limit menu appears in Activity even
when the failed turn or a background shell leaves hook state at Error or Working.
Choose the native stop, wait-and-continue, or usage-credit option explicitly;
there is no preselected answer or automatic approval. Zerus verifies the exact
process, conversation and complete current panel before each navigation key and
one confirmation. Usage-credit setup continues in Terminal for any further
billing consent; unfamiliar account choices are shown read-only there.

After an early reset, fresh complete account usage below the limits shows
**Usage available** beside the composer and explains how to continue. An open
native limit menu still needs an answer, and a stopped turn still needs a new
message. Stale, offline or failed usage reads cannot produce this notice, clear
the native failure or trigger a retry. Claude's own wait continues under its
native rules; Zerus does not change the shared recovery policy or enable credits.

Codex failures come from the exact conversation's native error journal and
structured failed turn completions, including compaction failures. Tool output
and quoted error examples cannot change the session state. New native progress
clears the failure; changing the account counter alone does not.
Matching Codex log and completion records retain their exact failure references,
so delayed duplicate evidence keeps the same recovery episode, deadline and
attempt budget. Separate native turns remain separate even for identical errors.
A previous cancellation caused by switching those sources can resume only with
confirmed matching native evidence and zero delivery attempts; user cancellation,
new input, interruption, changed identity and uncertain delivery still stop recovery.

A chip in the toolbar above the message field shows the countdown; its popover
shows the attempt number, **Attempts**, **Retry now** and **Cancel retry**. Retry now respects a known Retry-After and the reset deadline in reset mode. Policy edits apply to new
episodes; already scheduled episodes retain their delays and session-limit mode. Disabling recovery
cancels automatic continuation. Uncertain delivery stops the episode rather than
sending Enter again.

A fully submitted Codex recovery continuation can wait up to ten minutes from
its original dispatch for the native agent to acknowledge it. The durable
terminal receipt must match the exact request, conversation and continuation;
the receipt alone never establishes native acceptance. During this bounded wait
the UI shows **Waiting for delivery confirmation**, no additional input is sent,
and input, policy and identity cancellation guards remain active. The account's
single recovery slot is retained until acknowledgement and progress, cancellation
or expiry. Other providers and unproven, partial or interrupted deliveries keep
the 30-second confirmation cutoff; legacy uncertain episodes remain stopped.
The native hook confirms the latest submitted continuation text, not a request
UUID embedded in that text: it cannot distinguish identical text independently
typed in Terminal. Zerus never uses transcript text alone to acknowledge delivery.

An independent `hgs-recovery.service` / `com.hgdev.hgs-recovery` worker runs without
GUI. Custom installs can run `hgs recovery worker`. The same worker runs
[Keep cache warm](#keep-cache-warm) rounds, whatever the recovery policy says. Episodes and receipts live on
the agent host under `sessions/recovery/`, guarded by policy revision and a single
executor lock. CLI operations are `hgs [@host] recovery get|set|sync|action`, with
JSON writes on stdin. Adapters register explicitly in `src/state/recovery.rs`.
On Linux, client policy refreshes start the configured user service without
restarting an active executor. A detached worker remains the fallback when no
installed service or usable user manager is available; refreshes start none
while a worker runs. The service's own worker (the job's process as launchd
reports it, or the unit's main process as systemd reports it, not another
process in the unit's control group; only when systemd cannot answer does the
control group decide) never exits because another worker holds
the executor lock, which would make the service manager start it again every 10
seconds: it waits for the lock, and a detached worker hands over to it between
rounds.

Open Zerus clients synchronize policy versions over HGS/SSH every 15 seconds;
offline machines catch up later and executors keep their saved local policy.
Revision and writer identity establish deterministic order. Saving a stale form
requires Reload. No external cloud service is involved.

<a id="processes"></a>

## Processes launched by agents

**Processes** is off by default; enable it in **Settings → Processes**. It appears
beside Activity and Terminal, or Native UI for DeepSeek. When disabled, its list,
count and output are not polled; normal Activity, fleet and account updates continue.
The small **Background** summary is part of ordinary fleet polling even
when this tab is disabled. Full process history and output remain opt-in. A
background session's **Session actions** menu offers **Show running commands** when Processes
is enabled.

**Live shells / processes** shows verified shells and independent command trees.
Recorded tool calls appear separately, with completed history collapsed. Equal
command text does not prove that a tool record and OS tree are the same task.
Rows expose owner, state, elapsed time and available folder/progress/output. OS
trees show PID and child count; their stdout is not independently captured.
Saved commands retain a UTF-8 preview of at most 512 bytes and a SHA-256 digest
(`command_digest`); `command_truncated` identifies shortened previews. Processes
and the CLI keep the preview in `command`. **Copy command** copies that displayed
preview; command output and process identity/control are unchanged. Legacy full
texts still load and are compacted on their next live or archive record write.
For legacy jobs the digest covers the previously stored text, which older
versions may already have clipped.
The active count uses the maximum of confirmed tool records and OS trees, not
the sum of overlapping sources. Unconfirmed records do not count as active.

| Agent | Observation and output | Targeted control |
|---|---|---|
| Claude | Hooks, Bash/tool_result, backgroundTaskId and native output files | Verified live OS trees |
| Codex | Hooks and rollout CommandExecution, including code-mode and write_stdin | Verified live OS trees |
| Kimi | Hooks, main/child process-task registry and tasks/ID/output.log | Verified live OS trees |
| DeepSeek | tools/execute and ctx.jobs in the native bridge | Session-owned native jobs |

**Stop selected** verifies every selected target, run, conversation and ownership
before dispatch. Native jobs also require host generation and job identity. OS
trees require PID/start-time and ancestry checks, use TERM and escalate the selected
tree to KILL after two seconds if needed. Linux uses PID handles; macOS rechecks
start times. The agent and neighboring trees are excluded. Stale generations and
offline snapshots cannot stop a newly reused target. Requests are sequential,
with no automatic retry; identity changes cancel unsent ones. **Stopping** remains
until completion is confirmed. Whole-turn Stop in Activity is a separate action.

`hgs [@host] processes SESSION --json` reads records. `--output ID` and `--stop ID`
require `--run RUN --conversation ID`; native jobs additionally use
`--generation GENERATION`. Archives accept `--archive ID` for read-only history.
Older DeepSeek hosts retain history until their ordinary next start; live registry
and targeted stopping need the newer bridge. GUI updates do not restart hosts.

Default intervals are 2.5 seconds for open Processes, 30 seconds for background
counts, ten seconds for Unconfirmed output and 0.5 seconds before a loading
indicator. Detailed polling covers only the selected visible session. Output is
read only for the visible selected command, immediately on opening, and refreshed
on meaningful state/output changes. Disabling the feature uses
`hgs inspect --skip-processes`; explicit CLI reads remain available. Terminal/Native
UI inspection also follows the background interval. These reads use local state,
OS/native registry or SSH, never a model call.

Lost foreground hooks become Unconfirmed rather than staying Starting through
another command. Codex completion beyond the normal 8 MiB transcript window can
be read incrementally up to 16 MiB per poll and cached privately. Completion needs
the exact call ID in the verified conversation; absence of an event is not success.

## Terminal, SSH and clipboard

The built-in Terminal is a real PTY client attached to an existing tmux session,
locally or over SSH. It supports ANSI/VT, UTF-8, colors, fullscreen applications,
keys, selection, scrollback, paste and resize. Connect, Disconnect and Reconnect
change only that client. Closing the window detaches it and leaves the agent alive.
The client uses `hgs a SESSION --existing [--run-id ID]`; disappearance does not
restore a session automatically. Ordinary `hgs a SESSION` retains Saved restore
behavior. The vendored terminal engine is libvterm 0.3.3 under MIT; builds do not
download it. Rendering updates changed cells and the cursor rather than scanning
the full screen for every small change.

Dropping files into Terminal inserts paths without Enter, quoted for the session's
machine: in single quotes on Unix machines, and with backslashes for a session on
this Windows computer, in double quotes when a path has spaces or shell-special
characters, or in single quotes when it has `$` or a backtick, which PowerShell
would expand inside double quotes. Remote drops first copy files to the session host, limited to eight
files, 10 MiB each and 20 MiB total. The copy is bound to the session's run and
conversation. An untracked shell (`hgs sh -d`) has neither: its copy is bound to
the live session's creation time and refused when a tracked run owns the session.
Changing tab, session or connection during transfer cancels insertion. Zerus
Next on Linux and macOS frames the quoted paths as one literal terminal paste,
so path characters are not individual native approval shortcuts. It still sends
no Enter. Generic typing, text paste and Windows file-drop framing are unchanged.
**Open externally** and **Copy connect command** retain access from other clients.

In Zerus Next on Linux and macOS, an image-bearing browser paste event stays in
the selected Terminal. On Arch with WebKitGTK, use **Shift+Insert** for an image
on the clipboard; Ctrl+Shift+V retains the engine's text-paste behavior and
Ctrl+V remains the native agent's key. Its captured image is normalized to PNG (at most 10 MiB
before and after conversion, and 32 megapixels), then copied through the same
run/conversation-bound staging command even for a local session. One quoted
retained image-file path is inserted without Enter; the composer and Activity
are unchanged. Accompanying text is not inserted. Unreadable image data, unknown
identity, a changed/hidden/remounted terminal or an unconfirmed transfer inserts
nothing and is never retried automatically. This inserts a file reference; it
does not prove the native agent imported the image. Windows image-event staging
and non-POSIX peer paths are unsupported. The quoted image path uses the same
literal-paste framing as POSIX file drops. Existing text paste, native Ctrl+V
keys and the text-only application Paste menu are unchanged.

On Unix, **Terminate** can close an untracked Shell row after confirmation. The
confirmation captures the tmux socket and server, session ID and creation time,
and the sole pane's ID, PID and process birth identity. The owning CLI rechecks
those facts under its state lock and closes only that terminal, then verifies the
original shell process exited. Tracked agent rows keep their separate verified
termination path. An untracked shell creates no archive and never offers worktree
or cache removal; its working folder stays, and detached jobs may continue. If
the terminal changed or cannot be verified (including multiple panes, grouped
sessions or linked windows), refresh the row or open its terminal and use `exit`
after its jobs finish.

If SSH drops, tmux on the remote host continues running. Reattach normally. If the
local tmux server dies, its sessions are gone; `hgs` restores the terminal mode and
reports this instead of leaving the alternate screen active:

```text
hgs: the tmux server died; terminal restored, sessions on this box are gone
```

A client failure while tmux remains alive does not trigger terminal reset sequences
that could corrupt the normal screen. Saved conversation bindings can still be
restored after server loss, but tmux is not a reboot-persistence mechanism.

On Linux, tmux built with systemd support puts each pane in a `tmux-spawn-*.scope`
that is `PartOf=` the unit holding the tmux server, so stopping that unit ends every
pane, whatever its `KillMode=`. When `hgs` runs in a user service manager unit (the
desktop GUI, the mobile connector, the recovery and swarm workers, a desktop
terminal) and no server answers on the selected socket, it runs the `new-session`
that starts the server through `systemd-run --user --scope`, in a transient
`hgs-tmux-<uuid>.scope` that holds only tmux and is collected when the server
exits. Stopping or restarting the unit that launched the first session then leaves
the agents running; stopping that scope itself ends the server and its agents. A
running server is never moved, restarted or replaced: one started earlier, by an
older `hgs` or by a plain `tmux` command, stays in its unit until it exits.
Outside a user unit (an SSH login, macOS), without a reachable user manager or
`systemd-run`, or when systemd cannot create the scope, `hgs` starts tmux as
before.

The optional `tmux.conf` binds Ctrl-V to `hgs paste` for the remote image clipboard
bridge. At attach, `HGS_CLIENT` records the viewing host; an SSH hop forwards it
through `--client`. The session host fetches PNG from the client's `hgs clip`, puts
it in its own native clipboard and forwards Ctrl-V so the agent reads its usual
clipboard. Local sessions need no SSH. Errors still forward the key, with a short
status and a best-effort line in `${XDG_STATE_HOME:-~/.local/state}/hgs/paste.log`.
The log is not rotated automatically. Clipboard synchronization is specific to
the image-paste bridge, not general desktop synchronization. A remote host needs
a working graphical clipboard environment; choose the viewing peer explicitly
when several clients attach to the same session.

New sessions created through a peer obtain the host's graphical environment rather
than inheriting an unrelated SSH display. Terminal client environment and macOS
graphical security context are handled separately from agent conversation state.

### External terminal modes

Linux defaults to Konsole with `auto`, `tab`, `window` or `clipboard`. Without
Konsole in `PATH`, it opens the desktop's default terminal through
`xdg-terminal-exec`, or `x-terminal-emulator` on Debian and Ubuntu; the
Konsole-specific `tab` and `window` modes do not apply there. macOS offers Terminal.app or clipboard. A custom
`--terminal` template substitutes `{cmd}`.
Konsole tab insertion needs its security-sensitive D-Bus API and an available
window; automatic mode may open a new window when a usable tab is unavailable.
Enabling that API grants other session-bus processes control of terminal tabs.
Terminal.app remains the native macOS backend; alternate terminal bundles need a
supported explicit launch template, not an executable path guessed from a bundle.

Windows offers `auto`, `tab`, `window`, `console` and `clipboard`. `auto` and `tab`
open a tab in the most recent Windows Terminal (`wt.exe`) window and `window` a new
Windows Terminal window; Windows Terminal starts `hgs.exe` directly. When `wt.exe`
is missing or would change an argument (it re-joins the command line and expands
`%NAME%`), the session opens in a console window instead; `tab` and `window` report
that, `auto` does not. `console` always uses a console window, hosted by
`hgs-tray.exe --console-run`: it starts `hgs.exe` with the exact arguments and no
shell, and stays open with the exit code until Enter when `hgs` fails. A
`--terminal` template (`terminal=` in `tray.conf`) takes the place of every mode
but `clipboard`: Zerus starts the template's own program with `{cmd}` replaced by
the PowerShell command, and a console program gets a console window of its own.
Name a shell that runs that command, for example
`powershell.exe -NoExit -Command {cmd}`. A Linux template from a shared
`tray.conf`, such as `foot -e bash -lc {cmd}`, does not work on Windows. A
`tray.conf` shared with Linux keeps its mode; a macOS mode such as `terminal`
falls back to `auto` with a warning.

Clipboard mode copies the shell-safe connect/restore command instead of launching
a terminal. Explicit **Open terminal** still opens one without changing that mode.
Linux uses its desktop clipboard; macOS and Windows use the native clipboard.
New-session **Open terminal window** remains an independent preference.

On Windows, copied and displayed commands are written for PowerShell: arguments
are single-quoted when needed, `'@host'` is always quoted, and a quoted `hgs.exe`
path gets the `&` call operator. When Windows PowerShell 5.1 would pass an argument
incorrectly (an empty one, a double quote, or a space before a final backslash),
the command is copied as `Start-Process -FilePath … -ArgumentList … -NoNewWindow -Wait`
instead. **Open shell in session folder** starts PowerShell (`pwsh.exe` when
installed, otherwise Windows PowerShell) in a folder of this computer: in a Windows
Terminal tab or window that starts there, or in a console window started there when
`wt.exe` is missing or would change the folder (it expands `%NAME%`). A
`--terminal` template receives `Set-Location -LiteralPath FOLDER` as `{cmd}`.

The tray config lives at `~/.config/hgs/tray.conf` unless overridden; on Windows
`~` is `HOME` when set, otherwise the account's profile folder (normally
`%USERPROFILE%`; unlike `hgs.exe`, the desktop app ignores a changed
`USERPROFILE`). CLI flags win at startup; a later tray-menu selection takes effect
immediately and writes the config for subsequent runs. A later launch with the
same explicit flag wins again.

<a id="installation"></a>

## Installation, configuration and diagnostics

See [installation.md](installation.md) for dependency and install commands.
`~/.local/bin/hgs` must exist on peers because SSH calls use that path.
The peer's login shell may be any POSIX shell or fish. Arguments are quoted so
both read them alike, and commands with shell logic, such as the capability
checks of `wait`, `hooks` and `guide`, `hgs machine check` and
`hgs machine ssh --directory`, run under `sh -c`.

`hgs status` only reads. For this computer and each peer it shows the hgs
version, live sessions by what they need (working, needing attention, ready),
stopped and archived sessions, and whether the desktop app, swarm worker,
recovery worker and mobile connector are running. This computer also shows the
running `hgs` executable. A peer whose hgs predates `status` still reports its
version and sessions; update it to see its services. `--local` limits the report
to this computer, `@host status` to one peer, and `--json` prints the same data
for scripts. `hgs version` and `hgs @host version` print `hgs X.Y.Z`, like
`--version`.
Configurations are data assignments, not executable shell scripts:

```sh
HGS_SELF="workstation"       # aliases that identify this host
HGS_PEERS="macbook build-01"  # SSH aliases to poll and target
HGS_TAB=1                    # enable terminal title updates
HGS_TAB_COLORS="macbook=#80ff80"
```

An unavailable peer does not make healthy computers unavailable. The desktop
machine counter includes configured peers that have not connected, with their
connection state in its tooltip. A running background service captures
`HGS_PEERS` at startup; restart the background service after changing it.
Existing agent sessions keep running.

The CLI config is `~/.config/hgs/config`; `HGS_CONFIG_DIR` and `HGS_STATE_DIR` permit
isolated setups. `HGS_SSH` names the OpenSSH client for every peer connection
(dry runs still print `ssh`); without it `hgs` runs `ssh` from `PATH`, and on
Windows `%SystemRoot%\System32\OpenSSH\ssh.exe` first. Runtime storage, `HGS_*`
variables and application/service IDs stay compatible with existing installations. Source is [MIT](../LICENSE); see
[third-party notices](../THIRD_PARTY_NOTICES.md) for dependency licensing.

The GUI installer updates only Zerus and its desktop integration. It never
restarts native agents, tmux servers or DeepSeek hosts. A changed build replaces a
running Zerus through `tray/update-installed.py`, the only supported live update:
Zerus quits as its Quit action does, so drafts are saved, then starts again through
systemd or launchd. Do not stage GUI copies in `~/Applications` or `~/.local/bin` by
hand. The GUI it replaces stays aside until the new one was verified (running, or,
for a GUI that is not running, by its signature and its own `--build-info`) and
only then becomes the one rollback copy in `~/.local/share/hgs/desktop-previous`;
a new GUI that does not start is replaced by it again. A replaced bundle without
its identity, executable or valid signature never becomes the rollback copy, and
a Zerus process started during the update stops the change. Each displaced Qt/unclassified rollback image gets a unique entry under
`~/.local/share/hgs/desktop-qt-legacy/qt-*/` for manual recovery; the replaced
Next takes `desktop-previous`. Older retained images are never overwritten,
deleted or offered by `--rollback`. The settings export stays at its compatible
path. An archive-move failure keeps the image named and retained while the
verified Next update continues its draft/process/provenance checks. Interrupted
or unverifiable replacements remain in a named kept folder. Links at the
installed path or in the rollback folder are refused. The update confirms the
running image, unchanged nonempty drafts and native agent and tmux processes. It records the
installed builds in `~/.local/share/hgs/build-provenance.json`, then removes hidden
`.zerus-*` staging folders left beside the GUI by earlier hand-made updates once
they are an hour old, hold only installer copies and no process uses them. A
failed check leaves Zerus running and prints the private draft snapshot it kept. Zerus quits
gracefully on SIGTERM, SIGINT and SIGHUP; an older running build that does not
is closed from its tray menu, or the installer asks you to quit it first. Use
`HGS_TRAY_RESTART=0 ./tray/install.sh` to stage a GUI update while keeping a running
workspace and its unsaved drafts. The new UI starts on the next launch.

The installer refuses to put an unnumbered build over a numbered Dev
installation (AGENTS.md), right after its build and before it changes anything,
for the live update and `HGS_TRAY_RESTART=0` alike. A numbered Dev installation is
replaced only by a numbered Dev build or by a local preview build: configure
`tray/build` with `-DZERUS_DEV_BUILD_NUMBER=N`, or build the Tauri client with
`ZERUS_DEV_BUILD_NUMBER=N` in its environment, N being the CI sequence of the
main push the build comes from, or build an owner-requested branch preview with
`ZERUS_LOCAL_PREVIEW_LABEL`. The label is at most 32 lowercase letters and
digits in hyphen-separated words; it needs a clean Git checkout and no Dev
number, and the build shows as `DEV_VERSION-preview.LABEL.COMMIT`. The Qt GUI
reads it as a CMake cache variable (configure `tray/build` with
`-DZERUS_DEV_BUILD_NUMBER= -DZERUS_LOCAL_PREVIEW_LABEL=task-board` before
running `tray/install.sh`); the Tauri client reads the environment variable
when it builds. Clear the label before building main again. On either platform,
a switch between the Qt GUI and the Tauri client always takes the live update,
never `HGS_TRAY_RESTART=0`.

Linux installs the binary under `~/.local/bin`, a systemd user service under
`~/.config/systemd/user` and a launcher under `~/.local/share/applications`.
The launcher is not a second autostart entry. Fresh installs and upgrades default
to Zerus Next; `HGS_LINUX_GUI=client` selects it explicitly.
Qt source builds and the Qt installer selection are retired. `HGS_INSTALL_PREFIX` redirects the binary, launch files and
updater state into a scratch prefix and performs no service-manager action; see
[Linux packages and source installation](linux-distribution.md).
The service belongs to `graphical-session.target`, restarts after crashes and
does not loop on exit code 4 (system tray unavailable). Compare
`systemctl --user status hgs-tray.service` and `journalctl --user -u
hgs-tray.service` when startup fails.

Zerus Next does not exit without a tray host: on a desktop without a
StatusNotifierItem host (GNOME without an AppIndicator extension, for example)
it keeps running without an icon, writes "no tray host; open Zerus from the
applications menu" to its journal once, opens from the applications menu and
shows its icon as soon as a host starts. Opening Zerus from the menu while the
service is starting or restarting waits up to 30 s for the service's window
instead of starting a second one; Zerus reads the unit's state with
`systemctl --user show` and never changes it.

The separate `hgs-desktop.service` uses the selected CLI's rendered registration
and readiness contract. `zerus-setup --desktop-service` registers it for an AUR
installation; the Next source installer registers it directly. Current-login
graphical variables are imported before the service starts. A session refresh
restarts only this backend when needed, and UI-only upgrades preserve an
unchanged backend. CLI source upgrades refresh an existing owned registration
without creating a service for a headless CLI-only installation. Package
upgrades perform no user-service actions. Readiness means the matching IPC
endpoint is ready, not that background workers have started. See
[Linux desktop background service](linux-distribution.md#desktop-background-service)
for custom configurations, bare window managers and startup diagnostics.

On macOS the installer uses Qt/Homebrew, builds and ad-hoc-signs an app bundle,
and installs it under `~/Applications/hgs-tray.app` with
`~/Library/LaunchAgents/com.hgdev.hgs-tray.plist`. A graphical login is required
for the tray. Signing precedes bundle replacement; matching code hashes avoid
unnecessary replacement. Ad-hoc signing is not a Developer ID distribution
signature. LaunchAgent crash recovery is independent of native agent engines.
The LaunchAgent runs with `ProcessType` `Interactive` and the GUI thread requests
user-interactive QoS, as for an app started from Finder. Without that key launchd
starts a daemon and clamps all its threads below ordinary CPU load, niced load
included: a busy machine then starves the whole window until the main-thread
watchdog aborts it, and no QoS request from inside the process lifts the clamp.
The watchdog counts only time the Mac is awake and holds its check from the
system's sleep notification until the GUI thread runs again after the wake, for at
most ten minutes: macOS does not schedule that thread during the transition into
sleep or in a dark wake on battery.
`launchctl print "gui/$(id -u)/com.hgdev.hgs-tray"` shows `spawn type = interactive`.
At startup the GUI reads the highest priority its main thread may reach: a login item
(`XPC_SERVICE_NAME` `com.hgdev.hgs-tray`) limited below 31 offers **Fix login item**,
which runs the bundled `zerus-setup --autostart` only after confirmation.
Finder, the Dock and Spotlight start the bundle without the LaunchAgent's `--hgs`
and with PATH `/usr/bin:/bin:/usr/sbin:/sbin`; the desktop then finds the CLI in
the standard install directories listed under **Desktop option** below.

```sh
launchctl list | grep com.hgdev.hgs-tray
~/Applications/hgs-tray.app/Contents/MacOS/hgs-tray --selftest --hgs ~/.local/bin/hgs
```

A PID means the agent is running; `-` means it exited and the next field is its last
exit code. `mac-install.sh` requires an explicit target SSH alias, installs source
under `~/.local/src/hgs` and builds on that host. `--no-tray` updates only the CLI.
No reverse connection is created by default. Explicit peer setup uses
`HGS_MAC_PEER`, `HGS_MAC_PEER_HOST` and optional `HGS_MAC_PEER_USER`,
`HGS_MAC_PEER_KEY`, `HGS_MAC_PEER_COLOR` through local configuration.

| Desktop option | Purpose / default |
|---|---|
| `--hgs PATH` | CLI executable; otherwise `hgs=` in the tray config, PATH, then `~/.local/bin`, `/opt/homebrew/bin` and `/usr/local/bin`; on Windows `hgs.exe` next to `hgs-tray.exe`, then PATH, and the stable CLI once local sessions are enabled ([Windows](#windows)) |
| `--local-poll MS` | Local tray polling, 2000 ms |
| `--peer-poll MS` | Peer tray polling, 60000 ms |
| `--terminal TEMPLATE` | External terminal launcher with `{cmd}` |
| `--icon-color COLOR` | Override tray mark/counter foreground |
| `--open-mode MODE` | Linux auto/tab/window/clipboard; macOS terminal/clipboard; Windows auto/tab/window/console/clipboard |
| `--selftest` | Headless hgs connectivity check |
| `--sessions` | Open the workspace; on Windows also the default |
| `--desktop-target JSON` | Zerus Next on Linux and Windows: open the exact `computer`, `run` and `conversation`; implies `--sessions`. Navigation data only, with no notification or writer authority |
| `--background` | Start in the tray only; the default on Linux and macOS. Not together with `--sessions` |

A single-instance socket forwards repeated launches to the running GUI: one that
would open the workspace opens it there.
The target value is bounded to 8192 bytes and accepts only those three
fields. It follows the exact run and conversation through a session rename and
refuses an archived or replaced session. The service's selected-child launcher
requires the separate environment-input capability and passes the same target
as `ZERUS_CLIENT_DESKTOP_TARGET` in that child only, keeping the identifiers out
of its command line. On Linux, an ephemeral Wayland activation token also travels
only in that child's environment. The new GUI consumes both values before starting its
workers; forwarding navigation to an already running GUI carries no token.
On Windows the per-user activation pipe forwards the target as a `target` line
and the forwarding start exits successfully.
Conflicting argument/environment targets and target/background starts refuse.
On Linux and macOS, concurrent launches take turns on it, and a launch such as
`hgs-tray --sessions` leaves it to the `hgs-tray` systemd or launchd service
while that service runs or restarts: it waits up to 30 seconds for the
service's GUI and opens the workspace there. It starts its own GUI only when
the service stops or does not answer in time. A GUI that a launch started while
the service was stopped keeps the socket, and a later service start then exits
without a GUI of its own.
The desktop version in the rail opens **About**; its tooltip shows the complete
installed version, including the Dev build number or local source identity.
**Settings → About** also
shows the configured `hgs` CLI version, read when About opens; an unavailable or
unrecognized response remains explicit instead of using a compiled-in value.
Closing a workspace never owns agent process lifetime. Local machine settings,
credentials, runtime journals and personal connection values belong outside the
repository.

<a id="windows"></a>

## Windows

On Windows, `hgs.exe` and `hgs-tray.exe` manage remote Linux and macOS machines as
a control console. Once local sessions are enabled on the computer, `hgs.exe` also
runs agent sessions there, in a session host of the user on ConPTY
([Windows Phase 2](superpowers/specs/2026-10-13-windows-native-host-design.md)).
See [installation and limitations](installation.md#windows) and
[testing the session host](windows-host-testing.md).

**Local sessions.** `hgs session-host install | status | stop | uninstall [--json]`
exists only on Windows and manages only the computer it runs on; for another
machine, `hgs @host session-host` exits with code 2. `install` enables local
sessions: it stages the stable CLI `%USERPROFILE%\.local\bin\hgs.exe` and a host
image below `%USERPROFILE%\.local\share\hgs\host`, then writes the `local-sessions`
marker in the configuration directory. `status` reports the marker, the staged
files and the running host without starting one; `stop` stops an idle host;
`uninstall` refuses while sessions run, then removes the marker and leaves the
staged files. The marker alone decides, so a stopped or crashed host leaves the
computer enabled with its saved sessions. The host starts with a launch, resume or
fork, never with a listing, and exits a few seconds after its last session. Every
session runs the stable CLI's `hgs.exe --run`, which starts the agent; tracked
Claude Code, Codex and Kimi sessions get hooks that name the stable CLI. `hgs a`
needs a console such as Windows Terminal, not the pipes of the mintty Git Bash
window, and detaches with `Ctrl+B d`.

**Windows as a remote computer.** A Linux, macOS or Windows client can use a
Windows OpenSSH alias without changing its server-wide `DefaultShell`. Enable
local sessions on that Windows account with `hgs session-host install`, then add
the alias to Zerus's computers. PowerShell 5.1 and cmd are supported as SSH login
shells. Discovery is read-only and cached per alias/connection settings within
the calling CLI process; an unknown result refuses before sending an operation.
The Windows route selects `%USERPROFILE%\.local\bin\hgs.exe`, uses fixed encoded
PowerShell source and Windows-native argument quoting, and relays SSH stdin,
stdout and stderr as raw bytes. Noninteractive input has bounded byte frames,
an explicit EOF, and a two-second lease heartbeat. EOF closes the original
helper's stdin. Connection loss or a fifteen-second lease expiry ends only that
same original helper; native sessions remain with their session host. No remote
operation is retried. JSON requests and watch controls stay on stdin. Required help
capabilities, machine identities and session selectors still gate their commands;
an old peer is never tried as an unknown agent command. The cmd transport limit
is checked before dispatch; large request bodies belong on stdin. POSIX peers
keep their existing `~/.local/bin/hgs` commands and quoting. `machine check`
uses the Windows local-session marker instead of requiring tmux; POSIX source
bootstrap is refused for Windows. Remote downloads also accept absolute drive
paths such as `C:\work\report.txt` through SFTP.

**Commands.** Commands for another machine (`hgs @host …`) work as on Linux. Until local sessions
are enabled, `ls`, `machine`, `project`, `swarm`, `presence` and `session-host`
work locally. `account ls` and `account default` without an ID print an empty
catalog, and `account copy` works between two remote machines. Every other local
command, such as `a`, `send`, `inspect` of a local session or a launch without
`@host`, exits with code 2:

```text
hgs: Local sessions are not enabled on this computer; enable them on the Machines page or run hgs session-host install, or choose a remote machine.
```

With local sessions enabled, local session commands run this computer's sessions
in the session host. `paste`, `clip` and `notify` still exit with code 2
(`hgs: clip is not supported on Windows`), DeepSeek sessions and voice messages are
refused with their own messages, and OpenCode is not supported there. The DeepSeek
Native UI commands (`native-ui`, `account login native-dsh`, `a dsh/…`) and the
mobile-peer commands exit with code 2 in both cases, each with its own message.

**Listing.** Until local sessions are enabled, the local element of `hgs ls --json`
and `hgs ls --json --local` is a control-only box with `"local_sessions": false`,
`projects: {}` and `sessions: []`; plain `hgs ls` prints
`(control console: sessions run on remote machines)` for this computer, and
`--dry-run ls` prints nothing for it. With local sessions enabled, it lists this
computer's projects and sessions as on Linux. Either way it carries `local_host`,
the session host as found ([JSON](#json-and-external-clients)), and its metrics
include CPU, memory and uptime; load averages are null. The computer's name is
`HGS_SELF` when set, otherwise its lowercase `COMPUTERNAME`. Configuration and
state live under `HOME` when set, otherwise `%USERPROFILE%`, in the usual
`.config\hgs` and `.local\state\hgs` folders.

**OpenSSH.** Windows OpenSSH never exits after it has used a synchronous pipe for
one of its standard streams, the kind that Rust's standard library, Python and
PowerShell create. It never sends the end of a non-empty file or of `NUL` given
as input, so a remote command reading its input waits forever, and it writes an
output file from its start, over what `>>` keeps. Every stream that carries data
between `hgs.exe` and `ssh.exe` is therefore a console or an overlapped pipe:
`hgs.exe` relays its own input and output unless they are a console, so `< NUL`
ends the remote input and `>> log` appends. Scripts must not run `ssh.exe` with
such streams directly: call `hgs.exe @host …`. `hgs.exe` runs in a kill-on-close
job object, so its `ssh.exe` and `tar.exe` children end when it is killed. Windows
OpenSSH has no connection sharing, so every poll is a full SSH handshake.

**Interactive links.** `hgs @host a SESSION` runs `ssh.exe -t` on the inherited
console. Ctrl+C and Ctrl+Break reach the remote program; `hgs.exe` ignores them while
it waits. Windows OpenSSH runs the `LocalCommand` that marks an established link
through `cmd.exe`, as `type nul > "%%HGS_LINK_MARKER%%"`: the marker path travels in
that environment variable, so a `%` or another special character in the temporary
folder is never expanded. Each link uses a `hgs-link-<pid>-<random>` folder in
`%TEMP%`. A closing console window removes its folder. A client the desktop app
kills leaves its folder behind until the next interactive link, which removes the
folders of ended processes.

**Embedded terminal.** The built-in Terminal runs `hgs.exe @host a SESSION
--existing`, or `hgs.exe a SESSION --existing` for a session on this computer, in a
Windows pseudoconsole (ConPTY). Disconnect closes the pseudoconsole, waits up to a
second and then ends `hgs.exe` and `ssh.exe` through a job object; the session
keeps running on the remote machine or in the session host.

**Machine setup.** `hgs machine setup` (**Install / update HGS…**) needs a source
checkout on Windows. It packs the checkout with the system `tar.exe` and refuses one
whose `install.sh` or `hgs_state.py` has CRLF line endings; `.gitattributes` keeps
`*.sh` and `*.py` in LF. Identity files in `machines.local.json` may be Windows
paths or `~\…`.

On every platform, setup snapshots the CLI sources and the transitive local Cargo
path dependencies, including the root crates and `client/core`, before SSH starts.
The private snapshot includes package manifests, `src`, declared build/library/bin
inputs, the root lockfile, CLI installer/helper and installer scripts. The two
shared `voice_video.rs` and `voice_rejection.rs` validators under the relay source
are included when present; no relay or connector package is bundled. It excludes
build outputs, VCS directories, hidden entries, Node modules and Python caches.
It refuses symbolic or nonregular source entries, external path dependencies and
workspace inheritance or patch/replace overrides. The bounds are 32 packages,
20,000 visited/copied entries, 64 MiB total, 8 MiB per file (1 MiB per manifest),
32 directory levels and a five-second scan budget. A refusal makes no SSH call.
The remote CLI-only install preserves configuration and existing checkouts, skips
the mobile connector and does not install or restart services or agent processes.
The optional `--revision` binds setup to the saved profile snapshot, checked both
before source preparation and before SSH. Existing setup argv remains supported.
Next confirms the selected machine and source folder, keeps the page busy and
shows bounded setup output when the command ends. Keep Zerus running until it
finishes. A timeout or uncertain result is shown for review and never retried
automatically. Enabling local Windows sessions has its own confirmation and
rechecks the original local machine identity and current host status.

**Swarm worker.** In background mode, the durable Windows desktop service runs
one hidden `hgs.exe swarm worker`, including while the GUI is closed or consumes
the service. It restarts its own child after an exit, with a delay from one second
up to five minutes, reset after a run of at least a minute. The child starts
suspended in its own job before running. Stop and restart retain the original
process/job and service owner until physical exit and job drain are confirmed;
unknown cleanup is reported in `swarm_runtime` and prevents a replacement.

`hgs desktop swarm-restart --json` asks an already-running background service to
restart only its own worker. Passive, stopping and offline services refuse it;
it never starts the service or GUI, or stops a PID from `worker.json`. Stale-worker
checks use this same bounded request, with the existing ten-minute attempt limit
and default-configuration restriction. `hgs swarm get` still reports
`worker: {running, current}`. The standalone GUI retains its compatibility
supervisor outside desktop-service mode; `hgs swarm worker` and `hgs swarm sync`
remain available for manual use.

The explicit restart command has one two-second deadline from process entry,
including Windows PATH preparation, configuration reads and the pipe exchange.
An expected-machine guard reads only the existing catalog identity; it never
initializes or refreshes the store, takes its write lock, or saves metadata.
On timeout it prints one JSON result with `outcome: "unknown"` and
`retry_allowed: false`, then exits with code 3. Inspect service status before
another attempt; the timed-out request is never replayed automatically.

**Desktop app.** It uses the `hgs.exe` next to `hgs-tray.exe`, then the one on
`PATH`; `--hgs` or `hgs=` in `tray.conf` overrides both, and the Linux and macOS
fallback to `~/.local/bin` and Homebrew does not apply. Without `--hgs` or `hgs=`,
each start first asks that `hgs.exe` for `session-host status`, before the tray
icon appears; once local sessions are enabled, the app then runs the stable CLI
instead, after `session-host install` when that `hgs.exe` differs from the staged
one and is at least as new. A small *Starting Zerus…* note shows while these
checks take longer than half a second. Settings are in
`%APPDATA%\hgdev\hgs-tray.ini`, with drafts beside it, and one instance runs per
user. When the local box reports `local_sessions: false`, the app offers remote
machines only: in New session, Overview, the tray menu, Accounts, Projects and
search. The Machines page lists this computer as a control console with
**Enable local sessions** (`hgs.exe session-host install`, after a
confirmation). In both modes, this computer's page has no SSH settings or
actions; it shows the session host's state, PID, ConPTY, integrity level and
Windows session. Without remote machines, New session, Accounts and automatic
recovery send you to Machines: to add a remote machine and, where
Machines offers **Enable local sessions**, to enable them. New session then has
**Open Machines**, which only selects this computer there. Automatic recovery
settings of a control console are saved to the first reachable remote machine, and
it is listed at most once every 15 seconds. With local sessions enabled, New
session and Accounts offer this computer without DeepSeek and OpenCode, the shell
entry is **Run PowerShell**, voice messages to its sessions are disabled with the
reason as the tooltip, and it is listed at most once every 5 seconds. In both modes,
Settings → Mobile connection configures the bundled outbound Rust connector on
Windows, including before local sessions are enabled. Start and Stop are explicit;
closing the GUI preserves the connector, session host and native agents. The
first Windows transport controls existing local Codex/Claude/Kimi sessions and
refuses mobile creation, path/worktree/Git/file operations, peers and voice/speech.
Windows desktop presence is not recorded, so desktop activity does not delay
phone alerts. See [Windows gateway setup](mobile-deployment.md#windows-gateway).
Attention notices are tray notifications with the Zerus icon and a taskbar
badge. With the desktop service, the service posts its own toasts and the app
posts none; a toast click opens its exact run and conversation in the running
window and brings it to the front, or flashes its taskbar button when Windows
keeps another program in front. A start without options opens the workspace, because Windows 11 first
hides a new notification
icon in the overflow; `--background` starts in the notification area only, as a
Startup-folder shortcut should. Starting `hgs-tray.exe` again opens the running
app's workspace, unless with `--background`. After a main-thread watchdog abort,
Windows Error Reporting restarts the app with the same settings in the background
(`--background`, never `--sessions`): a window that opens by itself after a crash
would be a surprise.
When the desktop service receives a toast click and no Zerus window runs, it
starts the installed `hgs-tray.exe` (the per-user installation or the copy next
to the service's `hgs.exe`) for the exact session, after checking that its
`--help` lists the desktop target environment marker. It starts the window
outside the service's job when Windows allows that, so a service restart does
not close it, and passes it the right to come to the front.

**Updates.** The portable zip has no installer or self-update. Stable releases
after 0.42.2 and nightly releases carry the same tested zip. A portable
installation starts on the stable channel; choose Nightly to hear about nightly
builds too. Earlier portable builds saved Nightly as their default, and that saved
choice is kept. A zip that winget installed as `ufna.Zerus` offers its
`winget upgrade` command on the stable channel instead of the download link;
winget carries stable releases only. Settings → Updates reports
a stable release or nightly with a newer product version than the running app and
links to its release page; nightlies of the same product version cannot be told
apart. Stable 0.42.2 and earlier have no Windows build and are never reported as
an update, even when newer: Updates says so and points to the nightly channel.
Dev builds have no Windows build either: on that channel Updates says so, never
reports an update and offers no dev download. The zip is published on GitHub
only: the zerus.dev mirror neither stores it nor lists it in the v1 desktop feed,
and the v2 Dev catalogs have no Windows artifact.

## JSON and external clients

### Retained session metadata API

The desktop Graph destination has been [retired](retired-features/session-graph.md).
Its last implementation is preserved by a named Git tag for a separate future
iteration. The desktop no longer collects or renders Graph data.

`hgs [@host] graph snapshot --json`, `graph changes --json` and `graph receipt
--json` remain bounded native compatibility APIs for released clients. The
host-owned metadata index covers exact saved conversation/run identities,
archives, native children, confirmed forks and verified submission receipts.
It does not change source bindings, send input or change read marks.

Each call reads at most 32 new or changed metadata records and checks at most
512 cached stamps, with a 2 MiB ordinary input budget and a
50 ms cooperative work budget. One manifest may finish within a 16 MiB hard cap;
directory inventory is capped at 10,000 source files. These are bounds, not a
measured latency guarantee. Partial or capped inventory never removes unseen
sources. A malformed source retains last-good facts and reports a coverage gap.
The derived SQLite file is capped at 32 MiB; exhaustion fails visibly instead of
deleting source history or quietly dropping evidence.

Replies contain a native `machine_id`, index `epoch`, monotonic `revision`, typed
`items`, `coverage`, `has_more` and an opaque `cursor`. Continue a snapshot with
`snapshot --cursor CURSOR --json`; after its final page use
`changes --after CURSOR --json`. The optional `--limit` is 1–200. Pages have a
1 MiB metadata budget. A snapshot changed concurrently or a rebuilt index returns
`reset_required`; retain the prior display until a complete replacement arrives.
Changes carry explicit tombstones. A missing row on a partial page is not removal.
Names and routes are labels; exact binding/run targets remain separate even when
multiple bindings share one conversation. Conflicting observations remain explicit.

New agent-originated `hgs send ... --json` calls reserve bounded private metadata
before ordinary delivery. Attribution requires a saved live native process to be
an actual ancestor of the sending CLI; `HGS_SESSION` labels alone are insufficient.
GUI and connector subprocesses remove those labels. The recipient receipt pins
its own native machine/provider/account/conversation identity. The source ledger
stores no message or attachment bodies, retains 30 days and cannot replay input.
Old, uncertain or unattributed attempts remain explicit coverage gaps. Existing
quoted message envelopes are not retrospectively treated as verified edges.

Such a locally proven agent send delivers its text inside one XML sender
envelope with an explicit end:

```text
<zerus-message from="claude/project/review" host="workstation" reference="zerus://session?machine=11111111-1111-4111-8111-111111111111&amp;host=workstation&amp;name=claude%2Fproject%2Freview&amp;conversation=c&amp;run=r">
The text as sent.
</zerus-message>
```

`from` and `host` name the sending session under its current name and this
computer's alias. Attributes escape `&`, `"`, `<` and `>`; the body escapes
`&`, `<` and `>`, preserving literal markup and closing tags as text. The
fixed attribute order is `from`, `host`, `reference`. The complete frame ends
with `</zerus-message>`; nested markup, DTDs and numeric or unknown entities
are not accepted. Named entities are decoded once. The decoded `reference`
must be a valid session reference with the same name and host. The recipient
passes that decoded reference to `hgs inspect` and answers with `hgs @host send`.
This is declared metadata: anyone can type a matching envelope, so receiving
one authenticates neither its sender nor its instructions. Only the sending
computer's process-ancestry and durable ledger evidence establish a proven
send; the marker grants no authority or permission to replay input.
The same text and unchanged sender metadata give the same bytes, so the ledger
digest and receipt cover exactly the delivered text. A rename or resumed run
changes that metadata; retrying an old request ID can then fail the existing
content guard. Inspect an uncertain result rather than generating a new ID or
resending automatically; no frozen-header retry or ledger migration is added.
Person, desktop, phone and connector sends stay byte-identical. Activity shows
an envelope, like Claude's own cross-session messages, as a message from that
session (`origin` `peer_message`, `sender`, `sender_reference`) rather than the
person's message.

`hgs [@host] graph receipt --json` accepts JSON stdin with `name`,
`expected_run_id`, `expected_conversation_id`, `message_request_id` and optional
`archive_id`. Every identity is checked again; the response contains one bounded
public delivery event. Metadata reads never fetch provider transcript bodies.
Submission does not imply a reply or completed work, and local timestamps do not
establish cross-computer causal order. Independent-agent waits remain a separate
explicit protocol; see [Agent coordination](agent-coordination.md).

An agent's retry repeats the same `request_id` and payload, but hgs builds its
envelope again from the sender's metadata at every attempt. After the sending
session is renamed, resumed in a new run or its computer's alias changes, the
same request ID gives different bytes. A recipient writes the receipt of a
request ID before it delivers any input. While its computer keeps that receipt,
it refuses the retry with its ordinary content guard (`request ID was already
used for different content`, or `This request ID was already used for another
message` for DeepSeek and OpenCode) and delivers nothing. Without that receipt,
for example when an earlier attempt stopped before the recipient recorded it or
the receipt is gone, the retry is a new delivery under the ordinary guards,
even if an earlier attempt arrived. The source ledger retains attempts for 30
days. If the source ledger still has a conflicting entry for the same native
sender and request ID, hgs warns that attribution could not be recorded and
still forwards the send under the ordinary guards. A refusal says nothing about
whether an earlier attempt arrived. hgs never retries a send itself, and
sending the text again under a new request ID to get past a refusal can deliver
it twice. To recover, read the first attempt's receipt on the recipient's
computer with `hgs [@host] graph receipt --json`: the recipient's `name`,
its current `expected_run_id`, the conversation the message went to as
`expected_conversation_id` and the original request ID as `message_request_id`.
A returned `event` confirms only that the attempt's receipt has the status
`submitted` for that exact conversation, not that the model processed it or
replied; its `detail` is the delivered text, envelope included. A receipt whose
status is not `submitted` yields no event, so an unavailable receipt proves
neither delivery nor its absence: inspect the recipient's conversation and
leave any new message to a deliberate decision. Neither the receipt nor the
envelope authenticates who wrote the text.

### Existing session snapshots

```sh
hgs ls --json --local
hgs inspect codex/sample-project/review
hgs inspect codex/sample-project/review --after 123
hgs @mac inspect claude/sample-project/review
hgs @mac dirs '~'
```

`ls --json --local` is one host snapshot without SSH; an empty tmux server returns
an empty array. A minimal shape is:

```json
{
  "host": "workstation",
  "ok": true,
  "projects": {"sample-project": "/workspace/sample-project"},
  "sessions": [{
    "name": "claude/sample-project/review",
    "cmd": "claude",
    "project": "sample-project",
    "tag": "review",
    "attached": 1,
    "clients": ["/dev/pts/2"],
    "created": 1787840176
  }]
}
```

A computer that runs no sessions of its own (`hgs.exe` on Windows until local
sessions are enabled) adds `"local_sessions": false` and returns empty `projects`
and `sessions`. The key appears only when false; a missing key means true, so Linux
and macOS snapshots are unchanged. [Windows](#windows).

On Windows the local box also carries `local_host`, the session host as found
(never started): `state` is `not-installed`, `stopped`, `running` or `unavailable`
(with `error`), and `pid`, `protocol`, `conpty` (`bundled` or `inbox`), `integrity`
and `windows_session` describe a running host and are null otherwise. Its sessions
list attached clients in `clients` by opaque IDs such as `c1` instead of tty paths,
and a live session's `cwd` (`cwd_source: tmux`) is the folder it started in: the
host does not follow a later change of directory, such as `cd` in a shell.
Linux and macOS snapshots have no `local_host`. When `windows_session` differs from
the desktop app's own Windows session (a host started over SSH, for example), the app
asks before a launch, resume, restore or fork on this computer goes into that host,
whose agents cannot show windows such as a browser sign-in on the desktop; Cancel
keeps drafts and dialogs as they were.

Tracked live sessions add resumability and activity. Saved sessions use
`state: paused|stopped`, zero attached clients and an empty clients array.
Archives add `archive_id` and `archived_at`; missing state remains compatible with
older live-session snapshots. `ok: false` indicates an unavailable host.
A peer's entry in `hgs ls --json` and the answer of `hgs @alias ls --json` forward
that machine's own listing, so their `host` is the name the machine gives itself
(its first `HGS_SELF` name, otherwise its host name), which may differ from the
alias; an unreachable peer's entry names the alias. Key a peer by the alias you
asked for, as Zerus does; the local entry's `peers` lists those aliases.

A launch with `--launch-id` that no session or record of the snapshot carries, and
whose session existed before the snapshot was taken, has ended: such launches
appear newest first, at most 32, in `ended_launches`, as `{launch_id, name, code,
output}`. `code` is the exit code of the session's supervisor, or null when no
supervisor reported one (a command that runs without one, a supervisor that was
killed); `output` holds the last 8 lines (2 KiB) of the session's screen, followed
by the error that stopped the supervisor (on Windows, as the screen shows both
once the session's output has drained). A detached launch records its launch ID
once the terminal has created the session, and a supervisor that ends within two
minutes without a confirmed conversation records how; a run that confirmed one
leaves its record to carry the launch ID instead. These records live in
`launches/` of the hgs state directory for a week, as long as the desktop keeps a
launch placeholder. The key is omitted when no launch has ended.
When there is no supervisor completion report, `diagnostic_code` is
`launch_supervisor_report_missing`; the original started record also retains
that diagnostic. This identifies missing evidence, often a failure before
`hgs --run` during shell startup. It does not invent an agent exit code or
terminal output: those fields remain null and empty when unknown. The desktop
shows the missing-report explanation separately from captured output.

A running Zerus desktop records input idle time and screen lock with
`hgs presence set --json` (`{"idle_seconds": 12, "locked": false}` on stdin) every
30 seconds and when the screen locks, unlocks or the owner returns. The listing then
adds `desktop_presence: {idle_seconds, locked, locked_seconds}`, aged to the moment
of listing. It is omitted when the record is more than two minutes old (Zerus
closed, crashed or the machine slept) or written by a clock that has since moved
back. The record is `desktop-presence.json` in the hgs state directory and holds
no other desktop information. The listing also includes
`preferences: {mobile_delivery}` from the shared catalog, or the default when the
catalog is missing or unreadable.

Every session also has `attention_signature` and `read`, either of which may be
`null`. `attention_signature` is a lowercase hex SHA-256 of what currently needs
the owner: run, conversation, the phase when it is `approval`, `input` or
`error`, the attention identity, pending question identities (the IDs and hashes
the mobile connector publishes as `mobile_attention`) and actionable subagent
requests. An approval or input request also contributes `attention_started`, the
time it began, so requests without a native ID (a permission prompt without a
tool call, a tool known only by name) stay distinct; the end of a compaction and
a prompt repeated with the same native tool IDs, tool and input keep that time.
Each external child approval carries its own `attention_id`. It is `null` when nothing needs attention and
does not change on status-only updates. A listing indexes Codex question
transcripts within a shared one-second budget (and a short slice per session),
saving its progress so the next listing resumes; it never scans a long
transcript unbounded. While that index is incomplete, or held by another
process, the signature includes an explicit "questions unknown" component: it is
non-null, never equals an acknowledged signature, and `hgs read` does not
acknowledge it, so an unknown state never hides a question. `read` is the
host's mark for the current conversation:
`{conversation_id, reply_id, attention_signature, at}`. A reply is unread while
`reply_id` differs from `read.reply_id`; attention is unacknowledged while a
non-null signature differs from `read.attention_signature`.

<a id="optional-questions-in-listings"></a>
Every session also has `optional_questions`, the questions Codex asked with
`request_user_input_async` that still wait for the person while the agent
continues without them: `{count, ids, latest_at}`. `count` is exact; `ids`
holds the `question_id` of the newest 32, oldest first, and `latest_at` is when
the newest was asked (seconds since the epoch, or `null` when not recorded).
Without any it is `{"count": 0}`, as for saved, archived and non-Codex sessions.
A question stops counting once its answer appears in the conversation, it is
skipped, closed by native turn completion/interruption or ordinary follow-up,
or its answer was submitted (**Submitted / Awaiting agent**). Only the main
conversation is indexed: async questions of Codex native subagents are not
counted. The value is `null` while the question index is incomplete or held by
another process (as above) or cannot be read: unknown, never a count of 0, so a
reader keeps the last value it had for the conversation. A listing from an
older hgs has no `optional_questions`; peers forward theirs unchanged. Only
identities leave the host, never question text, and no value depends on the
time of the listing. Optional questions are not attention: they never change
`phase`, and the count does not affect `attention_signature`, which keeps
covering their identities as described above. Pending blocking requests of other
providers stay in `phase` and `pending_question_count` is not used for these
questions.

Kimi sessions report `schedules: {count, recurring, next_at}` folded from the
native cron journal: pending jobs the agent scheduled for itself, how many of them
recur, and the earliest next run in seconds since the epoch (`null` when no
expression can be evaluated). The next run is the first local-time minute matching
a job's five-field expression after its last fire or creation; Kimi's small jitter
is not modeled, and the value never depends on the time of the listing. The object
is `null` while the journal cannot be read. While `schedules.recurring` is
positive, turns opened by those jobs without a prompt from the person do not
advance `reply_id`; a prompt queued into such a turn makes its reply the person's.
`reply_scheduled` is true when the reported reply ended a scheduled turn, which
happens once no recurring job remains or while the count is unknown. DeepSeek
sessions report the same `schedules` object from the active tasks of the native
Schedule catalog while that optional bundle is loaded, with the earliest stored
target as `next_at`; delivered reminders remain ordinary replies. Other
providers report false and omit `schedules`.

```sh
printf '%s' '{"run_id":"…","conversation_id":"…","reply_id":"12:1787840176.5","attention_signature":null}' |
  hgs @mac read codex/sample-project/review --json
```

`hgs [@host] read` applies each non-null field only while it equals the session's
current value in the same run and conversation; other fields are reported as not
applied and the mark does not change. The current value is what a listing would
show at that moment: a Claude approval already answered in Terminal is settled
from the screen first, so its old signature is no longer acknowledged. It prints
`{"ok": true, "applied": {"reply_id": …, "attention_signature": …}, "read": …}`;
an unknown session prints `{"ok": false, "error": "not_found"}` and exits with
status 1. Marks live in `read-marks.json` in the hgs state directory, at most
eight conversations per session, and follow `hgs rename`.
Repeating an already-stored current mark reports it as applied without changing
its timestamp or rewriting the file. Existing-owner writes retain other owners;
admitting a new owner prunes abandoned entries using the current binding set.
Unreadable or malformed existing storage is refused rather than reset. The
global v1 JSON still has population-dependent parse and changed-write costs.
OpenCode renames publish the new binding and its unchanged v1 read marks before
retiring the old binding, in shared-read writer order. If a confirmed native title
change is followed by a local persistence failure, the scoped action remains
uncertain; inspect the retained bindings before choosing another action. An
uncertain receipt never authorizes another native rename automatically.

For a current journal-backed main session, `hgs inspect NAME --read-observation` or
`hgs [@configured-alias] inspect '<reference>'` also returns the host's **raw**
`read` object (or `null`) and `read_observation`. Direct-name inspection without
`--read-observation`, including Activity/Processes polling with `--after` or
`--skip-processes`, does not invoke this reader or add either field. The
reference route opts in automatically. This does not enrich the mark
with the current reply or acknowledge anything. Check `read_observation.complete`
and `freshness: "observed"`, then match both its `target` and the inspection's
current name/run/conversation to the intended target. A portable reference may
resolve a resumed conversation with a different run; that is not acknowledgement
of the original run.

The observation records `observed_at` (host Unix seconds), `source_state` and a
`sha256:` content `generation` of the observed `read-marks.json`; a missing file
has `source_state: "absent"`, null generation and null read. Marks are stored by
stable journal owner and conversation, not by run, so the target's current run
binding is reported separately rather than invented in the stored mark. Each
private regular file read is capped at 16 MiB, with a one-second cooperative
budget shared by the observation and exact selected binding checks. Only the
selected owner/conversation is materialized; unrelated marks are skipped during
parsing. The original file and its path are checked for replacement/change.
A busy, malformed, oversized, leaf-linked, nonprivate, changed or mismatched source
returns `complete: false`, `freshness: "unknown"`, a short `code` and null read;
this is not evidence of an unread or acknowledged part. An observation is a
point-in-time read, not a promise that the source stays unchanged afterward.
These fields are not supplied for archives, subagents or other provider-specific
inspection paths. No shared-read file, lock, delivery or background worker is
created or changed by this observer.

`hgs inspect NAME --current-read` adds `current_read` for a current tracked main
session with a conversation: `schema` 1, `run_id`, `conversation_id`, `reply_id`
(or null) and `attention_signature` (or null), the values `hgs read` would accept
now, plus `attention_complete`. These come from the same inspection a device
displays. The reply is captured before provider messages are read, so its final
message is in that read or an earlier one. The questions it signs are read
before the inspection lists its own, so every signed question that is still
pending was listed. When questions cannot be indexed within the shared one-second
question budget, `attention_complete` is false and the signature matches nothing
`hgs read` accepts. A recurring job keeps its already read reply current, as
`hgs read` does. Its only extra work is that bounded question check, plus the marks
file read for a recurring job. It acknowledges nothing and is absent without the
flag and for archives, subagents, DeepSeek and OpenCode. A
device acknowledges these values only after it has shown them; `hgs read` still
applies each one only while it is current.

`inspect` includes conversation, phase/activity, prompt/last message, tools,
subagents, events and cursor. Initial reads return recent events; `--after` advances
the cursor. Reset it when conversation ID changes. `history_truncated` discloses
journal limits; `tracked: false` identifies an unbound session. Folder/Git fields
include `cwd`, `cwd_source`, `cwd_canonical`, `git_root`, `git_common_dir`, branch,
worktree, detached state and metadata status. Git metadata has a bounded read and
15-second cache, independent of pause eligibility.

`hgs [@host] git-status --path /absolute/checkout --json` provides independent
read-only publication evidence for the GUI. It supports worktrees and subfolders,
caps Git output at 1 MiB and the whole check at three seconds, and never prompts
for credentials. Ahead/behind counts compare HEAD with its configured local
tracking ref; `remote_state: verified` requires that ref's OID to match an exact
read-only `ls-remote` result. `changed` and `missing` do not imply synchronization.
`no_upstream` requires a completed lookup that finds no configured upstream, and
a local tracking ref counts as absent (`changed` while the remote branch exists)
only when its lookup completes without finding it. A failed or timed-out upstream
or tracking ref lookup leaves `remote_state: unknown`.

Subagent count fields distinguish active, completed, total, completeness and
source; unknown totals are null. Up to six active/unreviewed previews accompany
the available full roster. Profile groups without individual IDs remain groups.

`hgs [@host] send SESSION --json` accepts stdin containing a UUID `request_id`,
`text`, `expected_run_id`, `expected_conversation_id` and optional attachments
`{name, mime, data_base64, reference?}`. Unique `[Image #1]` / `[File #2]` references
place attachments at their first marker; otherwise they append. IDs come from a
fresh inspection. `status: submitted` confirms native input delivery, not a model
reply. Saved receipts make identical requests idempotent. An uncertain result
needs manual inspection before another request. `agent_id` is supported only for
verified DeepSeek continuable children. Questions carry request identity/hash
through `pending_questions`; answers use the exact unchanged request.

Messages may begin with an absolute file path, including a directory named like
a command (`/help/capture.png`). A first token of `/name` without another slash,
or leading `!` shell mode, still requires Terminal. An argument containing a path
does not turn `/command` into an ordinary message.

### Desktop background service

Zerus Next defaults to the Service consumer on Linux and macOS when
`ZERUS_CLIENT_DESKTOP_SERVICE` is absent and the maintained helper's read-only
status confirms an owned registration. Otherwise startup stays guarded Native;
app/cask or AUR installation alone does not register a service. Run
`zerus-setup --desktop-service` on Linux, or plain `zerus-setup` on macOS,
first. Explicit `=1` selects Service on all supported
desktop platforms; Windows keeps that opt-in until its PC acceptance. Explicit
`=0` requests the guarded Native/rollback path, which still needs a verified
takeover and drain. It never forces a writer or starts the Qt rollback directly.
An already-background installation is unchanged. Explicit passive choices and
retained revokes require **Resume background work**; automatic read-only reconnect
never grants, rearms or changes mode. Automatic approvals stay off. See
[consumer activation and failure actions](desktop-consumer-activation.md).
The supported persistent desktop rollback is the maintained Qt updater route;
`=0` and transient manager environment settings only affect new process startup.

```sh
hgs desktop serve
hgs desktop status --json
hgs desktop watch --json
```

`hgs desktop serve --background` explicitly starts the service-owned bounded
fleet runtime. The actor applies its watch packets and publishes catalog/count
updates through the same endpoint. It keeps each original event's ingress permit
through the database transaction; UI-supplied feed packets cannot enter the owned
observation path. Failed frames invalidate their exact source until resnapshot.
Each connection row of its status (`background_runtime.connections`) carries the
source `incarnation` and its supervisor's `retry` (`idle`, `waiting` with
`delay_ms` and the `remaining_ms` measured from the supervisor's own deadline,
or `attempting`), plus the `retry_delays` in use; a change also publishes an
early `fleet_connections` frame (at most every 250 ms) between heartbeats.
Display only: no source generation or writer authority. The `desktop request`
operations `fleet_reconnect` (`computers`: 1 to 64 `{computer, incarnation?}`,
answering `requested`, `pending`, `connected`, `not_configured`,
`not_admitted`, `source_changed` or `stopping` for each) and
`fleet_retry_delays_reload` end an existing supervisor's wait or re-read the
saved reconnect timings; neither starts, restarts or activates anything, and a
passive service answers `fleet_unavailable`.
Plain `serve` and fresh registrations stay passive. On Linux and macOS, the maintained
installer supports explicit `--mode background|passive` after the authenticated
frontend handoff; see [Linux mode selection](linux-distribution.md#select-the-desktop-service-mode)
and [Mac mode selection](macos-distribution.md#explicit-desktop-service-mode).
The Windows native route uses explicit `registration install --mode
background|passive --json`; see [Windows mode selection](desktop-windows-mode.md)
for caller admission, passive bootstrap, retained task-instance custody and
unknown-result handling.
Ordinary refreshes preserve the selected mode. Generic approval policy requests
remain refused; ownership and each effect require their own authenticated grants.

Operation-ledger rows and bytes reserve outstanding original work, not every
historical receipt. The first durable physical drain returns that original's
reservation exactly once, including an Unknown result. Its outcome, UUID,
source, custody records and lookup history remain retained; returned capacity
never grants replay. A two-child operation keeps its full reservation between
children. Task-message staging inputs retain their separate charge until their
existing exact removal/review rules release it; creation scheduling slots keep
their separate predecessor protections.

Owned startup may convert earlier cumulative accounting only after complete,
bounded validation of its exact original charge formula and active roster.
Corrupt, mismatched or incompletely covered accounting refuses admission; it is
never reset or approximated. Status covers at most 4096 historical metadata rows
plus one overflow sentinel. A larger history reports `history_complete:false`
and null historical `rows`/`unsettled`, with null propagated to the aggregate.
The bounded outstanding roster and physical custody remain checked separately.

Background mode also owns one presence sampler/writer outside the actor. It
samples every two seconds and uses the existing `desktop-presence.json` format.
It writes on a 30-second heartbeat, a lock change or a return from at least
120 seconds idle to active; steady samples do not each sync a file. Readers
already age the recorded idle value between writes.
The worker retains the same scoped lock used by `hgs presence set` until its
last sample/write has returned and it has joined. A concurrent legacy writer is
refused. Unknown idle or lock input never defaults to zero/unlocked or refreshes
the previous record. Wayland durations measured only since observation began
are also withheld until actual resumed input is observed. Thus a service restart
does not turn an hour-away owner into a recently active owner. Existing records
expire normally after 120 seconds without valid samples. Status distinguishes
published, unknown-input, write-error and incomplete-shutdown states.

Revoking background mode stops and drains its own streams before returning a
checkpointed receipt. These receipts bind the service incarnation; an old revoke
cannot stop a replacement instance. Any retained revoke (even interrupted) or
historical handoff state also prevents background startup after login/restart:
the endpoint stays passive with `background_start.code=explicit_handoff_required`.
Passing `--background` again never clears that block. A new explicit handoff is
part of the coordinated UI cutover, not an automatic service restart.
Incomplete shutdown is reported as an error,
never as drained. Native agents, tmux and unrelated services are not stopped.

The existing `hgs presence set --json` payload and success output remain
compatible. Its writes now hold a scoped `desktop-presence.lock`. Before any
handoff, a passive service leaves that legacy producer available. A supported
non-revoked marker or active service policy refuses legacy writes and names
`hgs desktop revoke --json`; unknown/newer/mismatched state also refuses without
repair. Revoked, inactive policy allows the legacy path again. Future service
epoch activation and presence writes must use this same scoped fence. This
admission guard enables no sampler, producer lease or service writer. Presence
listings read at most 4 KiB of a private regular leaf; malformed, oversized or
special files provide no presence evidence.

The private `desktop-v1` service kernel is local to one user and canonical
configuration/state identity. Plain `serve` is passive; explicit background mode
owns the fleet readers and presence writer described above. Read workers,
notifications and automatic approvals remain inactive pending their integration.
Maintained installers register the service with the OS manager. Desktop client
cutover and native posting are separate changes.

`status` is read-only and never starts a service or creates state. Its JSON
reports `running`, `ownership`, `configuration`, executable `version`, schema,
generation/epoch, pending counts, component state and incomplete upstream
coverage. IPC connectivity does not establish peer freshness. An absent service
reports `running:false`; an unsafe or incompatible endpoint is an error.

`serve` holds an exclusive OS file lock under `HGS_STATE_DIR/desktop`, opens the
private versioned `state.sqlite3` and serves a same-user Unix socket or Windows
named pipe. A newer schema is refused without migrating/resetting it. Changing
configuration with the same state refuses reuse; canonical path aliases share
ownership. There is no agent/terminal/process shutdown operation. SIGTERM or
SIGHUP on Unix stops admissions, detaches clients and checkpoints only desktop
state. Native Windows graceful shutdown qualification remains separate.

Schema 2 adds the hgs-owned native-effect ledger. Schema 1 migrates
transactionally after the configuration check. Its writer policy starts revoked
and every process restart fences it again; manual revocation reasons survive.
`effect_journal` reports policy, a reason code and four maintained attempt counts.
Its `reason` describes writer revocation, not a native notification refusal.
`recent_failures.refused` and `.unknown` show at most the latest attempt of each
state by reservation sequence: `seq`, action `kind`, the closed receipt `code`
and `recorded_at` (Unix milliseconds when the actor recorded the receipt).
They expose no notification content or session identity. Schema 31 adds the
optional recorded time; older and restart-recovered attempts retain a null time.
Missing, corrupt or unrecognized receipt details have `details_available:false`
and null fields; they do not reveal raw errors. A failed diagnostic read sets
`recent_failures.available:false` without hiding the maintained counts.
The two reads use the attempt-state index and bounded identity/receipt strings;
status never scans history, retries an attempt or changes delivery permission.
Recovery marks at most 256 prior-process reservations unknown at startup and per
heartbeat. Neither reserved nor unknown work becomes a fresh delivery after a
restart. Recovery failure reports a component error and pauses admissions while
the passive service keeps serving IPC. The ledger library has no native I/O,
automatic-approval consent or read-mark authority. The current service exposes
no reservation, native adapter or writer-takeover command yet.

Schema 20 adds indexed attention retention. An enabled discovery turn examines
at most 32 blocked, non-live mappings and compacts at most 256 KiB of mapping
and baseline-intent data. Only mappings without any attempt record qualify;
live mappings and all reserved, delivered, refused or unknown attempt identities
stay intact. One transaction replaces each eligible mapping with its exact
32-byte logical suppression hash, removes its unreferenced blocked baseline
intent, and updates storage totals and the durable GC cursor. Interrupted
compaction rolls back all of these changes together.

Count or byte pressure defers the original discovery row while bounded GC
advances; it does not permanently fail discovery or busy-loop the actor.
Suppression hashes survive restart and prevent an old logical edge from posting
again after rename, resume or Tracker history eviction. A hash hit remains
blocked without recreating the discarded payload. These hashes are append-only,
32 bytes each before SQLite overhead, with no time-based expiry. Owner-scoped
reclamation requires proof that the owning conversation is absent from both its
authoritative catalog and every restorable archive; that separate sweep is
tracked in `zerus-n3qm`. Catalog absence or an offline source alone never proves
that an edge cannot reappear.

Mandatory startup questions can notify before SessionStart supplies a
conversation. Their separate identity contains the exact run and native
question ID; names, host leases and timestamps never substitute for either.
They use the same focus, baseline, supervised-child and outbox rules as other
attention. Answering withdraws the original notice. When the conversation
binds, its startup notice is retired; the same still-visible question is consumed
in the conversation scope rather than posted again. A different question is a
new edge. Reconnects and upgrades preserve the original intent and unknown
delivery is never replayed. Schema 32 adds the indexed run lookup needed for
simultaneous sessions awaiting their first conversation. A startup click also
requires that same run and question, and cannot follow a replaced run or a
question that has already ended or acquired a conversation.

Schema 3 adds the indexed revocation command journal. Before a maintained Qt
rollback or Qt installation starts its GUI, the installer must obtain a settled
receipt from this exact configuration:

```sh
hgs desktop revoke --json --request-id UUID --reason rollback
```

Before invoking it, probe `hgs --help` for the exact capability marker
`Desktop revoke ABI: desktop-revoke-v1`; a desktop-v1 kernel alone need not
support revoke. An older selected CLI must never be called speculatively.
The installer generates and retains a canonical UUID for this attempt. A
settled receipt requires exit zero and `accepted`, `durable`, `revoked` and
`checkpointed` all true, with the same `request_id` and configuration. When
there is no desktop database and no handoff marker, exit zero instead returns
`applicable:false`, `handoff:"none"`, `revoked:true`, `nothing_written:true`
and the exact request/configuration identity. This explicit no-op preserves
plain rollback without creating directories, locks or a database; an existing
live owner is still refused. It claims no durable command reservation. Failure, timeout or missing
receipt keeps Qt stopped. The command contacts the existing actor; when no
service endpoint exists it instead takes the real exclusive service and handoff
locks and opens only the desktop journal. It starts no service manager, listener,
agent or GUI. An unreachable owner holding its lock cannot be bypassed.
This CLI primitive does not yet wire the maintained installer rollback hook.
If `owner.lock` is held but IPC does not answer, keep Qt stopped. Stop only
`hgs-desktop` through its owning service manager (Linux `hgs-desktop.service`,
macOS `com.hgdev.hgs-desktop`, Windows task `hgs-desktop`), then retry the same
request in the same configuration. Never stop native agents, tmux servers,
process groups, recovery, voice or mobile connector services. An unmanaged or
uncertain owner requires operator inspection; its lock is never bypassed.

Revocation commits its private audit reservation and revoked writer policy
before draining owned work, atomically changes only a supported marker's phase
to `revoked`, syncs it and verifies SQLite checkpoint completion. No marker is
created when none exists. Unknown/newer/malformed or mismatched markers are
preserved and refused. A busy checkpoint or failed drain leaves a durable
pending command, never a successful receipt. The current passive kernel has no
effect workers to drain; later native dispatchers must provide lease cancellation
and owned-worker drain before takeover is enabled.

A repeated command must keep the exact UUID and reason. Its reservation pins
the original writer epoch; it cannot revoke a different later handoff or return
an old success for newly authorized writers. A running service can inspect a
result with `hgs desktop request --json` and stdin
`{"op":"revoke_status","request_id":"UUID"}`. The query never resumes a
pending transition. Explicitly repeating the same revoke may reconcile that
same pending authority reduction; it cannot deliver native effects. An unknown
result never permits the installer to start Qt or invent a fresh request ID.
The journal keeps reason/history privately; the marker payload stays unchanged.

Every IPC connection begins with one bounded JSON line containing `abi`,
`configuration`, `op` (`status`, `watch` or `request`) and optional `request`.
The CLI adds this private handshake. Frames contain `abi`, random service
`epoch`, broadcast `seq`, `t` and `value`. A watch begins with `hello` at the
current broadcast watermark; subsequent ten-second heartbeats advance `seq`.
Unary answers do not advance the broadcast watermark. A new epoch invalidates
subscriptions; it never authorizes replay. EOF on a watch's stdin detaches that
client. Control requests use a separate connection, not the display stream.

Status, watch hello/heartbeat and `catalog_changed` include `attention_counts`.
Its `shared_host_state` scope uses the existing session projection, shared read
marks and shared mute settings; `local_marks_included:false` means device-local
Review later and acknowledgement overlays are still separate. These presentation
counts confer no posting, read or approval authority. Per-computer entries carry
source incarnation/generation, online and complete flags. The aggregate reports
unknown attention as null while any expected computer is incomplete or offline;
`known_attention` is explicitly partial. The service epoch, projection generation
and revision bind counts to the same subscriber stream. A restart invalidates
coverage and catalog cursors, preserving only last-known display rows.

The fleet runtime seam reuses the existing watch-v1 source supervisors: at most
64 computer workers, four concurrent connection attempts and 16 MiB/1,024 ingress
items, with permits retained through actor application. There are no per-session
workers. Reconnect uses the existing 5/15/60/300-second backoff and resets source
generation; gaps trigger a complete resnapshot. Removed aliases retire their
exact projection and use the normal bounded collector. Slow UI subscribers never
hold source ingress. The active service lifecycle starts and drains this runtime;
the passive kernel and UI feed cannot activate it.

`hgs desktop request --json` reads a JSON object from stdin with `op` and a
canonical UUID `request_id`. `ping` returns an exact-ID, read-only `pong` with
`durable:false`.
`revoke` reduces authority through the durable command journal described below;
`revoke_status` reads its exact request ID without resuming it. Positive effect
requests return `accepted:false` and `handoff_required` before acceptance.
No positive command is queued for later replay or delegation implied.

Control lines including framing are capped at 64 KiB. Output frames allow
watch-v1's 1 MiB plus 4 KiB envelope metadata. Eight subscribers each have at most
4 MiB/1,024 queued frames; overflow disconnects only that subscriber. Four
additional connections preserve the unary request/status lane. Admission holds
at most sixteen control lines and twelve parsers, below the proposed 16 MiB
ingress budget. The kernel has no upstream ingress or per-session cache.

The service and the read-only importer share the configuration fingerprint from
`zerus-desktop-migration`. A running service reports a small `migration` summary
in status/watch frames. It checks at startup and each ten-second heartbeat,
under the separate `desktop/handoff.lock`; status requests use that cached
summary. The marker (64 KiB maximum) and fixed import manifest (256 KiB maximum)
must be private single-link regular files beneath private directories. Links,
malformed/newer/mismatched input and bad digests stay unchanged and keep the
service passive. Import pages are not scanned by this diagnostic probe.

`import_not_drained`, `handoff_incomplete` and `revoked` describe historical
import state. Even a complete, drained marker reports `delegation_required`: it
cannot enable writers or approvals without the later live delegation, effect
guards and revocation protocol. The kernel writes no takeover marker. See the
exact [migration contract](desktop-migration.md).

In service mode, Settings > Files > Apply saves the selected folder's cache
limit in the service. Imported UI preferences remain display data until that
explicit action. Refresh and download completion use the saved service policy;
they never fall back to UI-owned deletion if the service is unavailable. A
native request first prepares a bounded plan under the existing Qt folder lock.
A fresh original frontend child must then admit that exact plan before the
service reserves any removal. The original two-second native permit is never
extended to cover slow filesystem preparation.

One pass considers at most 16 files from an index of at most 1 MiB and 1024
entries, and verifies at most 64 MiB of content. Edited, linked, replaced and
unindexed files remain intact. Download completion admits fixed work with a
65-second opening grace; the folder lock is released while waiting, and any
change to its frozen index or files keeps the original copies. The admitted
pass can finish after the UI closes. Revoke stops new effects and remains
unsettled until the original worker physically joins. After the grace and exact
reopen, the actor durably marks effect start before releasing the original
worker into execution. A missing start reply cannot authorize unlink. An admitted
reservation without a committed start mark can settle as Kept from its retained
original receipt only after physical join or exclusive-owner restart; cache
totals are never reconstructed. Older reservations without this evidence remain
conservative. A lost result after the start mark is terminal unknown for that
original file/content identity, including after a rename or an unrelated index
edit. Another request ID cannot clear that fence.
The read-only `cache_maintenance_result` request accepts the original request ID
and returns its bounded receipt; it cannot prepare, retry or authorize work.

`hgs desktop registration --json` previews the current platform's per-user
passive registration and includes a `readiness` probe for that exact binary and
configuration. Run its `argv` directly with the two supplied `HGS_CONFIG_DIR`
and `HGS_STATE_DIR` values, then require the returned configuration to equal
`expected_configuration`. `lifecycle.ipc_ready` means the private endpoint and
store are ready; it does not mean background writers are active. The same
`lifecycle` object supplies the actual service PID, epoch and generation, plus
per-component readiness. Its epoch changes on service restart. The registration's
`session` object describes the graphical login environment needed by platform
installers; it is a requirement, not a claim that the environment is present.

`lifecycle.console_released` is true when the Windows service was given a
console window of its own and released it before serving, so no window stays
open. A logon task or a COM activation gives a console program such a window.
It is false when the service shares its console with another process, such as
the terminal it was started from, when it had no console, and on Linux and
macOS. It is diagnostic only; readiness never depends on it.

`lifecycle.mode` reports the running command's `passive` or `background`
selection independently of worker admission. On all three desktop platforms, `registration --json
--background` renders the explicit background variant without installing it;
`registration mode-check --json` checks the actual installer caller ancestry and
foreground terminal or installed frontend on Linux, macOS and Windows, without
granting service ownership. Windows requires a retained installed frontend
ancestor or an already attached visible foreground classic console; unverified
Windows Terminal/ConPTY and headless callers are directed to Settings. This
preflight changes no task; explicit Windows task changes use the separately
guarded [mode transition](desktop-windows-mode.md).
The [macOS ancestry boundary](macos-distribution.md) verifies Terminal's system
login bridge and launchd PID 1; SSH through a foreign session process refuses.
The maintained Linux helper's `status --json` reports the durable selected mode
separately from the running mode and any pending restart/revoke transition.

The preview also contains the per-user definition and exact configuration identity.
It creates no files, contacts no service manager and enables no writer.
Its `operation:preview` identifies this
render-only operation; it makes no claim about OS registration. Maintained installer
ownership checks and delegation are separate steps before using the definition.

On Windows, `hgs desktop notification-registration install|status --json`
manages only the owned `com.hgdev.hgs-tray` AUMID/COM registration. Install the
passive service task first so the selected stable CLI matches the current
installer. Status is metadata, not native notification readiness. The fixed
cold activation entry refuses before configuration or owner acquisition. No
worker repairs registration or enables background consent. The background
service checks the registration against its own running image and configuration
folders, not its `HOME`; another registered image is reported as
`notification_registration_other_image` and changes nothing. That diagnostic is
never removal authority. Only `notification-registration remove`, after a fully
read mismatch with the current image's registration, may select an earlier
image's registration instead. The registered command must be byte for byte what
this build writes for that absolute image with the CURRENT configuration and
state folders, and all three keys must match this build's exact contents for it
(same SID and configuration hash, this ABI marker, no extra or missing value or
key). The selection is fixed under the registration lock before the offline
guard and removed through the same guarded exact removal; the reply adds
`previous_image: true`. Install and status never take it, and nothing falls back
after an effect.

Before an admitted Windows notification post, a successful `Setting` getter
must report Enabled. A real disabled setting refuses as `permission_denied`;
a getter failure refuses as `getter_failed`. Only `ERROR_NOT_FOUND`
(`0x80070490`) permits one identity-less initialization attempt per service
process: a suppressed toast with no session content, a 15-second expiry,
group `hgs-bootstrap` and a fresh 16-character tag. The worker immediately
removes that exact tag/group for this AUMID and rechecks `Setting`; only Enabled
allows the original user notification. Failure, cancellation and worker restart
never reset the attempt. Status and withdrawal do not initialize an identity,
and initialization never repairs registry/COM registration or changes consent.
SuppressPopup prevents a popup banner, not an Action Center entry. Immediate
exact removal is the normal path. Only if that removal fails may the brand-only
scratch entry remain until its 15-second expiry. It carries no launch argument
or delegates: empty activation arguments are rejected, and the cold COM entry
exits 78 without opening a session or GUI.
This follows the upstream [identity-less notifier initialization](https://github.com/CommunityToolkit/WindowsCommunityToolkit/blob/c8f76d072df53d3622fb5440d63afb06cb9e7a10/Microsoft.Toolkit.Uwp.Notifications/Toasts/Compat/ToastNotificationManagerCompat.cs)
used before its [Setting getter](https://github.com/CommunityToolkit/WindowsCommunityToolkit/blob/c8f76d072df53d3622fb5440d63afb06cb9e7a10/Microsoft.Toolkit.Uwp.Notifications/Toasts/Compat/ToastNotifierCompat.cs).

Explicit removal uses `notification-registration remove --json --request-id UUID`
after a settled `desktop revoke --json --request-id UUID --reason rollback` with
that same UUID, a stop of the running service through its owner (ending only
that task, or a sign-out after the task's `registration remove`), and the
task's `registration remove`. Removal is
offline only. It holds the actual `owner.lock` and then the handoff lock through
the exact registry change; a missing endpoint, stopped task or PID is never the
exclusion. Without desktop state nothing more is needed. A sidecar or marker
without its database refuses. Existing state, also from a passive-only history,
needs the CURRENT proof of the shared read-only rollback check in one checked
transaction:
- the UUID is the newest command, settled with its exact receipt;
- ownership is revoked and rearm-required with no request or owner epoch;
- the writer is revoked under that rollback's writer;
- any marker is Revoked for the same writer;
- operations are drained.

A later grant, rearm, handoff or revoke supersedes the proof, and removal never
submits or reconciles a revoke. The checked database is retained, and the proof
is checked again right before the registry effect. The read-only lookup may
leave SQLite's reader sidecars (an empty `-wal` and the `-shm` index) next to a
cleanly stopped database; the database itself is not modified. UI-only
uninstall keeps the service task, staged CLI and Toast identity; its separate UI
login entry is removed normally. While the service runs, removal remains
unavailable until root Windows Runtime drain attachment is integrated. Metadata
removal does not claim native history deletion.

The proposed service names are `hgs-desktop.service` (systemd user),
`com.hgdev.hgs-desktop` (LaunchAgent), and `hgs-desktop` (per-user Windows logon
task). Definitions invoke the selected stable CLI with `desktop serve
--config-dir DIR --state-dir DIR`; this exact local form pins those directories
before configuration loading, including on Windows. All three paths must be
absolute for the target platform. Bare `desktop serve` keeps
existing `HGS_*` configuration selection. Nothing registers nondefault or dev
configurations automatically. Windows definitions use the current SID,
`InteractiveToken`, `LeastPrivilege`, one instance and at most three failure
restarts one minute apart (the [Task Scheduler schema minimum](https://learn.microsoft.com/en-us/windows/win32/taskschd/taskschedulerschema-interval-restarttype-element)).
Paths with environment-variable markers are refused on Windows because Task
Scheduler expands them even inside quoted arguments. Linux permits three starts
within five minutes and does not restart a startup refusal (exit 78). The Linux
unit is ordered after and is part of `graphical-session.target`, and is enabled
for that target rather than `default.target`. Plasma and GNOME establish this
session after importing the graphical environment into the user manager. Bare
window managers without that integration must arrange a session target that
pulls in `graphical-session.target`, or import their session environment and
manually enable/start `hgs-desktop.service` for that login. Direct manual start
of `graphical-session.target` itself can be refused by systemd. macOS adds
the private `--launchd` argument to the exact registered form: permanent startup
refusals print a diagnostic and exit successfully, so
[`SuccessfulExit=false`](https://github.com/apple-oss-distributions/launchd/blob/main/man/launchd.plist.5)
does not restart them; failures after startup still return nonzero. A refused
service needs explicit repair and another maintained start. Native installation,
windowless startup and Windows runtime qualification remain outstanding.

`hgs desktop import-evidence --json` publishes bounded private **Staged**
import evidence from G3's inherited-stdio stream. It reads no source store,
starts no service and grants no writer authority. Probe the exact
`Desktop import ABI: desktop-import-v1` help marker first. Input deadlines,
fixed digest paths, immutable UUID commits and the read-only `import-status`
lookup are specified in the [desktop import transport](desktop-import-transport.md).

### Event stream

```sh
hgs watch --json
hgs @mac watch --json
```

`watch --json` is a long-lived stream of one computer's live sessions, of the
Activity of the sessions a client follows and of the Git status of the checkout
folders it follows. It is additive: every command above keeps its output. `hgs -h` prints `Event stream ABI: watch-v1` where it exists.
A client that does not find the line keeps polling that computer. `@host` runs
the peer's stream over one SSH connection with `ServerAliveInterval=10` and
`ServerAliveCountMax=3`. An older peer answers with an error, never a launch.
From Windows, `hgs.exe @host watch --json` relays its streams to `ssh.exe`
through overlapped pipes as every remote command does, so control lines and
frames pass as they are written and the end of its input reaches the peer.

The stream writes one JSON frame per line on stdout and reads one control
operation per line on stdin. EOF on stdin ends it with exit code 0, so a closed
client or a dropped SSH link leaves no watcher behind. A closed stdout ends it
quietly. The frame types live in `crates/watch-protocol`.

Every frame has `v` (1), `t` and `seq`, which grows by one per frame. The first
frame is `hello` with `machine_id`, `host`, `hgs_version`, a random `stream`,
`heartbeat_ms`, `max_frame_bytes`, `topics` and `limits`. A `heartbeat` with `at`
follows 10 s without other frames.

| `t` | Fields | Meaning |
|---|---|---|
| `snapshot` | `topic`, `key?`, `items` or `value`, `page`, `last`, `cursor` | a replacement; keep the previous display until `last` |
| `delta` | `topic`, `upsert[]` of `{key, row}`, `remove[]` of `{key, reason}`, `cursor` | catalog changes |
| `events` | `topic`, `key`, `events[]`, `cursor` | new Activity events, oldest first |
| `state` | `topic`, `key`, `value`, `rev` | the latest summary of one session, or one folder's Git status |
| `event` | `topic`, `key`, `id`, `rev`, `event` | a message between tools; a higher `rev` of an `id` replaces it |
| `gap` | `topic`, `key?`, `reason` | discard the topic's state; a snapshot follows |
| `error` | `code`, `message`, `topic?`, `key?`, `fatal` | a refused operation or a failed source |
| `pong` | `id` | the answer to `ping` |

The `catalog` topic is always on. Its rows are exactly the `sessions` of
`hgs ls --json --local --no-archives`, keyed `run:<run_id>`, or `name:<name>` for
a row without a run. The stream starts with a snapshot of them in pages of at
most 512 KiB, then sends deltas. A rename is an upsert of the same key. A removal
gives `replaced` (the name lists under a new run), `archived`, `forgotten` or
`ended`. The listing's other fields arrive as one `catalog.meta` snapshot: at
once when a lasting field changes, and at most every 10 s when only `metrics` or
`desktop_presence` changed. Archives stay outside the stream; page them with
`archives --json`. A row retains its `launch_id` when it has one. For launch
binding, `catalog.meta` always includes `ended_launches`, including an empty
array when no launch has ended; its newest-first, 32-entry shape is the same as
`ls --json`. This is additive watch-v1 data. A client treats a missing field on
an older peer as a reason to retain its bounded `ls --json` binding poll, and an
explicit empty array as supported with no results.

```json
{"op":"subscribe","topic":"activity","session":{"name":"claude/sample-project/review","run_id":"RUN","conversation_id":"CONVERSATION"},"after":123,"tail":50}
{"op":"unsubscribe","topic":"activity","session":{"name":"claude/sample-project/review","run_id":"RUN"}}
{"op":"ping","id":1}
```

An Activity subscription follows one session by its catalog key. Its events are
the objects `inspect` returns, and `cursor` is the journal `seq` that
`inspect --after` takes. Without `after`, the first `events` frame carries the
newest `tail` events (at most 100). With `after`, it carries the events after
that `seq`, at most 200 per frame read. When `after` is older than the journal
keeps, or newer than its newest row, the stream sends `gap` (`retention` or
`reset`) and a tail `snapshot`. DeepSeek and OpenCode sessions answer from their
own hosts: each change replaces their event window with a `snapshot`. `state`
carries the `inspect --skip-processes` summary without `events`, `cursor` and
`history_truncated`. It is sent when anything other than `goal_observed_at`
changed.

For Codex and Claude sessions the stream follows the conversation's native
transcript from where it ended when the subscription began, and sends each
public message written there as an `event` frame within about half a second:
an `AgentMessage` between tools or Claude's `AgentThinking`, the same object
`inspect` lists in `provider_messages`, with the same privacy filters. Its `id`
is the message's `message_id`, or `{type}@{at}#{hash}` (16 hex digits of the
SHA-256 of its `detail`) when that is empty. A changed message with the same
`id` comes again with a higher `rev`; frames arrive in order. A client adds it
to the `provider_messages` of the latest `state`: it replaces the entry with the
same non-empty `message_id` and `type`, is dropped as a repeat of one with the
same `type` and `detail` within 3 s of its `at`, and is otherwise inserted in
`at` order, keeping the newest 100. The next `state` replaces the list and
includes these messages, so a client that ignores `event` still sees them,
summaries being re-read every 2 s while the agent is busy. A replaced or
shortened transcript, or a new conversation, is followed again from its end.
Kimi, DeepSeek and OpenCode messages arrive only through `state`.

A session that leaves the catalog ends its subscription with the error
`subscription_ended`. Unsubscribe stops it. A stream follows at most 16 sessions;
more fail with `too_many_subscriptions`. A topic `hello` does not list fails
with `unsupported_topic`; read it with its command.

```json
{"op":"subscribe","topic":"git","path":"/home/user/sample-project"}
{"op":"unsubscribe","topic":"git","path":"/home/user/sample-project"}
```

A git subscription follows one checkout folder, an absolute path of at most
4096 bytes. Its `state` frames are keyed by the path as subscribed and carry
exactly the object `git-status --path PATH --json` prints, also for a missing
folder (`folder_unavailable`) or one outside a repository (`not_repo`). The
first follows the subscription; later ones only when the object changed apart
from `sampled_at` and `remote_age_seconds`. A new remote check changes
`remote_checked_at` and is sent. The stream runs the checks of `git-status`
itself, with its read-only Git options and 60 s remote cache, on two worker
threads, so a slow check delays no other frame. A folder is checked again when
a stat of `HEAD`, `index`, `packed-refs`, `FETCH_HEAD`, the reftable list, the
current branch's ref or its upstream's tracking ref changes in the folder's Git
and common directories; when a session working in the folder (its `cwd`,
`cwd_canonical` or `git_root`) records a finished `Edit`, `Write`, `MultiEdit`,
`NotebookEdit`, `apply_patch` or `Bash` call; when its remote check expires; and
at least every 30 s. Checks of one folder start at least 1 s apart. Streams
on one computer share checks: each check is kept in the private `watch-git`
folder of the state directory, and a stream takes a kept check instead of
running Git when that check started after the change that made the folder
due (the journal row's time for a session's call; for the 30 s check, at most
30 s earlier) and the stat of the folder's Git metadata is unchanged. A stream
that needs a folder while another stream checks it waits for that check, at
most 4 s. A stream that took a kept check checks again a second after the
stream that ran it, so several clients check each folder about as often as
one. A new subscription takes only a check that started after it.
Subscribing a followed folder again sends its state again. A stream follows at
most 64 folders (`limits.git_subscriptions`); more fail with
`too_many_subscriptions`, and a missing or relative path fails with
`bad_request`.

The desktop session list follows only its selected and visible rows (including
the virtual list's small overscan), at most 48 row identities across computers.
Native catalog rows supply the checkout paths; equal paths on different
computers remain separate. In service mode these are Git demands on the
original fleet/details lease, within its existing 64-demand budget and the
original Hello's Git limit. Native mode uses its existing watch supervisors.
Both retire old interests before replacements and fence a new subscription
with the original Pong. Hidden rows retain no Git cache. Unsupported topics,
failed reads and offline/replaced sources clear the display facts; there is no
independent GUI Git/SSH poller or automatic retry of a refused route. Checkout
freshness and remote verification age separately, and missing upstream/count
evidence never paints a clean, verified checkout.

The `processes` topic follows one session's processes, subscribed like Activity
(`{"op":"subscribe","topic":"processes","session":{...}}`) and keyed by its
catalog key. Its `state` value is `{processes, process_work}` exactly as
`hgs inspect NAME` reports them. The first follows the subscription; later ones
only when the value changed apart from both `sampled_at` fields. Two worker
threads run the inspection, so a slow `ps` or native host delays no other frame.
A session is read again when its catalog row changes and every 10 s by default;
reads of one session start at least 1 s apart. A processes subscription may
include `interval_ms` (clamped to 1,000–600,000 ms). Repeating the subscription with `interval_ms`
changes its interval while keeping the current inventory, revision and running
read. Adding `"refresh":true` requests one read as soon as the minimum gap
allows; repeated refreshes while a read runs coalesce into one follow-up read.
Both fields are additive watch-v1 options. Older servers ignore them: each
interval change or refresh becomes a plain resubscription, which reads again
at once without the minimum gap, resends the state as revision 1, then keeps
the 10 s default. Plain resubscriptions without either option retain the
original behavior: replace the subscription and resend its first state.
A session that leaves the
catalog ends with `subscription_ended`, and an inspection that fails sends a
non-fatal `inspect_failed`. A stream follows the processes of at most 16
sessions (`limits.processes_subscriptions`).

The `tasks` topic follows one project's task list:
`{"op":"subscribe","topic":"tasks","project":"<project ID>"}`, keyed by the
project as subscribed. Each list goes out as a whole `snapshot` in pages: its
`items` are the task views exactly as `hgs task list --project ID --json` lists
them, in that order, and the last page's `value` is `{project, damaged,
digest}`, where `digest` identifies the list, so a client can skip an
unchanged list after subscribing again. A worker thread reads the list. A
project is read again when a stat of its task store changes and at least every
30 s, at most once a second, and the list goes out only when its digest
changed. A list that cannot be read, such as an unknown project, sends a
non-fatal `tasks_failed`. A stream follows at most 8 projects
(`limits.tasks_subscriptions`).

A subscription may add `"window": N` (1 to `limits.tasks_window`, 200; a server
that announces 0 or omits it always sends the whole list). It then carries only
the first N rows of each stage, read from the task index, in stage order (In
review, In progress, To do, Backlog, Done), never the whole list. Every stage
is read at one index revision: a window whose stages were read at different
revisions is read again, at most 4 times, and then from the task documents read
once, so each task is in exactly one stage of a window. That fallback, like
the index's first build, reads every task document of the project once,
without a separate I/O or memory budget, and sends at most N rows of each
stage and the first damaged tasks. The last
page's `value` adds the `counts` of every stage, `damaged_total`, the
`revision` and `window: {schema: 1, limit, stages: {STAGE: {total, next}}}`;
each `next` is the cursor of that stage's next page for `hgs task list --stage
STAGE --limit N --after CURSOR`, or null when the stage has no more rows. A
window that is not 1 to 200 is a non-fatal `bad_request`.

#### Shared drive-folder component

Remote Windows drive-absolute project links use `hgs swarm drive-folders
get|apply|inventory|exchange --json` and the independent `drive_folders_v1`
capability, not a new main-catalog key kind. `get --operation UUID --json` is the
only result-reconciliation query. Ordinary get/watch/apply/export, closed legacy
exchange envelopes, mobile/API v1, project inference and launch readers retain
their POSIX/legacy contracts. A stored link grants no filesystem, worktree,
account or session-launch authority.

`drive-folders.json` and immutable `drive-folders-init.json` live beside
`catalog.json`. The graph has at most 64 MiB, 100000 raw operations and 1024
parents per operation; exchange is at most 8 MiB and 128 causally ordered
operations per round, with complete parent closure in retained history plus the
batch. A bounded native observation has at most 200 folders and includes only
current records, raw-head frontiers and review states, never a growing renderer
journal. Both files absent is UNINITIALIZED, not an empty authority. Only a
validated first trusted Add or explicitly negotiated initial import creates the
witness and then the graph. A partial, corrupt, foreign, nonregular, oversized or
changed pair is UNAVAILABLE: its bytes remain, with no empty reset, automatic
repair or assumed write result. Main and drive saves report independent
outcomes; no two-file atomicity is claimed. Backups record both originals and
hashes or explicit incompleteness.

Operations are immutable `{id,actor,clock,folder_id,parents,value}` records with
canonical nonnil UUIDs. A value is `{project,machine,path,name}` or a retained
historical tombstone. Paths are canonical drive-absolute Windows paths, at most
8192 bytes; project/name limits remain 128/320 bytes and controls are refused.
Only fresh locally-known-unused UUID Add and same-kind live Edit are admitted.
Edit retains project and ID and requires fresh native source and destination
Windows cards. Remove, Transfer, clone, resurrection and cross-representation or
cross-swarm rehome are outside this cut. Cross-swarm enrollment refuses a
nonempty, initialized-empty or unavailable component before any membership
mutation. Raw records received before their project/machine context remain
unavailable rather than being discarded or shown as current.

Complete local main+drive histories, including tombstones, prove known-unused
IDs. They cannot prevent an unaware old/offline writer from creating a colliding
legacy ID later. Such a collision preserves both histories and quarantines only
that logical identity: no winner, erasure, cross-kind causal edge or
whole-catalog failure. Unrelated legacy edits and other drive IDs continue.
Absolute global exclusion is therefore not claimed.

#### Drive-folder watch topic

The `organization` topic follows the shared project catalog as `hgs swarm get`
prints it: `{"op":"subscribe","topic":"organization"}`, one per stream. A
worker thread reads the stored catalog without its lock, never saving it and
never restarting the catalog worker. It reads again when a stat of the
catalog, its member lists or the worker's marker changes and at least every
30 s, at most once a second. The catalog travels as keyed rows that a client
applies like the catalog topic's:

- `organization` is the snapshot without `versions`, `written` and the
  projects' contents. Its `organization.projects` lists the project IDs in
  order, and it carries `digest` and `worker`.
- `project:<id>` is a project without `sessions`, with `session_chunks`, the
  number of its member rows.
- `members:<id>:<n>` is `{sessions}`, at most 1000 sessions in order.
- `version:<token>` is one `versions` entry.

The first rows go out as a paged `snapshot`. A later change sends one `delta`
with only the changed rows and removed keys, or a new snapshot when that would
not fit one frame, so every frame leaves a client consistent. Rebuilding the
rows gives `swarm get`'s object without `written`. An unreadable catalog sends
a non-fatal `organization_failed`. The topic reports `worker.current` but never
supervises the worker; a client that sees `false` runs one `hgs swarm get`,
which restarts an outdated worker as before. Writes, including the first
`swarm initialize`, stay with `hgs swarm`.

The separately negotiated `organization.drive_folders` topic follows the capable
Windows drive-folder projection with `{"op":"subscribe","topic":"organization.drive_folders"}`.
It never initializes, saves or repairs the catalog or either drive file. The
`drive_folders` summary row carries the bounded source/store identity, status,
operation query result and record/conflict IDs. `record:<id>` rows carry the
current records, `version:<token>` rows their raw-head frontiers, and
`conflict:<id>:<n>` rows at most 16 retained raw variants each. Unavailable and
incomplete states are data, never an empty known frontier. The legacy
`organization` topic, its file set and all legacy apply/export envelopes remain
unchanged by this component. Drive reads run on their own bounded worker when
either drive file changes, with the same cancellation, paging and resnapshot
rules as the organization topic. A client without this topic keeps polling
`hgs swarm drive-folders get --json` instead.

The stream checks its sources every 250 ms. It stats the bindings, read marks,
notification preferences, waits, launches, renames, name routes, the archive
folder and the DeepSeek and OpenCode bindings, and holds a read-only connection
to the Activity journal. A changed binding, or new journal rows of its session,
derives only that session's row again, through the steps and under the lock of
the listing; every row's parent state is then read again from all listed rows,
so a derived child never reads its listed parent as missing and a parent's
change reaches its children at once (zerus-sa9qn.16). Any other change lists
all rows again. Both wait 150 ms after a
change and start at most every 500 ms and at most once per four durations of
the previous one. Every 5 s the stream probes what no file shows: tmux sessions
that start, end, are renamed, attached or detached, exited agent processes,
ended background work, sessions that have not reported a hook event yet and the
DeepSeek and OpenCode hosts. All rows are listed again every 5 s, for terminal
choosers and status lines of sessions that already reported hooks. Followed
summaries are re-read when their journal rows or catalog row change, at most
every 500 ms, and otherwise every 2 s while the agent is busy and every 15 s
while it is idle; DeepSeek and OpenCode every 2 s.

The stream also checks every 5 s whether an update put another file at the path
of the `hgs` it runs. That file must differ in device, inode, modification time
or size, and be a regular executable file. A missing path, or a file not yet
executable, does not count. When the next check finds the same file, the stream
sends a fatal `error` with code `hgs_replaced` and exits 0. Without this check,
a stream left running after a CLI-only update, or after a peer's update, would
keep sending the old code's summaries (such as Clear's reason) while new
commands run the new code. A client starts the new stream at once and keeps its
rows. Only one such restart a minute skips the delay; another within that minute
waits like any failure.

A frame is at most 1 MiB including its newline. An event too large for a frame
is cut with `"truncated": true`, and an oversized summary loses the oldest items
of its largest lists, named in `stream_truncated`; `inspect` returns them whole.
The outgoing queue holds 4 MiB. When the client reads slowly, catalog changes
and summaries coalesce per key. When the queue overflows, the topic receives
`gap` (`overflow`) and a fresh snapshot once the client catches up. Control lines
are at most 64 KiB.

The desktop runs one stream for each computer whose CLI prints the marker.
While a computer's stream is live, the desktop stops polling that computer's
listing and follows the selected session's Activity through the stream; the
Processes tab keeps its own poll interval. When the stream offers the `git`
topic, it also follows the checkout folders of that computer's sessions, the
selected one first, then shown rows, at most 64, instead of starting
`git-status` for each every 15 s. A followed value counts as checked every 15 s
while the stream follows it; its remote's age keeps counting, so a remote shows
verified for at most 60 s after its check. Refused folders, and all folders of
a computer whose stream is not live, are polled as before. Listings are applied at most every
2 s, Activity within 50 ms. A stream that fails or stays silent for 30 s
restarts after the reconnect schedule (by default 1 s, 2 s, 5 s, then every
10 s; see Machines and accounts), and the desktop polls meanwhile. `ZERUS_EVENT_STREAM=0` or the setting `dataPlane/eventStream=false`
keeps the desktop on polling.

## Validation

Use [CONTRIBUTING.md](../CONTRIBUTING.md) for standard checks. Integration tests
use synthetic fixtures and isolated tmux servers; do not run them against live
agent state. A separately installed native CLI or real desktop may be required.

```sh
cargo build --locked
cargo test --locked
HGS_TEST_BIN="$PWD/target/debug/hgs" bash tests/test_hgs.sh
HGS_TEST_BIN="$PWD/target/debug/hgs" python3 tests/test_pause.py
HGS_TEST_BIN="$PWD/target/debug/hgs" python3 tests/test_accounts.py
HGS_TEST_BIN="$PWD/target/debug/hgs" python3 tests/test_account_lifecycle.py
```

On macOS, the last suite can also verify SSH/tmux entry into the graphical security
session and needs the installed HGS desktop session service. The Mach protocol
can be checked without accessing Keychain:

```sh
clang -Wno-deprecated-declarations tests/test_macos_session_protocol.c \
  -framework Security -framework CoreFoundation -lbsm -o /tmp/hgs-session-protocol-test
/tmp/hgs-session-protocol-test
```

Process coverage includes exact call identity, background tasks, stale generations,
independent output cursors, targeted stopping and offline/read-position behavior.
Native DeepSeek tests use a local keyless model rather than paid API requests.
