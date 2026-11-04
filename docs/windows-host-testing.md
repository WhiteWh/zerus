# Testing the Windows session host

The Windows session host (`hgs.exe __host serve`, spec
[Windows Phase 2](superpowers/specs/2026-10-13-windows-native-host-design.md))
owns the ConPTY sessions of one user and state directory. This page shows how to
run a host that touches nothing of your own, how to run its tests, and the rules
new survival and contract tests follow. Commands are for Git Bash in the checkout,
as in [CONTRIBUTING](../CONTRIBUTING.md#windows).

## Who may run a host

A host serves only when local sessions are enabled (the `local-sessions` marker
that `hgs session-host install` writes) and only from the staged host image
(`%USERPROFILE%\.local\share\hgs\host\<hash>\hgs.exe`, named by the pointer
`current`). `hgs __host call --start` starts that image. Tests run the build under
test instead, with two variables that count only for an *isolated* state
directory, one that `HGS_STATE_DIR` names and that is not this user's default
(`XDG_STATE_HOME\hgs\sessions` or `~\.local\state\hgs\sessions`):

- `HGS_HOST_ALLOW_UNSTAGED=1`: the host serves without the marker and from any
  executable, and creates sessions without the marker. The processes of those
  sessions inherit it, so their supervisors (`hgs --run`) and tracking hooks
  (`hgs __state hook`) run where local sessions are off; every other command
  stays a control console's there.
- `HGS_HOST_IMAGE=<absolute path of an hgs.exe>`: `__host call --start` starts that
  executable. Any build works, so a test can pair one CLI with another host.

For any other state directory the first is ignored and the second is an error.
`HGS_STATE_DIR` alone proves nothing, as every hgs process exports it to its
children. The rules live in `src/host/gate.rs`.

`__host serve` exits with 3 when another host holds the state directory, 4 when
local sessions are not enabled, 5 when it does not run from a staged image and 1
on other failures; `__host call --start` reports these as errors.

## An isolated host by hand

Run this in a new Git Bash window and close it afterwards: it points `HOME` and
`USERPROFILE` at a temporary folder.

```sh
cargo build --locked
export MSYS_NO_PATHCONV=1
unset $(compgen -e | grep -E '^(HGS_|XDG_|TMUX)')
root="$(mktemp -d)"
mkdir "$root/state" "$root/home" "$root/work"
export HGS_STATE_DIR="$(cygpath -w "$root/state")"
export USERPROFILE="$(cygpath -w "$root/home")" HOME="$(cygpath -w "$root/home")"
export HGS_HOST_IMAGE="$(cygpath -w "$PWD/target/debug/hgs.exe")" HGS_HOST_ALLOW_UNSTAGED=1
hgs=target/debug/hgs.exe
create() {  # create <launch id> <session name>: a cmd.exe session in $root/work
    python -c 'import json, os, sys; print(json.dumps({"op": "create_session",
        "launch_id": sys.argv[1], "name": sys.argv[2], "cwd": sys.argv[3],
        "env": [f"{k}={v}" for k, v in os.environ.items()],
        "argv": [os.path.join(os.environ["SystemRoot"], "System32", "cmd.exe"), "/d", "/q", "/k"],
        "size": {"cols": 100, "rows": 30}}))' "$1" "$2" "$(cygpath -w "$root/work")"
}

"$hgs" __host call --start "$(create demo-1 cmd/demo)"  # starts the host, then the session
"$hgs" __host status                                   # hello (pid, image, in_job) and counts
"$hgs" __host call '{"op":"list"}'
"$hgs" __host call '{"op":"send_keys","target":{"pane":"%0"},"keys":[{"literal":"echo hello"},{"named":"Enter"}]}'
"$hgs" __host call '{"op":"capture","target":{"pane":"%0"},"styled":false}'
"$hgs" __host attach '{"target":{"pane":"%0"},"size":{"cols":100,"rows":30},"run_id":null}' \
    '[{"wait_ms":500},{"send":"echo attached\r"},{"wait_ms":500},{"detach":true}]'
"$hgs" __host call '{"op":"kill","target":{"pane":"%0"}}'
"$hgs" __host status        # {"running": false} about 5 s after the last session ended
cat "$root/state/host.log"  # starts, refusals, session ends and exits
```

`__host call` prints the response and exits with 0 (`ok`), 1 (`error`), 3 (no host
runs) or 4 (the host cannot be reached or started); without `--start` it never
starts a host. `{"op":"stop_host"}` stops an idle host at once.

Once that host has exited, the same window can enable local sessions as a user
would, in its temporary profile, and start the staged image instead:

```sh
unset HGS_HOST_IMAGE HGS_HOST_ALLOW_UNSTAGED
"$hgs" session-host install                            # stable CLI, host image, marker
"$hgs" __host call --start "$(create demo-2 cmd/staged)"
"$hgs" session-host status                             # running, with its PID and image
"$hgs" session-host stop                               # refused while a session runs
"$hgs" __host call '{"op":"kill","target":{"name":"cmd/staged"}}'
"$hgs" session-host uninstall                          # once the session has ended
"$hgs" session-host status                             # not-installed once the host exited
rm -rf "$root"
```

Remove the folder only after `__host status` reports no host: a running host keeps
files open in its state directory and image.

## Local sessions through the CLI

With local sessions enabled, `hgs` runs this computer's sessions in the host
(spec D3, D8; `src/state/backend/host.rs`). A launch starts the host from the
staged image when none runs; a listing never does, and only "no host" lists no
sessions. Each session's root process is the stable CLI's `hgs.exe --run`, which
runs the program as its child (PowerShell for `hgs sh`), with the launching
process's environment minus the variables of a session it runs in, plus the
launch's own; the launching side sets `HGS_EXECUTABLE` to the stable CLI and the
host sets `HGS_HOST_SESSION`, `HGS_HOST_PANE` and `TERM`. Programs are found with
`PATHEXT`; an npm command shim runs Node.js on its script, and any other batch
file is refused. Claude Code, Codex and Kimi run tracked, with hgs's hooks in
their configuration ([below](#tracked-agents); `HGS_TRACKING=0` runs them
untracked).
`hgs a` and a launch without `-d` attach the console they run in
([below](#attaching-a-terminal)). In a new Git Bash window, as above:

```sh
cargo build --locked && cargo build --locked --example fake_agent
export MSYS_NO_PATHCONV=1
unset $(compgen -e | grep -E '^(HGS_|XDG_|TMUX)')
root="$(mktemp -d)"
mkdir -p "$root/state" "$root/home" "$root/work/project" "$root/bin"
cp target/debug/examples/fake_agent.exe "$root/bin/"
export HGS_STATE_DIR="$(cygpath -w "$root/state")" HGS_TAB=0
export USERPROFILE="$(cygpath -w "$root/home")" HOME="$(cygpath -w "$root/home")"
export PATH="$root/bin:$PATH"
hgs=target/debug/hgs.exe project="$(cygpath -w "$root/work/project")"
"$hgs" session-host install                           # marker, stable CLI, host image
"$hgs" ls --json --local                              # no sessions, no host started
"$hgs" fake_agent "$project" --new -n one -d -- --prompt 'fake> '
"$hgs" ls --json --local                              # fake_agent/project/one
"$hgs" __host backend '{"op":"capture","pane":"%0"}'  # the agent's screen
"$hgs" __host backend '{"op":"send","pane":"%0","keys":[{"literal":"hi"},{"named":"Enter"}]}'
"$hgs" rename fake_agent/project/one fake_agent/project/two
"$hgs" --dry-run sh "$project" -n ps -d              # the create request, not sent
"$hgs" kill fake_agent/project/two
"$hgs" __host status                                  # {"running": false} once idle
rm -rf "$root"
```

A program that cannot start (one missing from `PATH`, an executable this user may
not run, a tracked agent whose hook setup the supervisor refuses) ends its session
at once, before any record exists. A launch therefore watches its new session for
3 s through the host's `launch_status` (how the session of a launch ID runs or
ended: its exit code and the last lines of its pane, wrapped rows joined): one
that ends in that time with a failure fails the launch with exit 1, and the GUI
shows the reason instead of waiting for the session. A detached launch that
starts returns after those 3 s; `hgs resume`, restores and forks do not wait. A
session that ends later without a record, or at once with exit 0, is reported by
the next listing in `ended_launches` instead ([JSON and external
clients](reference.md#json-and-external-clients)). Its supervisor reads its pane
before the pseudoconsole has delivered everything, so the host replaces those
lines in the supervisor's report with its pane's last lines once the output has
drained (followed by the supervisor's error if the pane does not show it),
before the session leaves the listing. In the window above:

```sh
"$hgs" no_such_agent "$project" --new -n missing -d  # exit 1
# hgs: session no_such_agent/project/missing ended immediately (exit 127):
# hgs: no_such_agent: not found on PATH
```

`hgs __host backend '<operation>'` runs one operation of the terminal backend
(`listing`, `panes`, `pane_info`, `capture`, `send`, `paste`, `set_run_marker`,
`rename`, `kill_session`, `kill_pane`, `session_exists`, `session_name`,
`input_state`, `request_exit`, `on_terminal`; see `src/host/command.rs`) and prints
`{"ok": ...}` or `{"error": ...}`. It reaches only a running host. A `send` of a
named Ctrl+letter key (`C-c`, `C-u`, `C-s`) goes as win32-input-mode key records
with the Latin letter and Left Ctrl when the session's ConPTY has asked for that
mode and its supervisor reports a console without VT and line input (Codex), so
Codex sees Ctrl+C under any keyboard layout; otherwise, and for every program
that reads VT input (Claude Code, Kimi, the fake agent), it goes as the C0 byte
(spec D6).

Text that names tmux on Unix says the same without it here (`Wording` in
`src/state/backend/mod.rs`): `hgs a --existing` and `rename` refuse a session ID
other than `$N` as `invalid session identity`, and the Terminal reasons of a
session that is not a live tracked pane, OpenCode and DeepSeek name no `tmux
pane` or `tmux Terminal`. `hgs --help` leaves out the Unix-only `hgs paste`,
`hgs clip` and `hgs notify` and the line on Ctrl+V, the tmux binding that runs `hgs
paste`, and gives `--dry-run` as printing the `session-host/ssh` command. Those
three commands exit with code 2 (`hgs: clip is not supported on Windows`)
instead of starting a program of that name.

## The supervisor channel

Every session's `hgs.exe --run` registers a channel with its host before it starts
its program (spec D10, D11; `src/host/supervisor.rs`). Through it the host reads
the console evidence that input checks judge (the input mode, the attached
processes and whether the supervisor's agent runs inside the session job) and asks
for a pause: Ctrl+Break for the supervisor's process group, which its agent
shares. Terminate pauses first and closes the console after 10 s. `host.log` notes
each session's end with its supervisor's exit code (130 after a pause that ended
the agent with `STATUS_CONTROL_C_EXIT`). In a window prepared as above, before its
`rm -rf`:

```sh
ready="$(cygpath -w "$root/ready.json")"
"$hgs" fake_agent "$project" --new -n three -d -- --ready "$ready" --prompt 'fake> '
until [ -s "$root/ready.json" ]; do sleep 0.2; done
pid=$(python -c 'import json, sys; print(json.load(open(sys.argv[1]))["pid"])' "$ready")
pane=$("$hgs" __host call '{"op":"resolve_target","target":{"name":"fake_agent/project/three"}}' |
    python -c 'import json, sys; print(json.load(sys.stdin)["ok"]["pane"])')
"$hgs" __host backend '{"op":"set_run_marker","name":"fake_agent/project/three","run_id":"run-1"}'
"$hgs" __host backend '{"op":"input_state","pane":"'"$pane"'","name":"fake_agent/project/three","run_id":"run-1","agent":"fake_agent","pid":'"$pid"'}'
ends=$(grep -c ' ended: ' "$root/state/host.log")
"$hgs" __host backend '{"op":"request_exit","pane":"'"$pane"'","pid":'"$pid"'}'  # the pause
until [ "$(grep -c ' ended: ' "$root/state/host.log")" -gt "$ends" ]; do sleep 0.2; done
grep ' ended: ' "$root/state/host.log" | tail -1  # session $N ended: exit 130 (0x00000082)
```

With recovery enabled (`hgs recovery set`), `hgs recovery get` asks a running host
to supervise the stable CLI's `hgs.exe recovery worker` (D14); without a host
nothing starts one.

## Tracked agents

A tracked Claude Code, Codex or Kimi session writes hgs's hooks into the agent's
configuration in the forms of spec D12 (`src/state/hooks.rs`), which spike S6
confirmed with the real agents. Hooks, once written, are kept: try them only with
a private agent home, the home variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
`KIMI_CODE_HOME`) naming a directory of the test, never the agent's own. In a
window prepared as in "Local sessions through the CLI", with a fake agent named
`claude` that answers hgs's version probe:

```sh
cp target/debug/examples/fake_agent.exe "$root/bin/claude.exe"
mkdir "$root/claude-home"
export CLAUDE_CONFIG_DIR="$(cygpath -w "$root/claude-home")"
FAKE_AGENT_PROBE_TEXT='2.1.296 (Claude Code)' FAKE_AGENT_PROMPT='claude> ' \
    "$hgs" claude "$project" --new -n tracked -d
cat "$root/claude-home/settings.json"     # exec form: the stable CLI, args __state hook
"$hgs" inspect claude/project/tracked      # conversation_id from the SessionStart hook
"$hgs" kill claude/project/tracked
```

Hooks name the stable CLI with forward slashes. Claude Code from 2.1.296 gets its
exec form (`command` and `args`, no shell), an older one the shell form
`"C:/…/hgs.exe" __state hook` with Git Bash; Codex gets the shell form as `command`
and `& "C:/…/hgs.exe" __state hook; exit $LASTEXITCODE` as `commandWindows`, which
it runs with PowerShell (`-NoProfile -Command`); Kimi gets the shell form in its
`config.toml` block and runs it with `%COMSPEC% /d /s /c`. PowerShell cannot run the
shell form. Before anything is written, the supervisor refuses a Claude Code whose
version cannot be read or that is older and finds no Git Bash, and a stable CLI
path that a hook command would read differently; `HGS_TRACKING=0` runs such an
agent untracked. The fake agent runs each configuration's hooks the way its agent
does.

## Attaching a terminal

`hgs a SESSION` and a launch without `-d` attach the console they run in to the
session (spec D13; `src/host/attach.rs`), as the GUI's embedded terminal runs
`hgs.exe a NAME --existing --run-id ID` on a pseudoconsole of its own. Standard
input and output must both be a console (Windows Terminal, the console host, a
pseudoconsole) that takes terminal sequences. The mintty window that the Git
Bash shortcut opens gives pipes, which are refused before anything is created,
resumed or attached (`attaching needs a console`, exit 1): use Git Bash inside
Windows Terminal, or `winpty`. The client gets a repaint of the pane, then its
output as it comes, written with `WriteConsoleW`; the console keeps its own
scrollback, as there is no alternate screen around the session. The pane takes
the size of the client that was active last, and an attached launch starts the
session at the console's size. Inside a session (`HGS_HOST_PANE` is set), `hgs a`
and a launch without `-d` are refused with exit 2, as inside tmux, but without
naming tmux (`already inside session NAME; open a new tab` for `hgs a`, `already
inside session NAME; use --new -d or open a new tab` for a launch, or `already
inside a session; ...` when the host cannot name it). `fork` and `resume` without
`-d` are refused the same way (`already inside a session; use fork -d or open a new
tab`). A launch with `-d`, `fork -d` and `resume -d` start their sessions from
there without touching the console. The new session starts from this process's
environment without the caller's session variables (`SESSION_CONTEXT` in
`src/state/backend/host.rs`, which also drops Claude Code's `CLAUDECODE` mark), and
a launch never passes on the caller's account profile.

| Keys | What happens |
|---|---|
| `Ctrl+B d`, `Ctrl+B Ctrl+D` | detach: `[detached (from session NAME)]`, exit 0 |
| `Ctrl+B Ctrl+B` | one Ctrl+B reaches the agent (tmux `send-prefix`) |
| `Ctrl+B`, then any other key | both reach the agent unchanged |

Keys inside a bracketed paste are never bindings, and ESC with Ctrl+B in one read
is Meta+Ctrl+B. Ctrl+C reaches the agent as a key, byte for byte as the
client's terminal sends it; Ctrl+Break is ignored. Under a non-Latin keyboard
layout Codex reads such a Ctrl+C as Ctrl and a letter of that layout and ignores
it, as it does in Windows Terminal itself (a crossterm limitation upstream, spec
D6). At the end the client resets what the session may have left on (mouse, paste, focus and
keypad modes, the alternate screen, a hidden cursor), restores the console modes
it found and moves below the last row. The session's end prints `[exited]` (exit
0). The host's death prints `hgs: the session host died; terminal restored,
sessions on this box are gone` (exit 1), as the tmux backend reports a dead
server. `--existing` never creates or resumes a session, and `--run-id` refuses a
changed run (`session run changed; refresh before reconnecting`, exit 1). While a
client is attached, `hgs ls --json` lists it in `clients` by an opaque ID such as
`c1`; the local box also carries `local_host` (`state`: `not-installed`,
`stopped`, `running` or `unavailable`; `pid`, `protocol`, `conpty`, `integrity`
and `windows_session` of a running host, null otherwise). In a window prepared as
above that is a console, before its `rm -rf`:

```sh
"$hgs" fake_agent "$project" --new -n four -d -- --prompt 'fake> '
"$hgs" a fake_agent/project/four       # type, then Ctrl+B d
"$hgs" ls --json --local               # local_host; clients while attached
"$hgs" fake_agent "$project" --new -n five -- --prompt 'fake> '  # created and attached
```

## Running the tests

```sh
cargo test --locked -- host:: state::backend state::control_console_tests platform::windows::tests </dev/null
cargo build --locked
cargo build --locked --example fake_agent
cargo test --locked --no-run --bin hgs   # names the test binary, target/debug/deps/hgs-<hash>.exe
HGS_TEST_BIN="$(cygpath -w "$PWD/target/debug/hgs.exe")" target/debug/deps/hgs-<hash>.exe host::attach </dev/null
python scripts/fetch-conpty.py --output target/conpty     # the pinned ConPTY pair
export HGS_TEST_CONPTY_DIR="$(cygpath -w "$PWD/target/conpty")"
python -m unittest discover -s tests -p 'test_windows_host*.py' -v </dev/null
python -m unittest discover -s tests -p test_windows_host.py -v -k Staged </dev/null
python -m unittest discover -s tests -p test_windows_lifecycle.py -v </dev/null
python -m unittest discover -s tests -p test_windows_hooks.py -v </dev/null
python -m unittest discover -s tests -p test_backend_contract.py -v </dev/null
```

The Rust filter `host::supervisor` includes
`a_supervisor_reports_its_console_and_pauses_its_group`, which runs the test binary
as a supervisor on a pseudoconsole of an in-process host and its raw-input agent.
`host::attach::console_tests::hgs_attaches_inside_a_pseudoconsole` runs the
`hgs.exe` that `HGS_TEST_BIN` names (it is skipped without it, as a unit test
cannot tell whether a build it finds is current) as `hgs a` on a pseudoconsole of
the test against an isolated host of that build: the repaint, keys and the prefix
bindings, a resize, `--existing --run-id`, the session's end and the host's
death, and the console modes before, during and after each attach. Run it from
the test binary itself: `cargo test` keeps its tests in a job that forbids
breakaway, where hgs starts no host (D4), so under `cargo test` it skips.
`tests/test_windows_host.py`, `tests/test_windows_host_conpty.py`,
`tests/test_windows_host_survival.py` and
`tests/test_windows_lifecycle.py` take the build from `HGS_TEST_BIN` (default
`target/debug/hgs.exe`) and the fake agent from `HGS_TEST_FAKE_AGENT` (default
`target/debug/examples/fake_agent.exe`); their fake agent cases skip without one.
The ConPTY cases take the pinned pair from `HGS_TEST_CONPTY_DIR` and skip without
it ([the bundled ConPTY pair](#the-bundled-conpty-pair)).
The lifecycle cases install into a temporary profile as `Staged` does and launch
from the build under test, which plays the portable copy: sessions must still run
the staged stable CLI. Their `Attach` cases run `hgs a` and an attached launch on
a pseudoconsole of the test (`PseudoConsole`), which reads the last frame as the
GUI does: once the program has exited, it closes the pseudoconsole and reads its
output to the end. `Listings` checks `local_host` in the local box.
`tests/test_windows_hooks.py` runs the hook shell form through `CreateProcess`,
`cmd.exe` as Kimi runs it, and Git Bash when installed, Codex's PowerShell form
through `pwsh` and Windows PowerShell as Codex runs it (as an isolated test host's
hooks, so local sessions need not be enabled), and tracked fake agents with
private homes in a profile whose folder name has spaces and Cyrillic. The
`windows-desktop` CI job runs the host, ConPTY, survival, lifecycle and hooks
modules and the backend contract against the release `hgs.exe`, with the pinned
pair it fetched in `HGS_TEST_CONPTY_DIR`
([CI](ci-and-aur.md#windows-client-desktop-and-portable-zip)); the `windows-host`
job runs the [real agents](#real-agents).

## Real agents

`tests/test_windows_native_agents.py` runs real Claude Code (`NativeWindowsClaude`)
and Codex (`NativeWindowsCodex`) in isolated session hosts of the build under test,
each with a private home and only a local fake API, so no account or model request
is involved. A class skips only off Windows or without `HGS_TEST_BIN` and its
agent's executable (`HGS_CLAUDE_TEST_BIN`, `HGS_CODEX_TEST_BIN`), and
`HGS_TEST_CONPTY_DIR` gives the pinned pair as for the host suites. Rather than
setting those by hand, run `scripts/ci/agent-compatibility.py`
([native agent compatibility](ci-and-aur.md#native-agent-compatibility-and-upstream-review)):
it resolves one latest `win32-x64` package per agent, records and verifies its SHA-512
integrity, extracts only `claude.exe` and `codex.exe` into a temporary folder and runs
each class with them, and a skipped or empty class fails the run. Codex never gets
its Windows sandbox: the suites run it without one, and its sandbox setup program
is never extracted. In a new Git Bash window with no `HGS_*` variables:

```sh
cargo build --release --locked
python scripts/fetch-conpty.py --output target/conpty
export HGS_TEST_BIN="$(cygpath -w "$PWD/target/release/hgs.exe")"
export HGS_TEST_CONPTY_DIR="$(cygpath -w "$PWD/target/conpty")"
python -X utf8 scripts/ci/agent-compatibility.py </dev/null                # both agents
python -X utf8 scripts/ci/agent-compatibility.py --agent claude </dev/null
```

The report and each class's log land in `artifacts/test-results/agent-compatibility`.
UTF-8 mode keeps the suites' non-ASCII prompts from failing on a Windows code page;
the runner also sets `PYTHONUTF8` for the suites. CI runs the same command in the
`windows-host` job, outside the step's job like the host suites
([CI](ci-and-aur.md#windows-real-agents-through-the-session-host)).

The Claude class has eleven cases. Besides the ordinary launch, input and
lifecycle contract, it requires the exec-form answer channel for questions and
Bash approvals, verifies that a Terminal answer or the owned hook's exit settles
the channel, and checks a Bash panel taller than the maximum unviewed height.
The separate tall-question case deliberately retains an earlier shell-form
tracking hook and asserts that no answer channel was installed: only the host's
unviewed-pane growth can make that question answerable. The Codex class remains
six cases. A channel-capable settings file is not proof of native delivery; the
native classes must run on the exact candidate without skips.

The input key cases also depend on the keyboard layout. Codex reads a control key
through the console as its character plus the Ctrl state, and crossterm resolves that
character's letter in the foreground window's layout: under a Cyrillic layout, Ctrl+C
became Ctrl+С and did nothing (zerus-c8r9). Hosted runners use a US layout, so run the
keys cases on a PC with a non-Latin layout too before trusting a change to key input.

## The terminal backend contract

`tests/test_backend_contract.py` holds the twelve CLI-level tests that every
terminal backend passes ([the contract](ci-and-aur.md#terminal-backend-contract)).
On Windows it runs against the session host (`HGS_TEST_BACKEND=host`, the default
there), with `HGS_TEST_BIN` and `HGS_TEST_FAKE_AGENT` as above. Each test uses its
temporary folder as `USERPROFILE` and `HOME`, installs into it and launches from
the build under test as the lifecycle cases do, and drives sessions only with `hgs`
commands: `ls --json`, `send --json`, `terminal --json`, `rename`, `pause`,
`resume`, `terminate`, `kill` and `a`, the last on a `PseudoConsole` client of the
test. Its sessions are tracked `claude` sessions of the fake agent (a copy named
`claude.exe` that answers the version probe as Claude Code 2.1.296, with a private
`CLAUDE_CONFIG_DIR`) and an untracked `fake_agent`. Whether an agent process still
runs is read through a handle and its creation time. The module's `Host` class and
its console client are its only Windows code, reusing the helpers of the lifecycle
and host suites; a behaviour every backend must have belongs in a contract test,
not there. Like the other host suites, it must run outside a job that
forbids breakaway.

## Writing host tests

Subclass `HostCase` in `tests/test_windows_host.py`. Each case then has its own
`HGS_STATE_DIR`, `USERPROFILE`, `HOME` and work folder below a temporary folder,
runs the build under test as an isolated test host, and stops what it started:
it kills its sessions, stops the host, waits for every host process it saw through
a handle opened while that process ran (never by PID alone), and ends one that
does not exit. Its helpers are `call`, `ok`, `error`, `create`, `cmd`, `capture`,
`info`, `keys`, `attach`, `status` and `wait_for`. `Staged` shows a case that
drops the test variables and installs into its temporary profile.

- Never point a case at your own profile or state directory, and never start a
  host without an isolated `HGS_STATE_DIR`.
- Start every process with `CREATE_NO_WINDOW` and give every wait a deadline.
- Use unique session names and launch IDs (`self.unique`); a host keeps a launch ID
  for a while after its session ended.
- Python started with `-I` ignores `PYTHONUTF8`: start a Python fake API with
  `-X utf8` as well, or a non-ASCII prompt fails it on a Windows code page.
- A survival test that starts the host from inside a job (as sshd does) checks
  `in_job` in the hello: the host must have broken away. A job that forbids
  breakaway makes `__host call --start` fail instead of starting a host that would
  end with that job.

## Survival tests

`tests/test_windows_host_survival.py` checks what a host outlives and what ends
with it (spec D2-D4, D9). Its cases subclass `SurvivalCase`, a `HostCase` that also
closes the named jobs it created, stops its helper processes and ends any process
it pinned that still runs. `Breakaway` runs `__host call --start` from a helper
process that first joins kill-on-close jobs, as a shell inside sshd's job runs
`hgs`: from a job that allows breakaway, the host (`in_job` false) and its session
outlive the job; a job that forbids breakaway starts no host; when breakaway leaves
the host in an outer job that forbids it, the host reports `in_job` true, its
launcher refuses it and it exits once idle. When that outer job also holds the
desktop's shell, a helper whose hidden window it makes the shell window with
`SetShellWindow` (only where no shell runs, as over SSH or on a CI runner), the host
reports `desktop_job` true and its launcher accepts it. A host also outlives the short-lived
client that started it. `Ownership` terminates a host and checks that every process
of its sessions ends with it, a process with a hidden console of its own included.
`StableCli` installs another build (the build under test with a few bytes appended,
so another hash and image) while the staged stable CLI runs an attach client: the
running file moves aside as `hgs.exe.old-<seconds>-<random>` and is never written,
the client finishes normally, and a later install removes the aside file and the
old image once nothing runs them. When the test process itself runs inside a job
that forbids breakaway, every host start fails with that reason; the survival cases
also say where the test process runs.

## The bundled ConPTY pair

A host uses the bundled ConPTY only when `conpty.dll` and `OpenConsole.exe` both
sit next to its own executable (spec D6, `src/host/conpty.rs`), and
`hgs session-host install` stages them from the folder of the `hgs.exe` it runs as
(D2). `hgs __host status` reports the result as `conpty` (`bundled` or `inbox`),
and `host.log` notes it, with the reason for `inbox`, when the host starts.
`python scripts/fetch-conpty.py --output DIR` writes the pair pinned in
`packaging/windows/conpty.json` into `DIR`
([CI](ci-and-aur.md#the-bundled-conpty-pair)); the portable zip ships it next to
`hgs.exe`.

`tests/test_windows_host_conpty.py` takes that folder from `HGS_TEST_CONPTY_DIR`
and checks it against the pin first. `Bundled` copies the build under test into a
temporary image folder with the pair and starts an isolated host from it
(`HGS_HOST_IMAGE`): the host reports `bundled`, its cmd.exe session runs, and the
host's pseudoconsole is the `OpenConsole.exe` of that folder, a child of the host.
`conpty.dll` would otherwise fall back to System32's `conhost.exe` without saying
so. With only `conpty.dll` left and then with neither file, the same copy reports
`inbox` with its reason and starts no `OpenConsole.exe`. `Portable` runs
`session-host install` from a folder laid out like the extracted zip, without the
test host variables: the staged image holds the pinned pair, and the host started
from it uses it. Every case waits until no process holds the files of its image
folders before its temporary folder goes.
