#!/usr/bin/env python3
"""The terminal backend contract, black box at the hgs CLI: a native fake agent, private state, no models.

A terminal backend lists, creates, resolves and attaches sessions, delivers input, captures the
screen, renames, pauses and ends them. These tests drive each operation only through hgs commands
and observe it only through their output and JSON and the records of the agent itself
(examples/fake_agent.rs, linked or copied as `claude`), so one suite holds for every backend.
HGS_TEST_BACKEND selects it: `tmux` (the default on Linux and macOS) or `host` (the Windows
session host of Windows Phase 2, the default on Windows). Only the backend classes below know
which one runs: setup and teardown, `running()`, `describe()`, `alive()` (whether an agent
process still runs) and the terminal client that attaches.

Build the agent with `cargo build --locked --example fake_agent`; HGS_TEST_BIN and
HGS_TEST_FAKE_AGENT name other builds.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import uuid

try:
    import fcntl
    import pty
    import termios
except ImportError:  # Windows: the host backend attaches on a pseudoconsole instead.
    pty = None

REPO = Path(__file__).resolve().parents[1]
EXE = ".exe" if sys.platform == "win32" else ""
HGS = Path(os.environ.get("HGS_TEST_BIN", REPO / f"target/debug/hgs{EXE}")).resolve()
# The approved default (owner, 2026-11-07; PR #1695): a Claude start without an
# account choice runs with its provider's bypass switch after the caller's own
# arguments (accounts::apply_permissions). Spawned argv only; the saved resume
# recipe stays as launched.
BYPASS = ["--permission-mode", "bypassPermissions"]
FAKE_AGENT = Path(os.environ.get("HGS_TEST_FAKE_AGENT", REPO / f"target/debug/examples/fake_agent{EXE}")).resolve()
TMUX = shutil.which("tmux")
# Windows: no console window for any hgs the tests start (docs/windows-host-testing.md).
HIDDEN = getattr(subprocess, "CREATE_NO_WINDOW", 0)
# What an agent may receive, by operation: Unix signals and Windows console events. On
# Windows a pause is Ctrl+Break for the supervisor's process group, terminate pauses first
# and then closes the console, and kill closes it at once (spec D11).
PAUSE_SIGNALS = {"TERM", "CTRL_BREAK"}
CLOSE_SIGNALS = {"HUP", "TERM", "CTRL_BREAK", "CTRL_CLOSE"}
PROMPT = "❯ "
# What the fake `claude` answers the version probe that picks its hook form on Windows (spec D12)
# and whether it gets PostModelSwitch.
CLAUDE_VERSION = "2.1.296 (Claude Code)"


def windows():
    """tests/test_windows_lifecycle.py, whose Windows helpers the host backend reuses: the
    pseudoconsole client and handles on processes pinned by their creation time."""
    tests = str(Path(__file__).resolve().parent)
    if tests not in sys.path:
        sys.path.insert(0, tests)
    import test_windows_lifecycle
    return test_windows_lifecycle


class Tmux:
    """A private tmux server: hgs runs `tmux` by name and finds this wrapper first on PATH."""

    # Seconds a wait for an observation and an hgs command may take.
    WAIT = 10
    COMMAND = 30

    @staticmethod
    def unavailable():
        if sys.platform == "win32":
            return "tmux sessions run on Linux and macOS"
        return None if TMUX else "tmux is required"

    @staticmethod
    def alive(pid, started):
        """Whether this exact process still runs: same start time, not a zombie."""
        result = subprocess.run(["ps", "-p", str(pid), "-o", "stat=,lstart="], capture_output=True, text=True,
                                env=dict(os.environ, LC_ALL="C", TZ="UTC"), timeout=5)
        fields = result.stdout.split(None, 1)
        return len(fields) == 2 and not fields[0].startswith(("Z", "X")) and fields[1].split() == started.split()

    def __init__(self, test):
        self.test = test
        for name in ("claude", "fake_agent"):
            (test.bin / name).symlink_to(FAKE_AGENT)
        # hgs reads Claude Code's version before it registers PostModelSwitch.
        test.env["FAKE_AGENT_PROBE_TEXT"] = CLAUDE_VERSION
        # A tmux found anywhere else still never reaches the user's server.
        folder = test.root / "tmux"
        folder.mkdir()
        folder.chmod(0o700)
        test.env["TMUX_TMPDIR"] = str(folder)
        self.socket = test.root / "tmux.sock"
        # An explicit socket and no configuration file never reach the user's server.
        server = f"{shlex.quote(TMUX)} -S {shlex.quote(str(self.socket))} -f /dev/null"
        test.script("tmux", f'#!/bin/sh\nexec {server} "$@"\n')
        # Panes start agents through `$SHELL -lic`; a login profile could reorder PATH.
        # tmux also runs the pane command through this wrapper as its default shell.
        # Exec it, as bash does with one command and dash does not, so no shell stays
        # the terminal's foreground process (hgs refuses input to a shell).
        test.script("shell", '#!/bin/sh\nshift\ncommand=$1; shift\n'
                    'case $command in exec\\ *) ;; *) command="exec $command" ;; esac\n'
                    'exec /bin/sh -c "$command" "$@"\n')
        test.env["SHELL"] = str(test.bin / "shell")

    def command(self, *args):
        return subprocess.run([TMUX, "-S", str(self.socket), "-f", os.devnull, *args], capture_output=True,
                              text=True, timeout=10)

    def running(self):
        return self.socket.exists() and self.command("list-sessions").returncode == 0

    def describe(self):
        """The pane fields and terminal processes hgs checks before input, for failure messages."""
        panes = self.command("list-panes", "-a", "-F", "#{session_name} #{pane_id} tty=#{pane_tty} "
                             "in_mode=#{pane_in_mode} command=#{pane_current_command} dead=#{pane_dead} "
                             "run=#{@hgs_run} paste=#{bracket_paste_flag}").stdout
        ttys = {field[4:] for line in panes.splitlines() for field in line.split() if field.startswith("tty=")}
        processes = [subprocess.run(["ps", "-o", "pid=,pgid=,tpgid=,tty=,stat=,comm=,args=", "-t", tty.removeprefix("/dev/")],
                                    capture_output=True, text=True).stdout for tty in sorted(ttys)]
        return "tmux panes:\n" + panes + "terminal processes:\n" + "".join(processes)

    def watch(self):
        """Nothing to note after a launch: teardown finds every agent and supervisor in their records."""

    def client(self, *args, cols, rows):
        return PtyClient(self.test, *args, cols=cols, rows=rows)

    def stop(self):
        # kill-server returns before panes exit. Wait for every agent and supervisor
        # of this test, so none writes into its folder after removal.
        owned = self.test.processes()
        self.command("kill-server")
        deadline = time.monotonic() + 6
        while (left := {process for process in owned if self.alive(*process)}) and time.monotonic() < deadline:
            time.sleep(.03)
        for pid, _ in left:
            os.kill(pid, signal.SIGKILL)
        if left:
            raise AssertionError(f"processes survived the private tmux server: {sorted(left)}")


class Host:
    """The Windows session host of Windows Phase 2 (spec D2, D3, D8 to D13), set up as
    tests/test_windows_lifecycle.py sets it up.

    The test folder is the temporary profile (USERPROFILE and HOME), and the isolated
    HGS_STATE_DIR names the host's pipe (it derives from the state directory and its private
    host.secret), so the suite never reaches a live host. `hgs session-host install` enables
    local sessions there: the marker, the stable CLI in `.local\\bin` beside copies of the fake
    agent (links need a privilege on Windows) and a host image, which a launch starts and a
    listing never does. Every hgs runs without a console window. Teardown kills the sessions and
    stops the host through `hgs __host call`, then waits for the host and every agent and
    supervisor through handles opened while they ran, never by PID alone (HostCase in
    tests/test_windows_host.py), and ends and reports any that outlive the deadline.
    """

    # The lifecycle suite's limits (test_windows_host.LIMIT): a freshly copied executable may be
    # scanned when it first starts.
    WAIT = 30
    COMMAND = 60

    @staticmethod
    def unavailable():
        return None if sys.platform == "win32" else "the Windows session host runs on Windows only"

    def __init__(self, test):
        self.test = test
        self.windows = windows()
        # Handles on the host, agent and supervisor processes seen running, by (PID, start).
        self.pinned = {}
        if not HGS.is_file():
            test.fail(f"{HGS} is missing; run cargo build --locked")
        for key in [key for key in test.env if key.upper().startswith("XDG_")]:
            del test.env[key]
        appdata = test.root / "appdata"
        appdata.mkdir()
        # Agents come from the profile's .local\bin, never from the user's %APPDATA%\npm
        # (spec D8), and Claude Code's configuration, where hgs writes its hooks, is the
        # test's own (spec D12).
        test.env.update(USERPROFILE=str(test.root), APPDATA=str(appdata),
                        CLAUDE_CONFIG_DIR=str(test.root / ".claude"), FAKE_AGENT_PROBE_TEXT=CLAUDE_VERSION)
        for name in ("claude", "fake_agent"):
            shutil.copy2(FAKE_AGENT, test.bin / f"{name}{EXE}")
        result = self.run("session-host", "install", "--json")
        if result.returncode != 0 or not (test.bin / f"hgs{EXE}").is_file():
            test.fail(f"hgs session-host install\n{result.stdout}{result.stderr}")

    def run(self, *args):
        return subprocess.run([str(HGS), *args], env=self.test.env, cwd=self.test.root, capture_output=True,
                              text=True, encoding="utf-8", errors="replace", timeout=self.COMMAND,
                              creationflags=HIDDEN, stdin=subprocess.DEVNULL)

    def status(self):
        """`hgs __host status`, which never starts a host; pins the host it reports."""
        result = self.run("__host", "status")
        if result.returncode != 0:
            raise AssertionError(f"hgs __host status\n{result.stdout}{result.stderr}")
        status = json.loads(result.stdout)
        if status.get("running"):
            self.pin(status["host"]["pid"], str(status["host"]["created"]))
        return status

    def call(self, request):
        """One request to the running host (`hgs __host call`, which never starts one): its answer,
        or None when it failed or no host runs."""
        result = self.run("__host", "call", json.dumps(request))
        try:
            return json.loads(result.stdout)["ok"] if result.returncode == 0 else None
        except (ValueError, KeyError, TypeError):
            return None

    def running(self):
        return bool(self.status().get("running"))

    def describe(self):
        """The host's hello and status, its sessions and panes and the end of its log, for failure messages."""
        found = []
        for args in (("__host", "status"), ("__host", "call", json.dumps({"op": "list"}))):
            try:
                result = self.run(*args)
                found.append(f"hgs {shlex.join(args)}: exit {result.returncode}\n{result.stdout}{result.stderr}")
            except (OSError, subprocess.SubprocessError) as error:
                found.append(f"hgs {shlex.join(args)}: {error}\n")
        try:
            log = (Path(self.test.env["HGS_STATE_DIR"]) / "host.log").read_text(encoding="utf-8", errors="replace")
        except OSError:
            log = ""
        return "session host:\n" + "".join(found) + "host.log:\n" + "".join(log.splitlines(True)[-20:])

    def pin(self, pid, started):
        """Keep a handle on process `pid` while it is the one that started at `started`."""
        if (pid, started) not in self.pinned:
            handle = self.windows.open_host(pid, started)
            if handle:
                self.pinned[pid, started] = handle

    def watch(self):
        """After a launch: pin the host and every agent and supervisor of the test while they run."""
        status = self.status()
        for pid, started in self.test.processes():
            self.pin(pid, started)
        return status

    def alive(self, pid, started):
        """Whether this exact process still runs: its creation time in 100 ns ticks, which hgs and
        the fake agent record on Windows (spec D9), matches, and it has not exited."""
        handle = self.windows.open_host(pid, started)
        if not handle:
            return False
        try:
            return self.windows.KERNEL32.WaitForSingleObject(handle, 0) == self.windows.WAIT_TIMEOUT
        finally:
            self.windows.KERNEL32.CloseHandle(handle)

    def client(self, *args, cols, rows):
        if not self.windows.CONPTY:
            raise unittest.SkipTest("attach needs a pseudoconsole, from Windows 10 1809")
        return ConsoleClient(self.test, *args, cols=cols, rows=rows)

    def stop(self):
        try:
            # Kill every session, then stop the idle host (HostCase.stop_sessions).
            if self.watch().get("running"):
                for pane in (self.call({"op": "list"}) or {}).get("panes", []):
                    self.call({"op": "kill", "target": {"pane": pane["pane"]}})
                deadline = time.monotonic() + self.WAIT
                while time.monotonic() < deadline and (self.call({"op": "list"}) or {}).get("sessions"):
                    time.sleep(.2)
                self.call({"op": "stop_host"})
                deadline = time.monotonic() + self.WAIT
                while time.monotonic() < deadline and self.status().get("running"):
                    time.sleep(.2)
        finally:
            # The host's pipe goes before its process, which holds files in the state
            # directory; its kill-on-close job then ends what its sessions left.
            kernel32, left = self.windows.KERNEL32, set()
            deadline = time.monotonic() + self.WAIT
            for (pid, _), handle in self.pinned.items():
                remaining = max(0, int((deadline - time.monotonic()) * 1000))
                if kernel32.WaitForSingleObject(handle, remaining) != self.windows.WAIT_OBJECT_0:
                    left.add(pid)
                    kernel32.TerminateProcess(handle, 1)
                    kernel32.WaitForSingleObject(handle, self.WAIT * 1000)
                kernel32.CloseHandle(handle)
            self.pinned.clear()
        if left:
            raise AssertionError(f"processes survived the session host: {sorted(left)}")


