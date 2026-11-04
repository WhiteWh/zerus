"""Windows portable zip contracts: packaging, layout checks, the pinned ConPTY
pair and size limits.

Packaging runs with a synthetic Tauri executable, MSVC runtime and
ConPTY pair, so these checks need no Windows tools or network and never execute
bundled programs. The synthetic pair passes as the pinned build because the
cases replace the pinned digests (`pinned`); `ConptyPinTests` checks the real pin.
"""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import conpty_pair
import windows_portable as portable

COMMIT = "a" * 40
VERSION = (ROOT / "VERSION").read_text().strip()
CLI_VERSION = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
QT_VERSION = "6.10.3"
# What the fake windeployqt deploys next to hgs-tray.exe.
QT_RUNTIME = [name for name in portable.REQUIRED if name.startswith(("Qt", "platforms/", "imageformats/",
                                                                   "iconengines/", "resources/", "multimedia/",
                                                                   "avcodec-", "avformat-", "avutil-", "swresample-", "swscale-"))]
# Stand-ins for the ConPTY pair.
PAIR = {"conpty.dll": b"synthetic conpty.dll", "OpenConsole.exe": b"synthetic OpenConsole.exe"}
# Runs a script as `python SCRIPT ARGS...` would, after replacing the pinned pair
# with the pin in argv[1]: `python -c WITH_PIN PIN SCRIPT ARGS...`.
WITH_PIN = "\n".join([
    "import json, runpy, sys",
    f"sys.path.insert(0, {str(ROOT / 'scripts')!r})",
    "import conpty_pair",
    "conpty_pair.PIN = conpty_pair.validate(json.loads(sys.argv[1]))",
    "sys.argv = sys.argv[2:]",
    "runpy.run_path(sys.argv[0], run_name='__main__')",
])


def pinned(files=PAIR, **package):
    """The repository's ConPTY pin with these bytes as its pair and these package fields."""
    pin = copy.deepcopy(conpty_pair.load())
    pin.update(package)
    for name, data in files.items():
        pin["files"][name].update(size=len(data), sha256=hashlib.sha256(data).hexdigest())
    return conpty_pair.validate(pin)


def use_pin(case, pin):
    """Replace the pinned pair for the rest of `case`."""
    patcher = patch.object(conpty_pair, "PIN", pin)
    patcher.start()
    case.addCleanup(patcher.stop)


def complete(**changes):
    """Every required file: the synthetic pair and placeholders for the rest."""
    return {**{name: PAIR.get(name, b"x") for name in portable.REQUIRED}, **changes}


def manifest(**updates):
    value = {"product": "Zerus", "version": VERSION, "cli_version": CLI_VERSION, "platform": "Windows",
             "architecture": "x86_64", "qt_version": QT_VERSION, "native_agents_bundled": False,
             "source_commit": COMMIT}
    value.update(updates)
    return value


def bundle(path, files=None, info=None, entries=()):
    """A minimal zip with every required file; entries adds raw ZipInfo/data pairs."""
    files = complete() if files is None else files
    files = {**files, "build-info.json": json.dumps(manifest() if info is None else info).encode()}
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, data in sorted(files.items()):
            archive.writestr(f"{portable.TOP}/{name}", data)
        for entry, data in entries:
            archive.writestr(entry, data)
    return path


class InspectTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zerus-windows-contract-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        use_pin(self, pinned())

    def test_complete_bundle_returns_its_manifest(self):
        self.assertEqual(portable.inspect(bundle(self.directory / "ok.zip")), manifest())

    def test_unsafe_names_and_entry_types_are_rejected(self):
        def raw(name, mode=stat.S_IFREG | 0o644):
            # Set the name after construction: ZipInfo would rewrite a Windows separator.
            entry = zipfile.ZipInfo("placeholder")
            entry.filename = name
            entry.create_system = 3
            entry.external_attr = mode << 16
            return entry
        unsafe = [raw(f"{portable.TOP}/../outside"), raw("/absolute"), raw(f"{portable.TOP}\\windows"),
                  raw(f"{portable.TOP}/C:drive"), raw(f"{portable.TOP}/stream:data"), raw("other/file"),
                  raw(f"{portable.TOP}/nul.txt"), raw(f"{portable.TOP}/trailing."), raw(f"{portable.TOP}//empty"),
                  raw(f"{portable.TOP}/nul-byte\x00suffix"),
                  raw(f"{portable.TOP}/HGS.EXE"), raw(f"{portable.TOP}/folder/", stat.S_IFDIR | 0o755),
                  raw(f"{portable.TOP}/link", stat.S_IFLNK | 0o777)]
        for index, entry in enumerate(unsafe):
            with self.subTest(entry=entry.filename), self.assertRaises(ValueError):
                portable.inspect(bundle(self.directory / f"unsafe-{index}.zip", entries=[(entry, b"x")]))

    def test_missing_runtime_or_notices_are_rejected(self):
        for name in ("hgs.exe", "Qt6Multimedia.dll", "multimedia/ffmpegmediaplugin.dll", "imageformats/qsvg.dll",
                     "vcruntime140_1.dll", "conpty.dll", "OpenConsole.exe", "licenses/qt/LGPL-3.0-only.txt",
                     "licenses/qt/IJG.txt", "licenses/conpty-LICENSE.txt", "licenses/conpty-NOTICE.md",
                     "licenses/rust/inventory.json"):
            files = {item: data for item, data in complete().items() if item != name}
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "lacks: " + name):
                portable.inspect(bundle(self.directory / "missing.zip", files=files))

    def test_the_conpty_pair_must_be_the_pinned_build_next_to_hgs(self):
        for name, data in PAIR.items():
            # Other bytes, of another size or of the same size.
            for other in (b"other " + data, data[:-1] + b"?"):
                with self.subTest(name=name, data=other), \
                        self.assertRaisesRegex(ValueError, f"{name} is not the pinned "
                                                           f"Microsoft.Windows.Console.ConPTY {conpty_pair.PIN['version']}"):
                    portable.inspect(bundle(self.directory / "other.zip", files=complete(**{name: other})))
        # A real build differs from the stand-ins: the repository pin rejects them.
        with patch.object(conpty_pair, "PIN", conpty_pair.load()), \
                self.assertRaisesRegex(ValueError, "conpty.dll is not the pinned"):
            portable.inspect(bundle(self.directory / "unpinned.zip"))
        # conpty.dll would run an OpenConsole.exe from x64\ when none sat next to it,
        # as the NuGet package's own build targets place it; no copy goes elsewhere.
        for name in ("x64/OpenConsole.exe", "arm64/openconsole.exe", "runtimes/conpty.dll", "licenses/CONPTY.DLL"):
            files = complete(**{name: PAIR.get(Path(name).name, b"x")})
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "belong next to hgs.exe"):
                portable.inspect(bundle(self.directory / "elsewhere.zip", files=files))
        # The license text is no ConPTY binary.
        self.assertEqual(portable.inspect(bundle(self.directory / "ok.zip")), manifest())

    def test_system_libraries_and_software_opengl_are_rejected(self):
        # windeployqt copies System32's ICU forwarder, Mesa's opengl32sw.dll and
        # Microsoft's Direct3D shader compilers by default.
        # Qt WebEngine (Chromium) is never part of the Windows bundle.
        for name in ("icuuc.dll", "ICUIN.DLL", "icu.dll", "opengl32sw.dll", "plugins/icudt72.dll",
                     "D3Dcompiler_47.dll", "d3dcompiler_46.dll", "dxcompiler.dll", "DXIL.dll",
                     "Qt6WebEngineCore.dll", "Qt6WebEngineWidgets.dll", "QtWebEngineProcess.exe",
                     "resources/icudtl.dat", "resources/qtwebengine_resources.pak", "resources/v8_context_snapshot.bin"):
            files = complete(**{name: b"x"})
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "must not contain"):
                portable.inspect(bundle(self.directory / "forbidden.zip", files=files))
        self.assertFalse([name for name in portable.REQUIRED if "webengine" in name.lower()])

    def test_manifest_must_name_the_exact_windows_build(self):
        for updates in ({"platform": "Arch Linux"}, {"architecture": "arm64"}, {"native_agents_bundled": True},
                        {"source_commit": "a" * 7}, {"version": "0.39"}, {"qt_version": None}, {"product": "Other"}):
            with self.subTest(updates=updates), self.assertRaises(ValueError):
                portable.inspect(bundle(self.directory / "manifest.zip", info=manifest(**updates)))
        with self.assertRaisesRegex(ValueError, "not an object"):
            portable.inspect(bundle(self.directory / "list.zip", info=[]))
        path = bundle(self.directory / "large.zip", info={**manifest(), "padding": "x" * portable.MANIFEST_LIMIT})
        with self.assertRaisesRegex(ValueError, "too large"):
            portable.inspect(path)

    def test_size_and_entry_limits(self):
        path = bundle(self.directory / "limits.zip")
        with patch.object(portable, "ZIP_LIMIT", path.stat().st_size - 1), \
                self.assertRaisesRegex(ValueError, "release asset limit"):
            portable.inspect(path)
        with patch.object(portable, "UNPACKED_LIMIT", 10), self.assertRaisesRegex(ValueError, "unpacks beyond"):
            portable.inspect(path)
        with patch.object(portable, "ENTRY_LIMIT", 3), self.assertRaisesRegex(ValueError, "entry count"):
            portable.inspect(path)
        (self.directory / "broken.zip").write_bytes(b"not a zip")
        with self.assertRaisesRegex(ValueError, "not a valid zip"):
            portable.inspect(self.directory / "broken.zip")

    def test_every_shipped_license_text_is_required(self):
        texts = sorted(path.stem for path in (ROOT / "packaging/windows/licenses").glob("*.txt"))
        self.assertEqual(texts, sorted(portable.QT_LICENSE_TEXTS))
        for name in texts:
            self.assertGreater((ROOT / "packaging/windows/licenses" / f"{name}.txt").stat().st_size, 200, name)

    def test_limits_match_the_release_mirror_and_candidate_bounds(self):
        import update_feed
        self.assertEqual(portable.ZIP_LIMIT, update_feed.ASSET_LIMIT)
        self.assertEqual(portable.UNPACKED_LIMIT, 500_000_000)


