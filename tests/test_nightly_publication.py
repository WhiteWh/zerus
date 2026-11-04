"""Contracts for trusted nightly provenance, immutable assets and AUR upgrades."""
import copy
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from homebrew_tap_fixture import make_tap_remote, remote_file
from windows_portable_fixture import pinned_pair, portable_zip
import conpty_pair
import macos_release
import nightly_common as nightly
import release_common as common
import windows_portable
import windows_installer_release
from windows_installer_fixture import installer_files


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


fetch = load("nightly_fetch", "fetch-release-candidate.py")
publish = load("nightly_publish", "publish-release.py")
prepare = load("nightly_prepare", "prepare-nightly.py")
COMMIT = "a" * 40
TAG = "nightly-123"
MACOS = {"qt_version": "6.11.2", "team_id": "ABCDE12345",
         "notarization_id": "0123abcd-4567-89ef-0123-456789abcdef", "minimum_system_version": "14.0"}


class Download:
    """An anonymous release download served from the local candidate."""
    def __init__(self, path): self.stream = path.open("rb")
    def __enter__(self): return self
    def __exit__(self, *args): self.stream.close()
    def geturl(self): return "https://example.com/asset"
    def readable(self): return True
    def read(self, *args): return self.stream.read(*args)
    def readinto(self, buffer): return self.stream.readinto(buffer)


class NightlyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zerus-nightly-contract-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        # The synthetic zips' ConPTY pair passes as the pinned build.
        pin = patch.object(conpty_pair, "PIN", pinned_pair())
        pin.start()
        self.addCleanup(pin.stop)
        self.info = {"schema": 1, "channel": "nightly", "publication": "candidate",
                     "product_version": "0.37.0", "revision_count": 60, "source_commit": COMMIT,
                     "workflow_run_id": 123, "workflow_run_number": 7, "release_tag": "nightly-123",
                     "version": nightly.nightly_version("0.37.0", 60, COMMIT, 7),
                     "architecture": "x86_64", "distribution": "Arch Linux", "native_agents_bundled": False,
                     "binary_dependencies": {name: "1.2.3" for name in common.BINARY_LIBRARIES}}

    def candidate(self, windows=False, name="candidate", macos=False, dmg=False, **zip_options):
        directory = self.directory / name
        directory.mkdir()
        version = self.info["version"]
        if windows:
            self.info["windows_portable"] = f"zerus-{version}-windows-x86_64.zip"
            portable_zip(directory / self.info["windows_portable"], **zip_options)
        binary = directory / f"zerus-{version}-arch-x86_64.tar.gz"
        binary.write_bytes(b"verified binary")
        (directory / f"{nightly.PACKAGE}-{version}-1-x86_64.pkg.tar.zst").write_bytes(b"checked package")
        if macos:
            macos_release.add_macos(directory, self.signed_macos(dmg=dmg), version, self.info, "nightly")
        (directory / "release-info.json").write_text(json.dumps(self.info))
        recipe = directory / "aur" / nightly.PACKAGE
        recipe.mkdir(parents=True)
        digest = common.sha256(binary)
        (recipe / "PKGBUILD").write_text(nightly.render_recipe(self.info, digest))
        (recipe / ".SRCINFO").write_text(
            f"pkgbase = {nightly.PACKAGE}\n\tpkgver = {version}\n\tpkgrel = 1\n\tarch = x86_64\n"
            f"\tsource = https://github.com/ufna/zerus/releases/download/nightly-123/{binary.name}\n"
            f"\tsha256sums = {digest}\n" +
            "".join(f"\tdepends = {name}>=1.2.3\n" for name in self.info["binary_dependencies"]) +
            "".join(f"\tdepends = {name}\n" for name in ("tmux>=3.7", "openssh", "python", "curl", "procps-ng", "hicolor-icon-theme", "bash", "tar")) +
            "".join(f"\tconflicts = {name}\n" for name in ("zerus", "zerus-git", "zerus-ade-bin")) +
            f"\tprovides = zerus={version}\n\tprovides = hgs\n\tprovides = hgs-tray\npkgname = {nightly.PACKAGE}\n")
        common.finish_candidate(directory, nightly.PACKAGES)
        return directory

    def signed_macos(self, version=None, dmg=False):
        directory = self.directory / "macos-signed"
        directory.mkdir()
        asset = macos_release.macos_asset(version or self.info["version"])
        (directory / asset).write_bytes(b"notarized nightly application")
        info = dict(MACOS, asset=asset)
        if dmg:
            image = macos_release.macos_dmg(version or self.info["version"])
            (directory / image).write_bytes(b"notarized nightly disk image")
            info["dmg"] = {"asset": image, "notarization_id": "1123abcd-4567-89ef-0123-456789abcdef"}
        (directory / "macos-info.json").write_text(json.dumps(info))
        return directory

    def test_nightly_cask_follows_the_immutable_prerelease(self):
        version = self.info["version"]
        text = macos_release.render_cask(version, "c" * 64, "nightly", TAG)
        self.assertTrue(text.startswith('cask "zerus@nightly" do\n'))
        self.assertIn(f'version "{version}"', text)
        self.assertIn("releases/download/nightly-123/zerus-#{version}-macos-arm64.zip", text)
        self.assertIn('conflicts_with cask: ["zerus", "zerus@dev"]\n', text)
        self.assertEqual(macos_release.cask_fields(text), {"version": version, "sha256": "c" * 64})
        self.assertNotRegex(text, r"@[A-Z_]+@", "template markers must all be replaced")
        for stanza in ("preflight", "postflight", "zap", "uninstall", "uninstall_preflight", "launchctl", "quit", "auto_updates"):
            self.assertNotRegex(text, rf"(?m)^  {stanza}\b", "upgrades must not quit the GUI or unload agents")
        for line in ("  depends_on arch: :arm64", '  app "Zerus.app"'):
            self.assertIn(line, text, "stable and nightly install the same application")
        with self.assertRaisesRegex(ValueError, "version"):
            macos_release.render_cask("0.37.0", "c" * 64, "nightly", TAG)
        with self.assertRaisesRegex(ValueError, "version"):
            macos_release.render_cask(version, "c" * 64)
        with self.assertRaisesRegex(ValueError, "tag"):
            macos_release.render_cask(version, "c" * 64, "nightly", "v0.37.0")

    def test_nightly_cask_versions_compare_by_build_number(self):
        def key(product, count, commit, number):
            return macos_release.cask_version_key(nightly.nightly_version(product, count, commit, number), "nightly")
        self.assertLess(key("0.37.0", 60, COMMIT, 7), key("0.37.0", 60, COMMIT, 8))
        self.assertLess(key("0.38.0", 70, "b" * 40, 9), key("0.37.0", 60, COMMIT, 10))
        with self.assertRaisesRegex(ValueError, "version"):
            macos_release.cask_version_key("0.37.0", "nightly")

    def test_macos_nightly_seals_its_zip_and_cask(self):
        directory = self.candidate(macos=True)
        info, sums = nightly.validate_nightly(directory)
        self.assertEqual((info["schema"], info["homebrew_casks"]), (2, ["zerus@nightly"]))
        self.assertIn(macos_release.macos_asset(info["version"]), sums)
        self.assertEqual(sorted(path.name for path in (directory / "homebrew").iterdir()), ["zerus@nightly.rb"])
        cask = directory / "homebrew/zerus@nightly.rb"
        original = cask.read_text()
        for text in (original + "# edited\n", original.replace(TAG, "nightly-124")):
            cask.write_text(text)
            with self.assertRaisesRegex(ValueError, "template"):
                nightly.validate_nightly(directory)
        cask.write_text(original)
        info = json.loads((directory / "release-info.json").read_text())
        info["homebrew_casks"] = ["zerus"]
        (directory / "release-info.json").write_text(json.dumps(info))
        with self.assertRaisesRegex(ValueError, "cask family"):
            nightly.validate_nightly(directory)

    def test_nightly_without_macos_cannot_carry_a_cask(self):
        directory = self.candidate()
        (directory / "homebrew").mkdir()
        (directory / "homebrew/zerus@nightly.rb").write_text(
            macos_release.render_cask(self.info["version"], "c" * 64, "nightly", TAG))
        with self.assertRaisesRegex(ValueError, "Unsealed"):
            nightly.validate_nightly(directory)

    def test_sealed_previous_cask_validates_unchanged_but_new_casks_use_the_current_template(self):
        directory = self.candidate(macos=True, dmg=True)
        version = self.info["version"]
        cask = directory / "homebrew/zerus@nightly.rb"
        digest = common.sha256(directory / self.info["macos"]["asset"])
        previous = (ROOT / "packaging/homebrew/previous/zerus@nightly.rb.in").read_text().replace(
            "@VERSION@", version).replace("@SHA256@", digest).replace("@RELEASE_TAG@", TAG)
        cask.write_text(previous)
        before = {path.relative_to(directory): path.read_bytes() for path in directory.rglob("*") if path.is_file()}
        nightly.validate_nightly(directory)
        self.assertEqual(before, {path.relative_to(directory): path.read_bytes()
                                  for path in directory.rglob("*") if path.is_file()})
        with self.assertRaisesRegex(ValueError, "template"):
            macos_release.require_template_cask(previous, version, digest, "nightly", TAG)
        self.assertNotIn("  zap ", macos_release.render_cask(version, digest, "nightly", TAG))
        for edited in (previous + "# edited\n", previous.replace(digest, "d" * 64),
                       previous.replace(TAG, "nightly-124"), previous.replace(version, "0.37.0.r60.gaaaaaaa.n8"),
                       previous.replace("com.hgdev.hgs-recovery", "com.example.foreign"),
                       previous.replace('conflicts_with cask: "zerus"',
                                        'conflicts_with cask: ["zerus", "zerus@dev"]')):
            with self.subTest(edited=edited):
                cask.write_text(edited)
                with self.assertRaisesRegex(ValueError, "template"):
                    nightly.validate_nightly(directory)

    def test_nightly_disk_image_is_sealed_beside_the_cask_zip(self):
        directory = self.candidate(macos=True, dmg=True)
        info, sums = nightly.validate_nightly(directory)
        image = directory / info["macos"]["dmg"]["asset"]
        self.assertEqual(sums[image.name], common.sha256(image))
        self.assertIn(info["macos"]["asset"], sums)
        image.unlink()
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            nightly.validate_nightly(directory)

    def tap(self, files):
        root = self.directory / f"tap-{len(list(self.directory.glob('tap-*')))}"
        root.mkdir()
        workspace = root / "work"
        workspace.mkdir()
        return make_tap_remote(root, files), workspace

    def test_nightly_tap_update_keeps_the_stable_cask(self):
        candidate = self.candidate(macos=True)
        version = self.info["version"]
        stable = macos_release.render_cask("0.38.0", "e" * 64)
        remote, workspace = self.tap({"Casks/zerus.rb": stable})
        directory = publish.prepare_tap(candidate, workspace, None, version, "nightly", remote=str(remote))
        self.assertIsNone(remote_file(remote, "Casks/zerus@nightly.rb"), "only a dry-run push happens before GitHub")
        publish.git(directory, "push", "origin", "HEAD:main")
        self.assertEqual(remote_file(remote, "Casks/zerus@nightly.rb"),
                         (candidate / "homebrew/zerus@nightly.rb").read_text())
        self.assertEqual(remote_file(remote, "Casks/zerus.rb"), stable)
        remote, workspace = self.tap({"Casks/zerus@nightly.rb": (candidate / "homebrew/zerus@nightly.rb").read_text()})
        self.assertIsNone(publish.prepare_tap(candidate, workspace, None, version, "nightly", remote=str(remote)))
        newer = nightly.nightly_version("0.37.0", 61, "b" * 40, 8)
        for existing in (macos_release.render_cask(newer, "e" * 64, "nightly", "nightly-200"),
                         macos_release.render_cask(version, "e" * 64, "nightly", TAG)):
            remote, workspace = self.tap({"Casks/zerus@nightly.rb": existing})
            with self.assertRaisesRegex(ValueError, "downgrade"):
                publish.prepare_tap(candidate, workspace, None, version, "nightly", remote=str(remote))
            self.assertEqual(remote_file(remote, "Casks/zerus@nightly.rb"), existing)

    def test_macos_nightly_publishes_the_tap_after_github_and_aur(self):
        directory = self.candidate(macos=True)
        arguments = ["publish-release.py", "--nightly", "--candidate", str(directory), "--ssh-key", "/unused/key"]
        with patch.object(sys, "argv", arguments), patch.object(publish, "ssh_environment", return_value=([], {})), \
                patch.object(publish, "prepare_aur") as aur, patch.object(publish, "publish_github") as github:
            with self.assertRaisesRegex(ValueError, "tap deploy key"):
                publish.main()
            aur.assert_not_called()
            github.assert_not_called()
        events = []
        def push(repo, *args, **kwargs):
            events.append(repo.name)
        tap_calls = []
        def prepare_tap(*args, **kwargs):
            tap_calls.append(args)
            return self.directory / "homebrew-zerus"
        with patch.object(sys, "argv", arguments + ["--tap-ssh-key", "/unused/tap-key"]), \
                patch.object(publish, "ssh_environment", return_value=([], {})), \
                patch.object(publish, "prepare_aur", return_value=[(nightly.PACKAGE, self.directory / nightly.PACKAGE)]), \
                patch.object(publish, "prepare_tap", side_effect=prepare_tap), \
                patch.object(publish, "publish_github", side_effect=lambda *args, **kwargs: events.append("GitHub")), \
                patch.object(publish, "git", side_effect=push):
            publish.main()
        self.assertEqual(events, ["GitHub", nightly.PACKAGE, "homebrew-zerus"])
        self.assertEqual(tap_calls[0][3:], (self.info["version"], "nightly"))

    def test_only_successful_upstream_scheduled_or_manual_runs_are_trusted(self):
        run = {"path": ".github/workflows/nightly.yml", "event": "schedule", "head_branch": "main",
               "head_repository": {"full_name": "ufna/zerus"}, "status": "completed",
               "conclusion": "success", "head_sha": COMMIT}
        self.assertEqual(fetch.validate_run(run, nightly=True), COMMIT)
        for updates in ({"event": "pull_request"}, {"path": ".github/workflows/ci.yml"},
                        {"head_branch": "feature"}, {"conclusion": "failure"},
                        {"status": "in_progress"}, {"head_repository": {"full_name": "example/zerus"}}):
            with self.subTest(updates=updates), self.assertRaises(ValueError):
                fetch.validate_run({**run, **updates}, nightly=True)

    def test_exact_commit_run_and_build_number_are_required(self):
        directory = self.candidate()
        info, hashes = nightly.validate_nightly(directory, COMMIT, 123, 7)
        self.assertEqual(info["release_tag"], "nightly-123")
        self.assertEqual(len(hashes), 4)
        for commit, run, number in (("b" * 40, 123, 7), (COMMIT, 124, 7), (COMMIT, 123, 8)):
            with self.assertRaises(ValueError):
                nightly.validate_nightly(directory, commit, run, number)

    def test_asset_and_recipe_tampering_fail_before_publication(self):
        directory = self.candidate()
        path = directory / "aur" / nightly.PACKAGE / "PKGBUILD"
        path.write_text(path.read_text() + "\nmalicious_command\n")
        with self.assertRaisesRegex(ValueError, "Unsealed nightly recipe"):
            nightly.validate_nightly(directory)
        common.finish_candidate(directory, nightly.PACKAGES)
        with self.assertRaisesRegex(ValueError, "trusted binary template"):
            nightly.validate_nightly(directory)

    def test_sealed_legacy_description_remains_valid_without_relaxing_the_template(self):
        directory = self.candidate()
        path = directory / "aur" / nightly.PACKAGE / "PKGBUILD"
        previous, current = common.DESCRIPTION_UPDATES[1]
        original = path.read_text()
        path.write_text(original.replace(current, previous))
        common.finish_candidate(directory, nightly.PACKAGES)
        nightly.validate_nightly(directory, COMMIT, 123, 7)
        for replacement in (original.replace(current, "Unapproved description"),
                            original.replace(current, previous).replace("tmux>=3.7", "tmux>=3.4"),
                            original.replace(current, previous) + "\nunreviewed_command\n"):
            path.write_text(replacement)
            common.finish_candidate(directory, nightly.PACKAGES)
            with self.assertRaisesRegex(ValueError, "trusted binary template"):
                nightly.validate_nightly(directory)

    def test_legacy_nightly_with_required_qt_webengine_still_validates(self):
        # Sealed before Qt WebEngine became optional: the exact earlier recipe.
        self.info["binary_dependencies"] = {name: "1.2.3" for name in common.LEGACY_BINARY_LIBRARIES}
        directory = self.candidate()
        recipe = (directory / "aur" / nightly.PACKAGE / "PKGBUILD").read_text()
        self.assertIn("'qt6-base>=1.2.3' 'qt6-webengine>=1.2.3' 'qt6-multimedia>=1.2.3'", recipe)
        self.assertNotIn(common.OPTIONAL_WEBENGINE, recipe)
        nightly.validate_nightly(directory, COMMIT, 123, 7)

    def test_next_nightly_has_no_qt_and_previous_optional_recipe_is_retryable(self):
        recipe = nightly.render_recipe(self.info, "0" * 64)
        self.assertNotIn("qt6-", recipe)
        self.assertIn("'webkit2gtk-4.1>=", recipe)
        nightly.validate_nightly(self.candidate(), COMMIT, 123, 7)
        self.info["binary_dependencies"] = {name: "1.2.3" for name in common.QT_BINARY_LIBRARIES}
        recipe = nightly.render_recipe(self.info, "0" * 64)
        self.assertIn(common.OPTIONAL_WEBENGINE, recipe)
        self.assertNotIn("gst-plugins-good", recipe)
        nightly.validate_nightly(self.candidate(name="previous-qt"), COMMIT, 123, 7)

    def test_moving_url_missing_floors_and_extra_files_are_rejected(self):
        directory = self.candidate()
        path = directory / "aur" / nightly.PACKAGE / ".SRCINFO"
        original = path.read_text()
        for replacement in (original.replace("nightly-123", "nightly"),
                            original.replace("depends = webkit2gtk-4.1>=1.2.3", "depends = webkit2gtk-4.1")):
            path.write_text(replacement)
            common.finish_candidate(directory, nightly.PACKAGES)
            with self.assertRaises(ValueError):
                nightly.validate_nightly(directory)
        path.write_text(original)
        common.finish_candidate(directory, nightly.PACKAGES)
        (directory / "unsealed.txt").write_text("unexpected")
        with self.assertRaisesRegex(ValueError, "Unsealed/missing"):
            nightly.validate_nightly(directory)

    def prepare(self, portable, output):
        """Run prepare-nightly.py with the source revision and Arch assembly replaced by fixtures."""
        product = (ROOT / "VERSION").read_text().strip()
        def run(command, **kwargs):
            verified = Path(command[command.index("--output") + 1])
            verified.mkdir(parents=True)
            (verified / f"zerus-{product}-arch-x86_64.tar.gz").write_bytes(b"verified binary")
            (verified / "release-info.json").write_text(json.dumps(
                {key: value for key, value in self.info.items() if key not in {"version", "product_version"}}))
        def check_output(command, **kwargs):
            return {"rev-parse": COMMIT + "\n", "rev-list": "60\n"}[command[1]]
        installer, build = installer_files(self.directory, portable,
            nightly.nightly_version(product, 60, COMMIT, 7), product, COMMIT)
        arguments = ["prepare-nightly.py", "--package", "arch.pkg.tar.zst", "--windows-zip", str(portable),
                     "--output", str(output), "--run-id", "123", "--run-number", "7"]
        arguments += ["--windows-installer", str(installer), "--windows-installer-info", str(build)]
        with patch.object(prepare.subprocess, "run", side_effect=run), \
                patch.object(prepare.subprocess, "check_output", side_effect=check_output), \
                patch.object(sys, "argv", arguments):
            prepare.main()

    def test_prepare_copies_the_tested_windows_zip_unchanged(self):
        product = (ROOT / "VERSION").read_text().strip()
        portable = portable_zip(self.directory / "zerus-windows-x86_64.zip", version=product)
        output = self.directory / "dist"
        self.prepare(portable, output)
        info = json.loads((output / "release-info.json").read_text())
        self.assertEqual(info["windows_portable"], f"zerus-{info['version']}-windows-x86_64.zip")
        self.assertEqual((output / info["windows_portable"]).read_bytes(), portable.read_bytes())
        setup = output / info["windows_installer"]["asset"]
        self.assertTrue(setup.is_file())
        self.assertEqual(info["windows_installer"]["portable_sha256"], common.sha256(portable))
        for name, options in (("commit", {"commit": "b" * 40}), ("version", {"version": "0.0.1"}),
                              ("layout", {"version": product, "entries": [("outside", "x")]})):
            with self.subTest(mismatch=name), self.assertRaises(ValueError):
                self.prepare(portable_zip(self.directory / f"{name}.zip", **options), self.directory / name)
            self.assertFalse((self.directory / name).exists())

    def test_rebuild_of_same_commit_upgrades_without_changing_stable_version(self):
        first = nightly.nightly_version("0.37.0", 60, COMMIT, 7)
        next_build = nightly.nightly_version("0.37.0", 60, COMMIT, 8)
        def version(value): return common.package_version({"pkgver": [value], "pkgrel": ["1"]})
        self.assertGreater(version(next_build), version(first))
        self.assertGreater(version(nightly.nightly_version("0.37.0", 61, "b" * 40, 9)), version(next_build))
        self.assertGreater(version("0.38.0"), version(next_build))

    def test_windows_portable_zip_is_sealed_with_its_exact_commit_and_version(self):
        directory = self.candidate(windows=True)
        info, hashes = nightly.validate_nightly(directory, COMMIT, 123, 7)
        name = info["windows_portable"]
        self.assertEqual(name, f"zerus-{info['version']}-windows-x86_64.zip")
        self.assertEqual(len(hashes), 5)
        self.assertEqual(hashes[name], common.sha256(directory / name))
        (directory / name).write_bytes((directory / name).read_bytes() + b"changed")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            nightly.validate_nightly(directory)

    def test_windows_portable_zip_and_manifest_must_agree(self):
        directory = self.candidate(windows=True)
        info = json.loads((directory / "release-info.json").read_text())
        # A zip the manifest does not name is never published unnoticed.
        (directory / "release-info.json").write_text(json.dumps({key: value for key, value in info.items()
                                                                 if key != "windows_portable"}))
        common.finish_candidate(directory, nightly.PACKAGES)
        with self.assertRaisesRegex(ValueError, "Unexpected/duplicate"):
            nightly.validate_nightly(directory)
        # A named zip that is missing, or named differently, fails too.
        (directory / "release-info.json").write_text(json.dumps(info))
        zip_path = directory / info["windows_portable"]
        zip_bytes = zip_path.read_bytes()
        zip_path.unlink()
        common.finish_candidate(directory, nightly.PACKAGES)
        with self.assertRaisesRegex(ValueError, "Unsealed/missing"):
            nightly.validate_nightly(directory)
        zip_path.write_bytes(zip_bytes)
        (directory / "release-info.json").write_text(json.dumps({**info, "windows_portable": "zerus-windows.zip"}))
        common.finish_candidate(directory, nightly.PACKAGES)
        with self.assertRaisesRegex(ValueError, "Invalid Windows nightly asset"):
            nightly.validate_nightly(directory)

    def test_windows_portable_zip_from_another_build_or_unsafe_is_rejected(self):
        for index, (options, message) in enumerate([
                ({"commit": "b" * 40}, "another commit/version"),
                ({"version": "0.36.0"}, "another commit/version"),
                ({"entries": [(f"{windows_portable.TOP}/../outside", "x")]}, "Unsafe Windows portable entry"),
                ({"entries": [("other/hgs.exe", "x")]}, "Unsafe Windows portable entry")]):
            self.info.pop("windows_portable", None)
            directory = self.candidate(windows=True, name=f"candidate-{index}", **options)
            with self.subTest(options=options), self.assertRaisesRegex(ValueError, message):
                nightly.validate_nightly(directory)

    def test_nightly_notes_describe_the_windows_zip_and_stay_prerelease(self):
        directory = self.candidate(windows=True, macos=True, dmg=True)
        info, sums = nightly.validate_nightly(directory)
        hashes = {**sums, "SHA256SUMS": common.sha256(directory / "SHA256SUMS")}
        draft = {"draft": True, "target_commitish": COMMIT, "tag_name": "nightly-123",
                 "assets": [{"name": name, "size": (directory / name).stat().st_size, "digest": "sha256:" + digest}
                            for name, digest in hashes.items()]}
        releases = [None]
        commands = []
        def gh(*args):
            commands.append(args)
            if args[:2] == ("release", "create"):
                commands.append(Path(args[args.index("--notes-file") + 1]).read_text())
            if args[:2] == ("release", "edit"):
                draft["draft"] = False
            return ""
        def release_for_tag(_):
            return releases.pop(0) if releases else copy.deepcopy(draft)
        with patch.object(publish, "release_for_tag", side_effect=release_for_tag), \
                patch.object(publish, "verify_tag", return_value=True), patch.object(publish, "gh", side_effect=gh), \
                patch.object(publish.urllib.request, "urlopen", side_effect=lambda url, **kw: Download(directory / url.rsplit("/", 1)[1])):
            publish.publish_github(directory, info, sums, nightly=True)
        create = commands[0]
        self.assertIn("--prerelease", create)
        self.assertIn("--latest=false", create)
        self.assertIn(info["windows_portable"], commands[1])
        self.assertIn("hgs-tray.exe", commands[1])
        self.assertIn("brew install --cask ufna/zerus/zerus@nightly`", commands[1])
        self.assertIn(f"/releases/download/{TAG}/{info['macos']['dmg']['asset']}", commands[1])

    def test_nightly_never_becomes_latest_stable_and_retry_changes_nothing(self):
        for windows in (False, True):
            with self.subTest(windows=windows):
                self.info.pop("windows_portable", None)
                self.retry_changes_nothing(self.candidate(windows=windows, name=f"retry-{windows}"))

    def retry_changes_nothing(self, directory):
        info, sums = nightly.validate_nightly(directory)
        release = {"draft": True, "target_commitish": COMMIT, "tag_name": "nightly-123",
                   "assets": [{"name": name, "size": (directory / name).stat().st_size,
                               "digest": "sha256:" + digest}
                              for name, digest in {**sums, "SHA256SUMS": common.sha256(directory / "SHA256SUMS")}.items()]}
        commands = []
        def gh(*args):
            commands.append(args)
            if args[:2] == ("release", "edit"): release["draft"] = False
        with patch.object(publish, "release_for_tag", side_effect=lambda _: copy.deepcopy(release)), \
                patch.object(publish, "verify_tag", return_value=True), patch.object(publish, "gh", side_effect=gh), \
                patch.object(publish.urllib.request, "urlopen", side_effect=lambda url, **kw: Download(directory / url.rsplit("/", 1)[1])):
            publish.publish_github(directory, info, sums, nightly=True)
            self.assertEqual(len(commands), 1)
            self.assertIn("--latest=false", commands[0])
            self.assertIn("--prerelease", commands[0])
            commands.clear()
            publish.publish_github(directory, info, sums, nightly=True)
            self.assertEqual(commands, [])


if __name__ == "__main__":
    unittest.main()