BACKENDS = {"tmux": Tmux, "host": Host}
BACKEND = os.environ.get("HGS_TEST_BACKEND") or ("host" if sys.platform == "win32" else "tmux")
SGR = re.compile(r"\x1b\[([0-9;:]*)m")


def apply_sgr(state, parameters):
    values = [int(value or 0) for value in parameters.replace(":", ";").split(";")]
    while values:
        value = values.pop(0)
        if value == 0:
            state.clear()
        elif value in (1, 2):
            state["bold" if value == 1 else "dim"] = True
        elif value == 22:
            state.pop("bold", None)
            state.pop("dim", None)
        elif 21 <= value <= 29:
            state.pop(value - 20, None)
        elif 30 <= value <= 37 or 90 <= value <= 97 or 40 <= value <= 47 or 100 <= value <= 107:
            state["fg" if value % 100 < 40 else "bg"] = value
        elif value in (39, 49):
            state.pop("fg" if value == 39 else "bg", None)
        elif value in (38, 48) and values[:1] == [5]:
            state["fg" if value == 38 else "bg"] = ("256", values[1])
            del values[:2]
        elif value in (38, 48) and values[:1] == [2]:
            state["fg" if value == 38 else "bg"] = ("rgb", *values[1:4])
            del values[:4]
        else:
            state[value] = True


