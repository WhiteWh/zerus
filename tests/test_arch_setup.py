"""Package migration must preserve existing user executables and services."""
import contextlib
import io
import os
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

SCRIPT = Path(__file__).resolve().parents[1] / "packaging/linux/zerus-setup"


class ArchSetup(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zerus-setup-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.prefix = self.root / "system"
        (self.prefix / "bin").mkdir(parents=True)
        for name in ("hgs", "hgs-tray", "hgs_state.py"):
            (self.prefix / "bin" / name).write_text("synthetic packaged executable\n")
        self.entrypoint = runpy.run_path(str(SCRIPT))["main"]

    def run_setup(self, uid=1000):
        with patch.dict(os.environ, {"HOME": str(self.home)}), \
                patch("sys.argv", [str(SCRIPT), "--prefix", str(self.prefix)]), \
                patch("os.geteuid", return_value=uid), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return self.entrypoint()

    def test_links_are_explicit_and_idempotent(self):
        self.assertEqual(self.run_setup(), 0)
        first = {}
        for name in ("hgs", "hgs-tray", "hgs_state.py"):
            link = self.home / ".local/bin" / name
            self.assertTrue(link.is_symlink())
            self.assertEqual(link.resolve(), (self.prefix / "bin" / name).resolve())
            first[name] = link.lstat().st_mtime_ns
        self.assertEqual(self.run_setup(), 0)
        self.assertEqual(first, {name: (self.home / ".local/bin" / name).lstat().st_mtime_ns for name in first})
        self.assertFalse((self.home / ".config").exists(), "setup must not enable services or change agent state")

    def test_last_conflict_prevents_all_link_creation(self):
        directory = self.home / ".local/bin"
        directory.mkdir(parents=True)
        existing = directory / "hgs_state.py"
        existing.write_text("existing user installation\n")
        with self.assertRaises(SystemExit):
            self.run_setup()
        self.assertEqual(existing.read_text(), "existing user installation\n")
        self.assertFalse((directory / "hgs").exists())
        self.assertFalse((directory / "hgs-tray").exists())

    def test_dangling_user_link_is_preserved(self):
        directory = self.home / ".local/bin"
        directory.mkdir(parents=True)
        existing = directory / "hgs"
        existing.symlink_to(self.root / "old-install")
        with self.assertRaises(SystemExit):
            self.run_setup()
        self.assertEqual(existing.readlink(), self.root / "old-install")
        self.assertFalse((directory / "hgs-tray").exists())

    def test_missing_packaged_file_does_not_create_home_paths(self):
        (self.prefix / "bin/hgs_state.py").unlink()
        with self.assertRaises(SystemExit):
            self.run_setup()
        self.assertFalse((self.home / ".local").exists())

    def test_root_setup_is_rejected(self):
        with self.assertRaises(SystemExit):
            self.run_setup(uid=0)
        self.assertFalse((self.home / ".local").exists())


    def test_desktop_registration_forwards_exact_selected_cli_and_configuration(self):
        tool = self.prefix / "lib/zerus/install-linux-desktop-service.py"
        tool.parent.mkdir(parents=True)
        tool.write_text("fixture helper\n")
        args = ["zerus-setup", "--prefix", str(self.prefix), "--desktop-service", "install",
                "--hgs", "/opt/example/hgs selected", "--config-dir", "/data/example/config",
                "--state-dir", "/data/example/state", "--no-start"]
        with patch.dict(os.environ, {"HOME": str(self.home)}), patch("sys.argv", args), \
                patch("os.geteuid", return_value=1000), \
                patch("subprocess.run", return_value=SimpleNamespace(returncode=0)) as run:
            self.assertEqual(self.entrypoint(), 0)
        # zerus-setup resolves --prefix; macOS temporary directories are symlinked.
        self.assertEqual(run.call_args.args[0][1:], [str(tool.resolve()), "install", "--hgs", "/opt/example/hgs selected",
                                                   "--config-dir", "/data/example/config",
                                                   "--state-dir", "/data/example/state", "--no-start"])
        self.assertFalse((self.home / ".local").exists(), "service registration is separate from compatibility links")


if __name__ == "__main__":
    unittest.main()
