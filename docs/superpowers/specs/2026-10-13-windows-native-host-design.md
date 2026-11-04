# Windows Phase 2: native session host

Status: approved direction, implementation starting, 2026-10-13. Tracking:
`zerus-ziq3.7` under the Windows epic `zerus-ziq3`. Phase 1 (the control console)
is described in [Windows support](2026-10-10-windows-support-design.md).

The owner asked on 2026-10-13 to plan and implement Phase 2 and delegated the
design choices. This document records those choices, the evidence behind them,
the spikes that must confirm the riskiest assumptions on a real Windows PC, and the
order of work. Facts were checked against `885eb7d`. Two adversarial reviews
(Windows systems; Unix compatibility and migration) were folded in on 2026-10-13.

## Goal

Agents (`claude`, `codex`, `kimi` and untracked commands) run natively on a Windows
computer under `hgs.exe`, with the same session model as Unix: a session survives
GUI restarts, GUI updates, terminal disconnects and the end of the SSH connection
that started it; it can be listed, inspected, sent to, attached, paused, resumed,
forked, renamed and terminated with the same CLI and GUI operations.

Unix behaviour stays byte-for-byte compatible: the same tmux argv in the same order,
the same stdin bytes, the same dry-run strings, the same JSON, the same files.

## Non-goals for Phase 2

These stay out and get their own work later (Phase 3 and beyond):

- Windows as an SSH *target* of other peers (remote shell detection, per-shell
  invocation, Windows paths in the shared swarm catalog, `machine setup` for
  Windows peers). `zerus://` references to Windows sessions resolve on that computer
  only.
- Windows as a mobile gateway (connector journal, `mobile_peers`, relay settings,
  desktop presence for phone alerts).
- DeepSeek (`dsh`) and its Native UI, voice-message workers and voice sends to local
  Windows sessions (the GUI disables them with an explanation).
- Installer, code signing, autostart at logon, winget, WSL as a target.

## Decisions

### D1. One binary, a hidden host entry

The host is `hgs.exe __host serve`, the same executable as the CLI, in the style of
`--macos-session-service` (`src/main.rs:26-28`). A separate binary would duplicate
the protocol, configuration and identity code.

### D2. Explicit enablement, a stable CLI and staged host images

Local sessions on Windows are off until the user enables them, from the GUI
("Enable local sessions" on the Machines page for this computer, D16; labelled a
preview until 2026-10-14) or with `hgs session-host install`. This keeps
home-directory writes explicit, as for `zerus-setup` on Linux and macOS. Enabling:

1. **Stages the stable CLI** `%USERPROFILE%\.local\bin\hgs.exe`, the Windows form of
   the Unix `~/.local/bin/hgs` contract. Hooks, session recipes, supervisors and the
   recovery worker use only this path (D8). An update renames the running file aside
   under a unique name (`hgs.exe.old-<timestamp>-<random>`) and moves the new one in;
   a running image is never overwritten, and aside files are deleted by a later
   install once no process uses them. A hook that starts in the few milliseconds
   between the two renames fails and is reported like any failed hook.
