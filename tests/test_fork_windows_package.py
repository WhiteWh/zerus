"""WhiteWh fork, task #65 (owner, 2026-11-06): the fork's Windows zip ships the
Tauri app as hgs-tray.exe with the fork's identity and feed. Task #388 (owner,
2026-11-08, "forget Qt"): it carries no Qt at all; the one-release Qt fallback
(qt/zerus-qt.exe) is retired, and its build runs no Qt step."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import zipfile

import test_windows_publication as publication
from test_windows_publication import PAIR, bundle, manifest, pinned, portable, use_pin

ROOT = Path(__file__).resolve().parents[1]
FEED = "https://github.com/WhiteWh/zerus/releases/latest/download/zerus-fork-feed.json"


class ForkInspectTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zerus-fork-zip-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        use_pin(self, pinned())
        self.files = {name: PAIR.get(name, b"x") for name in portable.NEXT_REQUIRED}
        self.info = manifest(desktop="tauri", webview2_runtime="evergreen", mobile_connector_bundled=True,
                             fork={"repository": "WhiteWh/zerus", "build": 41, "feed": FEED})
        del self.info["qt_version"]

    def test_a_fork_zip_without_qt_is_accepted(self):
        self.assertEqual(portable.inspect(bundle(self.directory / "ok.zip", self.files, self.info)), self.info)

    def test_the_retired_qt_fallback_is_refused(self):
        # Task #388: neither the declaration nor a qt/ folder of the old fallback.
        fallback = {f"qt/{name}": b"Qt" for name in ["zerus-qt.exe", "Qt6Core.dll", "platforms/qwindows.dll"]}
        declared = {**self.info, "qt_fallback": {"executable": "qt/zerus-qt.exe", "qt_version": "6.10.3"}}
        for files, info in [({**self.files, **fallback}, declared), (self.files, declared),
                            ({**self.files, "qt/readme.txt": b"x"}, self.info)]:
            extra = sorted(set(files) - set(self.files))
            with self.subTest(extra=extra, declared="qt_fallback" in info):
                with self.assertRaisesRegex(ValueError, "no Qt fallback"):
                    portable.inspect(bundle(self.directory / "fallback.zip", files, info))

    def test_qt_files_are_refused_anywhere(self):
        for name in ["Qt6Core.dll", "platforms/qwindows.dll", "licenses/qt/LGPL-3.0-only.txt", "other/Qt6Gui.dll"]:
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "must not contain Qt"):
                portable.inspect(bundle(self.directory / "outside.zip", {**self.files, name: b"Qt"}, self.info))


class ForkPackageTests(unittest.TestCase):
    setUp = publication.PackageTests.setUp
    run_script = publication.PackageTests.run_script

    def fork(self, output, *extra):
        arguments = ["--connector", self.directory / "zerus-mobile-connector.exe", "--cli", self.directory / "hgs.exe",
                     "--lore", self.directory / "lore.exe",
                     "--gui", self.directory / "hgs-tray.exe", "--output", output, "--crt-dir", self.crt,
                     "--rust-licenses", self.rust, "--frontend-licenses", self.frontend, "--conpty", self.conpty, *extra]
        environment = {key: value for key, value in os.environ.items() if not key.startswith(("QT_", "HGS_", "ZERUS_FORK_"))}
        environment["SOURCE_DATE_EPOCH"] = "1760000000"
        return self.run_script("package-windows.py", *map(str, arguments), cwd=self.directory, env=environment)

    def test_the_fork_zip_names_its_build_and_feed_and_the_fork_readme(self):
        output = self.directory / "fork.zip"
        result = self.fork(output, "--fork", "--fork-build", "41", "--fork-feed", FEED)
        self.assertEqual(result.returncode, 0, result.stderr)
        info = portable.inspect(output)
        self.assertEqual(info["fork"], {"repository": "WhiteWh/zerus", "build": 41, "feed": FEED})
        self.assertEqual(info["desktop"], "tauri")
        self.assertNotIn("qt_fallback", info)
        with zipfile.ZipFile(output) as archive:
            readme = archive.read(f"{portable.TOP}/README.txt").decode("utf-8")
            self.assertEqual(archive.read(f"{portable.TOP}/hgs-tray.exe"), b"tested tauri gui")
        self.assertIn("WhiteWh fork", readme)
        self.assertIn("41;", readme)
        self.assertIn(FEED, readme)
        self.assertIn("ships only the Tauri desktop", readme)
        with zipfile.ZipFile(output) as archive:
            self.assertFalse(any("/qt/" in name for name in archive.namelist()))
        self.assertNotRegex(readme, r"@[A-Z_]+@")
        self.assertIn("\r\n", readme)
        # An experimental build has neither a number nor a feed.
        experimental = self.directory / "experimental.zip"
        self.assertEqual(self.fork(experimental, "--fork").returncode, 0)
        self.assertEqual(portable.inspect(experimental)["fork"], {"repository": "WhiteWh/zerus", "build": None, "feed": ""})

    def test_a_fork_identity_outside_the_forks_releases_is_refused(self):
        for extra, words in [(["--fork-build", "41", "--fork-feed", "https://example.com/feed.json"], "release asset"),
                             (["--fork-build", "41"], "both a build number and a feed"),
                             (["--fork-build", "-3", "--fork-feed", FEED], "positive number")]:
            with self.subTest(extra=extra):
                output = self.directory / "refused.zip"
                result = self.fork(output, "--fork", *extra)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(words, result.stderr)
                self.assertFalse(output.exists())

    def test_the_packager_takes_no_qt_options(self):
        # Task #388: the Qt fallback options are gone, so a caller cannot add one.
        for option in ["--qt-fallback", "--qt-root", "--windeployqt"]:
            with self.subTest(option=option):
                result = self.fork(self.directory / "qt.zip", "--fork", option, self.directory / "hgs-tray.exe")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("unrecognized arguments", result.stderr)


class ForkExecutorPackageTests(unittest.TestCase):
    """zerus-y0cj: the fork zip ships the model-free artifact executor beside
    hgs.exe with its linked-license notices, README and examples, and declares
    its exact build identity in build-info.json."""
    setUp = publication.PackageTests.setUp
    run_script = publication.PackageTests.run_script
    fork = ForkPackageTests.fork

    def executor(self, binary=b"synthetic executor", attested=None, extra=None):
        import hashlib
        import json
        inventory = b"[]\n"
        build = {"protocol": 1, "desync_revision": "717d63b1756554abc470671ea591ad6d25e6059d",
                 "source_sha256": "a" * 64, "binary_sha256": attested or hashlib.sha256(binary).hexdigest(),
                 "inventory_sha256": hashlib.sha256(inventory).hexdigest()}
        files = {"hgs-executor.exe": binary, "licenses/build.json": json.dumps(build).encode(),
                 "licenses/inventory.json": inventory, "licenses/NOTICES.md": b"notices\n",
                 "licenses/00/LICENSE": b"BSD-3-Clause\n", "README.md": b"# executor\n",
                 "examples/executor.json": b"{}\n", "examples/scenario/run.ps1": b"exit 0\n", **(extra or {})}
        path = self.directory / "hgs-executor-windows-x86_64.zip"
        with zipfile.ZipFile(path, "w") as archive:
            for name, data in files.items():
                archive.writestr("hgs-executor-windows-x86_64/" + name, data)
        return path, build

    def test_the_executor_ships_beside_hgs_with_notices_and_identity(self):
        package, build = self.executor()
        output = self.directory / "fork.zip"
        result = self.fork(output, "--fork", "--fork-build", "41", "--fork-feed", FEED, "--executor", package)
        self.assertEqual(result.returncode, 0, result.stderr)
        info = portable.inspect(output)
        self.assertEqual(info["artifact_executor"], {"binary": "hgs-executor.exe", "binary_sha256": build["binary_sha256"],
                                                     "source_sha256": build["source_sha256"],
                                                     "desync_revision": build["desync_revision"]})
        with zipfile.ZipFile(output) as archive:
            names = set(archive.namelist())
            self.assertEqual(archive.read(f"{portable.TOP}/hgs-executor.exe"), b"synthetic executor")
        for name in ["hgs.exe", "hgs-executor.exe", "licenses/executor/build.json", "licenses/executor/inventory.json",
                     "licenses/executor/NOTICES.md", "licenses/executor/00/LICENSE", "executor/README.md",
                     "executor/examples/executor.json", "executor/examples/scenario/run.ps1"]:
            self.assertIn(f"{portable.TOP}/{name}", names)

    def test_a_changed_executor_or_unexpected_entry_is_refused(self):
        for options, words in [({"attested": "b" * 64}, "differs from its build attestation"),
                               ({"extra": {"identity/identity.key": b"x"}}, "Unexpected executor package entry")]:
            with self.subTest(words=words):
                package = self.executor(**options)[0]
                output = self.directory / "refused.zip"
                result = self.fork(output, "--fork", "--executor", package)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(words, result.stderr)
                self.assertFalse(output.exists())

    def test_only_a_fork_zip_ships_the_executor(self):
        result = self.fork(self.directory / "upstream.zip", "--executor", self.executor()[0])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Only a fork zip ships the artifact executor", result.stderr)

    def test_inspection_refuses_an_undeclared_or_mismatched_executor(self):
        files = {name: PAIR.get(name, b"x") for name in portable.NEXT_REQUIRED}
        info = manifest(desktop="tauri", webview2_runtime="evergreen", mobile_connector_bundled=True,
                        fork={"repository": "WhiteWh/zerus", "build": 41, "feed": FEED})
        del info["qt_version"]
        with self.assertRaisesRegex(ValueError, "needs its artifact_executor declaration"):
            portable.inspect(bundle(self.directory / "undeclared.zip", {**files, "hgs-executor.exe": b"x"}, info))
        declared = {**info, "artifact_executor": {"binary": "hgs-executor.exe", "binary_sha256": "0" * 64,
                                                  "source_sha256": "a" * 64,
                                                  "desync_revision": "717d63b1756554abc470671ea591ad6d25e6059d"}}
        complete = {**files, **{name: b"x" for name in portable.EXECUTOR_REQUIRED}}
        with self.assertRaisesRegex(ValueError, "differs from its declared identity"):
            portable.inspect(bundle(self.directory / "mismatch.zip", complete, declared))
        with self.assertRaisesRegex(ValueError, "artifact executor lacks"):
            portable.inspect(bundle(self.directory / "partial.zip", {**files, "hgs-executor.exe": b"x"}, declared))


class ForkWorkflowTests(unittest.TestCase):
    """Task #446: the manual build packages through the same release candidate as
    the local packages do; task #388: no Qt step and no Qt in the zip."""

    def setUp(self):
        self.text = (ROOT / ".github/workflows/fork-windows.yml").read_text(encoding="utf-8")
        steps = self.text.split("\n      - ")
        self.step = lambda words: next(step for step in steps if words in step)
        self.index = lambda words: next(index for index, step in enumerate(steps) if words in step)

    def test_the_fork_builds_and_packages_through_the_release_candidate(self):
        candidate = self.step("name: Release candidate build, package and self-test")
        self.assertIn('python scripts/release-candidate.py --commit "$COMMIT"', candidate)
        self.assertIn('--fork-build "$ZERUS_FORK_BUILD" --fork-feed "$ZERUS_FORK_FEED_URL"', candidate)
        # The release candidate runs the Tauri build, the package and the
        # self-test through scripts/release-candidate-local.py.
        local = (ROOT / "scripts/release-candidate-local.py").read_text(encoding="utf-8")
        for part in ["npm", "run", "tauri", "--", "build", "--ci", "--no-bundle",
                     "src-tauri/tauri.windows.switch.conf.json", "--features", "video",
                     "package-windows.py", "--fork", "check-windows-portable.py", "--selftest"]:
            self.assertIn(part, local)
        self.assertNotIn("package-windows-qt.py", self.text)
        self.assertNotIn("--qt-fallback", self.text)
        self.assertIn("ZERUS_FORK_BUILD:", self.text)
        self.assertIn("ZERUS_FORK_FEED_URL:", self.text)
        # The tests and the package come before the publication.
        candidate_at = self.index("name: Release candidate build, package and self-test")
        for words in ["name: Rust tests", "name: Latest publication contract tests",
                      "name: WebView2 Evergreen prerequisite"]:
            self.assertLess(self.index(words), candidate_at, words)
        self.assertLess(candidate_at, self.index("name: Update feed"))

    def test_the_fork_builds_and_tests_no_qt(self):
        # Task #388: no Qt install, cache, ccache, CMake build or offscreen suites.
        for words in ["install-qt-windows.py", "QT_ROOT_DIR", "gui.sh", "ccache", "Offscreen Qt suites",
                      "name: Desktop build", "windeployqt"]:
            self.assertNotIn(words, self.text)


