#!/usr/bin/env python3
"""The shared GUI isolation and its use by every app-starting smoke (zerus-5a99f)."""
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))
import gui_sandbox  # noqa: E402

# Smokes that start a built app. Each wraps its outer command in the sandbox;
# those with a Python worker also refuse a reachable guard before the app.
APP_SMOKES = {
    "phase4-smoke.py": False,
    "updates-smoke.py": False,
    "file-reference-smoke.py": True,
    "linux-startup-smoke.py": True,
    "machines-smoke.py": True,
    "native-ui-smoke.py": True,
    "native-cleanup-smoke.py": True,
    "project-vivid-smoke.py": True,
    "termination-cleanup-smoke.py": True,
    "video-poster-smoke.py": True,
    "worktree-review-smoke.py": True,
}
# Scripts that start a display without the Zerus entry point.
NOT_APP_STARTS = {
    "menu-smoke.py": "a browser against the local Vite server",
    "window-resize-smoke.py": "a synthetic window fixture, never the instance claim",
    "window-theme-smoke.py": "a synthetic window fixture, never the instance claim",
    "phase4-wheel-driver.py": "the app child phase4-smoke.py starts inside its sandbox",
}


class Sandbox(unittest.TestCase):
    def wrapped(self, keep=()):
        with mock.patch.object(gui_sandbox.shutil, "which", return_value="/usr/bin/bwrap"), \
                mock.patch.object(gui_sandbox.sys, "platform", "linux"):
            return gui_sandbox.sandboxed(["dbus-run-session", "--", "app", Path("/x")], keep=keep)

    def test_private_tmp_run_user_network_and_ipc_around_the_exact_command(self):
        command = self.wrapped()
        self.assertEqual(command[:5], ["/usr/bin/bwrap", "--dev-bind", "/", "/", "--tmpfs"])
        self.assertEqual(command[5], "/tmp")
        runtime = f"/run/user/{os.geteuid()}"
        self.assertEqual(Path(runtime).is_dir(), ["--tmpfs", runtime] == command[6:8])
        for flag in ["--unshare-net", "--unshare-ipc", "--die-with-parent"]:
            self.assertIn(flag, command)
        separator = command.index("--")
        self.assertEqual(command[separator + 1:], ["dbus-run-session", "--", "app", "/x"])
        self.assertNotIn("--share-net", command)

    def test_kept_paths_stay_shared_even_below_the_private_tmp(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as kept:
            command = self.wrapped(keep=[kept])
            bind = command.index("--bind")
            self.assertEqual(command[bind + 1:bind + 3], [str(Path(kept).resolve())] * 2)
            # Mounted after the private /tmp, so it is visible inside it.
            self.assertGreater(bind, command.index("/tmp"))
        with self.assertRaises(FileNotFoundError):
            self.wrapped(keep=["/nonexistent-zerus-sandbox-path"])

    def test_there_is_no_unsandboxed_mode(self):
        with mock.patch.object(gui_sandbox.shutil, "which", return_value=None), \
                mock.patch.object(gui_sandbox.sys, "platform", "linux"):
            with self.assertRaises(SystemExit):
                gui_sandbox.sandboxed(["app"])
        with mock.patch.object(gui_sandbox.sys, "platform", "darwin"):
            with self.assertRaises(SystemExit):
                gui_sandbox.sandboxed(["app"])

    def test_a_reachable_canonical_guard_refuses_the_start(self):
        self.assertEqual(gui_sandbox.canonical_guard(), Path(f"/tmp/com.hgdev.hgs-tray-{os.geteuid()}"))
        with tempfile.TemporaryDirectory() as root:
            guard = Path(root) / "guard"
            with mock.patch.object(gui_sandbox, "canonical_guard", return_value=guard):
                gui_sandbox.refuse_reachable_guard()
                guard.mkdir()
                with self.assertRaises(SystemExit) as refused:
                    gui_sandbox.refuse_reachable_guard()
                self.assertIn("reachable", str(refused.exception))

    def test_every_app_smoke_starts_through_the_sandbox(self):
        for name, worker in APP_SMOKES.items():
            source = (SCRIPTS / name).read_text(encoding="utf-8")
            self.assertIn("gui_sandbox.sandboxed(", source, name)
            if worker:
                self.assertIn("gui_sandbox.refuse_reachable_guard()", source, name)
        # A new script that starts a display must join one of the lists.
        for script in sorted(SCRIPTS.glob("*.py")):
            source = script.read_text(encoding="utf-8")
            if "xvfb-run" in source and not script.name.startswith("test_"):
                self.assertIn(script.name, APP_SMOKES.keys() | NOT_APP_STARTS.keys(), script.name)


if __name__ == "__main__":
    unittest.main()
