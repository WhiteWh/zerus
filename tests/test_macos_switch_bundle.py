"""The Tauri switch bundle takes Qt's identity (docs/macos-distribution.md#tauri-client-switch)."""
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
spec = importlib.util.spec_from_file_location("check_macos_bundle", ROOT / "scripts/check-macos-bundle.py")
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

QT_INFO = plistlib.loads((ROOT / "tray/packaging/Info.plist").read_bytes())
SWITCH_INFO = plistlib.loads((ROOT / "client/src-tauri/Info.switch.plist").read_bytes())
VERSION = (ROOT / "VERSION").read_text().strip()
OTOOL = """hgs-tray:
Load command 0
      cmd LC_BUILD_VERSION
  cmdsize 32
 platform 1
    minos 14.0
Load command 1
          cmd LC_LOAD_DYLIB
      cmdsize 88
         name /System/Library/Frameworks/WebKit.framework/Versions/A/WebKit (offset 24)
Load command 2
          cmd LC_LOAD_DYLIB
      cmdsize 56
         name /usr/lib/libSystem.B.dylib (offset 24)
"""
MACH_O = b"\xcf\xfa\xed\xfe" + b"\0" * 28


def installed_folder():
    install = (ROOT / "tray/install.sh").read_text()
    return re.search(r'^APP_DEST="\$\{HOME\}/Applications/(.+)\.app"$', install, re.M).group(1)


