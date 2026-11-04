#!/usr/bin/env python3
"""The per-user Windows installer: names, the unchanged payload and the Inno Setup
script's contract (docs/superpowers/specs/2026-10-18-windows-installer-design.md).
Runs on every platform; installing it is tested on Windows by
test_windows_installer_survival.py."""
import importlib.util
import json
from pathlib import Path
import re
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
import conpty_pair
import windows_installer
import windows_portable
from windows_portable_fixture import pinned_pair, portable_zip

ROOT = Path(__file__).resolve().parents[1]
ISS = ROOT / "packaging" / "windows" / "zerus.iss"
COMMIT = "c" * 40


def load_packager():
    spec = importlib.util.spec_from_file_location("package_windows_installer",
                                                  ROOT / "scripts" / "package-windows-installer.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Names(unittest.TestCase):
    def test_the_installer_is_named_after_the_zip_version(self):
        self.assertEqual(windows_installer.installer_name("0.45.0"), "zerus-0.45.0-windows-x86_64-setup.exe")
        self.assertEqual(windows_installer.installer_name("0.45.0.r12.gabcdef0.n7"),
                         "zerus-0.45.0.r12.gabcdef0.n7-windows-x86_64-setup.exe")

    def test_file_versions_have_four_numeric_parts(self):
        self.assertEqual(windows_installer.numeric_version("0.45.0"), "0.45.0.0")
        self.assertEqual(windows_installer.numeric_version("0.45.0.r12.gabcdef0.n7"), "0.45.0.7")
        self.assertEqual(windows_installer.numeric_version("0.45.0.r12.gabcdef0.n70000"), "0.45.0.0")
        for version in ("0.45", "0.45.0-dev.3", "0.45.0.r1.gzzzzzzz.n1", "0.45.0.r0.gabcdef0.n1", "0.45.0.r1.gabcdef0.n0", ""):
            with self.assertRaises(ValueError, msg=version):
                windows_installer.numeric_version(version)


class Payload(unittest.TestCase):
    def setUp(self):
        pin = patch.object(conpty_pair, "PIN", pinned_pair())
        pin.start()
        self.addCleanup(pin.stop)
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.stage = self.root / "payload"
        self.stage.mkdir()

    def zip(self, entries=()):
        return portable_zip(self.root / "zerus.zip", commit=COMMIT, version="0.45.0", entries=entries)

    def test_every_zip_file_is_staged_byte_for_byte_with_the_installation_record(self):
        archive = self.zip(entries=[(f"{windows_portable.TOP}/platforms/qwindows-extra.dll", b"\x00\xffthird-party")])
        record, expected = windows_installer.stage(archive, self.stage)
        self.assertEqual((record["version"], record["product_version"], record["source_commit"]), ("0.45.0", "0.45.0", COMMIT))
        with zipfile.ZipFile(archive) as bundle:
            for entry in bundle.infolist():
                relative = entry.filename[len(windows_portable.TOP) + 1:]
                self.assertEqual((self.stage / relative).read_bytes(), bundle.read(entry), relative)
                self.assertIn(relative, expected)
            zipped = len(bundle.infolist())
        self.assertEqual(len(expected), zipped + 2)
        record = (self.stage / "installation.json").read_text(encoding="utf-8")
        self.assertEqual(record, '{\n  "schema": 1,\n  "kind": "installer",\n  "product": "Zerus",\n'
                                 '  "version": "0.45.0",\n  "product_version": "0.45.0",\n'
                                 f'  "source_commit": "{COMMIT}"\n}}\n')
        listing = (self.stage / "uninstall" / "payload.txt").read_bytes().decode("utf-8")
        lines = listing.split("\r\n")
        self.assertEqual(lines[-1], "")
        self.assertEqual(lines[:-1], sorted(name.replace("/", "\\") for name in expected
                                            if name != windows_installer.PAYLOAD_LIST))
        self.assertIn("platforms\\qwindows-extra.dll", lines)
        self.assertIn("installation.json", lines)
        self.assertEqual(windows_installer.problems(self.stage, expected), [])

    def test_a_nightly_installer_records_its_nightly_version(self):
        nightly = f"0.45.0.r812.g{COMMIT[:7]}.n42"
        record, _ = windows_installer.stage(self.zip(), self.stage, nightly)
        self.assertEqual((record["version"], record["product_version"]), (nightly, "0.45.0"))
        self.assertEqual(json.loads((self.stage / "installation.json").read_text(encoding="utf-8")), record)

    def test_a_version_that_does_not_suit_the_zip_is_refused_before_anything_is_written(self):
        archive = self.zip()
        for version, reason in (("0.99.0", "not the zip's product version"), ("0.44.9", "not the zip's product version"),
                                (f"0.44.0.r812.g{COMMIT[:7]}.n42", "not a build of"),
                                ("0.45.0.r812.gabcdef0.n42", "not a build of"),
                                ("0.45.0-dev.3", "Dev builds have no Windows build"),
                                ("0.46.0-dev.3", "Dev builds have no Windows build"),
                                (f"0.45.0.r0.g{COMMIT[:7]}.n42", "Unsupported"), ("v0.45.0", "Unsupported")):
            with self.subTest(version=version):
                with self.assertRaisesRegex(ValueError, reason):
                    windows_installer.stage(archive, self.stage, version)
                self.assertEqual(list(self.stage.iterdir()), [])

    def test_staging_needs_an_empty_folder(self):
        (self.stage / "left-over.dll").write_bytes(b"old")
        with self.assertRaisesRegex(ValueError, "empty folder"):
            windows_installer.stage(self.zip(), self.stage)

    def test_a_zip_cannot_bring_the_files_the_installer_adds(self):
        for name in ("installation.json", "Installation.JSON", "uninstall/unins000.exe", "Uninstall/payload.txt"):
            with self.subTest(name=name):
                (self.root / "zerus.zip").unlink(missing_ok=True)
                with self.assertRaisesRegex(ValueError, "installer adds it"):
                    windows_installer.stage(self.zip(entries=[(f"{windows_portable.TOP}/{name}", "x")]), self.stage)
                # Nothing was extracted before the refusal.
                self.assertEqual(list(self.stage.iterdir()), [])

    def test_payload_names_are_ascii(self):
        with self.assertRaisesRegex(ValueError, "ASCII"):
            windows_installer.stage(self.zip(entries=[(f"{windows_portable.TOP}/licenses/café.txt", "x")]),
                                    self.stage)

    def test_an_unchecked_zip_is_refused(self):
        archive = self.root / "zerus.zip"
        with zipfile.ZipFile(archive, "w") as bundle:
            bundle.writestr(f"{windows_portable.TOP}/hgs.exe", "only")
        with self.assertRaisesRegex(ValueError, "lacks"):
            windows_installer.stage(archive, self.stage)


class Comparison(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.folder = Path(temporary.name)
        (self.folder / "platforms").mkdir()
        (self.folder / "uninstall").mkdir()
        (self.folder / "hgs.exe").write_bytes(b"cli")
        (self.folder / "platforms" / "qwindows.dll").write_bytes(b"qt")
        (self.folder / "uninstall" / "payload.txt").write_bytes(b"list")
        self.expected = {name: windows_installer.sha256(self.folder / name)
                         for name in ("hgs.exe", "platforms/qwindows.dll", "uninstall/payload.txt")}

    def test_the_uninstaller_is_the_only_allowed_addition(self):
        (self.folder / "uninstall" / "unins000.exe").write_bytes(b"inno")
        (self.folder / "uninstall" / "unins000.dat").write_bytes(b"log")
        self.assertEqual(windows_installer.problems(self.folder, self.expected), [])
        (self.folder / "uninstall" / "notes.txt").write_bytes(b"?")
        (self.folder / "stale.dll").write_bytes(b"old")
        self.assertEqual(windows_installer.problems(self.folder, self.expected),
                         ["extra stale.dll", "extra uninstall/notes.txt"])

    def test_changed_and_missing_files_are_reported(self):
        (self.folder / "platforms" / "qwindows.dll").write_bytes(b"qt, re-signed")
        (self.folder / "hgs.exe").unlink()
        self.assertEqual(windows_installer.problems(self.folder, self.expected),
                         ["missing hgs.exe", "changed platforms/qwindows.dll"])

    def test_names_compare_as_ntfs_does(self):
        (self.folder / "hgs.exe").rename(self.folder / "HGS.EXE")
        self.assertEqual(windows_installer.problems(self.folder, self.expected), [])


class Packager(unittest.TestCase):
    def test_the_compiler_receives_every_definition_the_script_requires(self):
        packager = load_packager()
        output = Path("/out/zerus-0.45.0.r3.gabcdef0.n9-windows-x86_64-setup.exe")
        command = packager.command(Path("ISCC.exe"), Path("/payload"), output, "0.45.0.r3.gabcdef0.n9")
        self.assertEqual(command[:2], ["ISCC.exe", "/Q"])
        self.assertEqual(command[-1], str(ISS))
        definitions = dict(item[2:].split("=", 1) for item in command[2:-1])
        self.assertEqual(definitions, {"AppVersion": "0.45.0.r3.gabcdef0.n9", "NumericVersion": "0.45.0.9",
                                       "PayloadDir": str(Path("/payload")), "OutputDir": str(output.parent),
                                       "OutputName": output.stem, "IconFile": str(packager.ICON), "TauriDesktop": "0"})
        required = set(re.findall(r"(?m)^#ifndef (\w+)$", ISS.read_text(encoding="utf-8")))
        self.assertEqual(required, set(definitions))
        self.assertTrue(packager.ICON.is_file())


class PackagerVersions(unittest.TestCase):
    """The packager checks --version against the zip before it stages or compiles."""

    def setUp(self):
        pin = patch.object(conpty_pair, "PIN", pinned_pair())
        pin.start()
        self.addCleanup(pin.stop)
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.archive = portable_zip(self.root / "zerus.zip", commit=COMMIT, version="0.45.0")
        self.packager = load_packager()

    def main(self, *arguments):
        with patch.object(sys, "argv", ["package-windows-installer.py", str(self.archive), *arguments]):
            self.packager.main()

    def test_a_mismatched_version_never_reaches_the_compiler(self):
        iscc = self.root / "ISCC.exe"
        iscc.write_bytes(b"")
        for version in ("0.99.0", "0.45.0.r812.gabcdef0.n42", "0.45.0-dev.3"):
            with self.subTest(version=version), patch.object(self.packager.subprocess, "run") as compile_:
                with self.assertRaises(ValueError):
                    self.main("--version", version, "--iscc", str(iscc), "--output", str(self.root / "setup.exe"))
                compile_.assert_not_called()
                self.assertFalse((self.root / "setup.exe").exists())

    def test_stage_only_refuses_a_mismatched_version_and_stages_nothing(self):
        staged = self.root / "staged"
        with self.assertRaisesRegex(ValueError, "not the zip's product version"):
            self.main("--version", "0.99.0", "--stage-only", str(staged), "--output", str(self.root / "setup.exe"))
        self.assertEqual(list(staged.iterdir()), [])

    def test_compiled_ci_metadata_binds_both_stable_and_nightly_payloads(self):
        import windows_installer_release
        before = self.archive.read_bytes()
        for index, version in enumerate(["0.45.0", f"0.45.0.r812.g{COMMIT[:7]}.n42"]):
            with self.subTest(version=version):
                output = self.root / f"setup-{index}.exe"
                info = self.root / f"setup-{index}.json"
                def compile_(command, **kwargs):
                    definitions = dict(arg[2:].split("=", 1) for arg in command[2:-1])
                    record = json.loads((Path(definitions["PayloadDir"]) / "installation.json").read_text())
                    self.assertEqual(definitions["AppVersion"], version)
                    self.assertEqual(record["version"], version)
                    output.write_bytes(b"MZsynthetic compiled installer")
                with patch.object(self.packager, "compiler", return_value=Path("ISCC.exe")), \
                        patch.object(self.packager.subprocess, "run", side_effect=compile_):
                    self.main("--version", version, "--output", str(output), "--info", str(info))
                windows_installer_release.require_build(output, self.archive, info, version, "0.45.0", COMMIT)
                self.assertEqual(self.archive.read_bytes(), before)
                with patch.object(self.packager.subprocess, "run") as compile_:
                    with self.assertRaisesRegex(ValueError, "fresh compiled"):
                        self.main("--version", version, "--output", str(output), "--info", str(info))
                    compile_.assert_not_called()

    def test_the_compiler_receives_the_version_installation_json_records(self):
        nightly = f"0.45.0.r812.g{COMMIT[:7]}.n42"
        staged = self.root / "staged"
        with patch("builtins.print") as printed:
            self.main("--version", nightly, "--stage-only", str(staged), "--output", str(self.root / "setup.exe"))
        command = printed.call_args_list[0].args[0]
        self.assertIn(f"/DAppVersion={nightly} ", command)
        self.assertIn("/DNumericVersion=0.45.0.42 ", command)
        record = json.loads((staged / "installation.json").read_text(encoding="utf-8"))
        self.assertEqual((record["version"], record["product_version"]), (nightly, "0.45.0"))


class Script(unittest.TestCase):
    def setUp(self):
        self.text = ISS.read_text(encoding="utf-8")
        self.setup = dict(re.findall(r"(?m)^(\w+)=(.*)$", self.text.split("[Setup]", 1)[1].split("\n[", 1)[0]))

    def test_it_installs_per_user_into_its_own_folder(self):
        for key, value in {"PrivilegesRequired": "lowest", "DefaultDirName": r"{autopf}\Zerus",
                           "DisableDirPage": "yes", "UninstallFilesDir": r"{app}\uninstall",
                           "ArchitecturesInstallIn64BitMode": "x64compatible", "MinVersion": "10.0.18362",
                           "CloseApplications": "yes", "RestartApplications": "no",
                           "ChangesEnvironment": "no", "ChangesAssociations": "no"}.items():
            self.assertEqual(self.setup.get(key), value, key)
        self.assertNotIn("PrivilegesRequiredOverridesAllowed", self.setup)
        # Upgrades and the uninstaller find earlier installs by this identity.
        self.assertEqual(self.setup["AppId"], "{{6F1B0C52-7E3A-4D8B-9C41-2A5E8F0D3B17}")

    def test_tauri_checks_evergreen_before_changes_and_login_is_an_explicit_unchecked_task(self):
        prepare = self.procedure("PrepareToInstall")
        self.assertLess(prepare.index("if not WebView2Installed()"), prepare.index("LinkedPath"))
        self.assertIn("HKCU", self.procedure("WebView2Installed"))
        self.assertIn("HKLM32", self.procedure("WebView2Installed"))
        self.assertIn("F3017226-FE2A-4295-8BDF-00C3A9A7E4C5", self.procedure("WebView2Installed"))
        self.assertIn('Name: "login"; Description: "Start Zerus at login"; Flags: unchecked', self.text)
        self.assertIn('Parameters: "--install-login-item"; Tasks: login;', self.text)
        uninstall = self.text.split("[UninstallRun]", 1)[1].split("\n[", 1)[0]
        self.assertIn('Parameters: "--remove-login-item"', uninstall)
        self.assertNotIn("RegWrite", self.text)
        self.assertNotIn("RegDelete", self.text)
        self.assertNotIn("/silent /install", self.text)
        hook = self.procedure("DesktopServiceRegistration")
        self.assertIn("desktop registration install --json", hook)
        self.assertIn("desktop notification-registration install --json", hook)
        self.assertLess(hook.index("desktop registration install"), hook.index("desktop notification-registration install"))
        self.assertIn("if not Installing then", hook)
        self.assertIn("UI-only removal keeps", hook)
        self.assertNotIn("desktop registration remove", hook)
        self.assertNotIn("notification-registration remove", hook)
        self.assertNotIn("schtasks", hook.lower())
        self.assertNotIn("desktop revoke", hook)
        self.assertIn("DesktopServiceRegistration(True)", self.procedure("CurStepChanged"))
        self.assertIn("DesktopServiceRegistration(False)", self.procedure("CurUninstallStepChanged"))

    def test_it_touches_nothing_outside_its_folder_and_the_start_menu(self):
        sections = set(re.findall(r"(?m)^\[(\w+)\]$", self.text))
        self.assertEqual(sections, {"Setup", "Files", "Icons", "Run", "Tasks", "UninstallRun", "UninstallDelete", "Code"})
        body = self.text.split("[Setup]", 1)[1]
        for forbidden in ("{userprofile}", "{%USERPROFILE", "{localappdata}\\hgs", ".local", ".config",
                          "{commonpf}", "{sys}", "restartreplace", "uninsneveruninstall", "Registry",
                          "deleteafterinstall", "{tmp}"):
            self.assertNotIn(forbidden, body.split("[Code]")[0], forbidden)
        self.assertEqual(re.findall(r"(?m)^Source: (.*)$", self.text),
                         ['"{#PayloadDir}\\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs'])
        self.assertEqual(re.findall(r"(?m)^Name: (.*)$", self.text.split("[Icons]", 1)[1].split("\n[", 1)[0]),
                         ['"{autoprograms}\\Zerus"; Filename: "{app}\\hgs-tray.exe"; WorkingDir: "{app}"; '
                          'AppUserModelID: "com.hgdev.hgs-tray"; '
                          'AppUserModelToastActivatorCLSID: "0F0F70A2-1D40-47D8-AC5E-D6A9B789605F"',
                          '"{autoprograms}\\Zerus"; Filename: "{app}\\hgs-tray.exe"; WorkingDir: "{app}"'])

    def procedure(self, name):
        """The body of a [Code] function or procedure, up to the next one."""
        code = self.text.split("[Code]", 1)[1]
        match = re.search(rf"(?m)^(?:function|procedure) {name}\b.*?(?=^(?:function|procedure) |\Z)", code, re.S)
        self.assertIsNotNone(match, name)
        return match[0]

    def test_stale_files_are_removed_only_after_a_complete_upgrade(self):
        code = self.text.split("[Code]", 1)[1]
        self.assertIn("{app}\\" + windows_installer.PAYLOAD_LIST.replace("/", "\\"), code)
        self.assertIn(f"CompareText(Name, '{windows_installer.UNINSTALL}') <> 0", code)
        self.assertRegex(code, r"CurStep = ssInstall then\s+Upgrade := OwnedFolder\(\);")
        self.assertRegex(code, r"\(CurStep = ssPostInstall\) and Upgrade then")
        self.assertNotIn("DelTree", code)

    def test_only_a_folder_the_installer_owns_is_cleaned_up(self):
        owned = self.procedure("OwnedFolder")
        self.assertIn(f"{{app}}\\{windows_installer.INSTALLATION}", owned)
        self.assertIn("(LinkedPath(ExpandConstant('{app}')) = '')", owned)
        # The markers it looks for are those of the record stage() writes.
        record = json.dumps(windows_installer.installation_record({"version": "0.45.0", "source_commit": "c" * 40}, "0.45.0"),
                            indent=2)
        markers = re.findall(r"Pos\('([^']+)', Contents\)", owned)
        self.assertEqual(markers, ['"kind": "installer"', '"product": "Zerus"'])
        for marker in markers:
            self.assertIn(marker, record)

    def test_links_are_never_followed(self):
        # FILE_ATTRIBUTE_REPARSE_POINT: junctions, symbolic links, mount points.
        self.assertRegex(self.text, r"(?m)^  ReparsePoint = \$400;")
        # The sweep and the scan test for a link before they treat an entry as a folder.
        for name in ("RemoveStale", "FindLink"):
            body = self.procedure(name)
            self.assertLess(body.index("(Find.Attributes and ReparsePoint) <> 0"),
                            body.index("(Find.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0"), name)
        self.assertNotRegex(self.procedure("RemoveStale").split("ReparsePoint) <> 0 then", 1)[1].split("else if", 1)[0],
                            r"DeleteFile|RemoveDir|RemoveStale")
        # Setup refuses before it changes anything when the folder is or holds a link.
        prepare = self.procedure("PrepareToInstall")
        self.assertIn("Link := LinkedPath(ExpandConstant('{app}'));", prepare)
        self.assertIn("Link := FindLink(ExpandConstant('{app}'));", prepare)
        self.assertIn("is a link or junction on the path of the Zerus folder or inside it.", prepare)
        # Every folder above {app} is checked, up to the root, and an absurd depth refuses.
        linked = self.procedure("LinkedPath")
        self.assertIn("IsLink(Current)", linked)
        self.assertIn("ExtractFileDir(Current)", linked)
        self.assertRegex(linked, r"refuse rather than guess\. }\s+Result := Path;\s+end;")

    def test_only_a_silent_self_update_relaunches_the_app(self):
        runs = re.findall(r"(?m)^Filename: (.*)$", self.text.split("[Run]", 1)[1].split("\n[", 1)[0])
        self.assertEqual(runs, ['"{app}\\hgs-tray.exe"; Parameters: "--install-login-item"; Tasks: login; Flags: runhidden',
                                '"{app}\\hgs-tray.exe"; Description: "Start Zerus"; Flags: nowait postinstall skipifsilent',
                                '"{app}\\hgs-tray.exe"; Flags: nowait; Check: Relaunch'])
        self.assertIn("CompareText(ParamStr(I), '/RELAUNCH') = 0", self.text)


class InstallerEnvironment(unittest.TestCase):
    def test_shell_profile_is_real_but_backend_home_and_roots_stay_private(self):
        from test_windows_installer_survival import installer_environment
        isolated = {'HOME': 'C:/Example/private', 'USERPROFILE': 'C:/Example/private',
                    'APPDATA': 'C:/Example/private/roaming', 'LOCALAPPDATA': 'C:/Example/private/local',
                    'HGS_CONFIG_DIR': 'C:/Example/private/config', 'HGS_STATE_DIR': 'C:/Example/private/state'}
        original = {'USERPROFILE': 'C:/Users/example', 'APPDATA': 'C:/Users/example/AppData/Roaming',
                    'LOCALAPPDATA': 'C:/Users/example/AppData/Local'}
        selected = installer_environment(isolated, original)
        self.assertEqual({key: selected[key] for key in original}, original)
        for key in ('HOME', 'HGS_CONFIG_DIR', 'HGS_STATE_DIR'):
            self.assertEqual(selected[key], isolated[key])
        self.assertEqual(isolated['USERPROFILE'], 'C:/Example/private')
        self.assertNotIn('USERPROFILE', installer_environment(isolated, {}))

    def test_setup_and_uninstall_share_the_selected_shell_environment(self):
        from types import SimpleNamespace
        from unittest.mock import Mock
        from test_windows_installer_survival import Installer
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            case = Installer()
            case.root = case.app = root
            case.setup_exe = root / 'setup.exe'
            (root / 'uninstall').mkdir()
            (root / 'uninstall' / 'unins000.exe').write_bytes(b'synthetic uninstaller, never executed')
            case.logs = 0
            case.backend_tracking = False
            case.installer_env = {'HOME': str(root), 'USERPROFILE': 'C:/Users/example'}
            case.env = {'USERPROFILE': str(root)}
            case.wait_for = Mock()
            with patch('test_windows_installer_survival.subprocess.run',
                       return_value=SimpleNamespace(returncode=0, stdout=b'', stderr=b'')) as run:
                self.assertEqual(Installer.setup(case)[0], 0)
                Installer.uninstall(case)
            self.assertEqual(run.call_count, 2)
            for call in run.call_args_list:
                self.assertIs(call.kwargs['env'], case.installer_env)


class BackendProfileRetention(unittest.TestCase):
    def test_unknown_backend_cleanup_retains_its_selected_cli_profile(self):
        from unittest.mock import Mock
        from test_windows_installer_survival import Installer
        case = object.__new__(Installer)
        case.backend_clean = False
        case.root = Path("C:/Example/private-fixture")
        case.temp = Mock()
        Installer.cleanup_temp(case)
        case.temp._finalizer.detach.assert_called_once_with()
        case.temp.cleanup.assert_not_called()

    def test_unconfirmed_ui_uninstall_keeps_profile_even_after_backend_cleanup(self):
        from unittest.mock import Mock, patch
        from test_windows_installer_survival import Installer
        case = object.__new__(Installer)
        case.backend_clean = True
        case.root = case.app = Path("C:/Example/private-fixture")
        case.temp = Mock()
        with patch("test_windows_installer_survival.registered", return_value={"InstallLocation": str(case.app)}):
            with self.assertRaisesRegex(AssertionError, "UI uninstall is not confirmed"):
                Installer.cleanup_temp(case)
        case.temp._finalizer.detach.assert_called_once_with()
        case.temp.cleanup.assert_not_called()

    def test_unreadable_uninstall_record_does_not_authorize_profile_removal(self):
        from unittest.mock import Mock, patch
        from test_windows_installer_survival import Installer
        case = object.__new__(Installer)
        case.backend_clean = True
        case.root = case.app = Path("C:/Example/private-fixture")
        case.temp = Mock()
        with patch("test_windows_installer_survival.registered", side_effect=OSError("unavailable")):
            with self.assertRaises(AssertionError):
                Installer.cleanup_temp(case)
        case.temp._finalizer.detach.assert_called_once_with()
        case.temp.cleanup.assert_not_called()

    def test_confirmed_backend_absence_allows_original_profile_cleanup(self):
        from unittest.mock import Mock
        from test_windows_installer_survival import Installer
        case = object.__new__(Installer)
        case.backend_clean = True
        case.temp = Mock()
        Installer.cleanup_temp(case)
        case.temp.cleanup.assert_called_once_with()
        case.temp._finalizer.detach.assert_not_called()


if __name__ == "__main__":
    unittest.main()
