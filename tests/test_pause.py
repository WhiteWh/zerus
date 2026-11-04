#!/usr/bin/env python3
"""End-to-end lifecycle tests: private tmux socket, fake agents, no model calls."""
import hashlib
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import pty
import re
import select
import selectors
import shutil
import shlex
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock
import uuid

REPO = Path(__file__).resolve().parents[1]
# The bounds of the report a missed stop deadline adds to its failure.
STOP_REPORT_SECONDS = 20
STOP_REPORT_CHARS = 16000
STOP_REPORT_PANES, STOP_REPORT_PROCESSES, STOP_REPORT_AGENTS = 8, 12, 4
STOP_REPORT_NATIVE, STOP_REPORT_THREADS = 6, 8
STOP_REPORT_COMMAND_BYTES, STOP_REPORT_TABLE_BYTES = 256 * 1024, 1024 * 1024


def fake_agent_interpreters(bin, path):
    """The executables a fake agent's `#!/usr/bin/env python3` can run: the
    first python3 on the agent's PATH, which hgs starts with HOME/.local/bin,
    /opt/homebrew/bin and /usr/local/bin (prepare_environment), and the app
    stub that a macOS framework build executes in its place."""
    for directory in [str(bin), "/opt/homebrew/bin", "/usr/local/bin", *path.split(os.pathsep)]:
        candidate = os.path.join(directory, "python3")
        if directory and os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            real = os.path.realpath(candidate)
            found = {real}
            if "/Python.framework/Versions/" in real and "/bin/" in real:
                stub = real.rsplit("/bin/", 1)[0] + "/Resources/Python.app/Contents/MacOS/Python"
                if os.path.isfile(stub):
                    found.add(os.path.realpath(stub))
            return found
    return set()
HGS = Path(os.environ.get("HGS_TEST_BIN", REPO / "hgs")).resolve()
# Set only when verifying the archived Bash/Python implementation before migration.
LEGACY_STATE = os.environ.get("HGS_TEST_LEGACY_STATE")
LEGACY_BIN = os.environ.get("HGS_TEST_LEGACY_BIN")
TMUX = shutil.which("tmux")

AGENT = r'''#!/usr/bin/env python3
import json, os, pathlib, shlex, signal, subprocess, sys, time, tomllib, uuid
home = pathlib.Path.home()
agent = pathlib.Path(sys.argv[0]).name
if sys.argv[1:] == ['--help']:
    print('--no-daemon' if not (home / 'old-codex').exists() else 'Legacy Codex help')
    sys.exit(0)
if sys.argv[1:] == ['--version']:
    # hgs reads Claude Code's version before it registers PostModelSwitch.
    with open(home / 'version-reads', 'a') as f:
        f.write(agent + '\n')
    version = home / 'claude-version'
    print(version.read_text().strip() if version.exists() else '2.1.296 (Claude Code)')
    sys.exit(0)
if agent == "kimi" and sys.argv[1:3] == ["session", "list"]:
    print(json.dumps([{"id": p.stem} for p in (home / "history").glob("*.jsonl")]))
    sys.exit(0)
with open(home / "argv.jsonl", "a") as f:
    f.write(json.dumps({"agent": agent, "argv": sys.argv[1:], "codex_home": os.environ.get("CODEX_HOME")}) + "\n")
args = sys.argv[1:]
sid = str(uuid.uuid4())
for flag in ("--resume", "--session", "resume"):
    if flag in args and not (home / "ignore-resume").exists():
        sid = args[args.index(flag) + 1]
if agent == "kimi":
    hooks = tomllib.loads((home / ".kimi-code/config.toml").read_text())["hooks"]
else:
    var, folder, filename = ("CLAUDE_CONFIG_DIR", ".claude", "settings.json") if agent == "claude" else ("CODEX_HOME", ".codex", "hooks.json")
    path = pathlib.Path(os.environ.get(var) or home / folder) / filename
    hooks = json.loads(path.read_text())["hooks"]
    version = home / 'claude-version'
    if agent == "claude" and "PostModelSwitch" in hooks and version.exists() and \
            tuple(map(int, version.read_text().split()[0].split('.'))) < (2, 1, 296):
        # Older Claude Code stops its start at a Settings Warning for the unknown event.
        (home / 'settings-warning').touch()
def event(kind, **fields):
    path = home / "history" / (sid + ".jsonl")
    path.parent.mkdir(exist_ok=True)
    path.write_text(json.dumps({"session_id": sid}) + "\n")
    payload = {"session_id": sid, "cwd": os.getcwd(), "transcript_path": str(path),
               "hook_event_name": kind, "source": "startup"}
    payload.update(fields)
    if "--no-hook" in args:
        return
    if agent == "kimi":
        commands = [h["command"] for h in hooks if h["event"] == kind]
    else:
        commands = [h["command"] for g in hooks.get(kind, []) for h in g["hooks"]]
    for command in commands:
        p = subprocess.run(command, shell=True, input=json.dumps(payload), text=True)
        if p.returncode == 2:
            return
def stop(*args):
    if (home / "ignore-term").exists():
        return
    delay = home / ("delay-term-" + sid)
    if delay.exists():
        (home / ("waiting-term-" + sid)).touch()
        while delay.exists():
            time.sleep(0.01)
    event("SessionEnd", reason="exit" if agent == "kimi" else "other")
    sys.exit(143)
signal.signal(signal.SIGTERM, stop)
# Only stop_report sends this: the Python stacks go to the pane's terminal.
import faulthandler
faulthandler.register(signal.SIGUSR1, all_threads=True)
if (home / "record-interrupts").exists():
    def interrupted(*args):
        with open(home / ("interrupts-" + sid), "a") as log:
            log.write("SIGINT\n")
    signal.signal(signal.SIGINT, interrupted)
event("SessionStart")
for line in sys.stdin:
    if line.strip() == "switch":
        sid = str(uuid.uuid4())
        event("SessionStart")
    elif line.strip() == "busy":
        event("UserPromptSubmit")
    elif line.strip() == "idle":
        event("Stop")
    elif line.strip() == "end":
        event("SessionEnd")
    elif line.strip() in ("exit", "/exit", "/quit", "crash-after-end"):
        event("SessionEnd", reason={"claude": "prompt_input_exit", "codex": "other", "kimi": "exit"}[agent])
        sys.exit(7 if line.strip() == "crash-after-end" else 0)
    elif line.strip() == "unknown-exit":
        break
    elif line.strip() == "crash":
        os._exit(7)
    elif line.strip() == "quiet-exit":
        sys.exit(0)
event("SessionEnd")
'''


def direct_signal_agent():
    """Opt-in diagnostics for Archive's three Linux direct-signal subcases only.

    Keep AGENT itself unchanged: all other cases still write its exact bytes.
    A failed diagnostic append must never change the fake's signal behavior.
    """
    trace = r'''
def signal_phase(phase):
    try:
        if agent not in ("claude", "codex", "kimi"):
            return
        row = {"agent": agent, "phase": phase, "pid": os.getpid(), "run": os.environ.get("HGS_RUN_ID", "")[:128],
               "conversation": sid[:128], "at_ns": time.monotonic_ns()}
        data = (json.dumps(row, separators=(",", ":")) + "\n").encode()
        fd = os.open(home / ("direct-signal-agent-" + agent + ".jsonl"),
                     os.O_WRONLY | os.O_CREAT | os.O_APPEND | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)
        try:
            if os.fstat(fd).st_size <= 8192 and len(data) <= 512:
                os.write(fd, data)  # A short write remains an explicitly incomplete diagnostic.
        finally:
            os.close(fd)
    except Exception:
        pass
'''
    replacements = (
        ('def stop(*args):', trace + '\ndef stop(*args):\n    signal_phase("handler_enter")'),
        ('    event("SessionEnd", reason="exit" if agent == "kimi" else "other")',
         '    signal_phase("SessionEnd_begin")\n'
         '    event("SessionEnd", reason="exit" if agent == "kimi" else "other")\n'
         '    signal_phase("SessionEnd_end")'),
        ('    sys.exit(143)', '    signal_phase("handler_exit_143")\n    sys.exit(143)'),
        ('signal.signal(signal.SIGTERM, stop)',
         'signal.signal(signal.SIGTERM, stop)\nsignal_phase("handler_registered")'),
        ('event("SessionStart")\nfor line in sys.stdin:',
         'signal_phase("SessionStart_begin")\nevent("SessionStart")\n'
         'signal_phase("SessionStart_end")\nfor line in sys.stdin:'),
    )
    source = AGENT
    for before, after in replacements:
        if source.count(before) != 1:
            raise AssertionError("direct signal diagnostic source no longer matches AGENT")
        source = source.replace(before, after, 1)
    return source


class DirectSignalTrace:
    """Content-free samples for one already-selected synthetic child; no signalling.

    This never gates success or adopts a replacement PID. Linux kernel metadata
    is sampled only for the exact target and supervisor from the captured binding.
    Every read is bounded; unknown evidence stays unknown, not a readiness proof.
    """
    def __init__(self, root, agent):
        if agent not in ("claude", "codex", "kimi"):
            raise ValueError("unexpected direct-signal agent")
        self.root, self.agent, self.samples = root, agent, []

    @staticmethod
    def process(pid, expected=None):
        if type(pid) is not int or pid <= 0:
            return {"unknown": "invalid PID"}
        try:
            def identity():
                with open(f"/proc/{pid}/stat", "rb") as file:
                    data = file.read(4097)
                if len(data) > 4096:
                    raise ValueError("oversized stat")
                fields = data.decode().rsplit(")", 1)[1].split()
                return {"pid": pid, "ppid": int(fields[1]), "birth": int(fields[19])}
            before = identity()
            if expected is not None and before != expected:
                return {"unknown": "identity changed"}
            with open(f"/proc/{pid}/status", "rb") as file:
                status = file.read(8193)
            if len(status) > 8192:
                raise ValueError("oversized status")
            masks = {}
            for line in status.decode().splitlines():
                key, _, value = line.partition(":")
                if key in ("SigBlk", "SigIgn", "SigCgt", "SigPnd", "ShdPnd") and re.fullmatch(r"[0-9a-fA-F]{1,32}", value.strip()):
                    masks[key] = value.strip()
            if identity() != before:
                return {"unknown": "identity changed during sample"}
            return {"identity": before, "masks": masks}
        except Exception as error:
            return {"unknown": type(error).__name__}

    def capture(self, phase, record, before=None):
        # Capture once, then signal that same numeric target exactly as before.
        # A different later binding is evidence, never permission to resend.
        def field(value):
            if value is None or type(value) is int and -(2 ** 63) <= value < 2 ** 63:
                return value
            if isinstance(value, str) and len(value) <= 128 and not any(ord(char) < 32 for char in value):
                return value
            return {"unknown": "invalid or oversized binding field"}

        def process(pid, kind):
            expected = (before or {}).get(kind, {}).get("identity")
            if before is not None and expected is None:
                return {"unknown": "no pre-signal identity"}
            return self.process(pid, expected)

        try:
            supervisor = record.get("supervisor") or {}
            # Sample the target last, immediately before returning to os.kill.
            parent = process(supervisor.get("pid"), "supervisor")
            child = process(record.get("pid"), "child")
            sample = {"agent": self.agent, "phase": phase, "at_ns": time.monotonic_ns(), "run": field(record.get("run_id")),
                      "conversation": field(record.get("conversation_id")), "pane": field(record.get("pane")),
                      "saved_pid": field(record.get("pid")), "saved_start": field(record.get("process_start")),
                      "saved_supervisor": {key: field(supervisor.get(key)) for key in ("pid", "start")},
                      "child": child, "supervisor": parent}
        except Exception as error:
            sample = {"agent": self.agent, "phase": phase, "unknown": type(error).__name__}
        self.samples.append(sample)
        return sample

    def failure(self):
        """Retain before the usual fixture cleanup, without replacing its failure."""
        try:
            raw, trace_error = b"", None
            try:
                with open(self.root / ("direct-signal-agent-" + self.agent + ".jsonl"), "rb") as file:
                    raw = file.read(8705)
            except Exception as error:
                trace_error = type(error).__name__
            phases, malformed = [], 0
            lines = raw[:8704].split(b"\n")
            for line in lines[:32]:
                if not line:
                    continue
                try:
                    row = json.loads(line)
                    if (len(line) > 512 or not isinstance(row, dict)
                            or set(row) != {"agent", "phase", "pid", "run", "conversation", "at_ns"}
                            or row["agent"] != self.agent
                            or row["phase"] not in ("handler_enter", "SessionEnd_begin", "SessionEnd_end",
                                                     "handler_exit_143", "handler_registered",
                                                     "SessionStart_begin", "SessionStart_end")
                            or type(row["pid"]) is not int or row["pid"] <= 0
                            or type(row["at_ns"]) is not int or row["at_ns"] < 0
                            or any(not isinstance(row[key], str) or len(row[key]) > 128
                                   or any(ord(char) < 32 for char in row[key]) for key in ("run", "conversation"))):
                        raise ValueError("unexpected phase fields")
                    phases.append(row)
                except (ValueError, TypeError):
                    malformed += 1
            data = {"schema": 1, "agent": self.agent, "samples": self.samples, "agent_phases": phases,
                    "agent_trace_sha256": hashlib.sha256(raw).hexdigest(),
                    "agent_trace_error": trace_error, "rows_omitted": max(0, len(lines) - 32),
                    "malformed_or_incomplete_rows": malformed, "trace_truncated": len(raw) > 8704}
            encoded = (json.dumps(data, sort_keys=True) + "\n").encode()
            if len(encoded) > 24576:
                raise ValueError("oversized signal diagnostic")
            artifact, durable = None, "unknown"
            try:
                folder = REPO / "artifacts/test-results/python"
                folder.mkdir(parents=True, exist_ok=True)
                with tempfile.NamedTemporaryFile(prefix="direct-signal-", suffix=".json", dir=folder, delete=False) as file:
                    artifact = Path(file.name).relative_to(REPO)
                    file.write(encoded)
                    file.flush()
                    os.fsync(file.fileno())
                fd = os.open(folder, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
                try:
                    os.fsync(fd)
                finally:
                    os.close(fd)
                durable = "file and parent synced"
            except Exception as error:
                durable = "unknown: " + type(error).__name__
            return (f"Direct signal diagnostics: artifact={artifact}, persistence={durable}\n"
                    + encoded.decode()[:8000])
        except Exception as error:
            return "Direct signal diagnostics unavailable: " + type(error).__name__


# BEGIN private Mac pause fixture observer.
MAC_PAUSE_CASE = "test_pause.Lifecycle.test_claude_status_line_is_not_registered_where_claude_would_differ"
MAC_PAUSE_PHASES = ("handler_registered", "SessionStart_returned", "stdin_enter", "term_handler_enter",
                    "SessionEnd_begin", "SessionEnd_returned", "handler_exit143")
MAC_PAUSE_RECEIVER = r'''
# Diagnostic only: Python callback/self-mask observations, never kernel delivery proof.
import hashlib as _mp_hashlib, stat as _mp_stat
_mp_root, _mp_nonce, _mp_root_id = pathlib.Path(__MP_ROOT__), __MP_NONCE__, __MP_ROOT_ID__
_mp_file_id, _mp_seq, _mp_busy, _mp_off = None, 0, False, False
_mp_run = _mp_hashlib.sha256(os.environ.get("HGS_RUN_ID", "").encode()).hexdigest()
_mp_conversation = _mp_hashlib.sha256(sid.encode()).hexdigest()
def mac_pause_phase(phase):
    global _mp_file_id, _mp_seq, _mp_busy, _mp_off
    if agent != "claude" or os.environ.get("HGS_SESSION") != "claude/p/own" or _mp_off:
        return
    if _mp_busy:
        _mp_off = True  # Reentrant telemetry is unknown; the original handler still runs.
        return
    _mp_busy = True
    directory, fd = None, None
    try:
        if _mp_seq >= 7:
            raise ValueError("phase bound")
        try:
            at = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
            if type(at) is not int or not 0 <= at < 2 ** 64:
                at = None
        except Exception:
            at = None
        handler = signal.getsignal(signal.SIGTERM)
        disposition = ("stop_callback" if handler is stop else "ignore" if handler == signal.SIG_IGN
                       else "default" if handler == signal.SIG_DFL else "other_callback" if callable(handler) else "unknown")
        try:
            blocked = signal.SIGTERM in signal.pthread_sigmask(signal.SIG_BLOCK, [])
        except Exception:
            blocked = None
        try:
            pending = signal.SIGTERM in signal.sigpending()
        except Exception:
            pending = None
        # Marker existence is fixture state, not a reason to change the stop.
        row = {"v": 1, "role": "receiver", "seq": _mp_seq + 1, "phase": phase, "nonce": _mp_nonce,
               "pid": os.getpid(), "ppid": os.getppid(), "clock": "CLOCK_MONOTONIC", "at": at,
               "run": _mp_run, "conversation": _mp_conversation, "handler": disposition,
               "term_blocked": blocked, "term_pending": pending,
               "ignore_marker": (home / "ignore-term").exists(),
               "delay_marker": (home / ("delay-term-" + sid)).exists()}
        data = (json.dumps(row, separators=(",", ":")) + "\n").encode()
        if len(data) > 768:
            raise ValueError("row bound")
        directory = os.open(_mp_root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
        meta = os.fstat(directory)
        if ((meta.st_dev, meta.st_ino) != _mp_root_id or meta.st_uid != os.geteuid()
                or not _mp_stat.S_ISDIR(meta.st_mode) or _mp_stat.S_IMODE(meta.st_mode) != 0o700):
            raise ValueError("root identity")
        flags = os.O_WRONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK
        flags |= os.O_CREAT | os.O_EXCL if _mp_file_id is None else os.O_APPEND
        fd = os.open("receiver.jsonl", flags, 0o600, dir_fd=directory)
        meta = os.fstat(fd)
        identity = (meta.st_dev, meta.st_ino)
        if (not _mp_stat.S_ISREG(meta.st_mode) or meta.st_uid != os.geteuid()
                or _mp_stat.S_IMODE(meta.st_mode) != 0o600 or meta.st_nlink != 1
                or (_mp_file_id is not None and identity != _mp_file_id)
                or meta.st_size > 7 * 768 - len(data)):
            raise ValueError("file identity or bound")
        _mp_file_id = identity  # Only the original own run wins the exclusive creator.
        if os.write(fd, data) != len(data):
            raise ValueError("partial diagnostic write")
        _mp_seq += 1
    except Exception:
        _mp_off = True
    finally:
        for owned in (fd, directory):
            if owned is not None:
                try:
                    os.close(owned)
                except Exception:
                    _mp_off = True
        _mp_busy = False
'''


class MacPauseProbe:
    """Two private bounded role files for one synthetic Mac pause, never a gate.

    The sender's pre-kill kernel birth remains UNKNOWN. Python's cached handler,
    current thread's mask and self-pending sample do not describe other threads
    or the receiver during a stall. SessionEnd_returned is the fake function's
    return, not a receipt that its hook subprocess succeeded or ended.
    """
    def __init__(self, root):
        self.root = (root / ".pause-signal").resolve()
        self.root.mkdir(mode=0o700)
        meta = self.root.stat()
        self.root_id, self.nonce = (meta.st_dev, meta.st_ino), uuid.uuid4().hex
        self.context, self.unknown = None, None
        try:
            self.created_at = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
        except Exception:
            self.created_at = None

    @staticmethod
    def selected(platform, case, legacy):
        return platform == "darwin" and case == MAC_PAUSE_CASE and not legacy

    def agent(self):
        trace = MAC_PAUSE_RECEIVER.replace("__MP_ROOT__", repr(str(self.root)))
        trace = trace.replace("__MP_NONCE__", repr(self.nonce)).replace("__MP_ROOT_ID__", repr(self.root_id))
        replacements = (
            ('def stop(*args):', trace + '\ndef stop(*args):\n    mac_pause_phase("term_handler_enter")'),
            ('    event("SessionEnd", reason="exit" if agent == "kimi" else "other")',
             '    mac_pause_phase("SessionEnd_begin")\n'
             '    event("SessionEnd", reason="exit" if agent == "kimi" else "other")\n'
             '    mac_pause_phase("SessionEnd_returned")'),
            ('    sys.exit(143)', '    mac_pause_phase("handler_exit143")\n    sys.exit(143)'),
            ('signal.signal(signal.SIGTERM, stop)',
             'signal.signal(signal.SIGTERM, stop)\nmac_pause_phase("handler_registered")'),
            ('event("SessionStart")\nfor line in sys.stdin:',
             'event("SessionStart")\nmac_pause_phase("SessionStart_returned")\n'
             'mac_pause_phase("stdin_enter")\nfor line in sys.stdin:'),
        )
        source = AGENT
        for before, after in replacements:
            if source.count(before) != 1:
                raise ValueError("Mac pause diagnostic source no longer matches AGENT")
            source = source.replace(before, after, 1)
        return source

    def environment(self, args, env, binding):
        if args != ("pause", "claude/p/own"):
            return env
        try:
            if self.context is not None:
                raise ValueError("diagnostic already reserved")
            record = binding("claude/p/own")
            if record.get("name") != "claude/p/own" or type(record.get("pid")) is not int or record["pid"] <= 0:
                raise ValueError("missing fixture identity")
            values = {}
            for key, source in (("run", "run_id"), ("conversation", "conversation_id"),
                                ("pane", "pane"), ("saved_start", "process_start")):
                value = record.get(source)
                if not isinstance(value, str) or not value or len(value) > 256 or any(ord(c) < 32 for c in value):
                    raise ValueError("unknown fixture identity")
                values[key] = hashlib.sha256(value.encode()).hexdigest()
            context = dict(version=1, nonce=self.nonce, target_pid=record["pid"],
                           root_dev=self.root_id[0], root_ino=self.root_id[1], **values)
            data = (json.dumps(context, separators=(",", ":")) + "\n").encode()
            directory = self.directory()
            try:
                fd = os.open("manifest.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
                             | os.O_NONBLOCK | os.O_CLOEXEC, 0o600, dir_fd=directory)
                try:
                    if len(data) > 1024 or os.write(fd, data) != len(data):
                        raise ValueError("incomplete context")
                finally:
                    os.close(fd)
            finally:
                os.close(directory)
            self.context = context
            return dict(env, HGS_TEST_MAC_PAUSE_OBSERVER="1", HGS_TEST_MAC_PAUSE_ROOT=str(self.root))
        except Exception as error:
            self.unknown = type(error).__name__
            return env  # Diagnostics never suppress or retry the original pause.

    def directory(self):
        import stat
        fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NONBLOCK | os.O_NOFOLLOW)
        try:
            meta = os.fstat(fd)
            if ((meta.st_dev, meta.st_ino) != self.root_id or meta.st_uid != os.geteuid()
                    or not stat.S_ISDIR(meta.st_mode) or stat.S_IMODE(meta.st_mode) != 0o700):
                raise ValueError("changed private root")
            return fd
        except BaseException:
            os.close(fd)
            raise

    @staticmethod
    def decode(raw, role, context, lower, upper):
        """A malformed/partial/stale stream stays unknown; never infer a signal."""
        maximum = 2 if role == "sender" else 7 if role == "receiver" else 0
        if not maximum or not raw or len(raw) > maximum * 768 or not raw.endswith(b"\n"):
            raise ValueError("missing or bounded stream")
        lines, rows, phases = raw.splitlines(keepends=True), [], set()
        if len(lines) > maximum:
            raise ValueError("row count")
        common = {"v", "role", "seq", "phase", "nonce", "pid", "clock", "at", "run", "conversation"}
        extra = ({"target", "pane", "saved_start", "birth_before", "sender_birth_after", "target_birth_after", "rc", "errno"}
                 if role == "sender" else {"ppid", "handler", "term_blocked", "term_pending", "ignore_marker", "delay_marker"})
        def unique(pairs):
            result = {}
            for key, value in pairs:
                if key in result:
                    raise ValueError("duplicate diagnostic field")
                result[key] = value
            return result

        for number, line in enumerate(lines, 1):
            row = json.loads(line, object_pairs_hook=unique)
            if (len(line) > 768 or not isinstance(row, dict) or set(row) != common | extra
                    or type(row["v"]) is not int or row["v"] != 1 or row["role"] != role
                    or type(row["seq"]) is not int or row["seq"] != number
                    or row["nonce"] != context["nonce"] or row["clock"] != "CLOCK_MONOTONIC"
                    or type(row["pid"]) is not int or not 0 < row["pid"] < 2 ** 31
                    or row["run"] != context["run"] or row["conversation"] != context["conversation"]
                    or row["at"] is not None and (type(row["at"]) is not int or not 0 <= row["at"] < 2 ** 64
                                                  or type(lower) is not int or type(upper) is not int
                                                  or not lower <= row["at"] <= upper)):
                raise ValueError("unexpected diagnostic fields")
            if role == "sender":
                if (row["phase"] != ("kill_before", "kill_return_or_errno")[number - 1]
                        or type(row["target"]) is not int or row["target"] != context["target_pid"]
                        or row["pane"] != context["pane"]
                        or row["saved_start"] != context["saved_start"] or row["birth_before"] != "unknown"
                        or any(value is not None and (not isinstance(value, str) or not re.fullmatch(r"[0-9]{1,20}:[0-9]{1,6}", value))
                               for value in (row["sender_birth_after"], row["target_birth_after"]))
                        or number == 1 and (row["rc"] is not None or row["errno"] is not None)
                        or number == 2 and (type(row["rc"]) is not int or row["rc"] not in (0, -1)
                                             or row["rc"] == 0 and row["errno"] is not None
                                             or row["rc"] != 0 and (type(row["errno"]) is not int or row["errno"] <= 0))):
                    raise ValueError("unknown sender identity or result")
            elif (row["phase"] not in MAC_PAUSE_PHASES or row["phase"] in phases
                    or row["pid"] != context["target_pid"] or type(row["ppid"]) is not int or row["ppid"] <= 0
                    or row["handler"] not in ("stop_callback", "ignore", "default", "other_callback", "unknown")
                    or any(row[key] is not None and type(row[key]) is not bool for key in ("term_blocked", "term_pending"))
                    or any(type(row[key]) is not bool for key in ("ignore_marker", "delay_marker"))):
                raise ValueError("unknown receiver identity or phase")
            if rows and row["at"] is not None and rows[-1]["at"] is not None and row["at"] < rows[-1]["at"]:
                raise ValueError("nonmonotonic role clock")
            phases.add(row["phase"])
            rows.append(row)
        if role == "sender" and (len(rows) != 2 or rows[0]["pid"] != rows[1]["pid"]):
            raise ValueError("incomplete sender")
        return rows

    def failure(self, deadline):
        try:
            if self.context is None:
                raise ValueError("missing reserved context")
            rows, errors = [], {}
            directory = self.directory()
            try:
                import stat
                for role, limit in (("sender", 1536), ("receiver", 5376)):
                    if time.monotonic() >= deadline:
                        raise TimeoutError("existing report deadline")
                    try:
                        fd = os.open(role + ".jsonl", os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC | os.O_NOFOLLOW, dir_fd=directory)
                        try:
                            before = os.fstat(fd)
                            if (not stat.S_ISREG(before.st_mode) or before.st_uid != os.geteuid()
                                    or stat.S_IMODE(before.st_mode) != 0o600 or before.st_nlink != 1 or before.st_size > limit):
                                raise ValueError("nonprivate role file")
                            raw = os.read(fd, limit + 1)
                            after = os.fstat(fd)
                            if ((before.st_dev, before.st_ino, before.st_size) != (after.st_dev, after.st_ino, after.st_size)
                                    or len(raw) != before.st_size or time.monotonic() >= deadline):
                                raise ValueError("changed, partial or late role read")
                            try:
                                upper = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
                            except Exception:
                                upper = None
                            rows.extend(self.decode(raw, role, self.context, self.created_at, upper))
                        finally:
                            os.close(fd)
                    except Exception as error:
                        errors[role] = type(error).__name__
            finally:
                os.close(directory)
            data = {"schema": 1, "observer": "private_mac_pause", "rows": rows, "unknown": errors,
                    "kernel_birth_before_kill": "unknown", "during_stall_thread_state": "unknown",
                    "clock_domain": "same-host CLOCK_MONOTONIC; null timestamps are unknown",
                    "python_state": "cached callback and calling-thread mask/pending only",
                    "session_end_return": "fake function return, not hook success or hook PID"}
            encoded = (json.dumps(data, separators=(",", ":")) + "\n").encode()
            if len(rows) > 9 or len(encoded) > 8192 or time.monotonic() >= deadline:
                raise ValueError("late or oversized diagnostic")
            folder = REPO / "artifacts/test-results/python"
            folder.mkdir(parents=True, exist_ok=True)
            with tempfile.NamedTemporaryFile(prefix="mac-pause-", suffix=".json", dir=folder, delete=False) as file:
                if file.write(encoded) != len(encoded):
                    raise ValueError("partial diagnostic artifact")
                file.flush()
                os.fsync(file.fileno())  # Failure collector only; never the handler.
            if time.monotonic() >= deadline:
                raise TimeoutError("late diagnostic artifact")
            return "Mac pause diagnostic artifact=" + str(Path(file.name).relative_to(REPO)) + "\n" + encoded.decode()
        except Exception as error:
            return "Mac pause diagnostic UNKNOWN: " + type(error).__name__
