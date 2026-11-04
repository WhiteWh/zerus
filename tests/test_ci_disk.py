"""Disposable-runner disk cleanup must preserve service and non-build state."""

import importlib.util
import os
from pathlib import Path
import tempfile
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
            with self.subTest(key=key), patch.dict(os.environ, {**valid, key: value}, clear=True):
                with self.assertRaises(ValueError):
                    DISK.hosted_workspace()

    def test_preparation_removes_only_the_fixed_unused_toolchains(self):
        with patch.object(DISK, "report"), patch.object(DISK.subprocess, "run") as run:
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


if __name__ == "__main__":
    unittest.main()
