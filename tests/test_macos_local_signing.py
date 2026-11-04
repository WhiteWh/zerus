"""Local rebuilds must preserve privacy identity before the installer replaces an app."""
import contextlib
import importlib.util
import io
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
spec = importlib.util.spec_from_file_location("local_sign", ROOT / "tray/sign-local-macos.py")
sign = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sign)
DEVELOPER_ID = ("A" * 40, "Developer ID Application: Example (ABCDE12345)")
DEVELOPMENT = ("B" * 40, "Apple Development: Example (ABCDE12345)")
OTHER_TEAM = ("C" * 40, "Developer ID Application: Other (FGHIJ67890)")
INSTALLED = {"Authority": DEVELOPER_ID[1], "TeamIdentifier": "ABCDE12345"}


class LocalSigning(unittest.TestCase):
    def test_identity_discovery_accepts_only_valid_application_certificates(self):
        listing = ('  1) ' + DEVELOPER_ID[0] + ' "' + DEVELOPER_ID[1] + '"\n'
                   '  2) ' + DEVELOPMENT[0] + ' "' + DEVELOPMENT[1] + '"\n'
                   '  3) ' + 'C' * 40 + ' "Developer ID Installer: Example (ABCDE12345)"\n'
                   '  4) ' + 'D' * 40 + ' "Developer ID Application: Expired (ABCDE12345)" (CSSMERR_TP_CERT_EXPIRED)\n')
        with mock.patch.object(sign, "run", return_value=subprocess.CompletedProcess([], 0, listing, "")):
            self.assertEqual(sign.identities(), [DEVELOPER_ID, DEVELOPMENT])

    def test_rebuild_chooses_the_installed_distribution_class_and_team(self):
        available = [OTHER_TEAM, DEVELOPMENT, DEVELOPER_ID]
        self.assertEqual(sign.select_identity(available, INSTALLED), DEVELOPER_ID[0])
        current = dict(INSTALLED, Authority=DEVELOPMENT[1])
        self.assertEqual(sign.select_identity(available, current), DEVELOPMENT[0])
        personal = ("D" * 40, "Apple Development: Example (KLMNO12345)")
        self.assertEqual(sign.select_identity([personal, DEVELOPER_ID], dict(current, Authority=personal[1])), personal[0])
        self.assertEqual(sign.select_identity([DEVELOPMENT, DEVELOPER_ID], None), DEVELOPER_ID[0])

    def test_source_bundle_rejects_external_or_nested_code_before_signing(self):
        with tempfile.TemporaryDirectory(prefix="zerus-sign-input-") as temporary:
            root = Path(temporary)
            app = root / "hgs-tray.app"
            executable = app / "Contents/MacOS/hgs-tray"
            executable.parent.mkdir(parents=True)
            executable.write_bytes(b"\xcf\xfa\xed\xfe")
            (app / "Contents/Info.plist").write_bytes(plistlib.dumps({
                "CFBundleIdentifier": sign.BUNDLE_ID, "CFBundleExecutable": "hgs-tray"}))
            with mock.patch.object(sign, "run") as run:
                with self.assertRaisesRegex(ValueError, "separate source bundle"):
                    sign.sign(app, app)
                nested = app / "Contents/MacOS/helper"
                nested.write_bytes(b"\xcf\xfa\xed\xfe")
                with self.assertRaisesRegex(ValueError, "nested code"):
                    sign.sign(app, root / "installed.app")
                nested.unlink()
                nested.symlink_to(executable)
                with self.assertRaisesRegex(ValueError, "contains a link"):
                    sign.sign(app, root / "installed.app")
                run.assert_not_called()

    def test_missing_ambiguous_or_adhoc_identity_never_becomes_a_fallback(self):
        for available, installed, requested in [([], None, ""), ([DEVELOPMENT], INSTALLED, ""),
                ([DEVELOPER_ID, OTHER_TEAM], None, ""), ([DEVELOPER_ID], None, "-"),
                ([DEVELOPER_ID], None, "Developer ID")]:
            with self.subTest(available=available, requested=requested), self.assertRaisesRegex(ValueError, "one compatible"):
                sign.select_identity(available, installed, requested)
        for selector in (DEVELOPER_ID[0].lower(), DEVELOPER_ID[1]):
            self.assertEqual(sign.select_identity([DEVELOPER_ID, OTHER_TEAM], None, selector), DEVELOPER_ID[0])

    def test_local_identifier_is_promoted_only_with_the_verified_certificate(self):
        with tempfile.TemporaryDirectory(prefix="zerus-local-identity-") as temporary:
            root = Path(temporary)
            app = root / "staged/hgs-tray.app"
            executable = app / "Contents/MacOS/hgs-tray"
            executable.parent.mkdir(parents=True)
            executable.write_bytes(b"\xcf\xfa\xed\xfe")
            plist = app / "Contents/Info.plist"
            original = plistlib.dumps({"CFBundleIdentifier": "com.hgdev.hgs-tray.local", "CFBundleExecutable": "hgs-tray"})
            installed = root / "installed.app"
            for failure in ("missing-key", "codesign", "continuity", None):
                with self.subTest(failure=failure):
                    plist.write_bytes(original)
                    def execute(*args):
                        self.assertEqual(plistlib.loads(plist.read_bytes())["CFBundleIdentifier"], sign.BUNDLE_ID)
                        if failure == "codesign":
                            raise subprocess.CalledProcessError(3, args, stderr="synthetic key failure")
                    with mock.patch.object(sign, "installed_identity", return_value=INSTALLED), \
                            mock.patch.object(sign, "identities", return_value=[] if failure == "missing-key" else [DEVELOPER_ID]), \
                            mock.patch.object(sign, "run", side_effect=execute) as commands, \
                            mock.patch.object(sign, "details", return_value=INSTALLED), \
                            mock.patch.object(sign, "requirement", return_value='identifier "com.hgdev.hgs-tray"'), \
                            mock.patch.object(sign, "verify_continuity", side_effect=ValueError("incompatible") if failure == "continuity" else None) as continuity:
                        if failure:
                            with self.assertRaises(ValueError):
                                sign.sign(app, installed)
                            self.assertEqual(plist.read_bytes(), original, "failed preparation keeps the local privacy identity")
                            if failure == "missing-key":
                                commands.assert_not_called()
                        else:
                            with contextlib.redirect_stdout(io.StringIO()):
                                sign.sign(app, installed)
                            self.assertEqual(plistlib.loads(plist.read_bytes())["CFBundleIdentifier"], sign.BUNDLE_ID)
                            continuity.assert_called_once_with(app, installed)

    def test_continuity_requires_both_designated_requirements(self):
        for fail_at in (0, 1, None):
            calls = []
            def execute(*args):
                calls.append(args)
                if len(calls) - 1 == fail_at:
                    raise subprocess.CalledProcessError(3, args, stderr="requirement failed")
            with self.subTest(fail_at=fail_at), mock.patch.object(sign, "requirement", side_effect=lambda app: "DR:" + app), \
                    mock.patch.object(sign, "run", side_effect=execute):
                if fail_at is None:
                    sign.verify_continuity("new", "installed")
                    self.assertEqual([call[-2:] for call in calls], [("=DR:installed", "new"), ("=DR:new", "installed")])
                else:
                    with self.assertRaisesRegex(ValueError, "incompatible"):
                        sign.verify_continuity("new", "installed")

    def test_installer_stops_before_replacement_when_signing_fails(self):
        with tempfile.TemporaryDirectory(prefix="zerus-install-sign-") as temporary:
            root = Path(temporary)
            tray, home, tools = root / "tray", root / "home", root / "bin"
            for path in (tray, home, tools):
                path.mkdir()
            installed = home / "Applications/hgs-tray.app/Contents/MacOS/hgs-tray"
            installed.parent.mkdir(parents=True)
            installed.write_bytes(b"previous installed application")
            raw = tray / "build/hgs-tray.app/Contents/Info.plist"
            raw.parent.mkdir(parents=True)
            original = plistlib.dumps({"CFBundleIdentifier": sign.LOCAL_BUNDLE_ID})
            raw.write_bytes(original)
            stage_root = root / "temporary"
            stage_root.mkdir()
            shutil.copy2(ROOT / "tray/install.sh", tray / "install.sh")
            scripts = root / "scripts"; scripts.mkdir()
            (scripts / "install-macos-desktop-service.py").write_text(
                "import sys\nassert sys.argv[2:] == ['--check-only']\n")
            # Exercise the installer boundary: failed signing must precede even
            # replacing the application or writing a LaunchAgent, with either restart policy.
            (tray / "sign-local-macos.py").write_text(
                "import pathlib, sys\n"
                "assert pathlib.Path(sys.argv[1]).resolve() != pathlib.Path(__file__).parent / 'build/hgs-tray.app'\n"
                "raise SystemExit(17)\n")
            for name, body in {"uname": "echo Darwin", "brew": "echo /fixture/qt", "cmake": "exit 0", "hgs": "exit 0",
                               "ditto": 'cp -R "$1" "$2"',
                               "launchctl": "touch \"$HOME/launchctl-called\""}.items():
                path = tools / name
                path.write_text("#!/bin/sh\n" + body + "\n")
                path.chmod(0o755)
            for restart in ("0", "1"):
                env = dict(os.environ, HOME=str(home), HGS_MAC_GUI="qt", PATH=str(tools) + os.pathsep + os.environ["PATH"],
                           HGS_TRAY_RESTART=restart, TMPDIR=str(stage_root))
                result = subprocess.run(["bash", str(tray / "install.sh")], env=env, capture_output=True, text=True, timeout=15)
                self.assertEqual(result.returncode, 17, result.stderr)
                self.assertEqual(installed.read_bytes(), b"previous installed application")
                self.assertFalse((home / "Library/LaunchAgents").exists())
                self.assertFalse((home / "launchctl-called").exists())
                self.assertEqual(raw.read_bytes(), original)
                self.assertEqual(list(stage_root.iterdir()), [], "the disposable signing copy is removed on failure")


