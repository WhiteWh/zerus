#!/usr/bin/env python3
"""One unsigned, no-network read-visibility UI campaign on a fresh GitHub-hosted macOS VM (zerus-a4mub.31).

Scrolling a message fully into view reads it after the settled dwell, a sliver does not, and a questions pane
taller than twice the viewport counts once its readable part is visible, through the unchanged production
placement reporting (ConversationReadLayout) and ZerusKit reading rules. Structure, deadlines and cleanup follow
scripts/ci/ios-approval-ui.py.

Requires separately reviewed exact Xcode-build/runtime/device-type inputs. No latest fallback,
runtime installation, keychain, physical device, product app launch, or broad cleanup command.
Cleanup shares one nonrenewable 60s end inside the original 900s whole end, including
owned-child stop/reap and the final qualification check. Late evidence cannot authorize success.
"""
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import selectors
import subprocess
import sys
import time
import uuid

CASES = (
    "test1TheOpeningReplyIsReadAfterTheDwell", "test2ASliverIsNotReadButTheWholeRowIs",
    "test3TheQuestionsPaneCountsFromItsReadablePart",
)


def results(text):
    """Exact original method inventory, one start and one PASS each, no native skips/retries."""
    started = re.findall(r"Test Case '-\[(?:ReadVisibilityUIFixtureTests\.)?ReadVisibilityUITests (test\w+)\]' started\.", text)
    passed = re.findall(r"Test Case '-\[(?:ReadVisibilityUIFixtureTests\.)?ReadVisibilityUITests (test\w+)\]' passed \([0-9.]+ seconds\)\.", text)
    assert len(re.findall(r"Test Case '.+' started\.", text)) == len(CASES), "no additional native tests"
    assert len(re.findall(r"Test Case '.+' passed \([0-9.]+ seconds\)\.", text)) == len(CASES)
    assert len(started) == len(passed) == len(CASES) and set(started) == set(passed) == set(CASES), "exact native case inventory"
    assert not re.search(r"Test Case .* (?:failed|skipped) ", text)
    assert "** TEST SUCCEEDED **" in text
    return {"started": started, "passed": passed, "failed": 0, "skipped": 0}


def owned_device(inventory, runtime, identifier, name):
    rows = [(key, row) for key, devices in inventory["devices"].items() for row in devices if row.get("udid") == identifier]
    assert len(rows) == 1 and rows[0][0] == runtime and rows[0][1].get("name") == name
    return rows[0][1]