# END private Mac pause fixture observer.


@unittest.skipUnless(TMUX, "tmux is required")
class Harness(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="hgs-pause-"))
        self.bin = self.root / ".local/bin"
        self.bin.mkdir(parents=True)
        self.project = self.root / "project with spaces"
        self.project.mkdir()
        self.socket = str(self.root / "tmux.sock")
        # Tests launched inside a native agent inherit its session and account
        # context. A managed account would make every fake relaunch fail.
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("HGS_")}
        self.env.update(HOME=str(self.root), HGS_CONFIG_DIR=str(self.root / "cfg"),
                        HGS_STATE_DIR=str(self.root / "state"), HGS_SELF="test", HGS_PEERS="",
                        HGS_TAB="0", HGS_TRACKING="1", SHELL=str(self.bin / "shell"),
                        PATH=str(self.bin) + ":" + os.environ["PATH"])
        for k in ("TMUX", "TMUX_PANE", "CLAUDE_CONFIG_DIR", "CODEX_HOME", "KIMI_CODE_HOME"):
            self.env.pop(k, None)
        # The exact selected Darwin fake alone gets passive signal diagnostics.
        self.mac_pause_probe = None
        if MacPauseProbe.selected(sys.platform, self.id(), LEGACY_STATE):
            try:
                self.mac_pause_probe = MacPauseProbe(self.root)
            except Exception:
                pass  # Unavailable telemetry must not change the original fixture.
        for name in ("claude", "codex", "kimi"):
            source = AGENT
            if name == "claude" and self.mac_pause_probe is not None:
                try:
                    source = self.mac_pause_probe.agent()
                except Exception:
                    self.mac_pause_probe = None
            self.script(name, source)
        # Fake agents need no login. Never read or unlock the real macOS keychain.
        self.script("security", "#!/bin/sh\nexit 0\n")
        self.script("shell", '#!/bin/sh\nshift\nexec /bin/bash -c "$@"\n')
        self.script("tmux", '#!/bin/sh\nexec ' + TMUX + ' -S "' + self.socket + '" -f /dev/null "$@"\n')
        (self.root / "cfg").mkdir()
        (self.root / "cfg/projects.local").write_text("p=" + str(self.project) + "\n")

    def tearDown(self):
        def process_table():
            result = subprocess.run(["ps", "-axo", "pid=,ppid=,stat=,lstart="],
                                    env=dict(self.env, LC_ALL="C", TZ="UTC"),
                                    capture_output=True, text=True, check=True)
            table = {}
            for line in result.stdout.splitlines():
                fields = line.split(None, 3)
                if len(fields) == 4:
                    pid, parent, status, started = fields
                    table[int(pid)] = (int(parent), status, " ".join(started.split()))
            return table

        # kill-server returns before the supervised agents have drained and
        # synced their final records. Identify only this private socket's pane
        # processes and recorded processes whose start identity still matches.
        panes = subprocess.run([TMUX, "-S", self.socket, "list-panes", "-a", "-F", "#{pane_pid}"],
                               env=self.env, capture_output=True, text=True)
        table = process_table()
        owned = {int(pid): table[int(pid)][2] for pid in panes.stdout.split()
                 if pid.isdigit() and int(pid) in table}
        for path in (self.root / "state").rglob("*.json"):
            try:
                record = json.loads(path.read_text())
            except (FileNotFoundError, json.JSONDecodeError):
                continue  # A supervisor can atomically archive a record here.
            if not isinstance(record, dict):
                continue
            identities = [(record.get("pid"), record.get("process_start"))]
            supervisor = record.get("supervisor")
            if isinstance(supervisor, dict):
                identities.append((supervisor.get("pid"), supervisor.get("start")))
            for pid, started in identities:
                if isinstance(pid, int) and isinstance(started, str) and started and \
                        pid in table and table[pid][2] == " ".join(started.split()):
                    owned[pid] = table[pid][2]

        def live_owned(table):
            # Include hook subprocesses before their parents disappear. Recheck
            # start identities every poll so PID reuse never waits on another run.
            while True:
                children = {pid: data[2] for pid, data in table.items()
                            if pid not in owned and data[0] in owned and
                            data[0] in table and table[data[0]][2] == owned[data[0]]}
                if not children:
                    break
                owned.update(children)
            return {pid: table[pid][1] for pid, started in owned.items()
                    if pid in table and table[pid][2] == started and not table[pid][1].startswith(("Z", "X"))}

        live_owned(table)
        subprocess.run([TMUX, "-S", self.socket, "kill-server"], env=self.env, capture_output=True)
        deadline = time.monotonic() + 6
        while remaining := live_owned(process_table()):
            if time.monotonic() >= deadline:
                self.fail(f"private tmux processes did not exit: {remaining}; preserved fixtures: {self.root}")
            time.sleep(0.02)
        self.remove_fixture_root()

    def remove_fixture_root(self):
        shutil.rmtree(self.root)

    def script(self, name, text):
        path = self.bin / name
        path.write_text(text)
        path.chmod(0o755)

    def hgs(self, *args, rc=0, cwd=None):
        probe = getattr(self, "mac_pause_probe", None)
        env = probe.environment(args, self.env, self.binding) if probe is not None else self.env
        p = subprocess.run([str(HGS), *args], env=env,
                           capture_output=True, text=True, timeout=20, cwd=cwd)
        stalled = p.returncode != rc and args[:1] == ("pause",) and "did not exit after SIGTERM" in p.stderr
        self.assertEqual(p.returncode, rc, p.stdout + p.stderr + (self.stop_report() if stalled else ""))
        return p.stdout

    def stop_report(self):
        """What still runs once a stop has missed its deadline, and where: the
        saved stop evidence, each process of the private panes, the fake
        agents' Python stacks, the host's load and, natively, what each
        process waits in.

        Collection is bounded: each command has a timeout, the whole report a
        deadline and a size, and a step that fails says so instead of
        replacing the failure the report explains. Only exact fake agents in
        the private panes, whose start identity still matches, are signalled."""
        deadline = time.monotonic() + STOP_REPORT_SECONDS
        lines = ["", "Stop diagnostics:"]
        state = {"panes": [], "rows": [], "owned": [], "signalled": False}

        def run(argv, limit, maximum=STOP_REPORT_COMMAND_BYTES, **options):
            # At most `maximum` bytes are read before the command's limit or the
            # report deadline; then the command is stopped and reaped.
            ends = min(deadline, time.monotonic() + limit)
            if ends <= time.monotonic():
                raise TimeoutError("report deadline reached")
            process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                       stderr=subprocess.DEVNULL, **options)
            captured, ended = bytearray(), False
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(process.stdout, selectors.EVENT_READ)
                    while len(captured) < maximum:
                        remaining = ends - time.monotonic()
                        if remaining <= 0:
                            raise subprocess.TimeoutExpired(argv, limit)
                        if selector.select(remaining):
                            chunk = os.read(process.stdout.fileno(), min(65536, maximum - len(captured)))
                            if not chunk:
                                ended = True
                                break
                            captured += chunk
                if ended:
                    process.wait(timeout=max(0.01, ends - time.monotonic()))
            finally:
                if process.poll() is None:
                    process.kill()
                    try:
                        process.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        lines.append(f"unavailable: cleanup: {argv[0]} {process.pid} did not stop")
                process.stdout.close()
            text = captured.decode(errors="replace")
            return text if ended else text + "\n[output truncated]"

        def step(name, action):
            try:
                action()
            except Exception as error:  # A diagnostic step never replaces the failure.
                lines.append(f"unavailable: {name}: {type(error).__name__}: {str(error)[:200]}")

        def bounded(path, limit):
            with open(path, "rb") as file:
                return file.read(limit).decode(errors="replace")

        def panes():
            output = run([str(self.bin / "tmux"), "list-panes", "-a", "-F",
                          "#{session_name} #{pane_id} #{pane_pid} #{pane_dead}"], 3, env=self.env)
            found = [line.split() for line in output.splitlines() if len(line.split()) == 4]
            state["panes"] = found[:STOP_REPORT_PANES]
            lines.append("panes: " + "; ".join(" ".join(pane) for pane in state["panes"])
                         + (f" (+{len(found) - STOP_REPORT_PANES} more)" if len(found) > STOP_REPORT_PANES else ""))

        def records():
            for path in sorted((self.root / "state").glob("*.json"))[:3]:
                record = json.loads(bounded(path, 1 << 20))
                lines.append("record: " + json.dumps({key: record.get(key) for key in (
                    "name", "pid", "supervisor", "activity", "pausing", "session_end", "exited_at", "completion_reason")})[:600])

        def processes():
            output = run(["ps", "-axo", "pid=,ppid=,lstart=,stat=,etime=,command="], 5, STOP_REPORT_TABLE_BYTES,
                         env=dict(self.env, LC_ALL="C", TZ="UTC"))
            rows = {}
            for line in output.splitlines():
                fields = line.split(None, 9)
                if len(fields) == 10:
                    rows[fields[0]] = dict(pid=fields[0], ppid=fields[1], start=" ".join(fields[2:7]),
                                           stat=fields[7], etime=fields[8], command=fields[9])
            children = {}
            for row in rows.values():
                children.setdefault(row["ppid"], []).append(row["pid"])
            descendants, pending = [], [pane[2] for pane in state["panes"]]
            while pending:
                pid = pending.pop(0)
                if pid in rows and pid not in descendants:
                    descendants.append(pid)
                    pending += children.get(pid, [])
            listed = descendants + [pid for pid, row in rows.items() if str(self.root) in row["command"] and pid not in descendants]
            state["rows"], state["descendants"] = rows, set(descendants)
            state["owned"] = [rows[pid] for pid in listed[:STOP_REPORT_PROCESSES]]
            lines.extend(f"process: {row['pid']} {row['ppid']} {row['stat']} {row['etime']} {row['command'][:200]}" for row in state["owned"])
            if len(listed) > STOP_REPORT_PROCESSES:
                lines.append(f"process: +{len(listed) - STOP_REPORT_PROCESSES} more not listed")

        def verified(row):
            """Whether the PID and each ancestor up to its pane still name the
            snapshot's processes (parent, start, command) and the pane is live."""
            panes, chain = {pane[2] for pane in state["panes"]}, [row["pid"]]
            while chain[-1] not in panes:
                parent = state["rows"].get(chain[-1], {}).get("ppid")
                if parent not in state["rows"] or len(chain) > 8:
                    return False
                chain.append(parent)
            for pid in chain:
                current = run(["ps", "-p", pid, "-o", "ppid=,lstart=,command="], 3,
                              env=dict(self.env, LC_ALL="C", TZ="UTC")).split(None, 6)
                snapshot = state["rows"][pid]
                if len(current) != 7 or current[0] != snapshot["ppid"] or " ".join(current[1:6]) != snapshot["start"] \
                        or current[6].strip() != snapshot["command"]:
                    return False
            live = run([str(self.bin / "tmux"), "list-panes", "-a", "-F", "#{pane_pid} #{pane_dead}"], 3, env=self.env)
            return f"{chain[-1]} 0" in live.splitlines()

        def fake_script(row):
            _, _, rest = row["command"].partition(" ")
            scripts = [str(self.bin / name) for name in ("claude", "codex", "kimi")]
            return any(rest == script or rest.startswith(script + " ") for script in scripts)

        def fixture_interpreter(row):
            # The executable the process runs, from the system, not its argv.
            if sys.platform.startswith("linux"):
                executable = os.path.realpath(f"/proc/{row['pid']}/exe")
            else:
                executable = os.path.realpath(run(["ps", "-p", row["pid"], "-o", "comm="], 3).strip())
            return executable in fake_agent_interpreters(self.bin, self.env.get("PATH", ""))

        def signal_agents():
            agents = [row for row in state["owned"] if row["pid"] in state.get("descendants", ()) and fake_script(row)]
            for row in agents[:STOP_REPORT_AGENTS]:
                if not verified(row):
                    lines.append(f"python {row['pid']}: not signalled, its process identity or pane changed")
                elif not fixture_interpreter(row):
                    lines.append(f"python {row['pid']}: not signalled, it does not run the fixture's Python")
                else:
                    os.kill(int(row["pid"]), signal.SIGUSR1)
                    state["signalled"] = True

        def python_stacks():
            if not state["signalled"]:
                return
            time.sleep(max(0, min(0.3, deadline - time.monotonic())))
            for pane in state["panes"]:
                screen = run([str(self.bin / "tmux"), "capture-pane", "-p", "-J", "-S", "-200", "-t", pane[1]], 3,
                             65536, env=self.env).splitlines()
                starts = [i for i, text in enumerate(screen) if re.match(r"(Current thread|Thread) 0x", text)]
                if starts:
                    lines.append(f"python in {pane[1]}: " + " | ".join(text.strip() for text in screen[starts[0]:] if text.strip())[:1500])
                else:
                    lines.append(f"python in {pane[1]}: no stack on its terminal")

        def host():
            # How loaded the computer is as the native stacks are taken: a slow
            # sample there is the collector's limit, not a lifecycle fault.
            load = " ".join(f"{value:.2f}" for value in os.getloadavg())
            lines.append(f"host: load {load} (1, 5, 15 min) on {os.cpu_count()} CPUs")

        def native(row):
            if not verified(row):
                lines.append(f"native {row['pid']}: omitted, its process identity or pane changed")
                return
            if sys.platform != "darwin":
                lines.append(f"wchan {row['pid']}: " + bounded(f"/proc/{row['pid']}/wchan", 256).strip())
                return
            sampled = run(["sample", row["pid"], "1", "-mayDie", "-file", "/dev/stdout"], 5)
            graph = sampled.split("Call graph:", 1)[-1].split("Total number in stack", 1)[0].splitlines()
            threads = [i for i, text in enumerate(graph) if text.startswith("    ") and text[4:5] != " " and "Thread_" in text]
            for start, end in list(zip(threads, threads[1:] + [len(graph)]))[:STOP_REPORT_THREADS]:
                frames = [text.strip() for text in graph[start + 1:end] if text.strip()]
                lines.append(f"stack {row['pid']} {graph[start].strip()[:60]}: " + " <- ".join(frames[-1:-7:-1])[:600])
            if len(threads) > STOP_REPORT_THREADS:
                lines.append(f"stack {row['pid']}: +{len(threads) - STOP_REPORT_THREADS} threads omitted")

        try:
            probe = getattr(self, "mac_pause_probe", None)
            if probe is not None:
                step("Mac pause trace", lambda: lines.append(probe.failure(deadline)))
            step("panes", panes)
            step("records", records)
            step("processes", processes)
            step("signals", signal_agents)
            step("python stacks", python_stacks)
            step("host", host)
            # Only the private panes' own processes: a listed path match is never sampled.
            for row in [row for row in state["owned"] if row["pid"] in state.get("descendants", ())][:STOP_REPORT_NATIVE]:
                step(f"native stack {row['pid']}", lambda row=row: native(row))
        except Exception as error:  # Even a broken report keeps the original failure.
            lines.append(f"unavailable: report: {type(error).__name__}")
        report = "\n".join(lines)
        if len(report) > STOP_REPORT_CHARS:
            report = report[:STOP_REPORT_CHARS] + f"\n[stop report truncated at {STOP_REPORT_CHARS} characters]"
        return report

    def t(self, *args):
        return subprocess.run([str(self.bin / "tmux"), *args], env=self.env,
                              capture_output=True, text=True, check=True).stdout

    def binding(self, name):
        state = self.root / "state"
        # Reused pre-upgrade names can have a canonical binding in bindings/;
        # the old physical slot is reserved for the running legacy supervisor.
        for p in [*state.glob("*.json"), *(state / "bindings").glob("*.json")]:
            r = json.loads(p.read_text())
            if r.get("name") == name and not r.get("rename_shadow"):
                return r
        return {}

    def assertInspectionEqual(self, actual, expected):
        # Process sampling is a fresh observation, even for immutable archives.
        # Compare every durable field and job, allowing only its sampling clock.
        actual, expected = json.loads(json.dumps(actual)), json.loads(json.dumps(expected))
        for value in (actual, expected):
            for key in ("processes", "process_work"):
                if isinstance(value.get(key), dict):
                    sampled = value[key].pop("sampled_at", None)
                    self.assertIsInstance(sampled, (int, float))
        self.assertEqual(actual, expected)

    def wait(self, predicate):
        until = time.monotonic() + 6
        while time.monotonic() < until:
            if predicate():
                return
            time.sleep(0.05)
        self.fail("timeout; state=" + str(list((self.root / "state").glob("*"))) +
                  "\n" + subprocess.run([str(self.bin / "tmux"), "capture-pane", "-p"],
                    env=self.env, capture_output=True, text=True).stdout + self.stop_report())

    def start(self, agent="claude", tag="one", *extra):
        name = f"{agent}/p/{tag}"
        self.hgs(agent, "p", "-n", tag, "-d", *extra)
        self.wait(lambda: self.binding(name).get("conversation_id"))
        return name

    def resume(self, name):
        previous_run = self.binding(name)["run_id"]
        self.hgs("resume", name, "-d")
        # SessionEnd may preserve the last idle activity. Wait for the new run's
        # confirmed hook, not the old manifest's still-idle value.
        self.wait(lambda: self.binding(name).get("run_id") != previous_run and
                  self.binding(name).get("activity") == "idle" and
                  not self.binding(name).get("expected_id"))

    def send_event(self, name, kind, **fields):
        r = self.binding(name)
        event = dict(session_id=r["conversation_id"], hook_event_name=kind)
        event.update(fields)
        env = dict(self.env, HGS_SESSION=name, HGS_RUN_ID=r["run_id"])
        return self.hook(event, env)

    def hook(self, event, env):
        command = (["python3", LEGACY_STATE, "hook"] if LEGACY_STATE else
                   [str(HGS), "__state", "hook"])
        return subprocess.run(command, env=env,
                              input=json.dumps(event), text=True, capture_output=True, check=True)

    def saved_v1(self, agent="claude", base=None, tag="legacy"):
        """A pre-migration record, independent of the current writer's schema."""
        name, sid = f"{agent}/p/{tag}", str(uuid.uuid4())
        transcript = self.root / "history" / (sid + ".jsonl")
        transcript.parent.mkdir(exist_ok=True)
        transcript.write_text(json.dumps({"session_id": sid}) + "\n")
        record = {"version": 1, "name": name, "agent": agent, "run_id": "legacy-run",
                  "pane": "%123", "cwd": str(self.project), "launch_dir": str(self.project),
                  "base": base or [agent], "hgs": str(HGS), "shell": str(self.bin / "shell"),
                  "created": 1700000000, "updated": 1700000010, "activity": "unknown",
                  "phase": "ended", "conversation_id": sid, "transcript": str(transcript),
                  "agent_home": None, "pid": 999999999, "process_start": "old process",
                  "paused": True, "prompt": "Saved before migration", "last_message": "Ready"}
        state = self.root / "state"
        state.mkdir(exist_ok=True)
        path = state / (hashlib.sha256(name.encode()).hexdigest() + ".json")
        path.write_text(json.dumps(record) + "\n")
        return record, path


