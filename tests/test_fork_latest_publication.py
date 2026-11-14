"""Rolling Latest replacement, failure recovery and manual numbered release isolation."""
import copy
import importlib.util
from pathlib import Path
import re
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from windows_portable_fixture import pinned_pair, portable_zip
import conpty_pair

spec = importlib.util.spec_from_file_location("fork_latest", ROOT / "scripts/publish-fork-latest.py")
latest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(latest)


class FakeGitHub:
    repository = "WhiteWh/zerus"

    def __init__(self):
        self.current, self.ref, self.created = None, None, 0
        self.next_asset_id = 10
        self.fail = None
        self.numbered = {"phoenix-2": {"commit": "0" * 40, "asset": b"manual release"}}

    def release(self):
        return copy.deepcopy(self.current)

    def create(self, commit, notes):
        self.created += 1
        self.current = {"id": 1, "tag_name": "latest", "draft": True, "prerelease": True,
                        "immutable": False, "assets": [], "html_url": "https://example.invalid/latest"}
        return self.release()

    def upload(self, path, name):
        if self.fail == "upload":
            raise RuntimeError("upload failed")
        self.current["assets"].append({"id": self.next_asset_id,
                                       "name": name, "state": "uploaded", "size": path.stat().st_size,
                                       "digest": "bad" if self.fail == "digest" else latest.digest(path)})
        self.next_asset_id += 1

    def rename(self, asset_id, name):
        if self.fail == "rename" and name == latest.ASSET:
            self.fail = None
            raise RuntimeError("rename refused")
        row = next(a for a in self.current["assets"] if a["id"] == asset_id)
        row["name"] = name
        if self.fail == "unknown-backup" and name == latest.BACKUP:
            self.fail = None
            raise RuntimeError("backup response lost")
        if self.fail == "unknown-rename" and name == latest.ASSET:
            self.fail = None
            raise RuntimeError("rename response lost")

    def delete(self, asset_id):
        self.current["assets"] = [a for a in self.current["assets"] if a["id"] != asset_id]

    def move_tag(self, commit):
        if self.fail == "tag":
            raise RuntimeError("tag update failed")
        self.ref = commit

    def finish(self, commit, notes):
        self.current.update(target_commitish=commit, body=notes, draft=False, prerelease=True)
        return self.release()


