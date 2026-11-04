#!/usr/bin/env python3
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


CLIENT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("phase4_smoke", CLIENT / "scripts/phase4-smoke.py")
assert SPEC and SPEC.loader
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)
PNG = RUNNER.PNG
fixture_config = RUNNER.fixture_config


class FixtureTests(unittest.TestCase):
    def test_phase4_config_has_the_driven_states(self):
        config = fixture_config()
        self.assertEqual(len(config["inspect"]), 29)
        docs = config["inspect"]["claude/sample-project/docs-pass"]["pending_questions"][0]
        approval = config["inspect"]["kimi/sample-project/flaky-test"]["pending_questions"][0]
        self.assertEqual(len(docs["question_hash"]), 64)
        self.assertEqual(len(approval["question_hash"]), 64)
        self.assertTrue(approval["approval"])
        self.assertFalse(approval["approval_choices"])
        queue = config["inspect"]["codex/sample-project/release-notes"]["input_queue"]
        self.assertEqual(queue["id"], "queue-fixture")
        self.assertEqual(config["inspect"]["claude/sample-project/review"]["state"], "paused")
        self.assertFalse(config["inspect"]["claude/sample-project/review"]["run_id"].endswith("-resumed"))
        resumed = config["resume_ready"]["claude/sample-project/review"]
        self.assertEqual(resumed["state"], "running")
        self.assertEqual(resumed["runtime_state"], "live")
        self.assertTrue(resumed["run_id"].endswith("-resumed"))
        self.assertEqual(base64.b64decode(config["attachments"]["req-fixture-image"]["data_base64"]), base64.b64decode(PNG))
        writable = config["subagent_inspect"]["claude/sample-project/api-refactor\nagent-writable"]
        readonly = config["subagent_inspect"]["claude/sample-project/api-refactor\nagent-readonly"]
        self.assertTrue(writable["send_supported"])
        self.assertFalse(readonly["send_supported"])
        self.assertEqual(writable["parent_conversation_id"], writable["conversation_id"].rsplit("/", 1)[0])

    def test_catalog_rows_keep_hgs_tracking(self):
        rows = []
        states = []
        for line in (CLIENT / "fixtures/replay.ndjson").read_text().splitlines():
            frame = json.loads(line)
            if frame.get("t") == "snapshot" and frame.get("topic") == "catalog":
                rows.extend(item["row"] for item in frame["items"])
            elif frame.get("t") == "state" and frame.get("topic") == "activity":
                states.append(frame["value"])
        self.assertEqual(len(rows), 29)
        self.assertTrue(all(row["tracked"] is True for row in rows))
        self.assertEqual(len(states), 28)
        self.assertTrue(all(state["tracked"] is True and state["run_id"] and state["conversation_id"] for state in states))
        self.assertNotIn("claude/sample-project/review", [state["name"] for state in states])

    def test_launch_uses_private_display_bus_and_scratch_environment(self):
        source = (CLIENT / "scripts/phase4-smoke.py").read_text(encoding="utf-8")
        self.assertIn('"dbus-run-session", "--", "xvfb-run"', source)
        self.assertIn('"XDG_RUNTIME_DIR": str(scratch / "runtime")', source)
        self.assertIn('"TMPDIR": str(scratch / "tmp")', source)
        self.assertNotIn("os.environ.copy()", source)

    def test_regenerating_is_deterministic(self):
        paths = [CLIENT / "fixtures/replay.ndjson", CLIENT / "fixtures/replay-mac.ndjson", CLIENT / "fixtures/replay-batches.json"]
        def hashes():
            return [hashlib.sha256(path.read_bytes()).hexdigest() for path in paths]
        before = hashes()
        subprocess.run([sys.executable, str(CLIENT / "scripts/make-fixture.py")], check=True, capture_output=True)
        middle = hashes()
        subprocess.run([sys.executable, str(CLIENT / "scripts/make-fixture.py")], check=True, capture_output=True)
        self.assertEqual(before, middle)
        self.assertEqual(middle, hashes())


class FakeTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory(prefix="zerus-phase4-fake-")
        root = Path(self.dir.name)
        self.hgs = root / "hgs"
        shutil.copy2(CLIENT / "src-tauri/tests/support/fake_hgs_phase4.py", self.hgs)
        self.config = fixture_config()
        self.config["delays"] = {}
        (root / "config.json").write_text(json.dumps(self.config), encoding="utf-8")

    def tearDown(self):
        self.dir.cleanup()

    def call(self, *args, stdin=""):
        result = subprocess.run([sys.executable, str(self.hgs), *args], input=stdin, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        log = [json.loads(line) for line in (self.hgs.parent / "log.ndjson").read_text().splitlines()]
        self.assertEqual(log[-1]["argv"], list(args))
        self.assertEqual(log[-1]["stdin"], stdin)
        return json.loads(result.stdout) if result.stdout else None

    def test_receipts_reads_and_resume(self):
        request = {"request_id": "req-1", "expected_run_id": "run-1", "expected_conversation_id": "conv-1",
                   "text": "phase 4 smoke", "attachments": []}
        receipt = self.call("send", "claude/sample-project/api-refactor", "--json", stdin=json.dumps(request))
        self.assertEqual(receipt["status"], "submitted")
        self.assertEqual(receipt["request_id"], "req-1")
        answer = {"request_id": "req-2", "expected_run_id": "run-1", "expected_conversation_id": "conv-1",
                  "question_id": "tool:fixture-1", "expected_question_hash": "h", "answers": []}
        self.assertEqual(self.call("answer", "claude/sample-project/docs-pass", "--json", stdin=json.dumps(answer))["status"], "answered")
        staged = dict(request, attachments=[{"path": "/home/example/notes.txt"}])
        receipt = self.call("attachment", "claude/sample-project/api-refactor", "--stage", "--json", stdin=json.dumps(staged))
        self.assertEqual(receipt["files"], [{"path": "/home/example/.hgs/drop/notes.txt"}])
        image = self.call("attachment", "claude/sample-project/api-refactor", "--request", "req-fixture-image",
                          "--index", "0", "--conversation", "conv-1")
        self.assertEqual(base64.b64decode(image["data_base64"]), base64.b64decode(PNG))
        output = self.call("processes", "claude/sample-project/api-refactor", "--output", "call-git-fetch",
                           "--run", "run-1", "--conversation", "conv-1")
        self.assertEqual(output["output"], "running 42 tests\n")
        self.call("resume", "claude/sample-project/review", "--expected-run-id", "run-1", "-d")
        inspected = self.call("inspect", "claude/sample-project/review", "--after", "0", "--skip-processes")
        self.assertEqual(inspected["runtime_state"], "live")
        self.assertTrue(inspected["run_id"].endswith("-resumed"))
        child = self.call("inspect", "claude/sample-project/api-refactor", "--after", "0", "--skip-processes", "--subagent", "agent-writable")
        self.assertTrue(child["send_supported"])
        receipt = self.call("send", "claude/sample-project/api-refactor", "--json", stdin=json.dumps(dict(request, agent_id="agent-writable")))
        self.assertEqual(receipt["agent_id"], "agent-writable")


if __name__ == "__main__":
    unittest.main()
