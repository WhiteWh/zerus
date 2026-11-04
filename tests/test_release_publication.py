"""Credential-free contracts for release provenance and partial-publication retries."""
import copy
import importlib.util
import io
import json
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import macos_release
from homebrew_tap_fixture import make_tap_remote, remote_file
from windows_portable_fixture import pinned_pair, portable_zip
import conpty_pair
import release_common as common
import windows_portable
import windows_installer_release
from windows_installer_fixture import installer_files


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


fetch = load("fetch_candidate", "fetch-release-candidate.py")
publish = load("publish_candidate", "publish-release.py")
prepare = load("prepare_candidate", "prepare-release.py")
VERSION = "0.37.0"
COMMIT = "a" * 40


class PublicationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zerus-release-contract-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        # The synthetic zips' ConPTY pair passes as the pinned build.
        pin = patch.object(conpty_pair, "PIN", pinned_pair())
        pin.start()
        self.addCleanup(pin.stop)
        self.info = {"schema": 1, "version": VERSION, "source_commit": COMMIT,
                     "architecture": "x86_64", "distribution": "Arch Linux",
                     "publication": "candidate", "native_agents_bundled": False,
                     "aur_packages": list(common.PACKAGES),
                     "binary_dependencies": {name: "1.2.3" for name in common.BINARY_LIBRARIES}}

    def candidate(self, macos=False, windows=False, name="candidate", dmg=False, **zip_options):
        directory = self.directory / name
        directory.mkdir()
        self.info.pop("windows_portable", None)
        if windows:
            self.info["windows_portable"] = windows_portable.asset_name(VERSION)
            portable_zip(directory / self.info["windows_portable"], **{"version": VERSION, **zip_options})
        if macos:
            archive = directory / macos_release.macos_asset(VERSION)
            archive.write_bytes(b"notarized application")
            self.info.update(macos={"asset": archive.name, "qt_version": "6.11.3", "team_id": "ABCDE12345",
                                    "notarization_id": "0123abcd-4567-89ef-0123-456789abcdef",
                                    "minimum_system_version": "14.0"})
            if dmg:
                image = directory / macos_release.macos_dmg(VERSION)
                image.write_bytes(b"notarized disk image")
                self.info["macos"]["dmg"] = {"asset": image.name,
                                             "notarization_id": "1123abcd-4567-89ef-0123-456789abcdef"}
            (directory / "homebrew").mkdir()
            (directory / "homebrew/zerus.rb").write_text(macos_release.render_cask(VERSION, common.sha256(archive)))
        for suffix in ("source", "arch-x86_64"):
            (directory / f"zerus-{VERSION}-{suffix}.tar.gz").write_bytes(suffix.encode())
        for package in ("zerus", "zerus-ade-bin"):
            (directory / f"{package}-{VERSION}-1-x86_64.pkg.tar.zst").write_bytes(package.encode())
        (directory / "release-info.json").write_text(json.dumps(self.info))
        for package in common.PACKAGES:
            recipe_dir = directory / "aur" / package
            recipe_dir.mkdir(parents=True)
            vcs = package == "zerus-git"
            if vcs:
                source = "zerus::git+https://github.com/ufna/zerus.git#branch=main"
                digest = "SKIP"
            else:
                suffix = "source" if package == "zerus" else "arch-x86_64"
                source = f"https://github.com/{common.REPOSITORY}/releases/download/v{VERSION}/zerus-{VERSION}-{suffix}.tar.gz"
                digest = common.sha256(directory / f"zerus-{VERSION}-{suffix}.tar.gz")
            (recipe_dir / "PKGBUILD").write_text(common.MAINTAINER + "\npkgname=" + package + "\n")
            (recipe_dir / ".SRCINFO").write_text(f"pkgbase = {package}\n\tpkgver = {VERSION}\n\tpkgrel = 1\n\tarch = x86_64\n\tsource = {source}\n\tsha256sums = {digest}\npkgname = {package}\n")
            if package == "zerus-ade-bin":
                with (recipe_dir / ".SRCINFO").open("a") as output:
                    output.write("".join(f"\tdepends = {name}>=1.2.3\n" for name in common.BINARY_LIBRARIES))
        common.finish_candidate(directory)
        return directory

    def test_sealed_candidate_validates_exact_version_and_commit(self):
        directory = self.candidate()
        info, hashes = common.validate_candidate(directory, VERSION, COMMIT)
        self.assertEqual(info["source_commit"], COMMIT)
        self.assertEqual(len(hashes), 6)
        with self.assertRaisesRegex(ValueError, "another commit"):
            common.validate_candidate(directory, VERSION, "b" * 40)

    def test_cask_template_renders_only_version_and_digest(self):
        text = macos_release.render_cask(VERSION, "c" * 64)
        self.assertIn(f'version "{VERSION}"', text)
        self.assertIn('sha256 "' + "c" * 64 + '"', text)
        self.assertIn("releases/download/v#{version}/zerus-#{version}-macos-arm64.zip", text)
        self.assertIn('conflicts_with cask: ["zerus@nightly", "zerus@dev"]', text)
        self.assertEqual(macos_release.cask_fields(text), {"version": VERSION, "sha256": "c" * 64})
        self.assertNotRegex(text, r"@[A-Z_]+@", "template markers must all be replaced")
        for stanza in ("preflight", "postflight", "zap", "uninstall", "uninstall_preflight", "launchctl", "quit", "auto_updates"):
            self.assertNotRegex(text, rf"(?m)^  {stanza}\b", "upgrades must not quit the GUI or unload agents")
        with self.assertRaisesRegex(ValueError, "template"):
            macos_release.require_template_cask(text + "  postflight do\n", VERSION, "c" * 64)
        with self.assertRaisesRegex(ValueError, "version"):
            macos_release.render_cask("0.37", "c" * 64)
        with self.assertRaisesRegex(ValueError, "digest"):
            macos_release.render_cask(VERSION, "C" * 64)

    def test_cask_versions_compare_numerically(self):
        self.assertLess(macos_release.cask_version_key("0.9.9"), macos_release.cask_version_key("0.10.0"))
        with self.assertRaisesRegex(ValueError, "version"):
            macos_release.cask_version_key("0.10.0-rc1")

    def test_schema_two_candidate_seals_macos_asset_and_cask(self):
        directory = self.candidate(macos=True)
        info, hashes = common.validate_candidate(directory, VERSION, COMMIT)
        self.assertEqual(info["schema"], 2)
        self.assertEqual(info["homebrew_casks"], ["zerus"])
        self.assertEqual(info["architecture"], "x86_64", "Arch fields stay unchanged for the mirror")
        self.assertIn(macos_release.macos_asset(VERSION), hashes)
        self.assertEqual(len(hashes), 7)

    def test_schema_two_rejects_changed_zip_or_cask(self):
        directory = self.candidate(macos=True)
        (directory / "homebrew/zerus.rb").write_text((directory / "homebrew/zerus.rb").read_text() + "# edited\n")
        with self.assertRaisesRegex(ValueError, "template"):
            common.validate_candidate(directory, VERSION)
        (directory / "homebrew/zerus.rb").write_text(macos_release.render_cask(VERSION, "d" * 64))
        with self.assertRaisesRegex(ValueError, "template"):
            common.validate_candidate(directory, VERSION)

    def test_sealed_previous_cask_validates_unchanged_but_new_casks_use_the_current_template(self):
        directory = self.candidate(macos=True, dmg=True)
        cask = directory / "homebrew/zerus.rb"
        digest = common.sha256(directory / self.info["macos"]["asset"])
        previous = (ROOT / "packaging/homebrew/previous/zerus.rb.in").read_text().replace(
            "@VERSION@", VERSION).replace("@SHA256@", digest)
        cask.write_text(previous)
        before = {path.relative_to(directory): path.read_bytes() for path in directory.rglob("*") if path.is_file()}
        common.validate_candidate(directory, VERSION, COMMIT)
        self.assertEqual(before, {path.relative_to(directory): path.read_bytes()
                                  for path in directory.rglob("*") if path.is_file()})
        with self.assertRaisesRegex(ValueError, "template"):
            macos_release.require_template_cask(previous, VERSION, digest)
        self.assertNotIn("  zap ", macos_release.render_cask(VERSION, digest))
        for edited in (previous + "# edited\n", previous.replace(digest, "d" * 64),
                       previous.replace(f'version "{VERSION}"', 'version "0.37.1"'),
                       previous.replace("com.hgdev.hgs-recovery", "com.example.foreign"),
                       previous.replace('conflicts_with cask: "zerus@nightly"',
                                        'conflicts_with cask: ["zerus@nightly", "zerus@dev"]')):
            with self.subTest(edited=edited):
                cask.write_text(edited)
                with self.assertRaisesRegex(ValueError, "template"):
                    common.validate_candidate(directory, VERSION, COMMIT)

    def test_disk_image_is_sealed_and_cannot_change_after_validation(self):
        directory = self.candidate(macos=True, dmg=True)
        info, hashes = common.validate_candidate(directory, VERSION, COMMIT)
        image = directory / info["macos"]["dmg"]["asset"]
        self.assertEqual(hashes[image.name], common.sha256(image))
        self.assertIn(info["macos"]["asset"], hashes)
        image.write_bytes(b"changed disk image")
        with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
            common.validate_candidate(directory, VERSION)

    def test_disk_image_requires_exact_name_and_its_own_notarization(self):
        directory = self.candidate(macos=True, dmg=True)
        original = json.loads((directory / "release-info.json").read_text())
        for image in (None, {}, {"asset": "foreign.dmg", "notarization_id": "1123abcd-4567-89ef-0123-456789abcdef"},
                      {"asset": macos_release.macos_dmg(VERSION), "notarization_id": None}):
            with self.subTest(image=image):
                info = dict(original, macos=dict(original["macos"], dmg=image))
                (directory / "release-info.json").write_text(json.dumps(info))
                common.finish_candidate(directory)
                with self.assertRaisesRegex(ValueError, "disk image provenance"):
                    common.validate_candidate(directory, VERSION)

    def test_schema_two_requires_verified_macos_provenance(self):
        self.info["team_id"] = "unused"
        directory = self.candidate(macos=True)
        info = json.loads((directory / "release-info.json").read_text())
        info["macos"]["team_id"] = "not a team"
        (directory / "release-info.json").write_text(json.dumps(info))
        common.finish_candidate(directory)
        with self.assertRaisesRegex(ValueError, "macOS provenance"):
            common.validate_candidate(directory, VERSION)

    def signed_macos(self, **changes):
        directory = self.directory / "macos-signed"
        directory.mkdir()
        (directory / macos_release.macos_asset(VERSION)).write_bytes(b"notarized application")
        metadata = {"asset": macos_release.macos_asset(VERSION), "qt_version": "6.11.2", "team_id": "ABCDE12345",
                    "notarization_id": "0123abcd-4567-89ef-0123-456789abcdef", "minimum_system_version": "14.0"}
        metadata.update(changes)
        (directory / "macos-info.json").write_text(json.dumps(metadata))
        return directory

    def test_signed_macos_artifact_enters_the_candidate_with_its_cask(self):
        output = self.directory / "dist"
        output.mkdir()
        info = {"version": VERSION}
        macos_release.add_macos(output, self.signed_macos(), VERSION, info)
        archive = output / macos_release.macos_asset(VERSION)
        self.assertEqual(archive.read_bytes(), b"notarized application")
        self.assertEqual(info["macos"]["notarization_id"], "0123abcd-4567-89ef-0123-456789abcdef")
        macos_release.require_template_cask((output / "homebrew/zerus.rb").read_text(), VERSION, common.sha256(archive))

    def test_signed_disk_image_is_copied_unchanged_and_cask_keeps_zip(self):
        image = {"asset": macos_release.macos_dmg(VERSION),
                 "notarization_id": "1123abcd-4567-89ef-0123-456789abcdef"}
        source = self.signed_macos(dmg=image)
        (source / image["asset"]).write_bytes(b"sealed disk image")
        output = self.directory / "dist"
        output.mkdir()
        info = {"version": VERSION}
        macos_release.add_macos(output, source, VERSION, info)
        self.assertEqual((output / image["asset"]).read_bytes(), b"sealed disk image")
        macos_release.require_template_cask((output / "homebrew/zerus.rb").read_text(), VERSION,
                                           common.sha256(output / macos_release.macos_asset(VERSION)))
        self.assertEqual(info["macos"]["dmg"], image)

    def test_signed_macos_artifact_must_be_exact(self):
        for changes in ({"asset": "zerus-0.37.0-macos-x86_64.zip"}, {"team_id": "bad"}, {"notarization_id": None}):
            with self.subTest(changes=changes):
                output = self.directory / "dist"
                output.mkdir()
                with self.assertRaises(ValueError):
                    macos_release.add_macos(output, self.signed_macos(**changes), VERSION, {"version": VERSION})
                self.assertFalse((output / "homebrew").exists())
                for path in (output, self.directory / "macos-signed"):
                    for child in sorted(path.rglob("*"), reverse=True):
                        child.unlink() if child.is_file() else child.rmdir()
                    path.rmdir()
        output = self.directory / "dist"
        output.mkdir()
        source = self.signed_macos()
        (source / "unreviewed.dylib").write_bytes(b"extra")
        with self.assertRaisesRegex(ValueError, "Unexpected signed macOS artifact"):
            macos_release.add_macos(output, source, VERSION, {"version": VERSION})

    def test_schema_one_candidate_cannot_carry_a_cask(self):
        directory = self.candidate()
        (directory / "homebrew").mkdir()
        (directory / "homebrew/zerus.rb").write_text(macos_release.render_cask(VERSION, "c" * 64))
        with self.assertRaisesRegex(ValueError, "Unsealed"):
            common.validate_candidate(directory, VERSION)

    def test_changed_binary_fails_before_publication(self):
        directory = self.candidate()
        (directory / f"zerus-{VERSION}-arch-x86_64.tar.gz").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
            common.validate_candidate(directory, VERSION)

    def test_recipe_edits_must_match_sealed_aur_archive(self):
        directory = self.candidate()
        (directory / "aur/zerus/PKGBUILD").write_text("changed")
        with self.assertRaisesRegex(ValueError, "sealed archive"):
            common.validate_candidate(directory, VERSION)

    def test_extra_file_and_unsafe_checksum_are_rejected(self):
        directory = self.candidate()
        (directory / "unreviewed.txt").write_text("unsealed")
        with self.assertRaisesRegex(ValueError, "Unsealed"):
            common.validate_candidate(directory, VERSION)
        (directory / "unreviewed.txt").unlink()
        with (directory / "SHA256SUMS").open("a") as output:
            output.write("0" * 64 + "  ../outside\n")
        with self.assertRaisesRegex(ValueError, "Invalid checksum"):
            common.validate_candidate(directory, VERSION)

    def test_stable_candidate_seals_the_tested_windows_zip(self):
        directory = self.candidate(windows=True)
        info, hashes = common.validate_candidate(directory, VERSION, COMMIT)
        name = info["windows_portable"]
        self.assertEqual(name, f"zerus-{VERSION}-windows-x86_64.zip")
        self.assertEqual(len(hashes), 7)
        self.assertEqual(hashes[name], common.sha256(directory / name))
        self.assertIn(f"{hashes[name]}  {name}\n", (directory / "SHA256SUMS").read_text())
        (directory / name).write_bytes((directory / name).read_bytes() + b"changed")
        with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
            common.validate_candidate(directory, VERSION)

    def test_candidates_sealed_before_the_windows_zip_still_validate(self):
        # 0.42.2 and earlier were sealed without a zip; retrying them must keep working.
        info, hashes = common.validate_candidate(self.candidate(), VERSION, COMMIT)
        self.assertNotIn("windows_portable", info)
        self.assertEqual(len(hashes), 6)
        self.assertFalse(any(name.endswith(".zip") for name in hashes))

    def test_later_stable_candidates_must_carry_the_windows_zip(self):
        # Portable Windows apps announce every stable release after this one as a
        # Windows update, so publication never lets one through without the zip.
        self.assertEqual(common.LAST_STABLE_WITHOUT_WINDOWS, "0.42.2")
        self.assertIn(f'lastStableWithoutWindows="{common.LAST_STABLE_WITHOUT_WINDOWS}"',
                      (ROOT / "tray/src/UpdateController.cpp").read_text())
        without = self.candidate()
        with_zip = self.candidate(windows=True, name="with-zip")
        with patch.object(common, "LAST_STABLE_WITHOUT_WINDOWS", "0.36.9"):
            with self.assertRaisesRegex(ValueError, "after 0.36.9 must carry the tested Windows zip"):
                common.validate_candidate(without, VERSION, COMMIT)
            self.assertIn("windows_portable", common.validate_candidate(with_zip, VERSION, COMMIT)[0])
        with patch.object(common, "LAST_STABLE_WITHOUT_WINDOWS", VERSION):
            self.assertNotIn("windows_portable", common.validate_candidate(without, VERSION, COMMIT)[0])

    def test_windows_zip_and_manifest_must_agree(self):
        directory = self.candidate(windows=True)
        info = json.loads((directory / "release-info.json").read_text())
        # A zip the manifest does not name is never published unnoticed.
        (directory / "release-info.json").write_text(json.dumps({key: value for key, value in info.items()
                                                                 if key != "windows_portable"}))
        common.finish_candidate(directory)
        with self.assertRaisesRegex(ValueError, "Unexpected/missing release assets"):
            common.validate_candidate(directory, VERSION)
        # A named zip that is missing, or named for another version or platform, fails too.
        (directory / "release-info.json").write_text(json.dumps(info))
        zip_path = directory / info["windows_portable"]
        zip_bytes = zip_path.read_bytes()
        zip_path.unlink()
        common.finish_candidate(directory)
        with self.assertRaisesRegex(ValueError, "Unexpected/missing release assets"):
            common.validate_candidate(directory, VERSION)
        zip_path.write_bytes(zip_bytes)
        for name in ("zerus-windows-x86_64.zip", "zerus-0.36.2-windows-x86_64.zip", f"zerus-{VERSION}-windows-arm64.zip"):
            with self.subTest(name=name):
                (directory / "release-info.json").write_text(json.dumps({**info, "windows_portable": name}))
                common.finish_candidate(directory)
                with self.assertRaisesRegex(ValueError, "Invalid Windows stable asset"):
                    common.validate_candidate(directory, VERSION)

    def test_windows_zip_from_another_build_or_unsafe_is_rejected(self):
        for index, (options, message) in enumerate([
                ({"commit": "b" * 40}, "another commit/version"),
                ({"version": "0.36.2"}, "another commit/version"),
                ({"entries": [(f"{windows_portable.TOP}/../outside", "x")]}, "Unsafe Windows portable entry"),
                ({"entries": [("other/hgs.exe", "x")]}, "Unsafe Windows portable entry")]):
            directory = self.candidate(windows=True, name=f"candidate-{index}", **options)
            with self.subTest(options=options), self.assertRaisesRegex(ValueError, message):
                common.validate_candidate(directory, VERSION)

    def prepare(self, portable, output):
        """Run prepare-release.py with the Git checkout and the verified pacman package replaced by fixtures."""
        product = (ROOT / "VERSION").read_text().strip()
        buildinfo = "".join(f"installed = {name}-1.2.3-1-x86_64\n" for name in common.BINARY_LIBRARIES)
        def check_output(command, **kwargs):
            if command[0] == "tar":
                return "usr/share/doc/zerus/build-info.json\n.BUILDINFO\n"
            return {"status": "", "rev-parse": COMMIT + "\n", "show": "1760000000\n",
                    "archive": b"exact source archive"}[command[1]]
        def run(command, **kwargs):
            root = Path(command[command.index("-C") + 1])
            (root / "usr/share/doc/zerus").mkdir(parents=True)
            (root / "usr/share/doc/zerus/build-info.json").write_text(
                json.dumps({"source_commit": COMMIT, "version": product}))
            (root / ".BUILDINFO").write_text(buildinfo)
        installer, build = installer_files(self.directory, portable, product, product, COMMIT)
        arguments = ["prepare-release.py", "--version", product, "--package", "arch.pkg.tar.zst",
                     "--windows-zip", str(portable), "--output", str(output)]
        arguments += ["--windows-installer", str(installer), "--windows-installer-info", str(build)]
        with patch.object(prepare.subprocess, "check_output", side_effect=check_output), \
                patch.object(prepare.subprocess, "run", side_effect=run), patch.object(sys, "argv", arguments):
            prepare.main()

    def test_prepare_copies_the_tested_windows_zip_unchanged_under_its_stable_name(self):
        product = (ROOT / "VERSION").read_text().strip()
        portable = portable_zip(self.directory / "zerus-windows-x86_64.zip", version=product)
        output = self.directory / "dist"
        self.prepare(portable, output)
        info = json.loads((output / "release-info.json").read_text())
        self.assertEqual(info["windows_portable"], f"zerus-{product}-windows-x86_64.zip")
        self.assertEqual((output / info["windows_portable"]).read_bytes(), portable.read_bytes())
        setup = output / info["windows_installer"]["asset"]
        self.assertTrue(setup.is_file())
        self.assertEqual(info["windows_installer"]["portable_sha256"], common.sha256(portable))
        self.assertEqual(info["source_commit"], COMMIT)
        # The zip of another commit or version, or an unsafe one, leaves no candidate.
        for name, options in (("commit", {"commit": "b" * 40, "version": product}), ("version", {"version": "0.0.1"}),
                              ("layout", {"version": product, "entries": [("outside", "x")]})):
            with self.subTest(mismatch=name), self.assertRaises(SystemExit), patch("sys.stderr", io.StringIO()):
                self.prepare(portable_zip(self.directory / f"{name}.zip", **options), self.directory / name)
            self.assertFalse((self.directory / name).exists())

    def test_wrong_package_architecture_fails(self):
        directory = self.candidate()
        path = directory / "aur/zerus/.SRCINFO"
        path.write_text(path.read_text().replace("arch = x86_64", "arch = aarch64"))
        common.finish_candidate(directory)
        with self.assertRaisesRegex(ValueError, "Wrong package metadata"):
            common.validate_candidate(directory, VERSION)

    def test_vcs_version_only_updates_are_not_pushed(self):
        old = "pkgver=0.36.2.r8.gabcd\ndepends=('python')\n"
        new = old.replace("0.36.2.r8.gabcd", "0.37.0.r20.g1234")
        self.assertFalse(common.recipe_changed(old, new, vcs=True))
        self.assertTrue(common.recipe_changed(old, new, vcs=False))
        self.assertTrue(common.recipe_changed(old, new.replace("python", "python bash"), vcs=True))
        old = "\tpkgver = 0.36.2.r8.gabcd\n\tprovides = zerus=0.36.2.r8.gabcd\n"
        self.assertFalse(common.recipe_changed(old, old.replace("0.36.2.r8.gabcd", "0.37.0.r20.g1234"), vcs=True))

    def test_version_comparison_prevents_downgrades(self):
        def version(text, release="1"):
            return common.package_version({"pkgver": [text], "pkgrel": [release]})
        self.assertGreater(version("0.37.0"), version("0.36.2.r99.gabcd"))
        self.assertGreater(version("0.37.0.r20.gabcd"), version("0.37.0.r19.g1234"))
        self.assertGreater(version("0.37.0", "2"), version("0.37.0"))

    def test_aur_retry_preserves_refreshed_descriptions_without_writes(self):
        directory = self.candidate()
        previous, current = common.DESCRIPTION_UPDATES[0]
        for package in common.PACKAGES:
            source = directory / "aur" / package
            for name, line in (("PKGBUILD", f"pkgdesc='{previous}'\n"),
                               (".SRCINFO", f"\tpkgdesc = {previous}\n")):
                path = source / name
                path.write_text(path.read_text() + line)

        def clone(args, **kwargs):
            destination = Path(args[-1])
            destination.mkdir(parents=True)
            source = directory / "aur" / destination.name
            for name in ("PKGBUILD", ".SRCINFO"):
                text = (source / name).read_text().replace(previous, current)
                if destination.name == "zerus-git":
                    text = text.replace(VERSION, "0.36.2.r8.gabcd")
                (destination / name).write_text(text)

        def git(directory, *args, **kwargs):
            self.assertEqual(args, ("ls-files",), "A retry must not write AUR refs or files")
            return "PKGBUILD\n.SRCINFO"

        with patch.object(publish.subprocess, "check_output", return_value=" ".join(common.PACKAGES)), \
                patch.object(publish.subprocess, "run", side_effect=clone), \
                patch.object(publish, "git", side_effect=git), \
                patch.object(publish.urllib.request, "urlopen", return_value=io.StringIO('{"results": []}')):
            self.assertEqual(publish.prepare_aur(directory, self.directory / "retry", [], {}), [])
        for package in common.PACKAGES:
            self.assertIn(current, (self.directory / "retry" / package / "PKGBUILD").read_text())

    def test_description_retry_exception_does_not_hide_recipe_or_version_changes(self):
        directory = self.candidate()
        package = "zerus-ade-bin"
        source = directory / "aur" / package
        previous, current = common.DESCRIPTION_UPDATES[0]
        for name, line in (("PKGBUILD", f"pkgdesc='{previous}'\n"),
                           (".SRCINFO", f"\tpkgdesc = {previous}\n")):
            path = source / name
            path.write_text(path.read_text() + line)
        cases = (("PKGBUILD", "\nunreviewed_command\n"),
                 (".SRCINFO", "\tdepends = unreviewed-library\n"),
                 (".SRCINFO", "\tsha256sums = " + "f" * 64 + "\n"),
                 (".SRCINFO", "\tpkgver = 0.38.0\n"))
        for index, (changed_file, addition) in enumerate(cases):
            def clone(args, **kwargs):
                destination = Path(args[-1])
                destination.mkdir(parents=True)
                for name in ("PKGBUILD", ".SRCINFO"):
                    text = (source / name).read_text().replace(previous, current)
                    if name == changed_file:
                        if "pkgver =" in addition:
                            text = text.replace("pkgver = " + VERSION, "pkgver = 0.38.0")
                        else:
                            text += addition
                    (destination / name).write_text(text)
            with self.subTest(changed_file=changed_file, addition=addition), \
                    patch.object(publish.subprocess, "check_output", return_value=package), \
                    patch.object(publish.subprocess, "run", side_effect=clone), \
                    patch.object(publish, "git", return_value="PKGBUILD\n.SRCINFO") as git, \
                    patch.object(publish.urllib.request, "urlopen", return_value=io.StringIO('{"results": []}')):
                with self.assertRaisesRegex(ValueError, "pkgrel/version bump|downgrade"):
                    publish.prepare_aur(directory, self.directory / f"retry-{index}", [], {}, (package,))
                self.assertEqual(git.call_count, 1)

    def test_binary_library_floors_come_from_buildinfo(self):
        text = "".join(f"installed = {name}-1:6.11.2-3\n" for name in common.BINARY_LIBRARIES)
        self.assertEqual(common.binary_dependencies(text), {name: "1:6.11.2" for name in common.BINARY_LIBRARIES})
        current = text.replace("-3\n", "-3-x86_64\n")
        self.assertEqual(common.binary_dependencies(current), {name: "1:6.11.2" for name in common.BINARY_LIBRARIES})
        with self.assertRaisesRegex(ValueError, "Missing/ambiguous"):
            common.binary_dependencies(text + "installed = webkit2gtk-4.1-6.11.2-4\n")
        with self.assertRaisesRegex(ValueError, "Missing/ambiguous"):
            common.binary_dependencies("")

    def test_next_recipes_drop_qt_and_both_legacy_candidates_still_validate(self):
        self.assertIn("webkit2gtk-4.1", common.BINARY_LIBRARIES)
        self.assertIn("alsa-lib", common.BINARY_LIBRARIES)
        self.assertNotIn("xdotool", common.BINARY_LIBRARIES)
        self.assertFalse(any(name.startswith(("qt6-", "kstatus", "kwindows", "kidletime"))
                             for name in common.BINARY_LIBRARIES))
        for name in ("zerus", "zerus-ade-bin"):
            recipe = (ROOT / "packaging/aur" / name / "PKGBUILD.in").read_text()
            self.assertNotIn("qt6-", recipe)
        for names in (common.QT_BINARY_LIBRARIES, common.LEGACY_BINARY_LIBRARIES):
            self.info["binary_dependencies"] = {name: "1.2.3" for name in names}
            directory = self.candidate(name="legacy-" + str(len(names)))
            path = directory / "aur/zerus-ade-bin/.SRCINFO"
            path.write_text(path.read_text() + "".join(f"\tdepends = {name}>=1.2.3\n" for name in names))
            common.finish_candidate(directory)
            common.validate_candidate(directory, VERSION)
        with self.assertRaisesRegex(ValueError, "build-library provenance"):
            common.binary_library_names({name: "1.2.3" for name in common.BINARY_LIBRARIES if name != "gtk3"})

    def test_binary_recipe_must_declare_library_version_floors(self):
        directory = self.candidate()
        path = directory / "aur/zerus-ade-bin/.SRCINFO"
        path.write_text(path.read_text().replace("depends = webkit2gtk-4.1>=1.2.3", "depends = webkit2gtk-4.1"))
        common.finish_candidate(directory)
        with self.assertRaisesRegex(ValueError, "version floors"):
            common.validate_candidate(directory, VERSION)

    def test_binary_library_names_match_exact_packages(self):
        text = "".join(f"installed = {name}-6.12.0-1-x86_64\n" for name in common.BINARY_LIBRARIES)
        text += "installed = gtk3-debug-6.12.0-1-any\ninstalled = qt6-base-6.12.0-1-x86_64\n"
        self.assertEqual(common.binary_dependencies(text), {name: "6.12.0" for name in common.BINARY_LIBRARIES})
        for malformed in (text.replace("installed = gtk3-6.12.0-1-x86_64\n", ""),
                          text + "installed = gtk3-6.11.2-1-x86_64\n"):
            with self.assertRaisesRegex(ValueError, "Missing/ambiguous.*gtk3"):
                common.binary_dependencies(malformed)

    def test_binary_library_versions_and_releases_still_fail_closed(self):
        text = "".join(f"installed = {name}-1:6.12.0-2.1-any\n" for name in common.BINARY_LIBRARIES)
        self.assertEqual(common.binary_dependencies(text), {name: "1:6.12.0" for name in common.BINARY_LIBRARIES})
        for replacement in ["gtk3-6.12.0-invalid-any", "gtk3-invalid-1-x86_64"]:
            with self.subTest(replacement=replacement), self.assertRaisesRegex(ValueError, "Unexpected"):
                common.binary_dependencies(text.replace("gtk3-1:6.12.0-2.1-any", replacement))

    def test_only_successful_upstream_main_manual_candidate_is_accepted(self):
        run = {"path": ".github/workflows/release.yml", "event": "workflow_dispatch",
               "head_branch": "main", "head_repository": {"full_name": common.REPOSITORY},
               "status": "completed", "conclusion": "success", "head_sha": COMMIT}
        self.assertEqual(fetch.validate_run(run), COMMIT)
        for field, value in (("event", "pull_request"), ("path", ".github/workflows/ci.yml"),
                             ("head_branch", "feature"), ("conclusion", "failure"),
                             ("head_repository", {"full_name": "example/fork"})):
            with self.subTest(field=field), self.assertRaises(ValueError):
                fetch.validate_run({**run, field: value})

    def test_zip_traversal_and_symlinks_fail_before_extraction(self):
        for name, attributes in (("../escape", 0), ("link", (stat.S_IFLNK | 0o777) << 16)):
            with self.subTest(name=name):
                path = self.directory / "bad.zip"
                with zipfile.ZipFile(path, "w") as output:
                    entry = zipfile.ZipInfo(name)
                    entry.external_attr = attributes
                    output.writestr(entry, "target")
                destination = self.directory / "extracted"
                with self.assertRaises(ValueError):
                    fetch.unpack(path, destination)
                self.assertFalse(destination.exists())

    def test_existing_tag_must_match_exact_candidate(self):
        def response(value):
            return subprocess.CompletedProcess([], 0, json.dumps({"object": value}), "")
        with patch.object(publish.subprocess, "run", return_value=response({"type": "commit", "sha": "b" * 40})):
            with self.assertRaisesRegex(ValueError, "different commit"):
                publish.verify_tag("v" + VERSION, COMMIT)
        with patch.object(publish.subprocess, "run", return_value=response({"type": "tag", "sha": "c" * 40})), \
                patch.object(publish, "api", return_value={"object": {"type": "commit", "sha": COMMIT}}):
            self.assertTrue(publish.verify_tag("v" + VERSION, COMMIT))

    def test_unpublished_draft_is_found_in_authenticated_paginated_listing(self):
        draft = {"id": 42, "tag_name": "v" + VERSION, "draft": True,
                 "target_commitish": COMMIT, "assets": []}
        missing = subprocess.CompletedProcess([], 1, "", "gh: HTTP 404")
        pages = [[{"tag_name": "v0.36.2", "draft": False}], [draft]]
        with patch.object(publish.subprocess, "run", return_value=missing), \
                patch.object(publish, "gh", return_value=json.dumps(pages)) as listing:
            self.assertEqual(publish.release_for_tag(draft["tag_name"]), draft)
            listing.assert_called_once_with("api", "--paginate", "--slurp",
                                            f"repos/{common.REPOSITORY}/releases?per_page=100")
        with patch.object(publish.subprocess, "run", return_value=missing), \
                patch.object(publish, "gh", return_value=json.dumps([[]])):
            self.assertIsNone(publish.release_for_tag(draft["tag_name"]))
        with patch.object(publish.subprocess, "run", return_value=missing), \
                patch.object(publish, "gh", return_value=json.dumps([[draft], [draft]])):
            with self.assertRaisesRegex(ValueError, "Multiple releases"):
                publish.release_for_tag(draft["tag_name"])

    def test_release_lookup_errors_cannot_be_treated_as_absence(self):
        forbidden = subprocess.CompletedProcess([], 1, "", "gh: HTTP 403")
        with patch.object(publish.subprocess, "run", return_value=forbidden), \
                patch.object(publish, "gh") as listing:
            with self.assertRaisesRegex(ValueError, "Cannot inspect"):
                publish.release_for_tag("v" + VERSION)
            listing.assert_not_called()

    def test_retry_finishes_existing_draft_then_leaves_public_assets_unchanged(self):
        directory = self.candidate()
        info, sums = common.validate_candidate(directory, VERSION)
        hashes = {**sums, "SHA256SUMS": common.sha256(directory / "SHA256SUMS")}
        release = {"draft": True, "tag_name": "v" + VERSION, "target_commitish": COMMIT, "assets": []}
        first = next(iter(hashes))
        def asset(name):
            return {"name": name, "digest": "sha256:" + hashes[name], "size": (directory / name).stat().st_size}
        release["assets"].append(asset(first))
        commands = []
        def gh(*args):
            commands.append(args)
            if args[1] == "upload": release["assets"].append(asset(Path(args[3]).name))
            elif args[1] == "edit": release["draft"] = False
            else: self.fail("Unexpected GitHub mutation: " + str(args))
        class Download(io.BytesIO):
            def geturl(self): return "https://release-assets.githubusercontent.com/verified"
        def download(url, **kwargs): return Download((directory / url.rsplit("/", 1)[1]).read_bytes())
        with patch.object(publish, "release_for_tag", side_effect=lambda _: copy.deepcopy(release)), \
                patch.object(publish, "verify_tag", return_value=True), \
                patch.object(publish, "gh", side_effect=gh), \
                patch.object(publish.urllib.request, "urlopen", side_effect=download):
            publish.publish_github(directory, info, sums)
            self.assertFalse(release["draft"])
            self.assertEqual(sum(args[1] == "upload" for args in commands), len(hashes) - 1)
            commands.clear()
            publish.publish_github(directory, info, sums)
            self.assertEqual(commands, [])

    def test_stable_notes_describe_the_windows_zip_and_retry_changes_nothing(self):
        for windows in (True, False):
            with self.subTest(windows=windows):
                directory = self.candidate(windows=windows, macos=True, dmg=True, name=f"notes-{windows}")
                info, sums = common.validate_candidate(directory, VERSION)
                hashes = {**sums, "SHA256SUMS": common.sha256(directory / "SHA256SUMS")}
                release = {"draft": True, "tag_name": "v" + VERSION, "target_commitish": COMMIT, "assets": []}
                state = {"created": False}
                commands = []
                def lookup(_):
                    return copy.deepcopy(release) if state["created"] else None
                def gh(*args):
                    commands.append(args)
                    if args[1] == "create":
                        state["created"] = True
                        commands.append(Path(args[args.index("--notes-file") + 1]).read_text())
                    elif args[1] == "upload":
                        name = Path(args[3]).name
                        release["assets"].append({"name": name, "digest": "sha256:" + hashes[name],
                                                  "size": (directory / name).stat().st_size})
                    elif args[1] == "edit":
                        release["draft"] = False
                class Download(io.BytesIO):
                    def geturl(self): return "https://release-assets.githubusercontent.com/verified"
                with patch.object(publish, "release_for_tag", side_effect=lookup), \
                        patch.object(publish, "verify_tag", return_value=True), \
                        patch.object(publish, "gh", side_effect=gh), \
                        patch.object(publish.urllib.request, "urlopen",
                                     side_effect=lambda url, **kw: Download((directory / url.rsplit("/", 1)[1]).read_bytes())):
                    publish.publish_github(directory, info, sums)
                    notes = commands[1]
                    self.assertNotIn("--prerelease", commands[0])
                    self.assertEqual(f"`zerus-{VERSION}-windows-x86_64.zip`" in notes, windows)
                    self.assertEqual("hgs-tray.exe" in notes, windows)
                    self.assertIn("brew install --cask ufna/zerus/zerus`", notes)
                    self.assertIn(f"/releases/download/v{VERSION}/zerus-{VERSION}-macos-arm64.dmg", notes)
                    self.assertIn("drag Zerus into Applications", notes)
                    uploaded = {Path(args[3]).name for args in commands if isinstance(args, tuple) and args[1] == "upload"}
                    self.assertEqual(uploaded, set(hashes))
                    self.assertIn(("release", "edit", "v" + VERSION, "--repo", common.REPOSITORY, "--draft=false", "--latest"), commands)
                    commands.clear()
                    publish.publish_github(directory, info, sums)
                    self.assertEqual(commands, [])

    def test_a_refused_draft_names_the_owner_path_and_writes_nothing(self):
        # zerus-ktg3: GitHub once refused the job token's draft (HTTP 403).
        directory = self.candidate(windows=True, name="refused")
        info, sums = common.validate_candidate(directory, VERSION)
        commands = []
        def gh(*args):
            commands.append(args)
            if args[1] == "create":
                raise subprocess.CalledProcessError(1, ["gh", *args])
            self.fail("Unexpected GitHub mutation: " + str(args))
        with patch.object(publish, "release_for_tag", return_value=None), \
                patch.object(publish, "verify_tag", return_value=True), \
                patch.object(publish, "gh", side_effect=gh), \
                patch("sys.stderr", new_callable=io.StringIO) as stderr:
            with self.assertRaises(ValueError) as raised:
                publish.publish_github(directory, info, sums)
        message = str(raised.exception)
        self.assertIn(f"gh release create v{VERSION} --repo {common.REPOSITORY} --draft --target {COMMIT}", message)
        self.assertIn(f"--title 'Zerus {VERSION}'", message)
        self.assertIn("Rerun this publisher for the same candidate", message)
        self.assertEqual([args[1] for args in commands], ["create"])
        # The printed notes are the ones the draft would have carried.
        self.assertIn(f"Source commit: `{COMMIT}`", stderr.getvalue())
        self.assertIn("hgs-tray.exe", stderr.getvalue())

    def test_new_draft_visibility_delay_retries_reads_without_recreating(self):
        directory = self.candidate()
        info, sums = common.validate_candidate(directory, VERSION)
        hashes = {**sums, "SHA256SUMS": common.sha256(directory / "SHA256SUMS")}
        release = {"draft": True, "tag_name": "v" + VERSION,
                   "target_commitish": COMMIT, "assets": []}
        commands, lookups = [], []
        def lookup(tag):
            lookups.append(tag)
            # Initial absence, then two transient misses after successful create.
            return None if len(lookups) <= 3 else copy.deepcopy(release)
        def gh(*args):
            commands.append(args)
            if args[1] == "create":
                self.assertEqual(Path(args[args.index("--notes-file") + 1]).read_text().count(COMMIT), 1)
            elif args[1] == "upload":
                name = Path(args[3]).name
                release["assets"].append({"name": name, "digest": "sha256:" + hashes[name],
                                          "size": (directory / name).stat().st_size})
            elif args[1] == "edit":
                release["draft"] = False
            else:
                self.fail("Unexpected GitHub mutation: " + str(args))
        class Download(io.BytesIO):
            def geturl(self): return "https://release-assets.githubusercontent.com/verified"
        with patch.object(publish, "release_for_tag", side_effect=lookup), \
                patch.object(publish, "verify_tag", return_value=True), \
                patch.object(publish, "gh", side_effect=gh), \
                patch.object(publish.time, "sleep") as sleep, \
                patch.object(publish.urllib.request, "urlopen", side_effect=lambda url, **kw: Download((directory / url.rsplit("/", 1)[1]).read_bytes())):
            publish.publish_github(directory, info, sums)
            self.assertEqual([call.args[0] for call in sleep.call_args_list], [1, 2])
        self.assertEqual(sum(args[1] == "create" for args in commands), 1)
        self.assertEqual(sum(args[1] == "upload" for args in commands), len(hashes))
        self.assertFalse(release["draft"])

    def test_invisible_new_draft_times_out_without_uploading_or_recreating(self):
        directory = self.candidate()
        info, sums = common.validate_candidate(directory, VERSION)
        with patch.object(publish, "release_for_tag", return_value=None), \
                patch.object(publish, "verify_tag", return_value=False), \
                patch.object(publish, "gh") as mutations, patch.object(publish.time, "sleep") as sleep:
            with self.assertRaisesRegex(ValueError, "same candidate"):
                publish.publish_github(directory, info, sums)
        self.assertEqual(len(mutations.call_args_list), 1)
        self.assertEqual(mutations.call_args.args[:2], ("release", "create"))
        self.assertEqual(sum(call.args[0] for call in sleep.call_args_list), 60)

    def test_new_draft_lookup_api_errors_abort_without_retries(self):
        with patch.object(publish, "release_for_tag", side_effect=ValueError("Cannot inspect GitHub release: HTTP 403")) as lookup, \
                patch.object(publish.time, "sleep") as sleep:
            with self.assertRaisesRegex(ValueError, "HTTP 403"):
                publish.wait_for_created_draft("v" + VERSION)
        lookup.assert_called_once()
        sleep.assert_not_called()

    def test_changed_or_incomplete_public_release_is_never_overwritten(self):
        directory = self.candidate()
        info, sums = common.validate_candidate(directory, VERSION)
        release = {"draft": False, "tag_name": "v" + VERSION, "assets": []}
        with patch.object(publish, "release_for_tag", return_value=release), \
                patch.object(publish, "verify_tag", return_value=True), patch.object(publish, "gh") as mutations:
            with self.assertRaisesRegex(ValueError, "missing assets"):
                publish.publish_github(directory, info, sums)
            mutations.assert_not_called()
        release["assets"] = [{"name": name, "digest": "sha256:" + "0" * 64} for name in [*sums, "SHA256SUMS"]]
        with self.assertRaisesRegex(ValueError, "immutable asset differs"):
            publish.verify_assets(release, directory, {**sums, "SHA256SUMS": common.sha256(directory / "SHA256SUMS")})

    def test_draft_with_moving_target_is_rejected_before_upload(self):
        directory = self.candidate()
        info, sums = common.validate_candidate(directory, VERSION)
        with patch.object(publish, "release_for_tag", return_value={"draft": True, "target_commitish": "main"}), \
                patch.object(publish, "verify_tag", return_value=False), patch.object(publish, "gh") as mutations:
            with self.assertRaisesRegex(ValueError, "moving branch"):
                publish.publish_github(directory, info, sums)
            mutations.assert_not_called()

    def tap_remote(self, files=None):
        return make_tap_remote(self.directory, files)

    def remote_cask(self, remote):
        return remote_file(remote, "Casks/zerus.rb")

    def test_tap_update_is_prepared_without_changing_the_remote(self):
        candidate = self.candidate(macos=True)
        remote = self.tap_remote()
        workspace = self.directory / "work"
        workspace.mkdir()
        directory = publish.prepare_tap(candidate, workspace, None, VERSION, remote=str(remote))
        self.assertEqual((directory / "Casks/zerus.rb").read_text(), (candidate / "homebrew/zerus.rb").read_text())
        self.assertIsNone(self.remote_cask(remote), "only a dry-run push happens before GitHub publication")
        publish.git(directory, "push", "origin", "HEAD:main")
        self.assertEqual(self.remote_cask(remote), (candidate / "homebrew/zerus.rb").read_text())

    def test_tap_retry_with_identical_cask_is_a_no_op(self):
        candidate = self.candidate(macos=True)
        remote = self.tap_remote({"Casks/zerus.rb": (candidate / "homebrew/zerus.rb").read_text()})
        workspace = self.directory / "work"
        workspace.mkdir()
        self.assertIsNone(publish.prepare_tap(candidate, workspace, None, VERSION, remote=str(remote)))

    def test_tap_refuses_downgrades_and_changed_same_version_casks(self):
        candidate = self.candidate(macos=True)
        for existing in (macos_release.render_cask("0.38.0", "e" * 64), macos_release.render_cask(VERSION, "e" * 64)):
            with self.subTest(existing=existing.splitlines()[1]):
                remote_root = self.directory / "remote"
                remote_root.mkdir()
                self.directory, original = remote_root, self.directory
                try:
                    remote = self.tap_remote({"Casks/zerus.rb": existing})
                    workspace = remote_root / "work"
                    workspace.mkdir()
                    with self.assertRaisesRegex(ValueError, "downgrade"):
                        publish.prepare_tap(candidate, workspace, None, VERSION, remote=str(remote))
                    self.assertEqual(self.remote_cask(remote), existing)
                finally:
                    self.directory = original
                    subprocess.run(["rm", "-rf", str(remote_root)], check=True)

    def test_tap_with_unexpected_casks_is_left_for_manual_review(self):
        candidate = self.candidate(macos=True)
        remote = self.tap_remote({"Casks/other.rb": "cask \"other\" do\nend\n"})
        workspace = self.directory / "work"
        workspace.mkdir()
        with self.assertRaisesRegex(ValueError, "Unexpected tap"):
            publish.prepare_tap(candidate, workspace, None, VERSION, remote=str(remote))

    def test_macos_candidate_needs_the_tap_key_before_any_publication(self):
        directory = self.candidate(macos=True)
        arguments = ["publish-release.py", "--candidate", str(directory), "--version", VERSION, "--ssh-key", "/unused/key"]
        with patch.object(sys, "argv", arguments), \
                patch.object(publish, "ssh_environment", return_value=([], {})), \
                patch.object(publish, "prepare_aur") as aur, patch.object(publish, "publish_github") as github:
            with self.assertRaisesRegex(ValueError, "tap deploy key"):
                publish.main()
            aur.assert_not_called()
            github.assert_not_called()

    def test_arch_only_candidate_never_touches_the_tap(self):
        directory = self.candidate()
        arguments = ["publish-release.py", "--candidate", str(directory), "--version", VERSION, "--ssh-key", "/unused/key"]
        with patch.object(sys, "argv", arguments), \
                patch.object(publish, "ssh_environment", return_value=([], {})), \
                patch.object(publish, "prepare_aur", return_value=[]), \
                patch.object(publish, "publish_github"), patch.object(publish, "prepare_tap") as tap:
            publish.main()
            tap.assert_not_called()

    def test_tap_push_follows_github_and_aur_and_failure_is_retryable(self):
        directory = self.candidate(macos=True)
        arguments = ["publish-release.py", "--candidate", str(directory), "--version", VERSION,
                     "--ssh-key", "/unused/key", "--tap-ssh-key", "/unused/tap-key"]
        events = []
        def push(repo, *args, **kwargs):
            events.append(repo.name)
            if repo.name == "homebrew-zerus": raise subprocess.CalledProcessError(1, "git push")
        plans = [(name, self.directory / name) for name in common.PACKAGES]
        with patch.object(sys, "argv", arguments), \
                patch.object(publish, "ssh_environment", return_value=([], {})) as ssh, \
                patch.object(publish, "prepare_aur", return_value=plans), \
                patch.object(publish, "prepare_tap", return_value=self.directory / "homebrew-zerus"), \
                patch.object(publish, "publish_github", side_effect=lambda *args: events.append("GitHub")), \
                patch.object(publish, "git", side_effect=push):
            with self.assertRaisesRegex(SystemExit, "same candidate"):
                publish.main()
        self.assertEqual(events, ["GitHub", *common.PACKAGES, "homebrew-zerus"])
        self.assertEqual(ssh.call_args_list[1].args[2], publish.GITHUB_FINGERPRINT)

    def test_aur_failure_precedes_github_and_partial_push_is_retryable(self):
        directory = self.candidate()
        arguments = ["publish-release.py", "--candidate", str(directory), "--version", VERSION,
                     "--ssh-key", "/unused/key"]
        with patch.object(sys, "argv", arguments), \
                patch.object(publish, "ssh_environment", return_value=([], {})), \
                patch.object(publish, "prepare_aur", side_effect=ValueError("AUR authentication failed")), \
                patch.object(publish, "publish_github") as github:
            with self.assertRaisesRegex(ValueError, "authentication"):
                publish.main()
            github.assert_not_called()
        events = []
        def push(repo, *args, **kwargs):
            events.append(repo.name)
            if repo.name == "zerus-git": raise subprocess.CalledProcessError(1, "git push")
        plans = [(name, self.directory / name) for name in common.PACKAGES]
        with patch.object(sys, "argv", arguments), \
                patch.object(publish, "ssh_environment", return_value=([], {})), \
                patch.object(publish, "prepare_aur", return_value=plans), \
                patch.object(publish, "publish_github", side_effect=lambda *args: events.append("GitHub")), \
                patch.object(publish, "git", side_effect=push):
            with self.assertRaisesRegex(SystemExit, "same candidate"):
                publish.main()
        self.assertEqual(events, ["GitHub", *common.PACKAGES])


if __name__ == "__main__":
    unittest.main()