class MobileConnectorBundleTests(unittest.TestCase):
    def test_new_declaration_requires_connector_but_retained_zips_stay_valid(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(conpty_pair, "PIN", pinned()):
            path=Path(temporary)/"bundle.zip"
            bundle(path)
            portable.inspect(path)
            bundle(path, info=manifest(mobile_connector_bundled=True))
            with self.assertRaisesRegex(ValueError, "mobile connector declaration"):
                portable.inspect(path)
            bundle(path, files=complete(**{"zerus-mobile-connector.exe":b"tested connector"}),
                   info=manifest(mobile_connector_bundled=True))
            self.assertTrue(portable.inspect(path)["mobile_connector_bundled"])


class ConptyPinTests(unittest.TestCase):
    """packaging/windows/conpty.json: the reviewed package and its x64 pair."""

    def test_the_pin_names_the_reviewed_package_and_its_x64_pair(self):
        pin = conpty_pair.load()
        self.assertEqual(pin, conpty_pair.PIN)
        self.assertEqual((pin["package"], pin["version"], pin["license"]),
                         ("Microsoft.Windows.Console.ConPTY", "1.25.260930003", "MIT"))
        self.assertEqual(pin["url"], "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/"
                                     "1.25.260930003/microsoft.windows.console.conpty.1.25.260930003.nupkg")
        self.assertEqual((pin["size"], pin["sha256"]),
                         (1763391, "02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee"))
        self.assertEqual(pin["files"], {
            "conpty.dll": {"path": "runtimes/win-x64/native/conpty.dll", "size": 119608,
                           "sha256": "feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a"},
            "OpenConsole.exe": {"path": "build/native/runtimes/x64/OpenConsole.exe", "size": 1089376,
                                "sha256": "3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586"},
        })

    def test_the_pair_is_what_the_host_stages_and_loads(self):
        self.assertEqual(sorted(conpty_pair.PIN["files"]), sorted(conpty_pair.NAMES))
        staging = (ROOT / "src/host/staging.rs").read_text()
        self.assertIn('pub const PAIR: [&str; 2] = ["conpty.dll", "OpenConsole.exe"];', staging)
        source = (ROOT / "src/host/conpty.rs").read_text()
        self.assertIn('const BUNDLED_DLL: &str = "conpty.dll";', source)
        self.assertIn('const BUNDLED_SERVER: &str = "OpenConsole.exe";', source)
        # conpty.rs relies on the exports of the pinned version: a new pin reviews it.
        self.assertIn(f"`Microsoft.Windows.Console.ConPTY` {conpty_pair.PIN['version']}", source)
        for name in (*conpty_pair.NAMES, "licenses/conpty-LICENSE.txt", "licenses/conpty-NOTICE.md"):
            self.assertIn(name, portable.REQUIRED)
        text = (ROOT / "docs/licenses/conpty-LICENSE.txt").read_text()
        self.assertTrue(text.startswith("Copyright (c) Microsoft Corporation. All rights reserved.\n\nMIT License\n"))
        # OpenConsole.exe contains third-party code (Chromium's numerics under
        # BSD-3-Clause, wil, GSL, {fmt} and others): Windows Terminal's NOTICE.md.
        notice = (ROOT / "docs/licenses/conpty-NOTICE.md").read_text()
        self.assertTrue(notice.startswith("# NOTICES AND INFORMATION\n"))
        for component in ("## chromium/base/numerics", "## \\{fmt\\}", "## `wil`", "## `GSL`"):
            self.assertIn(component, notice)
        self.assertIn("Microsoft.Windows.Console.ConPTY", (ROOT / "THIRD_PARTY_NOTICES.md").read_text())

    def test_pin_rejects_foreign_sources_and_other_files(self):
        changes = [
            lambda pin: pin.update(url=pin["url"].replace("api.nuget.org", "mirror.example")),
            lambda pin: pin.update(url=pin["url"].replace("1.25.260930003.nupkg", "1.25.260930004.nupkg")),
            lambda pin: pin.update(package="Other.ConPTY"),
            lambda pin: pin.update(version="1.25"),
            lambda pin: pin.update(license="Proprietary"),
            lambda pin: pin.update(sha256="0" * 63),
            lambda pin: pin.update(size=0),
            lambda pin: pin.update(size=True),
            lambda pin: pin.update(unexpected=True),
            lambda pin: pin["files"].pop("OpenConsole.exe"),
            lambda pin: pin["files"].update({"conpty.lib": dict(pin["files"]["conpty.dll"])}),
            lambda pin: pin["files"]["conpty.dll"].update(path="runtimes/win-arm64/native/conpty.dll"),
            lambda pin: pin["files"]["OpenConsole.exe"].update(path="build/native/runtimes/x86/OpenConsole.exe"),
            lambda pin: pin["files"]["conpty.dll"].update(path="runtimes/win-x64/../../conpty.dll"),
            lambda pin: pin["files"]["conpty.dll"].update(path="/runtimes/win-x64/native/conpty.dll"),
            lambda pin: pin["files"]["conpty.dll"].update(path="runtimes/win-x64/native/OpenConsole.exe"),
            lambda pin: pin["files"]["OpenConsole.exe"].update(sha256="A" * 64),
            lambda pin: pin["files"]["OpenConsole.exe"].update(size=conpty_pair.FILE_LIMIT + 1),
            lambda pin: pin["files"]["OpenConsole.exe"].update(unexpected=True),
        ]
        for index, change in enumerate(changes):
            pin = copy.deepcopy(conpty_pair.load())
            change(pin)
            with self.subTest(change=index), self.assertRaises(ValueError):
                conpty_pair.validate(pin)


class FetchTests(unittest.TestCase):
    """conpty_pair.fetch and scripts/fetch-conpty.py with a synthetic package
    served from a file: URL, so nothing reaches the network."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zerus-conpty-fetch-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.package = self.directory / "feed" / "microsoft.windows.console.conpty.1.25.260930003.nupkg"
        self.package.parent.mkdir()
        files = {entry["path"]: PAIR[name] for name, entry in conpty_pair.load()["files"].items()}
        # The other architectures, headers and build files stay in the package.
        files.update({"runtimes/win-arm64/native/conpty.dll": b"arm64 conpty.dll",
                      "build/native/runtimes/arm64/OpenConsole.exe": b"arm64 OpenConsole.exe",
                      "runtimes/win-x64/lib/uap10.0/conpty.lib": b"import library", "inc/conpty.h": b"header",
                      "Microsoft.Windows.Console.ConPTY.nuspec": b"<package/>"})
        with zipfile.ZipFile(self.package, "w", zipfile.ZIP_DEFLATED) as archive:
            for name, data in files.items():
                archive.writestr(name, data)
        data = self.package.read_bytes()
        self.pin = pinned(size=len(data), sha256=hashlib.sha256(data).hexdigest())
        use_pin(self, self.pin)

    def test_fetch_writes_exactly_the_pinned_pair(self):
        output = self.directory / "pair"
        found = conpty_pair.fetch(output, url=self.package.as_uri())
        self.assertEqual(found, {name: output / name for name in PAIR})
        self.assertEqual(sorted(path.name for path in output.iterdir()), sorted(PAIR))
        for name, data in PAIR.items():
            self.assertEqual((output / name).read_bytes(), data)
        self.assertEqual(conpty_pair.check_folder(output), found)
        # A downloaded copy gives the same pair without a download.
        again = self.directory / "again"
        with patch.object(conpty_pair.urllib.request, "urlopen", side_effect=AssertionError("downloaded")):
            conpty_pair.fetch(again, package=self.package)
        self.assertEqual(sorted(path.name for path in again.iterdir()), sorted(PAIR))

    def test_a_package_that_is_not_the_pinned_one_is_never_opened(self):
        tampered = self.directory / "tampered.nupkg"
        data = bytearray(self.package.read_bytes())
        data[-1] ^= 1
        tampered.write_bytes(bytes(data))
        output = self.directory / "pair"
        with patch.object(conpty_pair.zipfile, "ZipFile", side_effect=AssertionError("opened")):
            with self.assertRaisesRegex(ValueError, "is not the pinned Microsoft.Windows.Console.ConPTY"):
                conpty_pair.fetch(output, package=tampered)
            with self.assertRaisesRegex(ValueError, "is not the pinned Microsoft.Windows.Console.ConPTY"):
                conpty_pair.fetch(output, url=tampered.as_uri())
        self.assertFalse(output.exists())
        # A download never takes more than the pinned size.
        longer = self.directory / "longer.nupkg"
        longer.write_bytes(self.package.read_bytes() + b"\0")
        with self.assertRaisesRegex(ValueError, "larger than pinned"):
            conpty_pair.fetch(output, url=longer.as_uri())
        self.assertFalse(output.exists())

    def test_a_pinned_file_that_differs_writes_neither(self):
        for name, entry in (("OpenConsole.exe", {"sha256": "0" * 64}), ("conpty.dll", {"size": len(PAIR["conpty.dll"]) + 1}),
                            ("conpty.dll", {"path": "runtimes/win-x64/other/conpty.dll"})):
            pin = copy.deepcopy(self.pin)
            pin["files"][name].update(entry)
            output = self.directory / "pair"
            with self.subTest(name=name, entry=entry), patch.object(conpty_pair, "PIN", pin), \
                    self.assertRaisesRegex(ValueError, "is not the pinned build|lacks runtimes/win-x64/other"):
                conpty_pair.fetch(output, package=self.package)
            self.assertFalse(output.exists())

    def test_a_folder_must_hold_both_pinned_files(self):
        folder = self.directory / "pair"
        conpty_pair.fetch(folder, package=self.package)
        (folder / "OpenConsole.exe").write_bytes(PAIR["OpenConsole.exe"][:-1] + b"?")
        with self.assertRaisesRegex(ValueError, "is not the OpenConsole.exe of the pinned"):
            conpty_pair.check_folder(folder)
        (folder / "OpenConsole.exe").unlink()
        with self.assertRaisesRegex(ValueError, "OpenConsole.exe is missing"):
            conpty_pair.check_folder(folder)

    def test_the_fetch_script_reuses_a_downloaded_package(self):
        output = self.directory / "pair"
        result = subprocess.run([sys.executable, "-c", WITH_PIN, json.dumps(self.pin), str(ROOT / "scripts/fetch-conpty.py"),
                                 "--output", str(output), "--nupkg", str(self.package)],
                                cwd=self.directory, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"Fetched Microsoft.Windows.Console.ConPTY 1.25.260930003 (sha256 {self.pin['sha256']})",
                      result.stdout)
        self.assertEqual(sorted(path.name for path in output.iterdir()), sorted(PAIR))
        # The repository pin refuses the synthetic package, with a message and no traceback.
        result = subprocess.run([sys.executable, str(ROOT / "scripts/fetch-conpty.py"), "--output",
                                 str(self.directory / "refused"), "--nupkg", str(self.package)],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr, f"fetch-conpty: {self.package.name} is not the pinned "
                                        "Microsoft.Windows.Console.ConPTY 1.25.260930003 package.\n")
        self.assertFalse((self.directory / "refused").exists())


class SbomLicenseTests(unittest.TestCase):
    DOCUMENT = "\n".join([
        "SPDXVersion: SPDX-2.3",
        "PackageName: BundledLibjpeg",
        "PackageLicenseConcluded: IJG AND BSD-3-Clause",
        "PackageLicenseDeclared: (FTL OR GPL-2.0-only) AND LicenseRef-Local",
        "PackageCopyrightText: <text>Copyright (C) 1991 Someone",
        "LicenseConcluded: Not-A-Tag-Inside-Text</text>",
        "FileName: ./qtcore.cpp",
        "LicenseConcluded: LGPL-3.0-only OR GPL-2.0-only OR GPL-3.0-only WITH Qt-GPL-exception-1.0",
        "LicenseInfoInFile: NOASSERTION",
        "PackageLicenseConcluded: DocumentRef-qtbase-1:LicenseRef-Shared",
        "LicenseID: LicenseRef-Local",
        "ExtractedText: <text>Permission is granted.",
        "met: conditions apply</text>",
        "LicenseID: LicenseRef-Empty",
        "ExtractedText: <text></text>",
        "",
    ])

    def test_identifiers_and_extracted_texts_are_read(self):
        named, defined = portable.sbom_licenses(self.DOCUMENT)
        self.assertEqual(named, {"IJG", "BSD-3-Clause", "FTL", "GPL-2.0-only", "LicenseRef-Local", "LGPL-3.0-only",
                                 "GPL-3.0-only", "Qt-GPL-exception-1.0", "DocumentRef-qtbase-1:LicenseRef-Shared"})
        self.assertEqual(defined, {"LicenseRef-Local"})

    def test_every_named_license_needs_a_text(self):
        texts = set(portable.QT_LICENSE_TEXTS)
        other = "LicenseID: LicenseRef-Shared\nExtractedText: <text>Shared terms</text>\n"
        self.assertEqual(portable.missing_license_texts({"a.spdx": self.DOCUMENT, "b.spdx": other}, texts), [])
        self.assertEqual(portable.missing_license_texts({"a.spdx": self.DOCUMENT}, texts),
                         ["a.spdx: DocumentRef-qtbase-1:LicenseRef-Shared"])
        self.assertEqual(portable.missing_license_texts({"a.spdx": self.DOCUMENT, "b.spdx": other}, texts - {"IJG"}),
                         ["a.spdx: IJG"])
        unknown = "PackageLicenseConcluded: Made-Up-1.0 AND LicenseRef-Empty AND LicenseRef-Elsewhere\n"
        self.assertEqual(portable.missing_license_texts({"c.spdx": unknown, "b.spdx": other}, texts),
                         ["c.spdx: LicenseRef-Elsewhere", "c.spdx: LicenseRef-Empty", "c.spdx: Made-Up-1.0"])
        with self.assertRaisesRegex(ValueError, "Unterminated"):
            portable.sbom_licenses("PackageCopyrightText: <text>open\n")


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zerus-windows-package-")
        self.addCleanup(self.temporary.cleanup)
        base = self.directory = Path(self.temporary.name)
        self.crt = base / "Microsoft.VC143.CRT"
        self.crt.mkdir()
        for name in ("vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll", "concrt140.dll"):
            (self.crt / name).write_bytes(name.encode())
        self.rust = base / "rust-licenses"
        self.frontend = base / "frontend-licenses"
        for folder in (self.rust, self.rust / "desktop", self.frontend):
            folder.mkdir(parents=True, exist_ok=True)
            (folder / "inventory.json").write_text("[]")
            (folder / "LICENSE-MIT").write_text("MIT\n")
        for name, data in {"hgs.exe": b"tested cli", "hgs-tray.exe": b"tested tauri gui",
                           "zerus-mobile-connector.exe": b"tested connector"}.items():
            (base / name).write_bytes(data)
        self.conpty = base / "conpty"
        self.conpty.mkdir()
        for name, data in PAIR.items():
            (self.conpty / name).write_bytes(data)
        self.pin = pinned()
        use_pin(self, self.pin)

    def run_script(self, name, *arguments, **options):
        command = [sys.executable, "-c", WITH_PIN, json.dumps(self.pin), str(ROOT / "scripts" / name), *arguments]
        return subprocess.run(command, capture_output=True, text=True, **options)

    def package(self, output, **overrides):
        arguments = {"--connector": self.directory / "zerus-mobile-connector.exe", "--cli": self.directory / "hgs.exe",
                     "--gui": self.directory / "hgs-tray.exe", "--output": output, "--crt-dir": self.crt,
                     "--rust-licenses": self.rust, "--frontend-licenses": self.frontend, "--conpty": self.conpty, **overrides}
        command = [part for key, value in arguments.items() for part in (key, str(value))]
        environment = {key: value for key, value in os.environ.items() if not key.startswith(("QT_", "HGS_"))}
        environment["SOURCE_DATE_EPOCH"] = "1760000000"
        return self.run_script("package-windows.py", *command, cwd=self.directory, env=environment)

    def test_next_bundle_keeps_exact_binaries_notices_and_conpty_without_qt(self):
        output = self.directory / "out" / "zerus-windows-x86_64.zip"
        result = self.package(output)
        self.assertEqual(result.returncode, 0, result.stderr)
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        info = portable.inspect(output)
        self.assertEqual(info["desktop"], "tauri")
        self.assertEqual(info["webview2_runtime"], "evergreen")
        self.assertEqual(info["source_commit"], head)
        self.assertNotIn("qt_version", info)
        with zipfile.ZipFile(output) as archive:
            names = archive.namelist()
            self.assertEqual(names, sorted(names))
            self.assertTrue(all(name.startswith(portable.TOP + "/") for name in names))
            for name, expected in {"hgs.exe": b"tested cli", "hgs-tray.exe": b"tested tauri gui",
                                   "zerus-mobile-connector.exe": b"tested connector", **PAIR}.items():
                self.assertEqual(archive.read(f"{portable.TOP}/{name}"), expected)
            self.assertFalse([name for name in names if portable.NEXT_FORBIDDEN.search(name)])
            for name in ("licenses/frontend/inventory.json", "licenses/rust/desktop/inventory.json"):
                self.assertIn(f"{portable.TOP}/{name}", names)
            readme = archive.read(f"{portable.TOP}/README.txt").decode()
            self.assertIn("WebView2 Evergreen", readme)
            self.assertIn(head, readme)
            self.assertNotRegex(readme, r"@[A-Z_]+@")
            self.assertNotRegex(readme, r"[^\r]\n")
        again = self.directory / "again.zip"
        self.assertEqual(self.package(again).returncode, 0)
        self.assertEqual(again.read_bytes(), output.read_bytes())
        report = self.run_script("check-windows-portable.py", str(output))
        self.assertEqual(report.returncode, 0, report.stderr)
        self.assertNotEqual(self.run_script("check-windows-portable.py", str(output), "--commit", "b" * 40).returncode, 0)

    def test_shared_frontend_collector_keeps_vite_notice_in_the_windows_zip(self):
        spec = importlib.util.spec_from_file_location("shared_notices", ROOT / "client/scripts/collect-notices.py")
        collector = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(collector)
        client = self.directory / "client"
        vite = client / "node_modules/vite"
        vite.mkdir(parents=True)
        (client / "package-lock.json").write_text(json.dumps({"packages": {
            "node_modules/vite": {"version": "8.3.4", "dev": True}}}))
        (vite / "package.json").write_text(json.dumps(dict(name="vite", version="8.3.4", license="MIT")))
        (vite / "LICENSE").write_text("Vite MIT runtime notice\n")
        frontend = self.directory / "shared-notices"
        collector.collect(client, frontend)
        output = self.directory / "shared.zip"
        result = self.package(output, **{"--frontend-licenses": frontend})
        self.assertEqual(result.returncode, 0, result.stderr)
        with zipfile.ZipFile(output) as archive:
            self.assertEqual(archive.read(f"{portable.TOP}/licenses/frontend/vite-8.3.4/LICENSE"),
                             b"Vite MIT runtime notice\n")
            inventory = json.loads(archive.read(f"{portable.TOP}/licenses/frontend/inventory.json"))
            self.assertEqual(inventory["packages"][0]["name"], "vite")

    def test_wrong_conpty_or_missing_runtime_and_notices_leave_no_bundle(self):
        for path in [self.conpty / "OpenConsole.exe", self.crt / "vcruntime140_1.dll",
                     self.rust / "desktop/inventory.json", self.frontend / "inventory.json"]:
            with self.subTest(path=path):
                original = path.read_bytes()
                path.unlink()
                output = self.directory / "refused.zip"
                self.assertNotEqual(self.package(output).returncode, 0)
                self.assertFalse(output.exists())
                path.write_bytes(original)

    def test_accidental_qt_runtime_in_input_cannot_be_shipped(self):
        (self.crt / "Qt6Core.dll").write_bytes(b"old Qt")
        output = self.directory / "refused.zip"
        result = self.package(output)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must not contain Qt", result.stderr)
        self.assertFalse(output.exists())


class NextInspectTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zerus-next-zip-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        use_pin(self, pinned())
        self.files = {name: PAIR.get(name, b"x") for name in portable.NEXT_REQUIRED}
        self.info = manifest(desktop="tauri", webview2_runtime="evergreen", mobile_connector_bundled=True)
        del self.info["qt_version"]

    def test_next_layout_is_checked_without_requiring_qt(self):
        self.assertEqual(portable.inspect(bundle(self.directory / "ok.zip", self.files, self.info)), self.info)
        for name in portable.NEXT_REQUIRED:
            if name == "build-info.json": continue
            with self.subTest(name=name):
                files = {key: data for key, data in self.files.items() if key != name}
                with self.assertRaises(ValueError):
                    portable.inspect(bundle(self.directory / "missing.zip", files, self.info))

    def test_runtime_mode_unknown_backend_and_qt_payloads_are_refused(self):
        for update in [{"desktop": "unknown"}, {"desktop": {}}, {"webview2_runtime": "fixed"},
                       {"webview2_runtime": None}, {"qt_version": QT_VERSION}]:
            with self.subTest(update=update), self.assertRaises(ValueError):
                portable.inspect(bundle(self.directory / "bad.zip", self.files, {**self.info, **update}))
        for name in ["Qt6Core.dll", "platforms/qwindows.dll", "licenses/qt/LGPL-3.0-only.txt"]:
            with self.subTest(name=name), self.assertRaises(ValueError):
                portable.inspect(bundle(self.directory / "bad.zip", {**self.files, name: b"Qt"}, self.info))


if __name__ == "__main__":
    unittest.main()
