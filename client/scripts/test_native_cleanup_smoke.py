#!/usr/bin/env python3
"""What the manual native-cleanup walk depends on, checked in CI (zerus-2ldeb.3/.4).

native-cleanup-smoke.py drives the built GUI with OCR and reads the native
journals, so it runs only as a manual qualification (see
native-cleanup-smoke.md). These tests pin everything it clicks, calls and reads
to the product sources, so a product change fails here instead of silently
breaking the walk, and they pin its safety margins: the person's homes stay
read-only, the walk stays in its own namespaces, and no check can be skipped.
"""
from contextlib import ExitStack
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

SCRIPTS = Path(__file__).resolve().parent
REPO = SCRIPTS.parents[1]
SMOKE = SCRIPTS / "native-cleanup-smoke.py"


def load():
    spec = importlib.util.spec_from_file_location("native_cleanup_smoke", SMOKE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


smoke = load()


class Anchors(unittest.TestCase):
    def test_every_anchor_is_in_its_product_source(self):
        for what, path, text in smoke.ANCHORS:
            with self.subTest(what=what):
                source = REPO / path
                self.assertTrue(source.is_file(), f"{what}: {path} is gone")
                self.assertIn(text, source.read_text(encoding="utf-8"), f"{what}: {path} no longer has {text!r}")

    def test_what_the_walk_reads_on_screen_is_the_pinned_product_text(self):
        anchored = {what: text for what, _, text in smoke.ANCHORS}
        for constant, what in [
            (smoke.TERMINATE, "menu Terminate"),
            (smoke.REMOVE_FOLDER, "Remove working folder choice"),
            (smoke.STOP_AND_REMOVE, "Stop and remove folder button"),
            (smoke.ONE_PROGRAM, "one other program summary"),
            ("(" + smoke.PROGRAM_PID, "program PID line"),
            (smoke.REVIEW_TITLE, "cleanup review title"),
        ]:
            with self.subTest(constant=constant):
                self.assertIn(constant, anchored[what])

    def test_the_walk_uses_the_pinned_constants_not_copies(self):
        source = SMOKE.read_text(encoding="utf-8")
        walk = source[source.index("def worker("):source.index("def outer(")]
        for constant in (smoke.TERMINATE, smoke.REMOVE_FOLDER, smoke.STOP_AND_REMOVE, smoke.ONE_PROGRAM,
                         smoke.REVIEW_TITLE):
            with self.subTest(constant=constant):
                self.assertNotIn(repr(constant), walk)


class Admission(unittest.TestCase):
    """Only synthetic probes: no real /proc, display, namespace or home access."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(dir="/tmp")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.homes = [self.root / "owner-env", self.root / "owner-passwd"]
        for path in [self.root / "runtime", *self.homes]:
            path.mkdir()
        self.outer = {"namespaces": {"pid": "pid:[100]", "user": "user:[101]"},
                      "tmp": [10, 11], "read_only": list(map(str, self.homes))}
        (self.root / "outer.json").write_text(json.dumps(self.outer))
        self.namespaces = {"pid": "pid:[200]", "user": "user:[201]"}
        self.init = dict(pid=1, group=1, session=1, birth="1000", exe="/fixture/python",
                         argv=["/fixture/python", "--init", str(self.root)],
                         pid_namespace=self.namespaces["pid"])
        self.server = dict(pid=8, group=8, session=8, birth="1001", exe="/fixture/bin/Xvfb",
                           argv=["Xvfb", ":77", "-nolisten", "tcp"],
                           pid_namespace=self.namespaces["pid"])
        self.processes = [self.init, self.server]
        self.writable = set()
        environment = {"HOME": str(self.root / "home"), "HGS_STATE_DIR": str(self.root / "state"),
                       "GDK_BACKEND": "x11", "WAYLAND_DISPLAY": "zerus-private-no-wayland",
                       "XDG_RUNTIME_DIR": str(self.root / "runtime"), "DISPLAY": ":77"}
        patches = ExitStack()
        self.addCleanup(patches.close)
        patches.enter_context(mock.patch.dict(os.environ, environment, clear=True))
        patches.enter_context(mock.patch.object(smoke, "namespace", return_value=self.namespaces))
        patches.enter_context(mock.patch.object(smoke, "proc", return_value=self.init))
        patches.enter_context(mock.patch.object(smoke, "census", side_effect=lambda: self.processes))
        patches.enter_context(mock.patch.object(Path, "read_bytes", autospec=True, side_effect=self.init_cmdline))
        patches.enter_context(mock.patch.object(Path, "is_socket", autospec=True, side_effect=self.private_socket))
        real_stat = os.stat
        patches.enter_context(mock.patch.object(smoke.os, "stat", side_effect=lambda path, *args, **kwargs:
                              SimpleNamespace(st_dev=20, st_ino=21) if path == "/tmp"
                              else real_stat(path, *args, **kwargs)))
        self.access = patches.enter_context(mock.patch.object(smoke.os, "access", side_effect=self.home_access))
        self.launch = patches.enter_context(mock.patch.object(smoke.subprocess, "Popen",
                                           side_effect=AssertionError("admission must not launch a process")))
        self.walk = patches.enter_context(mock.patch.object(smoke, "worker",
                                         side_effect=AssertionError("admission must not run the walk")))
        patches.enter_context(mock.patch.object(smoke, "Screen",
                              side_effect=AssertionError("admission must not open a display")))

    def init_cmdline(self, path):
        self.assertEqual(path, Path("/proc/1/cmdline"))
        return b"/fixture/python\0--init\0" + str(self.root).encode() + b"\0full\0"

    def private_socket(self, path):
        self.assertEqual(path, Path("/tmp/.X11-unix/X77"))
        return True

    def home_access(self, home, mode):
        self.assertIn(home, self.outer["read_only"])
        self.assertEqual(mode, os.W_OK)
        return home in self.writable

    def refused(self, message):
        with self.assertRaises(SystemExit) as stopped:
            smoke.admission(self.root)
        self.assertIn(message, str(stopped.exception))
        self.assertFalse((self.root / "admission.json").exists())

    def test_private_fixture_is_admitted_and_records_its_witness(self):
        smoke.admission(self.root)
        recorded = json.loads((self.root / "admission.json").read_text())
        self.assertEqual(recorded["namespaces"], self.namespaces)
        self.assertEqual(recorded["display"], ":77")
        self.assertEqual(recorded["display_server"], self.server)
        self.assertEqual(recorded["read_only"], self.outer["read_only"])
        self.access.assert_has_calls([mock.call(str(home), os.W_OK) for home in self.homes])
        self.launch.assert_not_called()
        self.walk.assert_not_called()

    def test_no_private_xvfb_is_refused(self):
        self.processes = [self.init]
        self.refused("no single private Xvfb serves :77")

    def test_multiple_private_xvfbs_for_the_display_are_refused(self):
        self.processes.append({**self.server, "pid": 9, "birth": "1002"})
        self.refused("no single private Xvfb serves :77")

    def test_writable_environment_home_is_refused(self):
        self.writable.add(str(self.homes[0]))
        self.refused(f"the person's home {self.homes[0]} is writable")

    def test_writable_passwd_home_is_refused(self):
        self.writable.add(str(self.homes[1]))
        self.refused(f"the person's home {self.homes[1]} is writable")


class Safety(unittest.TestCase):
    def command(self, root):
        with mock.patch.object(smoke.gui_sandbox.shutil, "which", return_value="/usr/bin/bwrap"), \
                mock.patch.object(smoke.gui_sandbox.sys, "platform", "linux"):
            return smoke.sandbox_command(Path(root), {"HOME": f"{root}/home"})

    def test_the_persons_homes_are_read_only_and_only_the_walk_root_is_writable(self):
        homes = smoke.owner_homes()
        self.assertIn(Path.home().resolve(), homes)
        with tempfile.TemporaryDirectory() as root:
            command = self.command(root)
            binds = [(command[i], command[i + 1]) for i, word in enumerate(command) if word in ("--ro-bind", "--bind")]
            self.assertEqual(binds, [*(("--ro-bind", str(home)) for home in homes), ("--bind", str(Path(root).resolve()))])

    def test_different_environment_and_passwd_homes_are_both_bound_read_only(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as folder:
            root = Path(folder)
            environment_home, passwd_home, walk = (root / name for name in ("env-home", "passwd-home", "walk"))
            for path in (environment_home, passwd_home, walk):
                path.mkdir()
            real_is_dir = Path.is_dir
            with mock.patch.dict(os.environ, {"HOME": str(environment_home)}, clear=True), \
                    mock.patch.object(smoke.os, "getuid", return_value=4242), \
                    mock.patch.object(smoke.os, "geteuid", return_value=4242), \
                    mock.patch.object(smoke.pwd, "getpwuid", return_value=SimpleNamespace(pw_dir=str(passwd_home))) as passwd, \
                    mock.patch.object(Path, "is_dir", autospec=True, side_effect=lambda path:
                                      False if path == Path("/run/user/4242") else real_is_dir(path)):
                command = self.command(walk)
            binds = [(command[i], command[i + 1]) for i, word in enumerate(command) if word in ("--ro-bind", "--bind")]
            self.assertEqual(binds, [("--ro-bind", str(environment_home)), ("--ro-bind", str(passwd_home)),
                                     ("--bind", str(walk))])
            passwd.assert_called_once_with(4242)

    def test_the_walk_runs_only_below_its_own_user_and_pid_namespace_and_reaper(self):
        with tempfile.TemporaryDirectory() as root:
            command = self.command(root)
            inner = command[command.index("--") + 1:]
            self.assertEqual(inner[:6], ["/usr/bin/unshare", "--user", "--map-current-user", "--pid", "--fork", "--mount-proc"])
            self.assertEqual(inner[6:8], ["/usr/bin/env", "-i"])
            self.assertEqual(inner[-3:], ["--init", str(Path(root)), "full"])
            for flag in ("--unshare-net", "--unshare-ipc", "--die-with-parent"):
                self.assertIn(flag, command)

    def test_an_optimized_interpreter_is_refused_before_anything_runs(self):
        for argv, env in [([sys.executable, "-O", str(SMOKE), "--help"], {}),
                          ([sys.executable, str(SMOKE), "--help"], {"PYTHONOPTIMIZE": "1"})]:
            with self.subTest(argv=argv[1:2], env=env):
                result = subprocess.run(argv, env={**os.environ, **env}, capture_output=True, text=True, timeout=30)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("refuses python -O", result.stderr)

    def test_a_failed_check_stops_the_walk(self):
        with self.assertRaises(SystemExit) as stopped:
            smoke.check(False, "witness changed")
        self.assertIn("witness changed", str(stopped.exception))
        smoke.check(True, "unused")


if __name__ == "__main__":
    unittest.main()