def digest(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def hosted_user_environment(environment):
    """Keep the disposable hosted account used by Apple's installed services."""
    selected = {name: environment[name] for name in ("HOME", "TMPDIR", "USER", "LOGNAME")}
    for value in selected.values():
        assert isinstance(value, str) and 0 < len(value) <= 4096
        assert all(ord(character) >= 32 and ord(character) != 127 for character in value)
    assert all(Path(selected[name]).is_absolute() for name in ("HOME", "TMPDIR"))
    assert all(re.fullmatch(r"[A-Za-z_][A-Za-z0-9_.-]{0,63}", selected[name]) for name in ("USER", "LOGNAME"))
    assert selected["USER"] == selected["LOGNAME"]
    return selected


def main():
    assert sys.platform == "darwin" and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
    assert os.uname().machine == "arm64", "reviewed hosted process architecture"
    source = os.environ["EXPECTED_SOURCE"]
    runtime, device = os.environ["EXPECTED_RUNTIME"], os.environ["EXPECTED_DEVICE_TYPE"]
    expected_build = os.environ["EXPECTED_XCODE_BUILD"]
    assert re.fullmatch(r"[0-9a-f]{40}", source)
    assert re.fullmatch(r"com\.apple\.CoreSimulator\.SimRuntime\.iOS-[0-9]+-[0-9]+(?:-[0-9]+)?", runtime)
    assert int(runtime.split(".iOS-", 1)[1].split("-", 1)[0]) >= 18
    assert re.fullmatch(r"com\.apple\.CoreSimulator\.SimDeviceType\.iPhone-[A-Za-z0-9-]+", device)
    assert re.fullmatch(r"[0-9A-Za-z]{3,32}", expected_build)
    hosted_user = hosted_user_environment(os.environ)
    assert all(Path(hosted_user[name]).is_dir() for name in ("HOME", "TMPDIR"))
    # The workflow selects this reviewed installed Xcode; this driver never searches for or switches it.
    developer = Path(os.environ["DEVELOPER_DIR"]).resolve(strict=True)
    assert developer.parent.name == "Contents" and developer.parent.parent.parent == Path("/Applications")
    assert developer.parent.parent.name.startswith("Xcode") and developer.name == "Developer"
    root = Path(os.environ["RUNNER_TEMP"]) / "ios-read-visibility-ui-evidence"
    root.mkdir(mode=0o700)  # No resume/replay into an existing result root.
    end = time.monotonic() + 900
    project = Path.cwd()
    environment = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8", "DEVELOPER_DIR": str(developer), **hosted_user}
    for name, child in (("XDG_CACHE_HOME", "cache"), ("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data")):
        value = root / child; value.mkdir(mode=0o700); environment[name] = str(value)
    commands, cleanup = [], []
    identifier = None
    name = "Zerus read visibility " + str(uuid.uuid4())
    success = False
    unknown_child = False
    cleanup_started = cleanup_end = None
    source_before = {}
    tools_before = {}

    def save(filename, value):
        with (root / filename).open("x") as stream:
            json.dump(value, stream, indent=2, sort_keys=True); stream.write("\n"); stream.flush(); os.fsync(stream.fileno())

    def enter_cleanup():
        nonlocal cleanup_started, cleanup_end
        if cleanup_end is None:
            cleanup_started = time.monotonic()
            cleanup_end = min(end, cleanup_started + 60)
        return cleanup_end

    def run(label, arguments, cap=30, cleaning=False):
        nonlocal unknown_child
        phase_end = enter_cleanup() if cleaning else end - 60
        started = time.monotonic()
        deadline = min(phase_end, started + cap)
        remaining = deadline - started
        assert remaining > 0, "original whole deadline"
        save(label + ".command.json", {"argv": list(map(str, arguments)), "remaining": remaining})
        command = {"label": label, "argv": list(map(str, arguments)), "started_monotonic": started,
                   "whole_end_monotonic": end, "phase_end_monotonic": phase_end,
                   "deadline_monotonic": deadline, "cap_seconds": cap}; commands.append(command)
        out, err = root / (label + ".stdout"), root / (label + ".stderr")
        value = bytearray()
        with out.open("xb") as stdout, err.open("xb") as stderr:
            assert time.monotonic() < deadline, "command deadline before spawn"
            child = subprocess.Popen(command["argv"], cwd=project, env=environment, stdin=subprocess.DEVNULL,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            command["pid"] = child.pid  # Own the unreaped child before any later fallible call.
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(child.stdout, selectors.EVENT_READ, stdout)
                    selector.register(child.stderr, selectors.EVENT_READ, stderr)
                    total = 0
                    while selector.get_map():
                        assert time.monotonic() < deadline, "command deadline"
                        for key, _ in selector.select(min(.1, max(0, deadline - time.monotonic()))):
                            data = os.read(key.fileobj.fileno(), 65536)
                            if not data:
                                selector.unregister(key.fileobj); continue
                            total += len(data)
                            assert total <= 32 * 1024 * 1024, "bounded raw output"
                            key.data.write(data)
                            if key.fileobj is child.stdout: value.extend(data)
                    remaining = deadline - time.monotonic()
                    assert remaining > 0, "command deadline before reap"
                    child.wait(timeout=remaining)
                command["returncode"] = child.returncode
                assert child.returncode == 0 and time.monotonic() < deadline
            finally:
                if child.poll() is None:
                    success_stop = False
                    unknown_child = True  # Failed or late owned stop cannot be reusable success.
                    try:
                        stop_end = enter_cleanup()
                        assert time.monotonic() < stop_end, "cleanup deadline before terminate"
                        child.terminate()
                        remaining = min(3, stop_end - time.monotonic())
                        assert remaining > 0, "cleanup deadline before terminate reap"
                        child.wait(timeout=remaining)
                        success_stop = True
                    except BaseException:
                        try:
                            stop_end = enter_cleanup()
                            assert time.monotonic() < stop_end, "cleanup deadline before kill"
                            child.kill()
                            remaining = min(3, stop_end - time.monotonic())
                            assert remaining > 0, "cleanup deadline before kill reap"
                            child.wait(timeout=remaining)
                            success_stop = True
                        except BaseException:
                            pass
                    command["forced_child_stop"] = success_stop
                command["reaped"] = child.returncode is not None
                for pipe in (child.stdout, child.stderr): pipe.close()
                stdout.flush(); os.fsync(stdout.fileno()); stderr.flush(); os.fsync(stderr.fileno())
                command["finished_monotonic"] = time.monotonic()
                command["elapsed_seconds"] = command["finished_monotonic"] - command["started_monotonic"]
        text = value.decode("utf-8", errors="strict")
        command["closed_monotonic"] = time.monotonic()
        assert command["closed_monotonic"] < deadline, "command deadline after persistence and close"
        return text

    try:
        save("hosted-environment.json", {"environment": environment,
                                       "scope": "standard disposable hosted Apple account and caches; private evidence, build, package and XDG paths"})
        assert run("source", ["/usr/bin/git", "rev-parse", "HEAD"]).strip() == source
        assert run("source-clean", ["/usr/bin/git", "status", "--porcelain"]).strip() == ""
        paths = run("source-paths", ["/usr/bin/git", "ls-files", "-z"]).split("\0")
        assert len(paths) <= 10000
        source_before = {path: digest(project / path) for path in paths if path}
        save("source-before.json", source_before)
        version = run("xcode", ["/usr/bin/xcodebuild", "-version"])
        assert re.fullmatch(r"Xcode (?:2[6-9]|[3-9][0-9])(?:\.[0-9]+)*\nBuild version " + re.escape(expected_build) + r"\n", version)
        tools_before = {str(path): digest(path) for path in (developer / "usr/bin/xcodebuild", Path("/usr/bin/xcrun"))}
        save("tools.json", {"xcode": version, "developer": str(developer), "runtime": runtime, "device_type": device,
                            "sha256": tools_before, "process_architecture": "arm64", "destination_architecture": "arm64",
                            "scope": "selected installed tools on disposable VM; not transitive closure"})
        runtimes = json.loads(run("runtimes", ["/usr/bin/xcrun", "simctl", "list", "runtimes", "--json"], 120))["runtimes"]
        assert len([row for row in runtimes if row.get("identifier") == runtime and row.get("isAvailable") is True]) == 1
        types = json.loads(run("device-types", ["/usr/bin/xcrun", "simctl", "list", "devicetypes", "--json"]))["devicetypes"]
        assert len([row for row in types if row.get("identifier") == device]) == 1
        before = json.loads(run("devices-before", ["/usr/bin/xcrun", "simctl", "list", "devices", "--json"]))
        run("project", ["/usr/bin/python3", "-I", "-B", "scripts/ci/ios-read-visibility-ui-project.py", root / "project"])
        identifier = run("create", ["/usr/bin/xcrun", "simctl", "create", name, device, runtime]).strip()
        assert str(uuid.UUID(identifier)).upper() == identifier
        assert not any(row.get("udid") == identifier for rows in before["devices"].values() for row in rows)
        created = json.loads(run("created", ["/usr/bin/xcrun", "simctl", "list", "devices", "--json"]))
        assert owned_device(created, runtime, identifier, name)["state"] == "Shutdown"
        save("owned-simulator.json", {"udid": identifier, "name": name, "runtime": runtime, "device_type": device})
        run("boot", ["/usr/bin/xcrun", "simctl", "boot", identifier])
        run("bootstatus", ["/usr/bin/xcrun", "simctl", "bootstatus", identifier, "-b"], 120)
        text = run("ui-test", ["/usr/bin/xcodebuild", "-project", root / "project/ReadVisibilityUIFixture.xcodeproj", "-scheme", "ReadVisibilityUIFixture",
                   "-configuration", "Debug", "-sdk", "iphonesimulator", "-destination", "platform=iOS Simulator,arch=arm64,id=" + identifier,
                   "-derivedDataPath", root / "derived", "-clonedSourcePackagesDirPath", root / "packages", "-packageCachePath", root / "package-cache",
                   "-disableAutomaticPackageResolution", "-disablePackageRepositoryCache", "-parallel-testing-enabled", "NO",
                   "-maximum-concurrent-test-simulator-destinations", "1", "-jobs", "2", "-test-timeouts-enabled", "YES",
                   "-default-test-execution-time-allowance", "30", "-maximum-test-execution-time-allowance", "30",
                   "-resultBundlePath", root / "result.xcresult", "CODE_SIGNING_ALLOWED=NO", "CODE_SIGNING_REQUIRED=NO", "CODE_SIGN_IDENTITY=", "test"], 600)
        save("native-results.json", results(text))
        assert (root / "result.xcresult").is_dir()
        app = root / "derived/Build/Products/Debug-iphonesimulator/ReadVisibilityUIFixture.app"
        with (app / "Info.plist").open("rb") as stream: metadata = plistlib.load(stream)
        assert metadata["CFBundleIdentifier"] == "dev.zerus.fixture.readvisibility" and not (app / "embedded.mobileprovision").exists()
        save("fixture-product.json", {"info": metadata, "executable_sha256": digest(app / metadata["CFBundleExecutable"]),
                                       "scope": "separate unsigned simulator fixture; no maintained app"})
        success = True
    finally:
        enter_cleanup()  # Reuses an earlier owned-stop phase; never starts another 60s allowance.
        try:
            if identifier:
                current = json.loads(run("cleanup-identity", ["/usr/bin/xcrun", "simctl", "list", "devices", "--json"], 15, True))
                state = owned_device(current, runtime, identifier, name)["state"]
                assert state in {"Booted", "Shutdown"}
                if state == "Booted": run("shutdown", ["/usr/bin/xcrun", "simctl", "shutdown", identifier], 10, True)
                stopped = json.loads(run("cleanup-stopped", ["/usr/bin/xcrun", "simctl", "list", "devices", "--json"], 15, True))
                assert owned_device(stopped, runtime, identifier, name)["state"] == "Shutdown"
                run("delete", ["/usr/bin/xcrun", "simctl", "delete", identifier], 8, True)
                cleanup.append({"udid": identifier, "shutdown_and_deleted": True})
        except BaseException as error:
            success = False; cleanup.append({"unknown": type(error).__name__})
        same = bool(source_before) and all(digest(project / path) == value for path, value in source_before.items())
        tools_same = bool(tools_before) and all(digest(path) == value for path, value in tools_before.items())
        success = success and same and tools_same and not unknown_child and time.monotonic() < cleanup_end
        save("outcome.json", {"qualified": success, "source_unchanged": same, "tools_unchanged": tools_same, "cleanup": cleanup, "commands": commands,
                              "cleanup_started_monotonic": cleanup_started, "cleanup_end_monotonic": cleanup_end, "whole_end_monotonic": end,
                              "unknown_child": unknown_child, "scope": "one native read-visibility case; synthetic rows, no relay, owner device or keychain"})
    assert success and time.monotonic() < cleanup_end, "qualification including final persistence must meet original cleanup/whole end"


if __name__ == "__main__":
    assert len(sys.argv) == 1
    main()
