"""Disposable-runner disk cleanup must preserve service and non-build state."""

import importlib.util
import contextlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("ubuntu_disk", ROOT / "scripts/ci/ubuntu-disk.py")
DISK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DISK)


class HostedDiskTests(unittest.TestCase):
    def test_refuses_local_self_hosted_and_other_jobs_before_cleanup(self):
        valid = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                 "RUNNER_OS": "Linux", "GITHUB_JOB": "quick", "GITHUB_WORKSPACE": str(ROOT)}
        for key, value in (("GITHUB_ACTIONS", "false"), ("RUNNER_ENVIRONMENT", "self-hosted"),
                           ("RUNNER_OS", "Windows"), ("GITHUB_JOB", "publish")):
            with self.subTest(key=key), patch.dict(os.environ, {**valid, key: value}, clear=True), \
                    patch.object(sys, "argv", ["ubuntu-disk.py", "prepare"]), \
                    patch.object(DISK.subprocess, "run") as run, patch.object(DISK.shutil, "rmtree") as remove:
                with self.assertRaises(ValueError):
                    DISK.main()
                run.assert_not_called()
                remove.assert_not_called()

    def test_preparation_removes_only_the_fixed_unused_toolchains(self):
        with patch.dict(os.environ, {"GITHUB_JOB": "quick"}), \
                patch.object(DISK, "report"), patch.object(DISK.subprocess, "run") as run:
            DISK.prepare(ROOT)
        run.assert_called_once_with(
            ["sudo", "rm", "-rf", "--one-file-system", "--", *DISK.UNUSED_TOOLS], check=True,
        )
        self.assertNotIn("docker", " ".join(run.call_args.args[0]))
        self.assertNotIn(str(ROOT), run.call_args.args[0])

    def test_relay_retires_only_completed_host_targets_and_keeps_runtime_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            workspace = Path(temporary).resolve()
            for relative in DISK.RELAY_TARGETS:
                target = workspace / relative
                target.mkdir(parents=True)
                (target / "old-build").write_text("fixture")
            retained = workspace / "relay-venv/runtime-test"
            retained.parent.mkdir()
            retained.write_text("fixture")
            with patch.dict(os.environ, {"GITHUB_JOB": "relay"}), patch.object(DISK, "report"):
                DISK.relay_before_image(workspace)
            self.assertTrue(retained.exists())
            self.assertTrue(all(not (workspace / path).exists() for path in DISK.RELAY_TARGETS))

    def test_relay_refuses_replaced_target_and_preserves_its_destination(self):
        with tempfile.TemporaryDirectory() as temporary:
            workspace = Path(temporary).resolve()
            outside = workspace / "keep"
            outside.mkdir()
            (outside / "original").write_text("fixture")
            (workspace / "target").symlink_to(outside, target_is_directory=True)
            with patch.dict(os.environ, {"GITHUB_JOB": "relay"}), patch.object(DISK, "report"):
                with self.assertRaises(ValueError):
                    DISK.relay_before_image(workspace)
            self.assertTrue((outside / "original").exists())

    def test_quick_cannot_retire_relay_builds(self):
        with patch.dict(os.environ, {"GITHUB_JOB": "quick"}), patch.object(DISK, "report") as report:
            with self.assertRaises(ValueError):
                DISK.relay_before_image(ROOT)
            report.assert_not_called()

    def test_native_report_sizes_its_target_and_caches_without_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            workspace = Path(temporary).resolve()
            home = workspace / "private-home"
            paths = [workspace / "target/client-native", home / ".cargo/registry", home / ".cargo/git"]
            for path in paths:
                path.mkdir(parents=True)
            env = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                   "RUNNER_OS": "Linux", "GITHUB_JOB": "client-native-linux",
                   "GITHUB_WORKSPACE": str(workspace)}
            output = io.StringIO()
            with patch.dict(os.environ, env, clear=True), \
                    patch.object(DISK.Path, "cwd", return_value=workspace), \
                    patch.object(DISK.Path, "home", return_value=home), \
                    patch.object(sys, "argv", ["ubuntu-disk.py", "report", "native-test"]), \
                    patch.object(DISK.shutil, "disk_usage", return_value=SimpleNamespace(total=100, used=60, free=40)), \
                    patch.object(DISK.shutil, "rmtree") as remove, \
                    patch.object(DISK.subprocess, "run", return_value=SimpleNamespace(stdout="sizes\n", returncode=0)) as run, \
                    contextlib.redirect_stdout(output):
                DISK.main()
            self.assertEqual(json.loads(output.getvalue().splitlines()[0]), {
                "phase": "native-test", "disk_bytes": {"total": 100, "used": 60, "free": 40},
            })
            run.assert_called_once_with(
                ["du", "-sh", "--", *map(str, paths)], capture_output=True, text=True, timeout=15,
            )
            remove.assert_not_called()

    def test_native_cannot_retire_relay_builds(self):
        env = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
               "RUNNER_OS": "Linux", "GITHUB_JOB": "client-native-linux", "GITHUB_WORKSPACE": str(ROOT)}
        with patch.dict(os.environ, env, clear=True), patch.object(DISK.Path, "cwd", return_value=ROOT), \
                patch.object(sys, "argv", ["ubuntu-disk.py", "relay-before-image"]), \
                patch.object(DISK, "report") as report, patch.object(DISK.subprocess, "run") as run, \
                patch.object(DISK.shutil, "rmtree") as remove:
            with self.assertRaises(ValueError):
                DISK.main()
            report.assert_not_called()
            run.assert_not_called()
            remove.assert_not_called()

    def test_native_prepare_checks_fresh_space_after_fixed_cleanup_at_exact_threshold(self):
        threshold = 24 * 1024**3
        self.assertEqual(DISK.NATIVE_MIN_FREE_BYTES, threshold)
        tools = ("/usr/local/lib/android", "/usr/share/dotnet", "/opt/ghc", "/opt/hostedtoolcache/CodeQL")
        self.assertEqual(DISK.UNUSED_TOOLS, tools)
        env = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
               "RUNNER_OS": "Linux", "GITHUB_JOB": "client-native-linux", "GITHUB_WORKSPACE": str(ROOT)}
        for actual in (0, threshold - 1, threshold, threshold + 1):
            events, output = [], io.StringIO()
            measurements = iter((threshold - 1, threshold + 1, actual))

            def measure(_workspace):
                events.append("measure")
                return SimpleNamespace(free=next(measurements))

            def report(workspace, phase):
                events.append(phase)
                DISK.shutil.disk_usage(workspace)

            with self.subTest(actual=actual), patch.dict(os.environ, env, clear=True), \
                    patch.object(DISK.Path, "cwd", return_value=ROOT), \
                    patch.object(sys, "argv", ["ubuntu-disk.py", "prepare"]), \
                    patch.object(DISK, "report", side_effect=report), \
                    patch.object(DISK.shutil, "disk_usage", side_effect=measure) as usage, \
                    patch.object(DISK.subprocess, "run", side_effect=lambda *a, **kw: events.append("cleanup")) as run, \
                    patch.object(DISK.shutil, "rmtree") as remove, contextlib.redirect_stdout(output):
                if actual >= threshold:
                    DISK.main()
                else:
                    with self.assertRaisesRegex(SystemExit, f"required_free_bytes={threshold} .*actual_free_bytes={actual}"):
                        DISK.main()
                run.assert_called_once_with(["sudo", "rm", "-rf", "--one-file-system", "--", *tools], check=True)
                self.assertEqual(usage.call_count, 3)
                remove.assert_not_called()
            self.assertEqual(events, ["before-unused-tools", "measure", "cleanup",
                                      "after-unused-tools", "measure", "measure"])
            lines = output.getvalue().splitlines()
            self.assertEqual(json.loads(lines[0]), {
                "phase": "native-space-admission", "required_free_bytes": threshold,
                "actual_free_bytes": actual, "admitted": actual >= threshold,
            })
            if actual >= threshold:
                self.assertEqual(len(lines), 1)
            else:
                # One annotation names both measurements on the run summary.
                self.assertEqual(len(lines), 2)
                self.assertTrue(lines[1].startswith("::error title=Native Linux disk admission::"))
                self.assertIn(f"required_free_bytes={threshold} (24.0 GiB)", lines[1])
                self.assertIn(f"actual_free_bytes={actual} ({actual / 1024**3:.1f} GiB)", lines[1])

    def test_failed_cleanup_never_passes_native_space_admission(self):
        with patch.dict(os.environ, {"GITHUB_JOB": "client-native-linux"}), \
                patch.object(DISK, "report") as report, \
                patch.object(DISK.subprocess, "run", side_effect=subprocess.CalledProcessError(1, ["sudo", "rm"])), \
                patch.object(DISK.shutil, "disk_usage") as usage, contextlib.redirect_stdout(io.StringIO()) as output:
            with self.assertRaises(subprocess.CalledProcessError):
                DISK.prepare(ROOT)
            report.assert_called_once_with(ROOT, "before-unused-tools")
            usage.assert_not_called()
            self.assertEqual(output.getvalue(), "")

    def test_quick_and_relay_cleanup_have_no_native_minimum(self):
        for job in ("quick", "relay"):
            with self.subTest(job=job), patch.dict(os.environ, {"GITHUB_JOB": job}), \
                    patch.object(DISK, "report") as report, patch.object(DISK.subprocess, "run") as run, \
                    patch.object(DISK.shutil, "disk_usage") as usage:
                DISK.prepare(ROOT)
                self.assertEqual([call.args for call in report.call_args_list],
                                 [(ROOT, "before-unused-tools"), (ROOT, "after-unused-tools")])
                run.assert_called_once()
                usage.assert_not_called()


