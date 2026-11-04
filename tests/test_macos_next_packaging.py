"""Credential-free regression controls for shipping Zerus Next on macOS."""
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))


def load(name, relative):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


package = load("next_package", "scripts/package-macos.py")
check = load("next_check", "scripts/check-macos-bundle.py")
notices = load("next_notices", "client/scripts/collect-notices.py")
sign = load("next_sign", "scripts/sign-macos.py")
import macos_release  # noqa: E402

VERSION = (ROOT / "VERSION").read_text().strip()
DEV_BASE = (ROOT / "DEV_VERSION").read_text().strip()
MAGIC = b"\xcf\xfa\xed\xfe" + b"\0" * 28
OTOOL = "cmd LC_BUILD_VERSION\nminos 14.0\ncmd LC_LOAD_DYLIB\nname /usr/lib/libSystem.B.dylib (offset 24)\n"


def inventory(directory):
    folder = directory / "example-1.0.0"
    folder.mkdir(parents=True)
    (folder / "LICENSE").write_text("Synthetic license fixture; never distributed.")
    (directory / "inventory.json").write_text(json.dumps({
        "packages": [{"name": "example", "version": "1.0.0", "license": "MIT"}]}))


class NextPackaging(unittest.TestCase):
    def test_switch_config_embeds_each_release_without_modifying_the_template(self):
        template = ROOT / "client/src-tauri/Info.switch.plist"
        before = template.read_bytes()
        for release in (VERSION, VERSION + ".r42.gabcdef0.n8", DEV_BASE + "-dev.17"):
            with self.subTest(release=release), tempfile.TemporaryDirectory() as temporary:
                output = Path(temporary) / "switch.json"
                result = subprocess.run([sys.executable, str(ROOT / "client/scripts/switch-config.py"),
                                         str(output), "--release", release],
                                        capture_output=True, text=True, check=True)
                data = json.loads(output.read_text())
                self.assertEqual(data["version"], VERSION)
                info = plistlib.loads(Path(data["bundle"]["macOS"]["infoPlist"]).read_bytes())
                self.assertEqual(info["ZerusReleaseVersion"], release)
                self.assertEqual(template.read_bytes(), before)
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "switch.json"
            subprocess.run([sys.executable, str(ROOT / "client/scripts/switch-config.py"), str(output)],
                           env={**os.environ, "ZERUS_DEV_BUILD_NUMBER": "23"}, check=True)
            info = plistlib.loads(Path(json.loads(output.read_text())["bundle"]["macOS"]["infoPlist"]).read_bytes())
            self.assertEqual(info["ZerusReleaseVersion"], DEV_BASE + "-dev.23")

    def test_tauri_packaging_keeps_the_raw_app_and_ships_only_its_actual_notices(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            app, output = root / "raw.app", root / "output"
            info_path = app / "Contents/Info.plist"
            info_path.parent.mkdir(parents=True)
            info = plistlib.loads((ROOT / "client/src-tauri/Info.switch.plist").read_bytes())
            info.update(CFBundleIdentifier="com.hgdev.hgs-tray.local", CFBundleShortVersionString=VERSION,
                        CFBundleExecutable="hgs-tray", ZerusReleaseVersion=DEV_BASE + "-dev.17")
            original = plistlib.dumps(info); info_path.write_bytes(original)
            binary = app / "Contents/MacOS/hgs-tray"; binary.parent.mkdir(); binary.write_bytes(MAGIC)
            binary.chmod(0o755)
            cli = root / "hgs"; cli.write_bytes(MAGIC)
            for name in ("rust", "client-rust", "frontend"):
                inventory(root / name)
            commands = []
            def run(*args):
                commands.append(args)
                if args[0] == "ditto":
                    shutil.copytree(args[1], args[2])
                else:
                    self.assertEqual(args[0], "codesign")
            argv = ["package-macos.py", "--gui", "tauri", "--app", str(app), "--cli", str(cli),
                    "--connector", str(cli), "--licenses", str(root / "rust"),
                    "--client-licenses", str(root / "client-rust"), "--frontend-licenses", str(root / "frontend"),
                    "--version", VERSION, "--release", DEV_BASE + "-dev.17", "--output", str(output)]
            with mock.patch.object(sys, "argv", argv), mock.patch.object(package, "run", side_effect=run), \
                    mock.patch.object(package, "ffmpeg_provenance", side_effect=AssertionError("Qt must not be used")):
                package.main()
            bundle = output / "Zerus.app"
            final = plistlib.loads((bundle / "Contents/Info.plist").read_bytes())
            self.assertEqual(final["CFBundleIdentifier"], "com.hgdev.hgs-tray")
            self.assertEqual(final["CFBundleDisplayName"], "Zerus")
            self.assertEqual(final["ZerusReleaseVersion"], DEV_BASE + "-dev.17")
            self.assertEqual(info_path.read_bytes(), original)
            self.assertEqual((bundle / "Contents/MacOS/hgs").read_bytes(), MAGIC)
            self.assertFalse((bundle / "Contents/Resources/licenses/qt").exists())
            self.assertFalse((bundle / "Contents/Resources/licenses/libvterm-LICENSE").exists())
            for name in ("rust", "client-rust", "frontend"):
                self.assertTrue((bundle / "Contents/Resources/licenses" / name / "inventory.json").is_file())
            self.assertEqual(commands[-1][0], "codesign")

    def distribution(self, root):
        bundle = root / "Zerus.app"
        for name in check.TAURI_RELEASE_REQUIRED:
            path = bundle / name; path.parent.mkdir(parents=True, exist_ok=True); path.write_bytes(b"")
        info = plistlib.loads((ROOT / "client/src-tauri/Info.switch.plist").read_bytes())
        info.update(CFBundleIdentifier="com.hgdev.hgs-tray", CFBundleExecutable="hgs-tray",
                    CFBundleDisplayName="Zerus", CFBundleVersion=VERSION,
                    CFBundleShortVersionString=VERSION, LSMinimumSystemVersion="14.0", ZerusReleaseVersion=VERSION)
        (bundle / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
        for name in check.EXECUTABLES:
            path = bundle / name; path.write_bytes(MAGIC); path.chmod(0o755)
        for name in ("rust", "client-rust", "frontend"):
            inventory(bundle / "Contents/Resources/licenses" / name)
        return bundle

    def test_public_tauri_profile_checks_cli_notices_identity_and_refuses_qt(self):
        with tempfile.TemporaryDirectory() as temporary, mock.patch.object(
                check.subprocess, "check_output", side_effect=lambda args, **kw: "arm64\n" if args[0] == "lipo" else OTOOL):
            bundle = self.distribution(Path(temporary))
            self.assertEqual(check.tauri_bundle_findings(bundle, VERSION, VERSION), [])
            (bundle / "Contents/MacOS/hgs").unlink()
            (bundle / "Contents/Resources/licenses/frontend/example-1.0.0/LICENSE").unlink()
            (bundle / "Contents/Frameworks/QtCore.framework").mkdir(parents=True)
            findings = check.tauri_bundle_findings(bundle, VERSION, VERSION)
            self.assertIn("missing Contents/MacOS/hgs", findings)
            self.assertIn("frontend: invalid or incomplete dependency notices", findings)
            self.assertIn("Contents/Frameworks/QtCore.framework: Qt is not part of the Zerus Next bundle", findings)

    def test_public_selftest_never_uses_qts_selftest_switch(self):
        calls = []
        def run(args, **kwargs):
            calls.append(args)
            self.assertNotIn("DISPLAY", kwargs["env"])
            self.assertNotIn("DBUS_SESSION_BUS_ADDRESS", kwargs["env"])
            output = json.dumps({"version": VERSION, "devBuildNumber": 0, "versionName": VERSION})
            return subprocess.CompletedProcess(args, 0, output, "")
        with mock.patch.object(check.subprocess, "run", side_effect=run):
            self.assertEqual(check.tauri_selftest(Path("/synthetic/Zerus.app"), VERSION, VERSION), [])
        self.assertEqual([args[-1] for args in calls], ["--version", "--version", "--version", "--build-info"])
        self.assertNotIn("--selftest", [item for args in calls for item in args])

    def test_feed_metadata_keeps_the_required_legacy_qt_version_for_every_channel(self):
        for release, channel in ((VERSION, "stable"), (VERSION + ".r4.gabcdef0.n8", "nightly"),
                                 (DEV_BASE + "-dev.17", "dev")):
            with self.subTest(channel=channel), tempfile.TemporaryDirectory() as temporary:
                output = Path(temporary) / "signed"
                with mock.patch.object(sign.subprocess, "run"):
                    sign.release_archive(Path("/synthetic/Zerus.app"), output, release,
                                         "0123abcd-4567-89ef-0123-456789abcdef",
                                         macos_release.LEGACY_QT_VERSION, "ABCDE12345")
                metadata = json.loads((output / "macos-info.json").read_text())
                self.assertEqual(metadata["qt_version"], "6.11.2")
                self.assertEqual(macos_release.validate_macos_info(
                    {"macos": metadata, "homebrew_casks": [macos_release.cask_for(channel)]}, release, channel), metadata)

    def test_next_signer_does_not_read_or_require_a_qt_installation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "VERSION").write_text(VERSION)
            (root / "DEV_VERSION").write_text(DEV_BASE)
            app = root / "raw/Zerus.app"
            (app / "Contents/MacOS").mkdir(parents=True)
            (app / "Contents/MacOS/hgs-tray").write_bytes(MAGIC)
            (app / "Contents/Info.plist").write_bytes(plistlib.dumps({
                "CFBundleExecutable": "hgs-tray", "CFBundleShortVersionString": VERSION,
                "ZerusGUI": "tauri", "ZerusReleaseVersion": VERSION}))
            def run(args, **kwargs):
                if args[0] == "ditto":
                    shutil.copytree(args[1], args[2])
                return subprocess.CompletedProcess(args, 0, "", "TeamIdentifier=ABCDE12345\n")
            argv = ["sign-macos.py", "--app", str(app), "--identity", "synthetic-test-identity",
                    "--team-id", "ABCDE12345", "--version", VERSION, "--output", str(root / "signed"),
                    "--notary-key-file", str(root / "synthetic-notary-key"), "--notary-key-id", "synthetic-key-id"]
            with mock.patch.object(sys, "argv", argv), mock.patch.object(sign, "ROOT", root), \
                    mock.patch.object(sign.subprocess, "run", side_effect=run), \
                    mock.patch.object(sign, "notarize", return_value="0123abcd-4567-89ef-0123-456789abcdef"), \
                    mock.patch.object(sign, "sign_disk_image", return_value="1123abcd-4567-89ef-0123-456789abcdef"), \
                    mock.patch.object(sign, "release_archive", return_value=root / "candidate.zip") as archive:
                sign.main()
            self.assertEqual(archive.call_args.args[4], macos_release.LEGACY_QT_VERSION)
            self.assertFalse((root / "packaging/macos/qt.json").exists())

    def test_all_casks_use_next_bundle_without_quitting_or_writing_user_homes(self):
        for name in macos_release.CASKS.values():
            text = (ROOT / "packaging/homebrew" / (name + ".rb.in")).read_text()
            self.assertIn('app "Zerus.app"', text)
            for forbidden in ("postflight", "preflight", "uninstall ", "zap ", "launchctl:", "trash:", "quit:"):
                self.assertNotRegex(text, r"(?m)^  " + forbidden)


class FrontendNotices(unittest.TestCase):
    def client(self, root):
        package = root / "node_modules/example"
        package.mkdir(parents=True)
        (package / "package.json").write_text(json.dumps({"name": "example", "version": "1.0.0", "license": "MIT"}))
        (package / "LICENSE").write_text("Synthetic fixture license.")
        (root / "package-lock.json").write_text(json.dumps({"packages": {
            "": {}, "node_modules/example": {"version": "1.0.0"},
            "node_modules/build-only": {"version": "1.0.0", "dev": True},
            "node_modules/other-platform": {"version": "1.0.0", "optional": True}}}))
        return package

    def test_collects_installed_runtime_notices_and_omits_build_tools(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); self.client(root)
            result = notices.collect(root, root / "notices")
            self.assertEqual([row["name"] for row in result], ["example"])
            self.assertEqual((root / "notices/example-1.0.0/LICENSE").read_text(), "Synthetic fixture license.")

    def test_missing_notice_or_wrong_installed_version_fails_the_distribution(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); package = self.client(root)
            (package / "LICENSE").unlink()
            with self.assertRaisesRegex(ValueError, "No license text"):
                notices.collect(root, root / "missing")
            (package / "package.json").write_text(json.dumps({"name": "example", "version": "2.0.0"}))
            with self.assertRaisesRegex(ValueError, "differs from the lock"):
                notices.collect(root, root / "wrong")

    def test_vites_generated_runtime_helpers_keep_the_build_tools_notice(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); self.client(root)
            vite = root / "node_modules/vite"; vite.mkdir()
            (vite / "package.json").write_text(json.dumps({"name": "vite", "version": "8.0.0", "license": "MIT"}))
            (vite / "LICENSE").write_text("Synthetic Vite runtime-helper notice.")
            lock = json.loads((root / "package-lock.json").read_text())
            lock["packages"]["node_modules/vite"] = {"version": "8.0.0", "dev": True}
            (root / "package-lock.json").write_text(json.dumps(lock))
            result = notices.collect(root, root / "notices")
            self.assertEqual([row["name"] for row in result], ["example", "vite"])


if __name__ == "__main__":
    unittest.main()
