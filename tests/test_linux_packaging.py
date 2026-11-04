"""Linux staging and the dependency notices for the features actually shipped."""
import contextlib
import io
import json
from pathlib import Path
import runpy
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


class LinuxPackaging(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zerus-linux-package-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def file(self, name, contents="fixture\n"):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)
        return path

    def stage(self, gui="tauri"):
        for name in ("README.md", "CONTRIBUTING.md", "LICENSE", "THIRD_PARTY_NOTICES.md", "tmux.conf",
                     "hgs_state.py", "packaging/linux/zerus-setup", "packaging/linux/hgs-tray.desktop",
                     "packaging/linux/hgs-tray.service", "scripts/install-mobile-connector-service.py",
                     "scripts/install-linux-desktop-service.py",
                     "scripts/desktop-revoke-hold.py",
                     "docs/licenses/codex-LICENSE.txt", "tray/resources/icons/hgs-zerus-symbolic.svg"):
            self.file("source/" + name)
        for size in (16, 24, 32, 48, 64, 128, 256, 512, 1024):
            self.file(f"source/tray/resources/icons/hgs-zerus-{size}.png")
        self.file("source/VERSION", "1.2.3\n")
        for name in ("Cargo.toml", "services/mobile/connector/Cargo.toml"):
            self.file("source/" + name, '[package]\nversion = "1.2.3"\n')
        licenses = self.file("licenses/inventory.json", '{"packages": [{"name": "tauri"}]}').parent
        binaries = [self.file("bin/" + name) for name in ("hgs", "hgs-tray", "connector")]
        dest = self.root / "package"
        main = runpy.run_path(str(ROOT / "scripts/package-linux.py"))["main"]
        main.__globals__["ROOT"] = self.root / "source"
        args = ["package-linux.py", "--destdir", str(dest), "--cli", str(binaries[0]),
                "--gui", str(binaries[1]), "--connector", str(binaries[2]), "--licenses", str(licenses)]
        with patch.object(sys, "argv", args), \
                patch("subprocess.check_output", return_value=json.dumps({"version": "1.2.3", "ZerusGUI": gui})), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            main()
        return dest

    def test_next_stages_without_qt_plugin_or_vendor_notices(self):
        dest = self.stage()
        self.assertTrue((dest / "usr/bin/hgs-tray").is_file())
        self.assertFalse((dest / "usr/lib/hgs-tray/plugins").exists())
        self.assertFalse((dest / "usr/share/licenses/zerus-git/libvterm-LICENSE").exists())
        info = json.loads((dest / "usr/share/doc/zerus/build-info.json").read_text())
        self.assertEqual(info["desktop_build"]["ZerusGUI"], "tauri")
        self.assertTrue((dest / "usr/lib/systemd/user/hgs-tray.service").is_file())
        self.assertTrue((dest / "usr/lib/zerus/desktop-revoke-hold.py").is_file())

    def test_qt_binary_is_refused_by_next_packager(self):
        with self.assertRaises(SystemExit):
            self.stage(gui="qt")

    def test_notices_resolve_video_for_client_and_default_features_for_cli(self):
        calls = []

        def output(command, **kwargs):
            calls.append(command)
            if command[1] == "tree":
                return ""
            return json.dumps({"packages": [], "resolve": {"nodes": []}})

        manifest = "client/src-tauri/Cargo.toml"
        args = ["collect-licenses.py", str(self.root / "notices"), "--manifest-path", "Cargo.toml",
                "--manifest-path", manifest, "--manifest-features", manifest + "=video",
                "--target", "x86_64-unknown-linux-gnu"]
        main = runpy.run_path(str(ROOT / "scripts/collect-licenses.py"))["main"]
        with patch.object(sys, "argv", args), patch("subprocess.check_output", side_effect=output), \
                contextlib.redirect_stdout(io.StringIO()):
            main()
        self.assertEqual(len(calls), 4)
        for command in calls:
            is_client = command[command.index("--manifest-path") + 1] == manifest
            self.assertEqual("--features" in command, is_client)
            if is_client:
                self.assertEqual(command[command.index("--features") + 1], "video")


if __name__ == "__main__":
    unittest.main()
