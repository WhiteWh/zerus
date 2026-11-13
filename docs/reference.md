# Zerus reference

[Product overview](../README.md) | [Installation](installation.md)

This reference covers session lifecycle, desktop workflows and native integration
limits. Run `hgs -h` for the complete command syntax.

## Command guard

Zerus checks shell command text before execution in tracked Claude, Codex and
Kimi `PreToolUse` hooks (including subagents), and in the DeepSeek bridge's
`bash` and `pwsh` tools. A refusal starts with `Zerus blocked this command:` and
tells the agent how to correct it. Hook refusals exit with code 2; DeepSeek
receives a tool error with code `HGS_COMMAND_GUARD`.

The first phase has three rules:

- `readonly-variable`: writes to PowerShell read-only automatic variables,
  including assignments, loop variables, parameters and `Set-Variable`. Use
  a dedicated name such as `$appHome` instead of `$HOME` or `$PID`.
- `protected-path`: recognizable deletion, move-source or destructive mirror
  targets that name a filesystem root, a home directory, system directories,
  or protected profile directories such as `.ssh`, `.codex` and `AppData`.
  Ordinary cleanup of an explicit disposable subdirectory remains available.
- `empty-variable`: deletion targets such as `"$out\\*"` that can become a root
  wildcard when a variable is empty. Use a preceding nonempty literal path
  assignment in straight-line code, an explicit path, or shell `${VAR:?}`.
  Conditional assignments and dynamic reassignments do not establish safety.

This check is independent of permission modes (including `bypass` and
`danger-full-access`) and automatic tool approvals. There is no override switch.
It only inspects text: no commands, expansions, filesystem lookups or network
requests run during inspection. Lexical path normalization and known literal
assignments catch common accidents, but this is not a shell interpreter or a
sandbox. Computed paths, symlinks, encoded scripts and other execution languages
need runtime protection beyond this phase. OpenCode is not covered. Commands
over 256 KiB or more than eight nested shell command wrappers receive a
`protected-path` refusal asking the agent to split the command.

A running DeepSeek host retains its loaded bridge. It needs a restart after its
sessions have been safely closed to load the guard; upgrading Zerus never
restarts or hot-reloads a host that owns live agents. The shared acceptance
corpus is `tests/fixtures/command-guard.json`.

## Subagent confirmation

Zerus asks the owner before an agent starts subagents. The setting is on by
default, including existing installs, and is kept per computer in that
computer's hgs state, where the hooks that ask run. `hgs subagents-confirm get
--json` reads it and a bounded set request
`{"request_id":"UUID","confirm":false,"by":"human"} | hgs subagents-confirm set --json`
changes it, also where local sessions are not enabled; Settings > Sessions >
"Ask before an agent starts subagents" uses them. Neither goes to another
`@host`, an SSH peer or a WSL profile: set it on each computer whose agents
should not ask.
Only the owner changes it: a set from an agent session (a caller record, a dsh
conversation, an `HGS_SESSION` or an `HGS_RUN_ID`) is refused, so a member whose
own subagent call was denied cannot turn the ask off for every later session.
The same gate guards `hgs session-upload-block set --json`
(`{"request_id":"UUID","block":false,"by":"human"}`), which turns DeepSeek's own
session-log upload off for every later dsh host on that computer.
When it is on, Claude's `PreToolUse` hook answers an `Agent`/`Task` call with
`permissionDecision: "ask"` and a reason naming the subagent type, its model
(the tool input, else the definition in `.claude/agents/<type>.md`, else
"inherits <session model>"), its effort when known and the description.
Subagent calls of one turn are grouped as "Start N subagents: ...".

The ask becomes Claude's permission request. For a message that starts several
subagents, Claude asks for all of them at once and shows them one at a time
("1 of N"); each request starts a waiting `PermissionRequest` hook, and the
session record keeps the last one. Its card, "Subagents", lists every open call
of the group with **Yes**, **No**, a denial with your message and **Yes, and
don't ask again for subagents**. One answer decides every call the card
showed. The hook of the card's request answers it. The hook of every other call
of the batch waits for its group rather than for a card of its own, also after
another request has taken the record, and hands Claude the same allow, denial
or message for its own call. A hook matches its call by tool and input, as the
channel binds. Claude's requests for identical calls cannot be told apart, so
each of their hooks hands over only an answer all of those calls got. A new
call joining the group changes the card, so a one-time answer never covers a
call it did not show. A waiting hook stops with its turn (a new prompt, Stop or
SessionEnd), when Claude cancels it or the call ends, or when the session's
agent, run or conversation changes; it then removes its files.

An answer reports `claude_hook` only once every hook it was handed to says
that it gave Claude the answer; Claude stops the other hooks of a turn that
**No** ends, so they count. A call of the batch without a waiting hook (its
hook ended, or an older hgs) still waits in Terminal: the answer names it as
undelivered, and the session keeps waiting for an approval rather than showing
as working.

A card answered on the screen (no hook took its request, for example a request
too long to show) still goes to the hooks when every call it shows has a
waiting hook or one that already answered, so keys never land on the next
queued panel after a hook answered the one shown; its receipt then reports
`claude_hook`. Otherwise the keys answer the panel shown and the hooks the
other calls.
Without the waiting hooks (an older Claude Code, or the shell hook form on
Windows), the keys answer only the call on screen.

"Don't ask again" for subagents is **Allow all, don't ask again** on the
desktop (after a second **Confirm**), the card's own option on the phone, or
Claude's own "Yes, and don't ask again ..." on a screen card. It stops asking
for the rest of that Zerus session: across turns, a new conversation (/clear,
compaction) and a resume, until a fresh start, a fork or a new session with the
same name. It also allows every call still waiting in the group, including one
that joined after the card was drawn: the one exception to an answer covering
only the calls its card showed. Later subagent calls of the session get no
decision from Zerus, so Claude's own policy applies; they join no group.
Through a hook, Zerus also hands Claude the rule its own panel offered for that
request (`updatedPermissions` from the request's `permission_suggestions`,
only rules that allow `Agent` or `Task`), for this Claude session only: their
destination becomes `session`, so Claude saves no lasting rule. The allowance
is kept in this computer's hgs state and removed once its session is gone
(terminated into Archive, forgotten or replaced by a fresh start); a
permission hook or a new allowance looks for such files.

Claude's settings files never switch the ask off: an allow rule an agent
writes or a repository commits is no answer of the owner's, and a session with
`Agent` already allowed in its settings is still asked. An allow rule counts
only as the owner's "don't ask again" in Claude's own Terminal panel: one for
the call's subagent type that appears while its hook waits, in the user's
settings (`settings.json` in `CLAUDE_CONFIG_DIR`, else in the account home
Zerus started Claude with, else in `~/.claude`) or in the session folder's
`.claude/settings.local.json`. That hook then allows its call and records the
Zerus session allowance, so the rest of the batch goes ahead too. A rule that
existed when the hook started never counts. `Agent`, `Task`, `Agent(*)`,
`Task(*)` and an empty `Agent()` cover every subagent; `Agent(Explore)` covers
only that `subagent_type`, and a call without one is `general-purpose`. Any
ask or deny rule for the call in those files or the folder's
`.claude/settings.json` means no, and so does a settings file that cannot be
read. Managed settings, `--settings` files and the local settings of an
enclosing repository root are not read, so a rule saved there leaves the batch
to the card.

The card also lists its calls in `subagents` (`id`, `type`, `model`, `effort`
when the tool input or the agent definition gives it, and `description`); the
desktop shows one row per subagent with Allow all, Allow all, don't ask again
and Deny, and the question text stays for older readers. A member's permission
record and the lead's inbox item carry the same list.

Automatic tool approvals never answer a subagent card: the approval journal
refuses any card marked `subagent_confirmation` or for the `Agent`/`Task`
tool. A team member's card reaches its lead like any other permission
request: the lead's inbox item names the member, each subagent's type, model
and task, and the lead answers it with `hgs team answer` (rating it as any
permission: what the task needs, or ask the owner), which takes only its
one-time choices. The owner's own session still asks the owner; with the
setting off nothing is asked, recorded or grouped. In Claude's print mode
nobody can answer, so the call goes ahead and the session note says the ask
was skipped. Agents without a pre-tool subagent approval are not blocked;
their status says the confirmation is not supported.

A permission answer may carry its reason (task #37, team ede724c4). The
top-level `text` of `hgs team answer` reaches the member as one note right
after the decision: "Lead: denied write_file: <reason>", or "allowed" for an
Allow, from the owner, the lead or the parent lead. On an approval card, which
takes its outcome only (DeepSeek's Allow once or Reject refuses any text with
the item), a reason written on the answer item becomes that note too and the
outcome goes out alone. A free-text question keeps the text as its answer. The
lead's inbox item shows the approval's answer command with the top-level
`text`; empty sends no note.

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
Neither is the calling agent's command context (`AGENT_COMMAND_CONTEXT` in
`src/state/backend/mod.rs`): Claude Code's and Codex's session, process and tool
call markers, DeepSeek and OpenCode native session IDs, and `NO_COLOR`. A tmux
server that a launch starts keeps none of them, nor the caller's session
variables, in the environment it gives every later pane, and an agent starts
without them even when a server already holds them. `hgs send` reads the agent's
styled prompt; under `NO_COLOR` Claude Code's placeholder looks like a draft and
every send is refused while it shows.

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
place. **Restore session** follows the computer's restore decision, as batch
Restore does; a computer that reports no actions keeps the earlier rule.

A Claude or Codex session that exited before its first message reported a
conversation ID but wrote no transcript. Its archive therefore does not offer
Restore, and gives the reason "This session ended before its first message"
(zerus-wyrqg). The archive summary checks this with one metadata read of the
saved transcript (Claude's transcript, Codex's rollout): it must be a
non-empty regular file. The restore's own check stays the authority.

The session list context menu offers **Clear session…** for a live session
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

The static hint source covers tracked Claude Code, Codex, Kimi Code and DeepSeek
main conversations. Its native delivery is verified on reviewed versions.
OpenCode 2.2.7 uses the native instruction entry described below; older hgs
peers inject nothing.

The hint points to `hgs guide` with fixed English text, independent of session
name, computer and hgs version. Claude Code and Codex use the existing
SessionStart hook context. Kimi uses one UserPromptSubmit JSON `message` per
conversation, verified on interactive Kimi 2.5.3 launches, not the separate
`--prompt` mode. DeepSeek hosts started by hgs register the same sentence for
hgs-owned main agents; native subagents and sessions opened in the host's own
UI receive no hint. An already-running host keeps its bridge until its next
start and is never hot-reloaded. One effective copy means the latest prompt,
not every hint retained in history: opting out does not remove old hint-bearing
system messages, and re-enabling can retain them beside a new one. No channel
changes user instruction flags, project launchers or saved native arguments,
and none is sent as a person's Activity message. New conversations and
previously unmarked conversations receive an attempt; repeated resumes suppress
another attempt when the provider retains the context. Clear and compaction
replace context for Claude Code and Codex and request a new copy. Kimi retains
the existing hook part on resume and compaction, so those paths do not add
another copy; clear creates a new conversation and emits once. A fork inherits
suppression only from the exact marked context it forks. Renaming does not
change the text or hint revision. Arbitrary commands and native subagents
receive no hint.

For OpenCode, new-session creation and a verified existing-session launch whose
single-message native history read is empty write
`PUT /api/experimental/session/{id}/instructions/entries/zerus.agent-hint` with
`{"value": HINT}`. The exact conversation and hint revision are marked before
the request; the mark is suppression metadata, not a delivery receipt. A failed
or uncertain request does not block creation or resume and is not replayed,
because another PUT could duplicate the retained entry. OpenCode retains the
entry as effective prompt context across resume and compaction, without a
durable `Instructions updated` history row on the pinned version. A clear/new
session gets its own entry. Inspection and background polling never attach it.
A resume with a marked entry does not write again. A reconciled pending
creation follows the same emptiness check. An existing conversation with any
native message receives no instruction-entry attachment; an unreadable or
malformed history answer is not proof of emptiness.

OpenCode persists a noninitial instruction delta as an `Instructions updated`
system message. That makes the channel a durable transcript update rather than
a history-free instruction entry. Previously unmarked conversations with turns
instead use a context-only V2 plugin on OpenCode 2.2.7 or newer. Zerus loads its
own plugin through a launch-only configuration overlay when starting a service;
it never writes a plugin into the person's global configuration or project.
Existing plugins and settings stay intact. A service already running is never
restarted or changed to install the plugin. If the exact plugin source is not
active, launch/resume reports that the optional hint is unavailable and
inspection exposes `agent_hint_status.state` and its reason.

Cold starts reclaim at most 16 verified Zerus plugin folders using a 100 ms
maintenance deadline. Each unique launch folder records the exact service PID
and birth after a matching launch environment is verified. Cleanup inspects
only that service and keeps a folder while it is alive or uncertain, plus the
latest service marker's folder. Unrelated protected processes and host churn
do not block reclamation. A durable intent is written before spawn; abandoned
prepared folders are unused, while unresolved starts and legacy unpinned folders
stay retained. Unknown files and links also stay retained. The 256-folder limit
remains an allocation backstop and never grants deletion authority or changes
the person's OpenCode config.
Forgetting a binding also retires its private hint route. Ambiguous routes stay
blocked until a complete check finds no remaining binding; the check examines
at most 64 entries and 1 MiB in 25 ms, retaining the route on any uncertainty.

Each primary model request gets a bounded, read-only lookup of one exact saved
run/conversation/workspace/service binding. Its decided policy, rather than the
shared service's environment, controls injection. A stale, malformed, absent or
busy binding supplies no hint. Native subagents are excluded. The plugin appends
one system part without editing user messages, receipts or instruction history;
it does not run for compaction, title or transient generation. The next primary
request receives the hint again after compaction. A Zerus-managed service restart
loads the overlay again and resume verifies readiness for that new service.
Opting out on resume suppresses this context channel. Existing revision-one
entry-backed conversations keep their entry and never enable the plugin channel.

Configure optional injection in the hgs config file or caller environment:

```sh
HGS_AGENT_HINT=0                         # disable new hint injection globally
HGS_AGENT_HINT_OFF="native-codex work"   # exclude exact account IDs
```

Injection defaults to enabled. The global value accepts `0` or `1`; the exclusion
list is whitespace-separated and uses `native-claude`, `native-codex`,
`native-kimi`, `native-dsh` or `native-opencode` for the provider's own login.
Environment values take precedence, including an explicitly empty exclusion
list. Invalid or unreadable hint policy disables only optional injection with a
bounded diagnostic; unrelated configuration validation is unchanged. `hgs
guide` and `hgs self` remain available when injection is disabled. Opt-out does
not remove context already retained by the provider.

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

### Changing a session's account

```sh
hgs switch-account claude/sample-project/review --account work --request-id 8b1f6c2e-3d4a-4c5b-9e8f-7a6b5c4d3e2f
```

Only your explicit choice changes an account; a session nobody changes keeps the
account it has. The target account must belong to the same agent and be signed in
on that computer, and the session must be idle: no turn, tool call, question,
held message, draft or build Zerus runs for it. A busy session, another agent's
account, a signed-out account or a missing conversation file is refused with its
reason and nothing changes.

Otherwise Zerus stops the idle agent gracefully as **pause** does, so the old
account's store is final, copies the conversation into the target account's own
native store (Claude's `projects/<project>/<conversation>.jsonl` and its folder,
Codex's session file; the copy holds every record the agent wrote while
stopping), and launches it resumed into the same conversation, folder and session
name with the new account.
The JSON receipt's status is `switched` once the resumed conversation is
confirmed. A launch that fails puts the old account back and relaunches there
(`rolled_back`); one that cannot be confirmed is reported `uncertain` and is
never retried on its own. The copy stays in the target account and nothing is
deleted from the old one. `inspect` and `ls` name the session's account, with its
label, in `account_id` and `account_label`.

A team member's account changes only through `hgs team swap-member`, on your
word; `switch-account` refuses a member's session and names the team.

#### Moving a session to another agent

```sh
hgs switch-account claude/sample-project/review --to native-dsh --preview
hgs switch-account claude/sample-project/review --to native-dsh --request-id 3c9d2a71-5b8e-4f60-a1d4-9e2b7c6f0a18
```

An account of another agent (Claude, Codex, Kimi, DeepSeek or OpenCode) moves the
session there; `--to` is `--account`, and `hgs session switch-account` is the
same command. One agent cannot resume another's native conversation, so Zerus
starts a new session of the target agent in the same folder (`<agent>/<project>/<tag>`,
the old tag or the first free `tag-N`) and gives it a handoff as its first prompt
(DeepSeek and OpenCode get it as their first message): what the session was
about, its open Zerus tasks, the last turns and its attachments as `hgs
attachment` references, bounded like a team brief. The full handoff is kept in
`session_moves/<request-id>.md` under the state folder. The other agent's tool
state, prompt cache and the permissions chosen for the session are not carried.

`--preview` changes nothing and answers `kind` (`account` for the same agent,
`move` for another), the new session's name, what carries over and does not,
and `refusal`, empty when the move can run. A move is refused before anything
changes when the target agent is not installed, a Claude, Codex or Kimi account
is not signed in, the session is a team member's, it works or builds, a
question or a held message waits, it has no confirmed conversation, or its
folder is missing.

The old session is then paused and stays readable; the new one starts with the
chosen account. The receipt's status is `moved` once the new session shows its
conversation, `uncertain` when it did not in time or the first message was not
delivered (never retried), and `failed` when the start failed; the old session
is then resumed (`relaunched`). A retry with the same request id returns the
kept receipt. Both rows of `ls` and `inspect` name the other session:
`moved_to` on the old one and `moved_from` on the new one.

To move a team off an account, `swap-member` takes `from_account`,
`to_account` and `provider` instead of a member: every member of that agent
whose local session really runs on `from_account`, the lead included, moves
with one switch each. `""`, `native` and `native-<agent>` all name the default
account. A member whose switch is confirmed keeps its session, conversation and
attempt; one whose switch rolls back or is uncertain has its idle old session
paused and saved as `<name>-<account>` and gets a fresh session on the new
account; a busy old session is never interrupted, and that member keeps the
reason in its error ("waiting for the old session to finish its turn"). `team
status` names each member's real account in `session_account`.

A new session of a team member is briefed (task 135). When `swap-member` gives a
seat a new session - another agent, model or account, an adopted session or a
fresh one, the lead included - Zerus builds a **handoff brief** from the team's
own record, never from the old session: the goal; the seat's task with its
criteria, files, needs and the previous attempt's partial report and open
questions; what is done and decided; what else is open and which findings are
open; and the seat's last pulses (its recent journal items). It starts the new
session's first message, ahead of the task hand-out (or the lead's brief for a
lead), is at most 6,000 UTF-16 units and is cut by section, never in the middle
of the seat's own task. The journal says `handoff_sent`. A refused send keeps
it pending for the next try; a switch that keeps the session and its
conversation (the bulk account switch, a model switch) sends none, and a seat
that is not new is never briefed again. A session whose context was compacted or
cleared (the SessionStart sources `compact` and `clear`) gets the same brief in
recall form as start context, at most 3,000 units and only what the iron rules
and the hint leave of the 8,000-unit start context. A task may carry `control`:
up to 12 questions, each with 1 to 6 strings an answer holds (`{"q":"which file
holds the endpoint?","keys":["src/api/items.rs"]}`); a brief that holds none of
a question's strings is a `handoff_gap` in the journal, naming the questions, so
a brief nobody could use does not go out unnoticed. The brief is kept with the
team, so it survives a restart of the home computer: the latest of a task stays
while the team record does, the older versions of a task go when it closes, at
most 20 versions of a task and 200 briefs are kept. `hgs team handoff [TASK]
[--team ID] [--version N]` reads them: the latest of every task, or one task's
text with its versions and the questions it missed.

On the desktop, the team workspace's member menu has **Move to another
account…**: choose the agent, the account to leave and the one to move to; the
dialog lists the members it takes, asks once, and shows each one's progress
(moving, then resumed in the same conversation, a fresh session that launches,
waiting, uncertain or the refusal). Each member card shows the account its
session runs on. A member that waits for its old session offers **Move again…**;
a member folder left on another branch names both branches and offers **Check
out <member branch>…**, for a folder on this computer only: after one
confirmation it runs `git switch` there, never with `--force` or a reset (a
folder with uncommitted changes is left as it is and the card says so), then
relaunches the member.

On the desktop, the composer shows the session's account by its label beside
the model, in Activity, the team conversations and the designer. Its menu lists
the same agent's signed-in accounts on that computer, each with its usage. A
choice asks you to confirm, then the chip says **Switching to …** until the
receipt or the refusal, and the notice names how it ended. While the agent works,
is stopped or its computer is offline, the chip is disabled with the reason; a
team member's menu lists the accounts but changes none.

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

Sessions of the Claude desktop app (its Code tab) continue the same way, from
New session's **···** menu > **Continue a Claude app session…** or the CLI:

```sh
hgs [@host] claude-app ls                       # the app's newest 200 sessions, JSON
hgs [@host] claude-app resume local_ID -n NAME -d
```

`ls` reads the app's session files (`claude-code-sessions` in its data folder:
`%APPDATA%\Claude` on Windows, `~/Library/Application Support/Claude` on macOS,
`~/.config/Claude` on Linux) for the conversation, folder, title and model, and
whether Claude Code keeps that conversation's history. `resume` starts it as a new
session with `--account native-claude` and `claude --resume ID` in its folder; a
conversation another Zerus session already tracks is refused. Zerus never writes
the app's files. A session the app may still run is refused unless `--force`, and
the GUI asks first: two processes writing one conversation break its history.
"Running" means a Claude process continues the conversation (`--resume ID`), or
its files changed in the last two minutes; the app does not report it otherwise,
so stop the session in the app first.

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
settings, pause, resume, start fresh, rename and fork. It does so when the
listed row for the action's exact run carries a session ID and that computer's
`hgs --help` lists `session-id-selector-v1`; the answer is kept for ten minutes.
Reads, polls, history, status, watch, terminal input and read marks keep the
name until a label index exists. Confirmed delayed and worktree-cleanup end
actions keep their scoped envelope, which already names the exact run and
conversation. Archive, Terminate, Kill and Forget run only that way: without
their undo window's capture (or a confirmed worktree cleanup) the desktop runs
none of them, and it never sends a name-only `hgs archive` or `hgs kill` or a
run-only `hgs terminate` (zerus-sa9qn.25). Clear archive's Forget stays bound to
each entry's archive ID. A refused ID shows as a changed
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
conversations remain separate messages. For Kimi releases that omit the Stop
hook, inspection and listing can settle the exact current main turn from a
bounded, stable main-wire `turn.ended` or `prompt.completed` event. A matching
TurnStarted identity is required; a newer prompt, incomplete wire, outstanding
tool or changed run/conversation prevents settlement. Reply text never proves
completion. Stop and wire evidence share one durable completion and reply ID;
background jobs and subagents remain active until their own work finishes.
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

Ending a parent never ends its children by itself (zerus-sa9qn.17).
`hgs [@host] terminate SESSION --with-children` and
`hgs [@host] archive SESSION --with-children` are the explicit choice to end
them too. The parent runs first with its own command and rules; only when it
succeeds does each live (non-archived) descendant on that computer get the same
command, one at a time, every child before any grandchild and at most eight
levels deep. A link names a parent by its session ID; only a link without a
session ID names a parent listed without one, by conversation. This is stricter
than the listing's nesting, which also falls back to the conversation for a
link with a session ID. A link another computer forwarded never matches. Each
child call is bound to the session ID, run and conversation the listing showed,
so a session that took the label meanwhile, or the child restarted into another
run or given another conversation, is refused, and Terminate also passes
`--expected-run-id`. Each child keeps its own rules: Archive takes only stopped
or paused sessions, Terminate stops a running child and also moves a stopped or
paused child to Archive (as `hgs terminate` does for any such session; the
desktop's Terminate leaves those children as they are), a DeepSeek or OpenCode
child refuses an action it does not support, and no working folder removal is
implied; the configured worktree maintenance treats an archived child as any
archived session. A child that is not ended is reported on
standard error and never retried, and the command exits 1 with how many were
not ended. With `--dry-run` it validates the parent and lists the children it
would end. A parent refused or unknown ends no child. An older peer without the
flag refuses it as a usage error before acting.

On the desktop, the Terminate confirmation of a session with live child
sessions on its computer, and a Move to archive confirmation shown only for
such a session, list them and offer **Also end N child sessions** (Archive:
**Also archive N child sessions**), off by default. Children the action does
not apply to now (Terminate takes running sessions, Archive stopped or paused
ones; an offline computer or a write in flight leaves one out too) are listed
as staying. The count and the list cover every level, not only the direct
children, and the confirmation says so. As on the command line, a session whose
link its own listing marks `missing` (a link another computer forwarded is
always `missing`) is never offered, although the list may still nest it. One
deliberate difference remains: when a child's link falls back to its parent's
conversation ID because the parent was listed without a session ID, the
desktop still offers that child, by name, but
`hgs terminate --with-children` does not end it. At most 50 are named; the
rest are counted. When chosen, the children run after
the parent's own outcome is done as one preconfirmed selection batch: each
through its own guarded action, rechecked as the same instance, one at a time
and never retried, without the parent's Remove working folder choice. Like
every end action started in the desktop, each child's call passes the undo
window and runs the scoped `hgs session-action` envelope with the run and
conversation the confirmation listed, so a child restarted or given another
conversation meanwhile is refused and kept. A Terminate child without an HGS archive
is killed, as its own Terminate and a selection's Terminate kill it: it leaves
the list and any native conversation history is kept. The confirmation marks
each such child and counts them. Terminate lists a stopped or paused child as
staying and notes that `hgs terminate --with-children` would move it to
Archive. A parent that failed or whose outcome is unknown keeps every child. One notice counts the children ended,
failed or changed, and unknown.

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
An ended parent with live child sessions stays in the live list as their
container (zerus-sa9qn.17): a dimmed compact row with the parent's tile, title,
**Ended** or **Archived** state and its children's line, the children nested
under it. A stopped or paused parent that the filters hide is shown this way
from its own row; a terminated or archived parent, or one no longer listed on
the child's own computer, from its children's recorded link. The container is
presentational: it counts as no session, takes no keyboard step, drag target,
selection range or attention, and its menu offers only **Open**, **Resume** or
**Restore** and **Move to archive** of its own listed row, or **Open in
Archive** for a parent known only from the link. It leaves the list by itself
once no live (non-archived) child remains, and it is shown only for children
the filters show. A parent that still runs, is listed in another project, or
is missing on another computer keeps its children at top level with **from
PARENT**, as before. The containers come from the same one-pass resolution and
hash maps as the nesting; there is no extra read per row.
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
authentication failure or **Sign in required**). DeepSeek and OpenCode rows
carry it as well; their tool and permission approvals, listed as pending
questions, count as approvals. A supervised child is quiet
only when its host reports `human_request: false` and the row itself shows no
such approval or sign-in; a host that does not report it keeps every request
visible. A quiet child behaves like a muted session: no system notification, no rail, tray, menu bar or project header
count, and gray badges and edge, while its status, Activity, unread reply,
questions and approvals stay as they are. Nothing is marked read. Unlike a
muted session, it leaves the **Needs attention** filter, and **Active** unless
it is working, in Background, Scheduled or has a draft: its unread reply, retry
or provider limit waits for the parent (zerus-sa9qn.18), on desktop, Android
and iOS alike (zerus-sa9qn.19). Review later, the person's own mark, still
counts and lists it. A person-only request notifies as for any session.
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
refused identity never routes again. The ledger keeps at most 65,536 rows with
rolling retention (zerus-sa9qn.36): when it is full, one bounded pass reads at
most 1,024 settled (`submitted` or `refused`) rows older than a day, oldest
first from a rolling cursor, and deletes at most 256 whose question can never
be read again, because this computer's complete catalog no longer lists the
child at that row's run (a run ID is never reused, and a question routes only
from its listed run). Reserved and unknown rows, a row of a run still listed,
a row from an older ledger without a run while its session is listed, and
every row while the catalog is incomplete are kept, so nothing is replayed or
routed twice. A question that finds the ledger still full after that pass is
not routed then; Activity shows retained routes only. The text is a fixed English template of
at most 2 KiB: the header `[Zerus child question]`, the child's name and
reference (the name single-quoted; a child whose name holds anything but
ASCII letters, digits, `.`, `_` and `-` in three segments is never routed and
is recorded `refused` as `question_route_invalid`, as is a question whose run,
conversation, question ID or hash holds anything but ASCII letters, digits,
`:`, `_`, `.` and `-`), the question kind and option count, at most 300 characters of the
first question as one quoted line labelled as the child's data, `hgs inspect
'<reference>'` for the full question, the exact `hgs answer CHILD --json`
fields (run, conversation, question ID and hash) and "If only the person can
decide, say so in your reply." It carries no ticket, wait or other authority.
v1 routes on Linux only; macOS follows the deferred-send actor's macOS child
seam and Windows keeps today's behaviour. The service advertises `Desktop
question route: route-questions-v1` in `hgs --help`; an older service does not
route.

A submitted route shows one line in the child's and in the parent's desktop
Activity (zerus-sa9qn.33): **Question sent to the parent session** with the
parent's name, or **Question from a child session** with the child's name,
then the route's own preview of at most 300 characters as plain text and the
child's reference with Copy. The parent's own message of exactly the routed
text folds into its line, behind **Show the received message**; any other
line is placed by its time. Refused, reserved and unknown routes show nothing
in this version. The lines come from the background service's read-only
`question_route_activity` request (`request_id` and the session's durable
`session_id`, exactly): at most the 20 newest submitted routes where the
session is the child or the parent, read by two partial indexes of the route
ledger (Store schema 38) and each route's own saved original. The read sends,
settles and grants nothing. The desktop reads it for a local session in
background service mode: once when the session is shown, then at most every
3 s while its Activity changes, and a shown child every 15 s.

The person has two switches, both on by default (zerus-sa9qn.32):

- **Settings > Sessions > Send child questions to the parent session** is kept
  by the background service (Store schema 37) and changed only through the
  person's native frontend request (`question_routes`), never by a command
  an agent can run. It is shown only where the service routes (Linux).
- **Send questions to PARENT** in a Claude child's Session actions is kept on
  the child's computer beside its notification preference
  (`question-routing.json`, keyed by the child's durable session ID, so a
  rename or a new conversation keeps it) and shared with every desktop and
  phone. `hgs [@host] question-routing <session> --json` changes it by
  compare-and-swap (stdin `expected_run_id`, `expected_conversation_id`,
  `expected_revision`, `send_to_parent`; marker
  `session-question-routing-v1`) and refuses a change from an agent's own
  process tree (`agent_refused`), on the switch's own computer and, for `hgs
  @host question-routing`, in the local CLI before forwarding (also in the
  guarded `--expect-machine ID [--client C] [__state]` forms and `__state
  question-routing`, zerus-sa9qn.51). That refusal
  stops accidental agent use only; it is not a security boundary, because a
  same-user agent can clear its session variables, leave its process tree or
  write `question-routing.json` directly. The person-only control is the
  global switch above (zerus-sa9qn.49). Listing rows and `hgs inspect` carry
  `question_routing` (`send_to_parent`, `revision`), null when the setting is
  unreadable or the session has no durable ID. Android and iOS show the same
  entry in a live Claude child's Session actions and change it through the
  mobile relay's Rust-only `session_question_routing` operation and the Rust
  connector (zerus-sa9qn.33); the legacy Python connector refuses it. Phones
  see the switch only once both the relay and the computer advertise it: the
  relay lists the operation, and the connector offers it only when the
  computer's `hgs --help` shows the `session-question-routing-v1` marker.
  `mobile/android/app/src/test/resources/question-routing-contract.json` is
  the shared contract of the desktop, the connector, the relay, Android and
  iOS.

- **The person's latch** (zerus-sa9qn.50): the background service on the
  child's computer keeps a latch per durable session ID (Store schema 39).
  While it is set, the child's questions never route, whatever its host
  file says. The latch is the authority. After each change the service
  writes the child's host file to follow it (`send_to_parent` the latch's
  opposite), with a display-only mirror `person_off`. Listing rows,
  `hgs inspect` and switch replies then carry
  `question_routing` {`send_to_parent`, `revision`, `person_off`} (help
  marker `session-question-routing-v2`). Older clients that know only the
  two-key shape show the switch as unknown and fail closed. `hgs
  question-routing` changes only `send_to_parent` and keeps the mirror.
  On a child of this computer the desktop's **Send questions to PARENT**
  goes through the latch: the person's native frontend request
  (`question_route_child`, compare-and-swap at the child's exact run,
  conversation and latch revision; closed refusals `conflict`,
  `stale_identity`, `not_found`, `capacity`). The entry shows "You turned
  this child's questions off" while latched. A remote child keeps its host
  switch; while its mirror says a person latched it, the desktop shows it
  off and offers no "on", which only its own computer or a phone can
  give. The request lane an agent can reach only reads the latch
  (`question_route_child_status`, `question_route_child_result`).
  Boundary: the latch is person-only against an agent using commands (the
  CLI, `hgs desktop request`, the connector), not against the same user
  editing the service Store or the connector's files directly, the same
  as the global switch.
  A phone on an encrypted workspace sets the latch too, where its relay
  and the computer both offer `session_question_route_person` (help marker
  `question-route-person-v1`). The phone signs a statement with its
  workspace Ed25519 key: domain `zerus-question-route-person-v1`, with
  the workspace, phone, gateway, child session, run, conversation, value
  and request ID, valid at most 5 minutes. The connector forwards it as one
  `hgs desktop request` (`question_route_person`). The service runs this
  computer's connector itself, pinned: the desktop's absolute selection or
  the installed sibling of `hgs` (never a PATH lookup). Its directory must
  be owned by this user or root and not writable by group or others. The
  binary, opened relative to that directory without following a final
  symlink, must be a regular file under the same rule: a packaged one, such
  as the AUR's root-owned `/usr/bin/zerus-mobile-connector`, qualifies
  (zerus-sa9qn.56).
  It must answer `--version` as `zerus-mobile-connector X.Y.Z`, and the path
  must still name it after the run. On Linux the service executes the
  descriptor it checked (`execveat` with `AT_EMPTY_PATH`), so a file put at
  the path after the check never runs (zerus-sa9qn.54). macOS has no
  descriptor exec and runs the path. There, only this same user or root
  could swap the file between the check and the run and back before the
  recheck; that is outside this threat model. The run is bounded to 0.4 s
  and 1.2 s and 64 KiB of output, with a cleared environment:
  `zerus-mobile-connector --workspace verify-question-route-person`. The
  verifier checks that a current phone of this workspace signed exactly
  that statement for this gateway, and must reply exactly `{ok, verifier:
  "question-route-person-v1", verified}`. Any failure, timeout or other
  output is the closed refusal `not_verified`. The service then sets the
  latch once per request ID, and the host file follows. Decisions apply in
  the order the person made them (zerus-sa9qn.53): the latch keeps the
  time of its last decision (a desktop decision records the computer's
  time). A statement not issued after it, such as one delivered late, is
  the closed refusal `stale_decision` and changes nothing. A decision that
  changes nothing still records its time. The phone then refreshes and
  asks again. Peers and Windows
  gateways do not offer it, and a legacy connection keeps the host switch.
  The shared vector is
  `services/mobile/crypto/tests/vectors/question-route-person-v1.json`.

The router reads nothing while the global switch is off. Every reservation
reads both switches and the latch again in its own transaction, so a switch turned off
before it stops the send and writes nothing; off never cancels or replays a
question already reserved, sent or unknown. Turning routing on again reads the
catalog from a new baseline: a question still pending and supervised then is
routed once, also when it was first read, or its reservation found a switch or
the latch off, while routing was off (zerus-sa9qn.45). One asked and answered
while routing was off is never sent, and a question already in the route
ledger never routes again.

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
hgs terminate codex/my-project/lead --with-children # then its live child sessions
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

### First start without Qt data

Zerus reads no data of the retired Qt client. An installation that still has
Qt's settings, drafts, run groups, organization, private-store notices, WSL
presets or task forms starts with Zerus's own defaults: nothing is imported or
seeded from them, and Qt's files stay on disk exactly as they were, never read,
changed or removed. Zerus's own stores (`settings.json`, the drafts store,
`organization-layout.json` and the rest) load as before. A `settings.json` or
layout written while Qt data was still imported keeps the records of that import
as extension fields, so nothing in it is lost.

Two Qt-era formats remain because live code needs them:
- The downloads folder (`Zerus Files`) is locked with `.zerus-files.lock` in Qt's
  QLockFile format (`client/core/src/qt_lock.rs`). The workspace and the desktop
  service's cache maintenance both change the folder and serialize on that lock.
- The desktop service still reads an earlier switch's handoff marker
  (`<state>/desktop/handoff-v1.json`). While it is not revoked, the desktop epoch
  owns presence and the legacy CLI presence writer stays fenced. Ownership,
  startup, Windows registration and `hgs desktop revoke` read it the same way.
  Zerus no longer writes a new marker; `hgs desktop revoke` only marks an
  existing one revoked.

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
Needs attention filter, and its tab count includes them.

A team member on this computer is muted on every device by default. While a
session on this computer is a non-lead member of an active team (planning,
running or paused) and the owner has not recorded a choice, its host reports
`notification_preferences.muted` true at revision 0, so every desktop and the
phone that follow this host see one preference; the team's lead is never muted
this way. The default is computed when the host is read and writes nothing.
Unmuting records a choice at revision 1 or more, which wins on every device. A
desktop whose host predates the shared default still mutes the member locally
and says "Muted on this computer"; an updated host reports the shared mute, and
the desktop says "Muted on all devices". A member on another computer is not
muted by this default there: its own hgs has no team record, so its
notifications stay on and its desktop shows no team mute unless the owner
records a choice on that host. "Muted on all devices" is true for the members on
the computer that runs the team; the phone and the other desktops follow the
preference that host reports.

The desktop service accepts notification clicks only for a live, unblocked
attention request on Linux, macOS and Windows. After a read, mute or resolution
retires that request, a later click is ignored even if the operating system has
not yet removed the banner.

Notifications open Activity for the exact requesting run and conversation,
including remote or renamed sessions. Polling and restarts do not repeat the same
request; resolved notifications are removed. Main-agent reply banners wait until
the session is idle and has no reported active children. While native agent work,
jobs the session waits for or active subagents continue, intermediate replies remain
unread in Activity but show no new-reply badge or reply-only attention count on
desktop or Android. The pending reply becomes visible when work finishes; this
never marks it as read. Services, such as a dev server left running, never defer it.
Explicit questions, approvals, errors and review reminders
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
requests. Each Git read of the evidence has its own bound: 2 seconds for a local
read, 3 seconds for the remote check, so a busy computer's slow read never
expires the reads after it; one status takes at most 19 seconds and the desktop
waits 25 (30 over SSH). A read past its bound reports `timeout` and the next
refresh tries again. Local evidence refreshes about every 15 seconds and expires
after 30; read-only remote verification is cached for at most 60 seconds. A changed tracking
OID or remote endpoint invalidates that verification. Nothing fetches, commits or
pushes automatically. A muted cloud question mark means verification is unavailable,
expired, detached or lacks a remote upstream. Outgoing/incoming counts become muted
when only the local tracking ref is known; a changed remote asks you to fetch.
Ordinary non-Git folders show no badge. Offline and changed checkout evidence
cannot show a green check.

### Session states

A session is in one of seven states; **Offline** (the computer does not answer)
stays as before. A reply ending is not a long task being complete: the states
say what the session does now and what it waits for.

| State | `session_state` | Meaning |
|---|---|---|
| Working | `working` | The turn runs: the model writes code or runs tools. |
| Building | `build` | A build or test Zerus runs for the session (`hgs run`, `hgs team run`), also while the agent is idle. |
| Waiting for you | `user` | An approval, an input, an error, a reply to read, or a lead's "Needs you". |
| Waiting for dependencies | `deps` | Always with its reason (below). |
| Cooling | `cooling` | Done and waiting for a new task; the prompt cache is still warm. |
| Cold | `cold` | Done and idle; the prompt cache expired, a new session is cheaper. |
| Stopped | `stopped` | Stopped by you, or saved and not running. |

Clients draw the states in these tones (dark / light): Building `#4cc98a` /
`#2a9d63` (Working's green; the desktop tells them apart by Building's frame
and terminal glyph), Waiting for dependencies `#6fa8ff` / `#2f66c9`, Cooling
`#56c8e0` / `#137f95`, Cold `#8fa6c4` / `#55708f`, Stopped `#8b939e` /
`#6b7785`. On the desktop a badge in one of these states has a frame and a
caption in its tone (`SessionStatusBadge.h` is the reference); Android fills a
Building badge with 13 % of the tone over the card.