class NativeScriptCheckpoints(unittest.TestCase):
    def run_script(self, identity, failed_command=""):
        with tempfile.TemporaryDirectory() as temporary:
            workspace = Path(temporary)
            scripts = workspace / "scripts/ci"
            scripts.mkdir(parents=True)
            shutil.copyfile(ROOT / "scripts/ci/client-native.sh", scripts / "client-native.sh")
            log = workspace / "commands.jsonl"
            # Closed stand-ins: none of the test's Cargo, rustup, display or
            # diagnostic calls can launch a build or remove a host directory.
            recorder = '''#!/usr/bin/env python3
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
report = name == 'ubuntu-disk.py'
if report:
    assert sys.argv[1] == 'report'
with pathlib.Path(os.environ['CI_FIXTURE_LOG']).open('a') as stream:
    stream.write(json.dumps({'kind': 'report' if report else 'command', 'name': name, 'args': sys.argv[1:]}) + '\\n')
if name == os.environ.get('CI_FIXTURE_FAIL'):
    sys.exit(13)
if name == 'cargo' and sys.argv[2:3] == ['build'] and '--bin' in sys.argv:
    # Like Cargo, leave the built candidate CLI where the Tauri tests expect it.
    cli = pathlib.Path(os.environ['CARGO_TARGET_DIR'], 'debug', 'hgs')
    cli.parent.mkdir(parents=True, exist_ok=True)
    cli.write_text('#!/bin/sh\\n')
    cli.chmod(0o700)
'''
            (scripts / "ubuntu-disk.py").write_text(recorder)
            binaries = workspace / "bin"
            binaries.mkdir()
            for command in ("cargo", "rustup", "xvfb-run", "dbus-run-session"):
                stub = binaries / command
                stub.write_text(recorder)
                stub.chmod(0o700)
            env = dict(os.environ)
            # Synthetic Cargo must stay inside this fixture's workspace.
            for key in ("CARGO_TARGET_DIR", "GITHUB_ACTIONS", "RUNNER_ENVIRONMENT", "RUNNER_OS", "GITHUB_JOB", "GITHUB_WORKSPACE"):
                env.pop(key, None)
            env.update(identity)
            env.update(PATH=str(binaries) + os.pathsep + env.get("PATH", ""),
                       CI_FIXTURE_LOG=str(log), CI_FIXTURE_FAIL=failed_command)
            result = subprocess.run(["bash", str(scripts / "client-native.sh"), "linux"],
                                    cwd=workspace, env=env, capture_output=True, text=True, timeout=10)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            return result, calls

    def test_inherited_absolute_target_keeps_the_callers_cli_bytes(self):
        with tempfile.TemporaryDirectory(prefix="ci-disk-sentinel-") as temporary:
            target = Path(temporary) / "external target"
            cli = target / "debug/hgs"
            cli.parent.mkdir(parents=True)
            sentinel = b"caller-owned hgs sentinel\x00\xff\n"
            valid = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                     "RUNNER_OS": "Linux", "GITHUB_JOB": "client-native-linux"}
            for identity in ({}, valid):
                with self.subTest(identity=identity):
                    cli.write_bytes(sentinel)
                    cli.chmod(0o700)
                    with patch.dict(os.environ, {"CARGO_TARGET_DIR": str(target)}):
                        result, calls = self.run_script(identity)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertTrue(any(call["name"] == "cargo" and call["args"][1:2] == ["build"]
                                        and "--bin" in call["args"] and "hgs" in call["args"]
                                        for call in calls), "the actual candidate CLI build must run")
                    self.assertEqual(cli.read_bytes(), sentinel)

    def test_only_fixed_hosted_native_identity_reports_and_preserves_build_commands(self):
        valid = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                 "RUNNER_OS": "Linux", "GITHUB_JOB": "client-native-linux"}
        local, baseline = self.run_script({})
        self.assertEqual(local.returncode, 0, local.stderr)
        self.assertTrue(baseline)
        self.assertTrue(all(call["kind"] == "command" for call in baseline))
        hosted, calls = self.run_script(valid)
        self.assertEqual(hosted.returncode, 0, hosted.stderr)
        self.assertEqual([call for call in calls if call["kind"] == "command"], baseline)
        self.assertEqual([call["args"][1] for call in calls if call["kind"] == "report"], [
            "after-native-presence-tests", "after-native-media-tests", "after-native-tauri-tests",
            "after-native-tauri-clippy", "before-native-core-test-linking", "after-native-core-tests",
        ])
        for key, value in (("GITHUB_ACTIONS", "false"), ("RUNNER_ENVIRONMENT", "self-hosted"),
                           ("RUNNER_OS", "macOS"), ("GITHUB_JOB", "quick")):
            with self.subTest(key=key):
                result, calls = self.run_script({**valid, key: value})
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(calls, baseline)

    def test_checkpoint_does_not_hide_failed_build_command(self):
        result, calls = self.run_script({"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                                        "RUNNER_OS": "Linux", "GITHUB_JOB": "client-native-linux"},
                                       failed_command="dbus-run-session")
        self.assertEqual(result.returncode, 13, result.stderr)
        self.assertEqual(calls[-1]["name"], "dbus-run-session")
        self.assertEqual(calls[-2]["args"], ["report", "before-native-core-test-linking"])


if __name__ == "__main__":
    unittest.main()
