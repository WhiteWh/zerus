"""Scoped resident-adapter contract via a private synthetic Unix RPC server."""
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import unittest
import uuid

import receipt_locator

HGS = Path(os.environ.get("HGS_TEST_BIN", "target/debug/hgs")).resolve()


class DeepSeekScopedActions(unittest.TestCase):
    def setUp(self):
        # macOS TMPDIR can exceed the Unix socket path limit; keep the private root short.
        self.temp = tempfile.TemporaryDirectory(prefix="hgs-scoped-dsh-", dir=os.environ.get("ZERUS_TEST_SHORT_TMPDIR", "/tmp"))
        self.root = Path(self.temp.name)
        self.name = "dsh/example/session"
        self.record = {"name": self.name, "backend": "dsh", "agent": "dsh", "session_id": str(uuid.uuid4()), "run_id": "run", "conversation_id": "session-synthetic", "agent_home": str(self.root / "native"), "cwd": str(self.root)}
        self.binding = self.root / "state/dsh/bindings" / (hashlib.sha256(self.name.encode()).hexdigest() + ".json")
        self.binding.parent.mkdir(parents=True)
        self.binding.write_text(json.dumps(self.record))
        host = self.root / "state/dsh/hosts" / hashlib.sha256(self.record["agent_home"].encode()).hexdigest()[:16]
        host.mkdir(parents=True)
        self.server = socket.socket(socket.AF_UNIX)
        self.server.bind(str(host / "host.sock")); self.server.listen(); self.server.settimeout(.05)
        self.supported = True
        self.native = False
        self.fork_reply = True
        self.generation = "fixture-generation"
        self.status = "completed"
        self.answer_generation = "fixture-generation"
        self.list_items = None
        self.calls = []
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.serve, daemon=True); self.thread.start()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("HGS_")}
        (self.root / "tmux").mkdir(mode=0o700)
        self.env.pop("TMUX", None)
        self.env.update(TMUX_TMPDIR=str(self.root / "tmux"), HOME=str(self.root), HGS_STATE_DIR=str(self.root / "state"), HGS_CONFIG_DIR=str(self.root / "config"), HGS_SELF="fixture", HGS_PEERS="")

    def tearDown(self):
        self.stop.set(); self.thread.join(timeout=2); self.server.close()
        # An inspection may have started the receipt locator's detached worker;
        # it must finish before its state directory is removed (zerus-a4mub.11).
        receipt_locator.idle(self.root / "state")
        self.temp.cleanup()

    def serve(self):
        while not self.stop.is_set():
            try:
                client, _ = self.server.accept()
            except socket.timeout:
                continue
            with client:
                reader = client.makefile("rb")
                request = json.loads(reader.readline())
                self.calls.append(request)
                method = request["method"]
                params = request["params"]
                if method == "ping":
                    value = {"generation": self.generation, "sessionActions": self.supported, "version": "0.2.1-rc.2", "protocol": 1}
                    if self.native:
                        value["nativeOperations"] = ["fork", "history-search", "schedules"]
                elif method == "inspect":
                    value = {"generation": self.generation, "lifecycleActions": ["pause", "resume", "rename", "forget"],
                             "forkSupported": self.native, "forkReady": True}
                elif method == "list" and self.list_items is not None:
                    value = {"generation": self.generation, "items": self.list_items}
                elif method == "session-action":
                    value = {"generation": self.answer_generation, "sessionId": "session-synthetic", "action": params["action"], "status": self.status, "error": "Synthetic refusal"}
                elif method in ("fork", "search-history", "schedules") and not self.native:
                    # An older resident adapter answers every newer request this way.
                    client.sendall((json.dumps({"ok": False, "error": "Unsupported HGS native request"}) + "\n").encode())
                    continue
                elif method == "search-history":
                    event = {"type": "user/message", "seq": 3, "time": 1791742557000,
                             "data": {"role": "user", "content": "Find the fixture needle", "source": {"kind": "user"}}}
                    value = {"generation": self.generation, "sessions": [
                        {"sessionId": sid, "status": "ok", "events": [event], "truncated": False} for sid in params["sessionIds"]]}
                elif method == "fork":
                    if not self.fork_reply:
                        # The native fork may exist, but its reply is lost.
                        client.shutdown(socket.SHUT_RDWR)
                        continue
                    value = {"requestId": params["requestId"], "sourceSessionId": params["sessionId"], "sessionId": "session-forked",
                             "cwd": params["cwd"], "generation": self.generation, "live": True, "owned": False}
                else:
                    value = {}
                client.sendall((json.dumps({"ok": True, "value": value}) + "\n").encode())

    def run_action(self, action, **extra):
        request = {"request_id": str(uuid.uuid4()), "action": action, "expected_run_id": "run", "expected_conversation_id": "session-synthetic", **extra}
        result = subprocess.run([str(HGS), "session-action", self.name, "--json"], env=self.env, input=json.dumps(request), capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return request, json.loads(result.stdout)

    def test_listed_child_marks_only_person_requests(self):
        # zerus-lgr9s: a supervised DeepSeek child's listed row carries
        # human_request. Its unread reply is its parent's, so the row stays
        # quiet; a tool approval or signing in is the person's.
        self.record["parent"] = {"schema": 1, "machine": "11111111-1111-4111-8111-111111111111", "host": "fixture",
                                 "session_id": str(uuid.uuid4()), "name": "claude/example/lead",
                                 "conversation_id": "lead-conversation", "run_id": "lead-run", "source": "launch", "at": 1}
        self.binding.write_text(json.dumps(self.record))
        outline = [{"turn": 3, "seq": 3, "prompt": "Check the fixture", "response": "Fixture checked"}]
        idle = {"sessionId": "session-synthetic", "agentAvailable": True, "running": False, "updatedAt": 1_000_000.0,
                "projections": {"values": {"turnOutline": outline}}, "pendingQuestions": []}

        def listed(row):
            self.list_items = [row]
            result = subprocess.run([str(HGS), "ls", "--json", "--local"], env=self.env, capture_output=True, text=True, timeout=20)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            rows = [session for session in json.loads(result.stdout)["sessions"] if session["name"] == self.name]
            self.assertEqual(len(rows), 1, result.stdout)
            return rows[0]

        quiet = listed(idle)
        self.assertEqual((quiet["reply_id"], quiet["phase"], quiet["human_request"]), ("dsh-turn-3", "idle", False), quiet)
        question = {"id": "question-1", "questions": [], "approval": False}
        asked = listed({**idle, "pendingQuestions": [question]})
        self.assertEqual((asked["phase"], asked["human_request"]), ("input", False), asked)
        approval = listed({**idle, "pendingQuestions": [{**question, "approval": True}]})
        self.assertEqual((approval["phase"], approval["human_request"]), ("input", True), approval)
        sign_in = listed({**idle, "authRequired": True})
        self.assertEqual((sign_in["activity_summary"], sign_in["human_request"]), ("Sign in required", True), sign_in)

    def test_supported_resident_adapter_uses_generation_without_create_or_restart(self):
        _, result = self.run_action("pause")
        self.assertEqual(result["status"], "completed")
        self.assertEqual([call["method"] for call in self.calls], ["ping", "inspect", "session-action"])
        self.assertEqual(self.calls[-1]["params"]["generation"], "fixture-generation")
        self.assertTrue(json.loads(self.binding.read_text())["paused"])
        self.assertEqual(result["result_target"]["run_id"], "run")

    def test_old_resident_adapter_and_wrong_identity_are_definite_failures(self):
        self.supported = False
        _, result = self.run_action("pause")
        self.assertEqual(result["status"], "failed")
        self.assertEqual([call["method"] for call in self.calls], ["ping"])
        self.assertEqual(json.loads(self.binding.read_text()), self.record)
        _, result = self.run_action("pause", expected_conversation_id="session-other")
        self.assertEqual(result["status"], "failed")
        self.assertEqual(len(self.calls), 1)

    def test_confirmed_zero_effect_failure_and_uncertain_native_handoff_remain_distinct(self):
        for status in ("failed", "uncertain"):
            self.status = status
            request, result = self.run_action("pause")
            self.assertEqual(result["status"], status)
            self.assertEqual(json.loads(self.binding.read_text()), self.record)
            before = len(self.calls)
            duplicate = subprocess.run([str(HGS), "session-action", self.name, "--json"], env=self.env, input=json.dumps(request), capture_output=True, text=True, timeout=10)
            self.assertEqual(json.loads(duplicate.stdout), result)
            self.assertEqual(len(self.calls), before)

    def test_replacement_generation_failed_ack_cannot_claim_zero_effects(self):
        self.status = "failed"
        self.answer_generation = "replacement-generation"
        _, result = self.run_action("pause")
        self.assertEqual(result["status"], "uncertain")
        self.assertEqual(json.loads(self.binding.read_text()), self.record)

    def test_confirmed_resume_gets_new_run_and_rename_preserves_conversation(self):
        _, result = self.run_action("resume")
        self.assertEqual(result["status"], "completed")
        self.assertNotEqual(result["result_target"]["run_id"], "run")
        self.binding.write_text(json.dumps(self.record))
        _, result = self.run_action("rename", new_name="dsh/example/new")
        self.assertEqual(result["result_target"]["name"], "dsh/example/new")
        self.assertEqual(result["result_target"]["conversation_id"], "session-synthetic")
        self.assertFalse(self.binding.exists())

    def test_rename_keeps_the_shared_read_marks_of_the_session(self):
        # DeepSeek sessions own their marks by name; a rename must carry them over.
        marks = self.root / "state/read-marks.json"
        mark = {"conversation_id": "session-synthetic", "reply_id": "3:30.0", "attention_signature": None, "at": 1.0}
        other = {"dsh/example/other": {"session-other": dict(mark, conversation_id="session-other")}}
        marks.write_text(json.dumps({"version": 1, "sessions": {self.name: {"session-synthetic": mark}, **other}}))
        marks.chmod(0o600)
        _, result = self.run_action("rename", new_name="dsh/example/new")
        self.assertEqual(result["status"], "completed")
        self.assertEqual(json.loads(marks.read_text()), {"version": 1, "sessions": {"dsh/example/new": {"session-synthetic": mark}, **other}})

    def test_context_reset_keeps_the_explicit_fallback_without_native_effects(self):
        # The reviewed native release has no context reset; a clear must never reach the host.
        state = sorted(path.relative_to(self.root) for path in (self.root / "state").rglob("*"))
        request = {"request_id": str(uuid.uuid4()), "expected_run_id": "run", "expected_conversation_id": "session-synthetic"}
        result = subprocess.run([str(HGS), "clear-context", self.name, "--json"], env=self.env, input=json.dumps(request), capture_output=True, text=True, timeout=10)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("This DeepSeek harness does not expose a native context reset. Use its native UI.", result.stderr)
        self.assertEqual(self.calls, [])
        self.assertEqual(json.loads(self.binding.read_text()), self.record)
        self.assertEqual(sorted(path.relative_to(self.root) for path in (self.root / "state").rglob("*")), state)

    def hgs(self, *args, request=None):
        return subprocess.run([str(HGS), *args], env=self.env, input=json.dumps(request) if request else None,
                              capture_output=True, text=True, timeout=10)

    def bindings(self):
        return sorted(path.name for path in self.binding.parent.glob("*.json"))

    def test_older_adapter_keeps_fork_search_and_schedules_in_native_ui(self):
        before = self.bindings()
        fork = self.hgs("fork", self.name, "-n", "copy", "-d")
        self.assertNotEqual(fork.returncode, 0)
        self.assertIn("older adapter", fork.stderr)
        _, scoped = self.run_action("fork", tag="copy")
        self.assertEqual(scoped["status"], "failed")
        schedules = self.hgs("schedules", self.name)
        self.assertNotEqual(schedules.returncode, 0)
        self.assertIn("older adapter", schedules.stderr)
        search = self.hgs("search", "--query", "anything")
        self.assertEqual(search.returncode, 0, search.stderr)
        self.assertEqual((json.loads(search.stdout)["results"], json.loads(search.stdout)["unavailable_sessions"]), ([], 1))
        self.assertEqual([call["method"] for call in self.calls], ["ping", "ping", "ping", "search-history"])
        self.assertEqual(self.bindings(), before)
        self.assertEqual(json.loads(self.binding.read_text()), self.record)

    def test_unresponsive_hosts_do_not_hold_up_search(self):
        # Two other DeepSeek accounts whose hosts accept and never answer.
        self.native = True
        silent = []
        for index in range(2):
            home = self.root / f"silent-{index}"
            name = f"dsh/example/silent-{index}"
            record = {**self.record, "name": name, "conversation_id": f"session-silent-{index}", "agent_home": str(home)}
            (self.binding.parent / (hashlib.sha256(name.encode()).hexdigest() + ".json")).write_text(json.dumps(record))
            host = self.root / "state/dsh/hosts" / hashlib.sha256(str(home).encode()).hexdigest()[:16]
            host.mkdir(parents=True)
            server = socket.socket(socket.AF_UNIX); server.bind(str(host / "host.sock")); server.listen(4)
            silent.append(server)
        try:
            started = time.monotonic()
            result = subprocess.run([str(HGS), "search", "--query", "needle"], env=self.env,
                                    capture_output=True, text=True, timeout=120)
            elapsed = time.monotonic() - started
        finally:
            for server in silent:
                server.close()
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        # The desktop gives a local search 12 s; one bounded wait covers every host.
        self.assertLess(elapsed, 11)
        self.assertEqual([(hit["name"], hit["content"]) for hit in data["results"]], [(self.name, "Find the fixture needle")])
        self.assertEqual(data["unavailable_sessions"], 2)

    def test_uncertain_fork_reply_is_adopted_by_the_same_request(self):
        self.native = True
        self.fork_reply = False
        pending = self.root / "state/dsh/forks"
        lost = self.hgs("fork", self.name, "-n", "copy", "-d")
        self.assertNotEqual(lost.returncode, 0)
        self.assertIn("delivery uncertain", lost.stderr)
        self.assertEqual(len(list(pending.glob("*.json"))), 1)
        self.assertNotIn("dsh/example/copy", [json.loads(p.read_text())["name"] for p in self.binding.parent.glob("*.json")])
        self.fork_reply = True
        adopted = self.hgs("fork", self.name, "-n", "copy", "-d")
        self.assertEqual(adopted.returncode, 0, adopted.stderr)
        forks = [call["params"] for call in self.calls if call["method"] == "fork"]
        self.assertEqual(len(forks), 2)
        self.assertEqual(forks[0]["requestId"], forks[1]["requestId"])
        self.assertEqual((forks[1]["sessionId"], forks[1]["cwd"], forks[1]["title"]), ("session-synthetic", str(self.root), "copy"))
        target = json.loads((self.binding.parent / (hashlib.sha256(b"dsh/example/copy").hexdigest() + ".json")).read_text())
        self.assertEqual((target["conversation_id"], target["fork_parent_id"], target["agent_home"]), ("session-forked", "session-synthetic", self.record["agent_home"]))
        self.assertNotEqual(target["run_id"], "run")
        self.assertFalse(list(pending.glob("*.json")))
        self.assertEqual(json.loads(self.binding.read_text()), self.record)

    def test_unconfirmed_fork_from_a_replaced_host_is_not_replayed(self):
        self.native = True
        self.fork_reply = False
        self.hgs("fork", self.name, "-n", "copy", "-d")
        self.generation = "replacement-generation"
        self.fork_reply = True
        refused = self.hgs("fork", self.name, "-n", "copy", "-d")
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("was not confirmed", refused.stderr)
        self.assertEqual(len([call for call in self.calls if call["method"] == "fork"]), 1)
        original = list((self.root / "state/dsh/forks").glob("*.json"))
        self.assertEqual(len(original), 1)
        retained = original[0].read_bytes()
        retried = self.hgs("fork", self.name, "-n", "copy", "-d")
        self.assertNotEqual(retried.returncode, 0, retried.stderr)
        self.assertEqual(original[0].read_bytes(), retained)
        self.assertEqual(len([call for call in self.calls if call["method"] == "fork"]), 1)

    def ambiguous_launch(self, new, damaged=False):
        self.binding.unlink()
        folder = self.root / "state/dsh/sessions"
        folder.mkdir()
        for index in range(2):
            sid = str(uuid.uuid4())
            record = dict(self.record, layout="id", session_id=sid, conversation_id="session-original-" + str(index))
            (folder / (sid + ".json")).write_text(json.dumps(record))
        if damaged:
            next(folder.glob("*.json")).write_text("{damaged")
        before = {p.name: p.read_bytes() for p in folder.glob("*.json")}
        workspace = self.root / "example"
        workspace.mkdir()
        self.env["DSH_HOME"] = self.record["agent_home"]
        result = self.hgs("dsh", str(workspace), "-n", "session", "-d", *(["--new"] if new else []))
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.calls, [], "ambiguous/unreadable label reached the native host")
        self.assertFalse(self.binding.exists(), "a third legacy binding was created")
        self.assertEqual({p.name: p.read_bytes() for p in folder.glob("*.json")}, before)

    def test_ordinary_launch_refuses_native_twins_before_host_contact(self):
        self.ambiguous_launch(False)
    def test_new_launch_refuses_native_twins_before_host_contact(self):
        self.ambiguous_launch(True)
    def test_ordinary_launch_refuses_unreadable_native_labels(self):
        self.ambiguous_launch(False, damaged=True)
    def test_new_launch_refuses_unreadable_native_labels(self):
        self.ambiguous_launch(True, damaged=True)

    def test_fork_cannot_treat_unreadable_target_coverage_as_a_free_label(self):
        self.native = True
        index = self.root / "state/ids"
        index.mkdir()
        (index / (self.record["session_id"] + ".json")).write_text(json.dumps(
            dict(version=1, kind="dsh", name=self.name)))
        folder = self.root / "state/dsh/sessions"
        folder.mkdir()
        bad = folder / (str(uuid.uuid4()) + ".json")
        bad.write_text("{damaged")
        original = self.binding.read_bytes()
        refused = self.hgs("fork", self.name, "--session-id", self.record["session_id"], "-n", "copy", "-d")
        self.assertNotEqual(refused.returncode, 0)
        self.assertFalse([c for c in self.calls if c["method"] == "fork"])
        self.assertEqual(self.binding.read_bytes(), original)
        self.assertEqual(bad.read_text(), "{damaged")
        self.assertFalse(list((self.root / "state/dsh/forks").glob("*.json")))

    def test_an_old_binding_gets_a_stable_source_id_only_when_fork_is_admitted(self):
        self.native = True
        self.record.pop("session_id")
        self.binding.write_text(json.dumps(self.record))
        original = self.binding.read_bytes()
        dry = self.hgs("fork", self.name, "-n", "copy", "--dry-run")
        self.assertEqual(dry.returncode, 0, dry.stderr)
        stale = self.hgs("fork", self.name, "-n", "copy", "-d", "--expected-run-id", "wrong-run")
        self.assertNotEqual(stale.returncode, 0)
        self.assertEqual(self.binding.read_bytes(), original)
        self.assertEqual(self.calls, [])
        self.fork_reply = False
        for _ in range(2):
            lost = self.hgs("fork", self.name, "-n", "copy", "-d")
            self.assertIn("delivery uncertain", lost.stderr)
        saved = json.loads(self.binding.read_text())
        sid = saved.pop("session_id")
        self.assertEqual(str(uuid.UUID(sid)), sid)
        self.assertEqual(saved, self.record)
        forks = [c["params"] for c in self.calls if c["method"] == "fork"]
        self.assertEqual(len(forks), 2)
        self.assertEqual(forks[0], forks[1])

    def test_a_legacy_unknown_intent_is_migrated_not_erased_or_reminted(self):
        self.native = True
        self.fork_reply = False
        pending = self.root / "state/dsh/forks"
        pending.mkdir()
        request = str(uuid.uuid4())
        target = "dsh/example/copy"
        old = pending / (hashlib.sha256((self.name + "\n" + target).encode()).hexdigest() + ".json")
        old.write_text(json.dumps(dict(request_id=request, source=self.name,
            source_conversation_id=self.record["conversation_id"], target=target, generation=self.generation)))
        original = old.read_bytes()
        renamed = self.hgs("rename", self.name, "dsh/example/other")
        self.assertNotEqual(renamed.returncode, 0)
        self.assertIn("original legacy fork", renamed.stderr)
        self.assertEqual(self.calls, [])
        self.assertEqual(old.read_bytes(), original)
        # The exact retry upgrades only the storage key and keeps the native ID.
        retry = self.hgs("fork", self.name, "-n", "copy", "-d")
        self.assertIn("delivery uncertain", retry.stderr)
        self.assertFalse(old.exists())
        [modern] = pending.glob("*.json")
        self.assertEqual(json.loads(modern.read_bytes())["request_id"], request)
        self.assertEqual([c["params"]["requestId"] for c in self.calls if c["method"] == "fork"], [request])
        # A malformed original is never treated as an absent one.
        modern.write_text("null")
        bad = self.hgs("fork", self.name, "-n", "copy", "-d")
        self.assertNotEqual(bad.returncode, 0)
        self.assertEqual(modern.read_text(), "null")
        self.assertEqual(len([c for c in self.calls if c["method"] == "fork"]), 1)

    def test_a_legacy_unknown_cannot_disappear_behind_a_new_suffix(self):
        self.native = True
        pending = self.root / "state/dsh/forks"
        pending.mkdir()
        target = "dsh/example/copy"
        old = pending / (hashlib.sha256((self.name + "\n" + target).encode()).hexdigest() + ".json")
        old.write_text(json.dumps(dict(request_id=str(uuid.uuid4()), source=self.name,
            source_conversation_id=self.record["conversation_id"], target=target, generation=self.generation)))
        original = old.read_bytes()
        other = dict(self.record, name=target, conversation_id="session-other", session_id=str(uuid.uuid4()))
        (self.binding.parent / (hashlib.sha256(target.encode()).hexdigest() + ".json")).write_text(json.dumps(other))
        refused = self.hgs("fork", self.name, "-n", "copy", "-d")
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("legacy fork", refused.stderr)
        self.assertEqual(old.read_bytes(), original)
        self.assertFalse([c for c in self.calls if c["method"] == "fork"])

    def test_an_unknown_default_fork_retains_its_uuid_across_source_rename(self):
        self.native = True
        self.fork_reply = False
        self.binding.unlink()
        folder = self.root / "state/dsh/sessions"
        folder.mkdir()
        record = dict(self.record, layout="id", journal_name=self.record["session_id"])
        path = folder / (record["session_id"] + ".json")
        path.write_text(json.dumps(record))
        lost = self.hgs("fork", self.name, "--session-id", record["session_id"], "-d")
        self.assertIn("delivery uncertain", lost.stderr)
        pending = list((self.root / "state/dsh/forks").glob("*.json"))
        self.assertEqual(len(pending), 1)
        original = pending[0].read_bytes()
        new_name = "dsh/example/renamed"
        renamed = self.hgs("session-action", self.name, "--session-id", record["session_id"], "--json", request={
            "request_id": str(uuid.uuid4()), "action": "rename", "new_name": new_name,
            "expected_run_id": record["run_id"], "expected_conversation_id": record["conversation_id"]})
        self.assertEqual(renamed.returncode, 0, renamed.stderr)
        self.assertEqual(json.loads(renamed.stdout)["status"], "completed", renamed.stdout)
        self.assertEqual(json.loads(path.read_text())["name"], new_name)
        repeated = self.hgs("fork", new_name, "--session-id", record["session_id"], "-d")
        self.assertIn("delivery uncertain", repeated.stderr)
        self.assertEqual(pending[0].read_bytes(), original)
        forks = [c["params"] for c in self.calls if c["method"] == "fork"]
        self.assertEqual(len(forks), 2)
        self.assertEqual(forks[0], forks[1])

    def test_label_twins_keep_distinct_unknown_forks_and_the_original_retry_uuid(self):
        self.native = True
        self.fork_reply = False
        self.binding.unlink()
        folder = self.root / "state/dsh/sessions"
        folder.mkdir()
        first = dict(self.record, layout="id", journal_name=self.record["session_id"])
        second = dict(first, session_id=str(uuid.uuid4()), conversation_id="session-second", run_id="run-second")
        for record in (first, second):
            (folder / (record["session_id"] + ".json")).write_text(json.dumps(record))
        def fork(record):
            return self.hgs("fork", self.name, "--session-id", record["session_id"], "-n", "copy", "-d")
        one, two = fork(first), fork(second)
        for result in (one, two):
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn("delivery uncertain", result.stderr)
        pending = self.root / "state/dsh/forks"
        original = {p: p.read_bytes() for p in pending.glob("*.json")}
        self.assertEqual(len(original), 2)
        self.assertEqual({json.loads(b)["source_session_id"] for b in original.values()},
                         {first["session_id"], second["session_id"]})
        calls = [c["params"] for c in self.calls if c["method"] == "fork"]
        self.assertEqual([c["sessionId"] for c in calls], [first["conversation_id"], second["conversation_id"]])
        self.assertNotEqual(calls[0]["requestId"], calls[1]["requestId"])
        again = fork(first)
        self.assertNotEqual(again.returncode, 0)
        self.assertEqual({p: p.read_bytes() for p in pending.glob("*.json")}, original)
        calls = [c["params"] for c in self.calls if c["method"] == "fork"]
        self.assertEqual(calls[2], calls[0])
        self.fork_reply = True
        adopted = fork(first)
        self.assertEqual(adopted.returncode, 0, adopted.stderr)
        calls = [c["params"] for c in self.calls if c["method"] == "fork"]
        self.assertEqual(calls[3], calls[0])
        remaining = list(pending.glob("*.json"))
        self.assertEqual(len(remaining), 1)
        self.assertEqual(remaining[0].read_bytes(), original[remaining[0]])
        # The first adopted child now holds that label. The second unknown
        # intent cannot silently allocate copy-2 and issue a new native UUID.
        blocked = fork(second)
        self.assertNotEqual(blocked.returncode, 0)
        self.assertIn("original fork destination", blocked.stderr)
        self.assertEqual(len([c for c in self.calls if c["method"] == "fork"]), 4)
        self.assertEqual(remaining[0].read_bytes(), original[remaining[0]])
        for record in (first, second):
            self.assertEqual(json.loads((folder / (record["session_id"] + ".json")).read_text()), record)


if __name__ == "__main__":
    unittest.main()
