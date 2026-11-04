"""The macOS bundle must reference only itself and the system, for Apple Silicon only."""
import ast
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


check = load("check_macos_bundle", "check-macos-bundle.py")
package = load("package_macos", "package-macos.py")
sign = load("sign_macos", "sign-macos.py")
import macos_release  # noqa: E402
import macos_notices  # noqa: E402

OTOOL = """Zerus.app/Contents/MacOS/hgs-tray:
Load command 9
      cmd LC_BUILD_VERSION
  cmdsize 32
 platform 1
    minos 13.0
      sdk 15.5
   ntools 1
Load command 12
          cmd LC_LOAD_DYLIB
      cmdsize 88
         name @rpath/QtWidgets.framework/Versions/A/QtWidgets (offset 24)
   time stamp 2 Thu Jan  1 03:00:02 1970
Load command 13
          cmd LC_LOAD_DYLIB
      cmdsize 56
         name /usr/lib/libc++.1.dylib (offset 24)
Load command 14
          cmd LC_LOAD_WEAK_DYLIB
      cmdsize 88
         name /System/Library/Frameworks/AppKit.framework/Versions/C/AppKit (offset 24)
Load command 20
          cmd LC_RPATH
      cmdsize 48
         path @executable_path/../Frameworks (offset 12)
"""

BAD = """Zerus.app/Contents/MacOS/hgs-tray:
Load command 9
      cmd LC_BUILD_VERSION
  cmdsize 32
 platform 1
    minos 15.0
      sdk 15.5
Load command 12
          cmd LC_LOAD_DYLIB
      cmdsize 88
         name /opt/homebrew/opt/qtbase/lib/QtCore.framework/Versions/A/QtCore (offset 24)
Load command 13
          cmd LC_REEXPORT_DYLIB
      cmdsize 88
         name /Users/example/qt/lib/QtGui.framework/Versions/A/QtGui (offset 24)
Load command 20
          cmd LC_RPATH
      cmdsize 48
         path /Users/example/qt/lib (offset 12)
"""