class LatestPublication(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="zerus-latest-contract-")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        pin = patch.object(conpty_pair, "PIN", pinned_pair())
        pin.start()
        self.addCleanup(pin.stop)
        self.client = FakeGitHub()
        self.first = portable_zip(self.directory / "first.zip", commit="a" * 40)
        self.second = portable_zip(self.directory / "second.zip", commit="b" * 40)

    def publish(self, path=None, commit="a" * 40):
        return latest.publish(self.client, path or self.first, commit, "123")

    def canonical(self):
        return latest.assets(self.client.release())[latest.ASSET]

    def test_first_publish_then_replacement_reuses_one_prerelease_and_preserves_numbered(self):
        before = copy.deepcopy(self.client.numbered)
        self.publish()
        self.publish(self.second, "b" * 40)
        self.assertEqual(self.client.created, 1)
        self.assertEqual(self.client.ref, "b" * 40)
        self.assertEqual(self.client.numbered, before)
        self.assertEqual(list(latest.assets(self.client.release())), [latest.ASSET])
        self.assertEqual(self.canonical()["digest"], latest.digest(self.second))
        self.assertFalse(self.client.current["draft"])
        self.assertTrue(self.client.current["prerelease"])

    def test_upload_failure_keeps_the_previous_canonical_asset_and_tag(self):
        self.publish()
        before = self.canonical()
        self.client.fail = "upload"
        with self.assertRaisesRegex(RuntimeError, "upload failed"):
            self.publish(self.second, "b" * 40)
        self.assertEqual(self.canonical(), before)
        self.assertEqual(self.client.ref, "a" * 40)

    def test_refused_promotion_restores_previous_asset_and_retry_completes(self):
        self.publish()
        before = self.canonical()
        self.client.fail = "rename"
        with self.assertRaisesRegex(RuntimeError, "rename refused"):
            self.publish(self.second, "b" * 40)
        self.assertEqual(self.canonical(), before)
        self.publish(self.second, "b" * 40)
        self.assertEqual(self.canonical()["digest"], latest.digest(self.second))

    def test_unverified_upload_does_not_replace_the_previous_asset_or_tag(self):
        self.publish()
        before = self.canonical()
        self.client.fail = "digest"
        with self.assertRaisesRegex(ValueError, "SHA-256 differs"):
            self.publish(self.second, "b" * 40)
        self.assertEqual(self.canonical(), before)
        self.assertEqual(self.client.ref, "a" * 40)

    def test_unknown_promotion_is_reconciled_without_rolling_back_the_new_asset(self):
        self.publish()
        self.client.fail = "unknown-rename"
        with self.assertRaisesRegex(RuntimeError, "response lost"):
            self.publish(self.second, "b" * 40)
        self.assertEqual(self.canonical()["digest"], latest.digest(self.second))
        self.publish(self.second, "b" * 40)
        self.assertEqual(self.client.created, 1)
        self.assertEqual(self.client.ref, "b" * 40)

    def test_unknown_backup_rename_restores_the_old_asset_before_retry(self):
        self.publish()
        before = self.canonical()
        self.client.fail = "unknown-backup"
        with self.assertRaisesRegex(RuntimeError, "backup response lost"):
            self.publish(self.second, "b" * 40)
        self.assertEqual(self.canonical(), before)
        self.assertEqual(self.client.ref, "a" * 40)
        self.publish(self.second, "b" * 40)
        self.assertEqual(self.client.ref, "b" * 40)

    def test_tag_failure_can_resume_with_the_already_promoted_asset(self):
        self.publish()
        self.client.fail = "tag"
        with self.assertRaisesRegex(RuntimeError, "tag update failed"):
            self.publish(self.second, "b" * 40)
        self.client.fail = None
        self.publish(self.second, "b" * 40)
        self.assertEqual(self.client.ref, "b" * 40)
        self.assertEqual(list(latest.assets(self.client.release())), [latest.ASSET])

    def test_wrong_source_is_refused_before_creating_a_release(self):
        with self.assertRaisesRegex(ValueError, "another source"):
            self.publish(commit="b" * 40)
        self.assertEqual(self.client.created, 0)

    def test_stable_immutable_and_unrecognized_assets_are_preserved(self):
        self.publish()
        for changes in ({"prerelease": False}, {"immutable": True},
                        {"assets": [{"id": 5, "name": "owner-notes.txt"}]}):
            original = self.client.release()
            self.client.current.update(changes)
            before = self.client.release()
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.publish(self.second, "b" * 40)
            self.assertEqual(self.client.current, before)
            self.client.current = original



class GitHubRepositoryTests(unittest.TestCase):
    """Task #446: only the owner's fork may publish its Latest prerelease."""

    def test_only_the_forks_repository_may_publish(self):
        with self.assertRaisesRegex(ValueError, "explicitly supported forks"):
            latest.GitHub("Phoenix-/zerus")
        with self.assertRaisesRegex(ValueError, "explicitly supported forks"):
            latest.GitHub("someone/zerus")
        self.assertEqual(latest.GitHub("WhiteWh/zerus").repository, "WhiteWh/zerus")