The host computes them in this order: Working, unless the turn waits for
subagents in flight (`Waiting for 2 subagents: explorer, reviewer`) or for a
tool call running 30 seconds or more (`Waiting for cargo test --workspace ·
long operation, 6m so far`); Waiting for you for an approval, an input or a
provider error; Building while a build Zerus runs for the session still runs
(`Running cargo test · 6m so far`); Waiting for you for an unread reply (a muted session, such as a team member,
answers its lead, so its reply does not count); Stopped; the account's usage
limit (`Waiting for the usage limit to reset · 01:30`); the team's reason; an
`hgs wait` the session declared; a job under a declared team budget; then
Cooling while the time since the last native event is under the cache TTL,
otherwise Cold. A team reason that starts with "Waiting for you" makes a lead's
session Waiting for you and any other session Waiting for dependencies. Team
reasons come from the team store:

| Situation | `state_reason.text` |
|---|---|
| The phase has not started | `Waiting for phase 2 · Checkout to start (#5)` |
| A dependency is not accepted | `Waiting for #2 Saved cards API (Backend)` |
| The task is ready or held | `Waiting for Lead to hand out #4` |
| The task is in a draft phase | `Waiting for Lead: phase 2 is not accepted yet (#5)` |
| The task was submitted | `Waiting for Lead to review #4` |
| A lead's members work | `Waiting for reports: Backend #2, Frontend #3` |
| A draft phase waits | `Waiting for you to approve phase 2 · Next` |
| A phase waits for the owner's acceptance (U63) | `Waiting for you to accept phase 2 · Checkout` |
| The task's phase is paused (U63) | `Waiting for you to remove the pause of phase 3 (#6)` (or `the lead`) |
| A phase before it waits for the acceptance (U63) | `Waiting for the owner to accept phase 2 (#6)` |
| Every task is decided | `Waiting for you: all tasks are decided` |
| The team is paused, the plan is drafted | `Waiting for you to resume the team`, `Waiting for you to approve the plan` |

`state_reason` is null for Working, Cooling, Cold and Stopped, and otherwise
carries `kind` (`build`, `subagents`, `long_operation`, `usage_limit`,
`coordination`, `job`, `approval`, `input`, `error`, `reply` or a `team_*` kind)
with its source: the build's `label`, `command`, `since` and `source`
(`hgs_run` or `team_run`), subagent `names`, the `command` and its start
`since`, `resets_at`, or the team and task numbers. `build` is the running
build itself, or null. `last_active` is the last native event, a cache
keepalive included. `cache_ttl` is `{seconds, source, kept_warm}`: the TTL the
account's native usage last reported (`native_usage`), else Claude's settings
`env` (`ENABLE_PROMPT_CACHING_1H` gives one hour, `FORCE_PROMPT_CACHING_5M` five
minutes; `settings`), else five minutes (`default`). Keep cache warm extends it:
each keepalive turn is a native event, and `kept_warm` says the last turn only
warmed the cache. These fields are additive; older fields keep their meaning.

