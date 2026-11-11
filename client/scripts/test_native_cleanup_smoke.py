#!/usr/bin/env python3
"""What the manual native-cleanup walk depends on, checked in CI (zerus-2ldeb.3/.4).

native-cleanup-smoke.py drives the built GUI with OCR and reads the native
journals, so it runs only as a manual qualification (see
native-cleanup-smoke.md). These tests pin everything it clicks, calls and reads
to the product sources, so a product change fails here instead of silently
breaking the walk, and they pin its safety margins: the person's homes stay
read-only, the walk stays in its own namespaces, and no check can be skipped.
"""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
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