@unittest.skipUnless(sys.platform == "darwin" and os.environ.get("ZERUS_TEST_LOCAL_MACOS_SIGNING") == "1",
                     "opt-in native signing uses an existing private key, never CI credentials")
class NativeLocalSigning(unittest.TestCase):
    def test_changed_code_keeps_mutually_compatible_identity(self):
        with tempfile.TemporaryDirectory(prefix="zerus-local-sign-") as temporary:
            root = Path(temporary)
            first, second = root / "first/hgs-tray.app", root / "second/hgs-tray.app"
            source = root / "probe.c"
            def build(app, value):
                executable = app / "Contents/MacOS/hgs-tray"
                executable.parent.mkdir(parents=True)
                (app / "Contents/Info.plist").write_bytes(plistlib.dumps({
                    "CFBundleIdentifier": sign.BUNDLE_ID, "CFBundleExecutable": "hgs-tray",
                    "CFBundlePackageType": "APPL", "CFBundleVersion": "1"}))
                source.write_text(f"int main(void) {{ return {value}; }}\n")
                subprocess.run(["xcrun", "clang", str(source), "-o", str(executable)], check=True, capture_output=True)
            build(first, 0)
            build(second, 1)
            # Select the current installation's certificate when available, but
            # never replace, launch, reset permissions for, or re-sign that app.
            installed = Path.home() / "Applications/hgs-tray.app"
            requested = os.environ.get("HGS_MACOS_SIGN_IDENTITY", "")
            with contextlib.redirect_stdout(io.StringIO()):
                try:
                    sign.sign(first, installed, requested)
                    sign.sign(second, first, requested)
                except subprocess.CalledProcessError as error:
                    self.fail(error.stderr)
            self.assertNotEqual(sign.details(first)["CDHash"], sign.details(second)["CDHash"])
            sign.verify_continuity(first, second)
            if installed.exists() and sign.installed_identity(installed):
                sign.verify_continuity(second, installed)
            # An ad hoc rebuild has the same identifier yet fails this contract.
            subprocess.run(["codesign", "--force", "--sign", "-", str(second)], check=True, capture_output=True)
            with self.assertRaisesRegex(ValueError, "incompatible"):
                sign.verify_continuity(second, first)


if __name__ == "__main__":
    unittest.main()