A team's builds may share one build cache (U62): the project file
`.zerus/tests.toml` turns it on with `[build_cache]`, `enabled = true`
(off when absent), `size` (each tool's bound, `20G` by default, K/M/G/T),
`compilers` (`["rust", "cxx"]` by default) and `dir` (an absolute folder;
by default `build-cache` in Zerus state, so every worktree and team on the
computer shares it). The owner's `build_cache` object in the team templates
file (Settings > Teams) overrides any of these keys. With it on, the
`cargo-build`, `cargo-test` and `cargo-clippy` templates run rustc through
`sccache` (`RUSTC_WRAPPER`, `SCCACHE_DIR`, `SCCACHE_CACHE_SIZE`, and a server
of the run's own on a free port that ends with the run), and `cmake-configure`
sets `ccache` as the C and C++ compiler launcher, `cmake-configure` and
`cmake-build` with `CCACHE_DIR`, `CCACHE_MAXSIZE` and `CCACHE_BASEDIR` (the
member's folder, so worktrees hit each other's entries). A tool not found on `PATH` (or in cargo's bin folder)
is named at the start of the run's output and the run builds without it.

The owner's own shared sccache server is separate (task 261): when cargo builds
through sccache by `RUSTC_WRAPPER` or `build.rustc-wrapper` in cargo's
`config.toml`, every build on the computer shares one server on
`SCCACHE_SERVER_PORT` (4226 by default), and the first build after none runs
starts it. Zerus never lets a sandboxed member's run start it: a server
started in a Low-integrity or Codex-sandbox token inherits that token, and
every later build of any folder fails with "error writing dependencies to
...\deps\X.d: Access is denied (os error 5)"; a build script such a server
compiled runs at Low too ("failed to write asset"). A sandboxed member's run
therefore always builds without the wrapper (`RUSTC_WRAPPER` empty, task 320):
its compiler writes its outputs at the run's own level, so its next build
overwrites them (outputs the Medium server wrote would carry a Medium label it
may not overwrite), and its sccache client never starts the server after the
server's idle time ended it. The team worker, with its normal token, still
keeps the server at its own level before such a run: it restarts one that runs
in a restricted token and starts one if none runs (`sccache --start-server`).
Before a run at the worker's own level (a non-sandboxed member's `hgs run`, a
cargo template) a server found in a restricted token is restarted the same way
and the run's output says so. A `hgs run` or a cargo template whose failure
is that signature (os error 5 writing a `.d`, `.rlib` or `.rmeta`) makes the
worker restart the server (`--stop-server`, `--start-server`) and run the
build once more; the run's output says so and the lead's inbox gets one line.
`hgs team builds` carries the server's health as `cache_server`:
`configured`, `program`, `port`, `running`, the `pid` and `integrity` of the
process listening on the port, and `restricted` when that is Low.

A build or test template (`cargo-build`, `cargo-test`, `cargo-clippy`,
`cmake-configure`, `cmake-build`, `ctest`, `msbuild-build`) that fails with MSVC's C1083 "Cannot open
compiler generated file ... Permission denied" or D8050 "failed to get command
line into debug records" runs once more. These are the failures real-time
scanning (Microsoft Defender) gives a compiler in a folder it scans. The run's
output names the folder and asks for its build outputs (`.ci-build`, `target`)
to be excluded from scanning. No other failure is retried, and a commit or
merge never is.

Builds and tests of any CMake or MSBuild project (task 85, the provider model
of `docs/test-providers.md`) have their own templates beside the Qt ones:

| Template | Arguments | Runs |
|---|---|---|
| `cmake-configure` | `source` (default `.`), `build_dir` (default `build`), `generator`, `config`, `preset` | `cmake -S source -B build_dir [-G generator] [-DCMAKE_BUILD_TYPE=config] [--preset preset]`, plus the build cache's launcher |
| `cmake-build` | `build_dir`, `config`, `targets` | `cmake --build build_dir [--config config] [--target targets...]` |
| `ctest` | `build_dir`, `config`, `tests` or `regex` | `ctest --test-dir build_dir --output-on-failure --output-junit <report> [-C config] [-R ^(tests)$ or regex]` |
| `msbuild-build` | `project` (a `.sln` or project file), `config`, `platform`, `targets` | Visual Studio's MSBuild (vswhere), `/m /nologo /v:minimal [/p:Configuration] [/p:Platform] [/t:targets]` |
| `vstest` | `tests` (assemblies), `filter` | `vstest.console tests /Logger:trx [/TestCaseFilter:filter]` |
| `web-test` | `project` (default the member's folder), `paths` | the project's own `node_modules/vitest` run by `node` (or `npx --no-install vitest`), `run --reporter=default --reporter=junit --outputFile.junit=<report> paths...`; `npm ci` (or `npm install` without a lock file) first when `node_modules` are missing |

A web project's vitest run (task 239) happens through the worker like the
other templates: it reads its JUnit report per test file, keeps npm's cache in
the member's `.zerus-team/npm`, and installs its dependencies only when the
project has no `node_modules`. `hgs test affected` prints it for an `npm`
provider, so a sandboxed member never runs `npx` itself.

Every value is typed: a `source` or `build_dir` is a path inside the member's
folder (it may not exist yet), a target or test name a plain word, a regex,
generator, platform or filter one line of text. On Windows these templates load
the Visual Studio environment, and the build cache and the scanning retry apply
to them.

Task #58 (owner, 2026-11-10: Qt goes from Zerus completely): the Qt desktop's
own templates `qt-configure`, `qt-build` and `qt-test` are gone with it. A
request, a team's stored run or a project's `.zerus/tests.toml` that still
names one is refused with its replacement ("The qt-build template was removed
with the Qt desktop: use cmake-build"): `cmake-configure`, `cmake-build` and
`ctest`, which take the build folder, targets and tests as arguments.

`ctest` and `vstest` read their report (CTest's JUnit XML, VSTest's TRX, under
`.zerus-team/results` in the member's folder, removed before each run) into one
result per test: name, suite, `passed`/`failed`/`skipped`/`error`, duration and
the first message, at most 5000 per run with failures first. The run's answer
carries them as `report` (`results`, `passed`, `failed`, `skipped`, `errors`,
`note`), and the failing names fill `failures` when the output named none. The
exit code stays the verdict; a missing report is only a `note`.

A project declares its own templates in `.zerus/tests.toml`, typed like the
built-ins:

```toml
[[template]]
id = "make-target"
program = "make"
args = ["-C", "src", "{targets}", "JOBS={jobs}"]
timeout_secs = 900
results = { format = "junit", path = "out/report.xml" }   # optional

[[template.params]]
name = "targets"
kind = "identifiers"        # identifier, identifiers, text, path or paths
required = true

[[template.params]]
name = "jobs"
kind = "identifier"
```

The program is fixed; every placeholder is declared, a list is a whole
argument, an absent optional value drops its argument, and an id never shadows
a built-in. A project template runs outside the member's sandbox, so it runs
only once the owner accepted its exact content: `hgs team templates --json`
with `{"action":"project","folder":...}` lists a folder's templates with their
content `hash` and `accepted`, and `{"action":"accept","hashes":[...],"by":"human"}`
(the owner only) accepts them. A changed template waits again.

Start builds and long tests through Zerus so they show as work:
`hgs team run` for a team member's templates (`hgs guide team-run`), and
`hgs run --json [--wait]` with `{request_id, label, command, cwd?,
timeout_minutes?}` for any session (`hgs guide run`). `hgs run` names the
calling session by its process ancestry, or a DeepSeek session by the
conversation its harness exports as `DSH_SESSION_ID`, runs the command in
`sh -c` (`cmd /C` on Windows) in a detached runner with its whole process tree stopped after the
timeout (30 minutes by default, at most 240), keeps the last 4000 characters of
output and records the run under the session; the same `request_id` never runs
twice, and asking again reads its state. `hgs run --list --json` lists the
session's newest runs.

Task 290 (owner, 2026-11-08): a run's command gets the environment the team's
build templates have. A template starts from the team worker's environment (the
owner's shell); `hgs run` starts its runner from the calling agent's own
process, whose environment may lack the developer variables, and then MSVC's
cl.exe dies with `D8050 ... failed to get command line into debug records`
(stacker, ring) although the same cargo builds from the owner's shell. The
claim now carries TEMP and TMP, SystemRoot, windir, SystemDrive, ComSpec and
PATHEXT, PATH with the system folders, and the Visual Studio developer
variables (`INCLUDE`, `LIB`, `LIBPATH`, the tools and Windows SDK folders and
their PATH) read once from `vcvarsall.bat x64` through `vswhere` when the
caller's shell does not already carry them; a sandboxed member's run still
points TEMP and TMP into its own folder and keeps the build cache server's
variables. The command inherits the rest of the environment as before. When a
run fails and its output is the compiler-startup signature (D8050, the
debug-record message, or `cannot execute ... c1.dll`), the result says so in
`error` and carries `"compiler_cannot_start": true` instead of leaving only a
tail to read.

Task #18 (owner, 2026-11-10): Windows starts an executable whose file carries
the Low mandatory label at Low integrity, whatever its parent's level, and a
Low process may not create anything in an ordinary (Medium) folder. A DeepSeek
session's sandbox labels its working folder Low, inheritable by everything
below it, so every test program cargo builds there fails at its first
temporary folder with `Access is denied. (os error 5)` when the run's TEMP is
an ordinary folder. (A member template's isolated TEMP sits inside the
member's own folder and is Low as well, so a DeepSeek member's own runs pass.)
The merge queue checks a suite's folder, its `target` and a named target
folder before it runs: a Low build folder with an ordinary TEMP is one result
with status `failed`, flag `environment` and an excerpt starting
`Environment failure:`, which names the folder that carries the label and the
owner's step (`icacls "<folder>" /setintegritylevel "(OI)(CI)M" /T /C /Q`, no
administrator needed, and DeepSeek sessions in their own worktree); nothing
runs, nothing is retried, and the landing stays queued without reopening the
task. A failed `hgs run`, member template or queued suite whose output was
refused this way, from programs built in such a folder (its folder, a
`CARGO_TARGET_DIR` or `--target-dir` it names, or the programs cargo says it
ran), carries the same text in `error` (`hgs run` adds `"environment": true`).
Zerus only reads labels; it never changes them.

Task #35 (2026-11-10): so a DeepSeek (dsh) team member works only in its own
Zerus-made folder, its clone or worktree. A DeepSeek member with the shared
folder (the team folder, the integration folder, another member's folder) and a
DeepSeek lead, who works in the team folder, are refused: by the plan preflight
(so `approve` and `add-member` name it) and again at the start, before any
folder is created, granted or launched. The refusal names one action:
`{"team":"ID","members_update":[{"id":"qa","folder":"clone"}],"request_id":"UUID"} |
hgs team settings --json`, or another agent. A DeepSeek member given no folder
takes its own clone under every folder policy, unless the project leaves no own
folder (the shared policy, or worktrees and clones forbidden). A member that
already has a session (adopted) is not launched and not checked. The team
folder, the integration folder and shared members' folders that already carry
the Low label are named, with the owner's `icacls` step, as `low_folder`
warnings in the preflight at team start and in `hgs team status --json` as
`low_folders` (`folder`, `labeled`, `what`, `text`, `owner_step`; labels are
read at most once a minute). A member's own clone or worktree is Low by design
and is not named. Zerus sets a label only on its own team store folder in the
hgs state directory, and grants full control only on a DeepSeek member's own
new folder (`worktrees create --sandbox`) and on the cwd of a DeepSeek session
it starts (U34).

On Windows a Git Bash wrapper (`bash -c`, `env`, `nohup`, `timeout`, `xargs`,
`sh -c`, `npx -c`) ends a Windows process between the session and hgs, so the
process ancestry stops at a process that no longer exists. hgs then asks the
session host which session's job holds it (`caller_session`): the host answers
for the connection's own client, by the process ID the kernel gives it, and
the session, pane and run marker must be exactly those of the record that
`HGS_SESSION` and `HGS_RUN_ID` name. A leaked or stale session environment
therefore never names the caller, and a process outside every session is still
refused. A host from before this question answers `unsupported`, and the caller
stays unproven as before.

A job the session started otherwise makes it wait only while it waits for it. A
long-lived process it does not wait for, such as a dev server, a watcher or a
tunnel, a job past its declared budget, and any other job that still runs after
the turn ended with nothing waiting on it, are services: `process_work` lists them under
`services` with `kind: "job"` (and `unwaited_count`), no longer counts them, and
the session is Cooling or Cold by its idle time. Services never defer the reply
badge and never hold a team's capture gate. A session waiting for its own jobs
remains in **Active** and still accepts new messages; open **Processes** to
follow output or inspect the running commands. Input/approval requests and
native work take precedence. Offline and historical/unconfirmed process records
never establish current work. The CLI's `process_work` summary uses current
owned OS trees (one process-table sample per fleet listing) or the native
DeepSeek job registry, with at most eight `jobs`; it does not scan native
transcripts on each fleet poll.

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
each session once. Each status filter's tab count is the number of sessions it
lists before the text search, scoped like the list, and a parent or child that
fails the filter is never listed for its relative. Archive counts and lists the
same loaded date window. Desktop, Android and iOS use the same filter IDs and
membership rules, pinned by one shared case table
(`mobile/android/app/src/test/resources/session-filter-contract.json`); drafts
remain local to each device. Drafts for visible subagents also keep
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

<a id="agent-teams"></a>

## Agent teams

### Team requests

Every `hgs team OPERATION --json` reads one JSON object on stdin, or from a file
with `--json-file PATH` (PowerShell has no `<<<`); a UTF-8 BOM is dropped. A
command run from outside every agent session acts as the owner; `"by":"human"`
is accepted and any other value refused. An agent session acts as itself.

A member has one shape in `plan`, `add-member` and a draft:
`{"id":"dev","role":"Backend","agent":"claude","account":"work","model":"opus","effort":"medium"}`
(optionally `models`, `prompt`, `folder`, `computer`, `permissions`). Members,
leads and tasks are parsed strictly: an unknown field is refused with its name,
the accepted fields and an example, and a wrong type with serde's reason. A
task's text is `description`. A member without `account` starts on its
computer's default account for its agent, which the preflight names.
`max_members` counts the members besides the lead; more members than that is a
preflight error, so a plan, an approval and a draft's create are refused with it.

`draft get` answers the draft's `fields` (name, goal, addendum, settings,
members, tasks, phases and plan mode). A draft update sets only the fields it names;
`members` is a list that replaces them all or an object of changes by member
id, where null removes a member and a new id adds one. `plan_mode` is a draft
field: the owner chooses it and the lead offers it with `plan_mode_reason`.
The owner's settings (`OWNER_SETTINGS`: `base_branch`, `branch_per_task`,
`max_members`, `approval`, ...) work the same way in a draft (task #13): a
lead's draft update that sets one is not refused; the owner's value stays, the
lead's goes into the draft's `setting_offers` (the receipt names them in
`note`), New team lists them under **The lead offers settings only you set**
with **Apply**, and an offer goes when the owner applies it or sets that field
to something else. Outside a draft these settings stay the owner's card.
A draft's tasks keep their `phase` and its `phases` list phases 2..N
(`{"n":2,"title":"...","barrier":true}`), as a plan's do; a phase a task names
but the list leaves out is kept untitled. Create keeps them all as Draft phases
in order, which the owner approves one after another; a listed phase without
tasks is refused.

A swap card names, for each member, what changes (`agent`, `model`, `effort`,
`account`, `prompt` or `session`), or `"fresh": true` for a new session with
the same settings; an unknown field such as `with`, or a swap that changes
nothing, is refused. One card swaps several members with
`"members":[{"id":...},...]`. Before the new session starts, the session a swap
replaced is paused when it can be (its conversation stays under Saved) and
renamed to `<name>-replaced`, so the member's name is free even when the old
session waits on a dialog. A DeepSeek session is set aside through its host the
same way: one in a turn or waiting on a question keeps its name and its task
until the turn ends, then it is paused and renamed and the new session starts.
A swap's `session` may be the name of a running session (`"session":"NAME"`);
naming the session the member already has binds it again and sets nothing
aside. For a relaunch or a swap, the owner's word may name
the members together ("перезапусти их", "relaunch them all"); it then pays for
one change per member.

A team keeps one relaunch card per member. A card the lead proposed for a member
that had stopped, was stuck or had failed to start withdraws by itself once that
is over (the session runs again, it is no longer stuck, it started), and so does
any relaunch card for a member relaunched or swapped since; the lead's inbox
says why. A card for a member that ran normally stays until the owner decides.
A DeepSeek session has no archive: its name is freed by pausing and renaming it
(`hgs pause`, `hgs rename`), or `hgs kill` forgets it.

An `account` in a swap or for a new member is an account's id or its label, as
`hgs account ls` shows them (`"account":"additional"` names the profile
labelled Additional); the request keeps the id. It is checked where the request
is made, for members of this computer: a proposal with an unknown or ambiguous
account is refused with the accounts the computer has for that agent, the
owner's own `swap-member` and `add-member` likewise, and an allowed card is
checked whole before its first member is touched, so no session is stopped for
an account that cannot launch. A swap card that changes only the account, to
the one the member already runs on (a member without an account runs on the
computer's default for its agent), is refused: it would stop the session and
change neither the credentials nor the usage limit; `"fresh": true` asks for a
new session on the same account.

A new session never starts beside the one it replaces. While the replaced
session keeps its turn open, the member is not started
(`launch_waiting`); the lead's inbox names it at once (`member_waits_replaced`)
and, after five minutes, so does Waiting for you (`replaced_waits`, with
**Open Terminal** on the replaced session). A session that waits for a usage
limit keeps its turn open until the limit resets, so that wait ends only when
the owner stops the session in Sessions; Zerus does not stop a session in the
middle of its turn by itself.

In a member's usage-limit request, an option that pays (usage credits, extra
usage) is the owner's: the lead answers with a wait or stop option, and
`hgs team answer` refuses a paying one from the lead.

The build broker is offered only for a shared folder that builds: a project
build file at its root, an Unreal project or the project's `build_broker`
table. A decided proposal reaches the lead's inbox message once.

### The Team designer's project and folder

Team designer does not require an open session. Choose any project folder on
this computer in its header, then describe the project's work in the chat. A
selected local session still supplies its project automatically. The designer
reads that project and the existing catalog, asks about unclear tasks and
constraints, reuses suitable roles and templates, and proposes new or changed
ones only for uncovered needs. A running designer keeps its original project.

The Team designer runs in Zerus's own folder,
`<state>/hgs/sessions/team-designer` (`HGS_STATE_DIR` when set, else
`~/.local/state/hgs/sessions`). Zerus creates and owns that folder: it belongs
to no project, and the desktop shows the designer's session in General.

At `hgs team catalog designer` Zerus trusts exactly that folder in the Claude
account config the launch reads, so Claude's start-up "Trust this folder?"
prompt no longer stops the designer, and gives the session narrow permission
rules: Bash for `hgs team catalog get`, `propose` and `route` and for
`hgs guide`, plus read-only file tools (Read, Glob, Grep). The rules grant no
writing tool; the designer's only change is a catalog proposal that the owner
keeps or rejects.

Claude's start-up trust stays manual for every other folder. No other agent
(Codex, DeepSeek, Kimi, OpenCode) gets folder trust or rules from Zerus.

### The team roster on the owner's word

The lead changes a team's roster only on the owner's explicit word: a sentence
the owner typed in the lead's own session, at most 30 minutes old. Zerus reads
that session's journal under its current and earlier names, so a session
renamed after its journal began (its events stay under the name it had then)
is still the lead's own. For `add-member` the sentence must name what is
added — the member's role or its agent/model, in English or Russian
("backend"/"бэкенд", "frontend"/"фронт",
"qa", "reviewer"/"ревьюер", "expert"/"эксперт"; "DeepSeek"/"дипсик"/"dsh",
"Claude"/"клод", "Opus"/"опус", "Codex"/"кодекс", "Kimi"/"кими") — not the
generated id. The sentence says how many members it adds; one is the default.

When the add would exceed `max_members`, the same word raises the cap by
exactly the count the sentence names, within the tree's session budget, in the
same change, and the feed names the old and the new cap. One verified prompt
covers one add per count it names: the same sentence cannot add a second
member. `remove-member` and `swap-member` still need the member's id in the
sentence, and a separate lead `settings` cap change still names the new cap.
The team's roster and feed say "Added by Lead on your word" with the quote.
A refusal is never the owner's word: a sentence carrying a negation ("Don't
remove backend2", "не надо добавлять второго тестера") authorizes no roster,
scale, drain or phase change in either language.

### The lead plans inside the approved goal (task 162)

Owner, 2026-11-07: the owner gates the team's goal and plan approval, roster
changes (add, remove, swap, scale, drain, relaunch), wider rights, paid
resources and budgets, release and publication, and destructive actions.
Inside an approved goal (a running or paused team) the lead plans by itself:
`add-to-phase` (the tasks join at once, `added_late` with the confirmation
"inside the approved goal"), `extend` with `"phase"` into an accepted phase,
`edit-draft` and `move-phase` of an accepted phase that has not started,
`move-task`, and `cancel` (a split is an addition plus the cancel of the
empty task). None of these is a card any more: a proposal of add-to-phase,
edit-draft, move-phase, move-task or extend into a phase on a running team is
refused with the operation to run instead.

Each such change that touches accepted work is recorded in the team's
`lead_changes` (`lcN`: the operation, a summary such as "added #12 Fix the
login crash to phase 3; moved #9 from phase 2 to 3", who, when, the tasks and
phases as they were before, the tasks it added), journaled
`lead_changed_plan`. A change only of drafts, which the owner approves anyway,
is not recorded. Waiting for you shows one `lead_changes` item per team (action
`open_team`), which blocks nothing: the unseen changes, newest first, each
with its revert request. `{"team","change":"lcN","by":"human"} | hgs team
revert-lead-change --json` (the owner only) puts the tasks it changed back as
they were, removes the tasks it added and restores its phases; it is refused
while a later change touched the same work (revert that first), when a touched
task has started since, or when another task depends on a task it added.
`{"team","by":"human"} | hgs team seen-lead-changes --json` marks the digest
seen (with `"change"`, up to that one). `hgs team lead-changes --json` lists
them. At most 300 are kept, seen or reverted ones leaving first.

### Critical and Minor follow-ups (task 229)

Owner, 2026-11-07: "one task or review can spawn new tasks with a status -
Crit or Minor. Crit: blocks using the phase's result (its release). Minor:
shortcomings or missing functionality that do not block the phase's result."
The lead spawns them with `{"team","origin":N,"severity":"crit"|"minor",
"tasks":[...]} | hgs team spawn --json`; each spawned task carries its `origin`
(a task or review) and its `severity`, and a report's or a review's finding
may name that severity for the lead to turn into a task.

A **Crit** joins the running phase of its origin at once and without a card.
It becomes a hard dependency of what its origin produced (`deps` names the
origin), so nothing that uses the phase's result runs before it is decided, and
while it is open the phase cannot be accepted or closed, no release candidate
starts (the `release-candidate` run is refused naming the crits), and `finish`
waits. The owner is told at once, without acting: Waiting for you shows one
`crit_added` item per open crit, "The lead added critical task #N to phase P
(from #origin): title" (action `open_task`, the desktop notifies it like any
Waiting item), the plan feed carries `task_spawned`, and the change is the
lead's own `lead_changes` entry (`lcN`, "opened critical #N ..."), revertible
until the crit starts.

A **Minor** blocks nothing. It joins the next phase that has not started, or a
Minor phase the lead opens (`"new_phase":true`, or when no later phase is
unstarted): a phase of `kind` "minor" that runs at once without an approval
card and in free mode, ordered right after the origin's phase. A Minor phase
may end with **tail tasks** (`"tail":true`) that depend on every Minor in it,
such as the release build with the fixes. A Minor never gates its origin's
phase.

`hgs team status --json` carries each task's `severity` and `origin`, each
phase's `kind`, `crit` (its open critical task ids) and `crit_gate` (the line
that holds it), and the team's `crit` list of open crits with their phases;
`hgs team events` carries `task_spawned` with the severity, the origin, the
phase and the gate. The desktop renders these fields; the CLI is their source.


The owner's typed word (`owner_word`) is no longer taken where a card exists:
approve, approve-phase, discard-phase, return-to-draft, unpause-phase,
phase-acceptance, accept-phase, add-member, scale, drain, swap-member,
remove-member, relaunch and settings refuse it with the card to propose; for
the lead's planning operations it is ignored. The owner's own add-to-phase
keeps the warning and confirmation below.

### Editing a member while the team runs

`hgs team settings` with `members_update` changes one member per entry, and the
operation takes only the fields it names: an unknown field is refused with its
name, the fields it takes and an example, never dropped in silence. `agent`,
`account` and `computer` stay with Swap member; `folder`, `directory` and `role`
still need a member without a session.

A member that runs takes its start `prompt`, `model`, `effort`, `permissions`,
`permissions_profile`, `context_before_task`, `after_task`, `after_task_on`,
`after_task_at`, `models` and `iron_rules` while the
team runs. A model or effort change reaches its session before the member's next
task (the same `hgs settings` switch U45 uses); an agent that cannot switch in
place takes it at its next start, and a refused switch leaves the member on the
model its session kept. A prompt change is sent once to the member as a message
marked "replaces previous instructions", after the clear the owner asked for
with `"clear_context_first": true` (off by default; the clear needs the agent's
own clear action, so DeepSeek and OpenCode refuse it); an unanswered send is
recorded uncertain and never replayed, and the next hand-out carries the new
prompt as well. An edit made while a message is on its way stays for the next
one. Permissions and the permissions profile are the mode the member's next
start gets; its running session keeps its own. A swap to another session drops
the changes still pending for the session it replaced. The owner's After a task
policy (`after_task` empty keeps context, `compact` or `clear`, with
`after_task_on` `percent`/`tokens` and `after_task_at`) reaches a free member's
next hand-out only when its context passes the threshold; the lead's runs only
while the lead is free.

Only the owner changes a member while the team runs. The lead changes a member's
`model` and `effort` directly; every other member field goes through the
`{"kind":"settings","members_update":[...]}` card the owner allows. A permissions
mode the member's agent does not have (Codex has no `auto`) is refused in `plan`,
`add-member` and `members_update`, never accepted and left to fail when the
member starts.

In the Qt team workspace, a member's menu offers **Edit member** once its current
settings have been read. The dialog starts with that member's prompt, model,
effort, permissions/profile, per-task context action, **After a task** mode with
its criterion and threshold, allowed models and iron rules. The threshold field
takes 1-100 percent, or a token count when the criterion is tokens. Save sends
only changed fields in one `members_update`; a refusal stays
in the dialog and preserves the draft while the stored member stays unchanged.
A changed prompt reveals **Clear context first**, unchecked by default (only a
running member can clear). Permissions-profile widening requires the owner's
explicit checkbox confirmation. Agent, account, computer and folder changes stay
with **Swap member**.

### Tasks added to a running phase

The owner's task joins a running phase on a warning and a confirmation. The
first `hgs team add-to-phase` call (`{"team","phase","tasks"}`) changes
nothing: it keeps the addition pending and answers with a warning and its
`pending` id. The confirmation names that id in `confirm`, with the typed
`"confirmation":"add"` or the phase number (the owner on the desktop); only
then the tasks join as Pending. The lead's addition inside the approved goal
joins at once (above).
An addition waits 30 minutes. An expired one is no longer listed in Waiting for
you, cannot be confirmed and leaves the team on its next change.

The owner declines a pending addition with `{"team":"<id>","phase":N,
"confirm":"<pending id>","decline":true} | hgs team add-to-phase --json`; Cancel
in Waiting for you sends it. The addition leaves, nothing joins and the feed
records `phase_addition_declined`. Only the owner declines. A decline of an
addition that is already gone (confirmed, replaced, expired or declined)
changes nothing and succeeds, also when the phase no longer runs. An Allow on
the lead's add-to-phase proposal warns and confirms at once; when its
confirmation fails, its warning is declined, so no addition is left waiting.

### Follow-ups: a task's origin

A task added because of something missed or a bug carries `origin`, the task
that produced it, in the same team (task #60, owner 2026-11-06):

```json
{"request_id":"<uuid>","team":"<id>","tasks":[{"id":14,"title":"Fix the empty cart crash",
 "assignee":"dev","origin":9}]}
```

`origin` is accepted wherever a request carries tasks: `plan`, `extend`,
`add-to-phase` and its proposal card, `intake`, and `edit-draft`'s
`tasks_update` (`"origin":null` removes it). It is distinct from `deps`: it
never blocks or gates hand-out, it only places the task at least in its
origin's layer, as if the task had the origin's dependencies without depending
on the origin itself; dependencies that reach further place it further. A task
without deps and with an origin therefore sits beside its origin, not in the
first layer. An origin that names no task of the team, or the task itself, is
refused (`"Task #14 names #99 as its origin, which is neither in the plan nor
among the new tasks"`; validation reports `kind` `origin`). It follows
renumbering: new tasks numbered otherwise (#754) and drafts renumbered after a
discard (F285) keep their link, and a task whose origin was discarded loses it.
An urgent fix filed from a finding takes the finding's task as its origin
unless it names one.

Status and the list carry each task's `origin`; `layers` place it; the feed's
`phase_added`, `phase_edited` and `phase_tasks_added` carry `origins`
(`{"14":9}`) for the added tasks that have one, and an `edit-draft` change of
it is recorded with `field` `origin`. A record stored before the field has no
origin and reads as before. The desktop shows "Spawned from #9" on the task and
"from #9" on its card; the follow-ups of a task are listed on its card.

### Task pipelines and stage verdicts

A task may pass its team's pipeline before it reaches the lead: an ordered list
of `review` and `test` stages, each done by a member the stage names (a role, or
a pinned member; the implementer never takes its own task's stage). A preset
may also hold a `command` stage and a final `land` stage, which Zerus runs
itself with no member (below). While a stage runs the task is `in_review` or
`in_test`, between `submitted` and `accepted`.

The team catalog keeps named presets: `code` (review by the reviewer, test by
the tester, then the lead), `docs` (review, then the pipeline accepts) and
`check` (no stages; the lead decides), each with its maximum rounds (3 by
default), folder rule (`hold` or `checkout`) and final acceptance (`lead`,
`sub_lead` or `auto`). A template or a role may name a default preset; a task
names its own with `"pipeline":"code"`, `"docs"`, `"check"` or `"none"` in
`plan`, `extend` or `edit-draft`. Zerus snapshots the preset when the task is
first handed out, so a later catalog edit never changes a running task.

A stage member reports with `hgs team report --json` and a verdict, naming the
stage's attempt id: `{"task":7,"attempt":"STAGE-ATTEMPT","verdict":"pass",
"findings":[],"text":"..."}`, or `"verdict":"return"` with the findings (at most
50, each naming file and line). `pass` moves the task to its next stage;
`return` sends the findings to the implementer as a new attempt, without the
lead, and each such round counts. After the preset's maximum rounds the task
returns to the lead with every round's findings. The lead and the owner can
still skip a stage (`hgs team skip-stage {"task":7,"reason":"..."}`), send the
task back from any stage (`hgs team send-back`), accept or reject, each with a
reason; the feed names who did it.

A stage works in the task's own folder: the implementer's worktree or clone, or
the team's shared folder. Review reads the attempt's change against the task's
criteria; test runs only the suites of the files the change touched and takes
the criteria's shots. Neither edits the task's files: a stage writes only build
output and its own temporary folder. What changed is the attempt's commits in a
Git checkout, otherwise the changed files its report names. A `hold` preset
(the default) holds the folder while the task is in its test stage, so the
implementer's next task in that folder waits; a `checkout` preset tests a
detached checkout of the attempt's commits instead and frees it.

### Closed phase records and task reads

The team store (`teams/teams.sqlite3`) archives a phase after all its tasks
are accepted, rejected or cancelled and the phase is closed: a lead-accepted
phase closes on its last task decision; an owner-accepted phase must reach
`done`. A phase with an untested merge range or a queued/testing merge entry
stays live until its integration checks close. The open phase of a free-mode team is retained until it closes.
Full task records, including attempts, reports, runs and questions, move to
`team_task_archive` in the same transaction as their replacement in the live
record. Original phase records are retained in `team_phase_archive`.

The 500-task limit counts live, unarchived tasks. The 100-phase limit counts
unarchived phases. Historical work consumes neither limit. The live record
keeps the most recent 1,000 archived task stubs, with id, phase, title,
assignee, final status, acceptance time, merge commit, optional origin and frozen timing totals.
This window bounds routine plan reads to hundreds of kilobytes of archived
task metadata. A task stub leaves the live window when its phase header
leaves, so no archived phase is presented as a recordless draft. The live record also keeps at most 200 archived phase headers,
with counts, outcomes and closure time. Older headers and task lists are paged
from the archive. Evicting a finished phase header also removes its planning-mode
override; archived phase pages do not carry a planning mode. Constant-size archive totals preserve the team's historical
counts. Both windows use descending numeric IDs, so their membership is stable
across repeated reads. Task IDs remain unique
when stubs leave the window. Dependencies outside the window resolve through
the archive's `(team, task)` primary key; existing dependency outcome rules
still apply. Follow-up origins use the same indexed resolution without restoring
historical tasks into the live plan.

Read one full task, live or archived, without returning the whole team:

```sh
printf '%s' '{"team":"ID","task":42}' | hgs team status --json
```

This response is the task object. The byte limit is 8 MiB per task; larger
historical records remain intact and yield an explicit size-limit error.
Normal `status --json` keeps its team view and never includes archived full
histories or stubs outside the live window. Inbox report links name the
per-task form.

Read archived stubs for one phase with stable ascending task-ID pages (default
and maximum 50 rows):

```sh
printf '%s' '{"team":"ID","archived_phase":3,"cursor":0,"limit":50}' | hgs team status --json
```

Read archived phase headers, including those outside the 200-header window,
with `{"team":"ID","archived_phases":true,"cursor":0,"limit":50}` through the
same status command. Its cursor is a phase number; task pages use task IDs.
Both responses contain `items` and `next_cursor`; pass the latter as `cursor`.
The `(team, phase)` primary key and `(team, phase, task)` index bound each page. New archived phases do not
shift an older page. GUI clients can use these pages and per-task reads off
the UI thread.

The Tauri Teams plan, reports and Profile show archived task stubs and frozen
working time from the status window. They retain its limit of 1,000 task stubs
and 200 phase headers; older omitted phases are named with their count. Opening
an archived report reads just that task on a blocking worker. The report panel
shows loading, failure and retry states, discards late replies after selection
changes, and retains only the selected report (up to 64 Ki characters, with an
explicit truncation notice). It does not load archive history on refresh.

Existing records migrate on their next successful mutation. To migrate one
without changing its plan, the owner or lead sends `hgs team archive-phases
--json` with `team` and a canonical `request_id`; retries are idempotent.
The archive and live payload commit together or roll back together. The existing
1 GiB (262144-page) store cap remains. If SQLite reports a full database,
Zerus names `teams.sqlite3` and its cap, refuses the change and preserves the
original readable record. Archive retention or export is a separate policy. Explicit
edits of archived work restore only that phase, subject to the live 500-task
limit and an 8 MiB phase-history read cap, and retain the original history.

The Qt workspace and Profile offer **Archived phases**. This reads phase headers
and task summaries in replaceable pages of 50 through the archive status reads
above, including phases outside the live window. Each summary shows the title,
final outcome, recorded work time and closure time. Double-clicking a task reads
its full report through the per-task status read. CLI I/O and JSON decoding run
on the process worker, with a 15-second local deadline and an 8 MiB output cap.
Loading, refusal and retry remain visible; navigating away discards stale replies.
The drawer retains only its selected full task, and the archive browser retains
one phase page and one task page. A phase link seeks directly to that phase; it
does not scan earlier history. Archived Profile scopes show recorded metadata
instead of reconstructing timelines from compact stubs. The live Profile cache
keeps at most six scopes, 1,000 task rows and 1,000 timelines per scope.

### Reports and interim reports

A member reports with `hgs team report --json` (`task`, `attempt`, `text`): the
attempt becomes reported and the task `submitted`, then its pipeline's first
stage starts. `"interim":true` records progress on the same open attempt
instead: the attempt stays open and the task stays `in_progress`, the member
keeps its work, and no stage starts. The lead reads the interim text in its
inbox and in the task's feed (`interim_report`); Waiting for you does not show
it, so the owner sees it only in the feed. A newer interim report replaces the
previous one on the attempt. A question is not a report: it goes through
`hgs team ask` (the lead answers it with `hgs team reply`), never
`hgs team report`.

A turn may end without a report. Zerus keeps its last reply and the attempt
open, and the lead's inbox lists it as a missing report with the reply and the
one command that takes it: `hgs team use-last-reply --json` with the task and
attempt (task #362). The lead, the sub-lead the member reports to, and the
owner may run it; a member cannot use its own last reply as its report. The
reply becomes the attempt's report (so the task `submitted` and its pipeline
runs) and the feed event names the lead, the member it is for and the quoted
reply, while the lead-changes digest records the action with the reply quoted;
it is no plan change, so it cannot be reverted. The command without an attempt
uses the task's open attempt, and without `text` the CLI reads the attempt's
kept reply (or the member's session, for an older record).

Task #39 (team ede724c4): a member whose turn ended while it waits for its own
job is not missing a report. Its own job is the Zerus run it started
(`hgs run`, `hgs team run`), a background job its hooks registered, Background
work under a budget, or the run the dispatcher remembers it waits on; by the
session-state contract the session is Building. Its last reply ("still going,
I'm waiting for its numbers") must not become the report, so the inbox lists no
missing report. It shows the informational item `waits_for_own_job` instead,
naming the job and its start ("waits for its own Zerus build (perf baseline)
since 14:05"), with no action: Zerus wakes the member when the run settles.
A member whose session stopped waits for nothing.

### The integration line: landing, cherry-picks and rejected attempts

With an integration folder, accepted work lands on the line through the merge
queue. Task #370 (owner, 2026-11-08) adds three rules:

- A task counts as landed when the line holds an equivalent of its accepted
  commits (`git cherry`, a patch-id match) as well as when the commit is an
  ancestor, and when the lead marks it landed by a named line commit:
  `{"request_id":"UUID","team":"ID","task":N,"commit":"LINE-COMMIT"} | hgs team landed --json` (the lead or the owner; the mark is journaled as `task_marked_landed`).
- A landing refuses a range that carries a commit of a rejected attempt of any
  task: the queue merges no member branch then, keeps the task undecided, and
  the lead's inbox offers a cherry-pick of the task's own commit instead:
  `{"request_id":"UUID","team":"ID","task":N,"cherry_pick":true} | hgs team land --json`
  (`hgs team land` also re-queues a landing with or without the cherry-pick).
- A task whose reported commit is not in the integration repository at all
  (another project's work) never appears as unlanded.
- DCB-svn report (2026-11-12): a landing `hgs team land` queued for a task that
  is already accepted records the landing when the queue merges it
  (`tasks_landed`), never a second accept. A task marked landed (by Zerus's
  ancestry check or `hgs team landed`) loses its queued landing at once, and
  the queue's next step drops any such entry saved before
  (`merge_dropped_landed`): an accepted, landed task never holds the landings
  queued behind it. A landed task leaves playbook rule (b).
- A merge-queue step that fails writes nothing; its error goes to the worker
  log, to the team's events (`merge_queue_failed`) and, as the reason of the
  landing the step took, to the lead's inbox (`merge_waiting`), once per
  distinct error in 30 minutes. A landing held by uncommitted changes in the
  integration folder is a `merge_waiting` item with that reason once it has
  waited ten minutes.
- With the queue on, a pipeline that accepts by itself (`auto`, such as
  Docs: review, auto) queues the landing as the lead's accept does: the task
  stays undecided with a `merge` entry (`merge_queued` by Pipeline), and the
  queue's accept carries the pipeline's reason.

Task #443 (owner, 2026-11-09) adds the queue-off rule. With the merge queue off
(no `integration_folder`, the owner's p92: a hand landing in the team's own
folder) an accept decides at once: the task is `accepted` and its work waits for
the merger's hand landing. Such a task - the queue's landing still `queued` or
`testing` with no failure note, or a hand landing with no merge entry
(`not_queued`) - is never a "decision waiting": no report item, no idle digest
entry and no `report waiting` reminder. `hgs team status` names it in one line
instead, `waiting_to_land`: `line` ("accepted, waiting to land: #426 (26 min),
..."), `more`, and each task's `commit`, `age` and `state`; the line also lists
the queue's own waiting landings, so it is one place whatever the line's
setting. A failed, blocked or conflicted landing stays the lead's: its
`merge_waiting` item and the report reach the lead again. When the accepted
commit becomes an ancestor of the line (or of the team folder's HEAD when the
line is off), Zerus accepts the task and marks it landed by itself
(`landed.by` "Zerus", the `tasks_landed` event): the lead never accepts it a
second time with `"merged":true`.

### Free planning: the plan mode and the open phase

A team plans in `standard` mode (the default) or `free` mode. `hgs team status`
and the polled team list carry `plan_mode`; a team stored before this field has
none and reads standard. At the start of planning the lead proposes one of the
two with a one-line reason in `plan` (or in `extend` while the team is
Planning):

```json
{"request_id":"<uuid>","team":"<id>","plan_mode":"free",
 "plan_mode_reason":"a stream of small support requests","members":[...],"tasks":[...]}
```

The team keeps it as `plan_mode_offer` and `plan_mode_reason` (status carries
both) until the owner chooses at approval: `{"plan_mode":"free"}` in `approve`,
accepted only from you; a lead's approval on the owner's word, or a client that
sends no mode, keeps standard. The approval clears the offer.

A free team's open phase never closes by itself. `extend` adds tasks to it with
no new phase and no approval, and each task runs as it is added; an empty queue
is `idle`, not done. Status carries the phase's state:

```json
{"plan_mode":"free",
 "free_phase":{"phase":2,"state":"open","running":[7,8],"accepted":5,
  "accepted_since":2,"last_build_at":1793300000.0,"close_pending":null}}
```

`state` is `idle` when no task of the phase is unfinished; `accepted_since`
counts tasks accepted since the last build, `last_build_at` is the last accepted
build (0 for none) and `close_pending` is the phase whose close waits for a
build. Switching mid-plan is the owner's decision, bound to the request:

```sh
{"request_id":"<uuid>","team":"<id>","kind":"switch-plan-mode","mode":"free",
 "reason":"the support queue never drains"} | hgs team propose --json
{"team":"<id>","proposal":"p3","accept":true,"expected_hash":"<sha256>"} |
  hgs team decide-proposal --json
```

The proposal answer carries `hash`, the SHA-256 of the kind, mode and reason. A
missing or different `expected_hash` is refused ("reload the dialog and answer
again"). Allow sets `plan_mode` and emits `plan_mode_switched`; Deny keeps
standard, keeps the reason and emits `plan_mode_denied` for the lead. A card
without a phase switches the team's mode: the open phase and every phase
without a mode of its own follow it, and a switch to standard leaves no phase
free.

#### The mode of one phase

The mode is a property of each phase (task #58, owner 2026-11-06). A phase
plans in its own mode when it has one, else in the team's `plan_mode`, which
stays the default: a team stored before phase modes reads its mode on every
phase, and a new phase starts in it. Status and the polled team list carry
`phase_modes`, every phase's mode by number (phase 1 included), and each
`phases` item its `mode`; the watch's team rows carry `plan_mode` and
`phase_modes`, the phases that have a mode of their own:

```json
{"plan_mode":"standard","phase_modes":{"1":"standard","2":"free"},
 "phases":[{"n":2,"state":"approved","mode":"free"}],
 "free_phase":{"phase":2,"state":"open","running":[7],"accepted":3,
  "accepted_since":1,"last_build_at":null,"close_pending":null}}
```

The owner switches one phase directly, with one guarded operation; the lead
only proposes it, with a `switch-plan-mode` card that names the phase:

```sh
{"request_id":"<uuid>","team":"<id>","phase":2,"mode":"free","reason":"..."} |
  hgs team phase-mode --json
{"request_id":"<uuid>","team":"<id>","kind":"switch-plan-mode","mode":"standard",
 "phase":2,"reason":"ready for the release"} | hgs team propose --json
```

A lead's `phase-mode` is refused ("Only you switch a phase's mode; the lead
proposes it"). The card's `change` is `{"phase":2}`, covered by its hash, and
it is checked when it is made: a phase that cannot switch is refused at once.
Allow applies exactly that switch. Both answer
`{"status":"switched","phase":2,"mode":"free","build":null,"close_pending":null,
"phase_modes":{...}}` and emit `phase_mode_switched` (`by`, `phase`, `mode`,
`proposal`, `reason`, `build`, `close_pending`). Every other phase keeps its
mode.

- Standard to free opens a running phase (approved, or phase 1) for tasks as
  they come: `extend` without a phase adds to it with no new phase and no
  approval, and its empty queue is idle and never done. One phase is free at a time: opening another while one is free is
  refused ("Phase 2 already takes tasks as they come; switch it to standard
  first"). The free phase is the open (latest running) phase when it plans in
  free mode, else the running phase whose own mode is free; `integrate-build`
  and `close-phase` act on it.
- Free to standard locks an accepted phase. Its tasks keep their state and run
  on; no new task joins it without a plan edit (`extend` with its `phase`,
  which asks the owner for an accepted phase); a new `extend` plans a new
  phase. The lock resolves the free phase's own work explicitly: an open build
  task stays an ordinary task of the phase, handed out and decided like any
  other, and the phase is done only when it is decided; accepting it still sets
  `last_build_at` but closes nothing. A pending close of the phase ends with
  the lock, which is that close: `free_close_pending` clears and the receipt
  and the event name it as `close_pending`, with the build as `build`.
- A draft has no mode to switch; a switch to the mode the phase already has is
  refused.

`close-phase` keeps U59's close: the free phase drops a mode of its own and the
team continues in standard mode; a request that names `phase` must name the
free one.

Every proposal is checked when the lead makes it, on a copy of the team, by the
code Allow runs: one Allow could not apply (a task the plan cannot parse, work
that does not exist) is refused at once. A question to the owner is a proposal
too, never only prose in a reply:

```sh
{"request_id":"<uuid>","team":"<id>","kind":"question","question":"Install the cache on D:?",
 "options":["Yes","No"],"recommended":"Yes"} | hgs team propose --json
{"team":"<id>","proposal":"p4","accept":true,"expected_hash":"<sha256>",
 "answer":"Yes, with a 30 GB limit"} | hgs team decide-proposal --json
```

At most eight options; the recommended one is among them. The answer is an
option or the owner's own words; the proposal becomes `answered` with `answer`,
and the lead's inbox carries it with the question. Waiting for you lists the
question and answers it in place, as it does a lead's own open AskUserQuestion
(`hgs team answer` with the question's id and hash).

Every change the lead may make only with the owner's consent is a card the
owner answers with Allow or Reject (task #21, owner 2026-11-05); no flow needs a
typed sentence. Besides the kinds above, a proposal's kind may be one of the
owner-gated operations `relaunch`, `scale`, `drain`, `approve`, `approve-phase`,
`return-to-draft`, `edit-draft`, `move-phase`, `unpause-phase`,
`phase-acceptance`, `accept-phase`, `extend`, `settings` and `full-tests`, with
the operation's own fields beside `kind` and `reason` (or in `"input":{...}`):

```sh
{"request_id":"<uuid>","team":"<id>","kind":"relaunch","member":"hgs",
 "reason":"its session failed to start"} | hgs team propose --json
{"request_id":"<uuid>","team":"<id>","kind":"settings","settings":{"max_members":8},
 "reason":"two more testers"} | hgs team propose --json
```

The proposal's `change` keeps `op`, the checked `input`, and the `summary` and
`lines` the card shows. Allow runs that operation as the owner's own `hgs team
<op>` (the same checks, adoption, budgets and side effects) under the
proposal's stored request id, so a retried Allow answers its receipt and never
applies the change twice; Reject changes nothing. `proposal_decided` names the
`op`, the `summary` and the `request` it ran under, beside the operation's own
events by You. A `switch-setting` card also sets `base_branch`, `full_tests` or
`max_members` through the settings operation, and `iron-rules` (with `scope`
project or session, its `project` or session `name`, run and conversation ids,
`rules` and `expected_revision`) sets those rules as `hgs rules set` does. A
typed owner word stays a fallback: its quote may be part of the owner's
message, one message pays once for each member it names, and the check reads
the lead session's main messages, not its last 200 events of any kind.

Integration and a build are ordinary lead tasks, not a phase barrier. The owner
or the lead adds the build for the open phase (`hgs team integrate-build`) and
it is handed out and accepted like any task; accepting it sets `last_build_at`
and resets `accepted_since`. The owner closes the free phase:

```sh
{"request_id":"<uuid>","team":"<id>","reason":"...","integrate":true} |
  hgs team close-phase --json
```

The close is refused while tasks of the phase are undecided, naming them. With
`"integrate":true` and work accepted since the last build it adds the build task
and answers `{"status":"close_pending","phase":2,"build":17}`; the phase closes
when that build is accepted (`plan_mode` standard, `free_phase_closed`), and a
rejected or cancelled build drops the close (`free_phase_build_failed`) while
the phase stays open. With nothing accepted since the last build it closes at
once (`{"status":"closed","phase":2,"plan_mode":"standard"}`) and the team
continues in standard mode. The feed records `plan_mode_proposed`,
`plan_mode_switched`, `plan_mode_denied`, `phase_build_added`,
`free_phase_close_pending`, `free_phase_closed` and `free_phase_build_failed`.

### Phase checks before start

Team settings has a **Phase checks before start** group. Each check is independent
and off by default (`settings.phase_checks.critical_path_target` and
`settings.phase_checks.unresolved_prerequisites`). With both off, approval keeps
its existing behavior and cards show no extra check result.

A phase may name an optional positive `target_hours`, in `extend`, a `plan` phase,
or `edit-draft` (`null` clears it). With the target check enabled, its approval
card shows the longest dependency path weighted by task estimates in minutes.
Missing estimates are shown as a lower bound and a count, for example
`>= 6.5 h, 2 tasks without an estimate`. Only a known sum above the target warns;
this check never refuses approval. The profile keeps its existing count-based
critical path and output.

Tasks may name at most 32 prerequisites in `requires`, including through
`edit-draft.tasks_update`:

```json
{"requires":[{"task":12},{"commit":"0123456789abcdef0123456789abcdef01234567"},{"branch":"feature/api"}]}
```

A same-phase task requirement resolves when that distinct task exists in the
phase; it adds no dependency or ordering. An earlier-phase requirement resolves
when its task is accepted. A missing task, self-reference or later-phase task
is unresolved. Commit and branch requirements resolve only when Git verifies
that reference is an ancestor of the configured base branch (the team folder's
`HEAD` when no base is set). Unknown references, Git failures and timeouts remain
unresolved, with their reasons. File-existence requirements are not supported.

Draft mutations save `phases[].checks` for approval cards; reads and GUI refreshes
never invoke Git. Each Git call has five seconds within a shared ten-second
check deadline; approval rechecks the current plan and Git ancestry. With the prerequisite check enabled,
unresolved requirements refuse ordinary `approve-phase`. The card offers
**Approve anyway**, sending `approve_anyway:true`; the override only bypasses
prerequisites, preserving the other approval guards. An owner proposal card sends
the owner's explicit override through `decide-proposal`; a lead cannot preselect
it in the proposal. Existing approve requests and receipts remain compatible.

### When a phase starts

One phase-start model (task 158; owner, 2026-11-07). A task of any approved
phase is ready when its dependencies are satisfied; phases keep their order for
display, acceptance and the owner's acceptance gate, not for scheduling. An
approved phase therefore runs alongside the phases before it: approving a phase
while earlier work is open no longer queues it, and the receipt's
`phase_state` is `running` (`"alongside":true` changes nothing any more and is
only echoed). The receipt, the `phase_approved` and `phase_auto_accepted`
events and the phase summary name one state: a phase approved behind a
barrier, a pause or a phase awaiting the owner's acceptance is `queued`, and
the receipt's `waiting` says what holds it and how it goes. A task still waits
for:

- its dependencies (`hard` until accepted, `soft` until reported);
- the outputs an open task holds (task 155): a task whose `output` overlaps an
  open, unchained task's is refused at planning (`outputs`, chain them with a
  hard dependency) and held at the hand-out, also across phases;
- a barrier: only a phase marked `barrier` waits until every task of a lower
  phase is finished. The lead sets it when it plans; the owner sees it on the
  phase header and switches it in the phase menu (**Waits for earlier phases
  (barrier)**), also once the phase has started. Either sends
  `{"phase":N,"barrier":false}` to `edit-draft`; the lead changes the barrier
  of a draft directly and of an accepted phase with an `edit-draft` card;
- a pause on its phase (U63), or a phase before it that waits for the owner's
  acceptance (U63).

A draft runs nothing: its tasks are `draft` until the phase is approved, by
the owner or, with Auto-accept drafts on (`settings.auto_accept`), by Auto
(`phase_auto_accepted`) as soon as its plan has no blocker and no earlier phase
is a draft; a barrier draft is taken when the phases before it are done.
Auto-accept drafts is the owner's switch and takes every plain draft, whatever
approval (`review` or `auto`) the phase was added with; a draft the owner sent
back is never Auto's. Turning the switch on in the desktop names the drafts it
will approve and asks first. Until then the lead edits the draft freely
(`extend`, `edit-draft`, `move-task`, `cancel`). Status carries each phase's
`state` and `auto`: for a draft, whether Auto will take it; for an accepted
phase, the approval it was added with. With the switch on, no draft waits for
the owner's Accept in `attention`.

`edit-draft` takes `phase`, `revision`, `title`, `barrier`, `target_hours`,
`tasks_update` (a list of `{"id":N,...}` with the task fields to change) and
`tasks_remove` (a list of task numbers). A request naming any other field, a
`tasks_update` entry naming an unknown task field, or a request naming nothing
to change is refused with the fields it takes and an example; nothing is
dropped in silence.

This replaces start-ahead (U61) and work-ahead (U31) and the queued phase of
U25. A record stored before reads a phase's `queued` state as approved, and
drops `start_ahead` and `settings.work_ahead`; `hgs team start-ahead` and
`settings` with `"work_ahead":true` are refused with that reason. A report an
older record made ahead of a draft (`worked ahead of phase N`) still waits for
that phase's approval, and sending the draft back still tells its member to
stop.

### Starved members

A member starves when it is idle with a live session, has no ready task and
work of the team could start; the lead hears of it only when it can act (task
164; owner, 2026-11-07):

- only open tasks of a phase that can start count: a draft's, a paused
  phase's, and one behind a barrier or a phase awaiting the owner's
  acceptance do not;
- a review, QA, acceptance or merge role waits by design and starves only
  while a ready task of its role or kind sits with another member;
- after 20 minutes the lead gets one team-level digest (`plan_starved`, with
  `members`, `idle` and `actions`), at most once an hour (`starved_digest_at`).
  It names who is idle and the concrete actions: a ready task to reassign, a
  draft that can run alongside once approved (an owner card; with Auto-accept
  drafts on, only what keeps Auto from taking it), a phase held by its barrier
  with the request that removes the barrier (never an approval, which starts
  nothing behind a barrier), and what each
  startable task waits for (accept, start, release, retry, answer). It never
  says "give it work"; with no action to name, nothing is sent and the
  episode is marked told. A member that becomes due within the hour waits for
  the next digest.

### Changing, pausing and accepting a phase

An approved phase whose work has not started (no task has a
delivered attempt) is changed in place and stays accepted, with its number,
its place and its task ids: `edit-draft`, `extend` with `"phase"` and
`move-phase` work on it. The owner does it directly; the lead only with
`owner_word`, the owner's sentence naming the phase and the change. A draft
stays the lead's own. `return-to-draft` (the owner, or the lead on the
owner's word) is for a phase that must be approved again; it also keeps the
phase's number, place and task ids. Discarding or moving a draft renumbers
only the drafts after the last accepted phase.

`pause-phase` `{"phase":N,"reason":"..."}` holds such a phase: its tasks are
not handed out, and it keeps its place, so a later barrier phase waits for
it. The owner pauses, and the lead may pause on its own;
`unpause-phase` removes the pause. Only the owner removes the owner's pause;
the lead removes its own pause on the owner's word, or by itself when the
owner set `lead_removes_own_pause` (off by default).

A phase may require the owner's acceptance (`acceptance: "owner"`; new
phases take `settings.phase_acceptance`, `lead` by default). Once its tasks
are decided it is `awaiting_acceptance` and no later phase starts, urgent
ones included. Waiting for you shows "Phase N is ready for your acceptance"
with its tasks by outcome, its tests and its time; `accept-phase` makes it
`done`, and `send-back-phase` `{"comment":"..."}` returns it to the lead, who
adds the rework to the same phase with `extend`. The lead accepts a phase,
or changes whether one needs the owner's acceptance (`phase-acceptance`),
only on the owner's word. The feed records `phase_paused`, `phase_unpaused`,
`phase_acceptance_changed`, `phase_awaiting_acceptance`,
`phase_owner_accepted` and `phase_acceptance_sent_back`.

### A model per task, a task for a role, and the context before one

The owner's catalog gives a role the models its members may switch between,
`"models":[{"model":"claude-opus-4-1","effort":"high"},
{"model":"claude-sonnet-4-5"}]`; a member copies that list when it joins, so a
later catalog edit changes no running team, and the owner may edit a member's
list in the team settings. With no list a member runs only its launch model and
effort.

A task may name `"models":["claude-opus-4-1","claude-sonnet-4-5@high"]` (a
model, or `model@effort`); empty means any model of its member. It may name
`"role":"Backend"` instead of an assignee: such a task must not be `pinned`,
and a task naming both an assignee and a role is refused at `plan`, `extend`
and `edit-draft`. A plan naming a role with no member of that role is an
error. Zerus gives the task to the first free member of the role, in roster
order, whose models allow one of the task's; a member already on an allowed
model is preferred over one that needs a switch, and rebalancing never moves a
task to a member that cannot run one of its models. A role with members but
none whose models allow the task's gives a health warning at plan time and a
waiting reason at run time.

Before the hand-out Zerus switches the member's model and effort with
`hgs settings NAME --json {"model":"...","effort":"..."}` (Claude, Codex and
DeepSeek; Kimi and OpenCode cannot switch) and waits until the session record
shows the new model, up to 60 seconds. A refusal, or no confirmation, leaves
the task waiting with `waits`
(`{"for":"zerus","on":["model"],"reason":"...","at":...}`) and one inbox item;
`hgs team retry` or a reassignment clears it, and a waiting task is never
retried on its own. The steps Zerus took are recorded on the attempt's
`prepare` (`[{step, from, to, at, result, reason}]` in the order branch, model,
context; `result` is `done`, `skipped`, `refused` or `uncertain`).

`context_before_task` on a role (copied into the member when it joins, and
overridable per member in the team settings) is `none` (default), `compact` or
`clear`. Before each hand-out, a stage's included, Zerus runs
`hgs compact-context` or `hgs clear-context` on the idle member session and
waits for its confirmation; a clear starts a new conversation and the member's
`SessionRef` follows it, so the task text must not say "as before". An agent
without the action (OpenCode) gets `none` with one feed note; a refused action
leaves the task waiting, an unconfirmed one is `uncertain` and is never retried.
While the action runs the task's `waits` is `{"for":"zerus","on":["context"],
"state":"waiting",...}` with the action's request, so a restarted worker reads
the same request instead of running it again; it is not an inbox item until it
fails.

Task 209 (owner, 2026-11-07): every member's native context pressure is also
the rule. Zerus reads it with the session's own `hgs inspect`
(`session_usage.context`, built from DeepSeek's `contextPressure` for dsh),
keeps the last reading on the member (`context: {used, limit, percent, at}`,
shown on the member card), and at `context_compact_percent` (a team setting,
95 by default, set by the owner) compacts the context before the next hand-out
and when the member idles mid-task: the native compact, or for an agent
without one a fresh conversation with the handoff brief, and then the same
attempt continues (`context_compact_started`, `context_compacted`,
`context_compact_failed`; status shows the member's `state` as `compacting`
while the request is confirmed). A report, or a finished turn, that says
there is no context left while the meter is below the threshold is not a
report: Zerus answers the member with the actual figure
(`context_claim_refused`) and the attempt stays open; at or above the
threshold the claim starts the compaction instead. Without a reading a claim
cannot be disproved and is not refused. A relaunch gives a fresh conversation
with the handoff brief, never the same conversation, and the lead may propose
one for a stuck member, not only for one whose start failed.

Task 254 (owner, 2026-11-08): a cold prompt cache counts too. A session whose
listing says `session_state` cold (or, from a listing without it, whose cache
TTL passed since it was last active) and whose context holds at least
`cold_compact_percent` of its window (a team setting, 30 by default, 0 never,
set by the owner) is compacted the same way before a hand-out and before a
crashed attempt goes back to it; a warm session or a small context is
delivered at once. A failed compaction for a cold cache still delivers the
task and tells the lead (`context_compact_failed` with reason `cold`); at the
95% threshold a failure still makes the task wait for the lead, as before.
Task #42 (team ede724c4): while that compaction runs (or any compact or clear
Zerus runs for a hand-out: a task's U46 context step, a stage's, the clear
before a changed start prompt), the member's doorbell and urgent interrupt
wait. A DeepSeek host takes any message as consuming the compaction
("cancelled") and an interrupt aborts it; on 2026-11-10 a lead's message rang
20 s into the compaction before task 26 and the task went out on the cold
context. A
crashed attempt is not given back to a paused team, a paused phase, a paused or
draining member or a held task, by the tick or by the sweep after an update; it
waits, unconfirmed, and goes back on the first tick after the pause ends.

The owner's **After a task** policy per member and for the lead (`after_task`
empty keeps context, or `compact`/`clear`; `after_task_on` `percent` or
`tokens`; `after_task_at` the threshold) runs the native action only when the
free member's session record passes it (`context.used` against `context.limit`
for percent, `context.used` for tokens), before its next hand-out. A configured
policy replaces the role's every-task action; below the threshold the member
keeps its context. A session on another computer, or one whose usage is not
known, keeps its context. The lead's policy runs only while the lead is free:
the lead's own task still reaches it without a message (C1), but its context
action waits for a free lead. A mode the agent cannot run, a mode without a
criterion, and a threshold outside 1-100 percent or above 10000000 tokens are
refused by field, in a plan and in the owner's `members_update`.

### When Zerus starts a member

A member's session starts with its first task hand-out, after a swap, or for
the lead of a planning or draft team with its brief. It also starts when the
member has work that would otherwise never reach it: a sub-lead with untold
items in its inbox (its sub-brief and inbox steps follow once the session takes
a message), and the member a waiting stage would go to when no other candidate
is available. A paused, draining or removed member is not started this way, and
neither is one whose start is already in flight or whose last start failed: a
failed start waits for `hgs team relaunch`.

### Stages Zerus runs itself: a command, a land, a branch per task

The owner's catalog may give a preset a `command` stage — a command, its
expected exit code and a time limit — and a final `land` stage:

```json
{"kind":"command","command":{"command":"cargo test --locked",
 "expect_exit":0,"timeout_minutes":30}}
{"kind":"land","exits":{"0":"accepted","2":"returned","3":"held"},
 "command":{"command":"./scripts/land.sh","timeout_minutes":10}}
```

A command or land stage names no role or member: Zerus runs it in the stage's
folder as a detached `hgs run`, with no model, so the session shows Building
and the output is kept in the run log. The folder follows the preset's `folder`
rule exactly as a test's does: `checkout` gives a detached checkout of the
checked attempt's commit, and otherwise the command runs in the task's folder,
which the stage holds from the report on, so the implementer gets no next task
there until the stage ends. The checked commit is the attempt's recorded commit
(`hgs team commit`), else the folder's HEAD as the attempt reported; it is never
the folder's HEAD when the stage starts. A land stage in a Git folder refuses to
run when that commit is missing or the folder's HEAD is another commit (the task
waits for the lead, and the feed says why); in a folder without Git there is no
HEAD to check, and the stage runs in the held folder (a checkout needs Git).
Under the hold rule a review the preset puts before a command or land stage also
holds the implementer's folder from the report on: the later stage checks and
lands the very commit the review reads, even while that earlier review is open.
Under the checkout rule the later stage reads its own detached checkout and
nothing changes. In short, a test under the hold rule holds the folder from the
moment it waits; a review, command or land stage holds it from the report on
when the preset's folder rule is hold; under the checkout rule nothing holds it.
The command gets the task as
`HGS_TASK_ID`, its branch as `HGS_TASK_BRANCH` and the checked commit as
`HGS_TASK_COMMIT`. The exit code is the verdict: the expected one passes; any
other returns the task to the implementer with the last 4000 characters of the
output as the finding, counting toward the preset's rounds. The same output on
the same commit twice in a row is marked `flaky` and goes to the lead; a timeout
returns "timed out after N min". The plan chooses a preset, never a command:
only the owner's catalog writes it.

A stage member that ends its turn without a verdict, with no stage question
open, gives the lead one stalled item while the stage stays its own. A turn
that waits for the lead's answer or for Background work (or for a Zerus build,
such as the tests the reviewer runs) is not stalled: no silent item appears
while that work runs, and the member may still report its verdict. The budget
the member declared (`hgs team expect`) or the block timeout's "waiting N min"
message tells the lead when the wait is too long; it is never reported as a
missing report. An
unconfirmed hand-off is not retried on a timer: the lead hears it stalled only
with evidence that it did not arrive (the member stayed idle and free since
delivery, with no question and no run claim under the stage's id), and the
member's late verdict is still accepted while the stage waits. A stage whose
own run is still going when it goes back to Waiting keeps its folder and
checkout until that run ends; the next member's work never races it.

A `land` stage runs once every earlier stage passed and is the preset's last
stage. Its `exits` map an exit code to `accepted` (Zerus accepts the task,
`decided_by` is Zerus, and the feed names the stage), `returned` (back to the
implementer with the output) or `held` (the task waits for the lead with the
output). The default is 0 accepted and any other held. Because a land stage may
push, it needs the owner's `"land_allowed":true`: without it a plan choosing
the preset is refused, and a team that turns it off later holds the task for
the lead instead of running the stage.

A stage's return or a send-back goes back to the implementer, never to a peer:
the next hand-out names the stage's findings (or the send-back's reason), and
that attempt is not counted as a failed member. Once the preset's rounds run
out, the lead's decision is the last word: a rejection reaches the next
hand-out with its reason, not the returned stage it followed.

A non-final `hgs team reject` defaults to rework: the task keeps its assignee
and the next attempt goes to that member as soon as it can take it. The member
is reserved while the returned task is open, including an undecided report;
no other task or stage is handed or rebalanced to it. Task 160 (owner,
2026-11-07): a held or parked rework reserves nobody, so it never blocks its
member's later hand-outs; released, it reserves the member again. A member with an
open approval, a usage limit or a stopped session keeps the rework without a
time limit. The lead's inbox shows `#N waits for its member, N min`; an explicit
`hgs team reassign` moves it. With `"fresh":true`, reject uses the previous
retry policy, preferring a free member of the same role that did not fail the
task, subject to existing assignee pins. `"hold":true` still holds a returned
task until release, and `"final":true` ends it.
Only an explicit fresh rejection may bypass Output protection while the failed
member has other work. Automatic refusals preserve shared/missing Output
protection; a fresh rejection's exception ends when its next attempt is recorded.
A draining member may finish its own reserved rework before leaving. Explicit
reassignment clears the rework reservation and its wait age. Returning an
unstarted phase to a draft also clears rework reservations. Member-wait inbox
items exclude held rework and members available to take it.

Task 173 (owner, 2026-11-07): no shadow work. Every piece of a member's work
belongs to the attempt (or stage run) handed to it.
- A lead's message to a member that names a task (`#N`, `task N`, `for N:`
  for one of the team's tasks) is bound to that task's attempt when the task is
  handed to that member (the attempt's `lead_messages`, at most 20, with a
  `lead_message` event). Task #30 (2026-11-10): a task the text only mentions
  and the member does not hold is a reference: the message goes, its answer
  names it (`references`, `reference_note`). Only a message whose `"task"`
  field names a task the member does not hold is refused, naming the task's
  state and the hand-out to use (`hgs team retry`, `hgs team reassign`). This
  holds for `hgs send` from the lead's own session to a member's session and
  for the lead's `hgs team message`. A message naming no task goes through; it
  carries no work. Answers and decisions keep
  their own commands (`hgs team reply`, `accept`, `reject`, `hold`); the
  owner's messages and Zerus's own sends are never checked.
- A team member's `hgs run` needs its open attempt or stage run; it may name
  it in `"attempt"`, and one naming another attempt is refused. Without one:
  "No task is handed to you in team NAME: ask the lead". The run's claim
  records `team`, `task` and `attempt`. A sandboxed member's run is checked by
  the worker the same way, and `hgs team run`'s refusal says the same. The
  lead's own runs are not checked.
- A template run (`hgs team run`) is the build of the member that asked for
  it. Its claim in the member's folder names that member and attempt. Members
  that may not have worktrees work in one folder and its claims lie side by
  side, so a run there shows only its own member as Building, records
  `job_started` and `job_ended` for that member alone, never flags another
  member of the folder as unassigned work and never wakes another member's
  attempt with its result (`run_settled_woken`). A claim that names nobody,
  written by an older Zerus, counts only in a folder one member works in.
- A submitted report's receipt lists what the member made for the attempt and
  left in place (`leftovers`): extra build folders, worktrees it added and
  large run temporary folders. A worktree counts only when it is a linked one
  that appeared since the hand-out: the repository's main checkout, the
  checkout the member works in (a team's folder may be a subfolder of its
  repository) and the other members' folders are never listed.
- A member session building with nothing handed to it, or working past two
  minutes in a turn that began after its last attempt ended, is flagged at
  once: the member's `unassigned` ({since, what}), an `unassigned_work` event
  and lead inbox item with what it runs, `hgs team look` state
  `unassigned_work` (never idle) and its next action. The flag clears
  (`unassigned_work_ended`) when the member is free again or gets a hand-out.

Task 160 (owner, 2026-11-07): a task blocked on an outside event parks instead
of keeping its member in a waiting loop. `hgs team park --json` (`{team, task,
attempt, condition, eta_minutes, note}`) by the task's member, its reviewer or
you parks an open attempt with no report or open question. The condition is
one of:
- `{"kind":"ci","repo":"OWNER/REPO","run":N}`: a GitHub Actions run finishing;
- `{"kind":"build","version":"1.58.0","build":N,"commit":"SHA","program":"PATH"}`:
  that build installed (at least the version, at least the Dev number, that
  commit; `program` is an installed desktop answering `--build-info`, else the
  hgs the team worker runs);
- `{"kind":"task","task":N}`: another task accepted;
- `{"kind":"card","proposal":"pN"}`: the owner's answer on a card;
- `{"kind":"time","at":UNIX_SECONDS}`.

The estimate is 1 to 10080 minutes. The task becomes Pending with a `park`
(`attempt`, `member`, `by`, `at`, `condition`, `eta`, `note`, `state`
waiting/met/failed, `detail`) and its attempt `parked`, which is not open: the
member is free at once and takes its next ready task, and a
parked task is never ready, never rebalanced and reserves nobody.

Zerus checks the condition on a thread of the team worker, never the tick:
task and card states and the clock on every 30-second pass, the installed
build at most once a minute (a 15-second bounded `--version` or
`--build-info`), a CI run through `api.github.com` at most every two minutes
(`GITHUB_TOKEN` or `GH_TOKEN` for a private fork). Only an outcome writes the
team (`park_met` or `park_failed`). A met park goes back to the same member at
its next free turn, before any new task, in the same attempt (`resumed`), with
a message naming the condition, what held and the member's note. A failed one
(the run failed, the card was denied or withdrawn, the task was cancelled or
rejected) is a `park_failed` lead inbox item with the reason, and one well past
its estimate a `park_overdue` item. `hgs team unpark --json` (`{team, task,
reason}`) gives it back early; a report on the parked attempt gives it back and
reports; a cancel, reassign or take-over ends the park and its attempt.

Status shows `parked` on the task ("parked until <condition>, ETA HH:MM (in N
min)") and on its member; an idle member's session waits for it ("Waiting for
#N: parked until ...", reason kind `team_parked`), never Building.

Task 159 (owner, 2026-11-07): owner-only steps are planned for when the owner
is there, in one click.
- A lead's card may name the tasks that need it: `"for_tasks":[N]` (open
  tasks, at most 50; part of the card's hash). While the card is open those
  tasks are never ready, so no member is handed a task it would wait on; an
  idle member's session says "Waiting for the owner's card pN for #M" (reason
  kind `team_owner_card`). Allowed or answered, they are ready; denied, they are
  held for the lead with "you denied card pN: REASON".
- `hgs team waiting` marks the owner's cards (`owner_word_asked`) by presence:
  while the desktop reports the owner away (locked, or idle 10 minutes) each
  is `"notify":false, "held_for_presence":true`; otherwise `"notify":true` and
  every card listed together shares one `"batch"` (with `"batch_size"`). Each
  card names what waits on it (`waiting_tasks`, and "(#N wait for it)" in its
  title). Without a desktop's presence nothing is held. The desktop posts a
  batch as one notice and withdraws it with its last card.
- Task #246 (owner, 2026-11-08): every Waiting row can be dismissed. `hgs team
  waiting --json` gives each item a `state`: a drive's threshold band, a paused
  team's `paused_at`, a plan or phase's team revision, a tool offer's state, a
  deferred request's id and time, and so on. `{"action":"dismiss","id","team",
  "state"}` (owner only; `by":"human"` from a shell or the desktop) hides the
  row and takes it out of `items`/`total`, the badge and the notifications
  until that state changes; then the row returns by itself. `{"action":
  "undismiss","id"}` (Show dismissed > Restore) brings it back at once. A
  decision that blocks work (phase or plan approval, a proposal or question, a
  permission) stays in the `dismissed` section with `blocking:true` and
  `blocks` naming the team or member it still blocks; the other dismissed items
  just rest there. `dismissed`/`dismissed_total` come beside the live page.
- Task #286 (owner, 2026-11-08): a task's wait for the lead or the merger is
  not in the owner's Waiting; it is a `task_waits` item in the lead's inbox.
  Only when the lead leaves it unresolved for `Settings.owner_escalate_minutes`
  (60 by default, 0 never, only the owner changes it) does the owner see one
  `task_wait_escalated` item naming the lead and the task: it escalates once,
  however long it stays. A merge-conflict wait (a predecessor's accepted commit
  that conflicts in a folder) never reaches the owner: it is the lead's or the
  merger's resolve task (#267). The lead-changes digest is one `lead_changes`
  row, with `groups` holding the counts per kind (added, moved, cancelled,
  re-issued) and the newest few of each; Seen clears it.
- One owner rule for scratch teams, granted once by card
  (`{"kind":"scratch-rule","folder":"ABS","agents":["claude","codex"],"reason":"..."}`
  | `hgs team propose --json`, or the owner's own `hgs team scratch-rule
  --json`): a team that a running team's lead creates from its own session
  with its folder inside that folder starts with approval `auto` (no per-plan
  approval; `scratch_rule` names the rule, `scratch_rule_applied` in its feed).
  Before each local member launches, its folder is trusted for the rule's
  Claude and Codex agents in the account config that launch reads
  (`.claude.json`, Codex's `config.toml` `[projects."FOLDER"]`); Kimi,
  DeepSeek and OpenCode still ask at start-up. The rules live in the teams
  settings file (Settings > Teams): `hgs team scratch-rules --json` lists
  them, and `{"action":"revoke","id":"sXXXXXXXX","by":"human"}` revokes one;
  teams created under it keep what they were given.
- Task #228 (owner, 2026-11-07, on task 170): a QA member runs live checks on
  its own scratch teams. Each team has a scratch root, `scratch_root` or
  `<team folder>/.qa-scratch` (only the owner changes it in Settings > Teams);
  the owner grants **one standing scratch rule per team** at that root, for the
  agents of the team's roster: `{"standing":true,"by":"human"} | hgs team
  scratch-rule --json` (or the lead's existing `scratch-rule` card naming the
  team, whose folder is the root). A re-grant for the same team replaces its
  rule, so a team never has two. A member with the QA right (task 154's
  `operate` profile, which launches sessions) creates a scratch team from its
  own session: `hgs team create --json` with a folder inside the root. It
  becomes that scratch team's lead, its plan needs no card (`approval` and
  `phase_approval` are `auto`), the root is trusted at member launch for
  Claude and Codex as before, and the parent's `max_members`, `tree_depth` and
  `tree_sessions` cap the new team; its members use the agents of the parent's
  roster and their folders stay inside the root. A create outside the root,
  without the QA right, or before the standing rule is granted is refused;
  `scratch_parent` names the parent team, the creating task and the member.
  - Task #231 (owner, 2026-11-08, on task 170's F2): a scratch team's members
  get their own folders like a normal team. When the scratch team's folder is
  a git repository (or inside one), `Create team` gives each member a worktree
  or clone of it, cut as a sibling of the scratch team's folder - inside the
  rule's folder, trusted by the same rule - and the `git-commit` template,
  member branches, the predecessor's commit for a dependent task and the merge
  queue work there. A lead's scratch team (rule 159) and a QA member's both
  get this. A scratch folder that is not a git repository keeps today's shared
  team folder, and the commit refusal says why: make the scratch folder a git
  repository so each member gets its own folder.
  The parent lead sees each scratch team as `scratch_team_created` and
  `scratch_teams_stopped` in its feed and a `scratch_team` inbox item
  (created, stopped, or listed). When the creating task is accepted, rejected
  or cancelled, Zerus stops its scratch teams and terminates their sessions
  except the creating member's own (that session is the parent team's work);
  a session that cannot be stopped is listed for the owner in the inbox and
  is never reported stopped.

Task 161 (owner, 2026-11-07): neither the lead nor a member turns a task into a
waiting loop; waiting on an outside event is always a park.
- The lead's `hgs team reply` (and a parent lead's), `hgs team message` to
  members and `hgs send` from the lead's session to a member are refused when
  the text tells the member to wait for an outside event: a wait word with an
  install, a build or version, CI, a card or approval, a time. The refusal
  is the park request for the member's open task. A reply with
  `"park":{condition, eta_minutes, note}` answers the question and parks the
  task at once; the lead's inbox item for a question that says the member is
  blocked or waiting carries that reply, with a guessed condition.
  Task #276 (owner, 2026-11-08): the detector judges the *instruction*, not a
  wait word beside an event word, one clause at a time: "held until #5 is
  accepted" or "the owner allows card p77" describe state and pass, while an
  imperative to the member ("wait for the owner to answer", "check again in 5
  minutes", "poll the CI until it is green") is refused, quotes the phrase that
  triggered it, and offers the park of the condition that phrase names.
- `hgs run` refuses a job whose purpose is waiting: its label says it waits
  ("wait", "poll", "watch for", "until"), or its command (and the .cmd, .bat,
  .ps1, .sh or .py script it names, first 64 KB) loops (`while`, `until`,
  `for /l`, `goto`, `loop`) around a pause (`sleep`, `Start-Sleep`,
  `timeout /t`, `ping -n`). The refusal names the caller's park request.
- A turn that ends without a report and whose last reply says it waits or is
  blocked on an outside event gets one message offering the park with a
  guessed condition (`park_offered`); it is never kept as a wait.
- Every five minutes, from ten minutes after its hand-out, a member's open,
  delivered attempt with no report or open question is looked at
  (`wait_checked_at`): a running job of its session whose purpose is waiting,
  or the same read-only check (a version, a status, a CI run, a listing) its
  turn ran three or more times at least a minute apart, parks the task on a
  guessed condition (a CI run named, else an installed version named, else a
  time 30 minutes on) with a 60-minute estimate, by "Zerus (a wait loop)"
  (`wait_parked`). The member is told to stop the wait and end its turn; the
  lead is told once with the guessed condition to correct. Zerus does not stop
  the job itself; it ends at its own bound.
  Task #413: a command that changes the repository (a merge, a rebase, a
  checkout, a reset, a cherry-pick, a pull, a push) is work, never a poll,
  even when it also carries read-only words; a repeated check is a loop only
  while the state it reads stands - the member's folder HEAD is recorded
  (`wait_head`), a first observation never parks, and a HEAD that moved since
  the last check is a fast-forward left alone; a member with a background
  monitor or job running is never parked on a repeated command.
- The lead gets every park's outcome without watching: a met park is told
  when the task goes back to its member; a failed or overdue one is a lead
  inbox item.

Task 156 (owner, 2026-11-07): the owner's decisions are records that reach
the right work by relevance.
- Every owner answer to a card (allowed, denied, or a question answered) and
  every `hgs team decide --json` (`{team, question, choice, why, spec?, files?,
  tasks?, forbids?, supersedes?}` by the owner, or by the lead with
  `owner_word` quoting the owner's own words in the lead's chat) is recorded at
  once in the team's `decisions` (id `dN`, question, choice, why, by, at,
  card, spec, files, tasks, forbids), journaled `decision_recorded`. A card
  carries the same refs (`spec`, `files`, `binds`, `forbids`). A later
  decision on the same question (the same content words, or the one it names
  in `supersedes`) supersedes it: the old one stays with `superseded_by`. The
  records live in Zerus state for every memory mode; a team with a lore
  workspace also gets them as lore decision records (`t<team8>-dN`, superseded
  ones marked).
- A current decision binds a task that it names, whose outputs a file or spec
  path of it touches, that lore's `find` returns it for (a background pass of
  the team writer, ten tasks at a time, cached per task), or whose title,
  description and criteria share its subject (two or more content words, a
  third of the smaller set). Every hand-out carries "Decisions that bind this
  task" with their ids; a task in flight that a newer decision binds gets one
  message (`decision_told`). `hgs team decisions --json` lists them with the
  open tasks each binds.
- A report is checked against the decisions that bind its task (and those
  binding everything): a term a decision forbids, as a whole word, in the
  report or in the lines its commits add against the lead branch is flagged
  (`decision_flags`, a `decision_conflict` event and lead inbox item); accept
  is refused until the report is fixed or accepted with
  `"against_decisions":true`.
- A decision with a `spec` makes that spec a view: its Decisions section
  between `<!-- zerus:decisions:begin -->` and `<!-- zerus:decisions:end -->`
  is generated from the records (superseded ones struck through) and
  committed in the lead branch's checkout when it is clean
  (`decision_specs_written`); `{"action":"spec","spec":"PATH"}` shows it.
- The lead's message to a member that hands down an owner decision ("the
  owner decided ...") is refused by `hgs send` and `hgs team message`, naming
  `hgs team decide`.

Task 279 (owner, 2026-11-08): the owner's standing rules are kept with their
words and source, so the lead records them at any age.
- Every owner message of the lead's session is indexed already: it is a
  `UserPromptSubmit` event of the lead's conversation in the events journal
  (content search and lore history read it read-only).
  `hgs team decisions --json {"team":"ID","action":"owner-messages","limit":40}`
  lists them newest first with `id` (the journal's event id), `at` and `text`.
- `hgs team decide --json` takes `owner_message: {"id":N,"quote":"..."}` for a
  lead caller instead of `owner_word`: the message may be any age, Zerus checks
  it is the owner's own `UserPromptSubmit` in the lead's conversation (never an
  injected policy, a team message or a scheduled turn) and that the quote is in
  it (case and spacing ignored); a wrong quote, a quote from another message or
  an unknown id is refused with the message it names. The decision records
  `owner_message` (id, quote, at), and `by` says "the lead on your message N".
  The 30-minute `owner_word` path stays for a fresh sentence.
- `"kind":"standing rule"` (or `"rule"`) makes it a standing rule: a decision
  kind of its own, recalled in every lead brief ("Your standing rules ...") and
  in the hand-out of every task it binds, each line carrying its source
  (`From your message N: "..."`). It is superseded like any decision, and with a
  `spec` it reaches the project's lore section too.

A reassign, a take-over or a cancel of a task whose attempt its member is still
working on (sent or delivered, not reported) tells that member at once: one
`[Zerus team · from: Zerus · task: #N · stop]` message names the task and why it
left, asks it to stop the builds, tests and commands it started for it, not to
report it and to commit nothing more for it (`stop_told`). It is sent as soon as the
session runs, also while it is busy: an agent that reads messages mid-turn stops
at once, others when the turn ends, and the member's next task then goes out
without a message from the lead. With the owner's team setting
`interrupt_revoked` (off by default; the lead cannot change it) a busy turn is
interrupted once before the message (`revoked_interrupted`). A reported
attempt, or a reassign to the same member, sends nothing.

Tasks may declare `"output":["src/api.rs","src/protocol/"]`: repository-relative
files or directories, with a directory covering all paths beneath it. Plan,
extend, add-to-phase and edit-draft `tasks_update` normalize separators, remove
redundant `.` components and duplicate paths, and retain this field in task
status. A task has at most 128 paths, each at most 1024 characters and together
at most 32 KiB. Absolute paths, parent (`..`) components, colons and control
characters are refused; `.` covers the repository. Existing tasks without
output remain valid. Task 259 (owner, 2026-11-08): a path is a file or a module,
not a whole area. A bare top-level folder (`src`, `tray`, `client`) or a whole
area directly under a top-level container (`client/src`, `crates/replica`,
`mobile/android`) is a folder-level output: it is advisory and never holds
another task, since it would hold every task touching that area while it is
open. Plan validation warns once per task and asks for the files or module
paths; an overlap that only these folder-level paths cause suggests a soft
order instead of a hard dependency. Rebalance keeps a queued task with its
original member only when a file or module path overlaps that member's task in
progress (equal paths or containment by path components); it also keeps a task
without output whenever that member has work in progress. `src/api` does not
overlap `src/apis`.

Task 259 (owner, 2026-11-08): no idle member while a ready task with free
outputs waits. Zerus hands such a task to a free member of a compatible role
when its assignee cannot take it: the same role, or one doing the same kind of
work (Frontend SolidJS and Frontend DeepSeek share "Frontend"), and only when
that member's launch rights allow the task. The exact role and the planned
member order the choice, never wall off a compatible one; a member Zerus has not
started yet keeps its task. A pinned task, `keep_assignees` or a rework stays
with its member; while an idle member could take one of those, the lead's inbox
carries one `idle_with_ready_work` item naming the member, the task and the
reassign command. Every move records a `rebalanced` event with its `cross_role`
flag, and the lead gets one message that `hgs team reassign` reverts.

Task 263 (owner, 2026-11-08): a ready task waiting for its busy member never
holds back other members' ready work. The dispatcher claims a task's files for
a tick only when that tick hands it out or starts its member for it; a task
that merely waits for a busy member no longer blocks a later ready task writing
the same files (once it can start, that later task's work holds it back
instead, so the two still never run apart). A real hold - a task that runs, or
a returned task waiting for its member - is named rather than silent:
`waits` on the held task is `{"for":"outputs","on":[198],"unblock":"#198 waits
for ui2 (Frontend): hold or reassign #198"}`, and `hgs team look` says which
task holds the member's ready work and what frees it. When a member is idle
only because such a hold keeps its ready work, the lead's inbox carries one
`idle_with_ready_work` item with that reason (the hold command for a waiting
holder, `hgs team look` for a running one).

A stage member may ask the lead with `hgs team ask` (its question belongs to the
stage run and the lead answers it with `hgs team reply`), and the stage hand-off
names that route, `hgs team current` and the rule that a reply in its turn is
not a verdict. A stage given to a member that was removed or paused, or whose
hand-off was never confirmed, goes back to `Waiting` for a free member, and the
lead gets a stalled-stage item.

`Settings.branch_per_task` gives each task its own branch: `{"template":
"task/{id}"}` fills `{id}`, `{member}` and `{team8}` and must be a valid Git
branch name; `base_branch` (default the lead's branch) is what the branch
starts from. Before each hand-out Zerus refuses a folder with uncommitted
changes (the task waits "folder has changes"), then creates the branch in the
member's folder from the base or checks it out; a returned or reopened task
gets its existing branch. Branches are never deleted. Without `branch_per_task`
a member keeps one branch, as before.
A relaunch or a swap takes the member's own folder while it is on the task
branch of a task that member holds or worked on (DCB-svn report, 2026-11-12):
the next hand-out picks the task's branch itself. A folder on another member's
task branch is still refused.

Tasks that write the same files never run apart (owner, 2026-11-07). Two open
tasks whose `output` paths overlap (one path contains the other, by
components) must be chained: unless one waits for the other's acceptance
through hard dependencies (or `all_hard`), plan validation reports an
`outputs` error naming both tasks and the shared paths, so the plan does not
run and an `extend` or `add-to-phase` with such a task is refused. The
dispatcher enforces the same rule team-wide, whatever the plan says: a task
waits while an open task with an overlapping output has reached a member and is
not decided (assigned, in progress, reported, in a stage, or sent back), and of
two ready ones only the first in dispatch order starts; it never goes to
another member to run beside it. Two returned tasks never wait on each other:
the lower number goes first. A task without `output` is not held by this rule.

Every view names such a wait (task 295, owner, 2026-11-08: ui4 sat idle while
its ready #226 waited for #219 and then #225, and the session state, status and
look all said it waited for the lead to hand #226 out). It names the task that
holds the files, the ones that run and the ready ones before it, lowest first
(`plan::output_queue`):

- The member's session state is `deps` with kind `team_output`: "Waiting for #225
  <title>: it writes the same files as #226", with `holders`.
- The task's status `waits` is `{"for":"deps","on":[225],"unblock":"finish #225"}`.
- Look's next action is "Its task #226 waits for #225, which writes the same
  files; Zerus hands it out when #225 ends".

A ready task whose hand-out step was refused or never confirmed (an accepted
dependency's merge conflicted in the member's folder, a context action) is
named the same way. The session state has kind `team_task_waits`: "Waiting for
Lead to retry or reassign #N: <reason>", or "Waiting for Zerus to prepare #N"
while it runs. Look gives that reason too. Only a ready task nothing else holds
waits for the lead's hand-out, and the dispatcher hands it out at its next
tick. The lead may propose a relaunch for any member, also an idle one that
never takes its task. The owner's click runs the relaunch, a fresh
conversation with the handoff brief, without asking whether the session failed
to start.

Before a hand-out, after its task branch, Zerus merges the accepted work of the
task's dependencies (direct or through another dependency) into the member's
branch: for each accepted task, the commit its accepted report named
(`folder_head`), fetched from its author's folder when the member's folder
lacks it, oldest task first, skipped when the branch holds it already. The
attempt records a `dependencies` prepare step and the hand-out says what was
merged. A folder with uncommitted changes, a commit that cannot be fetched or a
conflicting merge is never left to the member: a conflict is aborted, the
folder put back, and the task waits for the lead with the conflicting files and
`hgs team retry`. The lead's own task and a member working in the team's own
folder are left alone; a member on another computer gets a `skipped` step.

Team messages are pulled, not pasted (task 134; owner, 2026-11-07). The text
of a hand-out, a stage hand-out, the lead's (or a sub-lead's) inbox message
and the starved notice is stored first in the working tier: the
`working_messages` table of `teams.sqlite3` (docs/lore.md 11.5, the Zerus-state
form), with `queued`, `rung` and `read`, a read message expiring 7 days after
its read and an unread one 30 days after it was written (a
`working_unread_expired` event), cleaned at most 500 rows a pass. The terminal
then receives only a one-line doorbell under the message's own request id
(the hand-out's attempt id, the stage run id), so receipts and request
identities are as before. The doorbell (task 186, docs/lore.md 11.11) is at
most 200 characters: who, `normal` or `URGENT`, the newest message's id, the
count when several wait, and where to read them, for example
`[Zerus team · from: Lead · task: #N · attempt: K · normal · w-message-1a2b3c4d]
read it with hgs team inbox` or `[Zerus team · message · from zerus · URGENT ·
w-message-5e6f7a8b · 3 messages] read it with hgs team inbox`; a long header is
shortened, never the id or the command. A doorbell that never submits loses nothing: the text is
stored, and a refused doorbell withdraws its message, so the retry's message is
the only copy. A message that cannot be stored is a refused send.

`hgs team inbox --json` (the team may be left out in a member's session or with
`attempt`) answers the lead's or sub-lead's items as before and the caller's
own unread `messages` with their text, and marks them read; `hgs team current
--json` answers the attempt with the same `messages`. These two are the only
read receipts: `read` names the ids, the request id and `via`, the message keeps
`read_at`, `read_via` and `read_request`, and the journal gets `message_read`.
`"peek":true` marks nothing, and anyone but the recipient (you on the desktop)
only peeks. Task 186 (docs/lore.md 11.6): only you peek at the lead's messages.
A call with `team` from a caller Zerus cannot verify (a session outside the
team, a stale session marker) is refused, never read as yours; a parent lead or
an intake key reads no team's messages. In a sandbox the receipt waits in the member's folder for the
worker (`messages-read`), as a report does. Task 277 (owner, 2026-11-08) adds
one more receipt: a hand-out or stage message whose text reached the member's
turn (`injected_at`) for an attempt it holds is stale, not unread, and the
worker marks it read (`read_via: taken`), so "does not read" counts only
messages that never reached it. Nothing else marks a message read.

Delivery does not depend on the agent's discipline. A Claude or Codex member's
turn-start hook (`UserPromptSubmit`) gets the unread text Zerus has not put into
a turn yet as `additionalContext`, each message once (`injected_at`, at most
24 KiB a turn), without marking it read; the doorbell's own turn is the first
such turn. An agent without that hook (Kimi, DeepSeek, OpenCode) gets the text
after the doorbell line in the same paste. A doorbell rings only at an idle
moment, never into a busy composer (docs/lore.md hard rule 3); a message posted
while the member works waits for its next safe point (task 186):
- **Tool boundary:** a Claude session's `PostToolUse` hook returns its urgent
  unread messages as `additionalContext` (Codex and Kimi have no such hook
  yet). Until one waits, the hook only checks one file
  (`teams/urgent/<key>`), so a tool call never opens the team store.
- **Turn start:** the turn-start hook returns every unread message (above).
- **Turn end:** the dispatcher rings when the turn ends; the message records
  `safe_point` `turn_end` (`idle` for one rung when the member was already free,
  `hook` for one a hook delivered).

Each message rings by its own schedule, not the oldest's. An urgent one rings
at the first safe point. Normal ones ring at most once per recipient per 5
minutes, and the doorbell names the count. A message rung once, or put into a
turn by a hook, rings once more after 20 minutes, or an urgent one after the
team's `urgent_bound_minutes` (default 10, 1 to 120, a setting the lead may
change). After two rings and the same time again, the lead gets an
`unread_messages` inbox item with the member's state and session. An urgent
message that reaches no safe point within `urgent_bound_minutes` is an item at
once, and the lead may stop the member natively so it rings at the turn end.
For an urgent message to the lead, you get the Waiting item instead
(`lead_message_undelivered`, with Open Terminal): Zerus never stops the lead.
Hand-outs, stage hand-outs and the lead's inbox message ring at once (the
inbox keeps its own `inbox_interval_seconds`).

The lead's inbox message reaches a lead in its own turn the same way: for a
Claude or Codex lead it is stored at once (`InboxWaits`, journal
`message_waits`) and never typed. A Claude lead takes an urgent one at its next
tool boundary; both take every one at their next turn start, and the doorbell
rings at the turn end. A Kimi lead hears when it is free, from a fresh list.
This replaces the owner's 2026-10-28 paste into a working lead.

Status shows each member's `unread`: `count`, `urgent`, the oldest message and
its rings, and the schedule `ring_at`, `stall_at` and `held_urgent_at`. Zerus
Next's member row shows `unread: N`.

Task 277 (owner, 2026-11-08): an urgent message does not wait for the turn end.
While the member works, Zerus interrupts its turn natively through `hgs
interrupt` (Claude, Codex and Kimi stop; DeepSeek aborts; OpenCode stops),
pinned to the displayed turn and its run and conversation, then waits for the
stop and opens a new turn that carries the message and `Continue your task from
where you stopped.`; the attempt goes on untouched. Claude and Codex get the
text from the turn hook, the others in the prompt. A member already handed a
task, a stage or a resume this tick gets that delivery first, and a question or
approval is a safe point of its own. An urgent message unread or not acted on
after `urgent_alarm_minutes` (team setting, default 3, 0 off) is an alarm: the
lead's inbox gets one urgent `urgent_unread` item naming the member, the sender
and why (its turn was interrupted and the message waits for the stop, the new
turn delivered it but it was not acted on, Zerus could not interrupt its turn,
or it waits for the turn end); for a message you sent, your Waiting list gets
the same item (`urgent_message`). Ordinary unread notices stay folded into the
one `unread_messages` item. The lead's inbox message is urgent when it names a
question, a permission, a Terminal question, a crash, a blocked input, a stuck
member or an urgent unread message.

The lead's inbox message is urgent when it names a question, a permission, a
Terminal question, a crash, a blocked input, a stuck member or an urgent unread
message (task 277). `hgs team message
--json` with `{"request_id","to","text","urgent"}` (`to`: a member id, `lead`
or `team`, one message per other member) stores a note from the lead, a member
or you (`by`: the member id, or `owner`) of at most 2,000 characters, and Zerus
rings it at the recipient's next idle moment; an urgent one interrupts a busy
member's turn as above. A recipient with 50 unread normal
or 5 unread urgent messages takes no more of that kind; the sender is refused
with the count. Zerus's own messages are at most 32 KiB each. A RustCodeLore
team keeps its team messages here too: hgs does not link the lore crate, and a
message must not wait for a store on another computer.

#### Decisions and notices (task 289)

Owner, 2026-11-08: the lead's inbox has two kinds of item. A **decision** is
something only the lead can settle: a report, a question, a permission, a task
wait, a card to propose. A **notice** is information: a decided proposal
(`proposal_decided`), a task whose dependency was cancelled
(`dependency_cancelled`), a member that does not read (`unread_messages`) and the
lead-changes digest (`lead_changes`). Every item says `"notice": true|false`.

A notice carries a `seen` command and is hidden by
`{"request_id":"UUID","team":"ID","items":["proposal:p77"]} | hgs team seen
--json`, which stores the notice's content signature with the team (at most 300,
across restarts). The notice returns by itself when its content changes (a new
decision note, another cancelled dependency, another unread count, a newer
digest); a decision is never marked seen - it is answered. Unknown or
decision keys come back in `unknown` and mark nothing.

Task 29 (owner, 2026-11-10): every item in `hgs team inbox --json` carries its
`key`, and every informational item closes the same way and carries the `seen`
command (`"notice": true`): the notices above, test results (`test_result`),
playbook items (`playbook`), records lore cannot write yet (`lore_missing`),
accepted work waiting to land (`waiting_to_land`), refused queued calls
(`refused_queued_call`), idle notices (`idle_with_ready_work`), recovered
attempts (`attempt_recovered`) and any item whose command starts with "no
action". A decision (a report, question, permission, a card or anything else
that waits for an accept, reject or answer) is never closed and clears only by
being decided; `seen` lists such keys in `unknown` and names each in
`decisions` with its command. The signature leaves out times: an item that
only grew older (a playbook item dated now, "#1 (26 min)") stays closed.

Informational items also clear by themselves: a task's test results once it
is accepted and landed, and every test result after 12 hours; a playbook item
as soon as its rule no longer breaks; a `lore_missing` record once its task
has a commit (a member's own commit counts: the accepted attempt's reported
folder head, the commit the merge queue lands); `waiting_to_land` once the
work landed.

`dependency_cancelled` offers `keep` next to hold and cancel:
`{"request_id":"UUID","team":"ID","task":N} | hgs team keep --json` (or
`"deps":[ids]` for some of them) records that the task does not need the
cancelled work, so the notice clears; the dependencies stay on the plan. The
idle-lead nag counts and names decisions only, so a notice can never keep it
alive ("You are idle while 1 decision(s) wait").

### Scaling and draining the roster

`hgs team scale --json {"request_id":"<uuid>","team":"<id>","role":"Backend",
"agent":"codex","model":"gpt-5","effort":"high","account":"work","count":2}`
adds `count` members of that role at once, as `add-member` adds one: their ids
are the first free of the role's words (`backend`, `backend2`, ...), and the
model, effort and account are copied. The whole scale is refused when it does
not fit `max_members` or the tree's session budget, and scale never raises the
cap. Only the owner, or the lead on the owner's word naming the role and the
count, may scale.

`hgs team drain --json {"request_id":"<uuid>","team":"<id>","member":"impl1"}`
lets the member finish its open attempt and stage run and gives it nothing new
(`member.draining: true`; its other ready tasks go to a same-role peer). When
it owes nothing, Zerus pauses its session (never a kill: the conversation stays
under Saved) and removes it, and the feed records both. A drain is undone
before it ends with the same command and `"draining":false`. The owner, or the
lead on the owner's remove word, may drain; the lead cannot drain itself, and a
sub-lead's reports move first.

### An intake key for urgent tasks

```sh
{"request_id":"<uuid>","team":"<id>","label":"health watch","by":"human"} | hgs team intake-key --json
{"key":"<the key shown once>","team":"<id>","tasks":[{"title":"Fix the outage","assignee":"backend"}]} | hgs team intake --json
```

The owner creates a key for one team; Zerus stores only its SHA-256 hash and
shows the key once, in the created receipt (a replay returns the receipt
without it). A process presenting the key may add one urgent phase: at most
five tasks, approved at once, without a barrier, titled `Urgent: <label>`,
handed out before the other ready tasks, with the key's label in the feed. The
key grants nothing else: any other operation presenting a key is refused, and
so is an unknown or revoked key or another team's. A team keeps at most eight
keys; the owner revokes one by id (`"revoke":"i1"`). Without a key only the
owner and the lead change a plan, as before.

### A member's turn that ended on a provider error

DCB-svn report (2026-11-12): a member whose live session ended its turn on a
provider error other than a usage limit (the listing's `phase` `error` with a
`provider_error`) is stored on the member (`turn_failed`: when, the error's
words and its recovery class) and is a `member_turn_failed` inbox item from the
first tick (`member_turn_failed` in the feed, `member_turn_recovered` when it is
over). Its state reason names the error. After a temporary error (recovery
class `network`, `service` or `rate_limit`; DeepSeek's "transport failed" is
`network`) a member holding work is asked once to continue that task, a minute
after the error (five after a rate limit), at most three times an hour, unless
the shared recovery service already resumes the session
(`failure_continued`). A permanent error waits for the lead or you.

### Restarting a crashed member

`Settings.restart_crashed` (on by default, only the owner changes it) launches a
member's session again into the same folder and conversation when its process
exits on its own: not a pause, an archive, a usage limit, a swap, or - for a
member with nothing in its hands - a session Zerus has not seen running since
its last restart. Task #334 (owner, 2026-11-08): an exit that leaves the member
holding or handed a task (an open attempt, or a stage delivered to it)
relaunches it even when no tick saw the session live, as an install that
restarted the agents while their members were paused did. `hgs team
resume-member` also launches an exited agent's conversation again on the next
tick, so a member the owner or the lead resumed comes back without a relaunch
card. An exit you type yourself in the member's terminal (for example `/exit`)
counts as such an exit: turn the setting off before stopping a member by hand,
or it is launched again. It happens at most three times an hour per member
(`member.restarts`, the last 20 kept); each restart returns the member's open
attempt to the lead as `uncertain` ("the session crashed at T; restarted") and
records `member_restarted` with `why` (`crashed`, `exited with a task` or
`resumed`). The fourth crash in an hour leaves the session stopped with one
inbox item and `member_crashed`; a resume that reaches the bound spends its
request the same way, instead of looping. Pausing the team stops restarts, a
member the team paused is never restarted, and a launch that fails records
`restart_failed`.

### The team store never makes a reader wait

The teams live in one SQLite file, `teams/teams.sqlite3` under the state root.
Since task 105 it runs in WAL mode: a reader (`hgs team list`, status, the
desktop's polls, the watch) reads the last committed state while a writer works,
and a writer never waits for readers. Before, in the rollback-journal mode, a
writer's commit waited for the readers and new readers waited behind it, so a
list could fail with "database is locked" after its 10 s wait; measured on a
copy of a live store, a write of a 3.7 MB team waited up to 17.5 s.

The first write open of an older store switches it (it waits 2 s for the other
connections; a busy store is switched by a later open). The `-wal` and `-shm`
files stay beside the store when the last connection closes, so a read-only
sandboxed reader can still open it; on Windows they inherit the folder's Low
integrity label like every file there. The log is checkpointed every 4000
pages and kept at 64 MB at most. Where SQLite refuses WAL (a store on a network
folder) the store stays in the rollback-journal mode. An older hgs reads a WAL
store; its write open switches it back to the rollback journal when no other
connection has it open, and this hgs's next write open switches it again.
`hgs team list` reads every record before it parses any, so a read never holds
the store while megabytes of JSON parse.

### The team store's write lock is held briefly

One team write at a time holds the store's write lock (`BEGIN IMMEDIATE` to its
commit), and SQLite queues nobody: a waiting call retries until its 60 s bound
and fails with "The team database stayed busy for 60 s". Since task 287:

- Every write logs how long it held the lock, with the code that took it, in
  `logs/team-store-locks.log` under the state root (one JSON line: `caller` as
  file and line, `team`, `held_ms`, `waited_ms`, `outcome` commit or rollback,
  `level`). A hold of 200 ms or more is `warn` and also goes to
  `logs/slow-calls.log`. Both logs keep 256 KB plus one older file. A process
  started while the lock is held (a git that slipped into a transaction) is a
  `warn` line naming the holder.
- A member's report makes its git reads (its folder's HEAD and branch, the
  test delta against the lead's commit) before the transaction; the
  transaction reads the answers from memory.
- `hgs team inbox` and `hgs team current` mark messages read by changing only
  the message rows and adding the receipt's event. The team record is not
  rewritten and its revision does not move. The member's unread summary in the
  record follows at the worker's next tick. A mark that fails (the store stayed
  busy) still prints the messages, with `read.error`; they stay unread and come
  again with the next inbox.
- The worker applies a team's steps of one tick to a working copy and writes
  it in one transaction at the end. A change with a request id, or one of the
  working tier (a doorbell), writes the copy first and the batch goes on after
  it. If another writer changed the team meanwhile, the worker's changes are
  merged over that record: objects key by key, lists of records by their `id`
  (or a phase's `n`). A value both changed keeps the other writer's, which the
  next tick recomputes from. The worker pauses 100 ms between teams so a
  waiting call takes the lock.
- The member projection (`team_rule_members`) is rebuilt only when the members
  change. The members are kept in their own column with the revision that
  wrote them. A write by an older hgs, which sets neither, still rebuilds it
  from the record.

### A team's tasks and attempts are rows

Since task 288 a team record keeps only its shared fields (members, phases,
settings and the like) in `teams.payload`. Each task, without its attempts, is
a row of `team_tasks` (its id, its place `ord` in the plan), and each attempt
is a row of `team_attempts` (its task and its place in the task). Every row
carries `rev`, the team revision that last wrote it; the `teams` row carries
`layout` (2 for rows) and `head_rev`. The store assembles the same team as
before for every reader (`load`, the list reads, the member lookup), with
the revision per team as before.

A change works on a copy of the team the store keeps. The copy is read
before the write lock. Under the lock only the revision is checked, and when
another writer changed the team meanwhile, only the rows written since are
read. The change then writes the rows it changed: one attempt's row for an
attempt, the task's row for the task's own fields, its place for a move, and
the shared fields only when one of them changed. A team of 4.7 MB of tasks no
longer reads, serializes and writes them all under the lock.
Measured on a copy of the live store (team 33fbafeb, a 5.75 MB record,
release build, 20 changes each), the lock was held as follows (median, p95
and max):

| Change | Whole record | Rows |
| --- | --- | --- |
| Member change with a request id | 49, 70, 84 ms | 27, 30, 34 ms |
| Worker tick | 25, 42, 42 ms | 13, 16, 17 ms |

The one-time migration held the lock for 89 ms.

The first write open of a store migrates each team stored whole, one
transaction a team, so an interrupted migration goes on at the next open and a
migrated team is never migrated again. A team whose task ids repeat keeps the
whole record (`layout` 1). An older hgs cannot read a migrated team. Its
shared fields hold `"tasks"` as a sentence, not a list, so the older hgs
answers "Invalid team record" and never writes the team back without its
tasks. Every hgs and desktop on the computer is updated together.

### The polled team list and inbox are small

The desktop follows `hgs team list` of every computer, peers over SSH
included, and reads it again whenever a team's revision moves. Since task #6
(team ede724c4) each item carries only what the session list's cards and strips
show. Tasks, phase records, attention items, proposals and each member's
queue, services, jobs and question are in `hgs team status`. Each item has:

- `id`, `name`, `status`, `project`, `folder`, `lead`, `revision`, `updated`;
- `plan_mode` and `phase_modes` (every phase's mode, phase 1 included);
- `members`: each one's `id`, `role`, `agent`, `state`, `session` (`host`,
  `name`, `run_id`, `conversation_id`), `session_state` and `state_reason`;
- `task_count`: the tasks the request covers (cancelled ones only with
  `include_cancelled`, as the desktop asks);
- `progress`: the phase strip over those tasks. `phase` is the lowest phase
  with an unfinished task (accepted and cancelled count as finished), else the
  last; `phase_done` and `phase_total` count its tasks; `phases_done` and
  `phases_total` count the phases. A team with no task has all five at 0;
- `accepted` (archived phases included), `submitted`, `in_review`, `in_test`;
- `attention_count` (the owner's attention items) and `open_proposals` (the
  lead's open proposals);
- `waiting_for_lead`, `pending_phase`, `stuck_scope`, `auto_accept` and
  `parent` (`team` and `task` of a sub-team, else null).

On the store copy of 2026-11-10 (21 teams), the list fell from 352 KB to 44 KB.
A desktop that follows a peer with an older hgs still reads that peer's
`tasks`, `attention` and `proposals` arrays and counts them itself. An older
desktop reading this hgs's list shows 0 tasks, attention items and proposals
until it is updated.

`hgs team inbox` resolves its caller once. A member's own call (`{}`) finds
its team through the members projection (`team_rule_members`), so only teams
whose member list names the session are read. Only when the projection names
none are all running teams read, as before. On Windows the ancestry check that
proves the session reads one process table for the whole walk, not one per
ancestor; each ancestor is still verified. The owner's read of a named team
(`{"team"}`) has no session identity to prove. Proposals are settled and
items are built only for the lead, a sub-lead or the owner.

### A stalled lead tells the owner

When the lead's decisions (a member's report, question or permission) have
waited longer than `Settings.lead_stall_minutes` (20 by default, 0 turns it
off, at most 1440; only the owner changes it) and the lead's session does not
work on them, the dispatcher records `team.lead_stall`. A lead that is in a
turn, runs a Zerus build or waits for its own Background jobs works; one that
is idle, has its own question or permission open, ended its turn on a refused
tool call (the listing's `interrupted` phase) or whose agent exited does not.
A computer that did not answer the listing decides nothing.

The record names the reason (`idle`, `question` with the question's title,
`rejected_tool`, `exited`), the decisions waiting past the threshold and the
members they block, the oldest one's time and a notification `round`. The owner
sees it as the attention item `lead_stalled` (in status and the team list; it
covers `lead_stopped`), as the Waiting-for-you item
`<team>/lead_stalled/<lead role>-<round>` (action `resume_lead` for an exited
agent, otherwise `open_lead`), on the team bar of both desktops, and on the
phone: the lead's `hgs ls --json` row carries one `mobile_attention` item
`{"question_id":"team-lead-stall:<team>:<round>","question_hash",
"kind":"team_lead_stall","team","team_name","items","members","reason","round",
"since"}`, never the question's text, and the lead's `state_reason` is
`team_lead_stalled` ("Waiting for you: the lead stalled, ..."). A row whose own
question already has an `attention_signature` mixes the alert into it; a null
signature stays null. The mobile connector keeps the row's `team_lead_stall`
items when it adds its own question cards.

Desktops notify a waiting id once, so each round is one notification; a new
round starts at most once an hour while the lead stays stalled. The record
clears as soon as the lead works again, the decisions are taken or fall back
under the threshold, the team stops running, or the setting is 0. The feed
records `lead_stalled` (raised or a new round) and `lead_stall_cleared`.

### The owner's question queue

A native permission prompt or question that nobody answers does not freeze a
team while the owner is away. When a request of a team's lead or member has
waited `Settings.defer_minutes` (10 by default, 0 turns it off, at most 1440;
only the owner changes it), the dispatcher answers it with the safe choice
and queues it for the owner:

- a permission gets its one-time Deny, never Allow and never a persistent
  rule; one without a one-time Deny is left on the screen;
- an agent's own question (Claude's AskUserQuestion, a Codex or Kimi question)
  gets its recommended option (an option marked recommended, or whose label
  says "(Recommended)"), else its first; an optional Codex question is skipped;
- folder trust, updates, resumes, permission modes and a request only the
  Terminal takes are queued but never answered.

The answer goes through `hgs answer` with the exact run, conversation,
question id and hash. It is reserved first, in the member's `deferred` list and
in the queue (`teams/owner-queue.json` under the state root, at most 500
entries, answered ones kept seven days), and neither reservation is undone: an
answer whose delivery is unknown is recorded as `uncertain` and never sent
again. After an answer that arrived, the session gets one guarded message:
"The owner is away; this request is queued for them. Continue with work that
does not need it, or do it yourself." The member's record keeps the answer
`by: "owner queue"` and the feed records `request_deferred`.

The queue's waiting entries are Waiting for you > Deferred: kind `deferred`,
id `deferred/<id>`, action `answer_deferred`, with the request's exact text,
its session, team and member, when it was asked and what Zerus did; the
desktops notify each once. On the phone, a team's entries are
`mobile_attention` items `{"question_id":"owner-deferred:<id>","question_hash",
"kind":"owner_deferred","team","team_name","request","since"}` on its lead's
`hgs ls --json` row (members' notifications are muted), an ordinary session's
on its own row; the mobile connector keeps them beside its question cards.

The owner answers with `{"id","decision":"allow"|"deny"|"answer","text",
"by":"human"} | hgs team answer-deferred --json` (the desktops send the same).
Zerus never replays the tool: the session gets one guarded message, "The owner
now allows <request>: you may run it again.", "The owner denies <request>: do
not run it; ...", or "The owner answers your question (<question>): <text>".
A message refused before it reached the session may be answered again; one
sent, or whose delivery is unknown, is never sent again. `{"limit":N} | hgs
team deferred --json` lists the entries, newest first, and the watched
sessions.

An ordinary session is never deferred unless the owner turns it on:
`{"session":NAME,"host":ALIAS,"minutes":N,"by":"human"} | hgs team
defer-session --json` (0 turns it off; at most 100 sessions). The team worker
then reads such a session while it asks, at most every 20 seconds, and keeps
running for it. A session of a team follows its team's setting.

#### A request only the Terminal takes is raised to the owner

A member's request that Zerus cannot answer (`can_answer` false: a Kimi
approval whose panel Zerus cannot verify, Kimi's Edit and Write approvals, a
request behind another one) is never a quiet wait. The owner sees it at once
(Waiting for you, kind `permission_escalated`, action `open_terminal`) and the
lead's inbox names it. Once it has waited `defer_minutes` (10 when the queue is
off), the dispatcher raises it once (task 113): it reads the member's Terminal
(`hgs terminal NAME --json`, action `snapshot`) and keeps the last 12 non-blank
lines in the member's `terminal_escalation`, bound to that request's id. The
owner's item then adds `escalated`, `raised_at`, `minutes`, `session`, `host`,
`tool`, `reason` and `screen`; the lead's `hgs ls --json` row carries one
`mobile_attention` alert `{"question_id":"team-terminal-prompt:<team>:<member>:<question>",
"question_hash","kind":"team_terminal_prompt","team","team_name","member",
"member_id","session","host","tool","screen","action":"open_terminal",
"action_label":"Open Terminal","since"}` with the screen's last six lines (at
most 120 characters each); the mobile connector keeps it beside its question
cards. The lead's starved-member message lists every member that waits in a
Terminal, with its session, minutes and request. The escalation goes with its
request (feed `terminal_prompt_raised`, `terminal_prompt_cleared`); Zerus never
answers it.

### A member's status is cross-checked from every source

Task 139: `hgs team status` named members' states wrong (working while an
approval waited, a prompt Zerus did not read, a delivery left in the composer).
The dispatcher compares a member's sources: its session record (native hooks
and events), a request Zerus read (`pending`, `terminal_question`), its
Terminal's recognised panel (a question or approval, an unsent paste in the
composer, an idle prompt, a working spinner), its process and jobs, and the time
of its last event against the stuck bound. The screen is read only for a
suspect member (busy without an event for 3 min, asking without a request Zerus
read, idle with a hand-out unreported for 3 min, or a disagreement still open),
at most every 3 min (1 min while it disagrees), two members at a time and on its
own thread, never on the tick.

