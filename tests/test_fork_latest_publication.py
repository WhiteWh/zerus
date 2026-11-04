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
    repository = "Phoenix-/zerus"

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


class WorkflowChannels(unittest.TestCase):
    def test_push_manual_and_tag_select_exactly_one_publisher_and_the_right_source(self):
        workflow = (ROOT / ".github/workflows/fork-windows.yml").read_text()

        def evaluate(expression, repository, event, ref, channel, values):
            expression = expression.strip().removeprefix("${{").removesuffix("}}").strip()
            expression = expression.replace("&&", " and ").replace("||", " or ")
            expression = re.sub(r"!(?!=)", "not ", expression)
            return eval(expression, {"__builtins__": {}}, {
                "github": SimpleNamespace(repository=repository, event_name=event, ref=ref, sha="a" * 40),
                "inputs": SimpleNamespace(channel=channel), "env": SimpleNamespace(**values),
                "startsWith": lambda a,b: a.startswith(b),
            })

        for repository in ("WhiteWh/zerus", "Phoenix-/zerus"):
            for event, channel, ref, expected in (
                ("push", "", "refs/heads/whitewh/main", "latest"),
                ("workflow_dispatch", "numbered", "refs/heads/whitewh/main", "numbered"),
                ("workflow_dispatch", "", "refs/heads/whitewh/main", "numbered"),
                ("workflow_dispatch", "latest", "refs/heads/whitewh/main", "latest"),
                ("push", "", "refs/tags/whitewh-exp-fixture", "experimental"),
            ):
                with self.subTest(repository=repository, event=event, channel=channel, ref=ref):
                    values = {}
                    def run(expression):
                        return evaluate(expression, repository, event, ref, channel, values)
                    for name in ("EXPERIMENTAL", "LATEST_BUILD"):
                        expression = re.search(r"(?m)^      " + name + r": (.*)$", workflow)[1]
                        values[name] = "true" if run(expression) else "false"
                    source = re.search(r"(?m)^          ref: (.*)$", workflow)[1]
                    self.assertEqual(run(source), "refs/heads/whitewh/main" if expected == "latest" else "a" * 40)
                    feed = re.search(r"(?m)^      ZERUS_FORK_FEED_URL: (.*)$", workflow)[1]
                    self.assertEqual(bool(run(feed)), repository == "WhiteWh/zerus" and expected == "numbered")
                    selected = []
                    for label, name in (("latest", "Refresh rolling Latest prerelease"),
                                        ("numbered", "Publish the fork release"),
                                        ("experimental", "Publish the experimental prerelease")):
                        step = next(s for s in re.split(r"(?m)^      - ", workflow) if s.startswith("name: " + name + "\n"))
                        if run(re.search(r"(?m)^        if: (.*)$", step)[1]):
                            selected.append(label)
                    self.assertEqual(selected, [expected])


if __name__ == "__main__":
    unittest.main()
