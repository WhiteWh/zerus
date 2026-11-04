#!/usr/bin/env python3
"""Register this user's configuration-scoped hgs desktop service on Linux."""
import argparse
import configparser
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import selectors
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import uuid

UNIT = "hgs-desktop.service"
ABI = "desktop-lifecycle-v1"
LIMIT = 65536
GRAPHICAL = "graphical-session.target"


_spec = importlib.util.spec_from_file_location("desktop_revoke_hold", Path(__file__).with_name("desktop-revoke-hold.py"))
_hold = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_hold)
Refusal = _hold.Refusal


class PendingRestart(Refusal):
    """Registration is durable; restarting needs the current graphical login."""


def require(condition, message):
    if not condition:
        raise Refusal(message)


def command(argv, env, timeout=10):
    """Bound both output streams and time; never signal a process group."""
    child = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, close_fds=True)
    streams = selectors.DefaultSelector()
    result = {"out": bytearray(), "err": bytearray()}
    deadline = time.monotonic() + timeout
    try:
        streams.register(child.stdout, selectors.EVENT_READ, "out")
        streams.register(child.stderr, selectors.EVENT_READ, "err")
        while streams.get_map():
            remaining = deadline - time.monotonic()
            require(remaining > 0, f"command timed out: {argv[0]}")
            for key, _ in streams.select(remaining):
                chunk = os.read(key.fd, 4096)
                if not chunk:
                    streams.unregister(key.fileobj)
                else:
                    result[key.data].extend(chunk)
                    require(sum(map(len, result.values())) <= LIMIT, "command output exceeded 64 KiB")
        code = child.wait(timeout=max(0.01, deadline - time.monotonic()))
        out, error = (result[name].decode("utf-8", errors="replace") for name in ("out", "err"))
        require(code == 0, error.strip() or f"command failed ({code}): {argv[0]}")
        return out
    except subprocess.TimeoutExpired:
        raise Refusal(f"command timed out: {argv[0]}") from None
    finally:
        streams.close()
        if child.poll() is None:
            child.kill()  # Only the original CLI/systemctl child created above.
            child.wait(timeout=2)
        child.stdout.close()
        child.stderr.close()