Sources that disagree for 60 s make the member `uncertain`: status shows its
`phase` and `activity` as `uncertain` and `check` with `state`, `since` and the
disagreements in words; it never shows a confident `working`. The lead is told
once per episode, with the disagreements, the screen's last lines and how to
look again (feed `member_uncertain`, `member_uncertain_told`); agreement or a
member no longer suspect ends it (`member_check_settled`). An idle lead with
decisions waiting (task 95's stall, reason `idle`) is told itself once a round
(`lead_idle_told`).
- Task #269 (owner, 2026-11-08): a member waiting on its own Zerus build
  (`hgs team run` or `hgs run`), on Background work (a job, subagents in
  flight, a declared budget) or on a tool call in flight is not uncertain: the
  quiet is expected, its state stays `working`, and the check and the snapshot
  name what it waits on (`sources.waiting`, top-level `waiting`), e.g. "its own
  Zerus build (release-candidate)" or "a tool call (Bash)". The build and
  Background exemptions are the dispatcher's stuck rule's; a tool call gets
  that rule's longer tool bound, and the prompt the agent sits at while it
  waits is not a disagreement. A quiet member without such work is still
  compared as before.
- Task 278 (owner, 2026-11-08): the jobs a member waits on are registered,
  whatever its activity says. These are the build Zerus runs for it, the
  background jobs its hooks registered (a Claude `run_in_background` Bash,
  confirmed against the agent's processes; a Codex exec session; a DeepSeek
  job from its native host), and Background work under a declared budget. The
  listing row carries the hooks' jobs as `background_jobs` (`command`,
  `description`, `started_at`). They are never counted as the session's
  Background work, so the session state, the capture gate and the reply badge
  keep their rules. A member with such a job is "waiting for <job> since HH:MM"
  (the first job, and how many more) in look's `waiting`. It is never suspect,
  never uncertain over its idle prompt, and never stuck or silent. Its status
  member card carries `jobs` (`kind` build, shell or job; `what`; `command`;
  `since`) and `waiting_for`. Each job's start, command and end are kept in
  the working tier (`working_jobs`, at most 500 ended rows a team) and
  journaled as `job_started` and `job_ended`. The lead's handoff brief lists
  them among its last pulses ("dev started waiting for its background job
  `cargo test` (since 14:05)"). The hand-out still asks for builds and tests
  through `hgs run` or `hgs team run`.

What counts as work, not silence (task 181): a member inside one open tool call
is held to the tool bound (six times the stuck bound, at least an hour) counted
from the call's start, for DeepSeek too (its open call is the newest
PreToolUse with no result, message or Stop after it in the session's events,
read off the tick), and one waiting on its own Zerus build (`hgs run`, `hgs team
run`) is Building; neither is `uncertain` or called silent. A Claude screen whose
prompt is empty but lists running subagents (`◯ general-purpose ... tokens`) or
`N shells still running` is working. A finished turn the hooks still call
working (an idle prompt, no agent row, no shell, no request, silent for 2 min,
two reads a minute apart agreeing) is settled in the record instead of being
called uncertain: `hgs subagent settle NAME --json` marks it idle and its
parent's turn done, ends the native subagents whose stop never came (a subagent
cannot outlive its parent's turn) and leaves a background job with live
evidence, a waiting request or a recent event alone; the member is then idle
for hand-outs (feed `member_settled`). A DeepSeek session's state is its host's
and is never settled this way.