2. **Stages a content-addressed host image**
   `%USERPROFILE%\.local\share\hgs\host\<sha256-16>\` holding `hgs.exe` and, when
   bundled, `conpty.dll` and `OpenConsole.exe` (D6), then writes the pointer file
   `host\current` last. The next host start uses the image `current` names; there is
   no "newest by version" rule, so a rollback is a re-install of the older build.
   Unused images are removed by a later install when no host runs from them.
3. **Writes the durable marker** `%USERPROFILE%\.config\hgs\local-sessions`. This
   marker, not the presence of staged files, decides that the computer is enabled.

`hgs session-host status` reports the marker, the staged versions and the running
host; `hgs session-host stop` stops an idle host and refuses while sessions run;
`hgs session-host uninstall` refuses while sessions run, removes the marker and
leaves staged files for a later cleanup. The `session-host` command exists only in
Windows builds (on Unix that word still launches a program of that name, as today),
is allowed before enablement (`src/cli.rs:355`), and is never forwarded to remote
machines in Phase 2.

**Admission (M3, `src/host/gate.rs`).** `__host serve` serves only when the marker
exists and its own executable is a staged image (`<images>\<name>\hgs.exe` whose
SHA-256 starts with `<name>`); otherwise it exits before writing anything (4: not
enabled, 5: not staged). A running host refuses `create_session` once the marker is
gone, so a launch racing `uninstall` starts nothing. A launcher reads `current`
again before each start, as an install may retire the image it read. Tests run an
unstaged build only for an isolated state directory: one that `HGS_STATE_DIR` names
and that is none of the user's default state directories (every hgs process
exports `HGS_STATE_DIR`, so the variable alone proves nothing). There
`HGS_HOST_ALLOW_UNSTAGED=1` lets the host serve without the marker and from any
image, and `HGS_HOST_IMAGE` names the executable `__host call --start` starts;
elsewhere the first is ignored and the second is an error
([testing the host](../../windows-host-testing.md)). The processes of such a test
host's sessions inherit the override: their `--run` supervisors and `__state`
tracking hooks run without the marker, while every other command, and both of these
without the override, stay refused as on a Phase 1 control console.

The compile-time `platform::LOCAL_SESSIONS` (`src/platform/mod.rs:27`) becomes a
run-time check: always true on Unix; on Windows true when the marker exists. Its
consumers (`src/cli.rs:695,742,766`, `src/accounts.rs:456,883`) call the function.

`local_sessions` in `hgs ls --json` means "this computer is enabled", never "the host
is running": saved sessions live in the state directory, and a host crash must not
turn the computer back into a control console. On Windows the local box also carries
`local_host` with `state` (`not-installed`, `stopped`, `running`, `unavailable`),
`pid`, `protocol`, `conpty` (`bundled` or `inbox`), `integrity` and `windows_session`.
Unix output is unchanged. Implemented in M5 (`host::admin::Host::listing_json`): every
key is present, the five facts are null unless a host answers, an `unavailable` host
adds `error`, and a stopped host of a computer without the marker is `not-installed`;
the control-console box carries it too.

**One CLI over the state directory.** Once enabled, the GUI runs the stable CLI, not
the `hgs.exe` in its own folder, from its next start. At start, a Windows GUI asks its
bundled `hgs.exe` for `session-host status`; on an enabled computer it refreshes the
staged CLI and host image with `session-host install` when the bundled build differs
(SHA-256) and is at least as new as the staged one (a staged build of unknown version
counts as older; the user consented to staging when enabling), then runs every `hgs`
call through the stable CLI. An explicit `--hgs` or `hgs=` setting is used as given.
These checks run before the tray icon exists (status for up to 15 s, the refresh for
up to 60 s): once they have taken half a second, a small frameless note that takes
no focus says "Starting Zerus…" (and, during a refresh, that it updates the hgs
command line) until they end. A second start meanwhile is handled once the tray runs.
A GUI that enables local sessions keeps its CLI until it restarts. State formats stay
readable in both directions across one version of skew.

### D3. Host lifetime: like the tmux server

- The host starts on demand from a launch, resume or fork, never from a listing or an
  attach. A listing with no host reports no live sessions, as tmux does with no
  server. "No host" means only `ERROR_FILE_NOT_FOUND` on the pipe; every other
  failure (busy past the retry budget, a protocol mismatch, a refused server
  identity) is an error, never an empty snapshot, because an empty snapshot would
  show live sessions as resumable (`src/state/mod.rs:247-260`,
  `src/state/lifecycle.rs:679-701`). The backend's `listing` and
  `attached_counts` are fallible for this (M4.1); tmux keeps answering as before.
- **One host per user and state directory.** The host holds an exclusive lock on
  `host.lock` in the state directory for its whole life; a second host exits at once.
- The host exits after its last session ends and no client is attached, after a
  short linger. It closes its listening pipe instances *before* deciding to exit; a
  client that loses that race gets `ERROR_FILE_NOT_FOUND` or a broken pipe before the
  hello completes, and retries the start. A launcher that cannot create the first
  instance because an exiting host still holds it retries until that host's process
  is gone.
- `create` is idempotent by launch ID, so a retried request never starts a session
  twice. `launch_status` (capability `launch-status`) answers by the same ID how
  that session runs or ended (its exit code and the last lines of its pane). A
  launch watches its new session for 3 s, the tmux launch's "ended immediately": a
  program that could not start ends it at once, before any record exists, and the
  launch then fails with that exit and output instead of reporting a start that the
  GUI would wait for (zerus-ziq3.10). A supervisor that fails only after those 3 s
  (a `--version` probe that times out) is still reported as started.
- The host owns its sessions: a host crash ends them, exactly as a tmux server crash
  does. The host joins its own kill-on-close job before the first session and fails
  closed if it cannot, so its death never orphans agents (Orca does the same).
- The host has a hidden console (`CREATE_NO_WINDOW`), so console children it starts
  outside a pseudoconsole (`git`, probes) never flash a window. A hidden top-level
  window receives `WM_QUERYENDSESSION`/`WM_ENDSESSION` to record a clean shutdown;
  the host and supervisors ignore `CTRL_LOGOFF_EVENT` and `CTRL_SHUTDOWN_EVENT`,
  which do not say which user is logging off.

### D4. Breaking away from the launcher's job

- **Spawning** uses raw `CreateProcessW` with `STARTUPINFOEXW`, because stable Rust
  std (MSRV 1.85) cannot attach a pseudoconsole, limit handle inheritance or resume a
  suspended child, and always inherits handles. hgs builds environment blocks itself:
  overrides merge case-insensitively (`Path` against `PATH`), the block is sorted
  case-insensitively, and the `=C:` drive entries are kept (precedent:
  `tray/src/PtyProcess_win.cpp:79-91`).
- **The launcher** starts the host with `CREATE_BREAKAWAY_FROM_JOB |
  CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT` and
  `bInheritHandles = FALSE`.
- **The host verifies its own placement.** Breakaway moves a child up the job
  hierarchy until it reaches a job that forbids breakaway, and `CreateProcess` still
  succeeds, so a check in the launcher cannot see that failure. The host checks
  `IsProcessInJob(GetCurrentProcess(), NULL) == FALSE` before it joins its own job and
  reports the result in the hello; the launcher fails closed with a clear message
  when the host is in any job, with one exception. Some desktops run every process
  of the interactive session in a job that forbids breakaway, `explorer.exe` first
  (reported on Windows 11 Home 26H2, build 26300, 2026-10-17; zerus-rwuf). There
  no host could ever start. When the job the host stopped in also holds the shell
  of its desktop (the process of `GetShellWindow`, in
  `JobObjectBasicProcessIdList` of that job), the host and its sessions end no
  earlier than the desktop, which already ends them at sign-out, so the hello
  reports `desktop_job` and the launcher accepts the host. An agent's sandbox, a
  terminal or a CI step holds no shell, and a host over SSH has none, so those
  jobs are still refused.
- `hgs.exe`'s own kill-on-close job (`src/platform/windows.rs:63-83`) gains
  `JOB_OBJECT_LIMIT_BREAKAWAY_OK`. Because the MSYS runtime of Git for Windows breaks
  away whenever it may, every bounded child (`git`, `ssh.exe`, `tar.exe`, probes)
  runs in its own per-call kill-on-close job *without* `BREAKAWAY_OK` through the
  bounded runner (M1.3); the immediate job then forbids escape.
- There is no WMI fallback. Orca removed its own: WMI process creation is refused to a
  standard user's network logon (`stablyai/orca@5e9489c0`,
  `src/shared/windows-breakaway-launcher.ts:1-8`).
- Win32-OpenSSH places a session's processes in a job that allows breakaway, so a
  host started over SSH survives the end of the SSH session (spike S4 confirms, with
  the tray's attach and swarm-worker jobs in the test matrix).

### D5. Endpoint and protocol

**Name.** `\\.\pipe\hgs-host-<hex>` where `<hex>` is the first 32 hex digits of
SHA-256 over the user SID, the case-folded canonical state directory and a random
secret read from the private file `host.secret` in the state directory (created on
first use). Isolated tests (`HGS_STATE_DIR`) never reach a live host; `HOME` and
`USERPROFILE` spellings of one directory map to one host; another local user cannot
compute the name to squat it. The name carries no protocol version.

**Integrity.** The host runs with the token of the launcher that started it. A
high-integrity host labels its pipe `S:(ML;;NWNR;;;HI)`. The host authenticates each
client with `ImpersonateNamedPipeClient` and `OpenThreadToken`: same user SID, and an
integrity level at least the host's; a lower-integrity client is refused with a clear
message ("the session host on this computer runs elevated"). The host reports its
integrity level and Windows session ID in the hello and in `local_host`, and the GUI
warns before launching into a host that runs in another Windows session (for example
one started over SSH; implemented 2026-10-14): when the local machine is Windows
(D16), its `local_host` runs and reports a `windows_session` other than the GUI's
own (`ProcessIdToSessionId`), New session, Resume, Restore, Resume all, Start fresh,
starting a stopped session to send and Fork on this computer ask "Launch anyway?"
first. Cancel preserves drafts and dialogs. Remote machines, Linux and macOS GUIs,
no running host (the launch starts one
in this session) and the same session never ask (`tray/src/LocalSessionHost.h`,
`LocalLaunchWarning.h`). Elevated SSH testing uses an isolated `HGS_STATE_DIR`.

Terminal attachment requires a supported adapter's verified live pane and running
process. A saved paused or stopped binding without that pane, or an archived
binding, cannot be opened directly in Terminal. Use the explicit Resume or Restore
action first; these actions retain the confirmation above. Opening Terminal never
resumes or restores a saved session implicitly.

**Security.** The first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE` and
`PIPE_REJECT_REMOTE_CLIENTS` and a protected DACL granting access only to the user SID.
The client checks the server before sending anything: `GetNamedPipeServerProcessId`,
then the server process token user must equal its own SID.

**Framing.** Byte-mode pipe, frames of `u32` little-endian length (at most 1 MiB),
one kind byte and a payload: `0` JSON request or response, `1` stream bytes, `2`
stream control JSON (resize, detach, exit, notice).