def styled(screen):
    """Rows of (character, attributes) from a styled capture; attributes carry across rows."""
    state, rows = {}, []
    for line in screen.split("\n"):
        row, position = [], 0
        for match in SGR.finditer(line):
            row += [(ch, frozenset(state.items())) for ch in line[position:match.start()]]
            position = match.end()
            apply_sgr(state, match.group(1))
        rows.append(row + [(ch, frozenset(state.items())) for ch in line[position:]])
    return rows


def style(rows, text):
    """The attributes of every character of the first occurrence of text."""
    for row in rows:
        plain = "".join(ch for ch, _ in row)
        if text in plain:
            start = plain.index(text)
            return {attributes for _, attributes in row[start:start + len(text)]}
    raise AssertionError(f"{text!r} is not on the screen")


class PtyClient:
    """`hgs a` in a private pseudo-terminal of a given size, its output drained."""

    def __init__(self, test, *args, cols=100, rows=30):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        try:
            self.process = subprocess.Popen([str(HGS), "a", *args], env=test.env, cwd=test.root, stdin=slave,
                                            stdout=slave, stderr=slave, start_new_session=True)
        finally:
            os.close(slave)
        self.output = bytearray()
        self.reader = threading.Thread(target=self.drain, daemon=True)
        self.reader.start()
        test.addCleanup(self.close)

    def drain(self):
        while True:
            try:
                data = os.read(self.master, 65536)
            except OSError:
                return
            if not data:
                return
            self.output += data

    def write(self, data):
        os.write(self.master, data)

    def wait(self):
        return self.process.wait(timeout=10)

    def close(self):
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGKILL)
            self.process.wait(timeout=5)
        self.reader.join(timeout=2)
        if self.master is not None:
            os.close(self.master)
            self.master = None


class ConsoleClient:
    """`hgs a` on a pseudoconsole of its own of a given size, as the GUI's embedded terminal runs
    it on Windows (spec D13); test_windows_lifecycle.PseudoConsole drains what it draws."""

    def __init__(self, test, *args, cols=100, rows=30):
        self.console = windows().PseudoConsole([HGS, "a", *args], test.env, test.root, (cols, rows))
        test.addCleanup(self.close)

    def write(self, data):
        self.console.keys(data)

    def wait(self):
        return self.console.wait()

    def close(self):
        self.console.close()