class LocalLegForkIdentityTests(unittest.TestCase):
    """Task 213: the local leg takes the fork identity explicitly, so the Tauri
    app embeds what build-info.json names and the portable self-test passes.

    The dry run prints the steps and the identity; nothing builds or packages.
    """

    def dry(self, *extra):
        return subprocess.run([sys.executable, "scripts/release-candidate-local.py", "--dry-run", *extra],
                              cwd=ROOT, capture_output=True, text=True, timeout=60)

    def test_the_local_leg_embeds_and_packages_the_fork_identity(self):
        result = self.dry("--fork-build", "41", "--fork-feed", FEED)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"fork identity: build 41, feed {FEED}", result.stdout)
        self.assertIn(f"tauri fork channel: ZERUS_FORK_BUILD=41 ZERUS_FORK_FEED_URL={FEED}", result.stdout)
        self.assertIn(f"--fork-build 41 --fork-feed {FEED}", result.stdout)
        # An experimental leg (neither) embeds and packages no identity.
        experimental = self.dry()
        self.assertEqual(experimental.returncode, 0, experimental.stderr)
        self.assertIn("fork identity: build none (experimental), feed none (experimental)", experimental.stdout)
        self.assertIn("ZERUS_FORK_BUILD=(unset) ZERUS_FORK_FEED_URL=(unset)", experimental.stdout)
        self.assertIn("--fork-build  --fork-feed ", experimental.stdout)

    def test_one_fork_value_without_the_other_or_a_foreign_feed_is_refused(self):
        for extra, words in [
            (["--fork-build", "41"], "both a build number and a feed"),
            (["--fork-feed", FEED], "both a build number and a feed"),
            (["--fork-build", "41", "--fork-feed", "https://example.com/feed.json"], "release asset"),
            (["--fork-build", "-3", "--fork-feed", FEED], "positive number"),
        ]:
            with self.subTest(extra=extra):
                result = self.dry(*extra)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(words, result.stderr)

    def test_the_local_leg_has_no_qt(self):
        # Task #388: no Qt fallback in the package and no --qt-root.
        result = self.dry("--fork-build", "41", "--fork-feed", FEED)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--qt-fallback", result.stdout)
        self.assertNotIn("--qt-root", result.stdout)
        self.assertIn("check-windows-portable.py artifacts/zerus-windows-x86_64.zip --run --selftest", result.stdout)
        refused = self.dry("--qt-root", "C:/Qt")
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("unrecognized arguments", refused.stderr)