**Versions.** The hello carries the client's protocol range; the reply carries the
host identity (`pid`, creation time, image path and hash, protocol, capability list,
integrity, Windows session, job placement). Protocol 1 is the first. Changes are
additive within a major version. A host accepts every protocol major a released CLI
may send, and a CLI speaks every major a running host may still use, in both
directions (a rolled-back CLI against a newer host included).

**Requests** cover the tmux vocabulary used today (62 call sites, about 70
invocations): create session, list sessions and panes, resolve a target, pane info,
capture (plain or styled), send keys, paste (bracketed or not), run marker and
session environment, rename, kill, signal, input state, attach, list clients, host
status. On Windows, `list` is one round trip, because hooks call `live()` on every
event (`src/state/hooks.rs:283`). A listed pane's `current_path` (tmux's
`pane_current_path`) is the directory its session started in: another process's
current directory lives in its memory, which the host does not read, so an untracked
session keeps its folder and a shell that changes directory is still listed where it
started (`zerus-ziq3.9`).

### D6. ConPTY: bundled pair first, inbox as degraded fallback

- The host loads `conpty.dll` by absolute path from its image folder with
  `LoadLibraryExW(LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32)` and
  resolves `ConptyCreatePseudoConsole` and friends with `GetProcAddress`;
  `conpty.dll` finds the matched `OpenConsole.exe` next to itself. The pair comes from
  the pinned NuGet package `Microsoft.Windows.Console.ConPTY` (MIT), verified by
  SHA-256 in the packaging script, and ships in the portable zip with its license.
- Before loading the pair the host reads `OpenConsole.exe`'s import table: when a DLL
  it imports at load time (API sets aside) is neither next to it nor in System32,
  the server could not start (1.25.260930003 imports `D3DCOMPILER_47.dll`, which
  Server Core lacks), so the host uses the inbox API and logs why.
- Without the pair the host falls back to the inbox `CreatePseudoConsole`, looked up
  with `GetProcAddress` like `ReleasePseudoConsole` (a static import would stop
  `hgs.exe` loading before 24H2), and reports `conpty: inbox`. Features gated on mode
  evidence then fail closed, as they do on Unix without it.
- Never pass `PSEUDOCONSOLE_INHERIT_CURSOR`. `ClosePseudoConsole` runs off the reader
  thread. The end of a session is detected by waiting on the supervisor's process
  handle, because without `ReleasePseudoConsole` the inbox output pipe never reaches
  end of file.
- **The host is the only responder to terminal queries.** tmux answers device and
  cursor queries itself and never relays them. The host answers forwarded `CSI 6n`,
  `CSI c`, `CSI > c` and OSC 10/11 from its screen model, removes them from the
  stream sent to attached clients, and drops replies coming from clients, so a query
  gets exactly one answer whether zero or several clients are attached.
- Spike S1 (Windows 11 25H2, pair 1.25.260930003) found that both backends forward
  DECSET 2004, 1049, 1 and the mouse modes and add their own `?9001h` and `?1004h`.
  Inbox ConPTY answers DA1 and DSR CPR itself and never forwards OSC 10/11; the
  bundled pair forwards DA1, CPR, OSC 10/11, XTVERSION, `CSI ? u` and `CSI ? 996 n`
  and sends its own DA1 at start. The host therefore answers, from the screen model,
  exactly the queries tmux answers and stays silent on the rest, as tmux does. The
  bundled pair stays first: on older inbox builds mode passthrough is missing, and
  only the pair gives the host every query.
- **Input is plain VT, but sent Ctrl+letter keys reach a program that reads key
  records as win32-input-mode records.** The host notes DECSET/DECRST 9001 in the
  session's output (and still strips it from what clients see, so their terminals
  keep their keyboard encoding); the mode is off until requested. A key send
  (`send_keys`) whose key tmux writes as one C0 byte for Ctrl and a letter (`C-c`,
  `C-u`, `C-s`, `^C`; not `C-h`, `C-i`, `C-j`, `C-m`, which ConPTY maps to
  Backspace, Tab and Enter) is written as `CSI Vk;Sc;Uc;1;8;1 _` and
  `CSI Vk;Sc;Uc;0;8;1 _` (key down and up: the letter's virtual key and US scan
  code, the lower-case Latin letter as the character and `LEFT_CTRL_PRESSED`)
  only when the console has requested 9001 and the session's supervisor, asked
  once per such send over its channel with the `input_state` timeout, reports an
  input mode without `ENABLE_VIRTUAL_TERMINAL_INPUT` and without
  `ENABLE_LINE_INPUT` (Codex: 0x0098). Otherwise the C0 byte goes as before:
  without a supervisor (untracked sessions), when it does not answer in time or
  does not report the flag, and for programs that read VT input (Claude Code,
  Kimi and the fake agent show 0x0208; `ssh.exe` reads VT input too). ConPTY
  gives such a program a key record's character alone, so a record would reach
  it as `c`; a cooked read would take it as typed text too. Literal text,
  pastes, query answers and what attached clients type are never converted.
  The reason is the keyboard layout: ConPTY turns a plain C0 byte into a key
  record whose character is the control character, and crossterm, which Codex
  uses to read key records, resolves such a record through the foreground
  window's layout, so on a Russian layout Ctrl+C reached Codex as Ctrl+`с` and
  was ignored (zerus-c8r9, probed with Codex 0.162.0 and Claude Code 2.1.296 on
  the bundled pair). With the Latin letter Codex sees Ctrl+C. Typed input of an
  attached client passes unchanged, as Windows Terminal would send it, so Codex
  under a non-Latin layout behaves there as it does in Windows Terminal itself:
  a known limitation of crossterm upstream, not of the host. Implemented in
  `src/host/keys.rs` (`win32_ctrl_letter`), `src/host/channel.rs`
  (`key_records`), `src/host/sequences.rs` and `src/host/session.rs`; the
  supervisor's evidence carries `virtual_terminal_input` (absent from older
  supervisors, which keeps the byte).

### D7. Screen model and capture