class Contract(unittest.TestCase):
    def setUp(self):
        backend = BACKENDS.get(BACKEND)
        if backend is None:
            self.fail(f"unknown HGS_TEST_BACKEND {BACKEND!r}; choose {' or '.join(BACKENDS)}")
        reason = backend.unavailable()
        if reason:
            self.skipTest(reason)
        if not FAKE_AGENT.is_file():
            self.fail(f"{FAKE_AGENT} is missing; run cargo build --locked --example fake_agent")
        self.root = Path(tempfile.mkdtemp(prefix="hgs-contract-")).resolve()
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        # hgs puts $HOME/.local/bin first on PATH, ahead of Homebrew and /usr/local/bin
        # (on Windows %USERPROFILE%\.local\bin, ahead of %APPDATA%\npm), so the wrappers
        # and agent stand-ins that the backend puts here win over any real tmux or claude.
        self.bin = self.root / ".local" / "bin"
        self.project = self.root / "project"
        for folder in (self.bin, self.project, self.root / "config"):
            folder.mkdir(parents=True)
        self.record = self.root / "agents.jsonl"
        # Nothing from a surrounding session, agent account or fake agent configuration.
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("HGS_", "TMUX", "FAKE_AGENT_", "CLAUDE_", "CODEX_", "KIMI_"))}
        self.env.update(HOME=str(self.root), HGS_CONFIG_DIR=str(self.root / "config"),
                        HGS_STATE_DIR=str(self.root / "state"), HGS_SELF="test", HGS_PEERS="", HGS_TAB="0",
                        TERM="xterm-256color", PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        FAKE_AGENT_RECORD=str(self.record), FAKE_AGENT_READY=str(self.root / "ready.json"),
                        FAKE_AGENT_CONTROL=str(self.root / "control"),
                        FAKE_AGENT_TRANSCRIPT_DIR=str(self.root / "history"), FAKE_AGENT_PROMPT=PROMPT)
        if sys.platform == "darwin":
            # Fake agents need no login: never read or unlock the real keychain.
            self.script("security", "#!/bin/sh\nexit 0\n")
        self.launched = False
        self.backend = backend(self)
        self.addCleanup(self.backend.stop)

    def script(self, name, text):
        path = self.bin / name
        path.write_text(text)
        path.chmod(0o755)

    def configure(self, **options):
        # A backend may keep the environment of the session that started it, so
        # every agent of a test shares one configuration.
        self.assertFalse(self.launched, "configure the fake agent before the first launch")
        self.env.update({"FAKE_AGENT_" + key: value for key, value in options.items()})

    def processes(self):
        """Every agent and supervisor this test has started, as (PID, start time), for teardown."""
        found = {(event["pid"], event.get("process_start")) for event in self.events(kind="start")}
        for path in (self.root / "state").rglob("*.json"):
            try:
                record = json.loads(path.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                continue  # A supervisor can archive a record meanwhile.
            if isinstance(record, dict):
                supervisor = record.get("supervisor") if isinstance(record.get("supervisor"), dict) else {}
                found.add((record.get("pid"), record.get("process_start")))
                found.add((supervisor.get("pid"), supervisor.get("start")))
        return {(pid, started) for pid, started in found
                if isinstance(pid, int) and isinstance(started, str) and started}

    # hgs and its observations

    def hgs(self, *args, rc=0, input=None):
        # Never the caller's terminal: a regressed `a --existing` would attach to it.
        result = subprocess.run([str(HGS), *args], env=self.env, cwd=self.root, input=input, capture_output=True,
                                stdin=subprocess.DEVNULL if input is None else None, text=True, encoding="utf-8",
                                errors="replace", timeout=self.backend.COMMAND, creationflags=HIDDEN)
        if rc is not None:
            self.assertEqual(result.returncode, rc, f"hgs {shlex.join(args)}\n{result.stdout}{result.stderr}"
                             + (self.backend.describe() if self.launched and result.returncode != rc else ""))
        return result

    def json(self, *args, input=None):
        return json.loads(self.hgs(*args, input=input).stdout)

    def sessions(self):
        return {session["name"]: session for session in self.json("ls", "--json", "--local")["sessions"]}

    def events(self, pid=None, kind=None):
        # A line still being appended has no newline yet.
        text = self.record.read_text(encoding="utf-8", errors="replace") if self.record.exists() else ""
        lines = text.split("\n")[:-1]
        return [event for event in map(json.loads, lines)
                if (pid is None or event["pid"] == pid) and (kind is None or event["kind"] == kind)]

    def tokens(self, pid):
        """What reached the agent's input, one record per token."""
        return [event for event in self.events(pid) if "read" in event]

    def received(self, pid):
        return b"".join(bytes.fromhex(token["hex"]) for token in self.tokens(pid))

    def agent_arguments(self, agent):
        """The agent's own arguments of a tracked Claude Code start. On Unix hgs adds its status line
        command as flag settings for this version (`claude_statusline`), which no file keeps; the
        Windows session host adds none."""
        argv = list(agent["argv"])
        if sys.platform == "win32":
            self.assertNotIn("--settings", argv)
            return argv
        at = argv.index("--settings")
        line = json.loads(argv[at + 1])["statusLine"]
        self.assertEqual((line["type"], shlex.split(line["command"])[1:]), ("command", ["__state", "statusline"]))
        del argv[at:at + 2]
        return argv

    def wait_for(self, predicate, what, timeout=None):
        deadline = time.monotonic() + (timeout or self.backend.WAIT)
        while not (value := predicate()):
            if time.monotonic() > deadline:
                self.fail(f"timed out waiting for {what}; agent records:\n" +
                          "\n".join(map(json.dumps, self.events()[-30:])))
            time.sleep(.03)
        return value

    def launch(self, tag="one", program="claude"):
        """Start a session as the GUI does; return its name, listing row and agent start record."""
        self.launched = True
        before = {event["pid"] for event in self.events(kind="start")}
        launch_id = str(uuid.uuid4())
        name = f"{program}/project/{tag}"
        result = self.hgs(program, str(self.project), "--new", "-n", tag, "-d", "--launch-id", launch_id)
        self.assertEqual(result.stdout, f"hgs: started {name}\n")
        agent = self.wait_for(lambda: next((event for event in self.events(kind="start") if event["pid"] not in before
                                            and self.events(event["pid"], "ready")), None), f"{name} to start")
        self.backend.watch()
        if program == "claude":
            # Confirmed by the agent's own SessionStart hook.
            row = self.wait_for(lambda: (row := self.sessions().get(name)) and row.get("activity") == "idle"
                                and row.get("conversation_id") == agent["session_id"] and row, f"{name} to be ready")
            if self.env.get("FAKE_AGENT_RAW") != "0":
                # The agent sets its modes before it is ready, but the backend may parse
                # them later; input is refused until it reports bracketed paste.
                self.screen(row, lambda frame, rows: frame["bracket_paste"], f"{name}'s input modes")
        else:
            row = self.sessions()[name]
        self.assertEqual(row["launch_id"], launch_id)
        return name, row, agent

    def resolves(self, name):
        """Whether `hgs a NAME --existing` finds a live session (refused ones never attach)."""
        result = self.hgs("a", name, "--existing", "--run-id", "no-such-run", rc=None)
        self.assertNotEqual(result.returncode, 0)
        if "session run changed" in result.stderr:
            return True
        self.assertIn("no longer running", result.stderr)
        return False

    def message(self, row, text):
        return dict(request_id=str(uuid.uuid4()), text=text, attachments=[], expected_run_id=row["run_id"],
                    expected_conversation_id=row["conversation_id"])

    def send(self, name, payload, rc=0):
        result = self.hgs("send", name, "--json", input=json.dumps(payload), rc=rc)
        return json.loads(result.stdout) if rc == 0 else result.stderr

    def snapshot(self, row):
        """The scoped Terminal frame of a listed session: screen, size, cursor, modes and binding."""
        return self.json("terminal", row["name"], "--json", input=json.dumps(dict(
            request_id=str(uuid.uuid4()), action="snapshot", expected_run_id=row["run_id"],
            expected_conversation_id=row["conversation_id"])))

    def terminal(self, frame, **fields):
        """Scoped Terminal input bound to a frame's terminal; returns the request and its receipt."""
        request = dict(request_id=str(uuid.uuid4()), action="input", expected_run_id=frame["run_id"],
                       expected_conversation_id=frame["conversation_id"],
                       terminal_binding_id=frame["terminal_binding_id"], expires_at=time.time() + 5, **fields)
        return request, self.json("terminal", frame["name"], "--json", input=json.dumps(request))

    def key(self, frame, pid, key):
        """Press one key and return the token the agent received for it."""
        count = len(self.tokens(pid))
        _, receipt = self.terminal(frame, key=key)
        self.assertEqual(receipt["status"], "submitted", receipt)
        self.wait_for(lambda: len(self.tokens(pid)) > count, f"{key} to reach the agent")
        token, = self.tokens(pid)[count:]
        return token

    def control(self, pid, text):
        """Run lines of the agent's control file (one agent of the test at a time)."""
        # Control bytes typed into the agent share the kind; only command records count.
        def commands():
            return [event for event in self.events(pid, "control") if "command" in event]
        done = len(commands()) + len(text.strip().splitlines())
        temporary = self.root / "control.tmp"
        temporary.write_text(text, encoding="utf-8")
        os.replace(temporary, self.root / "control")
        self.wait_for(lambda: len(commands()) >= done, "the agent's control lines")
        self.assertFalse([event for event in commands() if "error" in event])

    def screen(self, row, predicate, what):
        """Wait until a snapshot satisfies predicate(frame, plain rows)."""
        def ready():
            frame = self.snapshot(row)
            return predicate(frame, ["".join(ch for ch, _ in cells) for cells in styled(frame["screen"])]) and frame
        return self.wait_for(ready, what)

    def alive(self, pid, started):
        """Whether this exact process still runs, as the backend tells."""
        return self.backend.alive(pid, started)

    def client(self, *args, cols=100, rows=30):
        """`hgs a ARGS` on the backend's terminal client, of this size."""
        return self.backend.client(*args, cols=cols, rows=rows)


class Sessions(Contract):
    def test_listing_reports_launched_sessions_and_never_starts_the_backend(self):
        self.assertEqual(self.json("ls", "--json", "--local")["sessions"], [])
        self.assertFalse(self.resolves("claude/project/one"))
        self.assertFalse(self.backend.running(), "a listing or a refused attach started the backend")
        started = time.time()
        name, row, agent = self.launch()
        plain, _, untracked = self.launch("plain", program="fake_agent")
        rows = self.sessions()
        self.assertEqual(set(rows), {name, plain})
        self.assertEqual({key: row.get(key) for key in ("cmd", "project", "tag", "attached", "clients",
                                                        "runtime_state", "process_state", "resumable", "cwd")},
                         dict(cmd="claude", project="project", tag="one", attached=0, clients=[],
                              runtime_state="live", process_state="running", resumable=True,
                              cwd=str(self.project)))
        self.assertTrue(row["run_id"])
        self.assertTrue(started - 2 <= row["created"] <= time.time() + 1, row["created"])
        # The agent runs in the session folder on a terminal of its own, with nothing typed,
        # and only the default bypass switch of its own.
        self.assertEqual((self.agent_arguments(agent), agent["cwd"], agent["tty"], agent["raw"]),
                         (BYPASS, str(self.project), True, True))
        self.assertEqual(self.received(agent["pid"]), b"")
        # An untracked command is listed and runs the same way, without a conversation.
        self.assertEqual({key: rows[plain].get(key) for key in ("cmd", "project", "tag", "attached", "clients")},
                         dict(cmd="fake_agent", project="project", tag="plain", attached=0, clients=[]))
        self.assertFalse(rows[plain].get("conversation_id"))
        self.assertEqual((untracked["argv"], untracked["cwd"], untracked["tty"]), ([], str(self.project), True))
        self.assertTrue(self.backend.running())
        # A name is created once: the second launch starts nothing and changes nothing.
        again = self.hgs("claude", str(self.project), "--new", "-n", "one", "-d", "--launch-id", str(uuid.uuid4()),
                         rc=1)
        self.assertIn("already exists", again.stderr)
        self.assertEqual(len(self.events(kind="start")), 2)
        self.assertEqual({key: self.sessions()[name][key] for key in ("run_id", "launch_id", "conversation_id")},
                         {key: row[key] for key in ("run_id", "launch_id", "conversation_id")})

    def test_rename_keeps_the_running_agent(self):
        self.configure(ECHO="1")  # Claude shows the paste it settles on (zerus-iitw8).
        name, row, agent = self.launch()
        pid = agent["pid"]
        renamed = "claude/project/renamed"
        self.assertEqual(self.hgs("rename", name, renamed).stdout, f"hgs: renamed {name} to {renamed}\n")
        rows = self.sessions()
        self.assertNotIn(name, rows)
        self.assertEqual({key: rows[renamed][key] for key in ("run_id", "conversation_id", "runtime_state",
                                                              "process_state", "launch_id")},
                         {key: row[key] for key in ("run_id", "conversation_id", "runtime_state",
                                                    "process_state", "launch_id")})
        # The same process keeps its terminal: no restart, signal or end of input.
        self.assertEqual([event["kind"] for event in self.events(pid) if event["kind"] in
                          ("start", "signal", "eof", "exit")], ["start"])
        self.assertTrue(self.alive(pid, agent["process_start"]))
        self.assertFalse(self.resolves(name))
        self.assertTrue(self.resolves(renamed))
        self.send(renamed, self.message(rows[renamed], "after rename"))
        self.wait_for(lambda: self.received(pid) == b"\x1b[200~after rename\x1b[201~\r", "the message")
        # A live name cannot be taken.
        other, other_row, _ = self.launch("other")
        self.assertIn("already uses this name", self.hgs("rename", renamed, other, rc=1).stderr)
        rows = self.sessions()
        self.assertEqual((rows[renamed]["run_id"], rows[other]["run_id"]), (row["run_id"], other_row["run_id"]))

    def test_pause_signals_only_the_agent_and_resume_restores_its_conversation(self):
        self.configure(ON_SIGNAL="TERM=delay:200,CTRL_BREAK=delay:200")
        name, row, agent = self.launch()
        kept, kept_row, kept_agent = self.launch("kept")
        pid = agent["pid"]
        self.assertEqual(self.hgs("pause", name).stdout, f"hgs: paused {name}\n")
        # Pause returns once the agent has exited by its own handler, never forced.
        signals = self.events(pid, "signal")
        self.assertEqual(len(signals), 1, signals)
        self.assertIn(signals[0]["signal"], PAUSE_SIGNALS)
        exit_event, = self.events(pid, "exit")
        self.assertEqual(exit_event["cause"], "signal:" + signals[0]["signal"])
        self.assertGreaterEqual(exit_event["t"] - signals[0]["t"], 0.2)
        self.assertEqual(self.tokens(pid), [])
        self.assertFalse(self.alive(pid, agent["process_start"]))
        saved = self.sessions()[name]
        self.assertEqual({key: saved.get(key) for key in ("state", "runtime_state", "process_state", "resumable",
                                                          "conversation_id")},
                         dict(state="paused", runtime_state="stopped", process_state="exited", resumable=True,
                              conversation_id=row["conversation_id"]))
        self.assertFalse(self.resolves(name))
        # Another session is untouched.
        self.assertEqual(self.sessions()[kept]["run_id"], kept_row["run_id"])
        self.assertEqual([event["kind"] for event in self.events(kept_agent["pid"])
                          if event["kind"] in ("signal", "eof", "exit")], [])
        # Resume starts the agent again in its folder on the same conversation.
        self.hgs("resume", name, "-d")
        resumed = self.wait_for(lambda: next((event for event in self.events(kind="start")
                                              if event["pid"] not in (pid, kept_agent["pid"])), None), "the resume")
        self.assertEqual((self.agent_arguments(resumed), resumed["session_id"], resumed["resumed"], resumed["cwd"]),
                         (["--resume", row["conversation_id"], *BYPASS], row["conversation_id"], True, str(self.project)))
        live = self.wait_for(lambda: (live := self.sessions()[name]).get("runtime_state") == "live"
                             and live.get("activity") == "idle" and live["run_id"] != row["run_id"] and live,
                             "the resumed session")
        self.assertEqual((live["conversation_id"], live["process_state"]), (row["conversation_id"], "running"))
        self.assertTrue(self.resolves(name))

    def test_terminate_waits_for_the_agent_then_archives_the_session(self):
        # Unix Terminate fences and forcibly ends the verified tree, even when
        # native callbacks ignore shutdown. Windows retains its graceful delay.
        response = "ignore" if BACKEND == "tmux" else "delay:300"
        self.configure(ON_EOF="ignore", ON_SIGNAL=",".join(f"{event}={response}" for event in sorted(CLOSE_SIGNALS)))
        name, row, agent = self.launch()
        kept, kept_row, kept_agent = self.launch("kept")
        pid = agent["pid"]
        result = self.hgs("terminate", name)
        self.assertTrue(result.stdout.startswith(f"hgs: terminated and archived {name} ("), result.stdout)
        if BACKEND == "tmux":
            self.assertEqual(self.events(pid, "exit"), [], "Forced exit must not depend on a native exit callback")
        else:
            exit_event, = self.events(pid, "exit")
            signals = self.events(pid, "signal")
            self.assertTrue(signals and {event["signal"] for event in signals} <= CLOSE_SIGNALS, signals)
            self.assertEqual(exit_event["cause"], "signal:" + signals[0]["signal"])
            self.assertGreaterEqual(exit_event["t"] - signals[0]["t"], 0.3)
        self.assertEqual(self.tokens(pid), [])
        self.assertFalse(self.alive(pid, agent["process_start"]))
        archived = self.sessions()[name]
        self.assertEqual({key: archived.get(key) for key in ("state", "runtime_state", "conversation_id")},
                         dict(state="archived", runtime_state="stopped", conversation_id=row["conversation_id"]))
        self.assertFalse(self.resolves(name))
        self.assertEqual(self.sessions()[kept]["run_id"], kept_row["run_id"])
        self.assertTrue(self.alive(kept_agent["pid"], kept_agent["process_start"]))
        self.assertEqual(self.events(kept_agent["pid"], "signal"), [])
        if BACKEND == "tmux":
            # Both agents deliberately ignore hangup. After proving isolation,
            # explicitly terminate the retained fixture before socket teardown.
            self.hgs("terminate", kept, "--expected-run-id", kept_row["run_id"])

    def test_kill_ends_the_agent_and_forgets_the_session_but_not_its_history(self):
        name, row, agent = self.launch()
        pid = agent["pid"]
        self.hgs("kill", name)
        self.wait_for(lambda: not self.alive(pid, agent["process_start"]), "the agent process to end")
        # The agent saw its terminal close and ended by itself.
        exit_event, = self.events(pid, "exit")
        self.assertIn(exit_event["cause"], {"eof"} | {"signal:" + event for event in CLOSE_SIGNALS})
        self.assertEqual(self.tokens(pid), [])
        self.assertNotIn(name, self.sessions())
        self.assertFalse(self.resolves(name))
        self.assertIn("no such session", self.hgs("kill", name, rc=1).stderr)
        self.assertTrue((self.root / "history" / (row["conversation_id"] + ".jsonl")).is_file())

    def ended_launches(self):
        return {launch["launch_id"]: launch for launch in self.json("ls", "--json", "--local").get("ended_launches", [])}

    def test_a_launch_whose_session_ends_without_a_conversation_is_listed_as_ended(self):
        # The desktop's launch placeholder waits for a session or record with its launch ID.
        # An agent that started but then cannot go on says why and exits, without a
        # conversation: the listing reports how its launch ended, with its last lines.
        self.configure(HOOKS="none")
        self.launched = True
        launch_id = str(uuid.uuid4())
        name = "claude/project/failing"
        result = self.hgs("claude", str(self.project), "--new", "-n", "failing", "-d", "--launch-id", launch_id)
        self.assertEqual(result.stdout, f"hgs: started {name}\n")
        agent = self.wait_for(lambda: next((event for event in self.events(kind="ready")), None), "the agent")
        # Running, it is no ended launch.
        self.assertEqual(self.sessions()[name]["launch_id"], launch_id)
        self.assertNotIn(launch_id, self.ended_launches())
        staged = self.root / "control.new"
        staged.write_text("write \\r\\ncannot start: example failure\\r\\n\nexit 3\n", encoding="utf-8")
        os.replace(staged, self.root / "control")
        ended = self.wait_for(lambda: self.ended_launches().get(launch_id), "the ended launch")
        self.assertEqual((ended["name"], ended["code"]), (name, 3))
        self.assertEqual(ended["output"].splitlines()[-1], "cannot start: example failure")
        self.assertEqual([event["code"] for event in self.events(agent["pid"], "exit")], [3])
        self.assertNotIn(name, self.sessions())

    def test_a_launch_that_confirmed_a_conversation_is_never_listed_as_ended(self):
        name, row, _ = self.launch()
        self.hgs("pause", name)
        self.assertEqual(self.sessions()[name]["launch_id"], row["launch_id"])
        # Its record carried the launch ID; once it is forgotten the launch is still not ended.
        self.hgs("kill", name)
        self.assertNotIn(name, self.sessions())
        self.assertNotIn(row["launch_id"], self.ended_launches())

    @unittest.skipIf(os.name == "nt", "POSIX native-process ancestry fixture")
    def test_a_launch_run_by_a_live_agent_records_it_as_the_parent(self):
        # This test process stands in for a live tracked agent: its record
        # names this process, so the launching hgs proves it as the caller.
        start = subprocess.check_output(["ps", "-p", str(os.getpid()), "-o", "lstart="],
                                        env=dict(self.env, LC_ALL="C", TZ="UTC"), text=True).strip()
        lead_id = str(uuid.uuid4())
        lead = dict(version=1, name="claude/project/lead", agent="claude", run_id="run-lead",
                    conversation_id="conversation-lead", session_id=lead_id, subagents={},
                    pid=os.getpid(), process_start=start)
        state = self.root / "state"
        state.mkdir(exist_ok=True)
        lead_path = state / (hashlib.sha256(lead["name"].encode()).hexdigest() + ".json")
        lead_path.write_text(json.dumps(lead))
        # Teardown stops every recorded process; this one is the test itself.
        self.addCleanup(lead_path.unlink, missing_ok=True)
        # The link reads this computer's identity from its existing shared catalog.
        self.hgs("reference", lead["name"], "--json")
        # A value inherited from a surrounding session never reaches the record.
        self.env.update(HGS_SESSION=lead["name"], HGS_RUN_ID="run-lead", HGS_PARENT='{"forged": true}')
        name, row, _ = self.launch("kid")
        parent = row["parent"]
        self.assertEqual((parent["session_id"], parent["name"], parent["run_id"], parent["source"]),
                         (lead_id, lead["name"], "run-lead", "launch"))
        # The lead is not running in this backend, so the child is not supervised.
        self.assertEqual(parent["state"], "saved")
        # A launch the person runs outside any agent records no parent.
        for key in ("HGS_SESSION", "HGS_RUN_ID"):
            self.env.pop(key)
        _, plain, _ = self.launch("plain")
        self.assertNotIn("parent", plain)
        # Pause and resume keep the link.
        self.hgs("pause", name)
        self.assertEqual(self.sessions()[name]["parent"]["session_id"], lead_id)

    def test_a_launch_from_an_agent_command_passes_none_of_its_context_on(self):
        # zerus-01sz8: an agent's command carries that agent's session markers and its
        # plain-text switch. tmux kept the environment of the client that started its
        # server for every later pane, so NO_COLOR made Claude Code draw its prompt
        # placeholder without styles, and hgs send refused it as a draft.
        context = {"NO_COLOR": "1", "CLAUDECODE": "1", "AI_AGENT": "claude-code_test_agent",
                   # Above Linux's largest PID: never an ancestor, so no caller check matches it.
                   "CLAUDE_CODE_SESSION_ID": str(uuid.uuid4()), "CLAUDE_PID": "4194305",
                   "CLAUDE_CODE_MESSAGING_SOCKET": str(self.root / "cc.sock"),
                   "CODEX_THREAD_ID": str(uuid.uuid4()), "DSH_SESSION_ID": "session-" + str(uuid.uuid4())}
        kept = {"ZERUS_TEST_KEPT": "1"}

        def environment(pid):
            raw = Path(f"/proc/{pid}/environ").read_bytes()
            return dict(entry.split("=", 1) for entry in raw.decode("utf-8", "replace").split("\0") if "=" in entry)

        def assert_agent_clean(agent):
            if not sys.platform.startswith("linux"):
                return  # Only Linux shows another process's environment.
            seen = environment(agent["pid"])
            self.assertEqual({key: seen[key] for key in context if key in seen}, {})
            self.assertEqual(seen.get("ZERUS_TEST_KEPT"), "1")

        # A launch run by an agent's command, which may start the terminal server.
        self.env.update(context, **kept)
        _, _, agent = self.launch("from-agent")
        assert_agent_clean(agent)
        if BACKEND == "tmux":
            listing = self.backend.command("show-environment", "-g").stdout.splitlines()
            server = {line.split("=", 1)[0] for line in listing if "=" in line}
            self.assertEqual(server & set(context), set())
        # A server that already holds them, started by something else: the agent still starts without them.
        for key in context:
            self.env.pop(key)
        if BACKEND == "tmux":
            for key, value in context.items():
                self.assertEqual(self.backend.command("set-environment", "-g", key, value).returncode, 0)
        _, _, later = self.launch("later")
        assert_agent_clean(later)

    def test_a_launch_records_a_parent_another_computer_forwarded(self):
        # zerus-sa9qn.7: the peer side of a remote launch. The link names another
        # computer, so this computer lists the parent as missing: never supervised.
        link = {"schema": 1, "machine": "99999999-9999-4999-8999-999999999999", "host": "mac",
                "session_id": "22222222-2222-4222-8222-222222222222", "name": "claude/elsewhere/lead",
                "conversation_id": "c", "run_id": "r", "source": "launch", "at": 1}
        self.launched = True
        launch_id = str(uuid.uuid4())
        result = self.hgs("claude", str(self.project), "--new", "-n", "forwarded", "-d", "--launch-id", launch_id,
                          "--parent-link", json.dumps(link, separators=(",", ":")).encode().hex())
        self.assertEqual(result.stdout, "hgs: started claude/project/forwarded\n")
        row = self.wait_for(lambda: (row := self.sessions().get("claude/project/forwarded")) and row.get("parent") and row,
                            "the forwarded parent")
        self.assertEqual((row["parent"]["session_id"], row["parent"]["host"], row["parent"]["state"]),
                         (link["session_id"], "mac", "missing"))
        bad = self.hgs("claude", str(self.project), "--new", "-n", "refused", "-d", "--parent-link", "zz", rc=None)
        self.assertNotEqual(bad.returncode, 0)


class Input(Contract):
    def test_send_pastes_exact_bytes_then_one_enter_and_never_repeats(self):
        # The agent shows the paste in its composer as Claude does: hgs presses
        # Enter only once it is settled there (zerus-iitw8).
        self.configure(ECHO="1")
        name, row, agent = self.launch()
        pid = agent["pid"]
        marker = self.root / "never-created"
        text = f"Привет\nsecond line\t$(touch {marker}) `literal` ✓"
        payload = self.message(row, text)
        receipt = self.send(name, payload)
        self.assertEqual((receipt["status"], receipt["submitted_text"], receipt["run_id"], receipt["native"]),
                         ("submitted", text, row["run_id"], "transcript"))
        self.wait_for(lambda: len(self.tokens(pid)) == 2, "the message")
        paste, enter = self.tokens(pid)
        self.assertEqual((paste["kind"], paste["text"], enter["kind"]), ("paste", text, "enter"))
        self.assertEqual(self.received(pid), b"\x1b[200~" + text.encode() + b"\x1b[201~\r")
        # Enter is its own write, after the whole paste.
        self.assertGreater(enter["read"], paste["read"])
        self.assertFalse(marker.exists())
        # The same request is acknowledged again without a second delivery.
        self.assertEqual(self.send(name, payload), receipt)
        # The agent's own hooks confirm the turn; a new request is delivered once more.
        self.wait_for(lambda: self.events(pid, "hook")[-1]["event"] == "Stop"
                      and self.sessions()[name]["activity"] == "idle", "the end of the turn")
        self.send(name, self.message(row, "second"))
        self.wait_for(lambda: len(self.tokens(pid)) == 4, "the second message")
        self.assertEqual(self.received(pid), b"\x1b[200~" + text.encode() + b"\x1b[201~\r\x1b[200~second\x1b[201~\r")
        self.assertEqual([token["kind"] for token in self.tokens(pid)].count("enter"), 2)

    def test_send_needs_an_empty_composer_read_from_the_screen(self):
        self.configure(ECHO="1")
        name, row, agent = self.launch()
        pid = agent["pid"]
        payload = self.message(row, "hello")
        # A draft that the agent echoes on its prompt line.
        frame = self.snapshot(row)
        self.assertEqual(self.terminal(frame, text="draft", enter=False)[1]["status"], "submitted")
        self.screen(row, lambda frame, rows: rows[0] == PROMPT + "draft" and frame["cursor"] == dict(x=7, y=0),
                    "the draft")
        self.assertIn("draft", self.send(name, payload, rc=1))
        for _ in "draft":
            self.assertEqual(self.key(frame, pid, "BSpace")["hex"], "7f")
        self.screen(row, lambda frame, rows: rows[0] == PROMPT.strip() and frame["cursor"] == dict(x=2, y=0),
                    "an empty prompt")
        # Text after the prompt is a draft unless it is drawn dim, like a placeholder.
        self.control(pid, "write \\e[2J\\e[H" + PROMPT + 'Try "fix lint"\\e[1;3H')
        self.screen(row, lambda frame, rows: rows[0] == PROMPT + 'Try "fix lint"' and frame["cursor"] == dict(x=2, y=0),
                    "plain text after the prompt")
        self.assertIn("draft", self.send(name, payload, rc=1))
        self.control(pid, "write \\e[2J\\e[H" + PROMPT + '\\e[2mTry "fix lint"\\e[0m\\e[1;3H')
        frame = self.screen(row, lambda frame, rows: style(styled(frame["screen"]), 'Try "fix lint"') ==
                            {frozenset({("dim", True)})} and frame["cursor"] == dict(x=2, y=0), "a dim placeholder")
        self.assertEqual(style(styled(frame["screen"]), PROMPT.strip()), {frozenset()})
        self.assertEqual(self.send(name, payload)["status"], "submitted")
        # The refused sends typed nothing: only the draft, its erasure and this message.
        self.wait_for(lambda: len(self.tokens(pid)) == 8, "the message")
        self.assertEqual(self.received(pid),
                         b"\x1b[200~draft\x1b[201~" + b"\x7f" * 5 + b"\x1b[200~hello\x1b[201~\r")

    def test_send_needs_a_raw_terminal(self):
        self.configure(RAW="0")
        name, row, agent = self.launch()
        self.assertFalse(agent["raw"])
        self.assertIn("interactive input terminal", self.send(name, self.message(row, "hello"), rc=1))
        self.assertEqual(self.received(agent["pid"]), b"")

    def test_terminal_snapshot_reports_the_agents_screen_size_cursor_and_modes(self):
        name, row, agent = self.launch()
        pid = agent["pid"]
        drawing = self.root / "screen.bin"
        drawing.write_bytes("\x1b[2J\x1b[Hfirst line   \r\nsecond \u2713\r\n\x1b[1;31mbold red\x1b[0m tail\r\n"
                            "\x1b[2mdim\x1b[22m plain \x1b[38;5;208mx\x1b[38;2;1;2;3my\x1b[0m\r\n\u6f22\u5b57".encode())
        self.control(pid, f"render {drawing}")
        # ConPTY can repaint the text before it moves the cursor past the wide glyphs.
        frame = self.screen(row, lambda frame, rows: rows[4] == "\u6f22\u5b57" and frame["cursor"] == dict(x=4, y=4),
                            "the drawing and its cursor")
        resizes = self.events(pid, "resize")
        cols, rows = (resizes[-1]["cols"], resizes[-1]["rows"]) if resizes else agent["size"]
        self.assertEqual((frame["cols"], frame["rows"]), (cols, rows))
        # Every visible row, trailing spaces trimmed; wide glyphs once, the cursor counts cells.
        lines = frame["screen"].split("\n")
        self.assertEqual((len(lines), lines[-1]), (rows + 1, ""))
        self.assertFalse([line for line in lines if line.endswith(" ")])
        cells = styled(frame["screen"])
        self.assertEqual(["".join(ch for ch, _ in row) for row in cells[:6]],
                         ["first line", "second \u2713", "bold red tail", "dim plain xy", "\u6f22\u5b57", ""])
        self.assertEqual(frame["cursor"], dict(x=4, y=4))
        # Attributes as the agent drew them.
        expected = {"first line": {}, "bold red": {"bold": True, "fg": 31}, " tail": {}, "dim": {"dim": True},
                    " plain ": {}, "x": {"fg": ("256", 208)}, "y": {"fg": ("rgb", 1, 2, 3)}, "\u6f22\u5b57": {}}
        for text, attributes in expected.items():
            self.assertEqual(style(cells, text), {frozenset(attributes.items())}, text)
        self.assertEqual((frame["bracket_paste"], frame["multiline_supported"], frame["input_supported"]),
                         (True, True, True))
        # Bracketed paste follows the agent; without it a message is refused.
        self.control(pid, "modes no-paste")
        frame = self.screen(row, lambda frame, rows: not frame["bracket_paste"], "bracketed paste to end")
        self.assertFalse(frame["multiline_supported"])
        self.assertIn("input mode", self.send(name, self.message(row, "hello"), rc=1))
        self.control(pid, "modes paste")
        self.screen(row, lambda frame, rows: frame["bracket_paste"] and frame["multiline_supported"],
                    "bracketed paste again")
        self.assertEqual(self.received(pid), b"")

    def test_terminal_input_keys_and_text_arrive_exactly_once(self):
        name, row, agent = self.launch()
        pid = agent["pid"]
        frame = self.snapshot(row)
        keys = [("Tab", b"\t"), ("BTab", b"\x1b[Z"), ("BSpace", b"\x7f"), ("C-c", b"\x03"), ("C-d", b"\x04"),
                ("Up", b"\x1b[A"), ("Right", b"\x1b[C"), ("Escape", b"\x1b"), ("Enter", b"\r")]
        for key, value in keys:
            self.assertEqual(bytes.fromhex(self.key(frame, pid, key)["hex"]), value, key)
        # Cursor keys follow the agent's application cursor mode.
        self.control(pid, "modes cursor")
        self.assertEqual(bytes.fromhex(self.key(frame, pid, "Up")["hex"]), b"\x1bOA")
        self.control(pid, "modes no-cursor")
        before = self.received(pid)
        # Text is one bracketed paste, then Enter; a replayed request delivers nothing.
        request, receipt = self.terminal(frame, text="two\nlines", enter=True)
        self.assertEqual(receipt["status"], "submitted", receipt)
        self.wait_for(lambda: self.received(pid) == before + b"\x1b[200~two\nlines\x1b[201~\r", "the text")
        self.assertEqual(self.json("terminal", name, "--json", input=json.dumps(request)), receipt)
        # Without bracketed paste one line arrives as typed and several are refused.
        self.control(pid, "modes no-paste")
        before = self.received(pid)
        _, receipt = self.terminal(frame, text="plain", enter=False)
        self.assertEqual(receipt["status"], "submitted", receipt)
        self.wait_for(lambda: self.received(pid) == before + b"plain", "the typed text")
        _, receipt = self.terminal(frame, text="a\nb", enter=False)
        self.assertEqual(receipt["status"], "failed")
        self.assertIn("bracketed paste", receipt["error"])
        # The next key arrives right after the typed text: the refused one sent nothing.
        self.assertEqual(self.key(frame, pid, "Enter")["hex"], "0d")
        self.assertEqual(self.received(pid), before + b"plain\r")


class Attach(Contract):
    def attached(self, name, count):
        return self.wait_for(lambda: (row := self.sessions()[name])["attached"] == count
                             and len(row["clients"]) == count and row, f"{count} attached clients")

    def test_existing_attach_resolves_only_the_live_run(self):
        name, row, agent = self.launch()
        self.assertIn("session run changed", self.hgs("a", name, "--existing", "--run-id", "other", rc=1).stderr)
        self.assertIn("no longer running",
                      self.hgs("a", "claude/project/missing", "--existing", "--run-id", row["run_id"], rc=1).stderr)
        self.assertIn("--run-id requires --existing", self.hgs("a", name, "--run-id", row["run_id"], rc=1).stderr)
        # Nothing was created, attached or typed.
        self.assertEqual(set(self.sessions()), {name})
        self.assertEqual(self.sessions()[name]["attached"], 0)
        self.assertEqual(self.received(agent["pid"]), b"")

    def test_attach_relays_input_and_size_and_detaches_without_stopping_the_agent(self):
        name, row, agent = self.launch()
        pid = agent["pid"]
        first = self.client(name, "--existing", "--run-id", row["run_id"], cols=100, rows=30)
        self.attached(name, 1)
        # The agent gets the client's width; a status line may take rows.
        size = self.wait_for(lambda: next((event for event in reversed(self.events(pid, "resize"))
                                           if event["cols"] == 100), None), "the client's size")
        self.assertTrue(0 < size["rows"] <= 30, size)
        first.write(b"typed")
        self.wait_for(lambda: self.received(pid) == b"typed", "typed text")
        # Ctrl+B Ctrl+B sends one literal Ctrl+B.
        first.write(b"\x02\x02")
        self.wait_for(lambda: self.received(pid) == b"typed\x02", "a literal Ctrl+B")
        # Several clients attach; the size follows the latest one still attached.
        second = self.client(name, "--existing", cols=90, rows=20)
        self.attached(name, 2)
        self.wait_for(lambda: self.events(pid, "resize")[-1]["cols"] == 90, "the latest client's size")
        second.write(b"\x02d")
        self.assertEqual(second.wait(), 0)
        self.attached(name, 1)
        self.wait_for(lambda: self.events(pid, "resize")[-1]["cols"] == 100, "the remaining client's size")
        first.write(b"\x02d")
        self.assertEqual(first.wait(), 0)
        self.attached(name, 0)
        # Detaching typed nothing and left the agent running at the last size.
        self.assertEqual(self.received(pid), b"typed\x02")
        self.assertEqual([event for event in self.events(pid) if event["kind"] in ("signal", "eof", "exit")], [])
        frame = self.snapshot(row)
        last = self.events(pid, "resize")[-1]
        self.assertEqual((frame["cols"], frame["rows"]), (last["cols"], last["rows"]))
        self.assertEqual(last["cols"], 100)
        self.assertEqual(self.sessions()[name]["runtime_state"], "live")
        # A client returns when its session ends.
        third = self.client(name, "--existing", "--run-id", row["run_id"])
        self.attached(name, 1)
        self.hgs("kill", name)
        third.wait()
        self.wait_for(lambda: not self.alive(pid, agent["process_start"]), "the agent process to end")


if __name__ == "__main__":
    unittest.main()