class QtNoticeChecks(unittest.TestCase):
    def documents(self, prefix, expression="Libpng AND libpng-2.0"):
        directory = prefix / "sbom"
        directory.mkdir(parents=True)
        for module in macos_notices.QT_NOTICES:
            (directory / f"{module}-6.11.2.spdx.json").write_text(json.dumps({
                "packages": [{"name": "BundledLibpng", "versionInfo": "1.6.58",
                              "licenseConcluded": expression}],
            }))

    def test_copy_includes_the_full_texts_and_preserves_spdx_and_source_texts(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix, destination = root / "qt", root / "notices"
            self.documents(prefix)
            macos_notices.copy_notices(ROOT, prefix, "6.11.2", destination)
            self.assertEqual(macos_notices.notice_findings(destination), [])
            for identifier in ("Libpng", "libpng-2.0", "LGPL-3.0-only", "GPL-3.0-only"):
                self.assertEqual((destination / f"{identifier}.txt").read_bytes(),
                                 (ROOT / "packaging/windows/licenses" / f"{identifier}.txt").read_bytes())
            self.assertEqual((destination / "qtbase.spdx.json").read_bytes(),
                             (prefix / "sbom/qtbase-6.11.2.spdx.json").read_bytes())
            self.assertEqual((destination / "LGPL-3.0.txt").read_bytes(),
                             (ROOT / "packaging/macos/licenses/LGPL-3.0.txt").read_bytes())

    def test_bundle_check_refuses_a_missing_or_empty_libpng_text(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix, bundle = root / "qt", root / "Zerus.app"
            directory = bundle / "Contents/Resources/licenses/qt"
            self.documents(prefix)
            macos_notices.copy_notices(ROOT, prefix, "6.11.2", directory)
            (bundle / "Contents/Info.plist").write_bytes(plistlib.dumps({}))
            for absent in (True, False):
                with self.subTest(absent=absent):
                    text = directory / "libpng-2.0.txt"
                    if absent:
                        text.unlink()
                    else:
                        text.write_bytes(b"")
                    findings = check.bundle_findings(bundle, "0.45.1")
                    self.assertIn("missing Qt license text: qtbase.spdx.json: libpng-2.0", findings)

    def test_an_unknown_future_identifier_stops_packaging(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.documents(root / "qt", "MIT AND Future-License-9.9")
            with self.assertRaisesRegex(ValueError, "missing Qt license text: .*Future-License-9.9"):
                macos_notices.copy_notices(ROOT, root / "qt", "6.11.2", root / "notices")

    def test_license_alternatives_exceptions_and_file_fields_are_covered(self):
        documents = {"qtbase": {"packages": [{"licenseConcluded": "(MIT OR GPL-2.0-only) WITH Linux-syscall-note",
                                             "licenseDeclared": "NOASSERTION"}],
                                "files": [{"licenseInfoInFiles": ["Libpng", "libpng-2.0"]}],
                                "snippets": [{"licenseInfoInSnippets": ["BSD-3-Clause"]}]}}
        texts = {"MIT", "GPL-2.0-only", "Linux-syscall-note", "Libpng", "libpng-2.0", "BSD-3-Clause"}
        self.assertEqual(macos_notices.missing_license_texts(documents, texts), [])
        self.assertEqual(macos_notices.missing_license_texts(documents, texts - {"Linux-syscall-note"}),
                         ["qtbase: Linux-syscall-note"])

    def test_custom_licenses_need_their_nonempty_local_extracted_text(self):
        document = {"packages": [{"licenseConcluded": "LicenseRef-Example"}],
                    "hasExtractedLicensingInfos": [{"licenseId": "LicenseRef-Example", "extractedText": "full text"}]}
        self.assertEqual(macos_notices.missing_license_texts({"qtbase": document}, set()), [])
        for definitions in ([], [{"licenseId": "LicenseRef-Example", "extractedText": " \n"}]):
            with self.subTest(definitions=definitions):
                self.assertEqual(macos_notices.missing_license_texts(
                    {"qtbase": {**document, "hasExtractedLicensingInfos": definitions}}, {"LicenseRef-Example"}),
                    ["qtbase: LicenseRef-Example"])

    def test_external_custom_text_must_belong_to_the_named_document(self):
        documents = {"qtbase": {"packages": [{"licenseConcluded": "DocumentRef-svg:LicenseRef-Example"}],
                                "externalDocumentRefs": [{"externalDocumentId": "DocumentRef-svg", "spdxDocument": "urn:svg"}]},
                     "qtsvg": {"packages": [], "documentNamespace": "urn:svg", "hasExtractedLicensingInfos": [
                         {"licenseId": "LicenseRef-Example", "extractedText": "full text"}]}}
        self.assertEqual(macos_notices.missing_license_texts(documents, set()), [])
        documents["qtbase"]["externalDocumentRefs"][0]["spdxDocument"] = "urn:not-shipped"
        self.assertEqual(macos_notices.missing_license_texts(documents, set()),
                         ["qtbase: DocumentRef-svg:LicenseRef-Example"])

    def test_malformed_or_absent_metadata_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            self.assertEqual(len(macos_notices.notice_findings(directory)), 3)
            for value in ("{", "[]", '{"packages": [false]}',
                          '{"packages": [{"licenseConcluded": false}]}'):
                with self.subTest(value=value):
                    for module in macos_notices.QT_NOTICES:
                        (directory / f"{module}.spdx.json").write_text(value)
                    self.assertTrue(macos_notices.notice_findings(directory))


class FFmpegSourceChecks(unittest.TestCase):
    def libraries(self, bundle):
        info = macos_notices.ffmpeg_provenance()
        directory = bundle / "Contents/Frameworks"
        directory.mkdir(parents=True)
        for name, offset in info["arm64_configuration_offsets"].items():
            with (directory / name).open("wb") as stream:
                stream.seek(offset)
                stream.write(info["configuration"].encode() + b"\0")
        version = info["arm64_version_info"]
        with (directory / version["library"]).open("r+b") as stream:
            stream.seek(version["code_offset"])
            stream.write(bytes.fromhex(version["code_hex"]))
            stream.seek(version["value_offset"])
            stream.write(info["ffmpeg_version"].encode() + b"\0")
        return info

    def test_every_library_configuration_and_version_return_are_bound(self):
        with tempfile.TemporaryDirectory() as temporary:
            bundle = Path(temporary)
            info = self.libraries(bundle)
            self.assertEqual(macos_notices.ffmpeg_findings(bundle, info), [])
            probes = [(name, offset) for name, offset in info["arm64_configuration_offsets"].items()]
            probes += [(info["arm64_version_info"]["library"], info["arm64_version_info"][key])
                       for key in ("code_offset", "value_offset")]
            for name, offset in probes:
                with self.subTest(library=name, offset=offset):
                    path = bundle / "Contents/Frameworks" / name
                    with path.open("r+b") as stream:
                        stream.seek(offset)
                        original = stream.read(1)
                        stream.seek(offset)
                        stream.write(bytes([original[0] ^ 1]))
                    self.assertEqual(macos_notices.ffmpeg_findings(bundle, info),
                                     [f"{name}: reviewed FFmpeg version/configuration differs at {offset}"])
                    with path.open("r+b") as stream:
                        stream.seek(offset)
                        stream.write(original)
            (bundle / "Contents/Frameworks/libavutil.59.dylib").unlink()
            self.assertTrue(macos_notices.ffmpeg_findings(bundle, info))

    def test_qt_version_or_archive_change_requires_a_new_source_review(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = root / "packaging/macos"
            directory.mkdir(parents=True)
            (directory / "ffmpeg.json").write_bytes((ROOT / "packaging/macos/ffmpeg.json").read_bytes())
            original = json.loads((ROOT / "packaging/macos/qt.json").read_text())
            for changed in ({**original, "qt_version": "6.99.0"}, {**original, "archives": []}):
                with self.subTest(qt=changed):
                    (directory / "qt.json").write_text(json.dumps(changed))
                    with self.assertRaisesRegex(ValueError, "Review the FFmpeg source tuple"):
                        macos_notices.ffmpeg_provenance(root)

    def test_packaging_emits_the_supplement_and_refuses_changed_bytes_before_signing(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            app, prefix, licenses = root / "input.app", root / "qt", root / "licenses"
            (app / "Contents").mkdir(parents=True)
            prefix.mkdir()
            licenses.mkdir()
            version = (ROOT / "VERSION").read_text().strip()
            (app / "Contents/Info.plist").write_bytes(plistlib.dumps({
                "CFBundleIdentifier": "com.hgdev.hgs-tray.local", "CFBundleShortVersionString": version}))
            (prefix / "zerus-qt.json").write_bytes((ROOT / "packaging/macos/qt.json").read_bytes())
            QtNoticeChecks().documents(prefix, "MIT")
            (licenses / "inventory.json").write_text("{}")
            cli = root / "hgs"
            cli.write_bytes(b"stand-in; never executed")
            for altered in (False, True):
                output = root / f"output-{altered}"
                signed = []
                def execute(*args):
                    if args[0] == "ditto":
                        shutil.copytree(args[1], args[2])
                    elif args[0] == prefix / "bin/macdeployqt":
                        self.libraries(args[1])
                        if altered:
                            library = args[1] / "Contents/Frameworks/libavutil.59.dylib"
                            with library.open("r+b") as stream:
                                stream.seek(496676)
                                stream.write(b"7.1.3\0")
                    else:
                        self.assertEqual(args[0], "codesign")
                        signed.append(args[-1])
                argv = ["package-macos.py", "--qt", str(prefix), "--app", str(app),
                        "--cli", str(cli), "--connector", str(cli), "--licenses", str(licenses),
                        "--version", version, "--output", str(output)]
                with self.subTest(altered=altered), mock.patch.object(sys, "argv", argv), \
                        mock.patch.object(package, "run", side_effect=execute):
                    if altered:
                        with self.assertRaisesRegex(ValueError, "FFmpeg version/configuration differs"):
                            package.main()
                        self.assertEqual(signed, [])
                    else:
                        package.main()
                        self.assertEqual(signed, [output / "Zerus.app"])
                        notices = output / "Zerus.app/Contents/Resources/licenses/qt"
                        self.assertEqual((notices / "FFMPEG-SOURCE.json").read_bytes(),
                                         (ROOT / "packaging/macos/ffmpeg.json").read_bytes())
                        note = (notices / "SOURCE.md").read_text()
                        self.assertIn("FFmpeg 7.1.5", note)
                        self.assertIn("lists 7.1.3", note)
                        self.assertIn(macos_notices.ffmpeg_provenance()["source_sha256"], note)


class BundleChecks(unittest.TestCase):
    def test_packaging_promotes_only_its_private_copy_to_the_public_identity(self):
        with tempfile.TemporaryDirectory(prefix="zerus-package-identity-") as temporary:
            root = Path(temporary)
            qt, licenses = root / "qt", root / "licenses"
            qt.mkdir()
            licenses.mkdir()
            (qt / "zerus-qt.json").write_bytes((ROOT / "packaging/macos/qt.json").read_bytes())
            (licenses / "inventory.json").write_text("{}")
            version = (ROOT / "VERSION").read_text().strip()
            for index, identity in enumerate(("com.hgdev.hgs-tray.local", "com.hgdev.hgs-tray", "example.foreign")):
                with self.subTest(identity=identity):
                    app, output = root / f"input-{index}.app", root / f"output-{index}"
                    info = app / "Contents/Info.plist"
                    info.parent.mkdir(parents=True)
                    original = plistlib.dumps({"CFBundleIdentifier": identity, "CFBundleShortVersionString": version})
                    info.write_bytes(original)
                    deployed = []
                    def execute(*args):
                        if args[0] == "ditto":
                            shutil.copytree(args[1], args[2])
                        else:
                            self.assertEqual(args[0], qt / "bin/macdeployqt")
                            deployed.append(args[1])
                            raise RuntimeError("metadata checkpoint")
                    arguments = ["package-macos.py", "--qt", str(qt), "--app", str(app),
                                 "--cli", str(root / "hgs"), "--connector", str(root / "connector"),
                                 "--licenses", str(licenses), "--version", version, "--output", str(output)]
                    with mock.patch.object(sys, "argv", arguments), mock.patch.object(package, "run", side_effect=execute):
                        if identity == "example.foreign":
                            with self.assertRaises(SystemExit) as error:
                                package.main()
                            self.assertEqual(error.exception.code, 2)
                            self.assertEqual(deployed, [])
                        else:
                            with self.assertRaisesRegex(RuntimeError, "metadata checkpoint"):
                                package.main()
                            self.assertEqual(deployed, [output / "work/hgs-tray.app"])
                            staged = plistlib.loads((deployed[0] / "Contents/Info.plist").read_bytes())
                            self.assertEqual(staged["CFBundleIdentifier"], "com.hgdev.hgs-tray")
                    self.assertEqual(info.read_bytes(), original, "packaging never changes the source bundle")

    def test_exhausted_ticket_validation_is_a_bundle_failure(self):
        responses = [subprocess.CompletedProcess([], 0, "", ""),
                     subprocess.CompletedProcess([], 0, "", "TeamIdentifier=ABCDE12345\nflags=0x10000(runtime)"),
                     subprocess.CompletedProcess([], 0, "", "source=Notarized Developer ID")]
        with mock.patch.object(check.subprocess, "run", side_effect=responses), \
                mock.patch.object(check, "run_stapler", return_value=subprocess.CompletedProcess(
                    [], 68, "NSURLErrorDomain Code=-1005", "")) as validate:
            findings = check.signature_findings(Path("/fixture/Zerus.app"), "ABCDE12345")
        validate.assert_called_once_with("validate", Path("/fixture/Zerus.app"))
        self.assertEqual(len(findings), 1)
        self.assertIn("notarization ticket is not stapled", findings[0])

    @unittest.skipUnless(Path("/bin/bash").exists(), "requires Bash")
    def test_unattended_cask_execution_requires_prior_installed_gatekeeper_acceptance(self):
        script = (ROOT / "scripts/ci/macos-cask-install.sh").read_text()
        segment = script.split("app=/Applications/Zerus.app\n", 1)[1].split('\nfixture_home="', 1)[0]
        with tempfile.TemporaryDirectory(prefix="zerus-cask-gate-") as temporary:
            directory = Path(temporary)
            log = directory / "calls.jsonl"
            for name in ("python3", "xattr"):
                stub = directory / name
                stub.write_text(f"#!{sys.executable}\n" + '''import json, os, pathlib, sys
with open(os.environ["ZERUS_CASK_TEST_LOG"], "a") as log:
    log.write(json.dumps([pathlib.Path(sys.argv[0]).name, *sys.argv[1:]]) + "\\n")
if pathlib.Path(sys.argv[0]).name == "python3" and os.environ["ZERUS_CASK_TEST_REJECT"] == "1":
    sys.exit(7)
''')
                stub.chmod(0o755)
            for rejected in (False, True):
                with self.subTest(rejected=rejected):
                    log.write_text("")
                    env = dict(os.environ, PATH=f"{directory}:/usr/bin:/bin",
                               ZERUS_CASK_TEST_LOG=str(log), ZERUS_CASK_TEST_REJECT=str(int(rejected)))
                    command = 'set -euo pipefail\napp="$1"\nversion=0.42.2\nrelease=0.42.2\nteam=ABCDE12345\n' + segment
                    result = subprocess.run(["/bin/bash", "-c", command, "cask-gate", str(directory / "Zerus.app")],
                                            env=env, capture_output=True, text=True)
                    calls = [json.loads(line) for line in log.read_text().splitlines()]
                    self.assertEqual(result.returncode, 7 if rejected else 0, result.stderr)
                    self.assertEqual([call[0] for call in calls], ["python3"] if rejected else
                                     ["python3", "xattr", "python3"])
                    self.assertIn("--team-id", calls[0])
                    self.assertNotIn("--selftest", calls[0])
                    if not rejected:
                        self.assertEqual(calls[1], ["xattr", "-dr", "com.apple.quarantine", str(directory / "Zerus.app")])
                        self.assertIn("--selftest", calls[2])

    @unittest.skipUnless(Path("/bin/bash").exists(), "requires Bash")
    def test_cask_metadata_is_validated_by_the_system_bash_before_installation(self):
        script = ROOT / "scripts/ci/macos-cask-install.sh"
        # Execute the real pre-installation section, including its quoted
        # Python heredoc, with Apple's Bash 3.2 on macOS and Bash on Linux.
        preflight = script.read_text().split('\nasset="$signed/', 1)[0]
        preflight += '\nprintf "%s\\n" "$cask" "$release" "$team"\n'
        with tempfile.TemporaryDirectory(prefix="zerus-cask-metadata-") as temporary:
            directory = Path(temporary)
            metadata = directory / "macos-info.json"
            for channel, release, cask in (("stable", "0.42.2", "zerus"),
                    ("nightly", "0.42.2.r812.gabcdef0.n41", "zerus@nightly"),
                    ("dev", "0.42.3-dev.50", "zerus@dev")):
                info = {"asset": f"zerus-{release}-macos-arm64.zip", "qt_version": "6.11.2",
                        "team_id": "ABCDE12345", "minimum_system_version": "14.0",
                        "notarization_id": "0123abcd-4567-89ef-0123-456789abcdef",
                        "dmg": {"asset": f"zerus-{release}-macos-arm64.dmg",
                                "notarization_id": "1123abcd-4567-89ef-0123-456789abcdef"}}
                metadata.write_text(json.dumps(info))
                command = ["/bin/bash", "-c", preflight, str(script), str(directory), channel]
                with self.subTest(channel=channel):
                    result = subprocess.run(command, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout.splitlines(), [cask, release, "ABCDE12345"])
                for invalid in ({**info, "team_id": "invalid"},
                                {key: value for key, value in info.items() if key != "dmg"}):
                    metadata.write_text(json.dumps(invalid))
                    result = subprocess.run(command, capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "", "invalid metadata must stop before installation")

    def test_selftest_exposes_tmux_with_an_isolated_home_and_socket_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            tmux = root / "tmux"
            tmux.write_text("#!/bin/sh\nprintf 'tmux fixture\\n'\n")
            tmux.chmod(0o755)
            bundle = root / "Zerus.app"
            binaries = bundle / "Contents/MacOS"
            binaries.mkdir(parents=True)
            for name in ("hgs", "hgs-tray", "zerus-mobile-connector"):
                executable = binaries / name
                executable.write_text(f"#!{sys.executable}\n" + '''import os, pathlib, subprocess
assert os.environ["HOME"] != "/nonexistent/parent-home"
assert pathlib.Path(os.environ["HOME"]).is_dir()
assert os.environ["TMUX_TMPDIR"] == os.environ["HOME"]
assert "TMUX" not in os.environ and "HGS_SESSION" not in os.environ
assert "/nonexistent/parent-bin" not in os.environ["PATH"]
assert subprocess.check_output(["tmux", "-V"], text=True).strip() == "tmux fixture"
''')
                executable.chmod(0o755)
            with mock.patch.object(check.shutil, "which", return_value=str(tmux)), \
                    mock.patch.dict(os.environ, {"HOME": "/nonexistent/parent-home", "TMUX": "parent-socket",
                                                 "HGS_SESSION": "parent-session", "PATH": "/nonexistent/parent-bin"}):
                self.assertEqual(check.run_selftest(bundle), [])

    def test_selftest_reports_missing_tmux_before_executing_the_bundle(self):
        with mock.patch.object(check.shutil, "which", return_value=None):
            self.assertEqual(check.run_selftest(Path("/nonexistent/Zerus.app")),
                             ["bundle self-tests require tmux (brew install tmux)"])

    def test_bundled_gui_identity_must_match_dev_sequence_and_clean_source(self):
        info = {'version':'0.42.2','versionName':'0.42.3-dev.50','devBuildNumber':50,
                'sourceCommit':'a'*40,'sourceDirty':False}
        self.assertEqual(check.build_identity_findings(info,'0.42.2','0.42.3-dev.50','a'*40),[])
        for key, value in [('versionName','0.42.2'),('devBuildNumber',0),('sourceCommit','b'*40),('sourceDirty',True)]:
            with self.subTest(key=key):
                self.assertTrue(check.build_identity_findings({**info,key:value},'0.42.2','0.42.3-dev.50','a'*40))
        stable={**info,'versionName':'0.42.2','devBuildNumber':0}
        self.assertEqual(check.build_identity_findings(stable,'0.42.2','0.42.2','a'*40),[])
        self.assertEqual(check.build_identity_findings(stable,'0.42.2','0.42.2.r812.gabcdef0.n41','a'*40),[])

    def test_load_commands_are_parsed(self):
        self.assertEqual(check.load_commands(OTOOL), [
            ("LC_BUILD_VERSION", "13.0"),
            ("LC_LOAD_DYLIB", "@rpath/QtWidgets.framework/Versions/A/QtWidgets"),
            ("LC_LOAD_DYLIB", "/usr/lib/libc++.1.dylib"),
            ("LC_LOAD_WEAK_DYLIB", "/System/Library/Frameworks/AppKit.framework/Versions/C/AppKit"),
            ("LC_RPATH", "@executable_path/../Frameworks"),
        ])

    def test_bundle_and_system_references_are_allowed(self):
        self.assertEqual(check.reference_findings("hgs-tray", check.load_commands(OTOOL), "13.0"), [])

    def test_foreign_references_and_newer_minimum_are_reported(self):
        findings = check.reference_findings("hgs-tray", check.load_commands(BAD), "13.0")
        self.assertEqual(len(findings), 4, findings)
        joined = "\n".join(findings)
        for expected in ("/opt/homebrew", "/Users/example/qt/lib/QtGui", "rpath /Users/example/qt/lib", "15.0"):
            self.assertIn(expected, joined)

    def test_relative_references_must_resolve_inside_the_bundle(self):
        with tempfile.TemporaryDirectory() as temporary:
            bundle = Path(temporary) / "Zerus.app"
            main = bundle / "Contents/MacOS/hgs-tray"
            framework = bundle / "Contents/Frameworks/QtCore.framework/Versions/A/QtCore"
            helper = bundle / "Contents/Frameworks/QtHelpers.framework/Helpers/Helper.app/Contents/MacOS/Helper"
            for path in (main, framework, helper):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"")
            commands = [("LC_RPATH", "@executable_path/../Frameworks"),
                        ("LC_LOAD_DYLIB", "@rpath/QtCore.framework/Versions/A/QtCore"),
                        ("LC_LOAD_DYLIB", "/usr/lib/libc++.1.dylib")]
            self.assertEqual(check.unresolved_references(bundle, main, commands), [])
            nested = [("LC_RPATH", "@executable_path/../../../../../../Frameworks"),
                      ("LC_LOAD_DYLIB", "@rpath/QtCore.framework/Versions/A/QtCore")]
            self.assertEqual(check.unresolved_references(bundle, helper, nested), [])
            missing = commands + [("LC_LOAD_DYLIB", "@rpath/QtMultimedia.framework/Versions/A/QtMultimedia"),
                                  ("LC_LOAD_DYLIB", "@loader_path/libmissing.dylib")]
            findings = check.unresolved_references(bundle, main, missing)
            self.assertEqual(len(findings), 2, findings)
            self.assertIn("QtMultimedia", findings[0])

    def test_plist_identity_and_minimum_system_are_required(self):
        good = {"CFBundleIdentifier": "com.hgdev.hgs-tray", "CFBundleExecutable": "hgs-tray",
                "CFBundleName": "Zerus", "CFBundleDisplayName": "Zerus", "CFBundleShortVersionString": "0.38.0",
                "CFBundleVersion": "0.38.0", "LSMinimumSystemVersion": "14.0",
                "NSAppleEventsUsageDescription": "Zerus opens your agent sessions in Terminal.",
                "ZerusReleaseVersion": "0.38.0"}
        self.assertEqual(check.plist_findings(good, "0.38.0"), [])
        missing = dict(good)
        del missing["NSAppleEventsUsageDescription"]
        self.assertTrue(check.plist_findings(missing, "0.38.0"), "hardened runtime needs the Apple Events purpose")
        for key, value in (("CFBundleIdentifier", "dev.zerus.app"), ("CFBundleIdentifier", "com.hgdev.hgs-tray.local"),
                           ("CFBundleShortVersionString", "0.37.0"),
                           ("LSMinimumSystemVersion", "15.0"), ("CFBundleName", "hgs zerus")):
            self.assertTrue(check.plist_findings(dict(good, **{key: value}), "0.38.0"), key)

    def test_plist_records_the_channel_release_version(self):
        good = {"CFBundleIdentifier": "com.hgdev.hgs-tray", "CFBundleExecutable": "hgs-tray",
                "CFBundleName": "Zerus", "CFBundleDisplayName": "Zerus", "CFBundleShortVersionString": "0.38.0",
                "CFBundleVersion": "0.38.0", "LSMinimumSystemVersion": "14.0",
                "NSAppleEventsUsageDescription": "Zerus opens your agent sessions in Terminal."}
        nightly = "0.38.0.r812.gabcdef0.n41"
        self.assertTrue(check.plist_findings(good, "0.38.0"), "the release version key is required")
        self.assertEqual(check.plist_findings(dict(good, ZerusReleaseVersion="0.38.0"), "0.38.0"), [])
        self.assertEqual(check.plist_findings(dict(good, ZerusReleaseVersion=nightly), "0.38.0", nightly), [])
        self.assertTrue(check.plist_findings(dict(good, ZerusReleaseVersion="0.38.0"), "0.38.0", nightly))

    def test_required_files_include_setup_and_notices(self):
        names = set(check.REQUIRED_FILES)
        for name in ("Contents/MacOS/hgs", "Contents/MacOS/zerus-mobile-connector", "Contents/Resources/bin/zerus-setup",
                     "Contents/Resources/setup/install-macos-session-service.py",
                     "Contents/Resources/setup/install-mobile-connector-service.py",
                     "Contents/Resources/licenses/qt/LGPL-3.0.txt",
                     "Contents/Resources/licenses/qt/qtmultimedia.spdx.json",
                     "Contents/Resources/licenses/qt/SOURCE.md",
                     "Contents/Resources/licenses/rust/inventory.json",
                     "Contents/Resources/licenses/codex-LICENSE.txt"):
            self.assertIn(name, names)

    def test_a_connector_built_with_test_hooks_is_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            bundle = Path(temporary) / "Zerus.app"
            (bundle / "Contents/MacOS").mkdir(parents=True)
            (bundle / "Contents/Info.plist").write_bytes(plistlib.dumps({}))
            connector = bundle / check.CONNECTOR
            connector.write_bytes(b"ZERUS_CONNECTOR_TEST_CLOCK_SCALE")
            self.assertIn(f"{check.CONNECTOR} was built with test hooks", check.bundle_findings(bundle, "0.38.0"))
            connector.write_bytes(b"release build")
            self.assertNotIn(f"{check.CONNECTOR} was built with test hooks", check.bundle_findings(bundle, "0.38.0"))

    def test_qt_webengine_never_ships_in_the_bundle(self):
        self.assertFalse([name for name in check.REQUIRED_FILES if "webengine" in name.lower()])
        with tempfile.TemporaryDirectory() as temporary:
            bundle = Path(temporary) / "Zerus.app"
            (bundle / "Contents/Frameworks/QtWebEngineCore.framework").mkdir(parents=True)
            (bundle / "Contents/Info.plist").write_bytes(plistlib.dumps({}))
            findings = check.bundle_findings(bundle, "0.38.0")
            self.assertIn("Contents/Frameworks/QtWebEngineCore.framework: Qt WebEngine is not part of the macOS bundle",
                          findings)

    def test_setup_scripts_ship_with_their_local_imports(self):
        local = {path.stem for path in (ROOT / "scripts").glob("*.py")}
        for name in package.SETUP_SCRIPTS:
            self.assertIn(f"Contents/Resources/setup/{name}", check.REQUIRED_FILES)
            tree = ast.parse((ROOT / "scripts" / name).read_text(encoding="utf-8"))
            for node in ast.walk(tree):
                modules = [alias.name for alias in node.names] if isinstance(node, ast.Import) else \
                    [node.module] if isinstance(node, ast.ImportFrom) and node.module else []
                for module in modules:
                    if module in local:
                        self.assertIn(f"{module}.py", package.SETUP_SCRIPTS, f"{name} imports {module}")

    def test_minimum_system_matches_the_desktop_build(self):
        cmake = (ROOT / "tray/CMakeLists.txt").read_text(encoding="utf-8")
        floor = re.search(r'CMAKE_OSX_DEPLOYMENT_TARGET VERSION_LESS "([0-9.]+)"', cmake).group(1)
        for module in (package, check, sign, macos_release):
            self.assertEqual(module.MINIMUM_SYSTEM, floor, module.__name__)
        names = {"13.0": ":ventura", "14.0": ":sonoma", "15.0": ":sequoia"}
        for template in sorted((ROOT / "packaging/homebrew").glob("*.rb.in")):
            self.assertIn(f'depends_on macos: {names[floor]}', template.read_text(encoding="utf-8"),
                          template.name)


if __name__ == "__main__":
    unittest.main()