class Lifecycle(Harness):
    def test_directory_browser(self):
        folder = self.root / "browse ' test"
        folder.mkdir()
        for name in ("alpha", "with spaces", "кириллица", ".hidden", "$(touch should-not-run)"):
            (folder / name).mkdir()
        (folder / "file.txt").write_text("not a directory")
        (folder / "link").symlink_to(folder / "alpha")
        listing = json.loads(self.hgs("dirs", str(folder)))
        names = {d["name"] for d in listing["directories"]}
        self.assertEqual(names, {"alpha", "with spaces", "кириллица", "$(touch should-not-run)", "link"})
        self.assertEqual(listing["path"], str(folder.resolve()))
        self.assertEqual(listing["parent"], str(self.root.resolve()))
        self.assertIn(".hidden", {d["name"] for d in json.loads(self.hgs("dirs", "--hidden", str(folder)))["directories"]})
        self.assertEqual(json.loads(self.hgs("dirs", str(folder / "link")))["path"], str((folder / "alpha").resolve()))
        self.assertEqual(json.loads(self.hgs("dirs"))["path"], str(self.root.resolve()))
        self.hgs("dirs", str(folder / "missing"), rc=1)
        self.hgs("dirs", str(folder / "file.txt"), rc=1)
        self.hgs("dirs", "bad\npath", rc=1)

    def test_new_sessions_get_unique_names_and_never_attach(self):
        names = []
        for _ in range(2):
            output = self.hgs("claude", str(self.project), "--new", "-d")
            name = output.strip().removeprefix("hgs: started ")
            self.assertIn("/work-", name)
            self.wait(lambda: self.binding(name).get("conversation_id"))
            names.append(name)
        self.assertNotEqual(names[0], names[1])
        self.assertNotEqual(self.binding(names[0])["conversation_id"], self.binding(names[1])["conversation_id"])
        tag = names[0].rsplit("/", 1)[1]
        before = self.binding(names[0])["run_id"]
        self.hgs("claude", str(self.project), "--new", "-n", tag, "-d", rc=1)
        self.assertEqual(self.binding(names[0])["run_id"], before)
        self.hgs("pause", names[0])
        self.hgs("claude", str(self.project), "--new", "-n", tag, "-d", rc=1)
        self.assertEqual(self.binding(names[0])["run_id"], before)
        self.hgs("claude", "p", "--new", "-c", "-d", rc=1)
        self.hgs("claude", "p", "--new", "-n", "bad:name", "-d", rc=1)

    def test_reply_identity_tracks_only_main_responses_and_survives_resume(self):
        name = self.start()
        self.assertFalse(json.loads(self.hgs("inspect", name)).get("reply_id"))
        self.send_event(name, "SubagentStop", agent_id="child")
        self.assertFalse(json.loads(self.hgs("inspect", name)).get("reply_id"))
        self.send_event(name, "UserPromptSubmit", prompt="first")
        self.send_event(name, "Stop", last_assistant_message="First response")
        first = json.loads(self.hgs("inspect", name))
        self.assertTrue(first["reply_id"])
        listed = next(s for s in json.loads(self.hgs("ls", "--local", "--json"))["sessions"] if s["name"] == name)
        self.assertEqual(listed["reply_id"], first["reply_id"])
        self.assertEqual(listed["conversation_id"], first["conversation_id"])
        self.send_event(name, "SubagentStop", agent_id="child")
        self.send_event(name, "SessionEnd")
        self.assertEqual(json.loads(self.hgs("inspect", name))["reply_id"], first["reply_id"])
        self.hgs("pause", name); self.resume(name)
        self.assertEqual(json.loads(self.hgs("inspect", name))["reply_id"], first["reply_id"])
        renamed = name.rsplit("/", 1)[0] + "/renamed"
        self.hgs("rename", name, renamed); name = renamed
        self.assertEqual(json.loads(self.hgs("inspect", name))["reply_id"], first["reply_id"])
        self.send_event(name, "UserPromptSubmit", prompt="second")
        self.send_event(name, "Stop", last_assistant_message="Same text is still a new response")
        self.assertNotEqual(json.loads(self.hgs("inspect", name))["reply_id"], first["reply_id"])
        kimi = self.start("kimi", "reply-root")
        self.send_event(kimi, "Stop", agent_id="main", last_assistant_message="Kimi main response")
        main = json.loads(self.hgs("inspect", kimi))["reply_id"]
        self.send_event(kimi, "Stop", agent_id="child", last_assistant_message="Child response")
        self.assertEqual(json.loads(self.hgs("inspect", kimi))["reply_id"], main)

    def test_activity_journal_and_subagents(self):
        name = self.start()
        sid = self.binding(name)["conversation_id"]
        self.send_event(name, "UserPromptSubmit", prompt="Review <script>unsafe</script> changes")
        self.send_event(name, "PreToolUse", tool_name="Bash", tool_use_id="one", tool_input={"command": "pytest tests"})
        self.send_event(name, "PermissionRequest", tool_name="Bash")
        info = json.loads(self.hgs("inspect", name))
        self.assertEqual(info["phase"], "approval")
        self.assertEqual(info["active_tools"]["one"]["detail"], "pytest tests")
        cursor = info["cursor"]
        self.send_event(name, "PostToolUse", tool_name="Bash", tool_use_id="one")
        self.send_event(name, "SubagentStart", agent_id="reviewer", agent_type="Review")
        self.send_event(name, "Stop", last_assistant_message="Waiting for reviewer")
        self.assertEqual(self.binding(name)["activity"], "busy")
        self.hgs("pause", name, rc=1)
        self.send_event(name, "SubagentStop", agent_id="reviewer", agent_type="Review")
        info = json.loads(self.hgs("inspect", name, "--after", str(cursor)))
        self.assertEqual(info["activity"], "idle")
        self.assertEqual(info["conversation_id"], sid)
        self.assertEqual(info["subagents"]["reviewer"]["state"], "finished")
        self.assertEqual([e["type"] for e in info["events"]],
                         ["PostToolUse", "SubagentStart", "Stop", "SubagentStop"])
        self.assertEqual(json.loads(self.hgs("inspect", name, "--after", str(info["cursor"])))["events"], [])
        self.send_event(name, "SubagentStart", agent_id="background")
        self.assertEqual(self.binding(name)["activity"], "busy")
        self.hgs("pause", name, rc=1)
        self.send_event(name, "StopFailure", error_message="Provider unavailable")
        self.send_event(name, "SubagentStop", agent_id="background")
        self.assertEqual(self.binding(name)["activity"], "unknown")
        self.send_event(name, "UserPromptSubmit", prompt="Review <script>unsafe</script> changes")
        self.send_event(name, "Stop", last_assistant_message="Waiting for reviewer")
        self.hgs("pause", name)
        persisted = json.loads(self.hgs("inspect", name))
        self.assertEqual(persisted["prompt"], "Review <script>unsafe</script> changes")
        self.assertGreater(len(persisted["events"]), 4)
        self.assertEqual((self.root / "state/events.sqlite3").stat().st_mode & 0o777, 0o600)
        self.resume(name)
        self.assertEqual(json.loads(self.hgs("inspect", name))["last_message"], "Waiting for reviewer")

    def test_claude_next_message_suggestion_is_not_a_subagent(self):
        name = self.start()
        self.send_event(name, "UserPromptSubmit", prompt="Fix the build")
        self.send_event(name, "Stop", last_assistant_message="Fixed")
        cursor = json.loads(self.hgs("inspect", name))["cursor"]
        # Claude's internal suggestion agent reports only its SubagentStop.
        self.send_event(name, "SubagentStop", agent_id="a1b2c3", last_assistant_message="run the tests")
        info = json.loads(self.hgs("inspect", name, "--after", str(cursor)))
        self.assertEqual(info["prompt_suggestion"]["text"], "run the tests")
        self.assertNotIn("a1b2c3", info.get("subagents") or {})
        self.assertEqual(info["events"], [])
        self.send_event(name, "UserPromptSubmit", prompt="run the tests")
        self.assertNotIn("prompt_suggestion", json.loads(self.hgs("inspect", name)))

    def test_task_lists_are_confirmed_isolated_and_reset_with_conversation(self):
        name = self.start()
        run = self.binding(name)["run_id"]
        todo = {"todos": [{"content": "Inspect <source>", "status": "in_progress"}]}
        self.send_event(name, "PreToolUse", tool_name="TodoWrite", tool_use_id="plan-1", tool_input=todo)
        self.assertFalse(json.loads(self.hgs("inspect", name)).get("task_lists"))
        self.send_event(name, "PostToolUse", tool_name="TodoWrite", tool_use_id="plan-1")
        info = json.loads(self.hgs("inspect", name))
        self.assertEqual(info["task_lists"]["main"]["items"][0]["title"], "Inspect <source>")
        self.assertEqual(info["task_lists"]["main"]["run_id"], run)
        self.assertNotIn("pending_task_updates", info)
        self.send_event(name, "PostToolUseFailure", tool_name="TodoWrite", tool_input={"todos": []})
        self.send_event(name, "PostToolUse", tool_name="TodoWrite", agent_id="reviewer", tool_input={
            "todos": [{"content": "Review boundaries", "status": "completed"}]})
        info = json.loads(self.hgs("inspect", name))
        self.assertEqual(info["task_lists"]["main"]["items"][0]["status"], "in_progress")
        self.assertEqual(info["task_lists"]["agent:reviewer"]["items"][0]["status"], "completed")
        self.send_event(name, "PostToolUse", session_id="foreign-conversation", tool_name="TodoWrite", tool_input={"todos": []})
        self.assertEqual(json.loads(self.hgs("inspect", name))["task_lists"], info["task_lists"])
        self.send_event(name, "SubagentStop", agent_id="reviewer")
        self.send_event(name, "Stop")
        self.assertEqual(json.loads(self.hgs("inspect", name))["task_lists"]["main"]["items"][0]["status"], "in_progress")
        old = self.binding(name)["conversation_id"]
        self.t("send-keys", "-t", "=" + name + ":", "switch", "Enter")
        self.wait(lambda: self.binding(name)["conversation_id"] != old)
        self.assertFalse(json.loads(self.hgs("inspect", name)).get("task_lists"))

    def test_shell_process_hooks_identity_output_and_conversation_reset(self):
        for agent in ('claude','codex','kimi'):
            name=self.start(agent);binding=self.binding(name)
            for call in ('shell-a','shell-b'):
                self.send_event(name,'PreToolUse',tool_name='Bash',tool_use_id=call,tool_input={'command':'printf same'})
            self.send_event(name,'PostToolUse',tool_name='Bash',tool_use_id='shell-a',tool_response={'output':'foreground output','exit_code':7})
            self.send_event(name,'PostToolUse',tool_name='Bash',tool_use_id='shell-b',tool_response={'output':'background output','session_id':'native-b'})
            self.send_event(name,'Stop')
            data=json.loads(self.hgs('inspect',name));jobs=data['processes']['items'];self.assertEqual(len(jobs),2)
            a=next(j for j in jobs if j['call_id']=='shell-a');b=next(j for j in jobs if j['call_id']=='shell-b')
            self.assertEqual(a['status'],'failed');self.assertEqual(b['status'],'unknown');self.assertEqual(b['last_status'],'running')
            self.assertTrue(any(e.get('process_id')==a['id'] for e in data['events']))
            output=json.loads(self.hgs('processes',name,'--output',a['id'],'--run',binding['run_id'],'--conversation',binding['conversation_id']))
            self.assertEqual(output['output'],'foreground output')
            self.hgs('processes',name,'--output',a['id'],'--run','old','--conversation',binding['conversation_id'],rc=1)
            self.send_event(name,'PostToolUse',session_id='foreign',tool_name='Bash',tool_use_id='shell-c',tool_input={'command':'wrong'})
            self.assertEqual(len(json.loads(self.hgs('processes',name))['items']),2)
            self.t('send-keys','-t','='+name+':','switch','Enter')
            self.wait(lambda:self.binding(name)['conversation_id']!=binding['conversation_id'])
            self.assertEqual(json.loads(self.hgs('processes',name))['items'],[])

    def test_activity_errors_parallel_tools_and_fresh_conversation(self):
        name = self.start()
        self.send_event(name, "UserPromptSubmit", prompt="Run checks")
        for key in ("z-first", "a-second"):
            self.send_event(name, "PreToolUse", tool_name=key, tool_use_id=key)
        session = next(s for s in json.loads(self.hgs("ls", "--local", "--json"))["sessions"]
                       if s["name"] == name)
        self.assertEqual(session["current_tool"], "a-second")
        self.send_event(name, "PostToolUseFailure", tool_name="z-first", tool_use_id="z-first", error="test failed")
        self.assertEqual(self.binding(name)["phase"], "tool")
        self.assertEqual(list(self.binding(name)["active_tools"]), ["a-second"])
        self.send_event(name, "StopFailure", error_message="Provider unavailable")
        self.assertEqual(self.binding(name)["phase"], "error")
        self.assertEqual(self.binding(name)["activity"], "unknown")
        self.send_event(name, "UserPromptSubmit", prompt="Try again")
        self.assertEqual(self.binding(name)["last_error"], "")
        self.send_event(name, "Stop")
        old = self.binding(name)["conversation_id"]
        self.t("send-keys", "-t", "=" + name + ":", "switch", "Enter")
        self.wait(lambda: self.binding(name)["conversation_id"] != old)
        info = json.loads(self.hgs("inspect", name))
        self.assertFalse(info.get("prompt"))
        self.assertNotIn("prompt", self.binding(name))
        self.assertEqual([e["type"] for e in info["events"]], ["SessionStart"])
        self.assertFalse(json.loads(self.hgs("inspect", "sh/not-tracked"))["tracked"])

    def test_concurrent_hooks_preserve_subagents_and_journal(self):
        name = self.start()
        children = {"child-" + str(i) for i in range(12)}
        with ThreadPoolExecutor(max_workers=6) as pool:
            started = [pool.submit(self.send_event, name, "SubagentStart", agent_id=child)
                       for child in children]
            for result in started:
                result.result()
        info = json.loads(self.hgs("inspect", name))
        self.assertEqual(set(info["subagents"]), children)
        self.assertTrue(all(s["state"] == "working" for s in info["subagents"].values()))
        self.assertEqual({e["agent_id"] for e in info["events"] if e["type"] == "SubagentStart"}, children)
        self.hgs("pause", name, rc=1)
        with ThreadPoolExecutor(max_workers=6) as pool:
            finished = [pool.submit(self.send_event, name, "SubagentStop", agent_id=child)
                        for child in children]
            for result in finished:
                result.result()
        info = json.loads(self.hgs("inspect", name))
        self.assertTrue(all(s["state"] == "finished" for s in info["subagents"].values()))
        self.assertEqual(info["activity"], "idle")
        self.hgs("pause", name)

    @unittest.skipIf(LEGACY_STATE, "native pause also verifies pending tools")
    def test_pending_tool_blocks_pause_after_last_subagent_finishes(self):
        name = self.start()
        self.send_event(name, "PreToolUse", tool_name="Bash", tool_use_id="pending")
        self.send_event(name, "SubagentStart", agent_id="helper")
        self.send_event(name, "SubagentStop", agent_id="helper")
        record = self.binding(name)
        self.assertIn("pending", record["active_tools"])
        self.assertNotEqual(record["activity"], "idle")
        self.hgs("pause", name, rc=1)
        # A legacy manifest may carry the old incorrect idle summary. Preflight
        # must verify actual pending work, not rely only on that summary string.
        record["activity"] = record["phase"] = "idle"
        path = self.root / "state" / (hashlib.sha256(name.encode()).hexdigest() + ".json")
        path.write_text(json.dumps(record) + "\n")
        self.hgs("pause", name, rc=1)
        self.t("has-session", "-t", "=" + name)

    @unittest.skipIf(LEGACY_STATE, "native pause rechecks work immediately before signaling")
    def test_batch_pause_rechecks_activity_before_each_signal(self):
        first, second = self.start(tag="alpha"), self.start(tag="beta")
        sid = self.binding(first)["conversation_id"]
        delay = self.root / ("delay-term-" + sid)
        waiting = self.root / ("waiting-term-" + sid)
        delay.touch()
        operation = subprocess.Popen([str(HGS), "pause", "--all"], env=self.env,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            # Both records have passed preflight; the first agent is draining
            # after SIGTERM, while the second has not received any signal yet.
            self.wait(waiting.exists)
            self.assertTrue(self.binding(second).get("pausing"))
            self.send_event(second, "SubagentStart", agent_id="late-worker")
            self.assertEqual(self.binding(second)["activity"], "busy")
            delay.unlink()
            output, errors = operation.communicate(timeout=20)
            self.assertEqual(operation.returncode, 1, output + errors)
            self.assertTrue(self.binding(first).get("paused"))
            self.assertFalse(self.binding(second).get("paused"))
            self.assertFalse(self.binding(second).get("pausing"))
            self.t("has-session", "-t", "=" + second)
            self.assertEqual(self.binding(second)["subagents"]["late-worker"]["state"], "working")
        finally:
            delay.unlink(missing_ok=True)
            if operation.poll() is None:
                operation.terminate()
                operation.communicate(timeout=5)

    def test_reboot_roundtrip_all_agents(self):
        names = [self.start(a) for a in ("claude", "codex", "kimi")]
        ids = {n: self.binding(n)["conversation_id"] for n in names}
        runs = {n: self.binding(n)["run_id"] for n in names}
        self.hgs("pause", "--all")
        snapshot = json.loads(self.hgs("ls", "--local", "--json"))
        self.assertEqual({s["state"] for s in snapshot["sessions"]}, {"paused"})
        # The default server exits when the last pane closes: resume crosses a
        # real server lifetime, just as after reboot. No session name guessing.
        self.hgs("resume", "--all")
        for name in names:
            self.wait(lambda: self.binding(name).get("run_id") != runs[name] and
                      self.binding(name).get("activity") == "idle")
            self.assertEqual(self.binding(name)["conversation_id"], ids[name])
        launches = [json.loads(l) for l in (self.root / "argv.jsonl").read_text().splitlines()]
        self.assertTrue(all("--no-daemon" in x["argv"] for x in launches if x["agent"] == "codex"))

    def test_same_directory_switch_and_stale_hook(self):
        a, b = self.start(tag="one"), self.start(tag="two")
        original = self.binding(a)
        other = self.binding(b)["conversation_id"]
        self.assertNotEqual(original["conversation_id"], other)
        self.t("send-keys", "-t", "=" + a + ":", "switch", "Enter")
        self.wait(lambda: self.binding(a)["conversation_id"] != original["conversation_id"])
        selected = self.binding(a)["conversation_id"]
        self.hgs("pause", a)
        self.resume(a)
        stale = dict(self.env, HGS_SESSION=a, HGS_RUN_ID=original["run_id"])
        self.hook({"hook_event_name": "SessionStart", "session_id": "wrong"}, stale)
        self.assertEqual(self.binding(a)["conversation_id"], selected)
        self.assertEqual(self.binding(b)["conversation_id"], other)

    def test_busy_batch_preflight_and_missing_history(self):
        a, b = self.start(tag="one"), self.start(tag="two")
        self.t("send-keys", "-t", "=" + b + ":", "busy", "Enter")
        self.wait(lambda: self.binding(b)["activity"] == "busy")
        self.hgs("pause", "--all", rc=1)
        self.t("has-session", "-t", "=" + a)
        self.t("has-session", "-t", "=" + b)
        Path(self.binding(a)["transcript"]).unlink()
        self.hgs("pause", a, rc=1)
        self.t("has-session", "-t", "=" + a)

    def test_dry_run_and_kill_saved(self):
        name = self.start()
        before = self.binding(name)
        self.hgs("pause", name, "--dry-run")
        self.assertEqual(self.binding(name), before)
        self.hgs("pause", name)
        before = self.binding(name)
        self.hgs("resume", name, "--dry-run")
        self.assertEqual(self.binding(name), before)
        self.hgs("kill", name)
        self.assertFalse(self.binding(name))

    def test_untracked_and_replaced_pane_refused(self):
        self.t("new-session", "-d", "-s", "sh/untracked", "sleep 60")
        self.hgs("pause", "sh/untracked", rc=1)
        self.t("has-session", "-t", "=sh/untracked")
        name = self.start()
        self.t("kill-session", "-t", "=" + name)
        self.t("new-session", "-d", "-s", name, "sleep 60")
        self.hgs("pause", name, rc=1)
        self.t("has-session", "-t", "=" + name)

    def test_project_launcher_and_hook_merge(self):
        settings = self.root / ".claude/settings.json"
        settings.parent.mkdir()
        settings.symlink_to(self.root / "actual-settings.json")
        settings.write_text(json.dumps({"custom": True, "hooks": {"Stop": [
            {"hooks": [{"type": "command", "command": "true"}]}]}}))
        launcher = self.project / "run_claude.sh"
        probed = self.root / "launcher-probed"
        launcher.write_text('#!/bin/sh\n[ "$1" = --version ] && : > ' + shlex.quote(str(probed)) + '\nexec claude "$@"\n')
        launcher.chmod(0o755)
        name = self.start()
        self.hgs("pause", name)
        self.resume(name)
        self.assertEqual(self.binding(name)["base"][0], str(launcher))
        data = json.loads(settings.read_text())
        self.assertTrue(settings.is_symlink())
        self.assertTrue(data["custom"])
        self.assertEqual(len(data["hooks"]["Stop"]), 2)
        # A launcher runs only under supervision: its version is never read.
        self.assertFalse(probed.exists())
        self.assertNotIn("PostModelSwitch", data["hooks"])

    @unittest.skipIf(LEGACY_STATE, "Claude's PostModelSwitch requires the native CLI")
    def test_claude_model_switch_hook_records_the_exact_model(self):
        settings = self.root / ".claude/settings.json"
        # Older Claude Code would stop every start at a warning for the unknown event.
        (self.root / "claude-version").write_text("2.1.295 (Claude Code)\n")
        name = self.start()
        hooks = json.loads(settings.read_text())["hooks"]
        self.assertIn("Stop", hooks)
        self.assertNotIn("PostModelSwitch", hooks)
        self.hgs("pause", name)
        (self.root / "claude-version").unlink()
        self.resume(name)
        hooks = json.loads(settings.read_text())["hooks"]
        self.assertEqual(hooks["PostModelSwitch"], hooks["Stop"])
        self.assertNotIn("PreModelSwitch", hooks)
        # The payload of Claude Code 2.1.296 after /model in Terminal.
        binding = self.binding(name)
        switch = dict(transcript_path=binding["transcript"], cwd=str(self.project), prompt_id=str(uuid.uuid4()),
                      from_model="claude-opus-5-5", to_model="claude-fable-5-1", requested_model="fable",
                      source="command", context_tokens=0, prompt_cache_warm=False, cache_ttl="5m",
                      estimated_cache_write_usd=0, pricing="catalog")
        self.send_event(name, "PostModelSwitch", **switch)
        self.assertEqual(self.binding(name)["model"], "claude-fable-5-1")
        info = json.loads(self.hgs("inspect", name))
        self.assertEqual(info["model"], "claude-fable-5-1")
        self.assertEqual((info["activity"], info["phase"]), ("idle", "idle"))
        # It changes the session's model, not its Activity.
        self.assertNotIn("PostModelSwitch", [event["type"] for event in info["events"]])
        # Another conversation's switch and an invalid model change nothing.
        self.send_event(name, "PostModelSwitch", **dict(switch, session_id="foreign", to_model="claude-haiku-5-5"))
        self.send_event(name, "PostModelSwitch", **dict(switch, to_model="-flag"))
        self.assertEqual(self.binding(name)["model"], "claude-fable-5-1")
        # An automatic switch has no /model receipt in the transcript.
        self.send_event(name, "PostModelSwitch", **dict(switch, from_model="claude-fable-5-1",
                                                        to_model="claude-sonnet-5-5", requested_model=None, source="auto"))
        self.assertEqual(json.loads(self.hgs("inspect", name))["model"], "claude-sonnet-5-5")

    @unittest.skipIf(LEGACY_STATE, "Claude's PostModelSwitch requires the native CLI")
    def test_older_claude_loses_only_hgs_model_switch_entry(self):
        settings = self.root / ".claude/settings.json"
        reads = self.root / "version-reads"
        name = self.start()
        data = json.loads(settings.read_text())
        entry = data["hooks"]["Stop"]
        self.assertEqual(data["hooks"]["PostModelSwitch"], entry)
        # The answer channel is versioned the same way (Claude Code 2.1.296).
        channel = lambda hooks: [group for group in hooks.get("PermissionRequest", [])
                                 if any(hook["command"].endswith("__state answer-channel") for hook in group["hooks"])]
        others = lambda hooks: {event: [group for group in groups if group not in channel(hooks)]
                                for event, groups in hooks.items() if event != "PostModelSwitch"}
        self.assertEqual(len(channel(data["hooks"])), 1)
        # The user's own hook for the event, beside hgs's.
        user = {"matcher": "opus", "hooks": [{"type": "command", "command": "true"}]}
        data["hooks"]["PostModelSwitch"].append(user)
        settings.write_text(json.dumps(data))
        asked = len(reads.read_text().splitlines())
        # The same Claude Code is not asked for its version at each start.
        self.hgs("pause", name)
        self.resume(name)
        self.assertEqual(len(reads.read_text().splitlines()), asked)
        self.assertEqual(json.loads(settings.read_text())["hooks"]["PostModelSwitch"], entry + [user])
        # An older one installed in its place is, and hgs's entry goes before it starts.
        self.hgs("pause", name)
        self.script("claude", AGENT + "# Claude Code 2.1.295\n")
        (self.root / "claude-version").write_text("2.1.295 (Claude Code)\n")
        self.resume(name)
        hooks = json.loads(settings.read_text())["hooks"]
        self.assertEqual(hooks["PostModelSwitch"], [user])
        self.assertEqual(channel(hooks), [])
        self.assertEqual(others(hooks), others(data["hooks"]))
        self.assertEqual(len(reads.read_text().splitlines()), asked + 1)
        # A current version again gets the entry back, beside the user's.
        self.hgs("pause", name)
        self.script("claude", AGENT)
        (self.root / "claude-version").unlink()
        self.resume(name)
        data = json.loads(settings.read_text())
        self.assertEqual(data["hooks"]["PostModelSwitch"], [user] + entry)
        self.assertEqual(len(channel(data["hooks"])), 1)
        # Without the user's hook, an older version loses the event itself
        # and starts without a settings warning.
        data["hooks"]["PostModelSwitch"] = entry
        settings.write_text(json.dumps(data))
        self.hgs("pause", name)
        (self.root / "settings-warning").unlink(missing_ok=True)
        self.script("claude", AGENT + "# Claude Code 2.0.9\n")
        (self.root / "claude-version").write_text("2.0.9 (Claude Code)\n")
        self.resume(name)
        hooks = json.loads(settings.read_text())["hooks"]
        self.assertNotIn("PostModelSwitch", hooks)
        self.assertEqual(others(hooks), others(data["hooks"]))
        self.assertFalse((self.root / "settings-warning").exists())

    def launched_settings(self):
        """The flag settings of the newest agent start, or None."""
        argv = json.loads((self.root / "argv.jsonl").read_text().splitlines()[-1])["argv"]
        if "--settings" not in argv:
            return None
        self.assertEqual(argv.count("--settings"), 1, argv)
        return json.loads(argv[argv.index("--settings") + 1])

    def status_line(self, name, command, conversation=None, model="claude-opus-5-5", window=1000000):
        """Run the status line `command` as Claude Code 2.1.296 runs it for this run: through
        /bin/sh in the session folder with its environment and the JSON input on stdin."""
        binding = self.binding(name)
        payload = dict(session_id=conversation or binding["conversation_id"], transcript_path=binding["transcript"],
                       cwd=str(self.project), model=dict(id=model, display_name="Fixture"),
                       workspace=dict(current_dir=str(self.project), project_dir=str(self.project), added_dirs=[]),
                       version="2.1.296", output_style=dict(name="default"),
                       context_window=dict(total_input_tokens=100, total_output_tokens=8, context_window_size=window,
                                           current_usage=dict(input_tokens=100, output_tokens=8,
                                                              cache_creation_input_tokens=0, cache_read_input_tokens=0),
                                           used_percentage=0, remaining_percentage=100))
        env = dict(self.env, HGS_SESSION=name, HGS_RUN_ID=binding["run_id"], HGS_STATE_DIR=str(self.root / "state"),
                   CLAUDE_PROJECT_DIR=str(self.project))
        text = json.dumps(payload) + "\n"
        result = subprocess.run(["/bin/sh", "-c", command], env=env, cwd=self.project, input=text,
                                capture_output=True, text=True, timeout=20)
        return result, text

    def capacity(self, name):
        context = json.loads(self.hgs("inspect", name))["session_usage"].get("context", {})
        return {key: context.get(key) for key in ("limit", "limit_source", "limit_model")}

    @unittest.skipIf(LEGACY_STATE, "Claude's status line telemetry requires the native CLI")
    def test_claude_status_line_reports_capacity_and_keeps_the_users(self):
        settings = self.root / ".claude/settings.json"
        settings.parent.mkdir()
        user = {"type": "command", "command": 'cat > "$HOME/user-input.json"; printf "user line"; exit "${STATUS:-0}"',
                "padding": 1, "refreshInterval": 7}
        settings.write_text(json.dumps({"statusLine": user, "theme": "dark"}))
        name = self.start()
        # The status line is registered for the run only: settings.json keeps the user's.
        line = self.launched_settings()["statusLine"]
        self.assertEqual({key: line.get(key) for key in ("type", "padding", "refreshInterval")},
                         {"type": "command", "padding": 1, "refreshInterval": 7})
        self.assertEqual(shlex.split(line["command"])[1:], ["__state", "statusline"])
        self.assertEqual(json.loads(settings.read_text())["statusLine"], user)
        self.assertNotIn("--settings", self.binding(name)["base"])
        self.assertEqual(self.capacity(name), dict(limit=None, limit_source=None, limit_model=None))
        # Claude's input reaches the user's status line unchanged; its output is what Claude shows.
        result, text = self.status_line(name, line["command"])
        self.assertEqual((result.returncode, result.stdout), (0, "user line"), result.stderr)
        self.assertEqual((self.root / "user-input.json").read_text(), text)
        self.assertEqual(self.capacity(name), dict(limit=1000000, limit_source="native_statusline",
                                                   limit_model="claude-opus-5-5"))
        self.env["STATUS"] = "3"
        result, _ = self.status_line(name, line["command"])
        self.assertEqual((result.returncode, result.stdout), (3, "user line"))
        del self.env["STATUS"]
        # After a model switch the sample is stale until the status line reports the new model.
        binding = self.binding(name)
        self.send_event(name, "PostModelSwitch", transcript_path=binding["transcript"], cwd=str(self.project),
                        from_model="claude-opus-5-5", to_model="claude-haiku-5-5", requested_model="haiku",
                        source="command", context_tokens=0)
        self.assertEqual(self.capacity(name)["limit"], None)
        self.status_line(name, line["command"], model="claude-haiku-5-5", window=200000)
        self.assertEqual(self.capacity(name), dict(limit=200000, limit_source="native_statusline",
                                                   limit_model="claude-haiku-5-5"))
        # Another conversation's report never describes this one.
        self.status_line(name, line["command"], conversation=str(uuid.uuid4()), model="claude-haiku-5-5")
        self.assertEqual(self.capacity(name)["limit"], None)
        # A new run starts without the previous run's sample, and its own status line restores it.
        self.status_line(name, line["command"], model="claude-haiku-5-5", window=200000)
        self.hgs("pause", name)
        self.resume(name)
        line = self.launched_settings()["statusLine"]
        self.assertEqual(self.capacity(name)["limit"], None)
        self.status_line(name, line["command"], model="claude-haiku-5-5", window=200000)
        self.assertEqual(self.capacity(name)["limit"], 200000)
        # The user's change applies at once; without a status line of theirs nothing is shown.
        settings.write_text(json.dumps({**json.loads(settings.read_text()),
                                        "statusLine": dict(user, command="printf changed")}))
        self.assertEqual(self.status_line(name, line["command"])[0].stdout, "changed")
        data = json.loads(settings.read_text())
        del data["statusLine"]
        settings.write_text(json.dumps(data))
        result, _ = self.status_line(name, line["command"], window=400000)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, "", ""))
        self.assertEqual(self.capacity(name)["limit"], None)  # Inspect still reports Haiku.
        # Without one at launch, hgs's entry has only its command.
        self.hgs("pause", name)
        self.resume(name)
        self.assertEqual(set(self.launched_settings()["statusLine"]), {"type", "command"})

    @unittest.skipIf(LEGACY_STATE, "Claude's status line telemetry requires the native CLI")
    def test_claude_status_line_is_not_registered_where_claude_would_differ(self):
        # An older Claude Code: its status line input was not verified.
        (self.root / "claude-version").write_text("2.1.295 (Claude Code)\n")
        name = self.start()
        self.assertIsNone(self.launched_settings())
        self.hgs("pause", name)
        (self.root / "claude-version").unlink()
        self.resume(name)
        self.assertIsNotNone(self.launched_settings())
        # Settings the user passes themselves stay theirs.
        own = self.start("claude", "own", "--", "--settings", '{"theme": "dark"}')
        self.assertEqual(self.launched_settings(), {"theme": "dark"})
        self.hgs("pause", own)
        self.resume(own)
        self.assertEqual(self.launched_settings(), {"theme": "dark"})
        # So does a folder of project settings, a new worktree or policy settings the launch chooses.
        for tag, option in (("root", "--project-config-root=" + str(self.root)), ("policy", "--managed-settings"),
                            ("worktree", "-w")):
            self.start("claude", tag, "--", option, "{}")
            self.assertIsNone(self.launched_settings())
        # A status line in the enclosing repository's local settings is resolved by
        # ownership checks that hgs does not repeat.
        (self.project / ".git").mkdir()
        nested = self.project / "nested"
        (nested / ".claude").mkdir(parents=True)
        (self.project / ".claude").mkdir()
        (self.project / ".claude/settings.local.json").write_text(json.dumps(
            {"statusLine": {"type": "command", "command": "printf local"}}))
        self.hgs("claude", str(nested), "-n", "nested", "-d")
        self.wait(lambda: self.binding("claude/nested/nested").get("conversation_id"))
        self.assertIsNone(self.launched_settings())
        # In a linked worktree Claude reads the local settings of its repository.
        repository, linked = self.root / "repository", self.root / "linked"
        git = ["git", "-c", "user.name=HGS fixture", "-c", "user.email=hgs@example.invalid", "-c", "commit.gpgSign=false",
               "-c", "core.hooksPath=" + os.devnull]
        env = {key: value for key, value in self.env.items() if not key.startswith("GIT_")}
        env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull, GIT_TERMINAL_PROMPT="0")
        for args in (["init", "-q", "-b", "main", str(repository)],
                     ["-C", str(repository), "commit", "-q", "--allow-empty", "-m", "Fixture"],
                     ["-C", str(repository), "worktree", "add", "-q", "-b", "linked", str(linked)]):
            subprocess.run(git + args, env=env, check=True, capture_output=True, timeout=20)
        (repository / ".claude").mkdir()
        (repository / ".claude/settings.local.json").write_text(json.dumps(
            {"statusLine": {"type": "command", "command": "printf repository"}}))
        self.hgs("claude", str(linked), "-n", "linked", "-d")
        self.wait(lambda: self.binding("claude/linked/linked").get("conversation_id"))
        self.assertIsNone(self.launched_settings())
        (repository / ".claude/settings.local.json").write_text(json.dumps({"theme": "dark"}))
        self.hgs("claude", str(linked), "-n", "registered", "-d")
        self.wait(lambda: self.binding("claude/linked/registered").get("conversation_id"))
        self.assertIsNotNone(self.launched_settings())
        # A settings file Claude Code cannot read is left for it to report.
        (self.project / ".claude/settings.local.json").write_text("{")
        self.hgs("pause", name)
        self.resume(name)
        self.assertIsNone(self.launched_settings())

    def test_failed_resume_keeps_binding_and_no_fresh_fallback(self):
        name = self.start()
        sid = self.binding(name)["conversation_id"]
        self.hgs("pause", name)
        paused_run = self.binding(name)["run_id"]
        self.script("claude", "#!/bin/sh\nexit 7\n")
        self.hgs("resume", name, "-d")
        # The session ends only after its supervisor has recorded the resumed
        # run, the agent's failure and any fresh fallback.
        self.wait(lambda: subprocess.run([str(self.bin / "tmux"), "has-session", "-t", "=" + name],
                  env=self.env, capture_output=True).returncode != 0)
        self.assertNotEqual(self.binding(name)["run_id"], paused_run)
        self.assertEqual(self.binding(name)["expected_id"], sid)
        self.assertEqual(self.binding(name)["conversation_id"], sid)
        snapshot = json.loads(self.hgs("ls", "--local", "--json"))
        self.assertEqual(snapshot["sessions"][0]["state"], "stopped")
        self.script("claude", AGENT)
        self.resume(name)
        self.assertEqual(self.binding(name)["conversation_id"], sid)

    @unittest.skipIf(LEGACY_STATE, "fix introduced by the native lifecycle model")
    def test_session_ending_between_terminal_queries_lists_its_saved_state(self):
        # A listing reads the sessions, then their panes. A session that ends in
        # between lists with its saved state, never as an untracked live row.
        name = self.start()
        listed = self.root / "end-after-listing"
        tmux = f"{shlex.quote(TMUX)} -S {shlex.quote(self.socket)} -f /dev/null"
        self.script("tmux", f'''#!/bin/sh
{tmux} "$@"; status=$?
if [ "$1" = list-sessions ] && rm {shlex.quote(str(listed))} 2>/dev/null; then
    {tmux} send-keys -t {shlex.quote("=" + name + ":")} unknown-exit Enter
    tries=0
    while {tmux} has-session -t {shlex.quote("=" + name)} 2>/dev/null && [ $tries -lt 500 ]; do
        tries=$((tries + 1)); sleep 0.01
    done
fi
exit $status
''')
        listed.touch()
        sessions = json.loads(self.hgs("ls", "--local", "--json"))["sessions"]
        self.assertFalse(listed.exists())
        self.assertNotEqual(subprocess.run([str(self.bin / "tmux"), "has-session", "-t", "=" + name],
                                           env=self.env, capture_output=True).returncode, 0)
        self.assertEqual([(s["name"], s.get("state")) for s in sessions], [(name, "stopped")])
        self.assertTrue(sessions[0]["resumable"])

    def test_normal_launch_resumes_saved_and_fresh_replaces_it(self):
        name = self.start()
        sid = self.binding(name)["conversation_id"]
        self.hgs("pause", name)
        self.assertIn("--resume", self.hgs("claude", "p", "-n", "one", "--dry-run"))
        previous_run = self.binding(name)["run_id"]
        self.hgs("claude", "p", "-n", "one", "-d")
        self.wait(lambda: self.binding(name).get("run_id") != previous_run and
                  self.binding(name).get("activity") == "idle")
        self.assertEqual(self.binding(name)["conversation_id"], sid)
        self.hgs("pause", name)
        self.hgs("claude", "p", "-n", "one", "-d", "--fresh")
        self.wait(lambda: self.binding(name).get("conversation_id") not in (None, sid))

    def test_resume_fresh_keeps_launch_settings_and_earlier_activity(self):
        name = self.start("claude", "one", "--", "--effort", "high", "initial prompt")
        sid = self.binding(name)["conversation_id"]
        self.send_event(name, "UserPromptSubmit", prompt="Earlier request")
        self.send_event(name, "Stop", last_assistant_message="Earlier answer")
        self.hgs("resume", name, "--fresh", "-d", rc=1)  # A live agent uses Clear.
        self.hgs("pause", name)
        run = self.binding(name)["run_id"]
        self.hgs("resume", name, "--fresh", "--expected-run-id", "another-run", "-d", rc=1)
        self.hgs("resume", name, "--fresh", "--archive", "saved", "-d", rc=1)
        self.hgs("resume", name, "--expected-run-id", run, "-d", rc=1)
        self.hgs("resume", "--all", "--fresh", rc=1)
        dry = self.hgs("resume", name, "--fresh", "-d", "--dry-run")
        self.assertIn("HGS_FRESH_REPLACES=" + run, dry)
        self.assertNotIn("--resume", dry)
        self.hgs("resume", name, "--fresh", "--expected-run-id", run, "-d")
        self.wait(lambda: self.binding(name).get("conversation_id") not in (None, sid))
        last = json.loads((self.root / "argv.jsonl").read_text().splitlines()[-1])["argv"]
        self.assertEqual(last[:2], ["--effort", "high"])
        self.assertFalse({"--resume", sid, "initial prompt"} & set(last))
        record = self.binding(name)
        self.assertEqual(record["session_clear"]["previous_conversation_id"], sid)
        self.assertNotIn("fresh_from", record)
        inspection = json.loads(self.hgs("inspect", name))
        self.assertEqual(inspection["cleared_conversations"], [sid])
        timeline = [e["detail"] or e["type"] for e in inspection["events"]
                    if e["type"] in ("UserPromptSubmit", "Stop", "SessionCleared")]
        self.assertEqual(timeline, ["Earlier request", "Earlier answer", "SessionCleared"])
        # The new conversation resumes exactly; the replaced one stays native history.
        fresh = record["conversation_id"]
        self.hgs("pause", name)
        self.resume(name)
        self.assertEqual(self.binding(name)["conversation_id"], fresh)

    def test_no_hook_and_subagent_do_not_confirm_binding(self):
        self.hgs("claude", "p", "-n", "one", "-d", "--", "--no-hook")
        name = "claude/p/one"
        self.wait(lambda: self.binding(name).get("run_id"))
        r = self.binding(name)
        env = dict(self.env, HGS_SESSION=name, HGS_RUN_ID=r["run_id"])
        self.hook({"hook_event_name": "SessionStart", "session_id": "child",
                   "agent_id": "subagent"}, env)
        self.hgs("pause", name, rc=1)
        self.t("has-session", "-t", "=" + name)
        self.assertIsNone(self.binding(name)["conversation_id"])

    def test_resume_selector_and_initial_prompt(self):
        name = self.start("codex", "one", "--", "-c", 'model="a b"', "initial prompt")
        sid = self.binding(name)["conversation_id"]
        self.hgs("pause", name)
        self.resume(name)
        last = json.loads((self.root / "argv.jsonl").read_text().splitlines()[-1])["argv"]
        self.assertEqual(last[:4], ["resume", sid, "-c", 'model="a b"'])
        self.assertNotIn("initial prompt", last)

    @unittest.skipIf(LEGACY_STATE, "native Codex launch inserts daemon control before prompt delimiters")
    def test_codex_daemon_flag_precedes_prompt_separator(self):
        for tag, prompt in (("plain", "initial prompt"), ("literal", "--no-daemon")):
            with self.subTest(prompt=prompt):
                name = self.start("codex", tag, "--", "--", prompt)
                sid = self.binding(name)["conversation_id"]
                launched = json.loads((self.root / "argv.jsonl").read_text().splitlines()[-1])["argv"]
                delimiter = launched.index("--")
                self.assertEqual(launched[:delimiter].count("--no-daemon"), 1)
                self.assertEqual(launched[delimiter + 1:], [prompt])
                self.hgs("pause", name)
                self.resume(name)
                resumed = json.loads((self.root / "argv.jsonl").read_text().splitlines()[-1])["argv"]
                self.assertEqual(resumed[:2], ["resume", sid])
                self.assertEqual(resumed.count("--no-daemon"), 1)
                self.assertNotIn("--", resumed)
                self.assertNotIn("initial prompt", resumed)

    @unittest.skipIf(LEGACY_STATE, "native Codex capability detection")
    def test_old_codex_starts_and_resumes_without_unsupported_daemon_flag(self):
        (self.root / 'old-codex').touch()
        name = self.start('codex', 'legacy')
        sid = self.binding(name)['conversation_id']
        self.hgs('pause', name); self.resume(name)
        self.assertEqual(self.binding(name)['conversation_id'], sid)
        launches = [json.loads(line) for line in (self.root / 'argv.jsonl').read_text().splitlines()]
        self.assertTrue(launches)
        self.assertTrue(all('--no-daemon' not in launch['argv'] for launch in launches))

    @unittest.skipIf(LEGACY_STATE, "native Kimi tracking accounts for queued prompts and tasks")
    def test_kimi_queued_prompt_and_started_task_prevent_pause(self):
        name = self.start("kimi")
        for kind in ("UserPromptQueued", "TaskStarted"):
            with self.subTest(event=kind):
                self.send_event(name, kind, prompt="Queued work")
                self.assertEqual(self.binding(name)["activity"], "busy")
                self.assertFalse(self.binding(name)["main_done"])
                self.send_event(name, "SubagentStart", agent_id="helper")
                self.send_event(name, "SubagentStop", agent_id="helper")
                self.assertEqual(self.binding(name)["activity"], "busy")
                self.hgs("pause", name, rc=1)
                self.t("has-session", "-t", "=" + name)
                self.send_event(name, "Stop")
                self.assertEqual(self.binding(name)["activity"], "idle")
                self.hgs("pause", name, "--dry-run")

    def test_server_loss_and_custom_agent_home(self):
        custom = str(self.root / "custom codex home")
        self.env["CODEX_HOME"] = custom
        name = self.start("codex")
        before = self.binding(name)
        self.t("kill-server")
        self.env.pop("CODEX_HOME")
        self.hgs("resume", name, "-d")
        self.wait(lambda: self.binding(name).get("run_id") != before["run_id"] and
                  self.binding(name).get("activity") == "idle")
        self.assertEqual(self.binding(name)["conversation_id"], before["conversation_id"])
        last = json.loads((self.root / "argv.jsonl").read_text().splitlines()[-1])
        self.assertEqual(last["codex_home"], custom)

    def test_pausing_continue_does_not_trigger_fresh_fallback(self):
        name = self.start("codex", "one", "-c")
        sid = self.binding(name)["conversation_id"]
        self.hgs("pause", name)
        self.assertEqual(self.binding(name)["conversation_id"], sid)
        launches = (self.root / "argv.jsonl").read_text().splitlines()
        self.assertEqual(len(launches), 1)

    def test_timeout_does_not_escalate_to_forced_kill(self):
        name = self.start()
        (self.root / "ignore-term").touch()
        reports = []
        report = self.stop_report
        self.stop_report = lambda: reports.append(1) or report()
        self.hgs("pause", name, rc=1)
        self.assertEqual(reports, [], "an expected refusal carries no stop report")
        self.t("has-session", "-t", "=" + name)
        self.assertFalse(self.binding(name).get("pausing"))
        self.assertFalse(self.binding(name).get("paused"))

    def test_stop_report_follows_only_a_missed_stop_deadline(self):
        name = self.start()
        reports = []
        report = self.stop_report
        self.stop_report = lambda: reports.append(time.monotonic()) or report()
        # An ordinary pause and resume report nothing.
        self.hgs("pause", name)
        self.resume(name)
        self.assertEqual(reports, [])
        # A private fake agent ignores SIGTERM: the pause misses its deadline,
        (self.root / "ignore-term").touch()
        # and only then does the failure carry the report.
        started = time.monotonic()
        with self.assertRaises(AssertionError) as caught:
            self.hgs("pause", name)
        self.assertEqual(len(reports), 1)
        self.assertGreaterEqual(reports[0] - started, 10)
        message, pid = str(caught.exception), str(self.binding(name)["pid"])
        self.assertIn("did not exit after SIGTERM", message)
        self.assertIn("Stop diagnostics:", message)
        self.assertRegex(message, r"(?m)^process: " + pid + r" \d+ ")
        self.assertRegex(message, r"(?m)^python in %\d+: (Current thread|Thread) 0x.*line \d+ in <module>")
        self.assertRegex(message, r"(?m)^host: load \d+\.\d\d \d+\.\d\d \d+\.\d\d \(1, 5, 15 min\) on \d+ CPUs$")
        if sys.platform != "darwin":
            self.assertRegex(message, r"(?m)^wchan " + pid + ": ")
            return
        # The agent's native stack, or explicit evidence that Apple's sample did
        # not finish within its own limit: a loaded runner can keep it longer.
        # Neither replaces the failure; StopReportControls checks both exactly.
        sampled = f"Command '['sample', '{pid}', '1', '-mayDie', '-file', '/dev/stdout']' timed out after 5 seconds"
        self.assertRegex(message, rf"(?m)^(stack {pid} |unavailable: native stack {pid}: TimeoutExpired: {re.escape(sampled)}$)")

    def test_abandoned_pause_does_not_block_future_work(self):
        name = self.start()
        for p in (self.root / "state").glob("*.json"):
            r = json.loads(p.read_text())
            r["pausing"] = {"pid": 999999999, "start": "old process"}
            p.write_text(json.dumps(r))
        self.t("send-keys", "-t", "=" + name + ":", "busy", "Enter")
        self.wait(lambda: self.binding(name)["activity"] == "busy")
        self.t("send-keys", "-t", "=" + name + ":", "idle", "Enter")
        self.wait(lambda: self.binding(name)["activity"] == "idle")
        self.hgs("pause", name)

    @unittest.skipIf(LEGACY_STATE, "fix introduced by the native lifecycle model")
    def test_session_end_does_not_claim_live_idle_process_exited(self):
        name = self.start("codex")
        before = self.binding(name)
        self.send_event(name, "SessionEnd")
        self.t("has-session", "-t", "=" + name)
        info = json.loads(self.hgs("inspect", name))
        self.assertEqual(info["activity"], "idle")
        self.assertNotEqual(info["phase"], "ended")
        session = next(s for s in json.loads(self.hgs("ls", "--local", "--json"))["sessions"]
                       if s["name"] == name)
        self.assertNotIn(session.get("state", "live"), ("paused", "stopped"))
        self.assertTrue(session["resumable"])
        self.hgs("pause", name)
        self.hgs("resume", name, "-d")
        self.wait(lambda: self.binding(name).get("run_id") != before["run_id"] and
                  self.binding(name).get("activity") == "idle")
        self.assertEqual(self.binding(name)["conversation_id"], before["conversation_id"])

    @unittest.skipIf(LEGACY_STATE, "fix introduced by the native lifecycle model")
    def test_session_end_cannot_make_busy_process_safe_to_pause(self):
        name = self.start()
        self.send_event(name, "UserPromptSubmit", prompt="Still working")
        self.send_event(name, "PreToolUse", tool_name="Bash", tool_use_id="running")
        self.send_event(name, "SessionEnd")
        self.assertNotEqual(self.binding(name)["activity"], "idle")
        self.hgs("pause", name, rc=1)
        self.t("has-session", "-t", "=" + name)

    def test_foreign_conversation_and_ungated_hooks_leave_binding_unchanged(self):
        name = self.start()
        before = self.binding(name)
        self.send_event(name, "Stop", session_id="foreign-conversation", last_assistant_message="wrong")
        self.hook({"hook_event_name": "SessionStart", "session_id": "foreign"}, self.env)
        self.assertEqual(self.binding(name), before)

    def test_nested_codex_exec_keeps_parent_activity_and_transcript(self):
        name = self.start("codex")
        before = self.binding(name)
        self.send_event(name, "UserPromptSubmit", prompt="Parent task")
        self.send_event(name, "PreToolUse", tool_name="Bash", tool_use_id="parent-tool")
        sid = str(uuid.uuid4())
        path = self.root / ("rollout-" + sid + ".jsonl")
        path.write_text(json.dumps(dict(type="session_meta", payload=dict(id=sid, source="exec"))) + "\n")
        self.send_event(name, "SessionStart", session_id=sid, transcript_path=str(path), cwd=str(self.root / "child"))
        self.send_event(name, "Stop", session_id=sid, last_assistant_message="Child result")
        after = self.binding(name)
        for key in ("conversation_id", "transcript", "cwd", "run_id"):
            self.assertEqual(after[key], before[key], key)
        self.assertEqual(after["prompt"], "Parent task")
        self.assertIn("parent-tool", after["active_tools"])
        child = after["subagents"]["external:codex:" + sid]
        self.assertEqual(child["state"], "finished")
        self.send_event(name, "PostToolUse", tool_name="Bash", tool_use_id="parent-tool")
        self.send_event(name, "Stop", last_assistant_message="Parent result")
        info = json.loads(self.hgs("inspect", name))
        self.assertEqual(info["conversation_id"], before["conversation_id"])
        self.assertEqual(info["last_message"], "Parent result")
        # Real new conversations keep working after the nested invocation.
        self.t("send-keys", "-t", "=" + name + ":", "switch", "Enter")
        self.wait(lambda: self.binding(name)["conversation_id"] != before["conversation_id"])

    def test_exit_without_reason_is_saved_and_exactly_resumable(self):
        name = self.start("codex")
        before = self.binding(name)
        self.t("send-keys", "-t", "=" + name + ":", "unknown-exit", "Enter")
        self.wait(lambda: subprocess.run([str(self.bin / "tmux"), "has-session", "-t", "=" + name],
                  env=self.env, capture_output=True).returncode != 0)
        saved = json.loads(self.hgs("ls", "--local", "--json"))["sessions"]
        self.assertEqual(len(saved), 1)
        self.assertEqual(saved[0]["state"], "stopped")
        self.assertTrue(saved[0]["resumable"])
        self.hgs("resume", name, "-d")
        self.wait(lambda: self.binding(name).get("run_id") != before["run_id"] and
                  self.binding(name).get("activity") == "idle")
        self.assertEqual(self.binding(name)["conversation_id"], before["conversation_id"])