def read_file(path):
    """Keep custom files and symlinks; all installer records are single-link."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    except FileNotFoundError:
        return None
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.geteuid() and info.st_nlink == 1,
                f"not a private user-owned regular file: {path}")
        require(not info.st_mode & 0o022, f"file is writable by another user: {path}")
        raw = source.read(LIMIT + 1)
        require(len(raw) <= LIMIT, f"file exceeds 64 KiB: {path}")
        return raw


def write_file(path, raw):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".hgs-desktop-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(raw)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def binary_digest(path):
    with open(path, "rb") as source:
        before = os.fstat(source.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_size <= 512 * 1024 * 1024,
                "selected CLI is not a bounded regular executable")
        checksum = hashlib.sha256()
        remaining = before.st_size
        while remaining:
            chunk = source.read(min(1024 * 1024, remaining))
            require(chunk, "selected CLI changed during inspection")
            checksum.update(chunk)
            remaining -= len(chunk)
        require(not source.read(1), "selected CLI grew during inspection")
        value = checksum.hexdigest()
        after = os.fstat(source.fileno())
    current = os.stat(path)
    identity = lambda s: (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns)
    require(identity(before) == identity(after) == identity(current), "selected CLI changed during inspection")
    return value


def unit_quote(value):
    return '"' + value.replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%').replace('$', '$$') + '"'


def destination_for(env):
    root = Path(env.get("XDG_CONFIG_HOME") or str(Path(env["HOME"]) / ".config"))
    require(root.is_absolute(), "XDG_CONFIG_HOME must be absolute")
    return root / "systemd/user" / UNIT


def registration(hgs, env, run=command, mode="passive"):
    require("hgs desktop registration --json" in run([hgs, "--help"], env),
            "selected hgs does not support desktop registration; update the CLI first")
    try:
        value = json.loads(run([hgs, "desktop", "registration", "--json"]
                               + (["--background"] if mode == "background" else []), env))
        probe = value["readiness"]
        scope = probe["environment"]
        args = [hgs, "desktop", "serve", "--config-dir", scope["HGS_CONFIG_DIR"],
                "--state-dir", scope["HGS_STATE_DIR"]]
        if mode == "background":
            args.append("--background")
        configuration = value["configuration"]
        require(value["schema"] == 1 and value["label"] == UNIT and len(configuration) == 64
                and all(c in "0123456789abcdef" for c in configuration), "unsupported registration identity")
        require(value["argv"] == args and probe["argv"] == [hgs, "desktop", "status", "--json"],
                "registration must preserve the selected local CLI and exact command")
        require(value.get("mode", "passive") == mode, "registration mode does not match the selection")
        require(set(scope) == {"HGS_CONFIG_DIR", "HGS_STATE_DIR"}
                and all(isinstance(p, str) and Path(p).is_absolute() and not any(ord(c) < 32 for c in p)
                        for p in scope.values()), "registration configuration paths are invalid")
        require(probe["abi"] == ABI and probe["expected_configuration"] == configuration
                and probe["configuration_field"] == "lifecycle.configuration"
                and probe["ready_field"] == "lifecycle.ipc_ready"
                and probe["epoch_field"] == "lifecycle.epoch"
                and probe["generation_field"] == "lifecycle.generation", "unsupported desktop readiness contract")
        destination = destination_for(env)
        require(Path(value["destination"]) == destination, "registration names another user's unit destination")
        definition = value["definition"]
        require(isinstance(definition, str) and len(definition.encode()) <= LIMIT
                and definition.startswith(f"# Zerus desktop registration v1; configuration={configuration}\n"),
                "registration definition has no matching ownership marker")
        parsed = configparser.ConfigParser(interpolation=None)
        parsed.read_string(definition)
        require(parsed["Service"]["ExecStart"] == " ".join(map(unit_quote, args))
                and parsed["Service"]["KillMode"] == "process"
                and parsed["Service"]["Restart"] == "on-failure"
                and parsed["Service"]["RestartPreventExitStatus"] == "78"
                and parsed["Unit"]["After"] == GRAPHICAL and parsed["Unit"]["PartOf"] == GRAPHICAL
                and parsed["Install"]["WantedBy"] == GRAPHICAL,
                "desktop unit must retain its command, graphical ordering and process-only stop policy")
        require(value["session"]["kind"] == "graphical_user"
                and value["session"]["import_from"] == "current_login_session", "not a graphical user registration")
        return value
    except (KeyError, TypeError, ValueError, configparser.Error) as error:
        raise Refusal(f"invalid desktop registration: {error}") from None


def graphics(env):
    """No guessed display, autolaunch bus, or Xwayland idle substitution."""
    kind = env.get("XDG_SESSION_TYPE")
    if kind not in ("wayland", "x11"):
        return None
    names = ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "XDG_SESSION_TYPE"]
    names += ["WAYLAND_DISPLAY"] if kind == "wayland" else ["DISPLAY"]
    require(all(env.get(name) for name in names), "graphical session environment is incomplete; run from the current login session")
    values = {name: env[name] for name in names}
    if kind == "x11" and env.get("XAUTHORITY"):
        values["XAUTHORITY"] = env["XAUTHORITY"]
    if env.get("XDG_SESSION_ID"):
        values["XDG_SESSION_ID"] = env["XDG_SESSION_ID"]
    require(all(len(value) <= 4096 and not any(ord(c) < 32 for c in value) for value in values.values()),
            "invalid graphical session environment")
    runtime = Path(values["XDG_RUNTIME_DIR"])
    info = runtime.stat()
    require(runtime.is_absolute() and stat.S_ISDIR(info.st_mode) and info.st_uid == os.geteuid()
            and stat.S_IMODE(info.st_mode) == 0o700, "XDG_RUNTIME_DIR must be this user's private runtime directory")
    require(values["DBUS_SESSION_BUS_ADDRESS"].startswith("unix:"), "use the current local session D-Bus address")
    return values


class Installer:
    def __init__(self, hgs, env=None, run=command, mode=None, hold_factory=None):
        self.env = dict(os.environ if env is None else env)
        self.run = run
        self.hold_factory = hold_factory or _hold.Hold
        self.rollback_hold = None
        self.hgs = os.path.abspath(hgs)
        require(os.access(self.hgs, os.X_OK), "selected hgs is not executable")
        require(mode in (None, "passive", "background"), "unsupported desktop service mode")
        self.requested_mode = mode
        old = read_file(destination_for(self.env).with_name(".hgs-desktop.zerus-registration.json"))
        try:
            selected = json.loads(old).get("mode", "passive") if old is not None else "passive"
        except (TypeError, ValueError, AttributeError):
            raise Refusal("invalid desktop registration record; kept") from None
        require(selected in ("passive", "background"), "invalid selected mode; kept")
        self.mode = mode or selected
        self.registration = registration(self.hgs, self.env, self.run, self.mode)
        self.destination = Path(self.registration["destination"])
        self.record_path = self.destination.with_name(".hgs-desktop.zerus-registration.json")
        self.definition = self.registration["definition"].encode()
        self.configuration = self.registration["configuration"]
        self.binary = binary_digest(self.hgs)
        self.last_status = None

    def cli(self, *args, timeout=10):
        probe = self.registration["readiness"]
        value = json.loads(self.run([self.hgs, "desktop", *args],
                                    dict(self.env, **probe["environment"]), timeout=timeout))
        require(isinstance(value, dict), "invalid desktop command reply")
        return value

    def caller(self):
        require(not any(name in self.env for name in ("HGS_SESSION", "HGS_RUN_ID")),
                "agent session cannot change desktop mode; use Settings or a terminal outside Zerus")
        value = self.cli("registration", "mode-check", "--json")
        require(value.get("schema") == 1 and value.get("configuration") == self.configuration
                and value.get("admitted") is True
                and value.get("source") in ("interactive_terminal", "installed_frontend"),
                "mode changes require Settings or a terminal outside Zerus")

    def grant(self):
        value = self.cli("ownership-status", "--json")
        def valid_uuid(text):
            try:
                return str(uuid.UUID(text)) == text
            except (ValueError, TypeError, AttributeError):
                return False
        require(value.get("abi") == "desktop-ownership-v1"
                and value.get("configuration") == self.configuration
                and value.get("state") == "service" and value.get("identity_current") is True
                and value.get("rearm_required") is False
                and all(valid_uuid(value.get(key)) for key in ("journal", "request_id", "owner_epoch"))
                and type(value.get("revision")) is int and 0 < value["revision"] < 2 ** 53,
                "open Zerus Next in service mode and complete Grant/Rearm first; a retained revoke is never cleared by this installer")
        return value

    def manager(self, *args):
        self.fence()
        value = self.run(["systemctl", "--user", *args], self.env)
        self.fence()
        return value

    def fence(self):
        if self.rollback_hold is not None:
            self.rollback_hold.check()

    def state(self):
        raw = self.manager("show", UNIT, "--property=LoadState,ActiveState,MainPID,FragmentPath,DropInPaths", "--no-pager")
        value = dict(line.split("=", 1) for line in raw.splitlines() if "=" in line)
        require(not value.get("DropInPaths"), "custom desktop service drop-ins kept; review them before registration")
        require(value.get("LoadState") in ("loaded", "not-found")
                and value.get("ActiveState") in ("active", "inactive", "failed"),
                "desktop service manager state is transitional or unavailable; kept")
        fragment = value.get("FragmentPath", "")
        require(not fragment or fragment == str(self.destination), "a custom desktop service definition is already selected; kept")
        return value

    def ownership(self):
        raw, previous = read_file(self.destination), read_file(self.record_path)
        if previous is not None:
            try:
                record = json.loads(previous)
                require(type(record["schema"]) is int and record["schema"] == 1
                        and record["configuration"] == self.configuration,
                        "existing desktop registration belongs to another configuration; kept")
                require(type(record["pending_restart"]) is bool
                        and (record["session"] is None or isinstance(record["session"], dict))
                        and all(isinstance(record[key], str) and len(record[key]) == 64
                                and all(c in "0123456789abcdef" for c in record[key])
                                for key in ("binary_sha256", "definition_sha256")),
                        "invalid desktop registration record; kept")
                require(record.get("mode", "passive") in ("passive", "background"),
                        "invalid desktop registration mode; kept")
                transition = record.get("transition")
                if transition is not None:
                    require(isinstance(transition, dict) and set(transition) == {"target", "request_id"}
                            and transition["target"] == "passive"
                            and str(uuid.UUID(transition["request_id"])) == transition["request_id"],
                            "invalid pending mode transition; kept")
                last = record.get("last_revoke_request_id")
                require(last is None or str(uuid.UUID(last)) == last,
                        "invalid original revoke identity; kept")
                cancelled = record.get("last_cancelled_rollback_request_id")
                require(cancelled is None or (isinstance(cancelled, str) and str(uuid.UUID(cancelled)) == cancelled),
                        "invalid cancelled rollback identity; kept")
                # A crash after replacing our unit but before saving its new
                # digest can leave the old record. Recover only this selected
                # CLI's exact emitted definition under an otherwise valid owned
                # record; arbitrary edits and other configurations still refuse.
                require(raw is not None and (digest(raw) == record["definition_sha256"] or raw == self.definition),
                        "custom or missing desktop service definition kept")
                return record
            except (KeyError, TypeError, ValueError, AttributeError):
                raise Refusal("invalid desktop registration record; kept") from None
        require(raw is None or raw == self.definition, "custom desktop service definition kept")
        return None

    def probe(self, timeout=10):
        probe = self.registration["readiness"]
        value = json.loads(self.run(probe["argv"], dict(self.env, **probe["environment"]), timeout=timeout))
        require(isinstance(value, dict), "invalid desktop readiness object")
        self.last_status = value
        lifecycle = value.get("lifecycle", {})
        require(isinstance(lifecycle, dict) and lifecycle.get("abi") == ABI
                and lifecycle.get("configuration") == self.configuration,
                "desktop readiness ABI/configuration mismatch")
        require(type(lifecycle.get("ipc_ready")) is bool, "desktop readiness is not a boolean")
        require(lifecycle.get("mode") in (None, "passive", "background"), "invalid running desktop mode")
        if lifecycle["ipc_ready"]:
            require(lifecycle.get("phase") == "serving"
                    and type(lifecycle.get("pid")) is int and lifecycle["pid"] > 0
                    and type(lifecycle.get("generation")) is int and 0 < lifecycle["generation"] < 2 ** 64
                    and isinstance(lifecycle.get("epoch"), str) and 0 < len(lifecycle["epoch"]) <= 128
                    and not any(ord(c) < 32 for c in lifecycle["epoch"]),
                    "desktop readiness has no valid live lifecycle identity")
        return lifecycle

    def save(self, record):
        self.fence()
        write_file(self.record_path, (json.dumps(record, indent=2) + "\n").encode())
        self.fence()

    def status(self):
        previous = self.ownership()
        state = self.state()
        lifecycle = self.probe() if state.get("ActiveState") == "active" else {}
        if lifecycle:
            require(str(lifecycle.get("pid")) == state.get("MainPID"), "desktop PID/readiness mismatch")
        return {"schema": 1, "operation": "status", "configuration": self.configuration,
                "label": UNIT, "registered": previous is not None,
                "selected_mode": previous.get("mode", "passive") if previous else "passive",
                "running_mode": lifecycle.get("mode"), "ipc_ready": lifecycle.get("ipc_ready", False),
                "pending_restart": previous.get("pending_restart", False) if previous else False,
                "last_revoke_request_id": previous.get("last_revoke_request_id") if previous else None,
                "last_cancelled_rollback_request_id": previous.get("last_cancelled_rollback_request_id") if previous else None,
                "transition": previous.get("transition") if previous else None}

    def cancel_rollback(self, request_id):
        """Explicit record-only escape with a verified current Service grant.

        Also covers interruption before the original revoke was offered.
        Never issues a revoke, clears a native revoke, changes mode or touches
        the manager. A later passive request is a new explicit owner intent.
        """
        require(self.requested_mode is None, "rollback cancellation cannot select a mode")
        try:
            require(str(uuid.UUID(request_id)) == request_id, "pass the original pending rollback UUID")
        except (TypeError, ValueError, AttributeError):
            raise Refusal("pass --request-id with the original pending rollback UUID") from None
        require(self.ownership() is not None, "no owned desktop registration to reconcile")
        parent = self.destination.parent.stat()
        require(parent.st_uid == os.geteuid() and not parent.st_mode & 0o022,
                "desktop unit directory is writable by another user; kept")
        fd = os.open(self.destination.with_name(".hgs-desktop.registration.lock"),
                     os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            info = os.fstat(fd)
            require(stat.S_ISREG(info.st_mode) and info.st_uid == os.geteuid() and info.st_nlink == 1
                    and stat.S_IMODE(info.st_mode) == 0o600, "invalid desktop installer lock")
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            previous = self.ownership()
            require(previous is not None, "owned registration disappeared")
            transition = previous.get("transition")
            require((transition is not None and transition["request_id"] == request_id)
                    or (transition is None and previous.get("last_cancelled_rollback_request_id") == request_id),
                    "pending rollback UUID changed; inspect status before cancellation")
            self.caller()
            self.grant()  # An actual current Service grant, never background_permitted alone.
            original = read_file(self.destination)
            require(self.ownership() == previous, "registration changed during cancellation")
            self.caller()
            self.grant()
            require(read_file(self.destination) == original, "unit changed during cancellation")
            if transition is not None:
                record = dict(previous, last_cancelled_rollback_request_id=request_id)
                record.pop("transition")
                self.save(record)
            return "Pending rollback metadata cancelled. Native receipts and selected mode were kept; no service-manager action."
        finally:
            os.close(fd)

    def rollback(self, previous):
        help_text = self.run([self.hgs, "--help"], self.env)
        require(_hold.MARKER in (line.strip() for line in help_text.splitlines()),
                "selected CLI lacks the native rollback hold; update the CLI before changing mode")
        # Persist before the first effect. An interrupted attempt can only use
        # this SAME revoke ID; it cannot silently become a fresh permission.
        transition = previous.get("transition")
        if transition is None:
            transition = {"target": "passive", "request_id": str(uuid.uuid4())}
            previous = dict(previous, transition=transition)
            self.save(previous)
        value = self.cli("revoke", "--json", "--request-id", transition["request_id"], "--reason", "rollback")
        require(value.get("request_id") == transition["request_id"]
                and value.get("configuration") == self.configuration
                and all(value.get(key) is True for key in ("accepted", "durable", "revoked", "checkpointed")),
                "original rollback revoke has not settled; keep the same pending transition")
        hold = self.hold_factory(self.hgs, dict(self.env, **self.registration["readiness"]["environment"]),
                                 self.configuration, transition["request_id"], value,
                                 help_text)
        try:
            self.rollback_hold = hold.acquire()
        except Refusal as error:
            raise Refusal(f"{error} If a new frontend Grant/Rearm won, explicitly cancel-rollback "
                          f"--request-id {transition['request_id']} before choosing a new mode.") from error
        return transition

    def install(self, *, check=False, no_start=False, session=False, refresh=False):
        require(self.requested_mode is None or not refresh, "refresh preserves mode; use install --mode for an explicit transition")
        previous = self.ownership()
        if refresh and previous is None:
            return "No managed desktop registration; CLI-only installation kept without a new service."
        state = self.state()
        if check:
            return "Desktop service registration is valid."
        self.destination.parent.mkdir(parents=True, exist_ok=True)
        parent = self.destination.parent.stat()
        require(parent.st_uid == os.geteuid() and not parent.st_mode & 0o022,
                "desktop unit directory is writable by another user; kept")
        lock = self.destination.with_name(".hgs-desktop.registration.lock")
        fd = os.open(lock, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
        try:
            info = os.fstat(fd)
            require(info.st_uid == os.geteuid() and info.st_nlink == 1 and stat.S_ISREG(info.st_mode)
                    and stat.S_IMODE(info.st_mode) == 0o600,
                    "desktop installer lock is not a private owned file")
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            previous = self.ownership()
            state = self.state()
            require(self.requested_mode is not None or previous is None
                    or previous.get("mode", "passive") == self.mode,
                    "selected mode changed while waiting for registration; inspect status and retry")
            explicit = self.requested_mode is not None and not check
            transition = previous.get("transition") if previous else None
            require(not transition or (explicit and self.mode == "passive"),
                    "pending rollback: repeat the original --mode passive transition; do not rearm or change modes")
            if explicit:
                self.caller()
                require(not no_start and not refresh and graphics(self.env) is not None,
                        "explicit mode changes require the current graphical session and cannot use --no-start/refresh")
                if self.mode == "background":
                    require(previous is not None, "register the passive service and complete frontend Grant/Rearm first")
                    self.grant()
            if refresh:
                require(previous is not None, "managed registration disappeared; kept")
                if (previous["binary_sha256"] == self.binary
                        and previous["definition_sha256"] == digest(self.definition)
                        and not previous["pending_restart"]):
                    return "Desktop backend unchanged; existing service kept."
            active = state.get("ActiveState") == "active"
            if active:
                require(read_file(self.destination) is not None,
                        "active desktop service has no owned definition; kept for inspection")
                current = self.probe()
                require(current.get("ipc_ready") is True, "active desktop service is unavailable; kept for inspection")
                require(str(current.get("pid")) == state.get("MainPID"), "desktop service PID/readiness mismatch; kept")
            if explicit and self.mode == "passive" and previous is not None and (
                    previous.get("mode", "passive") == "background" or transition
                    or (active and current.get("mode") == "background")):
                transition = self.rollback(previous)
            context = None if no_start or (refresh and not active) else graphics(self.env)
            changed = previous is None or previous.get("binary_sha256") != self.binary or previous.get("definition_sha256") != digest(self.definition)
            record = {"schema": 1, "configuration": self.configuration, "definition_sha256": digest(self.definition),
                      "binary_sha256": self.binary, "pending_restart": bool(active and (changed or previous.get("pending_restart", False))),
                      "session": previous.get("session") if previous else None, "mode": self.mode}
            if transition is not None:
                record["transition"] = transition
            last_revoke = transition["request_id"] if transition else (previous or {}).get("last_revoke_request_id")
            if last_revoke:
                record["last_revoke_request_id"] = last_revoke
            if previous and previous.get("last_cancelled_rollback_request_id"):
                record["last_cancelled_rollback_request_id"] = previous["last_cancelled_rollback_request_id"]
            if session:
                require(previous is not None, "register the desktop service before starting its session")
            if explicit:
                self.caller()
                if self.mode == "background":
                    self.grant()
            if read_file(self.destination) != self.definition:
                self.fence()
                write_file(self.destination, self.definition)
                self.fence()
            self.save(record)
            if explicit:
                self.caller()
                if self.mode == "background":
                    self.grant()
            self.manager("daemon-reload")
            self.state()  # Refuse any newly loaded custom fragment or drop-in.
            if not refresh:
                self.manager("enable", UNIT)
            elif not active:
                return "Managed desktop registration refreshed; inactive service kept stopped."
            if context is None:
                if active and record["pending_restart"]:
                    raise PendingRestart("backend update is pending; run registration from the current graphical session")
                return "Desktop service registered for the next graphical login; IPC readiness was not established."
            needs_restart = active and (record["pending_restart"] or record["session"] != context)
            if context["XDG_SESSION_TYPE"] == "x11":
                self.manager("unset-environment", "WAYLAND_DISPLAY")
            # Import only these explicit session keys; never GUI-specific loader
            # paths or arbitrary HGS_* variables into the shared user manager.
            self.manager("import-environment", *sorted(context))
            if not active or needs_restart:
                record["pending_restart"] = True
                self.save(record)
                self.manager("restart" if active else "start", UNIT)
            deadline = time.monotonic() + 10
            while True:
                remaining = deadline - time.monotonic()
                require(remaining > 0, "desktop IPC is not ready; inspect hgs-desktop.service")
                lifecycle = self.probe(timeout=remaining)
                if lifecycle["ipc_ready"]:
                    if explicit:
                        require(lifecycle.get("mode") == self.mode,
                                "selected desktop mode is not running; keep pending_restart and inspect the service")
                    break
                time.sleep(min(0.2, max(0, deadline - time.monotonic())))
            served = self.state()
            require(served.get("ActiveState") == "active" and served.get("MainPID") == str(lifecycle.get("pid")),
                    "readiness does not belong to the registered desktop service")
            require(binary_digest(self.hgs) == self.binary, "CLI changed during service activation; inspect before retrying")
            if explicit and self.mode == "background":
                self.grant()
                require(self.last_status.get("background_start", {}).get("durably_blocked") is not True,
                        "background handoff is blocked; complete authenticated Rearm, never clear the revoke")
            if self.rollback_hold is not None:
                self.fence()
                self.rollback_hold.close()
                self.rollback_hold = None
            record.update(session=context, pending_restart=False)
            record.pop("transition", None)
            self.save(record)
            return "Desktop service IPC ready. Background workers have their separate lifecycle status."
        finally:
            try:
                if self.rollback_hold is not None:
                    self.rollback_hold.close()
            finally:
                os.close(fd)  # Retain the installer lock through the original hold's join.


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("install", "session", "refresh", "status", "cancel-rollback"), nargs="?", default="install")
    parser.add_argument("--hgs", default=shutil.which("hgs"))
    parser.add_argument("--config-dir")
    parser.add_argument("--state-dir")
    parser.add_argument("--check", action="store_true", help="validate ownership/registration without changing anything")
    parser.add_argument("--no-start", action="store_true", help="register for the next graphical login without starting/restarting")
    parser.add_argument("--mode", choices=("passive", "background"), help="explicit owner selection; omitted preserves the registered mode")
    parser.add_argument("--json", action="store_true", help="stable configuration-scoped receipt for the native Settings action")
    parser.add_argument("--request-id", help="with cancel-rollback: the original pending rollback UUID to cancel")
    args = parser.parse_args()
    instance = None
    def failed(code, error):
        if args.json:
            record = None
            if instance is not None:
                try:
                    raw = read_file(instance.record_path)
                    record = json.loads(raw) if raw is not None else None
                except (Refusal, OSError, ValueError):
                    pass
            print(json.dumps({"schema": 1, "operation": args.action, "ok": False,
                              "code": code, "message": str(error),
                              "configuration": instance.configuration if instance else None,
                              "selected_mode": record.get("mode", "passive") if isinstance(record, dict) else None,
                              "running_mode": None, "ipc_ready": None,
                              "pending_restart": record.get("pending_restart") if isinstance(record, dict) else None,
                              "last_revoke_request_id": record.get("last_revoke_request_id") if isinstance(record, dict) else None,
                              "transition": record.get("transition") if isinstance(record, dict) else None}))
    try:
        require(sys.platform.startswith("linux") and os.geteuid() != 0, "run as the Linux desktop user, without sudo")
        require(args.hgs, "selected hgs not found; pass --hgs PATH")
        env = dict(os.environ)
        for name, value in [("HGS_CONFIG_DIR", args.config_dir), ("HGS_STATE_DIR", args.state_dir)]:
            if value is not None:
                require(Path(value).is_absolute(), "desktop configuration paths must be absolute")
                env[name] = value
        require(args.mode is None or args.action in ("install", "session"),
                "only install/session can explicitly select a mode; status and refresh preserve it")
        require((args.action == "cancel-rollback") == (args.request_id is not None),
                "cancel-rollback requires its original --request-id; other operations cannot supply it")
        require(args.action != "cancel-rollback" or not (args.check or args.no_start),
                "rollback cancellation cannot use check/no-start")
        # A deliberate CLI upgrade must not discover/adopt a service merely
        # because the new CLI can render one. No manager or CLI probe if absent.
        record = destination_for(env).with_name(".hgs-desktop.zerus-registration.json")
        if args.action == "refresh" and read_file(record) is None:
            print("No managed desktop registration; CLI-only installation kept without a new service.")
            return 0
        require(args.action != "status" or args.mode is None, "status cannot select a mode")
        instance = Installer(args.hgs, env, mode=args.mode)
        if args.action == "status":
            print(json.dumps(instance.status()))
        else:
            result = instance.cancel_rollback(args.request_id) if args.action == "cancel-rollback" else instance.install(
                check=args.check, no_start=args.no_start, session=args.action == "session", refresh=args.action == "refresh")
            if args.json:
                print(json.dumps(dict(instance.status(), operation=args.action, ok=True, message=result)))
            else:
                print(result)
        return 0
    except PendingRestart as error:
        failed("restart_pending", error)
        print(f"Desktop service refresh pending: {error}", file=sys.stderr)
        return 3
    except (Refusal, OSError, ValueError, subprocess.SubprocessError) as error:
        failed("registration_refused", error)
        print(f"Desktop service registration refused: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