class ReleaseCandidateTests(unittest.TestCase):
    """Task #388: the release candidate runs no Qt configure, build or ctest and
    records each step's seconds with its result."""

    def setUp(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location("release_candidate_388", ROOT / "scripts/release-candidate.py")
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)

    def test_the_local_steps_run_no_qt(self):
        commands = []

        def run(command, env=None, timings=None, step=""):
            commands.append(command)
            timings[step] = 1

        self.module.run = run
        timings = {}
        self.module.local_steps("a" * 40, "41", FEED, timings)
        text = " ".join(" ".join(map(str, command)) for command in commands)
        for words in ["cmake", "ctest", "gui.sh", "tray", "Qt", "QT_"]:
            self.assertNotIn(words, text)
        self.assertIn("release-candidate-local.py --commit " + "a" * 40 + f" --fork-build 41 --fork-feed {FEED}", text)
        self.assertEqual(sorted(timings), ["fork_handoff_tests", "fork_update_tests", "hgs_release_build",
                                           "machine_dependent_scan", "tauri_package_selftest"])
        self.assertFalse(hasattr(self.module, "qt_windows"))

    def test_qt_only_is_gone(self):
        result = subprocess.run([sys.executable, "scripts/release-candidate.py", "--qt-only"], cwd=ROOT,
                                capture_output=True, text=True, timeout=60)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unrecognized arguments", result.stderr)

    def test_a_step_records_its_seconds(self):
        timings = {}
        self.module.run([sys.executable, "-c", "pass"], timings=timings, step="noop")
        self.assertIsInstance(timings["noop"], int)
        with self.assertRaises(SystemExit):
            self.module.run([sys.executable, "-c", "raise SystemExit(3)"], timings=timings, step="failed")
        self.assertIn("failed", timings)


if __name__ == "__main__":
    unittest.main()