@unittest.skipUnless(shutil.which("git"), "git is required")
@unittest.skipIf(LEGACY_STATE, "Git session metadata requires the native CLI")
class GitContext(Harness):
    def setUp(self):
        super().setUp()
        for key in list(self.env):
            if key.startswith("GIT_"):
                self.env.pop(key)
        self.env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
                        GIT_TERMINAL_PROMPT="0")
        self.git("init", "-b", "main", str(self.project))
        self.git("commit", "--allow-empty", "-m", "Private fixture commit", cwd=self.project)

    def git(self, *args, cwd=None):
        result = subprocess.run(["git", "-c", "core.hooksPath=" + os.devnull,
                                 "-c", "user.name=HGS fixture", "-c", "user.email=hgs@example.invalid",
                                 "-c", "commit.gpgSign=false", "-c", "maintenance.auto=false", *args],
                                cwd=cwd, env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout.strip()

    def row(self, name):
        return next(row for row in json.loads(self.hgs("ls", "--local", "--json"))["sessions"]
                    if row["name"] == name)

    def linked(self, branch="review"):
        directory = self.root / ("linked worktree " + branch)
        self.git("worktree", "add", "-b", branch, str(directory), "HEAD", cwd=self.project)
        return directory

    def assert_git(self, data, directory, branch, linked=False, detached=False):
        self.assertEqual(data["git_metadata_state"], "ok")
        self.assertEqual(Path(data["git_root"]).resolve(), directory.resolve())
        self.assertEqual(data["git_branch"], branch)
        self.assertEqual(data["git_worktree"], linked)
        self.assertEqual(data["git_detached"], detached)
        self.assertEqual(data["git_worktree_name"], directory.name if linked else "")

    def test_main_repository_metadata_survives_pause_and_archive(self):
        name = self.start()
        row = self.row(name)
        self.assert_git(row, self.project, "main")
        self.assertEqual(Path(row["cwd"]).resolve(), self.project.resolve())
        self.assertEqual(row["cwd_source"], "tmux")
        self.assert_git(json.loads(self.hgs("inspect", name)), self.project, "main")
        self.hgs("pause", name)
        saved = self.row(name)
        self.assert_git(saved, self.project, "main")
        self.assertEqual(saved["cwd_source"], "saved")
        self.assertEqual(saved["activity_summary"], "Paused")
        self.hgs("archive", name)
        archived = self.row(name)
        self.assertEqual(archived["state"], "archived")
        self.assertEqual(archived["activity_summary"], "Archived")
        self.assert_git(archived, self.project, "main")
        self.assert_git(json.loads(self.hgs("inspect", name, "--archive", archived["archive_id"])),
                        self.project, "main")

    def test_linked_worktree_is_distinct_from_main_repository(self):
        directory = self.linked()
        self.hgs("codex", str(directory), "-n", "work", "-d")
        name = "codex/" + directory.name + "/work"
        self.wait(lambda: self.binding(name).get("conversation_id"))
        self.assert_git(self.row(name), directory, "review", linked=True)
        self.assert_git(json.loads(self.hgs("inspect", name)), directory, "review", linked=True)
        main = self.start()
        self.assert_git(self.row(main), self.project, "main")

    def test_detached_worktree_has_no_invented_branch(self):
        directory = self.linked("detached")
        self.git("checkout", "--detach", "HEAD", cwd=directory)
        self.t("new-session", "-d", "-s", "sh/p/detached", "-c", str(directory), "sleep 60")
        row = self.row("sh/p/detached")
        self.assert_git(row, directory, "", linked=True, detached=True)
        self.assertFalse(row.get("tracked", False))
        self.assertEqual(row["cwd_source"], "tmux")

    def test_live_branch_change_refreshes_after_bounded_cache(self):
        name = self.start()
        before = self.binding(name)
        self.assert_git(self.row(name), self.project, "main")
        self.git("checkout", "-b", "renamed-branch", cwd=self.project)
        deadline = time.monotonic() + 20
        while True:
            row = self.row(name)
            if row.get("git_branch") == "renamed-branch" or time.monotonic() >= deadline:
                break
            time.sleep(0.2)
        self.assert_git(row, self.project, "renamed-branch")
        for key in ("pid", "run_id", "conversation_id"):
            self.assertEqual(self.binding(name)[key], before[key], key)

    def test_non_repository_and_untracked_terminal_metadata_are_explicit(self):
        directory = self.root / "plain"
        directory.mkdir()
        self.hgs("claude", str(directory), "-n", "one", "-d")
        name = "claude/plain/one"
        self.wait(lambda: self.binding(name).get("conversation_id"))
        row = self.row(name)
        self.assertEqual(row["git_metadata_state"], "not_repo")
        self.assertEqual(row["git_branch"], "")
        self.assertEqual(row["git_root"], "")
        self.assertFalse(row["git_worktree"])
        self.assertFalse(row["git_detached"])
        self.t("new-session", "-d", "-s", "sh/p/untracked", "-c", str(self.project), "sleep 60")
        untracked = self.row("sh/p/untracked")
        self.assert_git(untracked, self.project, "main")
        self.assertFalse(untracked.get("tracked", False))
        self.assertEqual(untracked["cwd_source"], "tmux")
        self.assertEqual(untracked.get("activity", "unknown"), "unknown")
        self.assertEqual(untracked["subagent_source"], "unavailable")


@unittest.skipIf(LEGACY_STATE, "compact activity summaries require the native CLI")
class ActivitySummary(Harness):
    def row(self, name):
        return next(row for row in json.loads(self.hgs("ls", "--local", "--json"))["sessions"]
                    if row["name"] == name)

    def preview(self, row, agent_id):
        return next(child for child in row["subagent_previews"] if child["id"] == agent_id)

    def counts(self, row, active, completed, total):
        self.assertEqual(row["subagent_active_count"], active)
        self.assertEqual(row["subagent_completed_count"], completed)
        self.assertEqual(row["subagent_total_count"], total)
        self.assertEqual(row["subagent_count"], active)

    def test_interrupted_workflow_children_without_stop_hooks_leave_active_roster(self):
        name = self.start()
        ids = ("spike-host", "spike-console", "screen-model")
        for child in ids:
            self.send_event(name, "SubagentStart", agent_id=child, agent_type="workflow-subagent")
            self.send_event(name, "PostToolUse", agent_id=child, tool_name="Bash")
        self.send_event(name, "Stop", last_assistant_message="Parent result")
        before = self.row(name)
        self.counts(before, 3, 0, 3)
        record = self.binding(name)
        session = record["conversation_id"]
        account = self.root / ".claude"
        transcript = account / "projects" / "fixture" / (session + ".jsonl")
        children = transcript.with_suffix("") / "subagents" / "workflows" / "wf-fixture"
        children.mkdir(parents=True)
        transcript.write_text("{}\n")
        at = datetime.now(timezone.utc).isoformat()
        for child in ids:
            row = dict(type="user", userType="external", isSidechain=True, agentId=child,
                       sessionId=session, timestamp=at,
                       message=dict(role="user", content=[dict(type="text", text="[Request interrupted by user]")]))
            (children / ("agent-" + child + ".jsonl")).write_text(json.dumps(row) + "\n")
        record.update(agent_home=str(account), transcript=str(transcript))
        state = self.root / "state" / (hashlib.sha256(name.encode()).hexdigest() + ".json")
        state.write_text(json.dumps(record))
        after = self.row(name)
        self.counts(after, 0, 3, 3)
        self.assertEqual(after["activity"], "idle")
        self.assertEqual(after["subagent_previews"], [])
        self.assertEqual(after["reply_id"], before["reply_id"])
        inspected = json.loads(self.hgs("inspect", name))
        self.assertEqual(inspected["reply_id"], before["reply_id"])
        self.assertEqual(set(inspected["subagents"]), set(ids))
        for child in ids:
            info = json.loads(self.hgs("inspect", name, "--agent", child))
            self.assertEqual(info["state"], "finished")
            self.assertEqual(info["history_scope"], "Native subagent history")
            self.assertTrue(any(e.get("detail") == "[Request interrupted by user]" for e in info["events"]))

    def test_tool_use_interruption_settles_workflow_child_and_context_actions(self):
        name = self.start()
        self.send_event(name, "SubagentStart", agent_id="worker", agent_type="workflow-subagent")
        self.send_event(name, "PostToolUse", agent_id="worker", tool_name="Bash")
        self.send_event(name, "Stop", last_assistant_message="Parent result")
        before = self.row(name)
        self.counts(before, 1, 0, 1)
        self.assertEqual(before["activity"], "busy")
        busy = json.loads(self.hgs("inspect", name))
        self.assertFalse(busy["clear_context_supported"])
        self.assertFalse(busy["compact_context_supported"])
        # Both actions name the child that keeps the session from Ready.
        waiting = "Wait until the agent reports Ready. Still working: subagent \u201cworkflow-subagent\u201d"
        self.assertEqual(busy["clear_context_reason"], waiting)
        self.assertEqual(busy["compact_context_reason"], waiting)
        record = self.binding(name)
        session = record["conversation_id"]
        account = self.root / ".claude"
        transcript = account / "projects" / "fixture" / (session + ".jsonl")
        child = transcript.with_suffix("") / "subagents" / "workflows" / "wf-fixture" / "agent-worker.jsonl"
        child.parent.mkdir(parents=True)
        transcript.write_text("{}\n")
        record.update(agent_home=str(account), transcript=str(transcript))
        state = self.root / "state" / (hashlib.sha256(name.encode()).hexdigest() + ".json")
        state.write_text(json.dumps(record))
        at = datetime.now(timezone.utc).isoformat()

        def row(kind, row_id, parent, message, **extra):
            return dict(parentUuid=parent, isSidechain=True, promptId="prompt-one", agentId="worker",
                        type=kind, uuid=row_id, timestamp=at, userType="external", sessionId=session,
                        version="2.1.296", message=message, **extra)
        # Claude 2.1.296 writes the stopped call's error result, then the marker.
        call = row("assistant", "native-call", "native-prompt",
                   dict(role="assistant", stop_reason="tool_use",
                        content=[dict(type="tool_use", id="toolu_call", name="Bash", input={})]))
        rejected = row("user", "rejected-result", "native-call",
                       dict(role="user", content=[dict(type="tool_result", tool_use_id="toolu_call", is_error=True,
                                                       content="The user doesn't want to proceed with this tool use.")]),
                       toolUseResult="User rejected tool use", toolDenialKind="user-rejected",
                       sourceToolAssistantUUID="native-call")
        marker = row("user", "interrupt-marker", "rejected-result",
                     dict(role="user", content=[dict(type="text", text="[Request interrupted by user for tool use]")]))
        # Without the stopped call's error result the marker settles nothing.
        child.write_text(json.dumps(call) + "\n" + json.dumps(marker) + "\n")
        unbound = self.row(name)
        self.counts(unbound, 1, 0, 1)
        self.assertEqual(unbound["activity"], "busy")
        child.write_text("".join(json.dumps(r) + "\n" for r in (call, rejected, marker)))
        after = self.row(name)
        self.counts(after, 0, 1, 1)
        self.assertEqual(after["activity"], "idle")
        self.assertEqual(after["reply_id"], before["reply_id"])
        info = json.loads(self.hgs("inspect", name, "--agent", "worker"))
        self.assertEqual(info["state"], "finished")
        self.assertEqual(info["tracking_evidence"]["kind"], "interrupt")
        self.assertEqual(info["tracking_evidence"]["native_id"], "interrupt-marker")
        idle = json.loads(self.hgs("inspect", name))
        self.assertEqual(idle["reply_id"], before["reply_id"])
        self.assertTrue(idle["clear_context_supported"])
        self.assertTrue(idle["compact_context_supported"])
        self.assertEqual((idle["clear_context_reason"], idle["compact_context_reason"]), ("", ""))

    def test_native_child_completion_and_resume_reconcile_missing_hooks(self):
        name = self.start()
        self.send_event(name, "SubagentStart", agent_id="worker", agent_type="workflow-subagent")
        self.send_event(name, "Stop", last_assistant_message="Parent result")
        record = self.binding(name)
        session = record["conversation_id"]
        transcript = self.root / ".claude" / "projects" / "fixture" / (session + ".jsonl")
        child = transcript.with_suffix("") / "subagents" / "workflows" / "wf-fixture" / "agent-worker.jsonl"
        child.parent.mkdir(parents=True)
        transcript.write_text("{}\n")
        record.update(agent_home=str(self.root / ".claude"), transcript=str(transcript))
        state = self.root / "state" / (hashlib.sha256(name.encode()).hexdigest() + ".json")
        state.write_text(json.dumps(record))
        native = dict(type="assistant", isSidechain=True, agentId="worker", sessionId=session,
                      timestamp=datetime.now(timezone.utc).isoformat(), uuid="native-finish",
                      message=dict(role="assistant", stop_reason="end_turn", content=[dict(type="text", text="Checks passed.")]))
        child.write_text(json.dumps(native) + "\n")
        finished = self.row(name)
        self.counts(finished, 0, 1, 1)
        self.assertEqual(finished["activity"], "idle")
        self.assertEqual(finished["subagents"]["worker"]["tracking_evidence"]["kind"], "end_turn")
        # A later native tool starts new child work even if both hooks are absent.
        native.update(timestamp=datetime.now(timezone.utc).isoformat(), uuid="native-resume",
                      message=dict(role="assistant", stop_reason="tool_use",
                                   content=[dict(type="tool_use", id="next-call", name="Read", input={})]))
        child.write_text(json.dumps(native) + "\n")
        resumed = self.row(name)
        self.counts(resumed, 1, 0, 1)
        self.assertEqual(resumed["activity"], "busy")
        self.assertEqual(resumed["reply_id"], finished["reply_id"])
        evidence = resumed["subagents"]["worker"]["tracking_evidence"]
        self.assertEqual(evidence["source"], "claude_transcript")
        self.assertEqual(evidence["kind"], "tool_use")
        # A newer real hook is authoritative, rather than the cached native row.
        self.send_event(name, "SubagentStop", agent_id="worker", last_assistant_message="Final result")
        current = self.row(name)
        self.counts(current, 0, 1, 1)
        self.assertEqual(current["subagents"]["worker"]["tracking_evidence"]["source"], "hook")

    def test_codex_native_child_turns_reconcile_missing_hooks(self):
        name = self.start("codex")
        child = str(uuid.uuid4())
        self.send_event(name, "SubagentStart", agent_id=child, agent_type="Review")
        self.send_event(name, "Stop", last_assistant_message="Parent result")
        before = self.row(name)
        record = self.binding(name)
        account = self.root / ".codex"
        rollout = account / "child.jsonl"
        with sqlite3.connect(account / "state_5.sqlite") as db:
            db.execute("CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT, cwd TEXT)")
            db.execute("INSERT INTO threads VALUES(?,?,?)", (child, str(rollout), str(self.project)))
        header = dict(type="session_meta", payload=dict(id=child, parent_thread_id=record["conversation_id"]))
        def write(kind, **extra):
            event = dict(type="event_msg", timestamp=datetime.now(timezone.utc).isoformat(),
                         payload=dict(type=kind, turn_id="child-turn", **extra))
            rollout.write_text(json.dumps(header) + "\n" + json.dumps(event) + "\n")
        write("task_complete", last_agent_message="Verified child result")
        finished = self.row(name)
        self.counts(finished, 0, 1, 1)
        self.assertEqual(finished["activity"], "idle")
        self.assertEqual(finished["reply_id"], before["reply_id"])
        info = json.loads(self.hgs("inspect", name, "--agent", child))
        self.assertEqual(info["state"], "finished")
        self.assertEqual(info["tracking_evidence"]["source"], "codex_native_rollout")
        write("task_started")
        self.counts(self.row(name), 1, 0, 1)
        self.assertEqual(self.row(name)["activity"], "busy")
        write("turn_aborted", reason="interrupted")
        self.counts(self.row(name), 0, 1, 1)
        # A transcript for a different parent cannot complete a newer hook turn.
        self.send_event(name, "SubagentStart", agent_id=child, agent_type="Review")
        header["payload"]["parent_thread_id"] = str(uuid.uuid4())
        write("task_complete", last_agent_message="Foreign result")
        self.counts(self.row(name), 1, 0, 1)

    def test_kimi_native_catalog_separates_same_profile_children(self):
        name = self.start("kimi")
        for task in ("First task", "Second task"):
            self.send_event(name, "SubagentStart", agent_name="code", prompt=task)
        self.send_event(name, "Stop", last_assistant_message="Parent result")
        before = self.row(name)
        record = self.binding(name)
        session = self.root / ".kimi-code/sessions/workspace" / record["conversation_id"]
        agents = {}
        for child in ("main", "agent-0", "agent-1"):
            directory = session / "agents" / child
            directory.mkdir(parents=True)
            (directory / "wire.jsonl").write_text("{}\n")
            agents[child] = dict(homedir=str(directory))
            if child != "main":
                agents[child].update(parentAgentId="main", labels=dict(profileName="code"))
        state = dict(id=record["conversation_id"], version=2, updatedAt=time.time() * 1000, agents=agents)
        (session / "state.json").write_text(json.dumps(state))
        def write(child, kind, **extra):
            event = dict(type=kind, agentId=child, time=time.time() * 1000, turnId=0, **extra)
            (session / "agents" / child / "wire.jsonl").write_text(json.dumps(event) + "\n")
        write("agent-0", "turn.ended", reason="completed")
        write("agent-1", "agent.turn.started")
        working = self.row(name)
        self.counts(working, 1, 1, 2)
        self.assertEqual(working["subagent_source"], "kimi_native")
        self.assertEqual(set(working["subagents"]), {"agent-0", "agent-1"})
        self.assertEqual(working["activity"], "busy")
        write("agent-1", "agent.turn.ended", outcome="aborted")
        finished = self.row(name)
        self.counts(finished, 0, 2, 2)
        self.assertEqual(finished["activity"], "idle")
        self.assertEqual(finished["reply_id"], before["reply_id"])
        info = json.loads(self.hgs("inspect", name, "--agent", "agent-1"))
        self.assertEqual(info["tracking_evidence"]["kind"], "agent.turn.ended")
        self.assertEqual(info["state"], "finished")
        write("agent-0", "agent.turn.started")
        self.counts(self.row(name), 1, 1, 2)
        self.hgs("pause", name, rc=1)
        # A fresh profile hook can beat a delayed native catalog, but old
        # profile starts cannot then masquerade as complete individual counts.
        self.send_event(name, "SubagentStart", agent_name="code", prompt="New task")
        delayed = self.row(name)
        self.assertEqual(delayed["subagent_source"], "hook_profiles")
        self.assertFalse(delayed["subagent_counts_complete"])

    def test_child_progress_previews_and_finished_details_preserve_hook_evidence(self):
        name = self.start()
        initial = self.row(name)
        self.assertEqual(initial["subagent_source"], "hooks")
        self.counts(initial, 0, 0, 0)
        self.send_event(name, "SubagentStart", agent_id="reviewer", agent_type="Review",
                        prompt="Review the changed files")
        self.send_event(name, "SubagentStart", agent_id="tests", agent_type="Test runner",
                        description="Check the regression suite")
        self.send_event(name, "PreToolUse", agent_id="reviewer", tool_name="Bash",
                        tool_use_id="review-tool", tool_input={"command": "git diff --stat"})
        working = self.row(name)
        self.counts(working, 2, 0, 2)
        child = self.preview(working, "reviewer")
        self.assertEqual(child["label"], "Review")
        self.assertEqual(child["state"], "working")
        self.assertEqual(child["current_tool"], "Bash")
        self.assertIn("git diff --stat", child["detail"])
        self.assertEqual(working["current_tool"], "", "a child tool is not the main agent's tool")
        self.send_event(name, "SubagentStop", agent_id="reviewer")
        partial = self.row(name)
        self.counts(partial, 1, 1, 2)
        self.assertNotIn("reviewer", [child["id"] for child in partial["subagent_previews"]])
        done = json.loads(self.hgs("inspect", name))["subagents"]["reviewer"]
        self.assertEqual(done["state"], "finished")
        self.assertEqual(done["current_tool"], "")
        self.assertIn("git diff --stat", done["detail"])
        self.send_event(name, "Stop", last_assistant_message="Waiting for the final worker")
        self.assertEqual(self.binding(name)["activity"], "busy")
        self.hgs("pause", name, rc=1)
        self.send_event(name, "SubagentStop", agent_id="tests")
        final = self.row(name)
        self.counts(final, 0, 2, 2)
        self.assertEqual(final["activity"], "idle")
        self.assertEqual(final["subagent_previews"], [])
        inspected = json.loads(self.hgs("inspect", name))
        self.assertIn("Check the regression suite", inspected["subagents"]["tests"]["detail"])
        self.counts(inspected, 0, 2, 2)
        self.assertEqual(inspected["subagent_previews"], final["subagent_previews"])

    def test_failed_claude_child_settles_with_its_error(self):
        # Claude sends no SubagentStop after a child's StopFailure, such as a
        # usage limit. The child ends as an error and never keeps the parent busy.
        name = self.start()
        self.send_event(name, "SubagentStart", agent_id="worker", agent_type="Research")
        self.send_event(name, "StopFailure", agent_id="worker", error="rate_limit",
                        error_message="You've hit your weekly limit")
        row = self.row(name)
        self.counts(row, 0, 1, 1)
        self.assertEqual(row["subagent_previews"], [])
        child = json.loads(self.hgs("inspect", name))["subagents"]["worker"]
        self.assertEqual((child["state"], child["display_state"]), ("finished", "error"))
        self.assertIn("weekly limit", child["detail"])
        self.send_event(name, "Stop", last_assistant_message="The worker hit its limit")
        self.assertEqual(self.binding(name)["activity"], "idle")
        self.assertEqual(self.row(name)["activity"], "idle")
        # A resumed child is current work again.
        self.send_event(name, "PreToolUse", agent_id="worker", tool_name="Bash", tool_use_id="later")
        self.counts(self.row(name), 1, 0, 1)
        self.send_event(name, "SubagentStop", agent_id="worker")
        self.counts(self.row(name), 0, 1, 1)
        self.assertNotIn("display_state", self.binding(name)["subagents"]["worker"])

    def test_unknown_child_status_is_not_reported_as_completed(self):
        name = self.start("codex")
        self.send_event(name, "SubagentStart", agent_id="worker", agent_type="Research")
        self.send_event(name, "Interrupt", agent_id="worker", error_message="Provider disconnected")
        row = self.row(name)
        child = self.preview(row, "worker")
        self.assertEqual(child["state"], "unknown")
        self.assertIn("Provider disconnected", child["detail"])
        self.assertEqual(row["subagent_completed_count"], 0)
        self.assertEqual(row["subagent_total_count"], 1)
        # Unknown is not current work: the finished parent is Ready.
        self.send_event(name, "Stop", last_assistant_message="Done")
        self.assertEqual(self.binding(name)["activity"], "idle")
        self.send_event(name, "SubagentStop", agent_id="worker")
        self.counts(self.row(name), 0, 1, 1)

    def test_current_activity_does_not_keep_finished_main_tool(self):
        name = self.start()
        prompt = "Validate changes and preserve the full task details. " * 20
        self.send_event(name, "UserPromptSubmit", prompt=prompt)
        self.send_event(name, "PreToolUse", tool_name="Bash", tool_use_id="tool",
                        tool_input={"command": "pytest private-fixtures"})
        working = self.row(name)
        self.assertEqual(working["current_tool"], "Bash")
        self.assertIn("pytest private-fixtures", working["tool_detail"])
        self.assertTrue(working["activity_summary"])
        self.assertIn("pytest private-fixtures", working["activity_detail"])
        self.send_event(name, "PermissionRequest", tool_name="Bash")
        approval = self.row(name)
        self.assertIn("approval", approval["activity_summary"].lower())
        self.assertEqual(approval["current_tool"], "Bash")
        self.assertIn("pytest private-fixtures", approval["activity_detail"])
        self.send_event(name, "Stop", last_assistant_message="Checks completed")
        final = self.row(name)
        self.assertEqual(final["activity"], "idle")
        self.assertEqual(final["current_tool"], "")
        self.assertEqual(final["tool_detail"], "")
        self.assertNotIn("pytest private-fixtures", final["activity_summary"] + final["activity_detail"])
        self.assertEqual(final["activity_detail"], "")
        inspected = json.loads(self.hgs("inspect", name))
        self.assertEqual(inspected["last_message"], "Checks completed")
        self.assertEqual(inspected["prompt"], prompt)

    def test_missing_hook_evidence_keeps_subagent_progress_unknown(self):
        record, _ = self.saved_v1()
        row = self.row(record["name"])
        self.assertEqual(row["subagent_source"], "unavailable")
        for key in ("subagent_active_count", "subagent_completed_count", "subagent_total_count"):
            self.assertIsNone(row[key], key)
        self.assertEqual(row["subagent_previews"], [])
        self.assertEqual(row["activity"], "unknown")

    def test_kimi_same_profile_concurrency_is_grouped_and_blocks_early_pause(self):
        name = self.start("kimi")
        for task in ("First task", "Second task", "Third task"):
            self.send_event(name, "SubagentStart", agent_name="code", prompt=task)
        started = self.row(name)
        self.assertEqual(started["subagent_source"], "hook_profiles")
        self.assertTrue(started["subagent_counts_complete"])
        self.counts(started, 3, 0, 3)
        self.assertEqual(len(started["subagent_previews"]), 1,
                         "a profile name cannot invent individual worker identities")
        group = self.preview(started, "profile:code")
        self.assertTrue(group["group"])
        self.assertEqual(group["label"], "code")
        self.assertEqual((group["active_count"], group["completed_count"], group["total_count"]), (3, 0, 3))
        self.send_event(name, "Stop", last_assistant_message="Waiting for all workers")

        def context_actions(waiting):
            # Clear and compaction wait for the profile groups, which the
            # session lists by profile, not as one invented child.
            info = json.loads(self.hgs("inspect", name))
            self.assertEqual(info["activity"], "busy")
            reason = "Wait until the agent reports Ready. Still working: " + waiting
            for action in ("clear_context", "compact_context"):
                self.assertFalse(info[action + "_supported"], action)
                self.assertEqual(info[action + "_reason"], reason, action)
        context_actions("3 \u201ccode\u201d subagents")
        for completed in (1, 2, 3):
            self.send_event(name, "SubagentStop", agent_name="code", response=f"Task {completed} complete")
            row = self.row(name)
            self.counts(row, 3 - completed, completed, 3)
            if completed < 3:
                self.assertIn("Third task", self.preview(row, "profile:code")["detail"])
                self.assertEqual(row["activity"], "busy")
                self.hgs("pause", name, rc=1)
                self.t("has-session", "-t", "=" + name)
                # The first profile Stop already records the session idle;
                # only the remaining groups keep it Working.
                self.assertEqual(self.binding(name)["activity"], "idle")
                context_actions("1 \u201ccode\u201d subagent" if completed == 2 else "2 \u201ccode\u201d subagents")
            else:
                self.assertEqual(row["subagent_previews"], [])
        self.assertEqual(self.binding(name)["activity"], "idle")
        inspected = json.loads(self.hgs("inspect", name))
        self.assertTrue(inspected["subagent_groups_complete"])
        self.assertEqual(inspected["subagent_groups"]["code"]["completed_count"], 3)
        self.assertEqual(inspected["activity"], "idle")
        for action in ("clear_context", "compact_context"):
            self.assertTrue(inspected[action + "_supported"], action)
            self.assertEqual(inspected[action + "_reason"], "", action)
        self.hgs("pause", name)

    def test_legacy_kimi_collapsed_profile_does_not_claim_one_unique_worker(self):
        record, path = self.saved_v1("kimi")
        record["subagents"] = {"code": {"name": "code", "state": "working", "detail": "Legacy profile"}}
        record["last_event_at"] = 1700000010
        path.write_text(json.dumps(record) + "\n")
        row = self.row(record["name"])
        self.assertFalse(row["subagent_counts_complete"])
        for key in ("subagent_active_count", "subagent_completed_count", "subagent_total_count"):
            self.assertIsNone(row[key], key)


class Recipes(Harness):
    def recipe(self, agent, base):
        record, path = self.saved_v1(agent, base)
        before = path.read_bytes()
        output = self.hgs("resume", record["name"], "--dry-run")
        self.assertEqual(path.read_bytes(), before, "dry-run changed the saved binding")
        tmux_command = shlex.split(output.strip())
        self.assertEqual(tmux_command[:3], ["tmux", "new-session", "-d"])
        runner = shlex.split(tmux_command[-1])
        expected = ('exec env HGS_AGENT_HINT=1 HGS_AGENT_HINT_OFF= "$0" --run "$@"'
                    if agent in ('claude', 'codex') else 'exec "$0" --run "$@"')
        self.assertEqual(runner[2], expected)
        self.assertEqual(runner[4], "0")
        return runner[5:], record["conversation_id"]

    def test_explicit_selectors_replaced(self):
        command, sid = self.recipe("claude", ["wrapper", "--resume=old", "--model", "opus", "hello"])
        self.assertEqual(command, ["wrapper", "--resume", sid, "--model", "opus"])
        command, sid = self.recipe("kimi", ["kimi", "-S", "old", "--plan"])
        self.assertEqual(command, ["kimi", "--session", sid, "--plan"])
        command, sid = self.recipe("codex", ["codex", "-c", "x=1", "resume", "--last"])
        self.assertEqual(command, ["codex", "resume", sid, "-c", "x=1"])

    def test_unsupported_switch_is_not_guessed(self):
        for agent, base in (("claude", ["claude", "--unknown", "value"]),
                            ("codex", ["codex", "exec", "hello"])):
            with self.subTest(agent=agent):
                record, path = self.saved_v1(agent, base)
                before = path.read_bytes()
                self.hgs("resume", record["name"], "--dry-run", rc=1)
                self.assertEqual(path.read_bytes(), before)


@unittest.skipIf(LEGACY_STATE, "dead unconfirmed launch attempts can be retried by the native CLI")
class FailedStartup(Harness):
    def setUp(self):
        super().setUp()
        self.gate = self.root / "launch-gate"
        self.ready = self.root / "wrapper-ready"
        self.launcher = self.project / "run_codex.sh"
        # A project launcher may prompt, unlock secrets or exit. It records every
        # run with its identity and never special-cases a capability probe.
        self.launcher.write_text("#!" + sys.executable + r'''
import json, os, pathlib, signal, sys
home = pathlib.Path.home()
with open(home / "launcher-runs.jsonl", "a") as log:
    log.write(json.dumps({"argv": sys.argv[1:], "run_id": os.environ.get("HGS_RUN_ID")}) + "\n")
gate = home / "launch-gate"
if gate.exists():
    status = gate.read_text()
    (home / "wrapper-ready").write_text(str(os.getpid()))
    if status == "wait":
        signal.pause()
    sys.exit(int(status))
os.execvp("codex", ["codex", *sys.argv[1:]])
''')
        self.launcher.chmod(0o755)

    def launch(self, *extra):
        output = self.hgs("codex", "-n", "dashboard", "-d", *extra, cwd=self.project)
        return output.strip().removeprefix("hgs: started ")

    def wait_failed(self, name, previous_run=None):
        self.wait(lambda: self.binding(name).get("exited_at") and
                  self.binding(name).get("run_id") != previous_run)
        self.wait(lambda: subprocess.run([str(self.bin / "tmux"), "has-session", "-t", "=" + name],
                  env=self.env, capture_output=True).returncode != 0)
        return self.binding(name)

    def attempts(self):
        return [json.loads(path.read_text()) for path in (self.root / "state/attempts").glob("*.json")]

    def assert_launcher_runs(self, *runs):
        """The launcher ran once per supervised run, never to detect capabilities."""
        log = self.root / "launcher-runs.jsonl"
        calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
        self.assertEqual([call["run_id"] for call in calls], list(runs))
        for call in calls:
            self.assertNotIn("--help", call["argv"])
            self.assertEqual(call["argv"].count("--no-daemon"), 1)

    def assert_unconfirmed_not_saved(self, name):
        self.assertIsNone(self.binding(name)["conversation_id"])
        self.assertFalse((self.root / "argv.jsonl").exists(), "wrapper failed before the agent started")
        self.assertFalse((self.root / "history").exists(), "failed startup cannot invent agent history")
        rows = json.loads(self.hgs("ls", "--local", "--json"))["sessions"]
        self.assertFalse([row for row in rows if row["name"] == name])
        self.hgs("resume", name, "-d", rc=1)

    def test_capability_detection_never_runs_the_launcher(self):
        # Without the probe the launcher's Codex is unknown: it gets the flag that
        # reviewed releases require. --bare still detects the native Codex.
        (self.root / "old-codex").touch()
        name = self.launch()
        self.wait(lambda: self.binding(name).get("conversation_id"))
        self.assert_launcher_runs(self.binding(name)["run_id"])
        bare = self.hgs("codex", "-n", "bare", "--bare", "-d", cwd=self.project).strip().removeprefix("hgs: started ")
        self.wait(lambda: self.binding(bare).get("conversation_id"))
        self.assertEqual(self.binding(bare)["base"], ["codex"])
        launches = [json.loads(line)["argv"] for line in (self.root / "argv.jsonl").read_text().splitlines()]
        self.assertEqual(len(launches), 2)
        self.assertEqual(launches[0].count("--no-daemon"), 1)
        self.assertNotIn("--no-daemon", launches[1])
        self.assert_launcher_runs(self.binding(name)["run_id"])

    def test_cancelled_wrapper_retries_same_command_without_fresh(self):
        self.gate.write_text("wait")
        name = self.launch()
        self.wait(self.ready.exists)
        self.t("send-keys", "-t", "=" + name + ":", "C-c")
        failed = self.wait_failed(name)
        self.assert_unconfirmed_not_saved(name)
        self.assertIn(failed.get("exit_code"), (None, 130))
        before = self.binding(name)
        self.hgs("__state", "exists", name, rc=1)
        self.hgs("codex", "-n", "dashboard", "--dry-run", cwd=self.project)
        self.assertEqual(self.binding(name), before)
        self.assertEqual(self.attempts(), [])
        self.gate.unlink()
        self.assertEqual(self.launch(), name)
        self.wait(lambda: self.binding(name).get("conversation_id"))
        self.assertNotEqual(self.binding(name)["run_id"], failed["run_id"])
        preserved = self.attempts()
        self.assertEqual(len(preserved), 1)
        for key, value in failed.items():
            if key in ("updated", "exited_at") and isinstance(value, float):
                self.assertAlmostEqual(preserved[0][key], value, delta=1e-6)
            else:
                self.assertEqual(preserved[0][key], value)
        self.assertEqual(preserved[0]["attempt_reason"], "replaced_unconfirmed_launch")
        self.assertEqual((self.root / "state/attempts").stat().st_mode & 0o777, 0o700)
        self.assertTrue(all(path.stat().st_mode & 0o777 == 0o600 for path in (self.root / "state/attempts").glob("*.json")))
        confirmed = self.binding(name)
        self.hgs("pause", name)
        self.hgs("codex", "-n", "dashboard", "-d", cwd=self.project)
        self.wait(lambda: self.binding(name).get("run_id") != confirmed["run_id"] and
                  self.binding(name).get("activity") == "idle")
        self.assertEqual(self.binding(name)["conversation_id"], confirmed["conversation_id"])
        self.assert_launcher_runs(failed["run_id"], confirmed["run_id"], self.binding(name)["run_id"])

    def test_new_can_retry_a_failed_unconfirmed_name(self):
        self.gate.write_text("7")
        name = self.launch("--new")
        failed = self.wait_failed(name)
        self.assert_unconfirmed_not_saved(name)
        self.gate.unlink()
        self.assertEqual(self.launch("--new"), name)
        self.wait(lambda: self.binding(name).get("conversation_id"))
        self.assertEqual([record["run_id"] for record in self.attempts()], [failed["run_id"]])
        self.assert_launcher_runs(failed["run_id"], self.binding(name)["run_id"])

    def test_failed_wrapper_can_retry_requested_id_under_same_name(self):
        self.gate.write_text("7")
        name = self.launch()
        failed = self.wait_failed(name)
        self.gate.unlink()
        requested = str(uuid.uuid4())
        self.hgs("codex", "-n", "dashboard", "-c", requested, "-d", cwd=self.project)
        self.wait(lambda: self.binding(name).get("conversation_id") == requested and
                  self.binding(name).get("activity") == "idle")
        self.assertEqual([record["run_id"] for record in self.attempts()], [failed["run_id"]])
        self.assert_launcher_runs(failed["run_id"], self.binding(name)["run_id"])
        argv = json.loads((self.root / "launcher-runs.jsonl").read_text().splitlines()[-1])["argv"]
        self.assertEqual(argv[argv.index("resume"):argv.index("resume") + 2], ["resume", requested])

    def test_repeated_failed_attempts_are_preserved_separately(self):
        failed = []
        for code in (7, 130):
            self.gate.write_text(str(code))
            name = self.launch()
            failed.append(self.wait_failed(name, failed[-1]["run_id"] if failed else None))
            self.assert_unconfirmed_not_saved(name)
        self.gate.unlink()
        self.launch()
        self.wait(lambda: self.binding(name).get("conversation_id"))
        attempts = self.attempts()
        self.assertEqual({record["run_id"] for record in attempts}, {record["run_id"] for record in failed})
        self.assertEqual({record["exit_code"] for record in attempts}, {7, 130})
        self.assertEqual(len({record["attempt_id"] for record in attempts}), 2)
        self.assertEqual(len(list((self.root / "history").glob("*.jsonl"))), 1)
        self.assert_launcher_runs(*[record["run_id"] for record in failed], self.binding(name)["run_id"])

    def test_living_unconfirmed_wrapper_is_not_replaced(self):
        self.gate.write_text("wait")
        name = self.launch()
        self.wait(self.ready.exists)
        # Under load the supervisor records the wrapper's own process shortly
        # after it starts; compare against that settled binding.
        self.wait(lambda: str(self.binding(name).get("pid")) == self.ready.read_text())
        before = self.binding(name)
        self.hgs("codex", "-n", "dashboard", "-d", cwd=self.project)
        self.hgs("codex", "-n", "dashboard", "--new", "-d", cwd=self.project, rc=1)
        self.assertEqual(self.binding(name), before)
        self.assertEqual(self.attempts(), [])
        self.t("has-session", "-t", "=" + name)
        self.assert_launcher_runs(before["run_id"])

    def test_living_orphan_unconfirmed_child_is_not_replaced(self):
        self.gate.write_text("7")
        name = self.launch()
        failed = self.wait_failed(name)
        orphan = subprocess.Popen(["sleep", "60"], env=self.env)
        try:
            started = subprocess.run(["ps", "-p", str(orphan.pid), "-o", "lstart="],
                        env=dict(self.env, LC_ALL="C", TZ="UTC"), capture_output=True, text=True, check=True).stdout.strip()
            failed.update(pid=orphan.pid, process_start=started)
            path = self.root / "state" / (hashlib.sha256(name.encode()).hexdigest() + ".json")
            path.write_text(json.dumps(failed) + "\n")
            self.hgs("codex", "-n", "dashboard", "-d", cwd=self.project, rc=1)
            self.hgs("codex", "-n", "dashboard", "--new", "-d", cwd=self.project, rc=1)
            self.assertEqual(self.binding(name), failed)
            self.assertIsNone(orphan.poll())
            self.assertEqual(self.attempts(), [])
            self.assert_launcher_runs(failed["run_id"])
        finally:
            orphan.terminate()
            orphan.wait(timeout=5)


@unittest.skipIf(LEGACY_STATE, "explicit conversation IDs use native provider resume with confirmation")
class RequestedConversation(Harness):
    def test_exact_id_starts_each_agent_without_attaching_project_default(self):
        for agent in ("claude", "codex", "kimi"):
            with self.subTest(agent=agent):
                self.hgs(agent, "p", "-d")
                default = f"{agent}/p"
                self.wait(lambda: self.binding(default).get("conversation_id"))
                original = self.binding(default)
                requested = str(uuid.uuid4())
                name = f"{agent}/p/resume-{requested}"
                self.hgs(agent, "p", "-c", requested, "-d")
                self.wait(lambda: self.binding(name).get("conversation_id") == requested and
                          self.binding(name).get("activity") == "idle")
                self.assertEqual(self.binding(default), original)
                first = self.binding(name)
                self.hgs(agent, "p", "--resume", requested, "-d")
                self.assertEqual(self.binding(name), first)
                self.assertFalse(first.get("expected_id"))

    def test_explicit_requested_launch_failure_never_falls_back_to_fresh(self):
        requested = str(uuid.uuid4())
        name = f"codex/p/resume-{requested}"
        log = self.root / "failed-native-launches"
        self.script("codex", "#!/bin/sh\nif [ \"$1\" = --help ]; then echo --no-daemon; exit 0; fi\nprintf '%s\\n' \"$*\" >> " + shlex.quote(str(log)) + "\nexit 7\n")
        self.hgs("codex", "p", "--continue", requested, "-d")
        self.wait(lambda: self.binding(name).get("exit_code") == 7)
        self.assertEqual(len(log.read_text().splitlines()), 1)
        self.assertIn("resume " + requested, log.read_text())
        self.assertFalse((self.root / "history").exists())
        self.assertEqual(self.binding(name).get("expected_id"), requested)
        self.assertIsNone(self.binding(name).get("conversation_id"))
        self.script("codex", AGENT)
        self.hgs("codex", "p", "-c", requested, "-d")
        self.wait(lambda: self.binding(name).get("conversation_id") == requested and
                  self.binding(name).get("activity") == "idle")
        self.assertFalse(self.binding(name).get("expected_id"))

    def test_wrong_requested_id_confirmation_does_not_rebind_or_fallback(self):
        requested = str(uuid.uuid4())
        name = f"codex/p/resume-{requested}"
        (self.root / "ignore-resume").touch()
        self.hgs("codex", "p", "--resume=" + requested, "-d")
        self.wait(lambda: self.binding(name).get("error"))
        record = self.binding(name)
        self.assertEqual(record.get("expected_id"), requested)
        self.assertIsNone(record.get("conversation_id"))
        self.assertEqual(len((self.root / "argv.jsonl").read_text().splitlines()), 1)
        self.hgs("pause", name, rc=1)
        self.t("has-session", "-t", "=" + name)

    def test_exact_id_reuses_existing_named_saved_binding(self):
        name = self.start("codex", "dashboard")
        before = self.binding(name)
        requested = before["conversation_id"]
        self.hgs("pause", name)
        self.hgs("codex", "p", "-c", requested, "-d")
        self.wait(lambda: self.binding(name).get("run_id") != before["run_id"] and
                  self.binding(name).get("activity") == "idle")
        self.assertEqual(self.binding(name)["conversation_id"], requested)
        rows = json.loads(self.hgs("ls", "--local", "--json"))["sessions"]
        self.assertEqual([row["name"] for row in rows], [name])

    def test_requested_id_never_overwrites_different_named_live_or_saved_binding(self):
        name = self.start("codex", "dashboard")
        requested = str(uuid.uuid4())
        for state in ("live", "saved"):
            with self.subTest(state=state):
                if state == "saved":
                    self.hgs("pause", name)
                before = self.binding(name)
                self.hgs("codex", "p", "-n", "dashboard", "-c", requested, "-d", rc=1)
                self.assertEqual(self.binding(name), before)


@unittest.skipIf(LEGACY_STATE, "archive records are supported by the native lifecycle model")
class Archive(Harness):
    def rows(self, name=None):
        rows = json.loads(self.hgs("ls", "--local", "--json"))["sessions"]
        return [row for row in rows if name is None or row["name"] == name]

    def archived(self, name=None):
        return [row for row in self.rows(name) if row.get("state") == "archived"]

    def archive_info(self, name, archive_id, *extra):
        info = json.loads(self.hgs("inspect", name, "--archive", archive_id, *extra))
        # These fields come from current native history, not the saved archive.
        # Deleting that history may change its availability but not the archive.
        for key in ("goal", "goal_observed_at", "goal_source", "provider_messages", "provider_messages_error", "session_usage"):
            info.pop(key, None)
        return info

    def wait_stopped(self, name):
        self.wait(lambda: subprocess.run([str(self.bin / "tmux"), "has-session", "-t", "=" + name],
                  env=self.env, capture_output=True).returncode != 0)

    def save_archive(self, agent="claude", tag="one"):
        name = self.start(agent, tag)
        record = self.binding(name)
        self.hgs("pause", name)
        self.hgs("archive", name)
        entries = self.archived(name)
        self.assertEqual(len(entries), 1)
        return name, record, entries[0]

    def test_terminate_archives_busy_agents_and_preserves_resume(self):
        for agent in ("claude", "codex", "kimi"):
            with self.subTest(agent=agent):
                name = self.start(agent)
                self.t("send-keys", "-t", "=" + name + ":", "busy", "Enter")
                self.wait(lambda: self.binding(name).get("activity") == "busy")
                before = self.binding(name)
                self.hgs("terminate", name, "--expected-run-id", before["run_id"])
                self.wait_stopped(name)
                rows = self.archived(name)
                self.assertEqual(len(rows), 1)
                info = self.archive_info(name, rows[0]["archive_id"])
                self.assertEqual(info["conversation_id"], before["conversation_id"])
                self.assertEqual(info["completion_reason"], "user_terminated")
                self.assertTrue(rows[0]["resumable"])
                self.assertTrue(info["events"])
                self.hgs("resume", name, "--archive", rows[0]["archive_id"], "-d")
                self.wait(lambda: self.binding(name).get("activity") == "idle")
                self.assertEqual(self.binding(name)["conversation_id"], before["conversation_id"])

    def test_terminate_dry_run_does_not_change_binding_or_process(self):
        name = self.start("codex")
        before = self.binding(name)
        output = self.hgs("terminate", name, "--dry-run")
        self.assertIn("would terminate and archive", output)
        self.t("has-session", "-t", "=" + name)
        self.assertEqual(self.binding(name), before)
        self.assertEqual(self.archived(name), [])

    def test_terminate_rejects_stale_run_and_replaced_terminal(self):
        name = self.start("codex")
        before = self.binding(name)
        self.hgs("terminate", name, "--expected-run-id", "stale", rc=1)
        self.t("set-option", "-p", "-t", before["pane"], "@hgs_run", "replacement")
        self.hgs("terminate", name, rc=1)
        self.t("has-session", "-t", "=" + name)
        self.assertEqual(self.binding(name), before)
        self.assertEqual(self.archived(name), [])

    def test_terminate_failure_keeps_binding_and_running_terminal(self):
        name = self.start("kimi")
        before = self.binding(name)
        self.script("tmux", '#!/bin/sh\nif [ "$1" = kill-session ]; then echo "cannot stop" >&2; exit 1; fi\nexec '
                    + shlex.quote(TMUX) + ' -S ' + shlex.quote(self.socket) + ' -f /dev/null "$@"\n')
        self.hgs("terminate", name, rc=1)
        self.t("has-session", "-t", "=" + name)
        self.assertEqual(self.binding(name), before)
        self.assertEqual(self.archived(name), [])

    def test_terminate_saved_binding_and_missing_native_history(self):
        name = self.start("codex")
        before = self.binding(name)
        self.hgs("pause", name)
        Path(before["transcript"]).unlink()
        self.hgs("terminate", name)
        rows = self.archived(name)
        self.assertEqual(len(rows), 1)
        info = self.archive_info(name, rows[0]["archive_id"])
        self.assertEqual(info["conversation_id"], before["conversation_id"])
        self.hgs("resume", name, "--archive", rows[0]["archive_id"], "-d", rc=1)
        self.assertFalse(self.binding(name))
        self.assertEqual(info["completion_reason"], "user_terminated")

    def test_terminate_archive_write_failure_keeps_saved_binding(self):
        name = self.start("codex")
        before = self.binding(name)
        blocked = self.root / "state" / "archives"
        blocked.write_text("blocked archive directory")
        self.hgs("terminate", name, rc=1)
        self.wait_stopped(name)
        self.assertEqual(self.binding(name)["conversation_id"], before["conversation_id"])
        self.assertTrue(Path(before["transcript"]).exists())
        blocked.unlink()
        self.hgs("terminate", name)
        self.assertEqual(len(self.archived(name)), 1)

    def test_explicit_kill_still_forgets_catalog_entry(self):
        name = self.start("codex")
        transcript = Path(self.binding(name)["transcript"])
        self.hgs("kill", name)
        self.wait_stopped(name)
        self.assertEqual(self.rows(name), [])
        self.assertTrue(transcript.exists())

    def test_explicit_user_exit_archives_each_agent(self):
        for agent in ("claude", "codex", "kimi"):
            for command in ("/exit", "/quit"):
                with self.subTest(agent=agent, command=command):
                    name = self.start(agent, command.removeprefix("/"))
                    before = self.binding(name)
                    self.t("send-keys", "-t", "=" + name + ":", command, "Enter")
                    self.wait_stopped(name)
                    entries = self.rows(name)
                    self.assertEqual(len(entries), 1)
                    self.assertEqual(entries[0]["state"], "archived")
                    archived = self.archive_info(name, entries[0]["archive_id"])
                    self.assertEqual(archived["conversation_id"], before["conversation_id"])
                    self.assertFalse(self.binding(name))

    def test_explicit_end_does_not_archive_live_process_or_failed_exit(self):
        for agent, reason in (("claude", "prompt_input_exit"), ("codex", "other"), ("kimi", "exit")):
            with self.subTest(agent=agent):
                name = self.start(agent)
                before = self.binding(name)
                self.send_event(name, "SessionEnd", reason=reason)
                self.assertEqual(self.archived(name), [])
                self.t("has-session", "-t", "=" + name)
                self.t("send-keys", "-t", "=" + name + ":", "crash-after-end", "Enter")
                self.wait_stopped(name)
                self.assertEqual(self.archived(name), [])
                self.assertEqual(self.rows(name)[0]["state"], "stopped")
                self.assertEqual(self.binding(name)["conversation_id"], before["conversation_id"])

    def test_pause_and_agent_signal_remain_saved(self):
        for agent in ("claude", "codex", "kimi"):
            with self.subTest(agent=agent):
                trace = DirectSignalTrace(self.root, agent) if sys.platform == "linux" else None
                if trace:
                    self.script(agent, direct_signal_agent())
                try:
                    name = self.start(agent)
                    sid = self.binding(name)["conversation_id"]
                    self.hgs("pause", name)
                    self.assertEqual(self.archived(name), [])
                    self.assertEqual(self.rows(name)[0]["state"], "paused")
                    self.resume(name)
                    record = self.binding(name)
                    before = trace.capture("before_SIGTERM", record) if trace else None
                    try:
                        os.kill(record["pid"], signal.SIGTERM)
                    except Exception as error:
                        if trace:
                            trace.capture("SIGTERM_raised_" + type(error).__name__, record, before)
                        raise
                    if trace:
                        trace.capture("after_SIGTERM_returned", record, before)
                    self.wait_stopped(name)
                    self.assertEqual(self.archived(name), [])
                    self.assertEqual(self.rows(name)[0]["state"], "stopped")
                    self.assertEqual(self.binding(name)["conversation_id"], sid)
                except Exception as error:
                    if trace:
                        error.add_note(trace.failure())
                    raise

    def test_supervisor_signal_reaches_real_child_without_archiving(self):
        name = self.start("kimi")
        before = self.binding(name)
        child = before["pid"]
        supervisor = int(subprocess.run(["ps", "-p", str(child), "-o", "ppid="],
                         text=True, capture_output=True, check=True).stdout.strip())
        pane_pid = int(self.t("display-message", "-p", "-t", "=" + name + ":", "#{pane_pid}").strip())
        self.assertEqual(supervisor, pane_pid, "the tmux pane must supervise the tracked child")
        self.assertNotEqual(child, supervisor)
        os.kill(supervisor, signal.SIGTERM)
        self.wait_stopped(name)
        self.wait(lambda: subprocess.run(["ps", "-p", str(child), "-o", "pid="],
                  capture_output=True).returncode != 0)
        self.assertEqual(self.archived(name), [])
        self.assertEqual(self.rows(name)[0]["state"], "stopped")
        self.assertEqual(self.binding(name)["conversation_id"], before["conversation_id"])

    def test_terminal_ctrl_c_reaches_agent_once_and_keeps_supervisor_alive(self):
        (self.root / "record-interrupts").touch()
        name = self.start("codex")
        before = self.binding(name)
        log = self.root / ("interrupts-" + before["conversation_id"])
        for count in range(1, 4):
            self.t("send-keys", "-t", "=" + name + ":", "C-c")
            self.wait(lambda: log.exists() and len(log.read_text().splitlines()) >= count)
            time.sleep(0.05)  # Allow any erroneous supervisor forwarding to arrive.
            self.assertEqual(len(log.read_text().splitlines()), count)
        self.t("has-session", "-t", "=" + name)
        self.assertEqual(self.binding(name)["run_id"], before["run_id"])
        self.assertEqual(self.archived(name), [])

    def test_tmux_loss_and_crash_do_not_archive_context(self):
        crashed = self.start("claude")
        interrupted = self.start("codex")
        ids = {name: self.binding(name)["conversation_id"] for name in (crashed, interrupted)}
        self.t("send-keys", "-t", "=" + crashed + ":", "crash", "Enter")
        self.wait_stopped(crashed)
        self.t("kill-server")
        self.wait_stopped(interrupted)
        self.assertEqual(self.archived(), [])
        self.assertEqual({row["state"] for row in self.rows()}, {"stopped"})
        self.hgs("resume", "--all")
        for name, sid in ids.items():
            self.wait(lambda: self.binding(name).get("activity") == "idle")
            self.assertEqual(self.binding(name)["conversation_id"], sid)

    def test_legacy_ended_binding_without_exit_evidence_remains_saved(self):
        record, path = self.saved_v1()
        record.pop("paused")
        path.write_text(json.dumps(record) + "\n")
        rows = self.rows(record["name"])
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["state"], "stopped")
        self.assertEqual(self.archived(), [])
        self.hgs("resume", record["name"], "-d")
        self.wait(lambda: self.binding(record["name"]).get("activity") == "idle")
        self.assertEqual(self.binding(record["name"])["conversation_id"], record["conversation_id"])

    def test_subagent_exit_is_not_evidence_of_parent_user_exit(self):
        name = self.start()
        sid = self.binding(name)["conversation_id"]
        self.send_event(name, "SessionEnd", reason="prompt_input_exit", agent_id="child")
        self.t("send-keys", "-t", "=" + name + ":", "quiet-exit", "Enter")
        self.wait_stopped(name)
        self.assertEqual(self.archived(name), [])
        self.assertEqual(self.rows(name)[0]["state"], "stopped")
        self.assertEqual(self.binding(name)["conversation_id"], sid)

    def test_manual_archive_preserves_context_and_journal(self):
        name = self.start()
        self.send_event(name, "UserPromptSubmit", prompt="Remember this archived task")
        self.send_event(name, "Stop", last_assistant_message="Saved answer")
        before = self.binding(name)
        self.hgs("archive", name, rc=1)
        self.t("has-session", "-t", "=" + name)
        self.assertEqual(self.archived(name), [])
        self.hgs("pause", name)
        paused = self.binding(name)
        self.hgs("archive", name, "--dry-run")
        self.assertEqual(self.binding(name), paused)
        self.assertEqual(self.archived(name), [])
        self.hgs("archive", name)
        entries = self.rows(name)
        self.assertEqual(len(entries), 1)
        archived = entries[0]
        self.assertEqual(archived["state"], "archived")
        self.assertTrue(archived["archive_id"])
        self.assertGreater(archived["archived_at"], 1700000000)
        info = self.archive_info(name, archived["archive_id"])
        self.assertEqual(info["conversation_id"], before["conversation_id"])
        self.assertEqual(info["prompt"], "Remember this archived task")
        self.assertEqual(info["last_message"], "Saved answer")
        self.assertIn("Remember this archived task", [event["detail"] for event in info["events"]])
        self.assertEqual(self.archive_info(name, archived["archive_id"], "--after", str(info["cursor"]))["events"], [])
        self.assertFalse(self.binding(name), "archived records must not remain an automatic resume binding")
        self.hgs("kill", name, "--archive", archived["archive_id"], "--dry-run")
        self.assertInspectionEqual(self.archive_info(name, archived["archive_id"]), info)

    def test_name_reuse_keeps_both_archives_and_their_conversations(self):
        name, old, first = self.save_archive()
        self.start()
        new = self.binding(name)
        self.assertNotEqual(new["conversation_id"], old["conversation_id"])
        self.assertEqual(self.archive_info(name, first["archive_id"])["conversation_id"], old["conversation_id"])
        self.assertEqual(json.loads(self.hgs("inspect", name))["conversation_id"], new["conversation_id"])
        self.hgs("pause", name)
        self.hgs("archive", name)
        entries = self.archived(name)
        self.assertEqual(len(entries), 2)
        self.assertEqual(len({entry["archive_id"] for entry in entries}), 2)
        self.assertEqual({self.archive_info(name, entry["archive_id"])["conversation_id"] for entry in entries},
                         {old["conversation_id"], new["conversation_id"]})
        self.hgs("kill", name, "--archive", first["archive_id"])
        remaining = self.archived(name)
        self.assertEqual(len(remaining), 1)
        self.assertEqual(self.archive_info(name, remaining[0]["archive_id"])["conversation_id"], new["conversation_id"])

    def test_resume_all_ignores_archives_and_resumes_saved(self):
        archived_name, _, archived = self.save_archive(tag="archive")
        saved_name = self.start(tag="saved")
        saved = self.binding(saved_name)
        self.hgs("pause", saved_name)
        self.hgs("resume", "--all")
        self.wait(lambda: self.binding(saved_name).get("run_id") != saved["run_id"] and
                  self.binding(saved_name).get("activity") == "idle")
        self.assertEqual(self.binding(saved_name)["conversation_id"], saved["conversation_id"])
        self.assertFalse(self.binding(archived_name))
        self.assertEqual([entry["archive_id"] for entry in self.archived(archived_name)], [archived["archive_id"]])
        self.assertNotEqual(subprocess.run([str(self.bin / "tmux"), "has-session", "-t", "=" + archived_name],
                                          env=self.env, capture_output=True).returncode, 0)

    def test_explicit_archive_restore_uses_exact_id_for_every_agent(self):
        for agent in ("claude", "codex", "kimi"):
            with self.subTest(agent=agent):
                name, old, archived = self.save_archive(agent)
                archive_id = archived["archive_id"]
                self.hgs("resume", name, "-d", rc=1)
                before = self.archive_info(name, archive_id)
                dry = self.hgs("resume", name, "--archive", archive_id, "--dry-run")
                self.assertIn(old["conversation_id"], dry)
                self.assertInspectionEqual(self.archive_info(name, archive_id), before)
                self.assertFalse(self.binding(name))
                self.hgs("resume", name, "--archive", archive_id, "-d")
                self.wait(lambda: self.binding(name).get("conversation_id") == old["conversation_id"] and
                          self.binding(name).get("activity") == "idle")
                self.assertNotEqual(self.binding(name)["run_id"], old["run_id"])
                self.assertEqual(self.archived(name), [])

    def test_archive_restore_refuses_live_and_saved_name_collisions(self):
        name, _, archived = self.save_archive()
        self.start()
        live = self.binding(name)
        self.hgs("resume", name, "--archive", archived["archive_id"], "-d", rc=1)
        self.assertEqual(self.binding(name), live)
        self.t("has-session", "-t", "=" + name)
        self.hgs("pause", name)
        saved = self.binding(name)
        self.hgs("resume", name, "--archive", archived["archive_id"], "-d", rc=1)
        self.assertEqual(self.binding(name), saved)
        self.assertEqual([entry["archive_id"] for entry in self.archived(name)], [archived["archive_id"]])

    def test_missing_history_and_wrong_name_cannot_destroy_archive(self):
        name, old, archived = self.save_archive()
        archive_id = archived["archive_id"]
        before = self.archive_info(name, archive_id)
        self.hgs("resume", "claude/p/other", "--archive", archive_id, "-d", rc=1)
        self.hgs("kill", "claude/p/other", "--archive", archive_id, rc=1)
        self.assertInspectionEqual(self.archive_info(name, archive_id), before)
        Path(old["transcript"]).unlink()
        without_history = self.archive_info(name, archive_id)
        self.assertEqual(without_history["processes"]["notes"], ["Native process history is unavailable"])
        self.hgs("resume", name, "--archive", archive_id, "-d", rc=1)
        self.assertInspectionEqual(self.archive_info(name, archive_id), without_history)
        expected = json.loads(json.dumps(before))
        expected["processes"]["notes"] = without_history["processes"]["notes"]
        self.assertInspectionEqual(without_history, expected)
        self.assertFalse(self.binding(name))

    def test_new_session_can_reuse_a_name_that_exists_only_in_archive(self):
        name, old, archived = self.save_archive()
        self.hgs("claude", "p", "--new", "-n", "one", "-d")
        self.wait(lambda: self.binding(name).get("conversation_id"))
        self.assertNotEqual(self.binding(name)["conversation_id"], old["conversation_id"])
        self.assertEqual(self.archive_info(name, archived["archive_id"])["conversation_id"], old["conversation_id"])

    def test_failed_restore_keeps_archive_until_exact_id_is_confirmed(self):
        name, old, archived = self.save_archive()
        self.script("claude", "#!/bin/sh\nexit 7\n")
        self.hgs("resume", name, "--archive", archived["archive_id"], "-d")
        self.wait_stopped(name)
        self.assertEqual(self.archive_info(name, archived["archive_id"])["conversation_id"], old["conversation_id"])
        self.assertFalse(self.binding(name), "failed restore must not leave a binding that blocks retry")
        self.script("claude", AGENT)
        self.hgs("resume", name, "--archive", archived["archive_id"], "-d")
        self.wait(lambda: self.binding(name).get("conversation_id") == old["conversation_id"] and
                  self.binding(name).get("activity") == "idle")
        self.assertEqual(self.binding(name)["conversation_id"], old["conversation_id"])
        self.assertEqual(self.archived(name), [])

    def test_stale_exit_hook_cannot_archive_new_run(self):
        name, old, archived = self.save_archive()
        self.start()
        before = self.binding(name)
        self.hook({"hook_event_name": "SessionEnd", "session_id": old["conversation_id"],
                   "reason": "prompt_input_exit"},
                  dict(self.env, HGS_SESSION=name, HGS_RUN_ID=old["run_id"]))
        self.assertEqual(self.binding(name), before)
        self.assertEqual([row["archive_id"] for row in self.archived(name)], [archived["archive_id"]])
        self.t("has-session", "-t", "=" + name)

    def test_wrong_conversation_confirmation_preserves_archive_and_allows_retry(self):
        name, old, archived = self.save_archive()
        marker = self.root / "ignore-resume"
        marker.touch()
        self.hgs("resume", name, "--archive", archived["archive_id"], "-d")
        self.wait(lambda: self.binding(name).get("error"))
        self.assertEqual(self.binding(name)["conversation_id"], old["conversation_id"])
        self.assertEqual(self.archive_info(name, archived["archive_id"])["conversation_id"], old["conversation_id"])
        self.t("has-session", "-t", "=" + name)
        self.t("send-keys", "-t", "=" + name + ":", "quiet-exit", "Enter")
        self.wait_stopped(name)
        self.assertFalse(self.binding(name))
        marker.unlink()
        self.hgs("resume", name, "--archive", archived["archive_id"], "-d")
        self.wait(lambda: self.binding(name).get("conversation_id") == old["conversation_id"] and
                  self.binding(name).get("activity") == "idle")
        self.assertEqual(self.archived(name), [])

    def test_malformed_archive_ids_never_target_a_live_session(self):
        name, _, archived = self.save_archive()
        self.start()
        before = self.binding(name)
        for archive_id in ("../escape", "/absolute", "", "not-an-archive-id"):
            with self.subTest(archive_id=archive_id):
                self.hgs("kill", name, "--archive", archive_id, rc=1)
                self.hgs("resume", name, "--archive", archive_id, "-d", rc=1)
                self.hgs("inspect", name, "--archive", archive_id, rc=1)
                self.assertEqual(self.binding(name), before)
                self.t("has-session", "-t", "=" + name)
        self.assertEqual([row["archive_id"] for row in self.archived(name)], [archived["archive_id"]])


