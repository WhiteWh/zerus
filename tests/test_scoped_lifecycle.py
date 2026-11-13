#!/usr/bin/env python3
"""Scoped lifecycle ABI against private tmux and synthetic scripted agents."""
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import subprocess
import unittest
import uuid

import test_pause

test_pause.HGS = Path(os.environ.get("HGS_TEST_BIN", Path(__file__).resolve().parents[1] / "target/debug/hgs")).resolve()


@unittest.skipUnless(test_pause.TMUX, "tmux is required for isolated lifecycle tests")
class SessionActions(test_pause.Harness):
    def command(self, name, action, record=None, **fields):
        record = self.binding(name) if record is None else record
        return {"request_id": str(uuid.uuid4()), "action": action,
                "expected_run_id": record["run_id"], "expected_conversation_id": record["conversation_id"], **fields}

    def action(self, name, payload, rc=0):
        result = subprocess.run([str(test_pause.HGS), "session-action", name, "--json"], input=json.dumps(payload),
                                env=self.env, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, rc, result.stdout + result.stderr)
        if rc:
            return result
        value = json.loads(result.stdout)  # No human status lines before/after the one JSON object.
        self.assertEqual(value["request_id"], payload["request_id"])
        self.assertEqual(value["name"], name)
        self.assertEqual(value["run_id"], payload["expected_run_id"])
        self.assertEqual(value["conversation_id"], payload["expected_conversation_id"])
        self.assertNotIn("request_hash", value)
        return value

    def test_wrong_conversation_does_not_stop_or_rename_same_run(self):
        name = self.start("codex")
        before = self.binding(name)
        for action, extra in [("terminate", {}), ("forget", {}), ("pause", {}), ("rename", {"new_name": "codex/p/new"})]:
            request = self.command(name, action, **extra)
            request["expected_conversation_id"] = "replacement-conversation"
            result = self.action(name, request)
            self.assertEqual(result["status"], "failed")
            self.assertEqual(self.binding(name)["run_id"], before["run_id"])
            self.t("has-session", "-t", "=" + name)

    def test_a_session_restarted_since_it_was_listed_is_refused_and_kept(self):
        # zerus-sa9qn.23: the desktop ends a session, and each chosen child
        # session, through this envelope with the run it listed. A restart
        # since then (another run of the same session) is refused and kept,
        # whether the end is Terminate, Kill (forget) or Move to archive.
        name = self.start("codex")
        listed = self.binding(name)
        self.assertEqual(self.action(name, self.command(name, "pause"))["status"], "completed")
        resumed = self.action(name, self.command(name, "resume"))
        self.assertEqual(resumed["status"], "completed")
        run = resumed["result_target"]["run_id"]
        self.assertNotEqual(run, listed["run_id"])
        self.wait(lambda: self.binding(name).get("run_id") == run and not self.binding(name).get("expected_id"))
        for action in ("terminate", "forget"):
            result = self.action(name, self.command(name, action, record=listed))
            self.assertEqual(result["status"], "failed", result)
            self.assertEqual(self.binding(name)["run_id"], run)
            self.t("has-session", "-t", "=" + name)
        self.assertEqual(self.action(name, self.command(name, "pause"))["status"], "completed")
        result = self.action(name, self.command(name, "archive", record=listed))
        self.assertEqual(result["status"], "failed", result)
        self.assertEqual(self.binding(name)["run_id"], run)
        # The session as it is now still archives.
        self.assertEqual(self.action(name, self.command(name, "archive"))["status"], "completed")
        self.assertFalse(self.binding(name))

    def test_concurrent_identical_rename_uuid_has_one_mutation_and_durable_receipt(self):
        name = self.start("codex")
        before = self.binding(name)
        request = self.command(name, "rename", new_name="codex/p/new")
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(lambda _: self.action(name, request), range(2)))
        self.assertEqual(results[0], results[1])
        self.assertEqual(results[0]["status"], "completed")
        self.assertEqual(results[0]["result_target"], {"name": "codex/p/new", "run_id": before["run_id"], "conversation_id": before["conversation_id"]})
        self.assertEqual(self.action(name, request), results[0])
        self.assertFalse(self.binding(name))
        self.assertEqual(self.binding("codex/p/new")["run_id"], before["run_id"])
        self.action(name, {**request, "new_name": "codex/p/other"}, rc=1)

    def test_pause_resume_terminate_restore_keep_exact_original_and_result_identities(self):
        name = self.start("claude")
        original = self.binding(name)
        paused = self.action(name, self.command(name, "pause"))
        self.assertEqual(paused["status"], "completed")
        self.assertEqual(paused["result_target"]["run_id"], original["run_id"])
        resumed = self.action(name, self.command(name, "resume"))
        self.assertEqual(resumed["status"], "completed")
        new_run = resumed["result_target"]["run_id"]
        self.assertNotEqual(new_run, original["run_id"])
        self.wait(lambda: self.binding(name).get("run_id") == new_run and not self.binding(name).get("expected_id"))
        terminated = self.action(name, self.command(name, "terminate"))
        self.assertEqual(terminated["status"], "completed")
        archived = terminated["result_target"]
        self.assertIn("archive_id", archived)
        restored = self.action(name, self.command(name, "restore", record=archived, archive_id=archived["archive_id"]))
        self.assertEqual(restored["status"], "completed")
        self.assertNotEqual(restored["result_target"]["run_id"], new_run)
        self.assertEqual(restored["result_target"]["conversation_id"], original["conversation_id"])

    def test_stale_forget_preserves_archive_referenced_by_replacement_restore(self):
        name = self.start("codex")
        original = self.binding(name)
        self.action(name, self.command(name, "pause"))
        archived = self.action(name, self.command(name, "archive"))["result_target"]
        archive_path = self.root / "state/archives" / (archived["archive_id"] + ".json")
        before = archive_path.read_bytes()
        # A new tracked run with a pending restore owns this immutable source.
        self.start("codex")
        replacement = self.binding(name)
        replacement["restore_archive_id"] = archived["archive_id"]
        replacement["expected_id"] = original["conversation_id"]
        slots = [* (self.root / "state").glob("*.json"), * (self.root / "state/bindings").glob("*.json")]
        slot = next(path for path in slots if json.loads(path.read_text()).get("name") == name and not json.loads(path.read_text()).get("rename_shadow"))
        slot.write_text(json.dumps(replacement))
        for extras in ({}, {"archive_id": archived["archive_id"]}):
            result = self.action(name, self.command(name, "forget", record=original, **extras))
            self.assertEqual(result["status"], "failed")
            self.assertEqual(archive_path.read_bytes(), before)
            self.assertEqual(self.binding(name)["run_id"], replacement["run_id"])
            self.assertEqual(self.binding(name)["restore_archive_id"], archived["archive_id"])
            self.t("has-session", "-t", "=" + name)

    def test_invalid_fields_and_archive_on_live_action_are_rejected_before_claim(self):
        name = self.start("codex")
        for fields in ({"action": "arbitrary"}, {"archive_id": str(uuid.uuid4())}, {"new_name": "unexpected"}, {"tag": "unexpected"}, {"extra": True}):
            request = {**self.command(name, "pause"), **fields}
            self.action(name, request, rc=1)
            self.assertFalse((self.root / "state/action_receipts" / (request["request_id"] + ".json")).exists())
        self.t("has-session", "-t", "=" + name)


if __name__ == "__main__":
    unittest.main()
