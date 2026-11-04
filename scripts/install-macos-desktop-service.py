#!/usr/bin/env python3
"""Register the selected hgs desktop service in this user's Aqua login session."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import re
import selectors
import stat
import subprocess
import sys
import tempfile
import time
import uuid
from contextlib import contextmanager

if os.name == "posix":
    import fcntl
else:
    fcntl = None

LABEL = "com.hgdev.hgs-desktop"
ABI = "desktop-lifecycle-v1"
MARKER = "Zerus desktop registration v1"
LIMIT = 128 * 1024


_spec = importlib.util.spec_from_file_location("desktop_revoke_hold", Path(__file__).with_name("desktop-revoke-hold.py"))
_hold = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_hold)
Refusal = _hold.Refusal


def require(condition, message):
    if not condition:
        raise Refusal(message)


def run(argv, env=None, timeout=10):
    """Bound both pipes and the lifetime of this one owned command."""
    with subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, env=env, close_fds=True) as child:
        output = {child.stdout: bytearray(), child.stderr: bytearray()}
        try:
            with selectors.DefaultSelector() as selector:
                for stream in output:
                    os.set_blocking(stream.fileno(), False)
                    selector.register(stream, selectors.EVENT_READ)
                deadline = time.monotonic() + timeout
                while selector.get_map():
                    remaining = deadline - time.monotonic()
                    require(remaining > 0, "Desktop service command timed out; inspect its status before retrying.")
                    for key, _ in selector.select(min(remaining, 0.2)):
                        stream = key.fileobj
                        chunk = os.read(stream.fileno(), 8192)
                        if not chunk:
                            selector.unregister(stream)
                        else:
                            output[stream].extend(chunk)
                            require(len(output[stream]) <= LIMIT, "Desktop service command output exceeds its bound.")
                child.wait(timeout=max(0.01, deadline - time.monotonic()))
            return subprocess.CompletedProcess(argv, child.returncode,
                output[child.stdout].decode("utf-8", "strict"), output[child.stderr].decode("utf-8", "replace"))
        finally:
            if child.poll() is None:
                child.kill()  # Only this Popen child; never a process group or native agent.
                child.wait()


def environment(binary, home, source):
    values = {"HOME": str(home), "PATH": source.get("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"),
              "HGS_EXECUTABLE": str(binary)}
    for name in ("HGS_CONFIG_DIR", "HGS_STATE_DIR", "XDG_CONFIG_HOME", "XDG_DATA_HOME",
                 "XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR", "TMPDIR", "TMUX_TMPDIR"):
        if source.get(name):
            values[name] = source[name]
    return values


def ordinary(path, directory=False):
    info = path.lstat()
    kind = stat.S_ISDIR if directory else stat.S_ISREG
    require(kind(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o022,
            "Desktop service path is not an ordinary user-owned file/directory.")
    if not directory:
        require(info.st_nlink == 1, "Desktop service definition has another hard link.")
    return info


def read_definition(path):
    try:
        before = ordinary(path)
    except FileNotFoundError:
        return None
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        current = os.fstat(descriptor)
        require((before.st_dev, before.st_ino) == (current.st_dev, current.st_ino)
                and stat.S_ISREG(current.st_mode), "Desktop service definition changed while opening it.")
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            content = stream.read(LIMIT + 1)
        require(len(content) <= LIMIT, "Desktop service definition exceeds its bound.")
        return content, current.st_dev, current.st_ino
    finally:
        os.close(descriptor)


def registration(binary, home, env, command=run, mode="passive"):
    result = command([str(binary), "desktop", "registration", "--json"]
                     + (["--background"] if mode == "background" else []), env=env)
    require(result.returncode == 0, "Cannot obtain desktop service registration from the selected hgs CLI.")
    value = json.loads(result.stdout)
    require(isinstance(value, dict) and value.get("schema") == 1 and value.get("operation") == "preview"
            and value.get("label") == LABEL, "Unsupported desktop registration definition.")
    destination = home / "Library/LaunchAgents" / (LABEL + ".plist")
    require(value.get("destination") == str(destination), "Desktop registration is outside this user's LaunchAgents.")
    config = value.get("configuration", "")
    require(isinstance(config, str) and re.fullmatch(r"[0-9a-f]{64}", config), "Invalid desktop configuration identity.")
    argv = value.get("argv")
    require(isinstance(argv, list) and len(argv) == (9 if mode == "background" else 8)
            and argv[:3] == [str(binary), "desktop", "serve"]
            and argv[3] == "--config-dir" and argv[5] == "--state-dir"
            and argv[7:] == (["--background", "--launchd"] if mode == "background" else ["--launchd"])
            and all(isinstance(part, str) and part and not any(ord(c) < 32 for c in part) for part in argv)
            and Path(argv[4]).is_absolute() and Path(argv[6]).is_absolute(), "Unexpected desktop service command.")
    require(value.get("mode", "passive") == mode, "Desktop registration mode differs from selection.")
    expected = {"Label": LABEL, "ProgramArguments": argv, "RunAtLoad": True,
                "KeepAlive": {"SuccessfulExit": False}, "ThrottleInterval": 10,
                "AbandonProcessGroup": True, "LimitLoadToSessionType": "Aqua"}
    require(isinstance(value.get("definition"), str), "Missing desktop LaunchAgent definition.")
    content = value["definition"].encode("utf-8")
    require(len(content) <= LIMIT and f"{MARKER}; configuration={config}".encode() in content
            and plistlib.loads(content) == expected, "Unexpected Aqua LaunchAgent definition.")
    probe = value.get("readiness")
    require(probe == {"abi": ABI, "argv": [str(binary), "desktop", "status", "--json"],
        "environment": {"HGS_CONFIG_DIR": argv[4], "HGS_STATE_DIR": argv[6]},
        "expected_configuration": config, "configuration_field": "lifecycle.configuration",
        "ready_field": "lifecycle.ipc_ready", "epoch_field": "lifecycle.epoch",
        "generation_field": "lifecycle.generation"}, "Unsupported desktop readiness interface.")
    require(value.get("session") == {"kind": "aqua", "notification_bundle": "com.hgdev.hgs-tray"},
            "Desktop service requires the Aqua session and the existing notification identity.")
    return value, destination, content


def owned(content, value):
    if content is None:
        return None
    definition = plistlib.loads(content)
    markers = re.findall(rb"<!--\s*" + re.escape(MARKER.encode())
                         + rb"; configuration=([0-9a-f]{64})\s*-->", content)
    require(markers == [value["configuration"].encode()] and isinstance(definition, dict)
            and definition.get("Label") == LABEL,
            "Existing desktop LaunchAgent is custom or belongs to another configuration; it was kept.")
    argv = definition.get("ProgramArguments")
    require(isinstance(argv, list) and len(argv) >= 3 and argv[1:3] == ["desktop", "serve"]
            and all(isinstance(part, str) and part and not any(ord(c) < 32 for c in part) for part in argv)
            and Path(argv[0]).is_absolute(), "Owned desktop definition has invalid arguments; it was kept.")
    return definition


def loaded_identity(printed, destination, argv):
    def field(name):
        values = re.findall(r"(?m)^\t" + name + r" = (.+)$", printed)
        return values[0] if len(values) == 1 else None
    blocks = re.findall(r"(?ms)^\targuments = \{\n(.*?)^\t\}", printed)
    arguments = [line[2:] for line in blocks[0].splitlines()] if len(blocks) == 1 else None
    require(field("path") == str(destination) and field("program") == argv[0] and arguments == argv,
            "Loaded desktop LaunchAgent does not match the owned definition; it was kept.")
    pid = field("pid")
    return int(pid) if pid and pid.isdecimal() else None


def ready(value, status, pid):
    require(isinstance(status, dict) and isinstance(status.get("lifecycle"), dict),
            "Missing desktop lifecycle status.")
    lifecycle = status.get("lifecycle", {})
    require(lifecycle.get("abi") == ABI and lifecycle.get("configuration") == value["configuration"],
            "Desktop lifecycle ABI/configuration differs; reconcile the selected service before retrying.")
    if lifecycle.get("ipc_ready") is not True:
        return False
    require(type(lifecycle.get("pid")) is int and lifecycle["pid"] > 0
            and lifecycle["pid"] == pid and isinstance(lifecycle.get("epoch"), str) and lifecycle["epoch"]
            and type(lifecycle.get("generation")) is int and lifecycle["generation"] >= 0,
            "Desktop readiness does not belong to the loaded service actor.")
    return True


def unpaired_rollback(binary):
    """Recognize only an existing unpaired app at the old registered location."""
    macos, contents, app = binary.parent, binary.parent.parent, binary.parent.parent.parent
    if (binary.name != "hgs" or macos.name != "MacOS" or contents.name != "Contents"
            or app.suffix != ".app" or os.path.lexists(binary)
            or os.path.lexists(contents / "Resources/zerus-local-pair.json")):
        return False
    try:
        for directory in (app, contents, macos):
            ordinary(directory, directory=True)
        ordinary(macos / "hgs-tray")
        info = read_definition(contents / "Info.plist")
        if info is None:
            return False
        info = plistlib.loads(info[0])
        return (info.get("CFBundleIdentifier") == "com.hgdev.hgs-tray"
                and info.get("CFBundleExecutable") == "hgs-tray")
    except (OSError, ValueError, Refusal):
        return False


def selected_mode(definition):
    if definition is None:
        return "passive"
    argv = definition["ProgramArguments"]
    require(len(argv) in (8, 9) and argv[3] == "--config-dir" and argv[5] == "--state-dir"
            and Path(argv[4]).is_absolute() and Path(argv[6]).is_absolute(),
            "Owned desktop arguments cannot establish a mode; they were kept.")
    if argv[7:] == ["--launchd"]:
        return "passive"
    require(argv[7:] == ["--background", "--launchd"], "Owned desktop mode is unknown; it was kept.")
    return "background"


def save_record(path, value):
    raw = (json.dumps(value, indent=2) + "\n").encode()
    require(len(raw) <= LIMIT, "Desktop registration record exceeds its bound.")
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stage:
        temporary = Path(stage.name)
        try:
            stage.write(raw); stage.flush(); os.fsync(stage.fileno())
            os.fchmod(stage.fileno(), 0o600)
            os.replace(temporary, path)
            fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
        finally:
            temporary.unlink(missing_ok=True)


class ModeTransition:
    """Bounded registration metadata; native ownership remains in the actor."""
    def __init__(self, binary, env, source, value, destination, previous, content, mode, command, hold_factory=None):
        self.binary, self.env, self.source = binary, dict(env, **value["readiness"]["environment"]), source
        self.value, self.destination, self.content, self.mode = value, destination, content, mode
        self.command = command
        self.hold_factory = hold_factory or _hold.Hold
        self.hold = None
        self.path = destination.with_name("." + LABEL + ".zerus-registration.json")
        self.original_definition = previous
        self.record = None
        old = read_definition(self.path)
        self.record_identity = old
        if old is not None:
            record = json.loads(old[0])
            require(isinstance(record, dict) and type(record.get("schema")) is int and record["schema"] == 1
                    and record.get("configuration") == value["configuration"]
                    and record.get("selected_mode") in ("passive", "background")
                    and type(record.get("pending_restart")) is bool
                    and isinstance(record.get("definition_sha256"), str)
                    and re.fullmatch(r"[0-9a-f]{64}", record["definition_sha256"]),
                    "Invalid or foreign desktop mode record; kept.")
            transition = record.get("transition")
            if transition is not None:
                require(isinstance(transition, dict) and set(transition) == {"target", "request_id"}
                        and transition["target"] == "passive" and self.valid_uuid(transition["request_id"]),
                        "Invalid original rollback transition; kept.")
            require(record.get("last_revoke_request_id") is None
                    or self.valid_uuid(record["last_revoke_request_id"]), "Invalid original revoke ID; kept.")
            require(record.get("last_cancelled_rollback_request_id") is None
                    or self.valid_uuid(record["last_cancelled_rollback_request_id"]),
                    "Invalid cancelled rollback ID; kept.")
            require(previous is not None and (hashlib.sha256(previous[0]).hexdigest() == record.get("definition_sha256")
                    or (record["pending_restart"] and previous[0] == content)),
                    "Desktop definition changed outside the owned transition; kept.")
            self.record = record

    @staticmethod
    def valid_uuid(value):
        try:
            return str(uuid.UUID(value)) == value
        except (ValueError, TypeError, AttributeError):
            return False

    def query(self, *args):
        result = self.command([str(self.binary), "desktop", *args], env=self.env)
        require(result.returncode == 0, result.stderr.strip() or "Desktop mode command refused; inspect the original transition.")
        value = json.loads(result.stdout)
        require(isinstance(value, dict), "Invalid desktop mode reply.")
        return value

    def caller(self):
        require(not any(k in self.source for k in ("HGS_SESSION", "HGS_RUN_ID")),
                "Agent sessions cannot change desktop mode; use Settings or a terminal outside Zerus.")
        value = self.query("registration", "mode-check", "--json")
        require(value.get("schema") == 1 and value.get("configuration") == self.value["configuration"]
                and value.get("admitted") is True and value.get("source") in ("interactive_terminal", "installed_frontend"),
                "Desktop mode changes require Settings or a foreground terminal outside Zerus.")

    def grant(self):
        value = self.query("ownership-status", "--json")
        require(value.get("abi") == "desktop-ownership-v1" and value.get("configuration") == self.value["configuration"]
                and value.get("state") == "service" and value.get("identity_current") is True
                and value.get("rearm_required") is False
                and all(self.valid_uuid(value.get(k)) for k in ("journal", "request_id", "owner_epoch"))
                and type(value.get("revision")) is int and 0 < value["revision"] < 2 ** 53,
                "Complete authenticated frontend Grant/Rearm first; this installer never clears retained revoke.")

    def prepare(self, previous, old_mode):
        record = self.record
        require(not record or not record.get("transition") or self.mode == "passive",
                "Resume the same --mode passive rollback; no new mode may replace its original ID.")
        require(self.mode is not None or not record or record["selected_mode"] == old_mode,
                "Interrupted mode change; resume its explicit --mode selection.")
        if self.mode is None:
            return
        self.caller()
        if self.mode == "background":
            self.grant()
        if record is None:
            record = {"schema": 1, "configuration": self.value["configuration"],
                      "selected_mode": old_mode, "pending_restart": False,
                      "definition_sha256": hashlib.sha256(previous[0]).hexdigest(), "transition": None}
        if self.mode == "passive" and (old_mode == "background" or record.get("transition")):
            help_result = self.command([str(self.binary), "--help"], env=self.env)
            require(help_result.returncode == 0
                    and _hold.MARKER in (line.strip() for line in help_result.stdout.splitlines()),
                    "Selected CLI lacks the native rollback hold; update it before changing mode.")
            if not record.get("transition"):
                record["transition"] = {"target": "passive", "request_id": str(uuid.uuid4())}
                self.persist(record)
            original = record["transition"]["request_id"]
            result = self.query("revoke", "--json", "--request-id", original, "--reason", "rollback")
            require(result.get("request_id") == original and result.get("configuration") == self.value["configuration"]
                    and all(result.get(k) is True for k in ("accepted", "durable", "revoked", "checkpointed")),
                    "Original revoke/drain is not settled; keep the same rollback ID.")
            self.hold = self.hold_factory(self.binary, self.env, self.value["configuration"], original,
                                          result, help_result.stdout)
            try:
                self.hold.acquire()
            except Refusal as error:
                raise Refusal(f"{error} Inspect status; cancel-rollback --request-id {original} "
                              "requires a verified current Service grant before a new mode choice.") from error
            record["last_revoke_request_id"] = original
        record.update(selected_mode=self.mode, pending_restart=bool(record["pending_restart"] or old_mode != self.mode))
        self.record = record
        self.persist(record)

    def fence(self):
        if self.hold is not None:
            self.hold.check()

    def persist(self, record):
        self.revalidate()
        require(read_definition(self.path) == self.record_identity,
                "Desktop mode record changed during the transition; kept.")
        save_record(self.path, record)
        self.record_identity = read_definition(self.path)
        require(self.record_identity is not None and json.loads(self.record_identity[0]) == record,
                "Desktop mode record changed after its write; inspect the original transition.")
        self.revalidate()

    def revalidate(self):
        self.fence()
        if self.mode is not None:
            self.caller()
            if self.mode == "background":
                self.grant()
        self.fence()

    def close(self, *, validate=False):
        if self.hold is not None:
            self.hold.close(validate=validate)
            self.hold = None

    def finish(self):
        self.fence()
        self.close(validate=True)
        if self.record is not None:
            self.record.update(selected_mode=self.value.get("mode", "passive"), pending_restart=False,
                definition_sha256=hashlib.sha256(self.content).hexdigest(), transition=None)
            self.persist(self.record)

    def cancel(self, request_id, *, check=False):
        require(self.mode is None and self.valid_uuid(request_id), "Pass the original pending rollback UUID only.")
        require(self.record is not None, "No owned mode record to cancel.")
        transition = self.record.get("transition")
        require((transition is not None and transition["request_id"] == request_id)
                or (transition is None and self.record.get("last_cancelled_rollback_request_id") == request_id),
                "Pending rollback UUID changed; inspect status before cancellation.")
        if check:
            return "checked"
        original_record, original_definition = read_definition(self.path), read_definition(self.destination)
        require(original_record == self.record_identity and original_definition == self.original_definition,
                "Owned registration changed before cancellation; kept.")
        self.caller()
        self.grant()
        require(read_definition(self.path) == original_record
                and read_definition(self.destination) == original_definition,
                "Owned registration changed during cancellation; kept.")
        self.caller()
        self.grant()
        require(read_definition(self.path) == original_record
                and read_definition(self.destination) == original_definition,
                "Owned registration changed during cancellation; kept.")
        if transition is not None:
            self.record.update(transition=None, last_cancelled_rollback_request_id=request_id)
            self.persist(self.record)
        return {"schema": 1, "operation": "cancel-rollback", "ok": True,
                "configuration": self.value["configuration"], "label": LABEL, "registered": True,
                "selected_mode": self.record["selected_mode"], "running_mode": None, "ipc_ready": None,
                "pending_restart": self.record["pending_restart"], "transition": None,
                "last_revoke_request_id": self.record.get("last_revoke_request_id"),
                "last_cancelled_rollback_request_id": request_id,
                "message": "Pending rollback metadata cancelled; native receipts, selected mode and manager kept."}

    def status(self, printed, old_definition):
        mode = selected_mode(old_definition)
        lifecycle = {}
        if printed is not None and printed.returncode == 0:
            pid = loaded_identity(printed.stdout, self.destination, old_definition["ProgramArguments"])
            result = self.query("status", "--json")
            ready(self.value, result, pid)
            lifecycle = result["lifecycle"]
        record = self.record or {}
        return {"schema": 1, "operation": "status", "configuration": self.value["configuration"], "label": LABEL,
                "registered": old_definition is not None, "selected_mode": record.get("selected_mode", mode),
                "running_mode": lifecycle.get("mode"), "ipc_ready": lifecycle.get("ipc_ready", False),
                "pending_restart": record.get("pending_restart", False), "transition": record.get("transition"),
                "last_revoke_request_id": record.get("last_revoke_request_id"),
                "last_cancelled_rollback_request_id": record.get("last_cancelled_rollback_request_id")}

@contextmanager
def registration_lock(home):
    require(fcntl is not None, "Desktop LaunchAgent registration requires Unix.")
    ordinary(home, directory=True)
    directory = home / "Library/LaunchAgents"
    for path in (home / "Library", directory):
        path.mkdir(mode=0o755, exist_ok=True)
        ordinary(path, directory=True)
    fd = os.open(directory / ("." + LABEL + ".registration.lock"),
                 os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
                and stat.S_IMODE(info.st_mode) == 0o600, "Desktop registration lock is not private; kept.")
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield
    finally:
        os.close(fd)


def install(binary, home, source, restart=False, check_only=False, remove=False,
            command=run, clock=time.monotonic, sleep=time.sleep, runner_hgs=None, refresh_runner=False,
            mode=None, status_only=False, cancel_rollback=None, hold_factory=None):
    args = dict(restart=restart, check_only=check_only, remove=remove, command=command, clock=clock,
                sleep=sleep, runner_hgs=runner_hgs, refresh_runner=refresh_runner, mode=mode, status_only=status_only,
                cancel_rollback=cancel_rollback, hold_factory=hold_factory)
    if cancel_rollback is not None:
        require(mode is None and not (restart or check_only or remove or refresh_runner or status_only),
                "Rollback cancellation cannot select a mode or perform another action.")
        _install(binary, home, source, **dict(args, check_only=True))
        with registration_lock(home):
            return _install(binary, home, source, **args)
    if check_only or status_only or (remove and not os.path.lexists(home / "Library/LaunchAgents" / (LABEL + ".plist"))):
        return _install(binary, home, source, **args)
    # Validate scope, existing definitions and paired selection before creating
    # directories/lock files. Repeat the full preflight under the lock.
    _install(binary, home, source, **dict(args, check_only=True, mode=None))
    with registration_lock(home):
        return _install(binary, home, source, **args)


def _install(binary, home, source, restart=False, check_only=False, remove=False,
             command=run, clock=time.monotonic, sleep=time.sleep, runner_hgs=None, refresh_runner=False,
             mode=None, status_only=False, cancel_rollback=None, hold_factory=None):
    require(mode in (None, "passive", "background") and (mode is None or not (remove or refresh_runner or status_only or check_only)),
            "Explicit mode selection cannot be combined with remove/check/status/runner refresh.")
    require(home.is_absolute(), "Desktop service HOME must be absolute.")
    binary = Path(os.path.abspath(binary))  # Retain the maintained CLI path, including a cask symlink.
    require(binary.is_file() and os.access(binary, os.X_OK), "Desktop service requires an executable hgs CLI.")
    ordinary(home, directory=True)
    for directory in (home / "Library", home / "Library/LaunchAgents"):
        if os.path.lexists(directory):
            ordinary(directory, directory=True)
    env = environment(binary, home, source)
    value, destination, content = registration(binary, home, env, command)
    previous = read_definition(destination)
    previous_definition = owned(previous[0] if previous is not None else None, value)
    old_mode = selected_mode(previous_definition)
    if refresh_runner and previous_definition is not None:
        # Root CLI-only installs must not change a maintained paired backend
        # back into the unsigned PATH CLI. Its owned registration is the
        # selection; this is not runtime signature or notification authority.
        old_environment = previous_definition.get("EnvironmentVariables", {})
        require(isinstance(old_environment, dict), "The owned desktop environment is invalid; it was kept.")
        old_runner = old_environment.get("HGS_EXECUTABLE")
        if old_runner is not None:
            require(isinstance(old_runner, str) and Path(old_runner).is_absolute(),
                    "The owned desktop runner selection is invalid; it was kept.")
            backend = Path(previous_definition["ProgramArguments"][0])
            if not unpaired_rollback(backend):
                ordinary(backend)
                return _install(backend, home, source,
                               check_only=check_only, remove=remove, command=command, clock=clock,
                               sleep=sleep, runner_hgs=binary, status_only=status_only, hold_factory=hold_factory)
            # update-installed.py can restore an old Qt/unpaired app. Its
            # missing helper is deliberate, so restore the ordinary backend
            # while retaining its owned mode. A remaining pair record,
            # missing app, link or present-but-broken helper never takes this path.
    desired_mode = mode or old_mode
    if desired_mode == "background":
        value, destination, content = registration(binary, home, env, command, desired_mode)
    if previous_definition is not None:
        old_environment = previous_definition.get("EnvironmentVariables", {})
        if mode is not None:
            require(isinstance(old_environment, dict), "Custom desktop environment kept; review before changing mode.")
            old_runner = old_environment.get("HGS_EXECUTABLE")
            require(runner_hgs is None or str(Path(os.path.abspath(runner_hgs))) == old_runner,
                    "A mode transition cannot change the independent runner; reconcile the ordinary installation first.")
        # An unchanged paired backend must not lose its runner merely because
        # a GUI-only refresh omitted an already selected --runner-hgs.
        if runner_hgs is None and isinstance(old_environment, dict) and (
                mode is not None or previous_definition["ProgramArguments"][0] == str(binary)):
            runner_hgs = old_environment.get("HGS_EXECUTABLE")
    if runner_hgs is not None:
        runner_hgs = Path(os.path.abspath(runner_hgs))
        ordinary(runner_hgs)
        require(os.access(runner_hgs, os.X_OK) and runner_hgs != binary
                and not os.path.samefile(binary, runner_hgs),
                "Desktop runner must be an independent executable CLI outside the bundle.")
        require(not any(part.endswith(".app") for part in runner_hgs.parts),
                "Desktop runner must stay outside the replaceable application.")
        # Query/validate the exact service image BEFORE applying the ordinary
        # runner override: registration itself honors HGS_EXECUTABLE.
        definition = plistlib.loads(content)
        definition["EnvironmentVariables"] = {"HGS_EXECUTABLE": str(runner_hgs)}
        content = plistlib.dumps(definition).replace(b"?>", b"?>\n<!-- " + MARKER.encode()
                    + b"; configuration=" + value["configuration"].encode() + b" -->", 1)
    transition = ModeTransition(binary, env, source, value, destination, previous, content, mode, command,
                                hold_factory=hold_factory)
    def guarded(argv, **kwargs):
        transition.fence()
        result = command(argv, **kwargs)
        transition.fence()
        return result
    transition.command = guarded
    if cancel_rollback is not None:
        return transition.cancel(cancel_rollback, check=check_only)
    try:
        domain = f"gui/{os.getuid()}"
        target = domain + "/" + LABEL
        def launchctl(*args):
            return guarded(["launchctl", *args], env=env)
        gui = launchctl("print", domain).returncode == 0
        printed = launchctl("print", target) if gui else None
        loaded = printed is not None and printed.returncode == 0
        changed = previous is None or previous[0] != content
        if loaded:
            require(previous is not None, "Desktop label is loaded without an owned definition; it was kept.")
            loaded_identity(printed.stdout, destination, previous_definition["ProgramArguments"])
            if remove or restart or changed:
                require(previous_definition.get("AbandonProcessGroup") is True,
                        "The old desktop definition cannot preserve its process group during unload; it was kept.")
        require(read_definition(destination) == previous, "Desktop definition changed during preflight; it was kept.")
        if check_only:
            return "checked"
        if status_only:
            return transition.status(printed, previous_definition)
        if remove and previous is None:
            return "unchanged"
        if mode is not None:
            require(gui and previous is not None, "Mode changes require the owned passive registration in this user's Aqua login.")
            # A mode change cannot also adopt or overwrite custom plist controls.
            old_plan, _, _ = registration(binary, home, env, command, selected_mode(previous_definition))
            expected = plistlib.loads(old_plan["definition"].encode())
            old_environment = previous_definition.get("EnvironmentVariables")
            if old_environment is not None:
                require(isinstance(old_environment, dict) and set(old_environment) == {"HGS_EXECUTABLE"},
                        "Custom desktop environment kept; review before changing mode.")
                expected["EnvironmentVariables"] = old_environment
            require(previous_definition == expected, "Custom desktop definition kept; update/reconcile it before changing mode.")
        transition.prepare(previous, old_mode)
        pending_restart = bool(transition.record and transition.record["pending_restart"])
        transition.revalidate()
        if loaded and (remove or restart or changed or pending_restart):
            result = launchctl("bootout", target)
            require(result.returncode == 0, "Cannot unload the owned desktop service; inspect it before retrying.")
            deadline = clock() + 15
            while launchctl("print", target).returncode == 0:
                require(clock() < deadline, "Desktop LaunchAgent is still unloading; no replacement was started.")
                sleep(0.2)
            loaded = False
        require(read_definition(destination) == previous, "Desktop definition changed during installation; it was kept.")
        if remove:
            destination.unlink()
            transition.path.unlink(missing_ok=True)
            return "removed"
        if changed:
            for directory in (home / "Library", destination.parent):
                directory.mkdir(mode=0o755, exist_ok=True)
                ordinary(directory, directory=True)
            temporary = None
            try:
                with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as stage:
                    temporary = Path(stage.name)
                    stage.write(content); stage.flush(); os.fsync(stage.fileno())
                    os.fchmod(stage.fileno(), 0o644)
                require(read_definition(destination) == previous, "Desktop definition changed before replacement; it was kept.")
                transition.revalidate()
                os.replace(temporary, destination)
                transition.revalidate()
            finally:
                if temporary is not None:
                    temporary.unlink(missing_ok=True)
        if not gui:
            return "deferred"
        transition.revalidate()
        if not loaded:
            result = launchctl("bootstrap", domain, str(destination))
            require(result.returncode == 0, "Cannot load desktop service in Aqua; registration was kept for inspection.")
        probe_env = dict(env, **value["readiness"]["environment"])
        deadline = clock() + 15
        while True:
            printed = launchctl("print", target)
            if printed.returncode == 0:
                pid = loaded_identity(printed.stdout, destination, value["argv"])
                result = guarded(value["readiness"]["argv"], env=probe_env)
                require(result.returncode == 0, "Desktop readiness command failed; inspect hgs desktop status --json.")
                status = json.loads(result.stdout)
                if ready(value, status, pid):
                    if mode is not None:
                        require(status["lifecycle"].get("mode") == desired_mode, "Selected desktop mode is not running; inspect its pending registration.")
                        transition.revalidate()
                        require(status.get("background_start", {}).get("durably_blocked") is not True or mode == "passive",
                                "Background handoff is blocked; complete authenticated Rearm, never clear revoke.")
                    if mode is not None:
                        receipt = transition.status(printed, plistlib.loads(content))
                        require(receipt["ipc_ready"] is True and receipt["running_mode"] == desired_mode,
                                "Desktop readiness changed before completion; retain its pending transition.")
                        transition.finish()
                        receipt.update(pending_restart=False, transition=None)
                        receipt.update(operation="install", ok=True, message="ready")
                        return receipt
                    transition.finish()
                    return "ready"
            require(clock() < deadline, "Desktop IPC did not become ready; inspect hgs desktop status --json.")
            sleep(0.2)
    finally:
        transition.close(validate=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--check-only", action="store_true")
    group.add_argument("--remove", action="store_true")
    parser.add_argument("--restart", action="store_true", help="reload only this owned desktop service after a backend upgrade")
    parser.add_argument("--runner-hgs", type=Path, help="stable independent CLI for ordinary service descendants")
    parser.add_argument("--refresh-runner", action="store_true", help="retain an owned paired service during a CLI-only install")
    parser.add_argument("--mode", choices=("passive", "background"), help="explicit owner selection after authenticated frontend handoff")
    group.add_argument("--status", action="store_true", help="read selected/running mode without manager mutations")
    group.add_argument("--cancel-rollback", action="store_true", help="cancel only the original pending registration record")
    parser.add_argument("--request-id", help="with --cancel-rollback: the original pending rollback UUID")
    parser.add_argument("--json", action="store_true", help="configuration-scoped native Settings receipt")
    parser.add_argument("--config-dir", type=Path)
    parser.add_argument("--state-dir", type=Path)
    args = parser.parse_args()
    if sys.platform != "darwin":
        return 0
    if os.geteuid() == 0:
        parser.error("run the desktop installer as the login user, without sudo")
    try:
        require(args.cancel_rollback == (args.request_id is not None),
                "--cancel-rollback requires its original --request-id; other actions cannot supply it.")
        source = dict(os.environ)
        for name, path in (("HGS_CONFIG_DIR", args.config_dir), ("HGS_STATE_DIR", args.state_dir)):
            if path is not None:
                require(path.is_absolute(), "Desktop configuration directories must be absolute.")
                source[name] = str(path)
        state = install(args.binary, Path.home(), source, args.restart, args.check_only, args.remove,
                        runner_hgs=args.runner_hgs, refresh_runner=args.refresh_runner,
                        mode=args.mode, status_only=args.status, cancel_rollback=args.request_id)
        if isinstance(state, dict):
            print(json.dumps(state))
        elif args.json:
            receipt = install(args.binary, Path.home(), source, command=run, runner_hgs=args.runner_hgs,
                              refresh_runner=args.refresh_runner, status_only=True)
            receipt.update(operation="remove" if args.remove else "install", ok=True, message=state)
            print(json.dumps(receipt))
        elif state == "deferred":
            print("Desktop service registered for the next Aqua login; IPC readiness is unverified.")
        elif state == "ready":
            print("Desktop service IPC is ready. Background workers remain controlled by the service.")
        else:
            print(f"Desktop service registration: {state}.")
        return 0
    except (Refusal, OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError) as error:
        if args.json:
            record = None
            try:
                raw = read_definition(Path.home() / "Library/LaunchAgents" / ("." + LABEL + ".zerus-registration.json"))
                record = json.loads(raw[0]) if raw else None
            except (Refusal, OSError, ValueError, TypeError):
                pass
            record = record if isinstance(record, dict) else {}
            print(json.dumps({"schema": 1, "operation": "cancel-rollback" if args.cancel_rollback else "status" if args.status else "install", "ok": False,
                "code": "registration_refused", "message": str(error), "configuration": record.get("configuration"),
                "selected_mode": record.get("selected_mode"), "running_mode": None, "ipc_ready": None,
                "pending_restart": record.get("pending_restart"), "transition": record.get("transition"),
                "last_revoke_request_id": record.get("last_revoke_request_id"),
                "last_cancelled_rollback_request_id": record.get("last_cancelled_rollback_request_id")}))
        print(f"hgs desktop installer: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