@unittest.skipIf(LEGACY_STATE, "session rename requires the native CLI")
class Rename(Harness):
    def rows(self):
        return json.loads(self.hgs("ls", "--local", "--json"))["sessions"]

    def pane_identity(self, name):
        return self.t("display-message", "-p", "-t", "=" + name + ":",
                      "#{session_id} #{pane_id} #{pane_pid} #{@hgs_run}").strip()

    def inherited_hook(self, old, kind, **fields):
        event = dict(session_id=old["conversation_id"], hook_event_name=kind)
        event.update(fields)
        return self.hook(event, dict(self.env, HGS_SESSION=old["name"], HGS_RUN_ID=old["run_id"]))

    def interrupted_transaction(self, old_name, new_name):
        """A durable checkpoint left behind if the rename process exits early."""
        session_id = self.t("display-message", "-p", "-t", "=" + old_name + ":",
                            "#{session_id}").strip()
        transaction = {"version": 1, "old": old_name, "new": new_name,
                       "record": self.binding(old_name), "session_id": session_id,
                       "legacy": False, "source_routed": False}
        folder = self.root / "state/renames"
        folder.mkdir(exist_ok=True)
        path = folder / (str(uuid.uuid4()) + ".json")
        path.write_text(json.dumps(transaction) + "\n")
        return path, session_id

    def test_interrupted_rename_before_tmux_change_keeps_original_binding(self):
        old_name = self.start()
        new_name = "claude/p/recovered"
        before = self.binding(old_name)
        pane = self.pane_identity(old_name)
        pending, _ = self.interrupted_transaction(old_name, new_name)
        recovered = json.loads(self.hgs("inspect", old_name))
        self.assertEqual(recovered["conversation_id"], before["conversation_id"])
        self.assertEqual(self.binding(old_name), before)
        self.assertEqual(self.pane_identity(old_name), pane)
        self.assertFalse(self.binding(new_name))
        self.assertFalse(pending.exists())
        self.send_event(old_name, "Stop", last_assistant_message="Original run is still reachable")
        self.assertEqual(self.binding(old_name)["last_message"], "Original run is still reachable")

    def test_interrupted_rename_after_tmux_change_recovers_identity_and_journal(self):
        old_name = self.start()
        new_name = "claude/p/recovered"
        self.send_event(old_name, "UserPromptSubmit", prompt="Survive interrupted rename")
        self.send_event(old_name, "Stop")
        before = self.binding(old_name)
        journal = json.loads(self.hgs("inspect", old_name))
        pane = self.pane_identity(old_name)
        pending, session_id = self.interrupted_transaction(old_name, new_name)
        self.t("rename-session", "-t", session_id, new_name)
        recovered = json.loads(self.hgs("inspect", new_name))
        self.assertEqual(recovered["events"], journal["events"])
        self.assertEqual(recovered["cursor"], journal["cursor"])
        for key in ("run_id", "pid", "process_start", "conversation_id", "transcript"):
            self.assertEqual(self.binding(new_name)[key], before[key], key)
        self.assertEqual(self.pane_identity(new_name), pane)
        self.assertFalse(self.binding(old_name))
        self.assertFalse(pending.exists())
        self.inherited_hook(before, "Stop", last_assistant_message="Recovered run is reachable")
        self.assertEqual(self.binding(new_name)["last_message"], "Recovered run is reachable")
        self.hgs("pause", new_name)
        self.resume(new_name)
        self.assertEqual(self.binding(new_name)["conversation_id"], before["conversation_id"])

    def test_live_rename_preserves_identity_history_hooks_and_resume(self):
        old_name = self.start()
        self.send_event(old_name, "UserPromptSubmit", prompt="Keep this conversation")
        self.send_event(old_name, "Stop", last_assistant_message="Ready to rename")
        old = self.binding(old_name)
        pane = self.pane_identity(old_name)
        transcript = Path(old["transcript"]).read_bytes()
        before = json.loads(self.hgs("inspect", old_name))
        new_name = "claude/p/review"
        self.hgs("rename", old_name, new_name)
        self.assertFalse(self.binding(old_name))
        renamed = self.binding(new_name)
        for key in ("pid", "process_start", "supervisor", "run_id", "conversation_id",
                    "transcript", "base", "cwd", "launch_dir", "agent_home"):
            self.assertEqual(renamed.get(key), old.get(key), key)
        self.assertEqual(self.pane_identity(new_name), pane)
        self.assertEqual(Path(old["transcript"]).read_bytes(), transcript)
        info = json.loads(self.hgs("inspect", new_name))
        self.assertEqual(info["events"][:len(before["events"])], before["events"])
        self.inherited_hook(old, "UserPromptSubmit", prompt="Hook from the original environment")
        self.assertEqual(self.binding(new_name)["activity"], "busy")
        self.hgs("pause", new_name, rc=1)
        self.inherited_hook(old, "Stop", last_assistant_message="Renamed and idle")
        after = json.loads(self.hgs("inspect", new_name, "--after", str(before["cursor"])))
        self.assertIn("Hook from the original environment", [e["detail"] for e in after["events"]])
        self.hgs("pause", new_name)
        self.assertTrue(self.binding(new_name)["paused"])
        self.assertFalse(self.binding(old_name))
        self.resume(new_name)
        self.assertEqual(self.binding(new_name)["conversation_id"], old["conversation_id"])
        self.assertEqual(json.loads(self.hgs("inspect", new_name))["last_message"], "Renamed and idle")

    def test_old_name_reuse_cannot_capture_renamed_run_hooks_or_exit(self):
        old_name = self.start()
        old = self.binding(old_name)
        new_name = "claude/p/renamed"
        self.hgs("rename", old_name, new_name)
        self.start()
        replacement = self.binding(old_name)
        self.inherited_hook(old, "UserPromptSubmit", prompt="Only the renamed run")
        self.assertEqual(self.binding(new_name)["prompt"], "Only the renamed run")
        self.assertEqual(self.binding(old_name), replacement)
        self.inherited_hook(old, "Stop")
        # The real child still has the old HGS_SESSION in its inherited environment.
        self.t("send-keys", "-t", "=" + new_name + ":", "/exit", "Enter")
        self.wait(lambda: any(row["name"] == new_name and row.get("state") == "archived"
                              for row in self.rows()))
        archived = next(row for row in self.rows() if row["name"] == new_name)
        info = json.loads(self.hgs("inspect", new_name, "--archive", archived["archive_id"]))
        self.assertEqual(info["conversation_id"], old["conversation_id"])
        self.assertEqual(self.binding(old_name), replacement)
        self.t("has-session", "-t", "=" + old_name)
        self.inherited_hook(old, "Stop", last_assistant_message="Delayed obsolete hook")
        self.assertEqual(self.binding(old_name), replacement)
        self.assertFalse(self.binding(new_name))

    def test_pending_requested_id_rename_confirms_through_original_environment(self):
        requested = str(uuid.uuid4())
        old_name, new_name = "codex/p/pending", "codex/p/review"
        gate = self.root / "rename-gate"
        gate.touch()
        launcher = self.project / "run_codex.sh"
        launcher.write_text("#!" + sys.executable + r'''
import os, pathlib, sys, time
home = pathlib.Path.home()
(home / "rename-ready").touch()
while (home / "rename-gate").exists():
    time.sleep(0.01)
os.execvp("codex", ["codex", *sys.argv[1:]])
''')
        launcher.chmod(0o755)
        self.hgs("codex", "p", "-n", "pending", "-c", requested, "-d")
        self.wait(lambda: (self.root / "rename-ready").exists())
        old = self.binding(old_name)
        self.assertEqual(old["expected_id"], requested)
        self.assertIsNone(old["conversation_id"])
        self.assertFalse((self.root / "history").exists())
        pane = self.pane_identity(old_name)
        self.hgs("rename", old_name, new_name)
        self.assertEqual(self.pane_identity(new_name), pane)
        self.assertEqual(self.binding(new_name)["run_id"], old["run_id"])
        self.assertEqual(self.binding(new_name)["expected_id"], requested)
        self.hgs("codex", "p", "-c", requested, "-d")
        self.assertEqual(self.pane_identity(new_name), pane)
        self.assertEqual(self.binding(new_name)["run_id"], old["run_id"])
        self.assertEqual([row["name"] for row in self.rows()], [new_name])
        gate.unlink()
        self.wait(lambda: self.binding(new_name).get("conversation_id") == requested)
        self.assertFalse(self.binding(new_name).get("expected_id"))
        self.assertFalse(self.binding(old_name))
        self.hgs("pause", new_name)
        self.resume(new_name)
        self.assertEqual(self.binding(new_name)["conversation_id"], requested)

    def test_untracked_live_session_rename_keeps_terminal_process(self):
        old_name, new_name = "codex/p/untracked", "codex/p/manual"
        self.t("new-session", "-d", "-s", old_name, "sleep 60")
        pane = self.pane_identity(old_name)
        self.hgs("rename", old_name, new_name)
        self.assertEqual(self.pane_identity(new_name), pane)
        self.assertFalse(self.binding(new_name))
        self.assertFalse(json.loads(self.hgs("inspect", new_name))["tracked"])
        self.assertEqual([row["name"] for row in self.rows()], [new_name])
        self.hgs("pause", new_name, rc=1)

    @unittest.skipIf(LEGACY_STATE, "fix introduced by the native lifecycle model")
    def test_session_renamed_between_terminal_queries_lists_once_under_its_new_name(self):
        # A listing reads the sessions, then their panes. A session renamed in
        # between keeps its terminal identity: it lists once, under its new
        # name, as its later listings do.
        old_name, new_name = self.start(), "claude/p/renamed"
        plain_old, plain_new = "codex/p/untracked", "codex/p/manual"
        self.t("new-session", "-d", "-s", plain_old, "sleep 60")
        listed = self.root / "rename-after-listing"
        tmux = f"{shlex.quote(TMUX)} -S {shlex.quote(self.socket)} -f /dev/null"
        self.script("tmux", f'''#!/bin/sh
{tmux} "$@"; status=$?
if [ "$1" = list-sessions ] && rm {shlex.quote(str(listed))} 2>/dev/null; then
    {shlex.quote(str(HGS))} rename {shlex.quote(old_name)} {shlex.quote(new_name)} >/dev/null || exit 99
    {shlex.quote(str(HGS))} rename {shlex.quote(plain_old)} {shlex.quote(plain_new)} >/dev/null || exit 99
fi
exit $status
''')
        listed.touch()
        sessions = json.loads(self.hgs("ls", "--local", "--json"))["sessions"]
        self.assertFalse(listed.exists())
        self.assertFalse(self.binding(old_name))
        renamed = self.binding(new_name)
        self.assertTrue(renamed.get("conversation_id"))
        self.assertEqual(sorted(s["name"] for s in sessions), [new_name, plain_new])
        after = {row["name"]: row for row in self.rows()}
        keys = ("name", "cmd", "project", "tag", "attached", "clients", "created", "launch_id",
                "state", "resumable", "tracked", "run_id", "conversation_id")
        for session in sessions:
            self.assertEqual({key: session.get(key) for key in keys},
                             {key: after[session["name"]].get(key) for key in keys})
        tracked = next(s for s in sessions if s["name"] == new_name)
        self.assertEqual((tracked["tag"], tracked["run_id"], tracked["conversation_id"], tracked["resumable"]),
                         ("renamed", renamed["run_id"], renamed["conversation_id"], True))
        self.assertFalse(next(s for s in sessions if s["name"] == plain_new).get("conversation_id"))

    @unittest.skipIf(LEGACY_STATE, "fix introduced by the native lifecycle model")
    def test_session_renamed_to_a_name_freed_between_terminal_queries_lists_once(self):
        # The listed session that ended must not list again under the name
        # that the renamed one now holds.
        ended, moved = "codex/p/first", "codex/p/second"
        launches = {name: str(uuid.uuid4()) for name in (ended, moved)}
        for name in (ended, moved):
            self.t("new-session", "-d", "-s", name, "-e", "HGS_LAUNCH_ID=" + launches[name], "sleep 60")
        self.assertEqual({row["name"]: row["launch_id"] for row in self.rows()}, launches)
        listed = self.root / "reuse-after-listing"
        tmux = f"{shlex.quote(TMUX)} -S {shlex.quote(self.socket)} -f /dev/null"
        self.script("tmux", f'''#!/bin/sh
{tmux} "$@"; status=$?
if [ "$1" = list-sessions ] && rm {shlex.quote(str(listed))} 2>/dev/null; then
    {tmux} kill-session -t {shlex.quote("=" + ended)} || exit 99
    {shlex.quote(str(HGS))} rename {shlex.quote(moved)} {shlex.quote(ended)} >/dev/null || exit 99
fi
exit $status
''')
        listed.touch()
        sessions = json.loads(self.hgs("ls", "--local", "--json"))["sessions"]
        self.assertFalse(listed.exists())
        self.assertEqual([(s["name"], s["launch_id"]) for s in sessions], [(ended, launches[moved])])

    @unittest.skipIf(LEGACY_STATE, "fix introduced by the native lifecycle model")
    def test_rename_committing_after_the_listing_reads_the_panes_lists_once(self):
        # A rename renames the terminal session, then moves its binding. One
        # that renames the terminal right after the listing has read the panes
        # and moves the binding only after the listing has finished must
        # still list the session once, with its saved state (zerus-gd02).
        old_name, new_name = self.start(), "claude/p/renamed"
        saved = self.binding(old_name)
        armed, snapshot, renamed, release, status, trace = (
            self.root / name for name in ("rename-armed", "panes-read", "terminal-renamed",
                                          "rename-release", "rename-status", "listing-trace"))
        def q(path):
            return shlex.quote(str(path))
        tmux =f"{shlex.quote(TMUX)} -S {shlex.quote(self.socket)} -f /dev/null"
        # Only the listing sets LISTING_PROBE. Its first terminal query after
        # its first pane snapshot starts the rename and answers once the
        # terminal session has its new name. The rename then holds its binding
        # move, under the state lock, until the listing has finished.
        self.script("tmux", f'''#!/bin/sh
if [ -n "$LISTING_PROBE" ]; then
    echo "$1" >>{q(trace)}
    if [ -e {q(snapshot)} ] && rm {q(armed)} 2>/dev/null; then
        (env -u LISTING_PROBE {shlex.quote(str(HGS))} rename {shlex.quote(old_name)} {shlex.quote(new_name)}
         echo $? >{q(status)}) </dev/null >/dev/null 2>&1 &
        tries=0
        until [ -e {q(renamed)} ] || [ -e {q(status)} ] || [ $tries -ge 1000 ]; do
            tries=$((tries + 1)); sleep 0.01
        done
    fi
    {tmux} "$@"; status=$?
    case "$1 $3 $4" in "list-panes -F "*session_id*) touch {q(snapshot)} ;; esac
    exit $status
fi
if [ "$1" = rename-session ]; then
    {tmux} "$@"; status=$?
    touch {q(renamed)}
    tries=0
    until [ -e {q(release)} ] || [ $tries -ge 1500 ]; do
        tries=$((tries + 1)); sleep 0.01
    done
    exit $status
fi
exec {tmux} "$@"
''')
        armed.touch()
        try:
            listing = subprocess.run([str(HGS), "ls", "--local", "--json"],
                                     env=dict(self.env, LISTING_PROBE="1"),
                                     capture_output=True, text=True, timeout=20)
        finally:
            release.touch()
            if not armed.exists():
                self.wait(status.exists)
        self.assertEqual(listing.returncode, 0, listing.stdout + listing.stderr)
        self.assertFalse(armed.exists(), trace.read_text())
        self.assertEqual(status.read_text().strip(), "0")
        self.assertFalse(self.binding(old_name))
        self.assertEqual(self.binding(new_name)["run_id"], saved["run_id"])
        sessions = json.loads(listing.stdout)["sessions"]
        self.assertEqual(len(sessions), 1, [(s["name"], s.get("state")) for s in sessions])
        session = sessions[0]
        self.assertIn(session["name"], (old_name, new_name))
        self.assertNotIn("state", session)
        self.assertEqual((session["run_id"], session["conversation_id"], session["resumable"]),
                         (saved["run_id"], saved["conversation_id"], True))
        self.assertEqual([row["name"] for row in self.rows()], [new_name])

    def rename_during_folder_query(self, old_name, new_name, *command):
        """Run `hgs *command` while `hgs rename old_name new_name` lands
        between its pane snapshot and its pane folder query.

        The command's folder query starts the rename and runs once the
        terminal session has its new name. The rename then holds its binding
        move until the command has finished. Returns the command's result."""
        armed, renamed, release, status = (
            self.root / name for name in ("rename-armed", "terminal-renamed",
                                          "rename-release", "rename-status"))
        def q(path):
            return shlex.quote(str(path))
        tmux = f"{shlex.quote(TMUX)} -S {shlex.quote(self.socket)} -f /dev/null"
        # Only the command under test sets FOLDER_PROBE.
        self.script("tmux", f'''#!/bin/sh
if [ -n "$FOLDER_PROBE" ]; then
    case "$*" in *pane_current_path*)
        if rm {q(armed)} 2>/dev/null; then
            (env -u FOLDER_PROBE {shlex.quote(str(HGS))} rename {shlex.quote(old_name)} {shlex.quote(new_name)}
             echo $? >{q(status)}) </dev/null >/dev/null 2>&1 &
            tries=0
            until [ -e {q(renamed)} ] || [ -e {q(status)} ] || [ $tries -ge 1000 ]; do
                tries=$((tries + 1)); sleep 0.01
            done
        fi ;;
    esac
    exec {tmux} "$@"
fi
if [ "$1" = rename-session ]; then
    {tmux} "$@"; status=$?
    touch {q(renamed)}
    tries=0
    until [ -e {q(release)} ] || [ $tries -ge 1500 ]; do
        tries=$((tries + 1)); sleep 0.01
    done
    exit $status
fi
exec {tmux} "$@"
''')
        armed.touch()
        try:
            result = subprocess.run([str(HGS), *command],
                                    env=dict(self.env, FOLDER_PROBE="1"),
                                    capture_output=True, text=True, timeout=20)
        finally:
            release.touch()
            if not armed.exists():
                self.wait(status.exists)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(armed.exists())
        self.assertTrue(renamed.exists())
        self.assertEqual(status.read_text().strip(), "0")
        return result

    @unittest.skipIf(LEGACY_STATE, "fix introduced by the native lifecycle model")
    def test_rename_before_the_listing_reads_pane_folders_keeps_the_live_folder(self):
        # A listing reads its pane snapshot under the state lock and asks for
        # the panes' current folders after releasing it. A session renamed in
        # between lists under the name of its snapshot, still with the live
        # folder of its pane rather than its saved one (zerus-18u1).
        old_name, new_name = self.start(), "claude/p/renamed"
        listing = self.rename_during_folder_query(old_name, new_name, "ls", "--local", "--json")
        sessions = json.loads(listing.stdout)["sessions"]
        self.assertEqual([s["name"] for s in sessions], [old_name])
        self.assertEqual(sessions[0]["cwd_source"], "tmux")
        self.assertEqual(Path(sessions[0]["cwd"]).resolve(), self.project.resolve())
        after = self.rows()
        self.assertEqual([(row["name"], row["cwd_source"]) for row in after], [(new_name, "tmux")])

    @unittest.skipIf(LEGACY_STATE, "fix introduced by the native lifecycle model")
    def test_rename_before_an_inspection_reads_pane_folders_keeps_the_live_folder(self):
        # An inspection releases the state lock before it asks for the panes'
        # current folders, as a listing does (zerus-byyw).
        old_name, new_name = self.start(), "claude/p/renamed"
        inspected = json.loads(self.rename_during_folder_query(old_name, new_name, "inspect", old_name).stdout)
        self.assertEqual((inspected["name"], inspected["tracked"]), (old_name, True))
        self.assertEqual(inspected["cwd_source"], "tmux")
        self.assertEqual(Path(inspected["cwd"]).resolve(), self.project.resolve())
        after = json.loads(self.hgs("inspect", new_name))
        self.assertEqual((after["name"], after["cwd_source"]), (new_name, "tmux"))

    def test_attached_terminal_title_changes_without_restarting_or_typing(self):
        old_name, new_name = "sh/p/before", "sh/p/after"
        self.t("new-session", "-d", "-s", old_name, "cat")
        pane = self.pane_identity(old_name)
        master, slave = pty.openpty()
        client = subprocess.Popen([str(self.bin / "tmux"), "-u", "attach-session", "-t", "=" + old_name],
                                  env=dict(self.env, TERM="xterm-256color"), stdin=slave,
                                  stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        output = bytearray()

        def collect_output():
            while select.select([master], [], [], 0.01)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                output.extend(chunk)
            return b"\x1b]0;" + new_name.encode() + b"\x07" in output

        try:
            self.wait(lambda: str(client.pid) in self.t("list-clients", "-F", "#{client_pid}").splitlines())
            collect_output()
            output.clear()
            self.env["HGS_TAB"] = "1"
            result = subprocess.run([str(HGS), "rename", old_name, new_name], env=self.env,
                                    capture_output=True, text=True, timeout=20)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            deadline = time.monotonic() + 6
            while not collect_output() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(collect_output(), result.stdout + result.stderr + repr(bytes(output)) +
                            self.t("list-clients", "-F", "#{client_pid} #{client_tty} #{session_name}"))
            self.assertIn(b"\x1b]30;%w\x07", output)
            self.assertEqual(self.t("show-options", "-v", "-t", "=" + new_name + ":", "set-titles").strip(), "on")
            self.assertEqual(self.t("show-options", "-v", "-t", "=" + new_name + ":", "set-titles-string").strip(), "#S")
            self.assertEqual(self.pane_identity(new_name), pane)
            pane_output = self.t("capture-pane", "-p", "-t", "=" + new_name + ":")
            self.assertNotIn(new_name, pane_output, "title update must never be sent as terminal input")
            self.assertNotIn("%w", pane_output)
        finally:
            os.close(master)
            if client.poll() is None:
                client.terminate()
            client.wait(timeout=6)

    def test_untagged_session_can_gain_a_tag(self):
        old_name, new_name = "claude/p", "claude/p/review"
        self.hgs("claude", "p", "-d")
        self.wait(lambda: self.binding(old_name).get("conversation_id"))
        old = self.binding(old_name)
        self.hgs("rename", old_name, new_name)
        self.assertEqual(self.binding(new_name)["conversation_id"], old["conversation_id"])
        self.assertEqual(self.binding(new_name)["run_id"], old["run_id"])
        self.assertFalse(self.binding(old_name))

    def test_unicode_tag_and_project_with_spaces_preserve_identity(self):
        old_name = "claude/project with spaces/before"
        new_name = "claude/project with spaces/новый обзор"
        self.hgs("claude", str(self.project), "-n", "before", "-d")
        self.wait(lambda: self.binding(old_name).get("conversation_id"))
        before = self.binding(old_name)
        pane = self.pane_identity(old_name)
        self.hgs("rename", old_name, new_name)
        self.assertEqual(self.pane_identity(new_name), pane)
        self.assertEqual(self.binding(new_name)["run_id"], before["run_id"])
        self.assertEqual(self.binding(new_name)["conversation_id"], before["conversation_id"])
        self.assertEqual(Path(self.binding(new_name)["cwd"]).resolve(), self.project.resolve())
        self.hgs("pause", new_name)
        self.resume(new_name)
        self.assertEqual(self.binding(new_name)["conversation_id"], before["conversation_id"])

    def test_saved_rename_preserves_exact_resume_and_journal(self):
        old_name = self.start("kimi")
        self.send_event(old_name, "UserPromptSubmit", prompt="Saved context before rename")
        self.send_event(old_name, "Stop", last_assistant_message="Saved reply")
        self.hgs("pause", old_name)
        old = self.binding(old_name)
        before = json.loads(self.hgs("inspect", old_name))
        new_name = "kimi/p/review"
        self.hgs("rename", old_name, new_name)
        self.assertFalse(self.binding(old_name))
        renamed = self.binding(new_name)
        self.assertTrue(renamed["paused"])
        self.assertEqual(renamed["run_id"], old["run_id"])
        self.assertEqual(renamed["conversation_id"], old["conversation_id"])
        after = json.loads(self.hgs("inspect", new_name))
        self.assertEqual(after["events"][:len(before["events"])], before["events"])
        self.resume(new_name)
        self.assertEqual(self.binding(new_name)["conversation_id"], old["conversation_id"])
        self.assertEqual(self.binding(new_name)["prompt"], "Saved context before rename")

    def test_archive_rename_selects_only_requested_record_and_can_restore(self):
        old_name = self.start("codex")
        self.send_event(old_name, "UserPromptSubmit", prompt="Archive A")
        self.send_event(old_name, "Stop")
        self.hgs("pause", old_name)
        self.hgs("archive", old_name)
        first = next(row for row in self.rows() if row.get("state") == "archived")
        before = json.loads(self.hgs("inspect", old_name, "--archive", first["archive_id"]))
        self.start("codex")
        self.hgs("pause", old_name)
        self.hgs("archive", old_name)
        second = next(row for row in self.rows() if row["archive_id"] != first["archive_id"])
        second_before = json.loads(self.hgs("inspect", old_name, "--archive", second["archive_id"]))
        new_name = "codex/p/archived-review"
        self.hgs("rename", old_name, new_name, "--archive", first["archive_id"], "--dry-run")
        self.assertInspectionEqual(json.loads(self.hgs("inspect", old_name, "--archive", first["archive_id"])), before)
        self.hgs("rename", old_name, new_name, "--archive", first["archive_id"])
        renamed = json.loads(self.hgs("inspect", new_name, "--archive", first["archive_id"]))
        for key in ("archive_id", "archived_at", "run_id", "conversation_id", "prompt", "events"):
            self.assertEqual(renamed[key], before[key], key)
        self.assertInspectionEqual(json.loads(self.hgs("inspect", old_name, "--archive", second["archive_id"])), second_before)
        self.hgs("resume", new_name, "--archive", first["archive_id"], "-d")
        self.wait(lambda: self.binding(new_name).get("conversation_id") == before["conversation_id"])
        self.assertEqual(self.binding(new_name)["conversation_id"], before["conversation_id"])
        self.assertInspectionEqual(json.loads(self.hgs("inspect", old_name, "--archive", second["archive_id"])), second_before)

    def test_archive_rename_can_coexist_with_live_and_saved_names(self):
        old_name = self.start()
        self.hgs("pause", old_name)
        self.hgs("archive", old_name)
        archive_id = next(row["archive_id"] for row in self.rows() if row.get("state") == "archived")
        live_name = self.start("claude", "live")
        live = self.binding(live_name)
        self.hgs("rename", old_name, live_name, "--archive", archive_id)
        self.assertEqual(self.binding(live_name), live)
        self.assertEqual(json.loads(self.hgs("inspect", live_name, "--archive", archive_id))["archive_id"], archive_id)
        saved_name = self.start("claude", "saved")
        self.hgs("pause", saved_name)
        saved = self.binding(saved_name)
        self.hgs("rename", live_name, saved_name, "--archive", archive_id)
        self.assertEqual(self.binding(saved_name), saved)
        self.assertEqual(self.binding(live_name), live)
        self.assertEqual(json.loads(self.hgs("inspect", saved_name, "--archive", archive_id))["archive_id"], archive_id)
        self.hgs("resume", saved_name, "--archive", archive_id, "-d", rc=1)

    @unittest.skipUnless(LEGACY_BIN, "set HGS_TEST_LEGACY_BIN to a real pre-rename native hgs")
    def test_existing_legacy_supervisor_finishes_under_renamed_identity(self):
        old_name, new_name = "claude/p/legacy", "claude/p/renamed-legacy"
        installed = self.bin / "hgs-before-rename"
        shutil.copy2(LEGACY_BIN, installed)
        result = subprocess.run([str(installed), "claude", "p", "-n", "legacy", "-d"],
                                env=self.env, capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.wait(lambda: self.binding(old_name).get("conversation_id"))
        old = self.binding(old_name)
        self.assertEqual(Path(old["hgs"]).resolve(), installed.resolve())
        # Model installation faithfully: the running supervisor retains old code,
        # while subsequent hook processes execute the atomically upgraded binary.
        upgraded = self.bin / "hgs-upgraded"
        shutil.copy2(HGS, upgraded)
        upgraded.replace(installed)
        self.hgs("rename", old_name, new_name)
        self.start("claude", "legacy")
        replacement = self.binding(old_name)
        self.t("send-keys", "-t", "=" + new_name + ":", "/exit", "Enter")
        self.wait(lambda: any(row["name"] == new_name and row.get("state") == "archived"
                              for row in self.rows()))
        archived = next(row for row in self.rows() if row["name"] == new_name)
        info = json.loads(self.hgs("inspect", new_name, "--archive", archived["archive_id"]))
        self.assertEqual(info["conversation_id"], old["conversation_id"])
        self.assertEqual(self.binding(old_name), replacement)
        self.t("has-session", "-t", "=" + old_name)

    def test_rename_dry_run_collisions_and_invalid_names_do_not_mutate(self):
        old_name = self.start()
        live_name = self.start("claude", "occupied")
        saved_name = self.start("claude", "saved")
        self.hgs("pause", saved_name)
        old = self.binding(old_name)
        pane = self.pane_identity(old_name)
        self.hgs("rename", old_name, "claude/p/review", "--dry-run")
        self.assertEqual(self.binding(old_name), old)
        self.assertEqual(self.pane_identity(old_name), pane)
        self.assertFalse(self.binding("claude/p/review"))
        for target in (live_name, saved_name, "codex/p/review", "claude/other/review",
                       "claude/p/bad:name", "claude/p/bad\nname", "claude/p/",
                       "claude/p/ leading", "claude/p/trailing ", "claude/p/bad.tag"):
            with self.subTest(target=target):
                self.hgs("rename", old_name, target, rc=1)
                self.assertEqual(self.binding(old_name), old)
                self.assertEqual(self.pane_identity(old_name), pane)
        for archive_id in ("../escape", "invalid", str(uuid.uuid4())):
            self.hgs("rename", old_name, "claude/p/review", "--archive", archive_id, rc=1)
            self.assertEqual(self.binding(old_name), old)
        self.hgs("rename", "claude/p/missing", "claude/p/review", rc=1)
        self.assertEqual(self.binding(live_name)["name"], live_name)
        self.assertEqual(self.binding(saved_name)["name"], saved_name)


class Migration(Harness):
    @unittest.skipIf(LEGACY_STATE, "native resume no longer depends on the previous installation path")
    def test_v1_resume_survives_missing_old_hgs_checkout(self):
        record, path = self.saved_v1("codex")
        record["hgs"] = str(self.root / "removed checkout/hgs")
        path.write_text(json.dumps(record) + "\n")
        self.hgs("resume", record["name"], "-d")
        self.wait(lambda: self.binding(record["name"]).get("activity") == "idle")
        self.assertEqual(self.binding(record["name"])["conversation_id"], record["conversation_id"])
        argv = json.loads((self.root / "argv.jsonl").read_text().splitlines()[-1])["argv"]
        self.assertEqual(argv[:2], ["resume", record["conversation_id"]])

    @unittest.skipIf(LEGACY_STATE, "native APIs do not need a Python interpreter")
    def test_native_state_and_project_apis_do_not_invoke_python(self):
        marker = self.root / "python-was-invoked"
        for name in ("python", "python3"):
            self.script(name, "#!/bin/sh\ntouch " + shlex.quote(str(marker)) + "\nexit 97\n")
        record, _ = self.saved_v1()
        self.assertEqual(json.loads(self.hgs("dirs"))["path"], str(self.root.resolve()))
        self.assertTrue(json.loads(self.hgs("inspect", record["name"]))["tracked"])
        self.assertEqual(len(json.loads(self.hgs("ls", "--local", "--json"))["sessions"]), 1)
        self.assertEqual(json.loads(self.hgs("project", "ls", "--json"))[0]["name"], "p")
        self.hgs("resume", record["name"], "--dry-run")
        self.assertFalse(marker.exists())

    @unittest.skipIf(LEGACY_STATE, "the legacy callback shim is installed by migration")
    def test_legacy_shim_is_ungated_noop_and_refuses_old_shell_cli(self):
        directory = self.root / "old install"
        directory.mkdir()
        shim = directory / "hgs_state.py"
        shutil.copyfile(REPO / "hgs_state.py", shim)
        marker = directory / "old-cli-executed"
        old_cli = directory / "hgs"
        old_cli.write_text("#!/bin/sh\ntouch " + shlex.quote(str(marker)) + "\n")
        old_cli.chmod(0o755)
        env = dict(self.env, PATH=str(self.bin), HGS_EXECUTABLE=str(old_cli))
        ungated = subprocess.run([sys.executable, str(shim), "hook"], env=env,
                                 input="{}", text=True, capture_output=True, timeout=5)
        self.assertEqual(ungated.returncode, 0, ungated.stderr)
        self.assertEqual(ungated.stdout + ungated.stderr, "")
        env.update(HGS_SESSION="claude/legacy", HGS_RUN_ID="legacy-run")
        gated = subprocess.run([sys.executable, str(shim), "hook"], env=env,
                               input="{}", text=True, capture_output=True, timeout=5)
        self.assertEqual(gated.returncode, 1)
        self.assertIn("Rust executable not found", gated.stderr)
        self.assertFalse(marker.exists())

    def test_v1_bindings_and_sqlite_journal_survive_resume(self):
        records = [self.saved_v1(tag=tag)[0] for tag in ("one", "two")]
        db = sqlite3.connect(self.root / "state/events.sqlite3")
        with db:
            db.execute("CREATE TABLE events (seq INTEGER PRIMARY KEY, name TEXT, conversation TEXT, payload TEXT)")
            db.execute("INSERT INTO events VALUES (?, ?, ?, ?)", (123, records[0]["name"],
                       records[0]["conversation_id"], json.dumps({"type": "Stop", "at": 1700000010,
                       "detail": "Existing journal entry", "run_id": "legacy-run", "agent_id": "", "tool": ""})))
        db.close()
        snapshot = json.loads(self.hgs("ls", "--local", "--json"))
        self.assertEqual({s["name"] for s in snapshot["sessions"]}, {r["name"] for r in records})
        self.assertTrue(all(s["resumable"] and s["state"] == "paused" for s in snapshot["sessions"]))
        old = json.loads(self.hgs("inspect", records[0]["name"]))
        self.assertEqual(old["events"][0]["seq"], 123)
        self.assertEqual(old["events"][0]["detail"], "Existing journal entry")
        self.hgs("resume", "--all")
        for record in records:
            name = record["name"]
            self.wait(lambda: self.binding(name).get("activity") == "idle")
            resumed = self.binding(name)
            self.assertEqual(resumed["conversation_id"], record["conversation_id"])
            self.assertEqual(resumed["created"], record["created"])
            self.assertEqual(resumed["prompt"], record["prompt"])
        self.assertIn("Existing journal entry", [e["detail"] for e in
                      json.loads(self.hgs("inspect", records[0]["name"]))["events"]])
        launches = [json.loads(line)["argv"] for line in (self.root / "argv.jsonl").read_text().splitlines()]
        self.assertEqual({argv[1] for argv in launches}, {r["conversation_id"] for r in records})

    @unittest.skipIf(LEGACY_STATE, "the legacy callback shim is installed by migration")
    def test_existing_hook_command_is_preserved_and_executes_native_core(self):
        legacy_dir = self.root / "old install with spaces"
        legacy_dir.mkdir()
        shim = legacy_dir / "hgs_state.py"
        shutil.copyfile(REPO / "hgs_state.py", shim)
        (legacy_dir / "hgs").symlink_to(HGS)
        command = shlex.join([sys.executable, str(shim), "hook"])
        settings = self.root / ".codex/hooks.json"
        settings.parent.mkdir()
        trusted_group = {"hooks": [{"type": "command", "command": command, "timeout": 5}]}
        custom_group = {"matcher": "Bash", "hooks": [{"type": "command", "command": "true"}]}
        settings.write_text(json.dumps({"custom": {"keep": True}, "hooks": {
            "SessionStart": [trusted_group], "Stop": [custom_group, trusted_group]}}))
        name = self.start("codex")
        data = json.loads(settings.read_text())
        self.assertEqual(data["custom"], {"keep": True})
        self.assertEqual(data["hooks"]["SessionStart"], [trusted_group])
        self.assertEqual(data["hooks"]["Stop"], [custom_group, trusted_group])
        self.hgs("pause", name)
        self.resume(name)
        launches = [json.loads(line)["argv"] for line in (self.root / "argv.jsonl").read_text().splitlines()]
        self.assertTrue(all("--dangerously-bypass-hook-trust" not in argv for argv in launches))




# What Apple's sample writes for a stopped Python agent, in its own layout: a
# main and a second thread, each deeper than the six frames a report keeps.
SAMPLED = """Sampling process 501 for 1 second with 1 millisecond of run time between samples
Call graph:
    868 Thread_54085961   DispatchQueue_1: com.apple.main-thread  (serial)
    + 868 start  (in dyld) + 6688  [0x1841ebe80]
    +   868 Py_BytesMain  (in Python) + 40  [0x10398bdd8]
    +     868 Py_RunMain  (in Python) + 524  [0x10398a2a4]
    +       868 PyEval_EvalCode  (in Python) + 132  [0x1038a81f8]
    +         868 _PyEval_EvalFrameDefault  (in Python) + 58912  [0x1038b8d1c]
    +           868 time_sleep  (in Python) + 184  [0x103a046b4]
    +             868 nanosleep  (in libsystem_c.dylib) + 220  [0x184457054]
    +               868 __semwait_signal  (in libsystem_kernel.dylib) + 8,16  [0x184580308,0x184580310]
    868 Thread_54085962
      868 thread_start  (in libsystem_pthread.dylib) + 8  [0x1845bccec]
        868 _pthread_start  (in libsystem_pthread.dylib) + 136  [0x1845c1d00]
          868 pythread_wrapper  (in Python) + 48  [0x1039754b4]
            868 thread_run  (in Python) + 160  [0x103a02f3c]
              868 lock_PyThread_acquire_lock  (in Python) + 60  [0x103a00f0c]
                868 _PySemaphore_Wait  (in Python) + 108  [0x1039475e8]
                  868 __psynch_cvwait  (in libsystem_kernel.dylib) + 8  [0x18458050c]

Total number in stack (recursion counted multiple times):
"""


class StopReportControls(unittest.TestCase):
    """The stop report against mocked commands, each a small real child that
    prints what the command would: the report stays within its deadline and
    sizes, signals only exact fake agents of its own panes, stacks only their
    verified processes, reaps every command and never replaces the failure."""
    hgs = Harness.hgs
    stop_report = Harness.stop_report
    real_popen = subprocess.Popen

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="hgs-stop-report-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.bin = self.root / ".local/bin"
        self.bin.mkdir(parents=True)
        (self.root / "state").mkdir()
        self.env = {"PATH": os.environ.get("PATH", "")}
        self.interpreters = fake_agent_interpreters(self.bin, self.env["PATH"])
        if not self.interpreters:
            self.skipTest("no python3 on PATH")
        self.started = "Sun Oct 18 00:00:00 2026"
        python = "/usr/bin/python3"
        # pid: (ppid, command). 999 has a fake agent's exact command line but
        # no private pane; 502 and 503 only mention the bin or run Python;
        # 504 names a fake script but runs another program.
        self.table = {"500": ("1", "/fixture/hgs --run 0 claude"),
                      "501": ("500", f"{python} {self.bin / 'claude'} --settings {{}}"),
                      "502": ("500", f"/bin/sh -c {self.bin / 'claude'}-helper"),
                      "503": ("500", f"{python} -c pass"),
                      "504": ("500", f"/synthetic/notpython {self.bin / 'claude'}"),
                      "999": ("1", f"{python} {self.bin / 'claude'}")}
        self.executables = {"501": sorted(self.interpreters)[0], "504": "/synthetic/notpython"}
        self.identity, self.flood, self.pane_gone, self.kills, self.commands, self.children = {}, False, False, [], [], []
        self.slow_sample = None
        real_kill = os.kill
        # Signals reach only the report's own command children; any other is recorded, not sent.
        def kill_child_or_record(pid, number):
            if any(child.pid == pid for child in self.children):
                real_kill(pid, number)
            else:
                self.kills.append((pid, number))
        kill = mock.patch.object(os, "kill", side_effect=kill_child_or_record)
        kill.start()
        self.addCleanup(kill.stop)

    def output(self, argv):
        if argv[0] == str(self.bin / "tmux") and argv[1] == "list-panes":
            if argv[4] == "#{pane_pid} #{pane_dead}":
                return "" if self.pane_gone else "500 0\n"
            return "claude/p/one %0 500 0\n"
        if argv[:2] == ["ps", "-axo"]:
            return "".join(f"{pid:>6} {ppid:>6} {self.started} S 00:10 {command}\n" for pid, (ppid, command) in self.table.items())
        if argv[:2] == ["ps", "-p"] and argv[4] == "comm=":
            return self.executables.get(argv[2], "/fixture/other") + "\n"
        if argv[:2] == ["ps", "-p"]:
            ppid, command = self.table[argv[2]]
            return f"{ppid} {self.identity.get(argv[2], self.started)} {command}\n"
        if argv[0] == str(self.bin / "tmux") and argv[1] == "capture-pane":
            return 'Current thread 0x1 (most recent call first):\n  File "claude", line 73 in <module>\n'
        if argv[0] == "sample":
            return SAMPLED
        return ""

    def spawn(self, argv, **options):
        argv = [str(part) for part in argv]
        self.commands.append(argv)
        if argv[0] == str(HGS):
            code = ["import sys; sys.stderr.write(sys.argv[1]); sys.exit(1)",
                    "hgs: claude/p/one: agent did not exit after SIGTERM (no forced kill was sent)\n"]
        elif self.hang or argv[0] == "sample" and argv[1] == self.slow_sample:
            code = ["import time; time.sleep(60)"]
        elif argv[0] == "sample" and self.flood:
            block = "".join(f"    9 Thread_{index}   DispatchQueue_1: fixture\n" + "      9 frame  (in fixture) + 8\n" * 40
                            for index in range(50))
            code = ["import sys\nwhile True: sys.stdout.write(sys.argv[1])", "Call graph:\n" + block]
        else:
            code = ["import sys; sys.stdout.write(sys.argv[1])", self.output(argv)]
        child = self.real_popen([sys.executable, "-c", *code], **options)
        self.children.append(child)
        return child

    def report(self, hang=False, platform="darwin"):
        self.hang, self.proc = hang, []
        real_open, real_realpath = open, os.path.realpath
        # /proc is recorded, never read: the mocked PIDs belong to no real pane.
        def proc_open(path, *args, **options):
            if str(path).startswith("/proc/"):
                self.proc.append(str(path))
                raise FileNotFoundError(path)
            return real_open(path, *args, **options)
        def proc_realpath(path, *args, **options):
            if str(path).startswith("/proc/"):
                self.proc.append(str(path))
                return str(path)
            return real_realpath(path, *args, **options)
        with mock.patch.object(subprocess, "Popen", side_effect=self.spawn), mock.patch.object(sys, "platform", platform), \
                mock.patch("builtins.open", side_effect=proc_open), mock.patch.object(os.path, "realpath", side_effect=proc_realpath), \
                mock.patch.object(time, "sleep"), mock.patch.object(os, "getloadavg", return_value=(5.5, 4.25, 3.0)), \
                mock.patch.object(os, "cpu_count", return_value=3):
            return self.stop_report()

    def sampled(self):
        return {argv[1] for argv in self.commands if argv[0] == "sample"}

    def tearDown(self):
        # Every command the report started was stopped and reaped.
        self.assertEqual([child.pid for child in self.children if child.poll() is None], [])

    def test_hung_commands_keep_the_original_failure_within_the_deadline(self):
        self.hang = True
        started = time.monotonic()
        with mock.patch.dict(globals(), STOP_REPORT_SECONDS=1.0), mock.patch.object(subprocess, "Popen", side_effect=self.spawn), \
                self.assertRaises(AssertionError) as caught:
            self.hgs("pause", "claude/p/one")
        self.assertLess(time.monotonic() - started, 4)
        message = str(caught.exception)
        self.assertIn("agent did not exit after SIGTERM", message)
        self.assertIn("unavailable: panes: TimeoutExpired", message)
        self.assertIn("report deadline reached", message)
        self.assertEqual(self.kills, [])

    def test_only_exact_fake_agents_of_the_private_panes_are_signalled(self):
        report = self.report()
        self.assertEqual(self.kills, [(501, signal.SIGUSR1)])
        self.assertIn("python 504: not signalled, it does not run the fixture's Python", report)
        self.assertIn('python in %0: Current thread 0x1 (most recent call first): | File "claude", line 73 in <module>', report)
        for pid in self.table:
            self.assertRegex(report, rf"(?m)^process: {pid} ")
        # A listed path match is informational only: it is never sampled.
        self.assertEqual(self.sampled(), {"500", "501", "502", "503", "504"})

    def test_a_reused_pid_is_neither_signalled_nor_sampled(self):
        self.identity["501"] = "Sun Oct 18 00:00:09 2026"
        report = self.report()
        self.assertEqual(self.kills, [])
        self.assertIn("python 501: not signalled, its process identity or pane changed", report)
        self.assertIn("native 501: omitted, its process identity or pane changed", report)
        self.assertTrue({"501", "999"}.isdisjoint(self.sampled()))

    def test_proc_is_read_only_for_verified_pane_processes(self):
        self.identity["501"] = "Sun Oct 18 00:00:09 2026"
        report = self.report(platform="linux")
        self.assertTrue({path.split("/")[2] for path in self.proc}.isdisjoint({"501", "999"}))
        self.assertIn("/proc/500/wchan", self.proc)
        self.assertIn("python 504: not signalled, it does not run the fixture's Python", report)
        self.assertEqual(self.kills, [])

    def test_a_sample_past_its_limit_stays_explicit_bounded_evidence(self):
        # The agent's sample outlasts its limit, as on a loaded runner: the report
        # says so, stops that sampler and still stacks every other process.
        self.slow_sample = "501"
        started = time.monotonic()
        report = self.report()
        self.assertGreaterEqual(time.monotonic() - started, 5)
        # The host's load comes before the stacks it explains.
        self.assertIn("\nhost: load 5.50 4.25 3.00 (1, 5, 15 min) on 3 CPUs\nstack 500 ", report)
        self.assertIn("unavailable: native stack 501: TimeoutExpired: Command '['sample', '501', '1', '-mayDie', "
                      "'-file', '/dev/stdout']' timed out after 5 seconds", report)
        self.assertNotRegex(report, r"(?m)^stack 501 ")
        for pid in ("500", "502", "503", "504"):
            # The innermost six frames of each thread, innermost first.
            self.assertIn(f"stack {pid} 868 Thread_54085961   DispatchQueue_1: com.apple.main-thread: "
                          "+               868 __semwait_signal  (in libsystem_kernel.dylib) + 8,16  [0x184580308,0x184580310]"
                          " <- +             868 nanosleep  (in libsystem_c.dylib) + 220  [0x184457054]"
                          " <- +           868 time_sleep  (in Python) + 184  [0x103a046b4]"
                          " <- +         868 _PyEval_EvalFrameDefault  (in Python) + 58912  [0x1038b8d1c]"
                          " <- +       868 PyEval_EvalCode  (in Python) + 132  [0x1038a81f8]"
                          " <- +     868 Py_RunMain  (in Python) + 524  [0x10398a2a4]\n", report)
            self.assertIn(f"stack {pid} 868 Thread_54085962: 868 __psynch_cvwait  (in libsystem_kernel.dylib) + 8  [0x18458050c]"
                          " <- 868 _PySemaphore_Wait  (in Python) + 108  [0x1039475e8]"
                          " <- 868 lock_PyThread_acquire_lock  (in Python) + 60  [0x103a00f0c]"
                          " <- 868 thread_run  (in Python) + 160  [0x103a02f3c]"
                          " <- 868 pythread_wrapper  (in Python) + 48  [0x1039754b4]"
                          " <- 868 _pthread_start  (in libsystem_pthread.dylib) + 136  [0x1845c1d00]", report)
        self.assertNotIn("<- 868 thread_start", report)
        self.assertIn("python in %0: Current thread 0x1", report)
        slow = [child for argv, child in zip(self.commands, self.children) if argv[:2] == ["sample", "501"]]
        self.assertEqual([child.wait(timeout=5) for child in slow], [-signal.SIGKILL])
        self.assertEqual(self.kills, [(501, signal.SIGUSR1)])

    def test_a_closed_pane_stops_signals_and_stacks(self):
        self.pane_gone = True
        report = self.report()
        self.assertEqual(self.kills, [])
        self.assertEqual(self.sampled(), set())
        self.assertIn("native 500: omitted, its process identity or pane changed", report)

    def test_flooded_output_is_bounded_where_it_is_captured(self):
        self.flood = True
        self.table.update({str(pid): ("500", f"/fixture/child {pid}") for pid in range(600, 650)})
        report = self.report()
        self.assertLessEqual(len(report), STOP_REPORT_CHARS + 60)
        self.assertIn("process: +44 more not listed", report)
        self.assertIn("threads omitted", report)
        self.assertIn("python in %0: Current thread 0x1", report)
        floods = [child for argv, child in zip(self.commands, self.children) if argv[0] == "sample"]
        self.assertTrue(floods)
        # Each endless writer was stopped at the byte ceiling, not drained.
        self.assertTrue(all(child.wait(timeout=5) == -signal.SIGKILL for child in floods))
        self.assertEqual(self.kills, [(501, signal.SIGUSR1)])


if __name__ == "__main__":
    unittest.main()