Each session feeds its pseudoconsole output into hgs's own screen model
(`src/screen`, merged in M2), which follows tmux's grid, input and reflow logic;
its only dependency is `unicode-width`. `vt100` 0.16 was evaluated and rejected
because it cannot store what tmux prints (`31m` versus `38;5;1m`, strikethrough,
bold with dim, tab cells, a line's allocated length). The model answers what
`tmux capture-pane -p` and `-p -e` answer today:

- visible screen only, every row, trailing spaces trimmed;
- one character per wide glyph while the cursor counts cells;
- SGR only, carried across rows the way tmux does, `\e[0m` when an attribute is
  removed, 256-colour and truecolour forms as tmux writes them; OSC 8 is not kept
  (consumers only strip it);
- pane facts: size, cursor, alternate screen, bracketed paste (DECSET 2004),
  application cursor keys, mouse modes.

A differential test (`tests/test_screen_model.py`) feeds the same byte streams to an
isolated tmux and to the model and compares both capture forms, 22 pane formats and
query replies byte for byte, plus each stream's repaint; it runs in the CLI jobs on
tmux 3.7c and Homebrew tmux. The corpus includes the raw ConPTY streams of Claude
Code, Codex and Kimi recorded on Windows in spike S1
(`tests/fixtures/screen-streams/`), so the model is proven on the bytes it will
actually see. On attach the host repaints the client from the model.

### D8. Launch without a login shell

On Unix the pane runs `$SHELL -lic 'exec env … "$0" --run "$@"' hgs 0 argv` so the
login PATH applies and the supervisor becomes the pane's root process. On Windows:

- the creating client sends its whole environment block plus the session overrides,
  and the host starts the stable CLI's `hgs.exe --run <argv>` with that block. A
  long-lived host never hands out its own stale `PATH`. The client first removes the
  variables of a session it may run in (`TMUX`, `HGS_HOST_*`, `HGS_SESSION`,
  `HGS_RUN_ID` and the other launch identities, `HGS_TRACKING`, `HGS_CLIENT`): tmux
  starts a pane from its server's environment, which never holds them. An override
  with an empty value removes the variable. `--run` runs an untracked program as its
  child, since Windows has no `exec`;
- **the launching client sets `HGS_EXECUTABLE` to the stable CLI** (decided in M4.1:
  the client already resolved the stable CLI as the supervisor's program, and the
  protocol keeps the host from adding environment of its own), and on Windows
  `executable_path()` and `runner_executable()` (`src/cli.rs:190-200`,
  `src/state/storage.rs:305-321`) fail closed if they would resolve to anything else:
  they answer the stable CLI whichever hgs.exe runs, and refuse an `HGS_EXECUTABLE`
  that names another file or a computer where it is not installed.
  Hooks, recipes and records therefore never embed the portable folder's path, which a
  GUI update replaces (`tray/src/AppConfig.cpp:71-73`). A test launches from a
  portable copy and checks that the session runs the stable CLI;
- the account environment that `run_script` applies on Unix after the login shell
  (`src/accounts.rs:919-944`: `HGS_ACCOUNT_ID`, `ANTHROPIC_CONFIG_DIR`, blanked
  `AUTH_ENV` keys, agent home variables) is among the session's own variables
  (`accounts::run_environment`), after the others;
- `--run` keeps its role: it is the supervisor, the pane's root process (`pane_pid`);
  it installs hooks, writes the record and starts the agent;
- the supervisor is created suspended, assigned to the session job (D9), then resumed;
- the environment adds `TERM=xterm-256color` and removes `COLORTERM`. Spike S1
  showed that Claude Code 2.1.296 and Kimi Code 2.1.1 emit truecolour on Windows
  even with `FORCE_COLOR=2` (Codex 0.162 emits 256-colour indices), so the host sets
  no `FORCE_COLOR`. The question parsers match exact 256-colour indices
  (`src/state/claude_question.rs:140,156`); on Windows they compare colours after
  mapping truecolour to the xterm-256 index (S1: `#999999` is 246), checked against
  the S3 dialog fixtures in M4;
- `HGS_HOST_PANE` and `HGS_HOST_SESSION` replace `TMUX_PANE` and the session name
  lookup (`src/state/lifecycle.rs:95-117`); the nested guard checks them as it
  checks `$TMUX` today. It refuses only what would attach the console there
  (`hgs a`, a launch, fork or resume without `-d`); detached ones start beside
  the enclosing session.

**Agent resolution.** `PATHEXT` lookup (`platform::which`) finds the program. An npm
shim (`.cmd`) is resolved to `node.exe <script>` only when it matches the exact npm
shim shape, all or nothing: a long `cmd.exe /c` line carrying a prompt is scored as
obfuscation by Defender for Endpoint (Orca's evidence) and breaks multi-line
arguments. Native executables run directly. Windows PATH preparation adds
`%USERPROFILE%\.local\bin` and `%APPDATA%\npm` when present, the counterpart of
`platform/unix.rs:22-43`. The `sh` command means PowerShell (`pwsh.exe`, else
`powershell.exe -NoLogo`) and stays untracked; it runs under `--run` like any other
program, because a root process started as a new process group ignores Ctrl+C and
only the supervisor turns it back on (D11). Process-name checks compare file stems
case-insensitively (`node.exe` is `node`; `src/state/external_agents.rs:15`,
`src/state/input.rs:568`, `src/state/processes/control.rs:267-281`).

**Accounts.** The local account catalog, login, inspection and permission modes work
on Windows; every direct `HOME` read (`src/accounts.rs:379,390`,
`src/accounts/claude.rs:31`) uses `platform::home()`.

### D9. Process ownership and identity

- **Jobs.** Each session has its own job, without `KILL_ON_JOB_CLOSE` and without
  `BREAKAWAY_OK`. The host's own kill-on-close job (D3) contains all session jobs. A
  supervisor started by the host does not create its own job; a reused job must never
  receive a process that already sits in its own job (verified on 2026-10-11: such an
  assignment makes later assignments fail with `ERROR_ACCESS_DENIED`).
- **Identity.** `process_start` on Windows is the creation time as decimal 100 ns
  ticks, compared for equality like the Unix `lstart` text; records are per machine.
  A pinned process is an open handle (`SYNCHRONIZE | PROCESS_TERMINATE |
  PROCESS_QUERY_LIMITED_INFORMATION`), which also blocks PID reuse. Parent links from
  Toolhelp32 are trusted only when the parent was created before the child.
- **IDs.** Sessions and panes keep the textual forms `$N` and `%N` with host-unique
  counters, because records, rename transactions and question hashes store them and
  validate their shape. The terminal binding hashes the host PID and creation time,
  as it hashes the tmux server's today.
- **Exit evidence.** NTSTATUS crash codes and `STATUS_CONTROL_C_EXIT` map to a
  completion reason without changing Unix values; the `--run` fallback test
  (`src/cli.rs:445`) treats them as a crash, not as "nothing to resume".
  Implemented in M4.2 (`platform::ntstatus_exit`): known statuses read as the
  signal of the same meaning (`STATUS_CONTROL_C_EXIT` is SIGINT, 130), any other
  error status Windows defines (severity "error", customer bit clear, such as a
  missing DLL) and unhandled C++ and .NET exceptions read as SIGABRT (134), and
  everything else, `exit(-1)` included, stays an exit code. The `--run` fallback
  also keeps a resume attempt that a stop request reached, whatever its code.

### D10. The supervisor channel and input evidence

The supervisor is the only process that is both hgs code and attached to the session
console, so it is the host's agent inside the session. At start it connects to the
host and keeps a **supervisor channel** open; the host forwards stop requests and
input-state queries over it.

On Unix, sends require the pane's tty to have `ICANON` and `ECHO` off, the agent to be
the foreground process group and not a shell (`src/state/input.rs:541-574`), plus
bracketed paste on. On Windows the supervisor answers `input_state` with:

- the console input mode from `GetConsoleMode(CONIN$)`: `ENABLE_LINE_INPUT` and
  `ENABLE_ECHO_INPUT` must be off (the counterparts of `ICANON` and `ECHO`);
- `GetConsoleProcessList`: no attached process may be a known interactive program
  other than the agent (editors such as `vim`, `nvim`, `nano`, `emacs`; pagers;
  interactive shells `cmd`, `powershell`, `pwsh`, `bash` started without a command),
  which replaces the foreground-group check that rejects an editor the agent opened;
- the tracked agent process alive and inside the session job.

The host adds bracketed paste from its screen model, and the existing agent-specific
composer checks run unchanged.

Spike S2 probes support this evidence on both backends:
- At an idle composer, line, echo and processed input are off and only the agent is
  attached (Claude Code and Kimi set `ENABLE_VIRTUAL_TERMINAL_INPUT`, Codex reads
  console input records without it).
- An editor that Claude Code opened adds `cmd.exe` and the editor to the process
  list while the input mode can stay raw.
- After an agent crash the raw mode lingers and only the supervisor stays
  attached, so the alive-and-attached check is required.
- Bracketed paste with LF or CR arrives as one prompt in all three agents, Enter
  180 ms after a paste submits, and a lone ESC and Ctrl+S are harmless. Ctrl+C
  cleared the composer in Claude Code and Kimi but not in Codex 0.162: the probe
  PC's Russian layout turned the plain `\x03` into Ctrl+`с` for Codex, which
  the host now avoids for key sends by writing Ctrl+letter as win32-input-mode
  key records to a program that reads key records (D6).

The exact rule for other attached processes (agent-launched MCP servers) is settled
with the host prototype after M3.

Implemented in M4.2 (`src/host/supervisor.rs`, judged by `src/host/channel.rs`):
every `--run` supervisor the host started (untracked ones included) registers its
channel before it starts its program and keeps it; a supervisor that cannot
register still runs its program, and the host then refuses the session's input
checks and pauses. The host waits for a session it is still creating when the
registration arrives first, and a request waits for a channel whose registration
answer is still going out. The evidence reports `ENABLE_PROCESSED_INPUT` as well
(not judged) and `ENABLE_VIRTUAL_TERMINAL_INPUT` (for Ctrl+letter key sends,
D6), names attached processes from a Toolhelp snapshot (a process that
exited meanwhile is left out), and reports the agent the supervisor started last
(the one `platform::forward_shutdown` received), whether it runs and whether its
ID is in the supervisor's own job (the session job). Editors and pagers are `vi`,
`vim`, `nvim`, `nano`, `emacs`, `notepad`, `less`, `more` and the others in
`channel::EDITORS`; agent-launched helpers that are no shell or editor pass.

### D11. Stopping, pausing and terminating

- **Process groups.** The host starts the supervisor with `CREATE_NEW_PROCESS_GROUP`.
  The supervisor calls `SetConsoleCtrlHandler(NULL, FALSE)` so its children process
  Ctrl+C normally, installs a handler that ignores Ctrl+C and Ctrl+Break for itself,
  and starts the agent in its own group, as on Unix, where they share the pane's
  process group.
- **Pause** (Unix: `SIGTERM` to the exact agent PID, never a forced kill): the host
  asks the supervisor over its channel; the supervisor raises `CTRL_BREAK_EVENT` for
  its group (it is already on the console, so no helper process is needed). The
  event reaches every process of the group, including tool and MCP processes; those
  end with the agent on Unix as well. If spike S5 shows an agent ignores Ctrl+Break,
  pause sends that agent's own exit command instead. Pause never force-kills.
- **Terminate** (Unix: `kill-session`, then a wait of up to 10 s,
  `src/state/terminate.rs:66-75`): the host first runs the pause path and waits up to
  10 s for the supervisor to write its completion record; then it closes the
  pseudoconsole (`CTRL_CLOSE_EVENT` with its 5 s system limit), then terminates the
  session job. The supervisor's close handler blocks for at most about 4 s until its
  completion record is written.
- Spike S5 probes: `CTRL_BREAK_EVENT` to the supervisor's group ends Claude Code
  (exit 0), Codex and Kimi (`0xC000013A`) within 60 ms while the supervisor
  survives; `ClosePseudoConsole` ends them with `0xC000013A` (Claude Code 129, Kimi 1
  without a supervisor); resume after either keeps the conversation for all three.
- Implemented in M4.2: `hgs pause` asks the backend (`request_exit`, the tmux
  backend's exact-PID SIGTERM), whose host form is the `signal` `pause` request
  for the pane; the supervisor records the request as SIGTERM, then raises
  `CTRL_BREAK_EVENT` for its own group (`GenerateConsoleCtrlEvent` with its PID,
  which reaches no process when it leads no group; its own handler runs on a
  console thread that may come after the agent has ended), and also re-raises a
  stop that arrived before its agent started. A
  pause whose supervisor ends with its agent before its answer arrives counts as
  done. A terminate's 10 s run from its request, so a slow pause answer is part
  of it; `hgs terminate` waits 20 s on Windows for the agent to exit (10 s on
  Unix).

### D12. Hooks on Windows

Hook bytes, once written, are kept (`src/state/hooks.rs:152-162,211-221`), so the
first Windows form had to be final. Spike S6 settled it (results below), and
tracked Claude Code, Codex and Kimi sessions run on Windows as on Unix;
`HGS_TRACKING=0` runs them untracked. The form:

- the command names the stable CLI (D8) by its absolute path with forward slashes in
  double quotes, followed by `__state hook`, which Git Bash, `cmd.exe` and a direct
  `CreateProcess` all parse the same way;
- Claude Code uses its exec form (command plus `args`, no shell) when the installed
  version supports it, else that shell form under Git Bash or PowerShell;
- Codex uses its Windows command override when present;
- the Kimi TOML block is found with CRLF-tolerant markers, so a CRLF `config.toml`
  never gets a second block;
- never `cmd.exe /c` with a prompt, never `powershell -WindowStyle Hidden
  -EncodedCommand` (Kaspersky blocks it at process creation, Orca issue 16003).

Spikes never write hooks into real agent configuration: every spike uses private
`CLAUDE_CONFIG_DIR`, `CODEX_HOME` and `KIMI_CODE_HOME`. Spike S6 fixes the exact form
per agent version.

Implemented in M4.3 (`src/state/hooks.rs`) and enabled once S6 passed. What each
agent runs, read on 2026-10-14 from Claude Code 2.1.296's settings schema and hook runner
(strings of the win32 build), Codex 0.162.0's source (`hooks/src/engine`,
`config/src/hook_config.rs`) and Kimi Code 2.1.1's bundled source
(`externalHooks/internal/runHook.ts`):

- **Claude Code** documents the exec form: "When present, `command` is resolved as
  an executable and spawned directly with these arguments — no shell." Without
  `args` it runs the command with Git Bash, and without Git Bash with PowerShell
  (`-Command`), which reads `"C:/…/hgs.exe" __state hook` as a string followed by
  unexpected tokens: the shell form cannot run there. hgs reads `claude --version`
  once, when it first writes hooks into a configuration: from 2.1.296 (the oldest
  version reviewed with the exec form) it writes
  `{"type":"command","command":"C:/…/hgs.exe","args":["__state","hook"],"timeout":5}`;
  an older version gets the shell form when Git Bash is found as Claude Code finds
  it (`CLAUDE_CODE_GIT_BASH_PATH`, Git for Windows in Program Files, the `git` on
  PATH) and is refused otherwise. A version that cannot be read is refused, as a
  form chosen without it would be kept.
- **Codex** prefers `commandWindows` on Windows and runs a command hook with the
  session's shell (`build_hooks_config` in `core/src/session/mod.rs`, from 0.140 to
  0.162), which `default_user_shell` makes PowerShell there: `pwsh`, else Windows
  PowerShell, as `-NoProfile -Command COMMAND`. Only a session without one ready
  local environment, which hgs never starts, or a computer without PowerShell uses
  `cmd.exe /C "COMMAND"`.
  PowerShell reads the shell form's quoted program as a string, runs it only with
  its call operator, and turns a native exit code other than 0 or 1 into 1. hgs
  writes the shell form as `command` and
  `& "C:/…/hgs.exe" __state hook; exit $LASTEXITCODE` as `commandWindows`; the
  command never reads `$input`, so PowerShell leaves the payload on standard input
  to hgs (PowerShell 7's console host reads it only for a command that uses
  `$input`). A stable CLI path with `“`, `”` or `„`, which PowerShell also reads as
  double quotes, is refused.
- **Kimi Code** runs `[[hooks]] command` with Node.js `spawn(command, {shell: true})`,
  that is `%COMSPEC% /d /s /c "COMMAND"`; it gets the shell form. On Windows the
  block's marker lines may end with CR LF; Unix keeps its LF-only search.
- The shell form refuses a stable CLI path that has `%`, `!`, `$`, `` ` `` or `"`
  (read differently by `cmd.exe` or bash inside double quotes) or that is not an
  absolute drive or UNC path; spaces, Cyrillic, `&`, `^`, `(`, `)`, `;` and `'` are
  fine. Existing hgs entries are repeated exactly for new events, exec form
  included, and Unix keeps its forms and recognition byte for byte.

**S6 results (2026-10-14).** Real Claude Code 2.1.296, Codex 0.162.0 and Kimi Code
2.1.1 ran through the host with private homes on Windows 11 Pro 10.0.26200 (bundled
ConPTY 1.25.260930003, Windows PowerShell 5.1.26100, Kaspersky real-time protection
on). All
eight scenarios passed; the real homes were unchanged and no process was left.

- Claude Code's exec form delivers SessionStart, UserPromptSubmit, PreToolUse,
  PostToolUse, Stop and SessionEnd: UserPromptSubmit about 0.1 s and Stop 0.22-0.37 s
  after the prompt. Pause takes 0.1 s, SessionStart confirms a resume 0.9 s later,
  and terminate (0.1 s) archives the session. The shell form runs under Git Bash;
  with `"shell": "powershell"` it never runs.
- Codex runs `commandWindows` with Windows PowerShell 5.1 and prefers it to a Unix
  `command`; the shell form as the only `command` never runs. UserPromptSubmit
  arrives about 1-2 s and Stop 2-3 s after the prompt (5.6 s for a turn with a
  tool); a resumed conversation is confirmed only by Codex's next message.
- Kimi runs the shell form through `cmd /d /s /c`. Pause takes 4.9 s and
  SessionStart confirms a resume after 5.6 s; a CRLF `config.toml` with a complete
  hgs block gets no second block.
- A prompt with Cyrillic and Latin diacritics reaches hgs intact through each
  runner (no shell, PowerShell, `cmd.exe`).
- The whole hook path (a Stop replayed through `hgs __state hook`) takes a median of
  37-38 ms, p95 at most 41 ms; a bare `hgs.exe` start takes 25 ms, both under
  Kaspersky's real-time protection (S7).

### D13. Attach

- `hgs.exe a <session>` connects to the host and sets its console modes: input clears
  `ENABLE_PROCESSED_INPUT` (so Ctrl+C arrives as `\x03`), `ENABLE_LINE_INPUT` and
  `ENABLE_ECHO_INPUT` and sets `ENABLE_VIRTUAL_TERMINAL_INPUT`; output sets
  `ENABLE_VIRTUAL_TERMINAL_PROCESSING` and `DISABLE_NEWLINE_AUTO_RETURN`. It writes
  with `WriteConsoleW`, buffering UTF-8 sequences split across reads, instead of
  changing the code page (`src/platform/windows.rs:58-62`), reads size changes as
  `WINDOW_BUFFER_SIZE_EVENT`, and restores every mode on detach and on host death.
- It receives a repaint and then relays bytes both ways. `Ctrl+B d` and
  `Ctrl+B Ctrl+D` detach, and `Ctrl+B Ctrl+B` sends a literal Ctrl+B (tmux's
  `send-prefix`; Claude Code binds Ctrl+B).
- Multiple clients may attach. The size follows the most recent client, as with
  `window-size latest`; with no client the last size stays.
- `--existing --run-id` never creates or restores a session and refuses a changed
  run, as today.
- Host death during an attach restores the terminal modes and prints the message the
  tmux path prints when the server dies.
- The GUI's embedded terminal keeps running `hgs.exe a NAME --existing --run-id`
  inside its own pseudoconsole. This doubles the ConPTY layer but needs no GUI
  protocol change; the host-as-sole-responder rule (D6) keeps queries answered once.
- `clients` in `ls --json` lists opaque client IDs on Windows (no tty paths).

Implemented in M5 (`src/host/attach.rs`, `state::backend::host`):

- The client writes the repaint and the relayed output as they come; it draws no
  screen of its own, so there is no alternate screen around the session and the
  client console keeps its scrollback. A host notice has no status line to show on
  and is ignored.
- Prefix keys: `Ctrl+B d` and `Ctrl+B Ctrl+D` detach, `Ctrl+B Ctrl+B` sends one
  Ctrl+B, and any other key after Ctrl+B passes through with the Ctrl+B, unchanged
  (tmux would run or swallow a binding; the client has no others, and dropping keys
  would lose input). Keys inside a bracketed paste are text, as in tmux; ESC and
  Ctrl+B in one read are Meta+Ctrl+B.
- Standard input and output must both be a console that takes the attach modes;
  pipes, files and a legacy console are refused before anything is created,
  resumed or attached (`TerminalBackend::attachable`), as tmux refuses a client
  without a terminal. The input mode also sets `ENABLE_WINDOW_INPUT`; the window
  size is also polled, because a classic console window sends no event when only
  its height changes. Ctrl+C and Ctrl+Break events are ignored while attached.
- At the end the client resets the modes a session may leave on its terminal (as
  tmux does on detach), leaves the alternate screen only when the session had
  entered it, moves below the last row and prints tmux's `[detached (from session
  NAME)]` or `[exited]` (exit 0). A lost connection exits 1; when no host answers
  any more the client prints `hgs: the session host died; terminal restored, sessions
  on this box are gone`, the tmux backend's message naming the session host.
- An attached launch creates the session at the console's size, then attaches by
  session ID; a session that ended before the attach counts as exited. With
  `--run-id` the host checks the run marker again as it attaches.

### D14. Workers

The host supervises the recovery worker (`hgs.exe recovery worker`, from the stable
CLI) with a delay that doubles from 1 s to 5 min, because recovery belongs to the
session lifecycle and Windows has no user service manager. On Windows,
`recovery::ensure_worker` (`src/state/recovery.rs:1080-1106`) asks the host instead of
detaching a process inside `hgs.exe`'s job, and does nothing when no host runs. The
swarm worker stays tray-supervised as in Phase 1.

Implemented in M4.2: the request is protocol 1's `recovery_worker` (capability
`recovery-worker`); the host starts supervising the staged stable CLI's worker on
the first one and stops it when it exits. A host that does not know the request
(an older one) is treated like no host; the next request reaches its successor.
The M3 variable `HGS_HOST_RECOVERY_WORKER` is gone.

### D15. Paths and files

- `platform::canonical` returns canonical paths without the `\\?\` prefix; every
  stored or compared path uses it (38 `canonicalize` sites in `src/state`, including
  `cwd_canonical` at `src/state/workspace.rs:185`).
- Comparisons of local paths on Windows are case-insensitive; session names derive
  from the canonical (on-disk) casing of the folder, in the CLI: the canonical name
  is used only when it equals the logical one ignoring case, so a junction keeps its
  own name.
- `hgs.exe` gets an application manifest (UTF-8 code page, `longPathAware`), as
  `hgs-tray.exe` already has; today only the GUI has one (`build.rs:1-11`).
- `.gitattributes` keeps `tests/fixtures/*.txt` and `src/state/data/*.txt`
  byte-exact (`-text`), so CRLF checkouts never change the fixtures that
  `include_str!` embeds.
- Git runs with Windows forms of its non-interactive settings (an askpass that fails,
  `NUL` instead of `/dev/null`) and an `ssh` that never receives synchronous pipes
  (`src/state/worktrees.rs:67-72`).
- **Local Windows folders stay local.** The GUI pushes the whole organization to the
  swarm (`tray/src/SessionsWindow.cpp:1827`), `flatten` publishes every folder
  (`src/swarm/model.rs:53-69`), the catalog accepts any path that starts with `/`
  (`src/swarm/catalog.rs:78`, so `//server/share` too), and `organizationReady`
  replaces the local organization (`SessionsWindow.cpp:1195-1199`). Local Windows
  folders stay in the GUI's organization model but never reach the swarm:
  `SwarmController::sharedOrganization` strips the drive and UNC folders of the local
  machine (an empty `machine_id` or the snapshot's `node_id`; every one before the
  first snapshot) before sending, and `presentation` merges those back, dropping the
  ones whose project was deleted. `flatten` skips the drive and UNC folder paths of
  the local machine (by alias or ID), so a GUI of any version that sends them never
  breaks an apply; other invalid paths, a peer's drive path among them, still fail.
  A peer's `//server/share` is a POSIX path, which a Windows GUI can store
  (`QDir::cleanPath` keeps the leading `//` there): other computers share, rename and
  remove it like any folder. The rules do not know a machine's platform, so the POSIX
  computer it belongs to still treats it as its own Windows folder: its GUI and CLI
  keep it out of requests, and when another computer removes it, that GUI keeps it
  until it is removed there too. A test checks that a Unix peer's sync never drops
  local Windows folders. A shared catalog kind for Windows paths is Phase 3.

### D16. GUI

Apart from the status text, these rules change only a Windows GUI and its local
machine: Linux and macOS GUIs, and every peer, behave as today.

- Machines page: "Enable local sessions" for this computer when it runs
  Windows, is a control console and its CLI reports `local_host`, running
  `hgs.exe session-host install`; the page shows the host's state, PID, ConPTY,
  integrity, Windows session and error. M6 has no disable action
  (`hgs session-host uninstall` remains). Launches on this computer warn when that
  host runs in another Windows session (D5). M6 labelled the button and its
  confirmation a preview; the owner removed that label on 2026-10-14 (Milestones).
- Once enabled, the GUI runs the stable CLI from its next start and refreshes it when
  its bundled build is newer (D2).
- Path model per machine (`tray/src/MachinePlatform.h`): the local machine is Windows
  when `metrics.platform` says so, and has the GUI's build platform before its box
  reports one; peers are POSIX. Windows folders are stored in Qt form (`C:/work`,
  `//server/share/dir`), compared with either separator and ignoring case, and shown
  with native separators. Drive and UNC roots are accepted for the local Windows
  machine wherever `startsWith('/')` or `QDir::isAbsolutePath` gates folders today
  (`SessionOrganization.cpp:24,176,187`, `ProjectsDialog.cpp:351,441`,
  `NewSessionDialog.cpp:77,183`, `SessionsWindow.cpp:2239,2964,4155,4208,4298`,
  `SessionCommands.h:109`), with the local-only folders of D15.
- Terminal file drops into a local Windows session use native separators and double
  quotes around a path with whitespace or shell-special characters (Windows names
  cannot contain `"`), or single quotes with `'` doubled around a path with `$` or a
  backtick, which PowerShell expands inside double quotes; remote drops keep POSIX
  quoting. The local folder shell opens
  PowerShell in that folder through Windows Terminal, or a console started there when
  Windows Terminal is missing or cannot pass the path unchanged. On a Windows GUI,
  file references accept `C:\…`, `C:/…` (with `:line[:col]`) and `file:///C:/…`,
  never UNC; elsewhere `C:\x` stays invalid.
- New session and Accounts hide `dsh` and `opencode` on the local Windows machine
  (OpenCode's launch path does not resolve npm `.cmd` shims); the shell entry stays,
  labelled PowerShell. Voice sends to local Windows sessions are disabled, with the
  reason as the button's tooltip.
- Mobile connection settings stay hidden on Windows independently of
  `local_sessions` (today both follow `SettingsPage::setLocalSessions`); Recovery
  keeps following `local_sessions`.
- Status text no longer names tmux ("the session continues on <machine>", with the
  local machine's display name), in the terminal view and the update tooltip
  (`TerminalView.cpp:63,141`, `SessionsWindow.cpp:1299`). The Machines page keeps its
  tmux wording for remote Unix machines.
- An enabled Windows computer waits at least 5 s between polls of itself until the host
  can push changes (a control console keeps its 15 s); Unix keeps the configured interval.

## Unix compatibility rules for the refactor

- The tmux backend builds every answer, including the batched `list` and
  `input_state`, from today's exact calls in today's order: `ls --json` keeps
  `list-clients`, `list-sessions` and both `list-panes` runs; the input check keeps
  `display-message`, then `tcgetattr`, then `ps`.
- It spawns plain `tmux` by name on every call, inheriting `TMUX` and
  `TMUX_TMPDIR`, so the PATH shim in `tests/test_input.py:73` and the `TMUX_TMPDIR`
  guard in `tests/test_hgs.sh:6-11` keep working.
- Dry-run rendering becomes a backend method; the Unix strings stay exactly as
  `tests/test_hgs.sh:84-186` pins them, and the Windows form is defined separately.
- The golden census (M1.1) records, for the whole Python and shell suites, every tmux
  invocation's argv shape, its order within each operation, its stdin bytes, the
  inherited `TMUX`/`TMUX_TMPDIR`, the `ps` argv and the error strings that reach JSON
  (`src/state/storage.rs:206-219`), before and after the refactor.

## Spikes

Run on the Windows development PC by the orchestrating session only (never by
subagents), in a dedicated folder, with disposable state, isolated endpoints and
private agent homes, never touching live sessions or real agent configuration. Each
spike records its results in Beads and adjusts this document.

| Spike | When | Question | Gates |
|---|---|---|---|
| S1 | M0 | With inbox and bundled ConPTY, real Claude and Codex (Kimi if available): are `?2004h`, `?1049h`, cursor keys and mouse modes visible to the host? Which queries reach the host and are they answered by ConPTY itself? Which colours and glyphs do the agents use with `TERM=xterm-256color`, with and without `FORCE_COLOR`? Record raw byte streams for the M2 corpus. | D6, D7, D8 |
| S3 | M0, then M4 | Record raw streams and rendered screens of every dialog the parsers read (trust, permission, approval, questions, effort and model pickers, update, resume, clear). | M4 parsers |
| S2 | probe now, confirmed after M3 | Bracketed paste, a lone ESC followed by text, Ctrl+C, Ctrl+S and Enter 180 ms after a paste reach each agent intact. Input evidence (D10) rejects an editor the agent opened and a crashed agent. | D10 |
| S4 | probe now, confirmed after M3 | Breakaway works from `hgs.exe` started by the GUI, a console and sshd, and inside the tray's attach and swarm-worker jobs; host and agents survive GUI exit, GUI update, SSH logout and a desktop logoff while an SSH-started host runs; pipe DACL, integrity label and server/client identity checks; rename-aside of a running image; agent credentials (DPAPI, Credential Manager) under an SSH-started host. | D3, D4, D5 |
| S5 | probe now, confirmed after M3 | Each agent's reaction to `CTRL_BREAK_EVENT` and `CTRL_CLOSE_EVENT`; resume after both. | D11 |
| S6 | after M3 | Hook forms per agent version: Claude exec form and Git Bash form, Codex Windows command, Kimi command shell; hook latency with real-time scanning. | D12 |
| S7 | after M3 | Start cost of `hgs.exe` and hooks under real-time scanning; SQLite journal writes within the 5 s hook timeout. | D12, M4 |

Status on 2026-10-14: S1 is done (results in D6, D7, D8). S3 is done (2026-10-13;
its report is kept outside the repository, and the M4 parser changes it called for
landed in PR #33). S6 and S7 are done (2026-10-14; results in D12 and below). S2
and S5 probes are
done (results in D10, D11) and are confirmed through the host after M3. S4 is folded
into the M3/M7 host survival tests. S7 on the development PC, whose real-time
antivirus is Kaspersky (Security Center reports it enabled and up to date; Defender
does not run there, and no exclusion for the test folders is known): `hgs.exe`
starts in about 11 ms warm and 132 ms for a freshly built image, a Git Bash hook
takes about 55 ms, and S6 measured a 25 ms start and a 37-38 ms whole hook path
(p95 at most 41 ms), all far inside the 5 s hook timeout. Defender's real-time
scanning itself remains unmeasured.

D10 and D11 are provisional until their spikes are confirmed through the host.

## Milestones

Each milestone lands with its tests; Unix stays byte-for-byte compatible after every
one.

- **M0 Spikes** S1 and S3 (raw streams and dialogs), plus early probes for S2, S4
  and S5.
- **M1 Unix-safe groundwork.**
  1. Guardrails: `scripts/ci/check-windows-target.sh` (a Linux type-check and clippy
     of the Windows MSVC target with a stand-in C compiler for build scripts) and the
     golden census described above.
  2. Shared-helper runtime traps fixed with no Unix change: directory sync, rename
     retry, lock contention, `home()` (including direct `HOME` reads in accounts),
     `canonical`, executable check, write-mode lock files.
  3. Platform abstractions: process (presence, start time, table, describe, current
     directories that fail closed when unreadable, open-file holders, pinned handles),
     file identity and private files, control (exit info, shutdown guard, detached
     spawn, bounded runner with per-call jobs), directories, Git settings. The
     fail-open `worktree_cleanup` fallback becomes fail-closed.
  4. `TerminalBackend` trait with the tmux implementation under the rules above;
     every call site moves behind it; tty and `ps` checks become `input_state`.
  5. `state` compiles on Windows with an unavailable backend; `state_windows.rs` is
     removed; the `state` unit tests that do not need Unix symlinks, `/bin/sh` or
     Phase 2 non-goals (`dsh`, `voice`) run in Windows CI, and the rest stay
     `cfg(unix)` by name.
- **M2 Screen model** with the differential tests (D7).
- **M3 Session host**: endpoint, protocol 1, ConPTY sessions, jobs, staging and the
  marker, breakaway start, the supervisor channel, `session-host` (D1-D6, D9).
- **M4 Windows lifecycle** (M4.1, the host backend and the launch path, implemented
  2026-10-14: untracked sessions, PowerShell, listings, rename, kill and the
  backend's input and paste paths; M4.2, the supervisor channel, input evidence,
  pause and terminate, exit evidence and the recovery worker (D9-D11, D14),
  implemented 2026-10-14 and run with untracked sessions; M4.3, the hook forms
  (D12), implemented 2026-10-14 and enabled for tracked agents once S6 passed the
  same day): host backend
  client, launch and accounts (D8), supervisor
  and signals (D11), identity, input evidence (D10), hooks (D12), recovery worker
  (D14), Windows dialog fixtures and path-aware screen parsers
  (`src/state/codex_trust.rs:33,49`, `claude_permission.rs:94`,
  `clear_context.rs:90-93`), open-file holders for Codex resume input.
- **M5 Attach client and listings** (D13, `local_host`; implemented 2026-10-14).
- **M6 GUI** (D15 local folders and on-disk session-name casing, D16; implemented
  2026-10-14).
- **M7 Tests, CI, packaging and docs**: a native `examples/fake_agent.rs` (raw input
  recorder, fixture renderer, mode toggles, hook emitter); one backend contract suite
  run against tmux on Unix and against the host on Windows; host survival tests
  (start inside a kill-on-close job emulating sshd's); real-agent Windows suites
  (keyless Claude against the local fake API, pinned win32 Codex) in a separate gated
  `windows-host` CI job, so failures point at the host and the `windows-desktop` job
  keeps its margin; the ConPTY pair in the zip and its checker; installation,
  reference, CI and CONTRIBUTING updates.

Local sessions on Windows were labelled a preview in the GUI and documentation
until the real-agent Windows suites passed in CI. A skipped native suite is not
compatibility evidence. Those suites run in the `windows-host` job that the CI gate
requires, and the owner removed the preview label on 2026-10-14: the Machines page
offers "Enable local sessions", and the documentation no longer calls local sessions
on Windows a preview.

## Risks

- **Screen fidelity.** Parsers fail closed if serialization, glyphs or colours
  differ. The differential test, the S1 streams and the S3 fixtures come before
  parsers are trusted on Windows.
- **Input through ConPTY.** Bracketed paste, ESC timing and control keys are
  unverified; S2 gates sending on Windows.
- **Antivirus and enterprise policy.** Unsigned executables staged in the user
  profile can be blocked by AppLocker, WDAC or Defender ASR rules; every hook starts
  `hgs.exe`. The host reports blocked starts clearly; signing is a later decision.
- **Logoff.** Windows ends a user's processes at logoff, so sessions end there, as
  user tmux servers do without lingering.
- **Elevation and Windows sessions.** An administrator's SSH session is elevated and
  runs in another Windows session. One host per state directory, integrity checks and
  GUI warnings keep the two from mixing silently.
- **Version skew.** The GUI's bundled CLI, the stable CLI and a running host can
  differ by one version; protocol and state formats stay compatible in both
  directions across that skew.