class SwitchBundle(unittest.TestCase):
    def test_the_configuration_takes_qt_identity_with_a_local_build_identifier(self):
        with tempfile.TemporaryDirectory() as scratch:
            output = Path(scratch) / "switch.conf.json"
            subprocess.run([sys.executable, str(ROOT / "client/scripts/switch-config.py"), str(output)], check=True)
            config = json.loads(output.read_text())
            generated_info = Path(config["bundle"]["macOS"]["infoPlist"])
            self.assertEqual(plistlib.loads(generated_info.read_bytes())["ZerusReleaseVersion"], VERSION)
        self.assertEqual(config["identifier"], "com.hgdev.hgs-tray.local")
        self.assertEqual((config["productName"], config["mainBinaryName"], config["version"]), ("Zerus", "hgs-tray", VERSION))
        bundle = config["bundle"]
        self.assertEqual(bundle["targets"], ["app"])
        self.assertEqual(list(bundle["resources"].values()), ["setup/com.hgdev.hgs-tray.plist"])
        self.assertEqual(bundle["macOS"]["minimumSystemVersion"], "14.0")
        self.assertTrue(generated_info.is_absolute())
        paths = [*bundle["resources"], *bundle["macOS"]["files"].values()]
        for path in paths:
            self.assertTrue(Path(path).is_absolute() and Path(path).is_file(), path)
        self.assertEqual(sorted(bundle["macOS"]["files"]), ["Resources/Assets.car", "Resources/en.lproj/InfoPlist.strings",
                                                            "Resources/hgs-zerus-swarm.icns"])

    def test_the_info_plist_keeps_qt_names_and_texts(self):
        # Launch Services shows the localized Zerus only while the unlocalized name equals the folder.
        self.assertEqual(SWITCH_INFO["CFBundleDisplayName"], installed_folder())
        self.assertIs(SWITCH_INFO["LSHasLocalizedDisplayName"], True)
        self.assertEqual(SWITCH_INFO["CFBundleName"], "Zerus")
        self.assertIs(SWITCH_INFO["LSUIElement"], True)
        self.assertEqual(SWITCH_INFO["ZerusGUI"], "tauri")
        protocol = (ROOT / "client/core/src/desktop/notifications.rs").read_text()
        abi = re.search(r'pub const ABI: &str = "([^"]+)";', protocol).group(1)
        self.assertEqual(SWITCH_INFO["HGSDesktopNotificationHostABI"], abi)
        for key in ("NSMicrophoneUsageDescription", "NSCameraUsageDescription", "CFBundleIconName", "CFBundleDevelopmentRegion"):
            self.assertEqual(SWITCH_INFO[key], QT_INFO[key], key)
        package = (ROOT / "scripts/package-macos.py").read_text()
        self.assertIn(SWITCH_INFO["NSAppleEventsUsageDescription"], package)
        self.assertEqual(json.loads((ROOT / "client/src-tauri/tauri.conf.json").read_text())["identifier"], "dev.zerus.next",
                         "the dev app keeps its own identity")

    def test_native_entry_precedes_renderer_and_is_not_tied_to_gui_live_data(self):
        main = (ROOT / "client/src-tauri/src/main.rs").read_text()
        self.assertLess(main.index("notification_host::run()"), main.index("renderer::choose()"))
        manifest = (ROOT / "client/src-tauri/Cargo.toml").read_text()
        native = manifest.split('[target.\'cfg(target_os = "macos")\'.dependencies]', 1)[1].split('\n[', 1)[0]
        self.assertIn('zerus-client-core = { path = "../core", features = ["macos-native-peer"] }', native)

    def bundle(self, root, **changes):
        app = Path(root) / "Zerus.app"
        info = dict(SWITCH_INFO, CFBundleIdentifier="com.hgdev.hgs-tray.local", CFBundleExecutable="hgs-tray",
                    CFBundleShortVersionString=VERSION, LSMinimumSystemVersion="14.0", ZerusReleaseVersion=VERSION)
        info.update(changes)
        for name in check.TAURI_REQUIRED:
            (app / name).parent.mkdir(parents=True, exist_ok=True)
            (app / name).write_bytes(b"")
        (app / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
        (app / check.TAURI_EXECUTABLE).write_bytes(MACH_O)
        (app / check.TAURI_EXECUTABLE).chmod(0o755)
        return app

    def tools(self, otool=OTOOL):
        def fake(command, **_):
            return "arm64\n" if command[0] == "lipo" else otool
        return mock.patch.object(check.subprocess, "check_output", side_effect=fake)

    def test_the_tauri_profile_accepts_the_switch_layout(self):
        with tempfile.TemporaryDirectory() as root, self.tools():
            self.assertEqual(check.tauri_bundle_findings(self.bundle(root), VERSION), [])

    def test_the_tauri_profile_refuses_what_local_signing_would_refuse(self):
        with tempfile.TemporaryDirectory() as root, self.tools():
            app = self.bundle(root, LSUIElement=False, ZerusGUI="qt")
            (app / "Contents/Frameworks/Helper.dylib").parent.mkdir(parents=True)
            (app / "Contents/Frameworks/Helper.dylib").write_bytes(MACH_O)
            (app / "Contents/Resources/link").symlink_to("/usr")
            (app / "Contents/Resources/setup/com.hgdev.hgs-tray.plist").unlink()
            findings = check.tauri_bundle_findings(app, VERSION)
        self.assertIn("Info.plist LSUIElement is False, expected True", findings)
        self.assertIn("Info.plist ZerusGUI is 'qt', expected 'tauri'", findings)
        self.assertIn("Contents/Frameworks/Helper.dylib: nested code; the switch bundle has one executable", findings)
        self.assertIn("Contents/Resources/link: symbolic link", findings)
        self.assertIn("missing Contents/Resources/setup/com.hgdev.hgs-tray.plist", findings)

    def test_homebrew_links_and_a_newer_minimum_are_refused(self):
        brewed = OTOOL.replace("/usr/lib/libSystem.B.dylib", "/opt/homebrew/lib/libexample.dylib").replace("minos 14.0", "minos 15.0")
        with tempfile.TemporaryDirectory() as root, self.tools(brewed):
            findings = check.tauri_bundle_findings(self.bundle(root), VERSION)
        self.assertIn("Contents/MacOS/hgs-tray: links /opt/homebrew/lib/libexample.dylib", findings)
        self.assertIn("Contents/MacOS/hgs-tray: requires macOS 15.0, newer than 14.0", findings)

    def test_both_identifiers_pass_and_others_do_not(self):
        good = dict(SWITCH_INFO, CFBundleExecutable="hgs-tray", CFBundleShortVersionString=VERSION,
                    LSMinimumSystemVersion="14.0", ZerusReleaseVersion=VERSION)
        for identifier in ("com.hgdev.hgs-tray", "com.hgdev.hgs-tray.local"):
            self.assertEqual(check.tauri_plist_findings(dict(good, CFBundleIdentifier=identifier), VERSION), [])
        self.assertTrue(check.tauri_plist_findings(dict(good, CFBundleIdentifier="dev.zerus.next"), VERSION))
        missing = dict(good, CFBundleIdentifier="com.hgdev.hgs-tray")
        del missing["NSMicrophoneUsageDescription"]
        self.assertIn("Info.plist lacks NSMicrophoneUsageDescription", check.tauri_plist_findings(missing, VERSION))


if __name__ == "__main__":
    unittest.main()