`hgs team look MEMBER [--team ID]`, or `{"member":"dev","team":ID} | hgs team look
--json`, reads the member's screen now and answers one compact snapshot: the
reconciled `state` (`uncertain`, `settling` within the grace), `disagree`,
`sources` (hooks, record, screen, process, activity, waiting), the last 12
`screen` lines, its open `task`, `waiting` (task #269: its own build, Background
work or a tool call), its `pulse` and `next`, what Zerus does next. It
changes nothing. A member is named by id, role or session; with several teams
the caller's own team is taken first.

### The profile of the work: losses, spans and unknown

`hgs team profile` reads a scope after the work: where its time and tokens
went and what was lost. It is read only, computed on request from the tasks'
timelines (the same spans U37's `timing` reads) and the tokens the members'
sessions spent inside them. Input is one JSON object:

```text
{"team":"ID"}                      the plan: every task that is not a draft
{"team":"ID","phase":2}            one phase
{"team":"ID","task":7}             one task
{"team":"ID","compare":[1,2]}      two phases side by side
{"team":"ID","phase":2,"limit":20}     20 losses a page (20, at most 100)
{"team":"ID","phase":2,"after_time":{"s":600,"id":"rework:7:a1"}}     next page
{"team":"ID","phase":2,"after_tokens":{"tokens":12000,"id":"..."}}    next page
{"team":"ID","phase":2,"after_task":50}                              next task page
{"team":"ID","phase":2,"tokens":false}   no session reads, every token unknown
```

Canceled work stays in its phase's profile: its time was spent. A draft has no
timeline yet. Every list is a stable page with a cursor, so a record that
arrives between two reads never repeats or skips one.

A timeline is a list of spans, one lane per task, that never overlap: a
question inside the work cuts the work, a stage after the report covers the
lead's wait. Each span has `kind`, `from`, `to`, `s` (seconds) and, where they
apply, `member`, `attempt`, `stage`, `round`, `stage_kind`, `state`, `by`,
`held_by`, `template` and `exit`. The span kinds are:

- `work` and `rework`: an attempt's work; `rework` is a rejected or returned
  attempt.
- `deps`: waiting for a dependency; `member`: waiting for a free member;
  `folder_held`: another task's stage holds the folder (`held_by`).
- `delivery`: the hand-off to its delivery; `start_delay`: delivered to
  started (A1); `lead`: waiting for the lead's decision; `hold`: a hold
  (`by`).
- `question` and `permission`: blocked on a native question or permission
  request (`by` is who answered); what the lead answers.
- `person` (task 118): waiting for a person's answer, with its `reason`:
  `acceptance` (a phase before waiting for the owner's acceptance, with
  `phase`), `permission` and `question` (a native request or a task question
  the owner answered, or a request that went to the owner after the team's
  `remind_owner_permission_minutes`, or only the Terminal takes), `deferred`
  (a request the owner queue answered safely for the owner, task 97) and
  `report` (the lead's decision on a report the owner decided, or that went
  to the owner after `remind_owner_minutes`). It is a wait, not the member's
  work; the lead's own waits stay `lead`, `question` and `permission`.
- `stage_wait`: waiting for a stage member; `stage`: a U36 stage of a round
  (`stage_kind`, `round`).
- `background`: Background work (U30); `no_report`: a task without a report;
  `stop`: a stop.
- `unknown`: a gap nothing recorded, with `from`, `to` and `s` null (never
  guessed). Builds lie on top of the lane in `overlay` as `build` spans with
  their `template` and `exit`.

The output is one JSON object:

- `summary`: `tasks`, `accepted`, `from`, `to`, `elapsed_s`, `worked_s`
  (work + rework + stage), `totals` per span kind, and `tokens` with `total`,
  `cost_usd` and `partial`, or `{"unknown":true}` when no part is known.
- `losses.by_time` and `losses.by_tokens`: ranked pages of `items` with
  `next_cursor` and `total`. Each loss has `id` (stable, kind first), `kind`,
  `s`, and `tokens`/`cost_usd` when its part's session is known. The kinds are
  `rework` (per rejected or returned attempt, with `cause`, `share` of the
  task's tokens and `tokens_unknown`), `lead_wait` (`lead_s`, `question_s`),
  `person_wait` (task 118: the seconds of the task's `person` spans, with
  `reasons` per reason), `permission_wait` (`answered_by`),
  `blocked_chain` (a folder another task's
  stage held: `folder_held_s`, `held_by`, with `deps_s` and `on` for context;
  waiting for dependencies is no loss by itself, see lateness below),
  `member_wait`, `idle_member` (`open_s`,
  `from`, `to`), `long_test` (a test stage or test build of at least 300 s),
  `merge` and `conflict` (a `git-merge-lead` run, or a land stage, that did
  not merge cleanly). `by_tokens` ranks only the losses whose tokens are
  known.
- `critical_path`: `planned` (the longest dependency chain by task count,
  ties to the lower ids), `real` (from the task that ended last back through
  the dependency that ended last each time, with `s`), `off_plan` and
  `matches`.
- `members`, `roles` and `models`: `busy_s`, `idle_s`, `utilization`,
  `accepted`, `tokens`, `tokens_unknown` and, where both are known,
  `tokens_per_accepted`. A member is idle only where a task of the scope was
  open after it joined.
- `tasks`: a page of rows (`task`, `title`, `status`, `phase`, `elapsed_s`,
  `worked_s`, `rework_s`, `tokens`, and `rework_tokens`/`rework_share` when
  known, and the two measures below: `lateness` and `overrun`), with
  `next_cursor`. `summary.lateness` counts the rows: `late`, `own_s`,
  `inherited_s` and `unknown`.
- `events` (`read`, `truncated`: the bounded journal window was cut) and
  `token_reads` (`asked`, `read`, `truncated`: at most 200 session reads).

Lateness and overrun (task 81) are two values per task, apart from the causes
above and from each other. Heat that adds up waits since the phase began tells
nothing: a task that by the plan starts after its dependencies is not losing
time while they work on schedule.

- Lateness is measured against a baseline schedule fixed when the plan is
  approved (the change that approves a phase, or adds a task to a running
  plan) and kept through later plan edits: a task's planned start is the latest
  planned end of its hard dependencies and of its member's previous task (a
  barrier phase also of the lower phases), and never before the baseline's
  moment; its planned end is that plus its estimate. `lateness` is `{planned_start,
  planned_end, start_late_s, inherited_s, own_s, end_late_s, from, actual_start,
  actual_end}` (task 82: `from` is the predecessor the inherited part comes
  from, null without one; `actual_start` and `actual_end` are the moments
  themselves, null until they happen): `start_late_s`
  is the actual start (or now, while the task has not started and is past its
  planned start) minus the planned start, never negative; `inherited_s` is the
  part of it that the tasks it was planned to follow are behind (they ended, or
  still run, past their planned end), and `own_s` the rest; `end_late_s` is the
  actual end (the report, or now while it runs) minus the planned end, null
  before the task starts. A moment that needs an estimate nobody has is null
  with `unknown` naming why, and the whole `lateness` is null for a task saved
  before baselines, one already running when they came, or a draft. The causes
  (`lead_wait`, `member_wait`, `rework`, `long_test`, `merge`) explain
  lateness; they are not added to it. `hgs team status` tasks carry
  `planned_start` and `planned_end`.
- `overrun` is `{estimate_s, work_s, overrun_s, ratio, running}`: the task's own
  work (every attempt, rework included, without waits) minus its estimate (the
  baseline's, else the current one), signed, and the ratio of the two. It is
  `{"not_started":true}` before the first attempt and its estimate, `overrun_s`
  and `ratio` are null with `unknown` when the task has no estimate.

Unknown is never zero: a member on another computer, a missing session record,
a provider without counters, or a read past the 200-read bound leaves those
tokens unknown, and `summary.tokens` says `partial` or `unknown`. A `compare`
answer has `a` and `b`, each a phase's summary and top three losses by time,
and `delta` with the changes in `elapsed_s`, `worked_s`, `tasks`, `accepted`,
`tokens` and per-kind `totals`.

The lead's phase report names the phase's top three losses by time
(`losses.by_time.items`), and planning weighs lanes with the per-role numbers
(`roles[]`: utilization and tokens per accepted task) of the last finished
phase.

### Test isolation

A test template that Zerus runs for a member (`cargo-test`, `ctest`,
`android-test` and `web-test` unless the project names others) runs in a run
folder of the
member's own folder, `.zerus-team/runs/<template>-<time>`, with its own
`HOME`, `TEMP`/`TMP`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`,
`XDG_CACHE_HOME` and, on Windows, `APPDATA` and `LOCALAPPDATA` (on Unix also
`TMPDIR` and `XDG_RUNTIME_DIR`). `ZERUS_TEST_RUN` names the run folder.
`CARGO_HOME` and `RUSTUP_HOME` keep the person's toolchain, the person's
`.gitconfig` is copied into the run's home, and Zerus's own state variables
(`HGS_STATE_DIR`, `HGS_CONFIG_DIR`, `HGS_TRAY_HOME`, `HGS_TRAY_LOCK_DIR`) are
removed. The newest five run folders are kept. On Windows the run shows no
error box, so a crash leaves no dump in the person's `CrashDumps`; a crash
exit is noted in the run folder's `crash.txt` and in the result.

The second line is a fingerprint of the person's own paths before and after
the run. The run fails, with the paths named in its output, when:

- a watched log or registry (Zerus's `private-stores.json` always) gains a
  mention of the run folder;
- a new entry in the person's temporary folder, or a watched folder, starts
  with the test program's name or a project prefix;
- a new crash dump in `%LOCALAPPDATA%\CrashDumps` belongs to a program found
  in the member's folder or with a project prefix;
- a watched file changes at all.

The project configures it in `.zerus/tests.toml`, `[isolation]`, with
`[isolation.windows]` and `[isolation.unix]` tables over it: `templates`,
`env` (variable to a folder below the run folder; `""` unsets a default),
`keep` (variable to a path below the person's home, used when unset),
`remove`, `copy`, `prefixes`, `logs`, `files`, `folders` and `enabled`.
`enabled = false` turns the run folders off but not the fingerprint, and the
file adds watched paths without removing the built-in ones. A file that cannot
be read keeps the defaults and says so in the result. Without the file the
defaults above apply.

Rust tests that set no state folder, or inherit one outside their temporary
and build folders, use one per test process beside the test binary
(`target/<profile>/deps/hgs-test-state/<pid>`), never the person's.

### Build broker (members sharing one folder)

Several members may work in one folder: a game project where a worktree per
member would copy hundreds of GB of assets. Without the broker their builds
run in the one build folder and compile each other's half-written files.

The owner turns it on with the team setting `build_broker` (off by default,
owner-only). The lead's inbox names it once when two or more members (the lead
not counted) share a folder, with the switch card to propose:
`{"kind":"switch-setting","setting":"build_broker","value":true,"reason":"..."}`
piped to `hgs team propose --json`; the owner's Allow sets it, a Deny's reason
reaches the lead, and a card already proposed is not proposed again. Off,
builds run as before.

On, a build template of a member runs in that member's snapshot,
`.zerus-team/runs/broker-<member>/tree` below the shared folder:

- The member names the files it changes in its build requests,
  `"args":{"files":["Source/Game/Unit.cpp"]}`. They stay its own edits until
  the file is clean again (committed or undone).
- The snapshot holds the folder's tracked files. The member's own changed
  files come from the working copy. A file another member is changing (or
  anyone's uncommitted change) is taken at HEAD, so its half-written state
  never reaches this build.
- Source files are copies, never hard links: a hard link shares the file's
  data, so a write on either side would reach the other. The project's asset
  and linked folders are one link each (a junction on Windows), so a 300 GB
  project costs no second copy; their contents count in the tree's key by a
  listing of paths, sizes and times (up to 200,000 entries; a larger folder
  makes the tree unknown, and an unknown tree is never served by another
  build's result). Only files whose content changed are written again (a
  clean file by its git blob, the member's own by a content digest, never by
  size and time), so the build stays incremental. The shared compiler cache
  (`[build_cache]`) serves it as usual.
- Builds of one build folder run one at a time: the folder's lock is the
  queue. The key is the tree, the template, its arguments and the toolchain's
  version lines (`rustc -vV` and `cargo -V`; `cmake --version` for the Qt,
  CMake and ctest templates; `msbuild -version` for `msbuild-build` and
  `vstest`). A build
  of the same key still running is waited for through a marker taken
  atomically. A finished result serves a check template (`cargo-test`,
  `cargo-clippy`, `ctest`, `vstest`) as its verdict for any member; a build
  template's result serves only the member whose tree holds its outputs. The
  newest 64 results are kept in `.zerus-team/runs/broker-results.json`.
- A failure in a file another member is changing is reported to both as an
  observation, with the files named: it may be the other member's unfinished
  change or a dependency of the asker's own. The asker checks whether its
  change touches what they use, else waits and builds again; it never edits
  the other member's files.

Shared mode is for projects where a snapshot costs too much. An Unreal
project (a `*.uproject` at the folder's root) uses it unless the tests file
says `mode = "snapshot"`; any project can choose it with `mode = "shared"`:

- A build runs in the shared folder itself: no copy, no second build folder
  (for Unreal, no second `Intermediate`), and the editor loads the DLL that
  was built. Builds of the folder run one at a time.
- Asking for a build says the member's own edits are ready
  (`.zerus-team/runs/broker-ready/<member>.json`). Before it runs, a build
  waits until every other member sharing the folder has asked for a build
  since it last wrote one of its changed files (a file no longer changed does
  not count), at most `ready_wait_s` (300 by default). Past that wait it runs
  anyway, and its note names the unfinished changes it included.
- A failure in a file another member is changing is reported to both as an
  observation, as in snapshot mode. Results are not reused: the folder is not
  a fixed tree.
- An Unreal project's `Content`, `Plugins/<plugin>/Content` and
  `Plugins/<group>/<plugin>/Content` count as asset folders without being
  configured, in either mode.

The lead's item states the mode and its cost: in shared mode, builds queue and
a member in the middle of an edit delays the others' builds up to the wait; in
snapshot mode, each member's sources are copied and its first build is a full
build in its own build folder.

`.zerus/tests.toml` configures it:

```toml
[build_broker]
enabled = true                 # false: the project vetoes the broker
mode = "auto"                  # "shared", "snapshot"; auto: shared for Unreal
ready_wait_s = 300             # shared mode: the wait for others' edits
templates = ["cargo-build", "cargo-test", "cargo-clippy",
             "cmake-configure", "cmake-build", "ctest", "msbuild-build", "vstest"]
assets = ["Content"]           # folders linked whole, never copied
link = ["ThirdParty"]          # untracked folders the build needs, linked
copy_limit_mb = 4              # copies above this size are named in the note
```

`hgs team builds --json` (`{"team": ...}`) reads the broker's state for the
desktop's Builds tab inside Tests: for each folder two or more members share,
per build folder (`target`, `.ci-build/gui`, and `.` for shared-mode builds
in the folder itself) the running build with its usual
time (the median of recent green builds), the queue with each request's wait
and the member whose identical tree it waits for (in shared mode,
`waiting_for`: the members whose edits are not ready), the cache's hit rate and the
recent builds (a failure lists the members whose files broke it); each
member's changed files and the files two members change; and `members`, each
member's `building` or `waiting` state with `ahead`. With the broker on, `hgs
team status` carries that `members` map as `build_states`, so task nodes and
member cards show "building (snapshot) 2:10" or "waiting for a build, 1 ahead".
The switch card (`switch-setting`) records `shared`, the folders and members
it is for.

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
DCB-svn report (2026-11-12): Claude Code cancels a hook that runs past its 5 s
timeout, and on a busy computer a new session's SessionStart was cancelled before
it bound the conversation; every later event was then dropped and the session
stayed "Status unavailable". The first main event (no `agent_id`) of a new Claude
session that has no conversation yet binds it as that start would have. A resume,
a fork, a restore or a fresh session that expects a conversation still waits for
its own start.
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

In Terminal, Ctrl+Shift+C or Ctrl+Insert (⌘C on macOS) copies the selection
exactly as shown, and Ctrl+Shift+V or Shift+Insert (⌘V) pastes. On Windows,
Ctrl+C over a selection copies it and clears it, as in Windows Terminal;
without a selection, and elsewhere, Ctrl+C and Ctrl+V stay the agent's keys.
Ctrl+Enter and Shift+Enter type a new line (a line feed, Ctrl+J, which Claude
Code and Codex take as a new line in their input); Enter submits. Shift with an
arrow, Home or End extends a keyboard selection from the cursor, and
Ctrl+Shift+A (⌘A) selects everything. Shift+Page Up/Down scrolls a page,
Ctrl+Shift+Up/Down a line and Ctrl+Shift+Home/End to the top or the bottom
(`client/src/ui/sessions/terminal-keys.ts`). Every other key goes to the agent.
Right-click opens **Copy selection** and **Paste**. While the agent reads
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

`hgs [@host] delivery receipt ORIGINAL_NAME --json` reads one original send
receipt, including text-only sends, without sending or checking a native agent.
Its input is an object containing `request_id` (the read transport UUID),
`message_request_id` (the original send UUID), `expected_machine_id`,
`expected_session_id`, `expected_run_id`, `expected_conversation_id`, `provider`,
`native_digest`, and an optional `agent_id`. Here `provider` is the agent kind:
`claude`, `codex`, `kimi`, `dsh` or `opencode`. The name is the original recorded
label, not a route to its current holder; this command never follows a rename.
The request's machine and durable session binding must match the original.
The native digest is the unchanged producer digest: SHA-256 of the exact stdin
bytes for terminal sends, or of the compact JSON `[original_name, request]` for
DeepSeek and OpenCode. It is distinct from a phone's canonical intent digest.
OpenCode proof requires the original provider-valid `msg_` message ID; missing,
malformed or unsupported native IDs remain unknown and are never exposed as delivery evidence.

A verified submitted original returns body-free metadata, its submission time,
queue flag and an exact native message ID only when the original records one.
This proves admission to the original input path, not completion of agent work.
Every other readable outcome is `status: unknown` with a bounded reason. Missing,
corrupt, oversized, pending, unbound or mismatched evidence never means Not sent
and never authorizes replay. New sends record their immutable machine/session
binding before delivery; older receipts without that binding stay unknown even
when their digest matches. No lookup retrofits or rewrites receipt history.

The lookup reads one UUID-named original of at most 256 KiB through pinned
directories whose entries only their owner can change. It does not open the
derived receipt database, enumerate receipts, run migration, resolve a live
session or contact a provider. Missing
state stays missing. Its existing computer-identity check is read-only too.
The `delivery-receipt-v1` capability marker gates remote forwarding to prevent an
older CLI from treating the command as a launch. This CLI operation is the first
part of mobile ordinary-send recovery; connector and phone support ship separately.

Recent attachment delivery projection uses the private, rebuildable
`delivery-receipts-v1.sqlite3` locator in the state directory. A refresh reads
at most 100 original receipts for the selected provider, conversation and child;
older receipts use an opaque cursor bound to that scope and an index snapshot.
New receipts cannot displace rows in an existing page chain. The locator stores
identities and bindings, not message bodies or attachment bytes. Original JSON
receipts remain authoritative for delivery, attachment access and no-replay
checks; a missing locator never permits another send.

The inspection's `receipt_projection` reports index coverage, its next cursor
and explicit missing, corrupt or changed-original gaps. An unavailable or still
building index is unknown coverage, not evidence that no message was sent.
Legacy discovery runs in one coalesced background worker per state directory,
with transactions limited to 64 directory entries or indexed bindings, or 25 ms,
and a pause between
batches. An interrupted worker retains committed locators and their sequence
numbers; discovery reopens in the background. It never deletes or rewrites
receipt history. New sends retain the original atomic receipt write and update
the locator on a best-effort basis afterward.

Older caches remain unverified until the worker reconciles their originals.
Removing a locator or changing its indexed identity invalidates coverage and
snapshot cursors. Every worker restart reconciles the original directory even
when its modification time is unchanged; inspection never does that walk.
Before clearing a cache discrepancy, the worker verifies every projection field
against its validated original and repairs compatible metadata. A conflicting
original fingerprint or pinned conversation remains an explicit gap.
A first-message receipt may acquire its original's resolved conversation while
keeping its page chain. The writer re-reads and validates the original before
this exact update. Other cached conversation changes immediately invalidate
coverage and report `binding_changed`. Legacy guards must upgrade before a
cache can serve verified coverage.
The worker also checks indexed bindings not encountered in that walk, counting
retained bindings within the same bounded pass. Finishing a source does not
rescan its rows. It publishes that count only if the source stamp, epoch and
trigger-maintained count stayed unchanged during counting; a racing walk keeps
the live count and incomplete coverage. A missing original keeps its binding and
becomes an explicit gap with a history change sequence. Discovery can finish
with a gap; question recovery still refuses
unverifiable evidence. Restoring the same original clears its gap, while a
replacement with another binding remains untrusted.

The same locator exposes exact name, run and conversation pages for question
recovery. Deadline-bounded inspection never walks the question receipt directory;
incomplete coverage keeps recovery unknown. An explicit answer authority read
can reconcile that family synchronously, then must validate every original and
drain a complete snapshot before treating recovery as complete. A cursor from an
incomplete snapshot requires a fresh chain after migration finishes. Locator
coverage alone never proves an answer's submission.

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

Task #349: the journal is the one list of every approval not given by the owner
in person. Besides the automatic entries (source `automatic`) it records the
team lead's answers (`hgs team answer`, source `team lead <session>`) and
Zerus's own safe answers (source `Zerus safe-call rule` for a single in-scope
`hgs team` call, `safe-action shape` for a recorded safe action), each with its
computer, session, team, member, question, command and the answer given. An
entry is written durably before the answer is delivered; a delivery that fails
settles the same entry as failed, and nothing is sent when the journal cannot be
written. `list` also takes exact `source` and `team` filters; its 50-entry pages
stay on the indexes and record-ID cursors, and older entries are never
backfilled.

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
On Unix, a public send to an existing-ID Claude, Codex or Kimi session can apply
one exact scheduled Settings change before its message. The same custodian keeps
the original Root and SID exclusions and one original 25-second deadline across
both steps. This path keeps the normal 64 KiB text limit and admits at most one attachment
with at most 1 MiB decoded data. These exact count and byte decisions precede
selection, receipts and terminal input; larger attachment requests use their
existing send path, including the normal 20 MiB total attachment limit. Its outer input
receipt precedes the first Settings key;
`input_pending_at` still immediately precedes the message paste. After input is
offered to that custodian, a refusal or lost acknowledgement means delivery is
uncertain and never authorizes retry. These observations omit the best-effort
diagnostic screen sample. Zero-pending sends, Background or busy sends and
internal submissions retain their ordinary routing. Codex sends whose native
conversation ID is still empty also keep their ordinary path; its startup
dialog checks and Settings walk exceed this path's measured budget margin.
Unprimed Kimi sends remain eligible. If another Settings
custodian already owns the slot, the send keeps its ordinary blocking path.
An expired or refused staged message gets one pre-reserved, UUID-specific tmux
buffer discard after Stage physically settles, within two seconds of the
original deadline. If Stage remains unsettled beyond that lease, cleanup is
uncertain and its buffer may remain; original custody lasts until its helpers
physically settle. This narrow path does not enable general native continuity.
The deadline is minted once before the keeper handoff; nested Settings borrows
it. Standalone Settings and Effort retain their 15-second keeper budgets.
The desktop waits up to 30 seconds for a local Send and 45 seconds for a peer.
The longer local caller lifetime also applies to ordinary and legacy local
sends; their command arguments, input, routing and native checks are unchanged.
The composer keeps its single-send guard until the original call settles.
A connector operator's `subprocess_timeout` below about 27 seconds would cut
a pending-Settings Send short; the connector default remains unchanged.
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

Text left in Claude's or Codex's own input holds every message sent from
Activity or a team window. When a send is refused for it, or messages wait
while the agent is not working, Activity and the team conversations show that
draft above the message field with **Send it** and **Open Terminal**. Send it
presses Enter in Terminal only while the input still holds exactly the draft
shown (`hgs terminal <session> --json` with `"action":"draft"`, then
`"action":"submit_draft"` and its `draft_hash`; a replayed request ID returns
its receipt and presses nothing). Your own message stays in its field. HGS
never types into, edits or clears the draft.

A delivery of Zerus's own can leave its paste behind: `hgs send` pasted the
message, then its Enter did not go (an idle compaction, a banner or a dialog
came first), so the composer shows the paste, for example
`[Pasted text #17 +62 lines]`, and refuses every later message. Right after
each paste `hgs send` reads the composer's draft; when its Enter fails, the
receipt keeps `pasted_draft_hash` and the session record `unsent_paste`
(`request_id`, `draft_hash`, `run_id`, `conversation_id`, `at`). The record
forgets it when a later message finds the composer empty.
`{"request_id","action":"submit_own_paste","expected_run_id",
"expected_conversation_id"} | hgs terminal <session> --json` presses Enter only
while the composer holds exactly that paste in the same run and conversation
(its draft hash binds run, conversation, pane, process and draft text). It
forgets the paste before the Enter goes, so it never presses twice, and a
draft the person typed or changed never matches ("The Terminal draft is not a
paste Zerus delivered; nothing was sent"). A finished paste marks its own
delivery's receipt `submitted` with `finished_by`.

In a team, a hand-out (or a stage hand-out) refused for a draft, dialog or
unrecognized prompt first asks `submit_own_paste`. When the paste was Zerus's,
it is finished (`own_paste_finished` in the feed) and the next tick hands the
task out again; an uncertain Enter is never repeated (`own_paste_uncertain`).
Anything else is counted in `member.input_block` (`since`, `at`, `count`,
`reason`, `screen`, `raised`). From the third such refusal the lead's inbox and
the owner's attention list hold one `input_blocked` item with the Terminal's
last twelve lines (colors removed) and what to do: open the member's Terminal
and submit or clear the draft, close any dialog and return to an empty prompt.
Waiting for you lists it with action `open_terminal`; the member cards in Qt
and Zerus Next show "Terminal refused N hand-outs" with hgs's reason. Zerus
never submits that draft. The next delivered hand-out ends the episode
(`input_unblocked`).
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
  with the second **Confirm**. A subagent card's own **Yes, and don't ask
  again for subagents** is the exception: the hook takes it, as described in
  [subagent confirmation](#subagent-confirmation). A **Yes** or **No** that
  the hook does not take within three seconds also goes through a verified
  panel; feedback it does not take is not sent.
- A decision the hook has taken is never sent again. Until Claude confirms it,
  the card stays locked and is answered in Terminal.
- `hgs answer` receipts report `transport` `claude_hook` or `claude_tui`.
- Automatic approvals use the hook only for a card whose complete Bash panel
  shows exactly that operation, with the same one-time choice.
- Requests made by subagents and workflows keep the screen path. Claude 2.1.296
  runs subagents in the background and shows their requests outside the main
  screen. The main agent's own subagent calls are answered as a batch (see
  [subagent confirmation](#subagent-confirmation)).

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
confirm delivery. In the fullscreen Goal panel, animated foreground RGB colours
on the exact `Using tools` status row above it may change between observations;
all other screen bytes and the complete validated panel must match. A different,
clipped or unverifiable panel stays read-only.

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
compared as decoded JSON.
Kimi Code 2.5 (task 113) writes the same wire records and draws the same
rows; in fullscreen mode (`tui_mode = "fullscreen"`) it docks the panel: its
body is one column narrower (the scrollbar's) and shows at most six rows, and a
longer body adds ` · shift+↑/↓ scroll` to the key hint. Zerus reads both
layouts row by row; a scrolled body, a panel without the keyboard (`esc back
to approval`) or a narrow one stays read-only and says why. A request of the
running turn whose tool call Kimi's PreToolUse hook has not reported to Zerus
still shows its exact command, read-only with **Open Terminal**, and becomes
answerable once the hook reports it; one older than the turn, or after it
ended, shows nothing.
Experimental and off by default: an empty file
`~/.config/hgs/kimi-unverified-edit-approvals` (in `HGS_CONFIG_DIR` when set)
also lets Zerus answer Kimi's **Edit** and **Write** approvals, from Activity
or by a team lead. Zerus cannot redraw Kimi's diff, so it checks less than for
Bash: the same sole unresolved request of every agent, its hash and active
tool, and exactly one complete panel on screen titled for that tool with the
default choices; the card says so above the request. Delete the file to turn
it off. Every key and pasted feedback line first finds the same
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

Supported startup prompts include Claude workspace trust, its **Bypass
Permissions** warning and auto-mode consent, Codex folder access, **Hooks need
review** and **Update available**, and Kimi folder/MCP trust. These can appear
before a conversation exists, but only with verified startup identity.
A recognized panel must show its native title, folder, exact choices and key
hint. Its explanation is shown verbatim and pinned to the answer, so changed
native wording stays answerable: Claude workspace trust preserves its full
permissions warning, Kimi includes the visible MCP commands, and Claude's
auto-mode and resume panels keep their explanation and cost warning. Zerus adds
descriptions only for reviewed text. In a Git subdirectory or worktree, Codex
folder access names the repository root that Codex will trust. Claude's bypass
permissions warning is the owner's card: its two choices are buttons, and the
owner's own click sends the arrows and one Enter to that run's terminal through
the guarded native path (`claude_tui`). The card carries `trust_request`, so the
automatic approvals never answer it and a team lead's answer is refused; its
exact warning, choices and hint must match, or it stays read-only. Claude wraps a
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

A change whose keys reached the agent but that it did not visibly take (it never
returned to its empty prompt in time) leaves the change **unconfirmed**: the
session record keeps `settings_unconfirmed_at` with the requested
`settings_unconfirmed_model` and `settings_unconfirmed_effort`, and nothing is
typed into the agent while it stands, because the keys may still be on their way
to a picker or dialog. It is confirmed, without Terminal, by any of:

- a native event after it, as before;
- the agent's own evidence: its terminal at an empty prompt with no dialog, and
  the model and effort it shows (its status line, Claude's `/model` receipt in the
  transcript, or its history) are the requested ones. Every `hgs send` checks
  this again before it refuses, so Start planning's brief and an ordinary message
  reach an idle lead once the switch shows; each `hgs inspect` reports it as
  `settings_confirmation` (`null` when nothing is unconfirmed, otherwise `state`
  `confirmed` or `unknown`, with `requested_model`, `requested_effort`,
  `shown_model`, `shown_effort`, `at_prompt` and `reason`);
- `hgs [@host] settings SESSION --check --json`, which reads that evidence again
  and settles a confirmed change in the record (it prints the state; `none` when
  nothing is unconfirmed);
- `hgs [@host] settings SESSION --check --accept --json`, the person's explicit
  confirmation to continue with what the agent shows. It settles the change only
  while the terminal shows an empty prompt with no dialog, and refuses with the
  reason (changing nothing) otherwise, so nothing that follows types into a dialog.

Settling makes the shown model and effort the session's (and its resume's) and
drops a pending change that would otherwise be typed again, or refused as unsafe
to retry, before the next message. A refused send names these actions instead of
sending the person to Terminal. Every desktop composer (the session view,
Activity, the Team tab and New team's lead conversation) shows the unknown state
above its editor: what was asked and what the agent shows, **Check again**
(which also runs by itself every 20 seconds while it shows) and **Continue
anyway…**, which asks first and is offered only while Terminal shows its empty
prompt. A host from before `settings_confirmation` keeps its refusal.

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
keep-warm resumes after the next real turn. Turning it on over a cache whose
estimate already ran out is the same case: it says the cache went cold at once
and starts after the next real turn. Claude Code's estimate that ran out keeps
its lifetime and the moment it lapsed (`session_usage.prompt_cache.ttl_seconds`
and `lapsed_at`, with `status` still `unknown`), which tells a cold cache from a
lifetime Claude has not reported yet. The cache clock belongs to the
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
`kimi_config`, `kimi_table`), `interval_seconds` and `cache_expires_at` (in the
past for a cache that went cold).
Each change of what the worker does for a session (its state or reason, a ping
sent, the setting turned off) leaves one line in `logs/keep-warm.log` in the
state directory, bounded as `slow-calls.log` is: the worker state holds only the
latest decision, and the log says afterwards why a cache was not kept warm.
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
JSON object and changes only the fields it names. A DeepSeek host's tool call
(`HGS_AGENT=dsh`) never acts as the person; while its session cannot be proven,
`hgs task` refuses it (see Session markers of hosted tool calls).

Another computer's hgs, reached as `hgs @host …`, sees no marker of the agent
that calls it and takes the caller for the person. So an agent's process
never forwards a command whose handler tells the person from an agent that way
(zerus-ppd26.9). Those commands are `task`, `team` and `question-routing`
(`PERSON_ONLY_FORWARDED` in `src/cli.rs`); a newer peer's `team` or
`question-routing` is covered too.
- **Where the check runs:** on this computer before any ssh, `--dry-run`
  included, on every route: plain `@host`, `@host --expect-machine ID
  [--client C]`, the private `__state` entry and a session reference's
  verified hop.
- **Who counts as an agent:** a process that claims a session (`HGS_SESSION`
  and `HGS_RUN_ID`, proven or not), a resident DeepSeek tool call
  (`HGS_AGENT=dsh`), or a process a live session proves as its caller, a
  Claude background job included.
- **What the agent does instead:** it runs `hgs task` on its own computer,
  which reaches every computer's copy. A person's shell that inherited an
  agent's variables can unset them.
- **Unchanged:** tasks' own forwarding of `send` and `cancel` to the
  executor's computer (`--agent NODE/SESSION`).
- **Still forwarded for an agent:** `answer`, `interrupt` and terminal input
  act the same for an agent and the person; `wait` acts on its ticket;
  `parent` and `wait begin` refuse a caller the other computer cannot see;
  `desktop` and `approval-log` refuse every other computer.

This guards against accidental agent use, not against an agent that clears its
own environment. Forwarding a verified caller instead is future work.

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

A DeepSeek account's catalog comes from its running host. Without one, it is
what the composed profile configures (`dsh --profile hgs --dump-config`, which
starts no host): the `models` of the `llm-deepseek` entry, or else the installed
transport's bundled models, each named as dsh names it (for example
`deepseek-official/deepseek-flash`, DeepSeek-V4.1-Flash). The answer then
carries `"source":"dsh-profile"`. New team's member rows offer these models, and
a DeepSeek member's model is applied after its session starts, as
`provider/model` or as a bare id (`deepseek-flash`) that one provider offers.

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
account's configured name from **Agent accounts** and compact cached limits, or
**limits n/a**. Desktop reuses the status footer's compact meter widget, including
the selected usage indicator; Android shows the same usage/elapsed rails and pace
colors. Default labels, profile IDs and quota error details are omitted from these
rows. The account list uses
the same provider identity and organization grouping as **Agent accounts**, with
bindings only from the selected computer. Aliases of one account share a row;
conflicting account IDs, organizations and unidentified profiles stay separate.
The selected, remembered or default local profile remains the exact launch binding.
For Claude, Codex, Kimi and DeepSeek, New session also lists the selected
account's native model catalog and, where the provider supports it, reasoning
effort. The empty model keeps the account/provider defaults; an explicit choice
applies only to the new conversation and is passed after `--` (Claude and DeepSeek
effort as `--effort`, Codex as `model_reasoning_effort`). Kimi effort stays in its
native settings. DeepSeek applies its choice through the native settings RPC
immediately after creating the session. OpenCode keeps its richer folder/provider
model picker and its own `--model`/`--effort` options.
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

**Permissions → Change…** reads the installed harness's own permission catalog
on the selected computer. It never uses a version table: Claude choices come
from the resolved executable's `--help` (`--permission-mode`), Codex from
`--help` (`--ask-for-approval` and `--sandbox` plus `--approve-for-me`), Kimi from
`--help` (`--yolo`, `--auto`, and the separate `--plan`), and DeepSeek from the
composed `dsh` profile's `@deepseek-ai/dsh-permission-presets` table. The row
carries `permission_catalog {status, status_reason, source, version?,
session_supported, selected, controls[]}` and a separate `permission_current
{source: "provider"|"zerus", values, detail}`. `permission_current` describes
launch settings, never a running session.

**Use <harness> settings** (for example, **Use Codex settings** or **Use dsh
settings**) leaves the native setting in charge. **Bypass**
remains the common shortcut (Claude `bypassPermissions`, Codex the dangerous
bypass flag, Kimi `--auto`/Never Ask, DeepSeek `danger-full-access`). Other
choices are native controls: Claude has `mode`; Codex has `approval`, `sandbox`
and the enabling `approve_for_me` switch; Kimi has `mode` and the independent
`plan` switch; DeepSeek has `preset`. Codex combinations are validated (for
example `approve_for_me` cannot accompany an explicit `approval` or `sandbox`,
even `workspace-write`; leave both on **Use Codex settings**). Controls can
publish `conflicts_with` control IDs so the shared editor rejects such
combinations before saving. Missing native
keys mean "inherit", and an empty native selection normalizes to provider.
Kimi's Always Ask has no forcing flag, so it is shown when inherited but is not
offered as a fabricated option. DeepSeek's Auto is current-session-only and is
never offered as an account or launch default.

The stored override is `provider`, `bypass`, legacy Claude `auto`, or
`native:<canonical compact JSON object<string,string>>` (at most 1 KiB, keys
`^[a-z][a-z0-9_]*$`). Decoding validates syntax only, so a saved choice survives
a version change; a write or launch revalidates it against fresh options. A
withdrawn native value is shown unchanged and fails with an actionable error;
provider and bypass launch without discovery. The CLI is `hgs [@host] account
permissions ID --mode provider|bypass|native:<json>`; `account ls` keeps the
legacy `permission_modes` array and adds the catalog. Discovery uses bounded,
read-only probe operations (at most 4 seconds for help and 1 second for the
optional version; DeepSeek's composed-profile read is separately bounded),
deduplicated per provider and executable, and never starts an agent, host or
session. If discovery fails, the row is
`unavailable` with a reason; **Use <harness> settings** and **Bypass** remain
available.

**New session → Permissions → Use Zerus account settings** inherits the selection
saved for the account in Zerus. **Use <harness> settings** instead uses the
harness's own settings, ignoring that Zerus account selection. A discovered
native selection can also override the account for one session. It becomes
`hgs <agent> … --permissions <selection>`, is
stored as a symbolic launch override (so resume revalidates it), and cannot be
combined with the agent's own permission flags. A per-launch `provider` choice
suppresses the account default even though it adds no native flag. The account
default stays dynamic on resume; an explicit per-launch choice does not.
Resume `--fresh`, clear-context replacement and native forks keep that explicit
per-launch choice, because they continue the same session's settings with a new
conversation; a brand-new session uses the account default. Permissions do not
accompany copied sign-in credentials.
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

<a id="model-servers"></a>

## Model servers

A model server is any computer on the LAN, or this one, that serves LLMs through
an OpenAI- or Anthropic-compatible API. The agent and its tools run on this
computer; the server only answers inference requests. Configuration is per
computer, local and never committed: `model-servers.json` in the private config
folder (`~/.config/hgs/` by default), written owner-only. A missing file means
no server and no extra tab.

Each server row has a `name`; the API `base_url`
(`http://` or `https://HOST[:PORT][/path]`, no credentials or query) and its
`kind` (`openai` or `anthropic`); an optional `key` (stored only in this private
file, never shown by a listing); an optional `ssh` host, either one of this
computer's Machines or an SSH alias, always reached through hgs's own SSH
transport (BatchMode, `-n`, never a piped `ssh.exe` on Windows); and optional
control commands as argv templates for `status`, `up`, `down` and `logs`.
`{model}` is the only input a command receives and `{server}` its server name.
Commands are argv lists, never shell lines: locally hgs starts the program
itself and over SSH it quotes each argument for the peer's login shell. A
Windows Machine has no POSIX login shell, so its control commands are refused
with a message naming it. `up_minutes` bounds a detached `up` (30 by default, 1
to 240).

`base_url` names the server root, without `/v1`: the root is stored and each
consumer derives its own URL from it — `GET root/v1/models` for the model list,
`root/v1` for an OpenAI-compatible client, `root` for Claude Code (which appends
`/v1/messages` itself). Both spellings, with and without `/v1`, are accepted and
name the same server; the trailing `/v1` is not stored.

```json
{"version":1,"servers":[
  {"name":"lab","base_url":"https://192.0.2.10:8443","kind":"openai","key":"<private>",
   "ssh":"labhost","up_minutes":30,
   "status":["serve","status"],"up":["serve","up","{model}"],
   "down":["serve","down","{model}"],"logs":["serve","logs","{model}"]}
]}
```

Without control commands the server is read only: `status` reads `root/v1/models`
(with `Authorization: Bearer` for an OpenAI kind, `x-api-key` for Anthropic) and
lists the models it returns. An `http://` server is read by hgs itself; an
`https://` server is read through the system `curl` (`curl.exe` on Windows 10+,
`curl` elsewhere), run with an argv list, `--fail-with-body` and `--max-time`,
with the key in a private `--config` file in the config folder so it never
appears on the command line. Curl's exit codes keep the errors distinct (6 and 7
unreachable, 28 timed out, 22 an HTTP error with 401/403 unauthorized); when
curl is missing, `status` says so as a refusal naming curl, and sessions still
work because the agents speak TLS themselves. A control command may print the
Zerus JSON contract
`{"models":[{"name":"small","state":"running"}],"memory":{"free":12.5,"total":64}}`
or plain lines `NAME: STATE`, with an optional `memory: FREE/TOTAL GB` line;
a model in the JSON contract may carry `since`, when its state began (seconds
since the epoch, or an RFC 3339 time), which the tab shows as "since 09:12";
any other line is kept as raw text. States are shown lowercased. A `NAME: STATE`
line splits at the last `": "`, so a model named with a tag (`coder-a:q4:
running`) keeps its whole name and `Down` targets exactly that model; a name
starting with `-` is offered by no list and refused by `up` and `down`.

Commands (all local to the configured computer; `--json` is available on `list`
and `status`):

```text
hgs model-server list [--json]
hgs model-server status SERVER [--json]
hgs model-server up SERVER MODEL [--detach]
hgs model-server up-result REQUEST_ID --json
hgs model-server sessions SERVER --json
hgs model-server down SERVER MODEL [--yes]
hgs model-server logs SERVER [MODEL] [--lines N]
hgs model-server save --json      (JSON on stdin)
hgs model-server test --json      (JSON on stdin)
```

Settings > Model servers writes the configuration with `save --json`: stdin
`{"by":"human","servers":[...]}` replaces the whole list atomically. Only you
save it (an agent session cannot, as for other owner settings). Each server
may carry `was`, its previous name: a server without `key` keeps the key stored
under `was`, and `"key":null` removes it. Every error names the server and the
field, nothing is written on an error, and the receipt lists the servers as
`list --json` does, never with a key. `test --json` takes one server as
Settings shows it, saved or not (an absent `key` means the stored key of
`was`, kept only while its root is unchanged — a test with another root never
sends the stored key), runs `status` once and prints what `status --json`
would, saving nothing. Settings tests the status and logs commands only; `up`
and `down` belong to the Model servers tab with its confirmations. `list --json`
also carries each server's command templates in `argv`.

`list` shows `name`, `base_url`, `kind`, its SSH host and which control commands
exist, never the key. `up` and `down` accept only a validated model name (letters,
digits and `-_.:/@+`, no leading `-`) and return a receipt; `up --detach` starts
the command as a detached `hgs run` with the server's time limit, so the receipt
carries its `request_id`. The run receives an argv, not a shell line, so a
Windows path or a quoted remote command arrives as one argument. `up-result
REQUEST_ID --json` reads that run's outcome: `status` `running`, `ran` (with
`exit`, `timed_out` and the last 1000 characters of its output in `tail`) or
`refused`, with its `server` and `model`; it reads only a detached up's own run
and refuses any other id. The Model servers tab reads it every few seconds
until the run ends, within its time limit, and shows the exit (with the last
line of a failure's output), the time limit or the refusal. `sessions SERVER
--json` lists this computer's live sessions whose binding names the server, as
`{"server":...,"sessions":[{"name":...,"model":...}]}`; Down and Down all name
them beside each model. A server may be
shared, so `down` asks first and says so: without `--yes` it returns
`{"status":"confirm",...}` and runs nothing. `logs` keeps the last `--lines`
lines (200 by default, at most 2000) within a fixed time and output bound.

Unreachable (the SSH host or API address does not answer), refused (the command
or the API answered with a failure), timed out (the fixed limit passed) and
unauthorized (`HTTP 401/403`) are distinct errors in `--json` output, for
example `{"status":"error","error":"unauthorized","detail":"..."}`. Nothing
retries on its own.

Choose a server's API and model for a new session with
`hgs codex DIRECTORY --new -n TAG --model-server lab --model small`. The launch
carries this session's provider settings only. Codex gets a session-scoped
custom provider, `-c model_providers.hgs-lab={name,base_url,env_key,wire_api}`
and `-c model_provider=hgs-lab` with `--model small`: its `wire_api` follows
the installed Codex (`chat` before 0.100, `responses` from then on, where
`chat` is deprecated and removed from the newest releases), and it reads its
key from `HGS_MODEL_SERVER_KEY`, so neither the built-in OpenAI provider nor a
ChatGPT login is used.
Claude Code gets `ANTHROPIC_BASE_URL` (the root) and its model variables, with
the token in `ANTHROPIC_AUTH_TOKEN`. OpenCode is not offered for a model server
until it has a per-session provider contract; a global provider entry would
break the session scope. The user's global agent configuration is never
changed.

The key never travels on a command line: the launch leaves the session's secret
environment in a private file and passes only its path to `hgs --run`, which
reads it, gives it to the agent and removes the file. It is therefore absent
from the tmux start command, from the session's environment listing and from
`--dry-run` (which prints `NAME=***` for a secret). Only the agent's own
environment inherits it.

A server may carry `contexts`, each model's context window in tokens as the
server runs it (`{"coder": 262144}`; 4096 to 10,000,000, at most 500 models).
Settings keeps it on save without editing it. A session on such a model tells
its agent the window: Claude Code gets `CLAUDE_CODE_MAX_CONTEXT_TOKENS` (it
otherwise assumes 200k for a model it does not know, and compacts there), Codex
`-c model_context_window=N`. Change the value when the server's engine changes
the window. A Claude Code launch also reads the server's model list (5 s at
most) into a settings file of that launch, passed with `--settings`: its
`/model` picker offers the server's models, the session's own first, instead
of the cloud models that answer nothing behind this server (Claude Code's own
gateway discovery keeps only `claude`/`anthropic` IDs). An unreadable list
leaves the picker as it is; the session still starts.

A session records its model server and model. `hgs resume` applies those
settings again; when the server is no longer configured the resume is refused
with a message naming it instead of falling back to the cloud account.

<a id="deepseek"></a>

## Official DeepSeek Harness

The native adapter starts every `@deepseek-ai/dsh` release (owner, 2026-11-04).
Zerus reviewed `0.2.1-rc.2` and `0.2.1-rc.5`; any other release starts with a
note naming them and `harness-unreviewed` in the host's folder, since the bridge
uses harness internals and a feature may fail there. Only a harness that reports
no version is refused, before any session exists. Install a reviewed release on the agent host,
apply its reviewed security overrides (docs/installation.md#agents), sign in and
choose **DeepSeek Harness** in New session:

```sh
npm install -g @deepseek-ai/dsh@0.2.2-rc.1
npm pkg set --prefix "$(npm root -g)/@deepseek-ai/dsh" \
  'overrides.@modelcontextprotocol/client=2.3.0' 'overrides.fflate=0.8.3'
npm install --omit=dev --prefix "$(npm root -g)/@deepseek-ai/dsh"
hgs account login native-dsh
hgs dsh /path/to/project --new -n work
```

Current upstream still pins MCP client 2.2.0 and nested fflate 0.8.2. The two
overrides fix their cross-origin redirect and malformed-ZIP64 issues without
changing other agents in the npm prefix; a bare global install retains those
risks. Other dependency advisories remain subject to normal review.

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

### Session markers of hosted tool calls

One native host serves every DeepSeek session of its agent home from one
process, so whichever session happens to start it is not the session of any
hosted agent (zerus-ppd26). Zerus starts the host without that session's
variables: the session markers, its account, launch and parent identities, its
terminal (`TMUX`, `TMUX_PANE`, `HGS_TERMINAL_NAME`) and `CLAUDECODE`, the same
list the Windows session host drops. The state and configuration roots stay.

Every tool call's processes then carry `HGS_AGENT=dsh` and exactly the calling
session's own markers: `HGS_SESSION` (its name), `HGS_RUN_ID`, `HGS_SESSION_ID`
and `HGS_ACCOUNT_ID=native-dsh`. Zerus writes them after each binding save to a
private file per native conversation, `shells/<conversation>.json` in the host
folder, so they always name the current run, and removes it when the session is
forgotten. The adapter reads that file for each call, foreground, background or
promoted, and a child agent's calls carry its parent session's markers. Without
a valid file, for example an agent the native UI opened, a call carries
`HGS_AGENT=dsh` and none of the four: the adapter removes them explicitly, never
passing a partial or inherited set. Its `ping` reports `shellIdentity` when it
sets these per call.

A hosted tool call is never the person. `hgs task` and the desktop's
agent-descended refusal treat `HGS_AGENT=dsh` without a session they can prove
as an agent, and refuse person actions plainly. Until a hosted session can be
proven, its `hgs task` commands are refused and its sends are unattributed.

The limit: one process serves all hosted sessions, so process ancestry proves
only that a command runs under the host serving a session, not which hosted
agent ran it. The markers are claims that the host sets per call, which ends
accidental attribution to another session; an agent that deliberately sets
another hosted session's markers, or edits the same-user identity file, is not
detected by them.

A host an older Zerus started keeps the markers it inherited and its adapter
until it exits on its own; it is never restarted or reloaded for this. Its
`ping` has no `shellIdentity`, and the existing refusals for those inherited
markers stay. The adapter version is unchanged, so this Zerus keeps using such a
host. Such a host started from a process without session markers, such as the
desktop or a plain terminal, gives its tool calls neither markers nor
`HGS_AGENT`: the markers cannot tell those calls from the person, and only the
check of recorded host ancestry below refuses them.

Under a recorded host, no other identity (zerus-ppd26.15). The process tree and
the host records decide this, never a marker the caller sets. hgs reads the host
records first: a computer or account that records no DeepSeek host is never
affected by the rest of this check. Otherwise hgs walks a command's ancestry
through processes of the same user, at most 64, up to the first process of
another user. When that walk contains a DeepSeek host this
computer records in `dsh/hosts`, matched by process ID and birth, the command is
never another agent's session and never the person, even when that agent's
process is also an ancestor, as it is while the `hgs` that started the host
still runs. The send and `hgs wait begin` origin and the parent link of a launch,
local and forwarded with `--parent-link`, are left unattributed; `hgs task`,
`hgs self`, `hgs parent adopt`, question routing (locally and before `@host`
forwarding) and desktop mode registration refuse. An unmarked `hgs send` or
`send-now` under a host found is refused too, locally, by reference and before
`@host` forwarding, because its target would receive it without a sender; an
unverifiable check refuses no send, so a sandboxed agent's sender-less send
keeps working. A launch under a host keeps an empty parent. `HGS_AGENT=dsh` stays
an extra guard. The checks also refuse in both directions, the person's commands and
task reads included, when:
- a host record cannot be read or is malformed;
- there are more than 64 host folders or 1024 entries;
- a record names an ancestor's process ID without its birth;
- the walk reaches 64 processes without the top;
- the birth of a process on the walk cannot be read;
- a process on the walk cannot be read or changes while it is read;
- the walk reaches a process of the user whose parent is 0: the first process
  of a separate process namespace the user owns, such as an agent's sandbox
  (bubblewrap with its own PID namespace), `unshare --user --pid` or a rootless
  container. A host outside it cannot be seen, so commands that need the caller
  proof must run outside that sandbox, or with its private process namespace
  turned off.
Each cause has its own refusal. A record's refusal names the exact path and how
to clear it: close the DeepSeek sessions of that agent home, then remove the
record. The desktop shows it. Agent Teams' caller proof uses the same check,
`state::dsh::hosted_ancestor`.
Records of an earlier boot never block anyone. A host's records are written
atomically (a synced temporary file renamed into place, then the folder synced),
so a crash leaves the previous record or the new one. Outside this check:
- a command that leaves the host's tree (a double fork, setsid, or a background
  job whose shell exits), which the user's service manager adopts;
- a host started by an attempt that ended before its running record.

The official minimal composition's persistent `bash` tool keeps one shell per
agent across calls, which starts with the environment of the call that opened
it. Its later commands keep that session's markers, which can name an earlier
run after a relaunch. The `hgs` profile Zerus launches uses the per-call `bash`
tool.

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

Accounts and New session show the balance of an API key from DeepSeek's
`/user/balance`, read with the key that `DEEPSEEK_API_KEY` or
`DSH_HOME/.credentials.yaml` holds, also while no native host runs. The key goes
only to `api.deepseek.com`; Zerus keeps an opaque hash of it as the identity.

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
balance. An unreachable API or a rejected key leaves `configured` without a
balance. A
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
Arch packages, `~/.local/lib/...` from `packaging/desktop/install.sh`), which alone needs Qt
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

A DeepSeek row in `hgs ls` and the GUI reports its background work without an
inspection, so an idle session with a running background job shows
**Background**. The adapter answers `ping` with `listJobs: true`. Each row of
its `list` answer then carries `listJobs`: a summary of the running bash jobs
of that session and of its subagents listed in the same answer. Jobs are taken
from the same native job registry that an inspection reads:

- `{"complete": true, "count": N, "jobs": [...]}` when at most 32 such jobs run.
  `count` equals the number of `jobs` entries, and 0 means no background work.
  Each entry is `{"id", "kind": "bash", "owner", "status", "startedAt",
  "label", "callId"}`. `status` is `starting`, `running` or `stopping`;
  `owner` is the session or subagent ID; `startedAt` is milliseconds since the
  epoch; `label` holds at most 240 characters; `callId` may be null.
- `{"complete": false, "count": null, "jobs": []}` when there is no job registry
  or more than 32 jobs run. There is no count.

A count is a number only when `complete` is true. HGS lists such a row with
`process_work.count` N; an incomplete summary, or one whose count disagrees with
its jobs, leaves `process_work.count` null. An adapter from an older Zerus has
no `listJobs` in `ping` and no `listJobs` key in its rows. Its rows also keep
`process_work.count` null. Consumers read null as unknown, never as free. A host
is never restarted to gain this; hosts started by this version provide it. The
bridge's own `ping` and `list` answers for this contract are captured in
`tests/fixtures/dsh-bridge-list-row.json`; `tests/test_dsh_bridge.py` fails when
they differ and rewrites the file with `ZERUS_REGENERATE_DSH_BRIDGE_CAPTURE=1`.

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

## Activity never sticks on a timed-out refresh

The desktops give a local `hgs inspect` 5 s (a peer's 15 s). Task 117 keeps
the read inside that and lets a failure pass by itself:

- `hgs inspect` answers its phase timings in `timings_ms` (`total` and each step:
  `state_lock`, `record`, `live_sessions`, `questions`, `processes`,
  `provider_messages`, `events_open`, `activity`, `waits`, `session_state`,
  `team_store` and others). A call of 1 s or more leaves one JSON line in
  `<state>/logs/slow-calls.log`, which rotates at 256 KiB to one older file.
- `hgs ls` names its phases the same way (task 193, task 234):
  - a listing of 1 s or more is an `ls` line: `reads`, `rows` (with `rows_live`,
    `rows_saved` and their count), `dsh`, `opencode` and `finish_rows`;
  - parts of 100 ms or more get their own lines: `ls-row` (one live row),
    `ls-dsh` (`host_rows` is the DeepSeek host's answer), `ls-opencode` and
    `ls-finish` (`workspace`, `session_state`).
- Task 234: what made a listing take 6 to 8 s on 65 sessions:
  - **DeepSeek host replays.** The host builds a stopped session's row by
    replaying its stored history: about half a second each, 7 s for twelve.
    The host's last answer for a stopped session is kept in
    `dsh/hosts/<id>/listing-rows.json` and reused while all of these hold:
    the host's generation is the same (checked by one `ping`), the binding's
    run is the same (a launch or resume through Zerus asks again), and the
    host does not name the session as loaded. A bridge that names its loaded
    sessions (`loadedSessions` in `ping`) keeps such a row up to an hour, an
    older one 15 minutes. One row past its bound is refreshed per listing,
    oldest first.
  - **Git metadata.** Every listing ran `git` twice for each session folder,
    and with 27 folders it spent its whole 750 ms budget. A folder's metadata
    in `metadata-cache/` is now reused while its `HEAD` is unchanged (a branch
    switch, a detached commit or a removed worktree changes it), up to a day;
    a folder that is not a repository is read again after 2 minutes. A read
    that runs out of the budget keeps the last good metadata instead of
    replacing it with `unavailable`.
- The team reasons every listing and inspection reads wait at most 300 ms for a
  team writer. A store that stays busy answers with the last good reasons, kept
  in `teams/waiting-cache.json` for 2 minutes. Before, a rollback-journal store
  (hgs before task 105's WAL) made this read wait up to 10 s behind a team
  write: past the desktop's limit.
- An inspection waits at most 3 s for the state lock, then answers "The session
  state is busy" with its timings instead of being killed.
- Qt and Zerus Next keep the last good Activity after a failed refresh. The
  status says "Activity not refreshed since HH:mm:ss, retrying: <why>" (before
  any good read, "Could not refresh activity: <why>"). The session is read
  again by itself after 2 s (Activity's interval in Zerus Next, 2.5 s, or the
  background interval behind Terminal), doubled for each failure in a row up to
  a minute, also while a live event stream follows it, which before left the
  notice standing. A read that succeeds, or a stream state, clears it; a
  timed-out request never holds the next one.

## Build folder cleanup

Zerus removes the build folders it creates, automatically and only its own
(task 108, owner 2026-11-06: a team filled two drives with about 50 GB of
scratch cargo targets). hgs records each build folder a Zerus build uses in a
bounded registry, `build-dirs.json` under the state root (at most 2000
entries), with its team, task, member or session, a size estimate and its last
use:

- `hgs run`: cargo's `--target-dir` and `CARGO_TARGET_DIR`, and CMake's `-B`
  and `--build` folders the command names (relative to its `cwd`; a folder
  named through a variable is not guessed);
- `hgs team run`: the template's `target` (beside each `manifest`) or
  `.ci-build/gui`, recorded by the worker when it runs it;
- the cargo targets and `.ci-build/*` CMake builds inside the member worktrees
  and clones Zerus created (scanned at most hourly, three levels deep), and the
  folders the last 200 `hgs run` records name.

A recorded folder is Zerus's when a Zerus run created it, when it is inside a
member folder Zerus created, or when an `hgs run` was told to use it outside
any repository. A folder that existed inside a repository before Zerus used it
is the person's: it is listed, never removed. Before each removal Zerus checks
again that the folder is still a build output (a cargo target's
`CACHEDIR.TAG`, or `CMakeCache.txt`, and no `.git`, `Cargo.toml`,
`CMakeLists.txt` or `package.json`), that it is not a drive root, the home, the
state folder or one of their parents, and that nothing uses it: no running
`hgs run` names it or runs in it, no team run runs in its folder, cargo does not
hold its `.cargo-lock`, and no file in it (two levels deep) was written in the
last ten minutes. It is then renamed aside (Windows refuses that while a file
in it is open; then nothing was removed) and deleted. A delete that fails after
the rename leaves the folder in `review`: it is never tried again by itself.

The policy is per computer, on by default: remove a recorded folder of Zerus's
when its team is finished, stopped, archived or gone or its member removed,
when it was unused for `unused_days` (7; its newest write counts as use), or
oldest first while its drive's free space is under the larger of `free_gb`
(50) and `free_percent` (10) of the drive. The team worker runs the pass on a
thread of its own at most every ten minutes (measuring a few folders' sizes,
each scan bounded to 200,000 entries or 3 s). A drive under its threshold is a
Waiting-for-you item `disk/<drive>` (kind `disk_low`, action `open_disk`) with
what Zerus's folders there hold.

`hgs team disk --json` serves the Disk view and its actions:
`{"action":"list","refresh":true?}` (the policy, the drives, the folders by
team, what Zerus holds), `{"action":"clean","scope":"policy"|"all",
"dry_run":true|false,"paths":[...],"by":"human"}` (a dry run decides and
removes nothing; Clean now needs the owner and removes only folders of
Zerus's that are not in use), `{"action":"policy","policy":{"enabled",
"unused_days","free_gb","free_percent"},"by":"human"}` and
`{"action":"forget","path","by":"human"}` for a folder in review the owner dealt
with. Settings > Build folders in Qt and Zerus Next shows the policy, the
drives, the folders per team with their state, a dry run and Clean now (with a
confirmation, Cancel focused).

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

**Clear input** (an eraser icon in the Terminal bar) discards text left in the
native agent's own composer, such as a message whose delivery was uncertain
(zerus-wkj9l). It shows only while the selected live session's composer is
recognized and not empty, its agent is not working, and nothing is being sent
to it; the Terminal tab reads the composer every few seconds while it is shown.
The confirmation shows the exact text that will be discarded. After **Clear
input**, `hgs terminal SESSION --json` (`action: "clear-input"`, the composer
read's `terminal_binding_id`, a 5-second deadline and that exact `text`) takes
the input lock and refuses a busy agent, a changed terminal or any other
composer text before a key is sent. It then presses End and Ctrl+U, which clear
a line in Claude Code, Codex and Kimi Code and do nothing on an empty composer.
Ctrl+C is never sent: Codex quits on it once the composer is empty. It reads
the composer again after every round and stops once it is empty (`cleared`),
or reports `uncertain` with the remaining text when it stops changing, as a
multi-line draft can. A request ID is never replayed. The clear is recorded with
the session (`input_cleared`) and settles an unconfirmed earlier input whose
residue it was. It is only ever the person's action: no delivery or retry uses
it. The read is `action: "composer"`, which returns the text, `clearable` and
the reason when it is not.

While a send to that run from any client (another desktop, a phone, an agent
or a task) is still in its proof window, both the read and the clear refuse, and
nothing is typed (zerus-wkj9l.1). The proof window lasts while the send's
receipt is `in_progress` with no delivery outcome and its input is at most 40
seconds old. During it, the message being delivered may sit in the composer.
Once that send ends uncertain, its stuck text is clearable again. Claude Code
text is read only when the cursor lies between the composer's two rules, never
from a past prompt shown above it. The Terminal tab keeps at most one read in
flight and reads only Claude Code, Codex and Kimi Code sessions. A Codex session started in the home folder itself
is not recognized and is cleared in Terminal.

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
running Zerus through `packaging/desktop/update-installed.py`, the only supported live update:
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
`HGS_TRAY_RESTART=0 ./packaging/desktop/install.sh` to stage a GUI update while keeping a running
workspace and its unsaved drafts. The new UI starts on the next launch.

The installer refuses to put an unnumbered build over a numbered Dev
installation (AGENTS.md), right after its build and before it changes anything,
for the live update and `HGS_TRAY_RESTART=0` alike. A numbered Dev installation is
replaced only by a numbered Dev build or by a local preview build: build the
Tauri client with `ZERUS_DEV_BUILD_NUMBER=N` in its environment, N being the CI
sequence of the main push the build comes from, or build an owner-requested
branch preview with `ZERUS_LOCAL_PREVIEW_LABEL`. The label is at most 32
lowercase letters and digits in hyphen-separated words; it needs a clean Git
checkout and no Dev number, and the build shows as
`DEV_VERSION-preview.LABEL.COMMIT`. The Tauri client reads the environment
variable when it builds. Clear the label before building main again. Task #28
removed the Qt GUI; an installed Qt build is replaced by the Tauri client with
the live update, never `HGS_TRAY_RESTART=0`.

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
checks take longer than half a second. An explicit `--hgs` or `hgs=` skips
automatic staging: before testing such a local build, run its
`hgs.exe session-host install` so the stable CLI used by native sessions and
hooks matches the GUI's CLI. Staging preserves running hosts and agents. This
also ensures the session lifecycle can read saved `native:` permission choices.
Settings are in
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

**DeepSeek members' folders.** A DeepSeek session's sandbox needs full control
(WRITE_DAC and WRITE_OWNER) of the folder it confines. When Zerus creates a
member's worktree or clone it adds this user's explicit, inheritable FullControl
entry to that new folder, so the sandbox starts there. The grant is asked for
only where it is needed: `hgs worktrees create --sandbox`, passed for a DeepSeek
member or session. Every other worktree, including the desktop's New session for
Claude or Codex, is created without touching its ACLs, so a share or a file
system without ACL support cannot turn a created checkout into a failed create.
A folder Zerus did not
create (a direct, existing or custom folder) is checked first: when a right is
missing the session does not start, and the member's error — shown in the Lead's
inbox — says `the DeepSeek sandbox needs full control of <folder>` with the
one-line fix `icacls "<folder>" /grant "*<SID>:(OI)(CI)F"`. A member never repairs
ACLs itself: it reports the exact error to the Lead.

**DeepSeek members whose host is starting.** One DeepSeek host serves every
DeepSeek session of a user on a computer. A start launches it when none runs
and waits 35 s for it to answer; a later start that finds a host launched
recently and still running waits for that one instead of launching a second.
When a member's start fails only with `DeepSeek host is still starting`, the
dispatcher starts the member again by itself (a `launch_waiting` event, no
error, no inbox item), up to three times. If the host still does not answer,
the member fails once with that reason and the number of starts, and the
Lead's inbox shows it; any other start failure is reported at once. A relaunch
starts the count again.

**Members' clones.** A clone member's folder is made by `hgs worktrees create
--clone`: a full `git clone --local --no-hardlinks` of the team's repository (no
hard links, because the folder's ACL grant would reach the source's files), then
the member's branch, started from the team's base branch when one is set, else
from the HEAD of the team's folder. The clone has its own 25-minute deadline and
the checkout of the branch its own 10 minutes, so a large repository on Windows
no longer fails with `Git operation cancelled or timed out`. When the clone
finished but the branch was not made, the clone is kept: starting the member
again finishes it (fetching the base when it moved on), provided the folder is a
clone of this repository with no changes and without that branch. Any other
folder there is refused, never changed.

Zerus's own merges into a member's folder (`git-merge-lead`) and into the lead
branch (the merge queue and the accepted-work merge) pass the catalogs' `i18n`
merge driver with `-c merge.i18n.driver=...`; a clone therefore needs no merge
driver in its config, which Zerus refuses on purpose. A person merging by hand in
a clone enables the documented one-line `git config` from `CONTRIBUTING.md`.

**DeepSeek members in their sandbox.** On Windows a DeepSeek member's shell runs
in DeepSeek's sandbox at Low integrity. It reads Zerus state but may not write
it, and the session host keeps it out (a sandboxed caller hears
`this process runs in a sandbox below the session host's integrity level`).
Such a member still does its work through its own folder, where its sandbox
writes, and the team worker applies what it leaves there on its next tick
(about three seconds):

- `hgs team report`, `ask`, `commit` and `hgs team run` wait there as before.
- `hgs team create` of its own scratch team under the owner's standing rule
  (task #228), and its `hgs team` calls for a scratch team it leads (plan,
  approve, relaunch and the other team operations), wait there the same way: the
  worker applies them with the member's identity and checks its profile and the
  rule again. A call outside that scope is not queued - it keeps the sandbox's
  error, so the member asks the lead (task #346).
- An approval request that escalates a sandbox for ONE
  `hgs team <op> --json-file PATH` call in that scope is answered by Zerus once,
  recorded in the lead's feed; another command, a wrapper, a pipe or a call
  outside the scope still reaches the lead. A sandboxed member calls `hgs team`
  directly, one command per call with `--json-file`, never wrapped in a script.
- `hgs run` waits there too. The worker records the run as the member
  session's work, so the session is Building, and starts it at Low integrity
  with its temporary files in `<folder>/.zerus-team/tmp`: a member's run never
  runs above its sandbox. `--wait` and `--list` read the run's record as for
  any session.
- `hgs ls`, `hgs inspect <session>` (with `--skip-processes` or
  `--after N`), and `hgs task list` or `hgs task get` of a named project
  (`--project P` or `PROJECT/T-xxxxxx`) are answered by the worker, which runs
  the read outside the sandbox and leaves the output in the folder. These reads write, send, start
  and attach nothing, and the sandbox already reads every file they read.
  Every other command, such as `send`, `launch` or `attach`, stays closed to a
  sandbox.

**Codex members on Windows.** Codex's Windows sandbox runs a member's commands
as its own local user (`CodexSandboxOffline`). That user reads the hgs state but
may write none of it, and may not read the owner's configuration
(`~/.config/hgs`), which stays the owner's only. hgs recognizes such a process
because the state belongs to another user and the configuration is unreadable.
It then loads no configuration (no peers) and takes the same ways as a DeepSeek
sandbox: the calls above (a scratch team's create and its operations included),
`hgs run` and the reads above wait in the member's folder for the team worker.
The member is the session its inherited `HGS_SESSION` and
`HGS_RUN_ID` name; only the folder the request is written to proves it. A Codex
member's `hgs run` runs in `codex sandbox` on its own folder, never above it:
the command must start in that folder. Codex keeps `.git` read-only, so its
commits go through the `git-commit` template.

This needs a running team: a sandboxed DeepSeek session outside a team is told
so. `hgs self` answers a DeepSeek tool command from the binding of its
`DSH_SESSION_ID`, in or outside a sandbox. The worker runs a member's `hgs run` at Low integrity but outside
DeepSeek's own confinement, so the run, like any Low process, may use the
network and named pipes of Low processes.

A DeepSeek member's own folder is a clone: a worktree's git metadata lives in
the main checkout's `.git`, which its sandbox may not write. A worktree asked
for it (by a role, a template, the New team form or the worktree policy, which
a clone also meets) becomes a clone; an adopted session keeps its folder.

**DeepSeek session-log upload.** Zerus starts and resumes DeepSeek sessions with
the official session-log upload off, unless the owner turns the block off in
Settings. The promise is checked, not assumed: a session whose own records still
accept an upload after its launch is marked, and `hgs ls`, the team status, the
session card and the account on the Accounts page say
`DeepSeek session-log upload still active`. A host that was already running when
the block was turned on keeps the state it started with; a session launched or
resumed into it says
`DeepSeek session-log upload may still be active: the running DeepSeek host was started before the block`
until that host is restarted (which Zerus never does by itself — it would close
the live agents).

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

Private stores (read marks, the analytics store, the approval journal, the
history index, voice storage and the Windows mobile configuration) are checked
before use. On Windows, each missing directory created for private storage gets
a protected, inheritable DACL granting only the current user FullControl. New
files inside it inherit that protection. Existing directories keep their ACLs,
including a directory another process creates during the creation race; private
directory creation does not silently repair them or relax the privacy check.
A store that is this user's, lies in the hgs state or configuration
folder and is only too open (another tool added a group to its ACL, or Unix mode
bits wider than 0600/0700) is made private again in place: on Windows a
protected DACL granting this user FullControl through a handle that never
follows a link, on Unix `fchmod`; the content and the owner stay. Each repair
adds one line to the session host's `host.log` and to `private-stores.jsonl`
(owner-only, the newest 200 lines or 64 KiB: path, kind, the removed accounts
or the reason, time, never content). A store owned by another account, a link
or reparse point, another kind of entry or a path outside those folders is
never changed; it and a store a write failed on are kept in
`private-stores.json` and reported until the store is next found private: in
`hgs status` (with the one-line `icacls`/`chmod` fix), in `hgs inspect --json`
as `private_stores` (empty when none) and in `hgs ls --json` as
`private_stores` (absent when none), from which the desktop raises one notice
per computer, path and reason. The session host checks the known store paths
when it starts, and `hgs status` does too; the listing only reads the current
set.
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
flag and for archives and subagents. A
device acknowledges these values only after it has shown them; `hgs read` still
applies each one only while it is current.

DeepSeek and OpenCode main inspections name the same fields from the view `hgs
read` compares a mark with for that provider, owned by the session's name.
- DeepSeek captures that view from one extra host `list` before it reads the
  page it shows. Its reply is the turn outline's settled response, which the
  host commits at `turn/end` after the turn's final message. From dsh 0.2.2 the
  host's list no longer carries that outline (`summary: false`), so an idle
  row's failure read, the same cached read, supplies it from the session's
  projections; a running or stopped row keeps the outline last read for its
  run. `hgs read` takes its view the same way. Its requests are
  signed through `attention_id` before the page lists its own. The capture's
  host reads, including an idle row's failure read, share a 7-second budget,
  so a stalled host delays the inspection by at most about that much. The
  field is absent when any of those reads fails or runs out of time, or when
  another host generation served the page. A host row without its pending
  requests leaves `attention_complete` false. DeepSeek has no archive
  inspection: `hgs inspect NAME --archive ID` refuses with "DeepSeek archives
  are unavailable" instead of answering for the live session.
- OpenCode names the observation it shows: the forms and permissions it lists
  and the newest completed reply of its history, never a streaming one. A row
  without an observation (no service, a failed read, an OpenCode 1 binding) has
  no reply, and its `attention_complete` is false.

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

A checkout with changes also carries `changes` for the inspector's Changes card:
`{"added":A,"deleted":D,"tracked_files":N,"untracked_files":U,"truncated":bool,
"files":[{"path":"src/a.rs","added":12,"deleted":3},...]}`. The counts compare
the work tree and index with HEAD (`git diff --numstat`); a binary file has null
counts and an untracked file is listed with `"untracked":true` and null counts.
At most 50 files are listed (paths clipped to 300 characters); the totals cover
every tracked change.

`hgs [@host] git-diff --path /absolute/checkout --json` returns that diff as
text: `{"state":"ok","path":...,"diff":"...","truncated":bool,"bytes":N}`, at
most 256 KiB of it (`truncated` and the full `bytes` say when it is cut). Both
commands run Git directly, without a shell or external diff or text converters,
and never write the checkout, its index or its configuration. A diff over the
1 MiB Git output cap answers `"state":"too_large"`.

Subagent count fields distinguish active, completed, total, completeness and
source; unknown totals are null. Up to six active/unreviewed previews accompany
the available full roster. Profile groups without individual IDs remain groups.

`hgs [@host] send SESSION --json` accepts stdin containing a UUID `request_id`,
`text`, `expected_run_id`, `expected_conversation_id` and optional attachments
`{name, mime, data_base64, reference?}`. Unique `[Image #1]` / `[File #2]` references
place attachments at their first marker; otherwise they append. IDs come from a
fresh inspection. `status: submitted` confirms native input delivery, not a model
reply. Saved receipts make identical requests idempotent. An uncertain result
needs manual inspection before another request.

Claude Code attaches a pasted line that is only an image path asynchronously:
until it has read the image its composer is empty with a `Pasting…` footer, and
an Enter then submits nothing (zerus-iitw8). For Claude the single Enter
therefore waits until the composer shows the paste settled, and `submitted`
requires Claude's own transcript to record exactly this message after the
paste: a new typed user row (`native: transcript`) or, while it works, a new
queue `enqueue` (`native: queue`). The row must hold the same lines in the same
order, none added, apart from Claude's image tokens and blank lines, and be
stamped no earlier than the paste. A prompt queued earlier that Claude takes
now (a `queued` user row, a `queued_command` attachment, `remove` or `dequeue`)
never proves a later send. A new conversation with no transcript yet counts
its settled composer emptying after Enter (`native: composer`). Both waits
share one 8-second window from the paste and hold no state lock, so other
sessions' hooks run meanwhile. The send's `input_typing` marker on the session
keeps every other input out until it ends (other sends and session actions,
Terminal input and Clear input). Its term starts again with the window, after
the paste, so slow work before the paste never lets it lapse inside the window;
it never lasts more than the window plus 4 seconds. Before its single
Enter the send takes the lock again, by the same deadline, and checks that its
marker still holds and the composer still shows the settled text. Otherwise the
receipt stays `in_progress` with `delivery: uncertain`, its reason and the
composer's text (at most 2,000 characters, never image bytes), and the error
names that text; it is never retyped. Codex and Kimi show a paste at once, so
their Enter still confirms the receipt. A busy agent's tool events never
confirm the previous input: only an input-acceptance event (a submitted or
queued prompt, a turn or task start) or Claude's own transcript or queue record
of the message lets the next request through. An emptied composer
(`native: composer`) proves the Enter but not that Claude accepted the input. While a busy agent may still hold the previous message in its
own queue, the refusal says so. In the WhiteWh fork (owner, 2026-11-13) the composer's frames read the
pane's own cursor, one cheap terminal read, instead of the console's full input
evidence: under a build's load that evidence took so long that the window read
no composer at all and a delivered paste stayed unsent. The input check still
runs once before the Enter.

In the WhiteWh fork (owner, 2026-11-13) an uncertain Claude delivery can be
confirmed later: `hgs send SESSION --confirm REQUEST_ID --json` reads Claude's
transcript for the same proof (a user row or queue entry carrying the message,
stamped at or after the receipt, from the transcript length the receipt kept as
`transcript_offset`, or from the start for an older receipt). It types and sends
nothing. When the proof is there the receipt becomes `submitted` with
`delivery: late`, its `native` proof and `confirmed_at`, and the command prints
it; otherwise it refuses, as it does for another session, a moved run or
conversation, and a request that never reached the terminal. While the open
session on this computer holds an uncertain send's identity in its draft and no
desktop service takes sends (Windows, and Linux or macOS outside desktop-service
mode, whose service checks its own held sends), the desktop composer runs this
check at once and then after 15, 30, 60 and at most
every 120 seconds while the window is shown. A submitted receipt of exactly that request, session, run
and conversation ends the uncertainty: the identity goes, the field is cleared
only while it still holds that message, and the Activity row shows it sent.
Allow retry stays for every other answer.

The agent confirms an input with an input-acceptance event: UserPromptSubmit at once
for an idle Claude or Codex, UserPromptQueued for Kimi while it works (DeepSeek
confirms in its own request). A working Claude or Codex holds the text in its own
queue and reports it only when that turn ends, so there the text shown in its
queue panel confirms it. Until then a further send is refused, unless the request
sets `queue_when_unconfirmed: true` and the inspection reports
`input_hold_supported`: hgs then holds the message (at most 8 per session, in
order) and answers `status: queued` under its request ID. One deliverer per
session sends each held message through the same checks once the previous input
is confirmed; `held_inputs` in the inspection lists those still waiting. A held
message's receipt ends `submitted`, `cancelled` or `failed`. `hgs [@host]
send-cancel SESSION --json` with `{"request_id"}` cancels one still held; a
cancelled message is never sent. After an uncertain delivery nothing more is sent
automatically, and a held message is never replayed. Dialogs, drafts and unknown
prompts still refuse a direct send; open the terminal there. `agent_id` is
supported only for verified DeepSeek continuable children. Questions carry
request identity/hash
through `pending_questions`; answers use the exact unchanged request.

An inspection indexes the Codex question transcript and recovers earlier
submitted answers from this session's question receipts within one shared
one-second budget, saving its progress so the next inspection resumes. Its
recent messages never wait for that work, and it never scans a long transcript
or every question receipt unbounded: recovery reads one bounded page of the
session's receipts at a time through the receipt locator. While the index or
recovery is incomplete, or held by another process, `pending_questions_complete`
is `false`, `pending_questions` lists no optional Codex question and
`pending_questions_error` says the questions are still being indexed. That is
unknown, never an empty list, and nothing becomes answerable from it. A
completed earlier check grants nothing: each inspection and each answer checks
the locator's current coverage again from its head. A receipt indexed since is
recovered, and a gap the locator records (a receipt it cannot attribute) makes
`pending_questions_error` say earlier answers cannot all be checked. An answer
first completes the index and recovery, and refuses while earlier answers
cannot all be checked. An older hgs omits `pending_questions_complete`.

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

An inspection that fails when a subscription begins (the session state lock is
busy, say) is reported as a non-fatal `inspect_failed`; the subscription stays,
and its first frames follow as soon as an inspection succeeds, read again after
1 s and then at doubling waits of at most 8 s. A client never subscribes again
for that. Only `session_changed` refuses the subscription.

Earlier Activity is read by position, as pages:

```json
{"op":"subscribe","topic":"activity","session":{"name":"claude/sample-project/review","run_id":"RUN"},"before":123,"tail":100}
{"v":1,"seq":57,"t":"older","topic":"activity","key":"run:RUN","events":[],"cursor":88,"last":true,"more":true}
```

A subscribe with `before` belongs to a session the stream already follows. It
leaves that subscription, its cursor and its `state` as they are, inspects
nothing, and answers with `older` frames: the newest `tail` events before that
`seq` (100 by default, at most `limits.activity_older`, 200), oldest first, in
one or more frames. The frame with `last` ends the read; its `more` says whether
earlier events remain, and `cursor` is the `before` of the next page, also when
every row of the page was left out. A failed read ends with an empty last frame
and `more`, so the client stops waiting and may ask again. A server whose
`hello` has no `limits.activity_older` ignores `before` and would subscribe
anew: a client asks for a page only when that limit is above 0, and otherwise
reads earlier events by subscribing with an earlier `after`. Journal sequence
numbers are shared by every session, so a client never estimates how far back a
session's earlier events lie.

The stream's loop answers controls and sends Activity frames; the catalog's
slow reads never run on it. A whole listing, the rows derived again between
listings, the probe of the terminal and agent processes, and the rows of
DeepSeek and OpenCode sessions (read from their hosts, which may take a host's
whole request limit to answer) each run on a thread of their own, at most one
of each at a time: a slow host or a held session state lock delays those rows
only, never subscriptions, controls or Activity frames. A row derived again
while a whole listing ran is newer than the listing's: it stays when the
listing lands and is derived once more, so a slow listing never brings back an
answered approval or a finished turn. A pass of the loop that takes a second or
more leaves a `watch-loop` line with the milliseconds of each step in
`logs/slow-calls.log`.

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

The `team` topic follows every team of the computer's team store:
`{"op":"subscribe","topic":"team"}`, one per stream. Its rows are keyed like
the organization's: `team:<id>` is the team's head `{id, name, lead, status,
revision, updated}`, and `teams` is `{count, truncated}`. Every change of a team
(a command, the dispatcher, a member's report) raises its `revision`, so a
client reads a team again with `hgs team status`, `inbox` and `events` when its
revision moved instead of polling them. At most 1000 heads go out, the most
recently changed first; `truncated` says more exist. A worker thread reads them
with one indexed query and never writes. It reads again when a stat of the
store, its rollback journal or its write-ahead log changes and at least every
30 s, at most once a
second. The first rows go out as a paged `snapshot`, a later change as one
`delta` of the changed rows and removed keys. A delta the client is too far
behind to take becomes `gap` (`overflow`) and a fresh snapshot once it caught
up. A read that fails, such as a store held by a writer, sends a non-fatal
`team_failed`. A peer whose `hello` does not list `team` answers the
subscription with `unsupported_topic`; read its teams with `hgs team` as
before.

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
Processes tab keeps its own poll interval. Until a newly selected session's
first read arrives, each refresh also inspects it directly, and selecting the
followed session anew subscribes again from its latest window, so Activity and
Send never wait on the stream alone. When the stream offers the `git`
topic, it also follows the checkout folders of that computer's sessions, the
selected one first, then shown rows, at most 64, instead of starting
`git-status` for each every 15 s. A followed value counts as checked every 15 s
while the stream follows it; its remote's age keeps counting, so a remote shows
verified for at most 60 s after its check. Refused folders, and all folders of
a computer whose stream is not live, are polled as before. When the stream
offers the `team` topic, the Team tab's reads of that computer (the team list,
Waiting, and the watched team's status, inbox and feed) run when a team's
revision moved, plus once a minute, instead of every 3 s; until the topic's
first complete snapshot, after a `gap` or `team_failed`, while the stream is
down, and on a peer without the topic they are polled every 3 s as before.
Listings are applied at most every
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
### Affected tests (U62)

A project names its test suites in `.zerus/tests.toml`; Zerus reads it, and the
owner may override any field in the team settings (`settings.tests`, an object
shaped like the file and merged over it, providers by name; only the owner sets
it). Without the file `hgs test config` shows a proposal from the stacks it
finds (Cargo, CMake/CTest, pytest or unittest, Cython with `setup.py`,
MSBuild with a `.sln` or `.vcxproj`, Unreal with a `.uproject`, Gradle, npm)
and nothing is enforced: a team without a configuration keeps today's flow.

- `hgs test map --write` writes the committed map (`.zerus/test-map.json`) from
  the build: a Cargo package's module graph (each module's tests, widened to
  the modules that use it, `include_str!` files included); CMake's file API
  reply, the CTest files and the include graph for each test binary (its
  functions too); and for Python, Gradle and npm the files a test names or
  imports (Cython's `.pyx`, `.pxd` and `.pxi` too). For MSBuild (task 137) a
  file belongs to the projects whose item lists name it (`ClCompile`,
  `ClInclude` and the others; macros and wildcards are skipped); a test
  project (a native unit test or Google Test project, or one the provider's
  `tests` name) answers for itself and every project it references, and a
  project no test project reaches is its own target, checked by its build.
  `[[provider.paths]]` adds path rules, with targets or with the tests
  that mention a text (such as the CLI binary).
- `hgs test map --check` fails on a tracked file outside every provider's area
  and `map.ignore` and on a tracked test file no provider maps; the CLI job runs
  it. Untracked files never count (#741).
- The map is read from the sources when it is used (#741): `--check` computes
  it whole, `affected` recomputes the providers the change touches. The
  committed `.zerus/test-map.json` is a snapshot that stands in only for a build
  this checkout cannot read (the Qt build in the CLI job), so a merge that
  changes sources never leaves the check failing or the selection behind; how
  far the snapshot lags is printed, and `--write` refreshes it on a computer
  with the configured build.
- A CMake project's map comes from the build's file API reply, so it needs a
  configured build: configure once (`hgs team run` with `cmake-configure`),
  run `hgs test map --write`, then configure again so the next
  reply covers the map's own test targets. Without the first configure there is
  no reply to read; without the second, a new test file waits until the next
  configure.
- `hgs test affected [RANGE|--staged|--files ...] [--full] [--json]` prints the
  commands to run with their estimate from `.zerus/test-durations.json`. A file
  the map does not know runs its provider's whole set; a file in no area is
  named, never skipped. `--full` prints the whole set and its time; the
  full-run policy (`[full]`) says who may start it. Each run also carries its
  `hgs team run` request where a template expresses it: `cargo-test` with
  `bin`, `manifest` (another crate), `filters` and `skips` (up to 512 each).
  Every other kind names its own templates (task 137; the Qt GUI's `qt-test`
  went with Qt, task #58), with the build its tests need as `team_build`,
  sent first:
  - ctest: `cmake-build` (`build_dir`, the configuration a multi-config
    build's binary folder names, the chosen `targets`), then `ctest` with the
    chosen tests (`tests`, or an anchored `regex` for other names);
  - pytest: `py-test` with the chosen test files as `paths`;
  - cython: `cython-build` when an extension source (`.pyx`, `.pxd`, `.pxi`,
    C or C++, `setup.py`) changed or for the whole set, then `py-test`;
  - a Python provider with a `root` (task 153) passes it as both templates'
    `project`: the environment (`.venv`, `venv`, uv, Poetry) and `setup.py`
    are that folder's and the commands run there; it must be a folder inside
    the member's folder, its real path too. Paths stay member-relative and
    the JUnit report and pytest cache stay in the member's `.zerus-team`;
  - msbuild: `msbuild-build` of the chosen test project (its configuration
    and platform), or of `manifest` for the whole set, then `vstest` with its
    assemblies; without a test project the build is the run;
  - unreal: `ue-build` of `<project>Editor` (Win64, Development), then
    `ue-test` with the chosen automation tests, or the module prefixes for
    the whole set.
- `hgs test durations --record LOG --provider NAME` keeps the slowest measured
  time per target from CTest, QtTest, Cargo (and libtest JSON), unittest or
  pytest output, or a JSON object of test ids to seconds.
- `hgs test coverage --provider NAME --report TARGET=FILE` compares one
  target's coverage (lcov, Cobertura or llvm-cov JSON) with the map: a covered
  file the map does not give the target is a gap. The weekly
  `test-map-coverage.yml` workflow measures every Rust module and Qt binary.

#### Reused results, flaky suites and budgets (#718)

What the test queue makes of its results, by the project's thresholds:

- Reuse. A suite's input hash is what its map names now: the content hashes
  of its targets' files (`reuse = "inputs"`, the default of most kinds), or a
  Qt test binary's own bytes and its data files (`"binary"`, ctest's default;
  the sources it compiles count once they are built). Rust is never reused
  (`"never"`, cargo's default: it runs in seconds when filtered). An unforced
  request whose inputs match the suite's last green measured run records a
  `reused` result from that run, due for re-measuring; a reused result is never
  the source of another.
- Full run. When `[reuse] full_after` results were reused since the last full
  run, or a reused measurement is older than `max_age_days`, the lead's inbox
  and the owner's Waiting for you show the proposal with its estimate from the
  recorded times. `{"team":ID,"action":"full-done"} | hgs team test --json`
  records the run and counts again from it.
- Flaky. A plain failure is retried `[retries] flaky` times; a pass on a retry
  is `flaky` (its result keeps the hidden failure's excerpt), recorded in the
  suite's history and quarantined for `quarantine_days` (shown to the lead and
  the owner until it expires). Every `episodes` episodes become one lead item
  to file a task; dismissing it (`"action":"dismiss","kind":"flaky","suite":S`)
  counts newer episodes only. A crash (an abnormal exit code, a sanitizer,
  heap corruption or segmentation report) is never retried.
- Budgets. A run slower than its recorded time by more than `[budgets]
  grow_percent` is flagged `slower` on its result and the lead's card; one over
  its time budget (`budget_minutes`, else `suite_minutes`) is flagged and a lead
  item; a suite whose test lines pass its size budget (`budget_lines`, else
  `suite_lines`) is a lead item when it is submitted.
- Reports. Every report records the test lines and test functions its change
  adds and removes against the lead's commit (the project's test globs and the
  Rust files with tests), on the lead's card.
- Coverage. `hgs test coverage` also names the targets without unique coverage;
  the monthly run of `test-map-coverage.yml` reports them.

#### One integration line (#267)

Owner, 2026-11-08: with an `integration_folder` the team has one line. An
accepted task's commit lands on it at acceptance through the merge queue, in
acceptance order (below). A member's new attempt starts from that line's head,
never from its own previous branch or another member's commit: the task
branch is created from the integration branch, and a folder that gets a new
task with uncommitted work commits it on its old branch first, then moves to
the integration head (only when it is clean after that). Zerus no longer merges
predecessors' commits into member branches: the `dependencies` prepare step
reads "skipped" with that reason, so "a member's accepted commit conflicts"
cannot occur; the team's base is named once, by the integration branch.
`hgs team status` carries `integration`: `branch`, `head`, `landed`
task/commit pairs and every accepted task's `unlanded` state and why (queued,
testing, conflict with its `resolve` task, or no landing queued); the lead's
inbox lists each with an `unlanded` item, and an accept answers with its
`landing`.

#### Landing by hand on a named line (task #19)

Owner, 2026-11-10: a lead who reviews and merges by hand names the team's
line, `{"manual_line":"BRANCH"}` through `hgs team settings` (the lead may set
it; the name is a plain branch name). With it:

- the merge queue is off for the team whatever the project's tests file says:
  an accept answers `accepted`, queues no landing and runs no suites;
- `hgs team landed` takes a commit on `BRANCH` of the team folder's
  repository, whatever that folder has checked out (it may be on another
  team's detached line), and refuses one off it naming the line; accepted work
  whose commit reaches the line is recorded as landed by itself (task #443);
- members merge `BRANCH` with git-merge-lead, and hand-outs bring a member's
  branch up to it (task #15). Without a manual line, a team folder on no
  branch merges the team's `base_branch` (else its recorded line) instead of
  refusing "The folder is on no branch"; with none named the refusal says to
  set `manual_line`;
- the playbook's rules (a), (b) and (h) raise nothing, nor for a team created
  with working mode `none`;
- the lore takes a task's landed commit, else its accepted report's commit,
  when Zerus made none: no "accepted with no commit Zerus made" item.

#### Nothing blocks creating a team (task #13)

Owner, 2026-11-10: the path from New team to running members has no refusal
without a way through. What changed on that path:

- Members created at once share the repository's worktree lock, which
  `worktrees create` waits for only five seconds. A member's creation refused
  with "Another worktree operation is running" is tried again for up to five
  minutes (nothing was reserved); after that the member's error says Zerus
  starts it again on its next tick and that `hgs team relaunch` starts it now.
- The lead relaunches a member whose session failed to start (no session and
  a launch error) itself, with no card; so also a member a swap or relaunch
  left without a session whose new one has not started yet. While that start
  waits for the replaced session, the receipt's `waiting` says what holds it.
  A running member's relaunch is still the owner's card.
- `add-to-phase` by the lead of a running or paused team is the lead's own
  operation (task 162). The CLI no longer asks for the owner's word first,
  which `propose` refused as the lead's own operation: a loop with no way
  through. A word the lead does send is still checked.
- A lead's draft value for an owner's setting is an offer (above).
- Create team with **Keep the settings as they are** (working mode `none`)
  records the owner's Keep of the project's preset (task 336), so the playbook
  and the working-mode block do not ask about the settings again.
- A model or effort change the agent did not visibly confirm no longer blocks
  sends or the planning brief: task #7 (settings evidence) covers it.

#### The working mode follows the project (#270)

Owner, 2026-11-08: "a small project, code first - the branch pipeline pays; a
huge one with binaries - one folder with soft file locks". Zeros profiles the
project when a team is planned and proposes a preset, so the mode is a decision
it makes, not one the lead must remember.

- The **project profile** reads the tracked tree of the team's folder (bounded:
  at most 200,000 entries, a 10 s Git budget): the tracked files and bytes, the
  binary files by extension and their share, the files of 5 MiB or more, Git LFS
  (`.gitattributes` with `filter=lfs`, or `.git/lfs`), the engine markers
  (`.uproject`, `.uplugin`, `Content/`, `Binaries/`), the test subsystem's
  suite providers and whether `[merge_queue]` is on, and the folder's branch
  (the integration line at planning).
  Task #451 (owner, 2026-11-09): files under `design/`, a docs image (an image
  extension under `docs/`) and files under `tests/fixtures/` are
  **previews** - they count in the tracked files and bytes, never as binaries,
  large assets or engine markers. A source repository with many suites and source
  files is never proposed one shared folder because of its design previews,
  docs screenshots or fixtures.
- The **presets**:
  - `branch-pipeline` - a branch per task cut from `base_branch` (the
    integration line), the `code` pipeline (review -> tests), accepted work
    landing through the merge queue. It pays for a code repository whose tests
    run: no LFS and no engine markers, and at least one test suite provider.
    Task 336 (owner, 2026-11-08): when the source files are the majority by
    count and the tests run, the preset stays `branch-pipeline` until the
    binaries are far above the size floor (2 GiB of tracked bytes or 1 GiB of
    binaries); the binary share alone never flips it (design previews and
    fixtures live in code repositories). Without that code-first shape the old
    rules hold: 512 MiB of binaries, a 20% share, 20 or more large assets, LFS
    or engine markers choose the shared folder.
  - `branch-per-task` - the same branches and base without a pipeline, for code
    with no test subsystem.
  - `shared-folder` - one folder for every member, the soft file locks per task
    output (task 155), no branches or merges, the build broker for builds. It
    pays for a binary or asset project, and task #451 requires a real signal:
    Git LFS, engine markers, 20 or more large assets (binaries of 5 MiB or
    more), 2 GiB or more tracked, 512 MiB or more of binaries or a binary share
    of 20% or more - with the previews above never counted.
- **Writing it**. The settings a preset writes are `branch_per_task`
  (`task/{id}`), `base_branch` and `folder_policy` (`worktree`) for the branch
  presets, and `folder_policy` `shared`, no branch per task, the build broker for
  `shared-folder`. `hgs team draft` carries `working_mode`: the proposal (preset,
  title, reasons, settings, profile) and the preset the owner accepted so far
  (`auto` is the proposal); the draft's `update` action sets
  `working_mode` to a preset, `auto` or `none`. Create team writes it into the
  settings; an explicit setting the owner named wins, and a folder that is not a
  Git repository keeps the plain flow. `hgs team status` carries `working_mode`
  with `preset`, the `proposal` and `disagreeing` (what the settings still miss).
  A create request may name `working_mode` (`auto` is the default for a Git
  repository).
- **Keep this team's settings** (task 336): the owner records the choice for
  the project's profile as it is now (`{preset, fingerprint, at}` on the team;
  `hgs team keep-working-mode --json`, `forget-working-mode` to drop it). The
  disagreement block and the playbook's mode item stay away while the preset
  and the profile's fingerprint (files, bytes, source and binary counts, large
  assets, suites, LFS, merge queue) are unchanged; a material profile change,
  or another preset's keep, brings the proposal back. Accepting a mode card
  forgets the keep.
- **A running team** whose settings disagree with its profile gets ONE owner
  card from the lead (`kind:"working-mode"`, the preset, its settings and the
  reasons), created at a safe point only: the team runs or is paused and no
  attempt is open. The owner's Keep (above) silences it until the profile
  changes materially. The card is not repeated for the same preset; an Allow writes
  the preset's settings (the owner setting the mode directly may also use
  `hgs team settings` for the fields the lead cannot change). A card whose mode
  is already in force, or whose project now profiles to another mode, withdraws
  by itself.
  Task #451 (owner, 2026-11-09): the block's own press answers. **Propose the
  switch** is the owner's own `hgs team propose` (`kind:"working-mode"`,
  `preset:"auto"`), so it may propose without the lead; hgs re-reads the
  project's profile, replaces the cached proposal the block showed with it, and
  answers `{"status":"proposed", ...}` with the card, or
  `{"status":"already", "working_mode":...}` when the settings already follow
  the project (no card; the block leaves by itself). A refusal shows its reason.
  The tick re-reads a cached profile older than ten minutes
  (`MODE_CHECK_SECONDS`) while a block is showing, so a block left over from an
  older profile or an older hgs stops proposing the wrong mode. Keep hides the
  block only after hgs kept it; a failed call keeps it with its reason.
- The lead guide (`hgs guide team-lead`) is size-bounded; the presets and their
  reasons are read from the draft and `hgs team status` instead.

#### The lead playbook and the brief (#271)

Owner, 2026-11-08: "let us make sure such mistakes in running a project do not
happen; let Zeros itself propose the methods of running it and remind you after
a compaction".

- **The playbook** (`src/state/teams/playbook.rs`) is the team's working-mode
  preset (task 270) plus nine rules: (a) the mode its project needs; (b) accepted
  work lands on the integration line; (c) no member idles while ready work
  waits; (d) a task names files or module paths, not a whole folder; (e) the
  lore (project memory) has a task shipping its binary in the package; (f) one
  session, one team; (g) a report's tests are run before the accept; (h) a task
  starts from the integration line; (i) no real-format token or secret in a task
  text or a report. It is stored on the team (`playbook`) at create and from a
  draft, and the lead brief carries it.
- **The check** is read-only and cheap: every rule is evaluated from the team
  record (settings, cached mode proposal, tasks and attempts, merge entries,
  reports, decisions), never from Git, the session listing or the file system,
  so it runs on every inbox or status read. Each breach is ONE `playbook` item
  with the rule, what it sees and the exact fix command; a breach clears when
  the team record no longer shows it. What only the owner may decide is marked
  in the item ("the owner's decision"): the working-mode preset's settings, and
  a leaked secret.
- **The brief** is short plain text: the working mode and the base, the rules,
  the open breaches with their fixes, the owner's standing decisions from the
  lore, the integration line's last landing, and which members have no work. It
  is sent to the lead at team start, at each new phase, and when the lead's
  session or context shows a compaction or a clear (a new run or conversation,
  or a context reading well below the one the last brief saw, at most once per
  change). `hgs team brief --lead` (or `--json`) prints it on demand; the same
  payload carries the playbook and the breaches for the clients.

#### The merge queue (#760)

With `[merge_queue] enabled = true` (the owner may override it), the lead's or
the owner's accept of a report does not decide it at once: it answers
`merge_queued` with a `landing` (`{state:"queued",line,head}`), the task stays
undecided with a `merge` entry (status shows it) and the worker takes the queue
in accept order, one merge under test at a time. A check, a phase build, a
sub-team's task and work done in the lead's own folder are decided at once as
before.

- The worker merges the commit the author reported (`folder_head`; a clone's
  branch is fetched first) into the lead's folder: a fast-forward or a merge
  commit "Merge accepted work from MEMBER (SHA) (#N): TITLE", hooks and the
  repository's code-running config off. A lead folder with uncommitted changes
  makes it wait, with that note.
- Task #267: a conflict is aborted and becomes ONE resolve task for the
  reviewer (the lead when the team has none), with the two commits and the
  conflicting files (`merge_resolve_task` in the feed); the conflicting landing
  keeps its `merge` entry in state `conflict` and stays undecided, and no later
  landing runs while one conflict waits. Once the resolve task is decided, the
  landing is back in the queue and retries even when nothing else changed; its
  commit is on the line already when the resolution landed it, and the queue
  then keeps it without merging again.
- A clean merge queues the task's affected tests of `prev..merged` in the
  test queue (forced, never reused) on the merged commit. All green keeps the
  merge and accepts the task; any failure resets the lead branch to `prev`
  (only while it is still the merge) and sends the task back with the failed
  suites, their records and the end of the failing log. A selected suite whose
  declared platforms exclude this computer is not queued here: with a
  configured test runner of its class it is dispatched there and waited for as
  before; without one the merge records it as "not run here: SUITE runs on
  PLATFORMS; no runner of that class" and decides on the suites that ran. The
  merge view, the feed and the merge log name both the suites that ran and the
  ones not run here. Each kept merge advances the queue's tested point for the
  suites that ran, or when nothing was selected; a suite not run here keeps its
  own range open, so it cannot hold the tested point of the suites that ran, and
  the phase build names it and that range. On Windows each queued command
  reaches `cmd /D /S /C` as one unescaped string, so a provider command that
  quotes its folder (`cd /d "tests" && ...`) runs as written; Linux and macOS
  keep `sh -c`.
- An entry whose `merged` is reachable from HEAD is never merged again: the
  queue first tries the rollback
  while HEAD is that merge; once HEAD moved it selects and runs the recorded
  `prev..merged` tests on HEAD. Success keeps the merge normally; a test or
  selection failure returns the task and records one typed untested merge range
  on that task (not a suite in `merge_not_run`). Repeated returns widen the one
  range. A later merge selects the union of its own files and open ranges on
  its HEAD, so overlapping tests run once. A range closes only after every
  affected suite ran successfully on a HEAD containing it, or an accepted phase
  build proved that exact named range. A build says "untested merge of task #N,
  range X..Y"; newer ranges not named by that build remain open. Selection and
  build snapshots are bounded to one batch of 500 ranges; each task has at most
  one persisted open range. Old synthetic `merge-N` records migrate with their
  original start point. If selecting the tests or
  reading the runner config fails while HEAD is still the merge, the queue rolls
  it back at once and requeues it with the reason; a rolled-back entry retries
  only when HEAD or the tests map changed, or when the lead accepts or retries
  it. A failed rollback itself retries only for a changed HEAD or an explicit
  lead accept/retry, even if the tests map changes. A second selection error on
  such a retry returns the task. While a merge is testing, reject and cancel
  first say "its tests run; wait for the merge queue's result". Otherwise they
  are refused with "the merge is still in the lead branch; retry, or
  revert it by hand" while the merge is reachable; once a reset or revert makes
  it unreachable they clear the entry. Only a `merge-base --is-ancestor` exit of
  1 means not reachable; an unreadable answer keeps the entry with its reason.
  Saved `waiting` entries become `queued`; saved `reverting` entries are dropped
  when the task is Pending or Cancelled, or the merge is gone, and otherwise
  become queued. Reachable untested work is retained as a range. Queue waits
  carry a serde-default typed reason; Busy filtering and inbox guidance never
  inspect the wording of the diagnostic. Missing or future reason values use
  generic guidance, so old saved states still load.
- Each kept merge advances `merge_tested` for the suites that ran (or when
  nothing was selected), and the bounded per-suite points keep a suite not run
  here at its own last run. A phase build checks
  `hgs test affected MERGE_TESTED..HEAD` and the not-run suites from their own
  points to HEAD.
- With the queue on, every hand-out tells the member to merge the lead branch
  into its folder before it starts (the git-merge-lead template).
- Task 174: the lead branch is the branch checked out in the owner's
  `integration_folder` (an owner-only team setting, an absolute path), else in
  the team's folder. The queue merges there, the test queue selects and runs
  suites there (also `hgs team test`), and git-merge-lead merges from there. A
  set folder that is missing, belongs to another repository or has a detached
  HEAD holds the queue with that note (a Busy wait); nothing is merged. A
  phase's successor branch is checked out in the same folder, or the owner
  names a new one. A member working in the team's own folder is merged too,
  when the integration folder is another checkout.
- Task 174: queued suites run on a thread of their own per team, never in the
  worker's tick, so a merge's long suites never hold up hand-outs; the merge
  queue reads their results as before. With `[build_cache]` on, the queue's
  runs get the same sccache wrapper as the members' cargo templates.

#### Remote test runners (#732)

The owner makes a Machine a test runner of one platform class:
`hgs machine set ALIAS --json '{..., "test_runner":"linux"}'` (windows, linux,
macos or android; empty is none; a disabled Machine runs nothing). The map
gives each provider its classes (`platforms` of the provider; a Gradle
provider without them is `android`), carried by every queued request.

- A suite with no class, or one this computer serves (its own platform, and
  android with `ANDROID_HOME` or `ANDROID_SDK_ROOT` set), runs here as before.
  Any other goes only to an enabled runner of its class; with none, or with
  every such runner unreachable, it stays queued with the reason (`hgs team
  test --json` action `queue` shows it) and never runs on another platform.
  The merge queue does not wait for such a suite when no runner of its class is
  configured: it records it as not run here (see the merge queue above) and
  decides on the suites that ran; a suite of this computer's class always runs
  here.
- The scheduler reaches a runner only through hgs's own SSH transport:
  `hgs @ALIAS test runner --json` (BatchMode, connect timeout, the Windows peer
  channel; the peer needs this hgs, marker `test-runner-v1`). It first asks
  which commits the runner's mirror has, then sends the exact commit as a git
  bundle of only what it lacks (no reverse connection, nothing pushed).
- The runner keeps a mirror and run folders below its own Zerus state
  (`test-runner/<project>`), checks the commit out into a fresh folder, builds
  the suite's command from that commit's `.zerus/tests.toml` (a command it is
  sent is never read), runs it with the project's isolation by construction
  and its fingerprint check, answers exit, duration, excerpt and anything it
  touched of the person's own paths, and removes the folder.
- The result goes to the same database with its `runner`; the retry, crash,
  slower and budget rules are the ones of a local run, and a run that touched
  the person's paths fails.

#### Missing tools (#742)

A tool the tests or builds need and this computer lacks is offered to the
owner, never installed by an agent and never left to fail quietly. What a
project needs: the build cache's `sccache` and `ccache` (`[build_cache]`, with
the owner's override), each provider's runner (cargo, ctest, python, pytest,
npm, a JDK for Gradle) on the platforms it runs on, and its `[[tool]]` entries
(`name`, `why`, `saves`, `platforms`; `programs` and `install` by manager for a
tool Zerus's catalog does not know). `hgs test tools [--json]` lists them for
this computer with their state; `hgs test affected` and a template run whose
build cache misses its tool (the run's note says so) record the offers too.

- Each missing tool is offered once per computer and project (every worktree
  of a repository is one project): an item of Waiting for you (`kind`
  `tool_missing`, action `decide_tool`) with the tool, why it is needed, what
  it saves and the install command of this computer's manager: `winget
  install --id ID --exact --scope user` on Windows, `brew install` on macOS,
  `sudo -n pacman -S --needed --noconfirm` or `sudo -n apt-get install -y` on
  Linux, else `cargo install --locked` or `python -m pip install --user`.
- The owner answers with `{"offer":ID,"allow":true|false,"by":"human",
  "reason":"..."} | hgs team decide-tool --json` (the desktop sends the same).
  Only the owner decides: an agent session is refused. Deny is remembered for
  that project on that computer. Allow starts the install as the owner's
  operation in a detached runner (`hgs test tools --install ID`, which refuses
  any offer the owner did not allow); its exit and output are recorded, a
  failed install shows them and may be allowed again or denied, and a tool
  found later leaves the list. The offers live in the state folder's
  `tests/tool-offers.json`.
- Agents never install tools: every hand-out and the team guides say to work
  without it and to propose it on the question card.

The file's fields: `version = 1`; `[map]` `file`, `durations`, `unmapped`
(`report` or `all`), `ignore` (never hides a file the map knows, such as a text
a module compiles in); `[full]` `where` (`here`, `ci`, `runner`), `policy`
(`owner`, `standing`, `anyone`), `target`; `[retries]` `flaky`,
`quarantine_days`, `episodes`; `[budgets]` `grow_percent`, `suite_minutes`,
`suite_lines`; `[reuse]` `full_after`, `max_age_days`; `[isolation]`
`enabled` and the keys under "Test isolation"; `[build_cache]`; `[merge_queue]`
`enabled`; `[resources.NAME]` `cpu`, `ram_mb`, `gpu`, `exclusive`; and each
`[[provider]]`: `name`, `kind` (`cargo`, `ctest`, `pytest`, `cython`,
`unittest`, `gradle`, `npm`, `msbuild`, `unreal`, `command`), `root`, `sources`, `tests`, `manual`, `elsewhere`,
`affected` (`build`, `paths`, `all`), `[[provider.paths]]` (`files` with
`targets` or `mentions`), `command` (`{targets}`, `{manifest}`, `{build_dir}`),
`manifest` (cargo's `Cargo.toml`, required for msbuild's `.sln` or project
file and unreal's `.uproject`), `widen` (`none`, `dependants`, `transitive`),
`build_dir`,
`resources`, `platforms`, `budget_minutes`, `budget_lines`, `reuse` (`auto`,
`inputs`, `binary`, `never`), `disabled`.

CTest providers may also declare optional `configure`, `build` and `probe`
command strings. `configure` and `build` are required together when preparation
is enabled; `probe` is optional. These stages run only in the **local merge
queue**. Providers without preparation retain the existing behavior; remote
runners are unchanged.

The queue reads the provider from the merged checkout. It first runs `probe`,
then runs `configure` only when the provider's `build_dir/CMakeCache.txt` is
missing. It always runs `build` before CTest, including for an existing build
folder. An incremental build therefore uses the merged sources instead of
running old binaries. Probe, configuration, build, CTest and any retries share
one existing suite deadline and bounded output capture. Configuration/build
failures after a successful probe fail the suite and follow ordinary merge
rollback, with a `build` flag and no flaky-test retry. A probe failure, missing probe executable or unresolved placeholder
records `not_run` with a bounded `not run here: SUITE runs on ...; reason` message.
It remains an untested range, never a pass or a green result eligible for reuse.

Stage commands expand `{build_dir}` relative to the provider's `root`. In the
build stage, `{targets}` means `--target` followed by the selected CMake target
names. A whole-provider `(all)` selection expands it to nothing, so the default
build includes a newly added test even if the previous map did not name it.
That fallback may build other default targets too. CTest test names and CMake
binary target names need not match: the existing map already records their
relationship. The CTest command keeps its existing test-name selection.

Named placeholders such as `{qt_prefix}`, `{cc}`, `{cxx}`, `{python}` and
`{toolchain}` obtain their values only from the local file
`.zerus/tests.local.toml`. The file is ignored by Git; if `git ls-files` finds it
tracked, Zerus refuses it. Unknown providers, keys or malformed values are also
refused. Its only provider tables are `vars` and `env`, for example:

```toml
[provider.tray.vars]
python = "python3"
toolchain = ""
cc = "cc"
cxx = "c++"
qt_prefix = "/example/sdk/qt"

[provider.tray.env]
QT_QPA_PLATFORM = "offscreen"
QT_QUICK_BACKEND = "software"
```

Use the local machine's actual SDK and compiler values. On Windows the local
`toolchain` value can invoke `scripts/ci/msvc-env.py`. Local `env` entries replace
the entire variable; they perform no expansion. Have the toolchain wrapper
prepend the selected Qt kit's `bin` directory to `PATH` before invoking tools,
or supply the complete `PATH` through local `env` for CTest. Local
values cannot replace command strings or the reserved `build_dir`/`targets`
placeholders. The local file is limited to 64 KiB, with at most 64 vars and 64
environment entries per provider, and is excluded from exported project
configuration. Commit portable recipes and placeholders, never machine paths.


#### What the team enforces (U62)

A member runs only the tests the task's change touches (`hgs test affected`)
and never a full suite; a run over 10 minutes asks the lead first. The
hand-out and the team guides carry this rule (`hgs guide team-tests`). A phase
integration runs the affected tests of the phase's range.

The full run happens only before a release: the owner hears its estimate from
`hgs test affected --full` and confirms; in the fork CI it is the candidate's
run. A team setting `full_tests` says who may start one (`never`, `ask` or
`allowed`; `ask` by default), and the owner may grant one run bound to a
release; `hgs team run` refuses a full-suite template without that grant and
names the estimate and the command that asks. A `cargo-test` is the full suite
only when it names no `filter`, `bin`, `package` and no non-empty `filters`:
the request `hgs test affected` prints for a crate (its `filters`, `skips` and
`manifest`) is narrowed; `skips` or a `manifest` alone are not. `hgs team status` shows the
policy and an active grant. Zerus computes a task's affected set from its own
diff and accepts its report only with that set green on the merging commit. Every
test is weighed by real need, speed first (owner, 2026-10-31): a run earns
its place only if it can fail for the change; lint and screenshots ride on the
integration's and the test's own run. A split keeps every test, counted before
and after; a repeat becomes a case of a table test, and a test that guards
nothing is removed with the reason in the report.

### Iron rules

Iron rules are stored by Rust at four cumulative levels: project, team, member,
and session. A lower level adds requirements and cannot cancel a higher level.
Each level allows at most 20 nonempty rules of 300 characters each. Team settings
use `settings.iron_rules`; members use `iron_rules` in their plan or
`members_update`. These fields default to empty for old records. `project_id`
is the stable project identity; the legacy team `project` remains its label.
Give teams a stable `project_id`; an old label resolves only when unique.

Only the owner changes rules. A lead may change its team's rules, its project
rules or a member session's rules with a verified, unused `owner_word` from the
owner's recent message in that lead session (the same journal verification as
phase approvals). Merely writing `by: human` from an agent does not authorize it.
A member cannot edit any level. A settings or plan update that changes rules
passes this gate too. The GUI edits project/session rules in Iron rules, team
rules in Team settings and member additions in the member editor.

`hgs [@host] rules get|set|migrate --json` is additive (`iron-rules-v1` in help).
Input is one JSON object on stdin. Project input names `scope: "project"` and
`project: "STABLE_ID"`. Session input names `scope: "session"`, `name`,
`expected_run_id` and `expected_conversation_id`; all identities must match the
current binding. Get returns `rules` and `revision`. Writes add a canonical
`request_id`, `expected_revision` from get, and a bounded `rules` array. Owner
writes use `by: "human"`; a lead also supplies `team` and `owner_word`.
Project requests are replay-safe; session requests replay the last exact write,
while stale revisions or replaced identities are refused. Project rows and
request receipts live in `HGS_STATE_DIR/iron-rules.sqlite3`; session additions
remain in the exact native binding; team/member additions remain in the team.
Migration imports the GUI's `workspace/ironRules` once per project and computer.
It never overwrites an existing Rust row, including an intentionally empty list.
The GUI retains its old value as a backup until and after a confirmed receipt;
a failed import remains retryable. Old CLIs retain the legacy per-send fallback.

Rules accompany every team task, including a pipeline stage, regardless of the
catalog's prompt order or disabled rules block. Claude Code receives all levels
through root SessionStart `additionalContext` on startup, resume, clear and
compact, combined with the hint/task commands in one JSON response. Ordinary
messages omit the wrapper only after startup delivered the current effective
rules for the selected project. Changed rules, a missing or mismatched project,
or missing startup proof keep the fallback until the next successful start.
The current Codex, DeepSeek, Kimi and OpenCode adapters retain per-send delivery;
they have no equivalent verified startup/resume/clear/compact delivery path in
this integration. SessionStart never injects rules into subagents or ordinary
message hooks. Team terminal startup introductions also carry inherited rules.
Remote task delivery reads the exact remote session's own rules before sending;
an unreadable new peer refuses delivery rather than omitting its rules.

For a remote session, the lead-local read-only resolver accepts `host` with the
exact session identities in `rules get`. It joins indexed local team membership
with the remote binding's own rules. Remote team Claude sessions retain per-send
delivery: their host cannot independently restore the lead's team/member rules
at startup. GUI project rules belong to this GUI computer's Rust project store;
session writes go to the selected session's host. No rules synchronization or
remote startup contract is implied by this resolver.

Iron-rules compatibility and failure handling:
- Use the public `hgs @host rules ... --json` route; forwarding probes
  `iron-rules-v1` and never requests a terminal or agent launch. Windows uses
  the guarded peer channel or a guarded SSH command.
- A startup proof belongs to the exact run and conversation and is invalidated
  at every accepted main SessionStart before resolution. Foreign starts cannot
  invalidate the parent's proof. Resolution errors are logged without losing
  the ordinary hint/task context; they keep per-send delivery.
- The combined startup context is written and flushed before the confirmed
  binding is published. Its rules proof is part of that same atomic record
  update, with no later proof-only write after a fresh conversation is visible.
  Early invalidation remains in place: failed output leaves no proof and does
  not publish a fresh binding as started.
- Claude Code's [hook output limit](https://code.claude.com/docs/en/hooks#json-output)
  is 10,000 characters per context string. Zerus accepts startup proof only for
  at most 6000 UTF-16 units of rules and 8000 units of combined context. Larger
  lists retain the per-send fallback. All remote sessions retain this fallback,
  including non-team Claude; local project rules remain mandatory even if the
  peer has a different row under the same project identity.
- A failed GUI migration keeps sending the legacy project rules and retries
  migration later. It does not authorize dropping session or inherited rules.
- For an ambiguous legacy project label, delivery fails closed and the team's
  inbox shows an iron-rules blocker. The owner resolves it with
  `hgs team settings --json`, giving the team's exact `team`, a stable
  `project_id`, `by: "human"` and a canonical `request_id`. A lead cannot change
  this association by claiming to be the owner.
- An older hgs that rewrites a team record does not preserve unknown
  `iron_rules`/`project_id` fields. Keep team writers on a rules-aware hgs;
  additive deserialization alone cannot protect these fields from old writers.

### Project memory

Project memory (task 126, owner 2026-11-07) turns RustCodeLore on for a whole
project, a project of the organization catalog with all its folders. It is off
until the owner turns it on in the project's settings (Projects in Zerus Next
and in the Qt GUI). The store is a lore root (`lore.toml`, records in
`lore/*.jsonl` and `lore/plans/*.jsonl`, team workspaces under
`teams/<team8>/`, each its own lore root, and the search index under
`.cache/index.sqlite`, ignored by git). Its default folder is
`HGS_STATE_DIR/lore/projects/<project id>`; Browse chooses another while the
store holds no records. As docs/lore.md section 2.5 has it (the owner's answer
2), a store outside any git working tree, the default among them, is its own
private git repository, made as CodeLore's `private.rs` makes it: `git init`
on branch `main`, the local identity `Zerus <lore@zerus.invalid>`, the marker
`zerus-lore-store` in its git folder, `commit.mode = "plain"`, no remote, and a
first commit "lore: a new store"; RustCodeLore commits every write. Only an
empty or new folder is initialized. A folder inside another git working tree
(a project's) is never initialized and gets `commit.mode = "shared-tree"`. A
team workspace in a Zerus store is `plain` and is committed into the store's
repository ("lore: a new team workspace"). A store with records is moved, never repointed: after
the owner confirms, hgs copies every file to an empty or new folder, compares
each file's size and SHA-256 with the source, checks that the source did not
change meanwhile, and only then switches. A failed check removes only the copy
it made and switches nothing. The old store is kept and listed in `moved_from`;
Zerus never deletes it. A move to a folder outside any working tree takes
the store's repository and history along (a store without one gets its own,
"lore: the store moved here"); a move into another working tree leaves the
repository behind and switches the store to `shared-tree`. Turning memory off keeps the store as it is. The
status shows the records (nonempty lines of the record files), the files and
their size, and the index state: `none`, `built`, or `stale` when a record
file changed after the index.

`hgs project config get|set|move|find --json` (`project-memory-v1` in help) reads
one JSON object on stdin naming `project` (the stable identity). Get returns
`enabled`, `store`, `default_store`, `default`, `revision`, `moved_from` and
`status` (`exists`, `records`, `files`, `bytes`, `index`). Set takes `enabled`
and/or `store` (a full path, "" for the default); move takes `to`. Find (task
#397) takes `query` and an optional `limit` (5 by default) and searches the
project's own records and its team sub-stores (`lore find --stores all`), so a
team's workspace is found from the project: every hit names its `store`
(`project`, or `team NAME`), and `notes` carry what lore reports, the model
download included. Writes are
the owner's (`by: "human"`; an agent session is refused) and carry a canonical
`request_id` and `expected_revision` from get; the same request replays its
receipt, and a stale revision is refused. Settings and receipts live in
`HGS_STATE_DIR/project-memory.json`, per computer, like project iron rules.

A team's memory is the owner's setting `settings.memory` (task 127,
docs/lore.md 4.1): `mode` is one of `plain`, `off`, `team`, `project` and
`read-only` (task 126's `lore` still reads as the default RustCodeLore mode).
New team, in Zerus Next and the Qt GUI, offers what the project allows: with
the project's memory on all five, `project` preselected; with it off `team`,
`plain` and `off`, `team` preselected. `project` and `read-only` without the
project's memory are refused by name. Nothing is created when New team opens
or a draft is saved; Create team (or a team created without a draft) makes the
store:
- `project`, and `team` while the project's memory is on: a shared
  `workspace` `<store>/teams/<team8>` in the project's store, committed into
  its repository (`source` `project`);
- `team` without it: the team's own private store under
  `HGS_STATE_DIR/lore/teams/<team8>` (`source` `team`);
- `read-only`: the project's `store`, and the workspace when one exists;
- `plain` and `off`: no store.
A team created without `memory` keeps task 126's rule: a workspace when the
project's memory is on, else plain memory. A lead's settings that leave
`memory` out keep the owner's choice; only the owner changes it.

`hgs team switch-memory-mode --json` (`{team, mode, reason, request_id}`) is
the owner's switch of a team's mode (docs/lore.md 4.2). It applies in one
team update: a switch to `team` or `project` creates or attaches the
workspace; a switch away from them keeps the workspace on disk and named, so
switching back attaches the same records. It writes one `memory_mode_changed`
event. A finished team keeps its memory. The lead proposes the same as a card
(`{"kind":"switch-memory-mode","mode":"...","reason":"..."} | hgs team propose --json`):
the team keeps `memory_mode_offer` and `memory_mode_reason` (both in
`hgs team status`) until the owner allows it or denies it
(`memory_mode_denied`, with the reason). Trying the card touches no store.

Task 128 (docs/lore.md 6): a team in `team` or `project` mode writes its
memory. The team worker's background writer (never the dispatcher tick) puts
each approved phase's plan and its tasks' outcomes (done with commits,
dropped with the reason and who), the owner's decisions on cards, faults for
fixed review findings and for reworks (the reviewer's exact reason) and the
newest passing test reading per template into the workspace, at most 50
changes per pass, through the `lore` binary (`HGS_LORE`, beside hgs, then
`PATH`; without it the team keeps plain memory and journals
`lore_unavailable` once). A fault Zerus cannot complete waits as a
`lore_missing` lead inbox item; refusals journal `lore_refused`. The team's
`lore` field keeps the last pass's hash, the waiting records and the refused
ids. The lead's inbox message ends with the workspace's open items and the
newest decisions, at most 10 lines. Reads, from a member's session or with
`--team ID`: `hgs team lore find WORDS` (the workspace, the project store and
its team sub-stores, each hit marked with its store: `project` or
`team NAME`), `hgs team lore plans` (with the
open items of the newest three phases, phase 1 included) and
`hgs team lore show ID`; a sandboxed member asks the lead, since brokered
reads run in the worker's tick; JSON
forms `lore-find {team, query, n}`, `lore-plans {team}`, `lore-show {team,
id}`. A finished team's workspace gets `.closed` and stays searchable;
`hgs team lore export --team ID` (the owner only) copies its faults,
decisions, readings, topics and recipes into the project store's area
`team-<team8>`, skipping ids already there.