class WorkflowChannels(unittest.TestCase):
    """Task #446 (owner, 2026-11-09, "сборка только по пинку"): nothing builds on
    a push. One manual dispatch publishes exactly one channel - a numbered release
    with the stable feed, or a refresh of the rolling Latest prerelease with no
    feed - and a dispatch from another branch publishes nothing."""

    def setUp(self):
        self.workflow = (ROOT / ".github/workflows/fork-windows.yml").read_text(encoding="utf-8")

    def evaluate(self, expression, ref, channel, values, build=""):
        expression = expression.strip().removeprefix("${{").removesuffix("}}").strip()
        expression = expression.replace("&&", " and ").replace("||", " or ")
        expression = re.sub(r"!(?!=)", "not ", expression)
        return eval(expression, {"__builtins__": {}}, {
            "github": SimpleNamespace(repository="WhiteWh/zerus", event_name="workflow_dispatch",
                                      ref=ref, sha="a" * 40, run_number=41),
            "inputs": SimpleNamespace(channel=channel, build=build), "env": SimpleNamespace(**values),
            "startsWith": lambda a, b: a.startswith(b),
        })

    def expression(self, name):
        return re.search(r"(?m)^      " + name + r": (.*)$", self.workflow)[1]

    def test_nothing_starts_a_build_on_a_push(self):
        triggers = self.workflow.split("\non:")[1].split("\npermissions:")[0]
        self.assertIn("workflow_dispatch:", triggers)
        self.assertNotIn("push:", triggers)
        self.assertIn("options: [numbered, latest]", triggers)
        self.assertIn("default: numbered", triggers)

    def test_one_manual_channel_publishes_and_selects_its_own_source_and_identity(self):
        publishers = (("Refresh the rolling Latest prerelease", "latest"),
                      ("Update feed", "numbered"),
                      ("Publish the fork release", "numbered"))
        for ref, channel, expected in (
            ("refs/heads/whitewh/main", "numbered", ["Update feed", "Publish the fork release"]),
            ("refs/heads/whitewh/main", "", ["Update feed", "Publish the fork release"]),
            ("refs/heads/whitewh/main", "latest", ["Refresh the rolling Latest prerelease"]),
            ("refs/heads/probe", "numbered", []),
            ("refs/heads/probe", "latest", []),
        ):
            with self.subTest(ref=ref, channel=channel):
                values = {}
                run = lambda expression: self.evaluate(expression, ref, channel, values)
                values["LATEST_BUILD"] = "true" if run(self.expression("LATEST_BUILD")) else "false"
                numbered = bool(run(self.expression("ZERUS_FORK_BUILD")))
                feed = run(self.expression("ZERUS_FORK_FEED_URL"))
                # A numbered release from whitewh/main embeds its number and the
                # stable feed; every other dispatch embeds neither.
                self.assertEqual(numbered, expected == ["Update feed", "Publish the fork release"])
                self.assertEqual(feed.startswith("https://github.com/WhiteWh/zerus/releases/"), numbered)
                # Latest builds the current branch head; a numbered build keeps the
                # dispatched commit.
                source = re.search(r"(?m)^          ref: (.*)$", self.workflow)[1]
                self.assertEqual(run(source), "refs/heads/whitewh/main" if channel == "latest" else "a" * 40)
                selected = []
                for name, _ in publishers:
                    step = next(s for s in re.split(r"(?m)^      - ", self.workflow)
                                if s.startswith("name: " + name + "\n"))
                    if run(re.search(r"(?m)^        if: (.*)$", step)[1]):
                        selected.append(name)
                self.assertEqual(selected, expected)

    def test_a_numbered_release_uses_the_named_build_or_the_run_number(self):
        # Owner, 2026-11-14: local builds had passed the run numbers, so the
        # dispatch may name the release build; every build site follows it.
        sites = [self.expression("ZERUS_FORK_BUILD")]
        for name in ("Check the release build number", "Update feed", "Publish the fork release"):
            step = next(s for s in re.split(r"(?m)^      - ", self.workflow)
                        if s.startswith("name: " + name + "\n"))
            sites.append(re.search(r"(?m)^          BUILD: (.*)$", step)[1])
        self.assertNotIn("BUILD: ${{ github.run_number }}", self.workflow)
        for site in sites:
            for build, expected in (("", 41), ("198", "198")):
                with self.subTest(site=site, build=build):
                    value = self.evaluate(site, "refs/heads/whitewh/main", "numbered", {"LATEST_BUILD": "false"}, build)
                    self.assertEqual(value, expected)
        check = next(s for s in re.split(r"(?m)^      - ", self.workflow)
                     if s.startswith("name: Check the release build number\n"))
        self.assertIn("must exceed the latest numbered release", check)
        self.assertLess(self.workflow.index("name: Check the release build number"), self.workflow.index("name: Rust tests"))

    def test_the_manual_build_uses_the_release_candidate_and_no_qt(self):
        candidate = next(s for s in re.split(r"(?m)^      - ", self.workflow)
                         if s.startswith("name: Release candidate build, package and self-test\n"))
        self.assertIn("scripts/release-candidate.py --commit \"$COMMIT\"", candidate)
        self.assertIn("--fork-build \"$ZERUS_FORK_BUILD\" --fork-feed \"$ZERUS_FORK_FEED_URL\"", candidate)
        self.assertLess(self.workflow.index("name: Rust tests"), self.workflow.index("name: Release candidate build, package and self-test"))
        self.assertLess(self.workflow.index("name: Release candidate build, package and self-test"),
                        self.workflow.index("name: Publish the fork release"))
        for words in ("cmake", "ctest", "gui.sh", "install-qt-windows.py", "QT_ROOT_DIR", "qt-configure"):
            self.assertNotIn(words, self.workflow)


if __name__ == "__main__":
    unittest.main()


