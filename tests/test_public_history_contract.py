"""Public history ABI on synthetic private files; no provider or tmux is started.

Persisted binding changes model a confirmed same-conversation resume. These tests
exercise indexing and wire identity, not lifecycle execution or model traffic.
"""
import hashlib
from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import unittest
import uuid


REPO = Path(__file__).resolve().parents[1]
HGS = Path(os.environ.get("HGS_TEST_BIN", REPO / "target/debug/hgs")).resolve()


class PublicHistoryContract(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="hgs-public-history-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.state = self.root / "state"
        self.state.mkdir(mode=0o700)
        socket = self.root / "tmux-private"
        socket.mkdir(mode=0o700)
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith("HGS_") and key not in ("TMUX", "TMUX_PANE", "TMUX_TMPDIR")}
        self.env.update(HOME=str(self.root), HGS_STATE_DIR=str(self.state),
                        HGS_CONFIG_DIR=str(self.root / "config"), HGS_SELF="history-fixture",
                        HGS_PEERS="", TMUX_TMPDIR=str(socket))
        self.name = "codex/history-fixture/mobile"
        self.run = str(uuid.uuid4())
        self.conversation = str(uuid.uuid4())
        self.transcript = self.root / f"rollout-{self.conversation}.jsonl"
        self.binding = dict(version=1, name=self.name, agent="codex", run_id=self.run,
                            conversation_id=self.conversation, agent_home=str(self.root / ".codex"),
                            account_id="synthetic", transcript=str(self.transcript), state="stopped")
        self.write_binding()
        self.write_source([])

    def write_binding(self, name=None, **updates):
        self.binding.update(updates)
        if name is not None:
            self.binding["name"] = name
        path = self.state / (hashlib.sha256(self.binding["name"].encode()).hexdigest() + ".json")
        path.write_text(json.dumps(self.binding), encoding="utf-8")
        path.chmod(0o600)

    @staticmethod
    def message(index, incoming=True, at=None, text=None, message_id=None):
        return {"type": "event_msg", "timestamp": at if at is not None else 1_700_000_000 + index,
                "payload": {"type": "agent_message" if incoming else "user_message",
                            "id": message_id or f"public-{index}",
                            "message": text if text is not None else f"literal public message {index}"}}

    def write_source(self, rows):
        header = {"type": "session_meta", "payload": {"id": self.conversation}}
        with self.transcript.open("w", encoding="utf-8") as stream:
            for row in [header, *rows]:
                stream.write(json.dumps(row, ensure_ascii=False) + "\n")
        self.transcript.chmod(0o600)

    def append_source(self, rows):
        with self.transcript.open("a", encoding="utf-8") as stream:
            for row in rows:
                stream.write(json.dumps(row, ensure_ascii=False) + "\n")

    def journal(self, rows):
        path = self.state / "events.sqlite3"
        with closing(sqlite3.connect(path)) as db, db:
            db.execute("CREATE TABLE IF NOT EXISTS events (seq INTEGER PRIMARY KEY, name TEXT, conversation TEXT, payload TEXT)")
            for row in rows:
                db.execute("INSERT INTO events(name,conversation,payload) VALUES(?,?,?)",
                           (self.name, self.conversation, json.dumps(row)))
        path.chmod(0o600)

    def request(self, **updates):
        return {"request_id": str(uuid.uuid4()), "expected_run_id": self.run,
                "expected_conversation_id": self.conversation, "limit": 100, **updates}

    def seed_voice_metadata(self, count, repeated=()):
        """Synthetic accepted metadata only: no worker, ASR, media or native input."""
        directory = self.state / "voice"
        directory.mkdir(mode=0o700)
        path = directory / "messages.sqlite3"
        source = self.transcript.stat()
        ids = []
        with closing(sqlite3.connect(path)) as db, db:
            db.execute("CREATE TABLE messages(id TEXT PRIMARY KEY,digest TEXT NOT NULL,state TEXT NOT NULL,"
                       "created REAL NOT NULL,revision INTEGER NOT NULL,conversation TEXT NOT NULL,body TEXT NOT NULL,"
                       "audio BLOB NOT NULL,change_seq INTEGER NOT NULL,waveform_version INTEGER NOT NULL,video BLOB NOT NULL)")
            for index in range(count):
                identity = str(uuid.uuid4())
                ids.append(identity)
                at = 1_700_000_000 + (0 if index in repeated else index) * 60
                job = dict(id=identity, name=self.name, run_id=self.run, conversation_id=self.conversation,
                    agent="codex", account_id="synthetic", agent_home=self.binding["agent_home"],
                    created_at=1_700_000_000 + index * 60, revision=1, state="submitted",
                    transcript="repeated voice" if index in repeated else f"voice transcript {index}",
                    audio=dict(request_id=identity, index=0, name="voice.wav", mime="audio/wav", bytes=44,
                               duration_ms=1, waveform=[0] * 64),
                    native_submitted_at=at, native_receipt_status="submitted",
                    boundary=dict(transcript=str(self.transcript), source_device=source.st_dev,
                                  source_inode=source.st_ino, source_bytes=0, journal_seq=0))
                db.execute("INSERT INTO messages VALUES(?,?,?,?,?,?,?,?,?,?,?)", (
                    identity, "synthetic", "submitted", job["created_at"], 1, self.conversation,
                    json.dumps(job), b"", index + 1, 1, b""))
        path.chmod(0o600)
        return ids

    def voice_metadata(self, identity):
        with closing(sqlite3.connect(self.state / "voice/messages.sqlite3")) as db:
            return json.loads(db.execute("SELECT body FROM messages WHERE id=?", (identity,)).fetchone()[0])

    def test_old_voice_provenance_beyond_newest100_is_reconciled(self):
        self.write_source([self.message(0, incoming=False, at=1_700_000_000,
                                        text="voice transcript 0", message_id="old-native")])
        ids = self.seed_voice_metadata(250)
        head = self.complete()
        self.assertEqual(self.voice_metadata(ids[0]).get("native_binding", {}).get("provider_message_id"), "old-native")
        rows = self.all_older(head)
        self.assertEqual(len(rows), 250)
        self.assertNotIn("old-native", [row.get("message_id") for row in rows])

    def test_cross_batch_repeated_voice_text_does_not_establish_unique_proof(self):
        self.write_source([self.message(0, incoming=False, at=1_700_000_000,
                                        text="repeated voice", message_id="ambiguous-native")])
        ids = self.seed_voice_metadata(250, repeated=(0, 200))
        rows = self.all_older(self.complete())
        for index in (0, 200):
            self.assertNotIn("native_binding", self.voice_metadata(ids[index]))
        self.assertEqual(len(rows), 251)

    def test_old_unbound_voice_is_revisited_when_native_evidence_arrives(self):
        ids = self.seed_voice_metadata(250)
        self.complete()
        self.assertNotIn("native_binding", self.voice_metadata(ids[0]))
        self.append_source([self.message(0, incoming=False, at=1_700_000_000,
                                         text="voice transcript 0", message_id="late-old-native")])
        self.complete()
        self.assertEqual(self.voice_metadata(ids[0])["native_binding"]["provider_message_id"], "late-old-native")

    def test_bound_old_voice_does_not_compete_for_a_later_repeated_native_message(self):
        self.write_source([self.message(0, incoming=False, at=1_700_000_000, text="repeated voice", message_id="first-native"),
                           self.message(1, incoming=False, at=1_700_000_005, text="repeated voice", message_id="ordinary-native")])
        start = self.transcript.stat().st_size
        ids = self.seed_voice_metadata(250, repeated=(0, 200))
        with closing(sqlite3.connect(self.state / "voice/messages.sqlite3")) as db, db:
            for index in (0, 200):
                job = json.loads(db.execute("SELECT body FROM messages WHERE id=?", (ids[index],)).fetchone()[0])
                if index == 0:
                    job["native_binding"] = dict(provider_message_id="first-native", provider_source="codex_transcript", provider_source_offset="0:0", journal_seq=1)
                else:
                    job["native_submitted_at"] = 1_700_000_010
                    job["boundary"]["source_bytes"] = start
                    job["boundary"]["journal_seq"] = 2
                    job["native_binding"] = dict(journal_seq=3)
                db.execute("UPDATE messages SET body=? WHERE id=?", (json.dumps(job), ids[index]))
        self.append_source([self.message(2, incoming=False, at=1_700_000_011, text="repeated voice", message_id="second-native")])
        rows = self.all_older(self.complete())
        self.assertEqual(self.voice_metadata(ids[0])["native_binding"]["provider_message_id"], "first-native")
        self.assertEqual(self.voice_metadata(ids[200])["native_binding"].get("provider_message_id"), "second-native")
        self.assertEqual(len(rows), 251)
        self.assertIn("ordinary-native", [row.get("message_id") for row in rows])

    def test_native_updates_do_not_starve_the_current_old_voice_sweep(self):
        ids = self.seed_voice_metadata(350)
        self.history()
        for index in range(8):
            rows = [self.message(index + 500, incoming=False, at=1_700_100_000 + index, text=f"ordinary live prompt {index}")]
            if index == 0:
                rows.append(self.message(200, incoming=False, at=1_700_012_000, text="voice transcript 200", message_id="native-during-sweep"))
            self.append_source(rows)
            self.history()
        self.assertEqual(self.voice_metadata(ids[200]).get("native_binding", {}).get("provider_message_id"), "native-during-sweep")
        self.complete()

    def test_old_voice_reconciliation_survives_index_recreation_and_source_replacement(self):
        original = self.message(0, incoming=False, at=1_700_000_000,
                                text="voice transcript 0", message_id="retained-native-id")
        self.write_source([original])
        ids = self.seed_voice_metadata(250)
        self.complete()
        self.assertEqual(self.voice_metadata(ids[0])["native_binding"]["provider_message_id"], "retained-native-id")
        indexes = list((self.state / "public_history").glob("*.sqlite3"))
        self.assertEqual(len(indexes), 1)
        indexes[0].unlink()
        rows = self.all_older(self.complete())
        self.assertEqual(len(rows), 250)
        # Same retained native identity in a replaced file is still authoritative;
        # the old offset alone must not authorize a different native message.
        retired = self.transcript.with_suffix(".old")
        self.transcript.rename(retired)
        self.write_source([original, self.message(1, incoming=False, at=1_700_000_000,
                            text="voice transcript 0", message_id="replacement-other-id")])
        rows = self.all_older(self.complete())
        self.assertEqual(len(rows), 251)
        self.assertIn("replacement-other-id", [row.get("message_id") for row in rows])
        self.assertEqual(self.voice_metadata(ids[0])["native_binding"]["provider_message_id"], "retained-native-id")

    def test_voice_reconciliation_checkpoint_and_candidate_queries_are_bounded(self):
        self.seed_voice_metadata(350)
        positions = []
        for _ in range(15):
            page = self.history()
            index = next((self.state / "public_history").glob("*.sqlite3"))
            with closing(sqlite3.connect(index)) as db:
                entry = db.execute("SELECT value FROM meta WHERE key='voice_reconcile_after'").fetchone()
                positions.append(int(entry[0]) if entry else 0)
            if page["head"]["complete"]:
                break
        else:
            self.fail("bounded metadata reconciliation did not finish")
        self.assertEqual(positions[-1], 350)
        self.assertTrue(all(0 <= second-first <= 100 for first, second in zip([0]+positions, positions)))
        with closing(sqlite3.connect(self.state / "voice/messages.sqlite3")) as db:
            for name, clause, params in [
                ("voice_native_time", "json_extract(body,'$.native_submitted_at') BETWEEN ? AND ?", (0, 1)),
                ("voice_native_provider", "json_extract(body,'$.native_binding.provider_source')=? AND json_extract(body,'$.native_binding.provider_message_id')=?", ("source", "id")),
                ("voice_native_journal", "json_extract(body,'$.native_binding.journal_seq')=?", (1,)),
            ]:
                plan = db.execute("EXPLAIN QUERY PLAN SELECT id,body FROM messages WHERE state='submitted' "
                                  "AND conversation=? AND " + clause + " LIMIT 65", (self.conversation, *params)).fetchall()
                self.assertIn(name, repr(plan))

    def history(self, payload=None, reject=False, name=None, **updates):
        payload = payload or self.request(**updates)
        result = subprocess.run([str(HGS), "history", name or self.name, "--json"],
                                input=json.dumps(payload), text=True, capture_output=True,
                                env=self.env, timeout=15)
        if reject:
            self.assertNotEqual(result.returncode, 0, result.stdout)
            return result.stderr
        self.assertEqual(result.returncode, 0, result.stderr)
        body = json.loads(result.stdout)
        self.assertEqual(body["request_id"], payload["request_id"])
        self.assertEqual(body["name"], name or self.name)
        self.assertEqual(body["run_id"], payload["expected_run_id"])
        self.assertEqual(body["conversation_id"], payload["expected_conversation_id"])
        self.assertLessEqual(len(body["events"]), payload.get("limit", 100))
        return body

    def test_recent_inspection_does_not_wait_for_historical_index_or_shared_receipts(self):
        """The initial mobile transcript can use inspect while older pages build.

        The latest provider progress is after a large private tool result and
        the latest journal prompt is before a long tool-only timeline. Neither
        needs the public_history index or unrelated delivery classification.
        """
        self.write_source([self.message(0), self.large_tool_output(9 * 1024 * 1024),
                           self.message(1, text="Newest native progress after large private output.",
                                        message_id="newest-native-progress")])
        self.journal([dict(type="UserPromptSubmit", agent_id="", at=1_700_000_002,
                           detail="Newest person prompt before tool activity."),
                      *[dict(type="PostToolUse", agent_id="", at=1_700_000_003 + index,
                             detail="Private tool-only activity.") for index in range(150)]])
        receipts = self.state / "input_receipts"
        receipts.mkdir(mode=0o700)
        for index in range(130):
            path = receipts / f"unrelated-{index:03}.json"
            path.write_text(json.dumps(dict(status="submitted", conversation_id="another-conversation",
                                            agent_id="", attachments=[])), encoding="utf-8")
            path.chmod(0o600)

        def inspect():
            result = subprocess.run([str(HGS), "inspect", self.name, "--skip-processes"],
                                    text=True, capture_output=True, env=self.env, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
            body = json.loads(result.stdout)
            self.assertEqual((body["run_id"], body["conversation_id"]), (self.run, self.conversation))
            self.assertIn("newest-native-progress", [row.get("message_id")
                                                    for row in body["provider_messages"]])
            self.assertIn("Newest person prompt before tool activity.",
                          [row.get("detail") for row in body["message_events"]])
            self.assertNotIn("PRIVATE TOOL RESULT", result.stdout)
            self.assertLessEqual(len(body["provider_messages"]), 100)
            self.assertLessEqual(len(body["message_events"]), 100)
            return body

        inspect()
        self.assertFalse((self.state / "public_history").exists(),
                         "A recent inspection must not create a historical index")
        page = self.history()
        self.assertFalse(page["head"]["complete"])
        # The attachment source may be complete at once (the locator scopes
        # the 130 shared receipts out of this conversation) or indexing while
        # its private discovery builds; the inspect above never waits for it.
        self.assertIn(page["source_status"]["attachments"], ("indexing", "complete"))
        progress = page["source_status"]["progress"]
        self.assertLess(progress["source_bytes"], progress["known_source_bytes"])
        self.assertLess(progress["receipt_files"], 130)
        index = next((self.state / "public_history").glob("*.sqlite3"))
        with closing(sqlite3.connect(index)) as db:
            before = dict(db.execute("SELECT key,value FROM meta WHERE key LIKE 'offset:%'"))
            classified = db.execute("SELECT count(*) FROM receipt_seen").fetchone()[0]
        inspect()
        with closing(sqlite3.connect(index)) as db:
            self.assertEqual(dict(db.execute("SELECT key,value FROM meta WHERE key LIKE 'offset:%'")), before)
            self.assertEqual(db.execute("SELECT count(*) FROM receipt_seen").fetchone()[0], classified)

    def complete(self, **updates):
        # Each call has bounded source/reconciliation work. Completion is
        # explicit; a page that happens to have 100 rows is not proof of it.
        for _ in range(100):
            page = self.history(**updates)
            if page["head"]["complete"]:
                self.assertFalse(page["indexing"])
                return page
            self.assertIsNone(page["head"]["incoming_seq"])
            self.assertIsNone(page["head"]["total_incoming"])
        self.fail("synthetic history indexing did not finish within bounded calls")

    def all_older(self, head):
        rows = list(head["events"])
        page = head
        seen = set()
        for _ in range(100):
            if not page["has_more_before"]:
                self.assertIsNone(page["next_before"])
                return rows
            cursor = page["next_before"]
            self.assertNotIn(cursor, seen)
            seen.add(cursor)
            page = self.history(before=cursor)
            self.assertEqual(page["history_epoch"], head["history_epoch"])
            self.assertEqual(page["head"], head["head"])
            rows = page["events"] + rows
        self.fail("older paging failed to make progress")

    @staticmethod
    def large_tool_output(size=3 * 1024 * 1024):
        return {"type": "response_item", "payload": {"type": "custom_tool_call_output",
                "call_id": "synthetic-large-tool", "output": "PRIVATE TOOL RESULT " + "x" * size}}

    def legacy_oversized_gap(self):
        path = next((self.state / "public_history").glob("*.sqlite3"))
        with closing(sqlite3.connect(path)) as db, db:
            db.execute("INSERT OR REPLACE INTO meta(key,value) VALUES('gap','oversized_source_line')")
            db.execute("DELETE FROM meta WHERE key='source_reader_version'")

    def test_large_private_tool_rows_do_not_block_latest_or_read_sequence(self):
        self.write_source([self.message(0), self.large_tool_output(), self.message(1)])
        page = self.complete()
        self.assertEqual([row["detail"] for row in page["events"]], ["literal public message 0", "literal public message 1"])
        self.assertEqual([row["incoming_seq"] for row in page["events"]], [1, 2])
        self.assertEqual(page["head"]["total_incoming"], 2)
        self.assertNotIn("PRIVATE TOOL RESULT", json.dumps(page))
        self.assertNotIn("gap", page["source_status"])

    def test_legacy_oversized_gap_replays_without_resetting_public_identity(self):
        self.write_source([self.message(0), self.large_tool_output(), self.message(1)])
        before = self.complete()
        self.legacy_oversized_gap()
        repairing = self.history()
        self.assertFalse(repairing["head"]["complete"])
        self.assertTrue(repairing["indexing"])
        self.assertIsNone(repairing["head"]["total_incoming"])
        after = self.complete()
        self.assertEqual(after["history_epoch"], before["history_epoch"])
        self.assertEqual([(row["history_id"], row["incoming_seq"]) for row in after["events"]],
                         [(row["history_id"], row["incoming_seq"]) for row in before["events"]])
        anchored = self.history(around=before["events"][0]["history_cursor"])
        self.assertEqual(anchored["history_epoch"], before["history_epoch"])
        unread = self.history(around_incoming_seq=1, history_epoch=before["history_epoch"])
        self.assertEqual(unread["head"]["total_incoming"], 2)

    def test_legacy_gap_repair_checks_old_source_before_rewinding(self):
        self.write_source([self.message(0), self.large_tool_output(), self.message(1)])
        before = self.complete()
        self.legacy_oversized_gap()
        self.write_source([self.message(0), self.large_tool_output(), self.message(1, text="changed public reply")])
        reset = self.history()
        self.assertEqual(reset["source_status"]["provider"], "source_reset")
        self.assertNotEqual(reset["history_epoch"], before["history_epoch"])
        after = self.complete()
        self.assertEqual(after["events"][-1]["detail"], "changed public reply")

    def test_legacy_mid_row_skip_repair_keeps_existing_identity(self):
        self.write_source([self.message(0), self.large_tool_output(), self.message(1)])
        before = self.complete()
        self.legacy_oversized_gap()
        source = self.transcript.read_bytes()
        offset = source.index(b'{"type": "response_item"') + 2 * 1024 * 1024
        path = next((self.state / "public_history").glob("*.sqlite3"))
        with closing(sqlite3.connect(path)) as db, db:
            for key, value in [("offset:0", str(offset)), ("skip:0", "1"),
                               ("boundary:0", hashlib.sha256(source[offset - 256:offset]).hexdigest())]:
                db.execute("INSERT OR REPLACE INTO meta(key,value) VALUES(?,?)", (key, value))
        after = self.complete()
        self.assertEqual(after["history_epoch"], before["history_epoch"])
        self.assertEqual([(row["history_id"], row["incoming_seq"]) for row in after["events"]],
                         [(row["history_id"], row["incoming_seq"]) for row in before["events"]])

    def test_legacy_gap_repair_rejects_changed_checkpoint_in_an_appended_file(self):
        self.write_source([self.message(0), self.large_tool_output(), self.message(1)])
        before = self.complete()
        self.legacy_oversized_gap()
        source = self.transcript.read_bytes()
        changed = source.replace(b"literal public message 1", b"altered public message 1")
        self.assertEqual(len(changed), len(source))
        self.transcript.write_bytes(changed)
        self.append_source([self.message(2)])
        reset = self.history()
        self.assertEqual(reset["source_status"]["provider"], "source_reset")
        self.assertNotEqual(reset["history_epoch"], before["history_epoch"])
        self.assertEqual(self.complete()["head"]["total_incoming"], 3)

    def test_large_public_row_is_retained_as_an_explicit_bounded_projection(self):
        text = "Public reply " + "z" * (3 * 1024 * 1024)
        self.write_source([self.message(0, text=text), self.message(1)])
        page = self.complete()
        self.assertEqual(page["head"]["total_incoming"], 2)
        self.assertTrue(page["events"][0]["detail_truncated"])
        self.assertLess(len(json.dumps(page).encode()), 1024 * 1024)
        self.assertEqual(page["events"][1]["detail"], "literal public message 1")
        self.assertIn(text, self.transcript.read_text())

    def test_large_malformed_row_remains_a_blocking_gap(self):
        self.write_source([self.message(0)])
        with self.transcript.open("a") as stream:
            stream.write(json.dumps(self.large_tool_output())[:-1] + "\n")
        self.append_source([self.message(1)])
        for _ in range(8):
            page = self.history()
        self.assertEqual(page["source_status"]["gap"], "malformed_source_line")
        self.assertFalse(page["head"]["complete"])
        self.assertFalse(page["indexing"])
        self.assertIsNone(page["head"]["incoming_seq"])

    def test_large_final_row_waits_for_newline_without_a_permanent_gap(self):
        self.write_source([])
        with self.transcript.open("a") as stream:
            stream.write(json.dumps(self.message(0, text="Public reply " + "z" * (3 * 1024 * 1024))))
        for _ in range(3):
            pending = self.history()
        self.assertFalse(pending["head"]["complete"])
        self.assertNotIn("gap", pending["source_status"])
        self.assertEqual(pending["source_status"]["provider"], "partial_final_line")
        with self.transcript.open("a") as stream:
            stream.write("\n")
        complete = self.complete()
        self.assertEqual(complete["head"]["total_incoming"], 1)

    def test_exact_large_row_cap_completes_but_cap_plus_one_stays_blocked(self):
        for extra in (0, 1):
            with self.subTest(extra=extra):
                row = self.large_tool_output(0)
                prefix_size = len((json.dumps(row) + "\n").encode())
                row["payload"]["output"] += "x" * (16 * 1024 * 1024 + extra - prefix_size)
                self.write_source([self.message(0), row, self.message(1)])
                if not extra:
                    self.assertEqual(self.complete()["head"]["total_incoming"], 2)
                else:
                    for _ in range(8):
                        page = self.history()
                    self.assertEqual(page["source_status"]["gap"], "oversized_source_line")
                    self.assertFalse(page["head"]["complete"])

    def test_unsupported_large_row_legacy_repair_is_one_bounded_replay(self):
        self.write_source([self.message(0), self.large_tool_output(17 * 1024 * 1024), self.message(1)])
        for _ in range(12):
            self.history()
        self.legacy_oversized_gap()
        for _ in range(12):
            page = self.history()
        self.assertEqual(page["source_status"]["gap"], "oversized_source_line")
        self.assertFalse(page["head"]["complete"])
        at_end = page["source_status"]["progress"]["source_bytes"]
        self.assertEqual(at_end, page["source_status"]["progress"]["known_source_bytes"])
        for _ in range(2):
            self.assertEqual(self.history()["source_status"]["progress"]["source_bytes"], at_end)

    def test_long_public_history_behind_tool_noise_and_unread_watermark(self):
        source = []
        for index in range(1240):
            source.extend([
                {"type": "response_item", "payload": {"type": "function_call", "arguments": "PRIVATE TOOL NOISE"}},
                {"type": "event_msg", "payload": {"type": "agent_message", "phase": "analysis", "message": "PRIVATE ANALYSIS"}},
                self.message(index, incoming=index % 2 == 1),
            ])
        self.write_source(source)
        first = self.history()
        self.assertTrue(first["indexing"])
        self.assertFalse(first["head"]["complete"])
        self.assertIsNone(first["head"]["total_incoming"])
        head = self.complete()
        rows = self.all_older(head)
        self.assertEqual([row["detail"] for row in rows], [f"literal public message {i}" for i in range(1240)])
        self.assertEqual(len({row["history_id"] for row in rows}), 1240)
        incoming = [row for row in rows if row["type"] == "AgentMessage"]
        self.assertEqual([row["incoming_seq"] for row in incoming], list(range(1, 621)))
        self.assertTrue(all(row["incoming_seq"] is None for row in rows if row["type"] != "AgentMessage"))
        self.assertEqual(head["head"]["total_incoming"], 620)
        # A durable read-through=100 selects the global first unread reply,
        # rather than the first row of the newest bounded window.
        unread = self.history(around_incoming_seq=100, history_epoch=head["history_epoch"], limit=10)
        self.assertEqual(unread["events"][0]["incoming_seq"], 101)
        self.assertEqual(unread["events"][0]["detail"], "literal public message 201")
        self.assertEqual(unread["head"], head["head"])
        serialized = json.dumps(rows)
        self.assertNotIn("PRIVATE TOOL NOISE", serialized)
        self.assertNotIn("PRIVATE ANALYSIS", serialized)

    def test_equal_timestamp_order_and_both_cursor_directions(self):
        self.write_source([self.message(index, at=100, message_id=f"reverse-{30-index:02}") for index in range(30)])
        head = self.complete(limit=7)
        rows = self.all_older(head)
        self.assertEqual([row["detail"] for row in rows], [f"literal public message {i}" for i in range(30)])
        anchor = rows[9]
        around = self.history(around=anchor["history_cursor"], limit=3)
        self.assertEqual([row["history_id"] for row in around["events"]], [row["history_id"] for row in rows[9:12]])
        before = self.history(before=anchor["history_cursor"], limit=3)
        after = self.history(after=anchor["history_cursor"], limit=3)
        self.assertEqual([row["history_id"] for row in before["events"]], [row["history_id"] for row in rows[6:9]])
        self.assertEqual([row["history_id"] for row in after["events"]], [row["history_id"] for row in rows[10:13]])

    def test_identity_cursor_authentication_and_selector_rejection(self):
        self.write_source([self.message(index) for index in range(4)])
        head = self.complete()
        cursor = head["events"][1]["history_cursor"]
        self.history(self.request(expected_run_id=str(uuid.uuid4())), reject=True)
        self.history(self.request(expected_conversation_id=str(uuid.uuid4())), reject=True)
        self.history(self.request(before=cursor, after=cursor), reject=True)
        self.history(self.request(around_incoming_seq=0), reject=True)
        self.history(self.request(around_incoming_seq=0, history_epoch=str(uuid.uuid4())), reject=True)
        self.history(self.request(before=cursor[:-1] + ("0" if cursor[-1] != "0" else "1")), reject=True)
        # A different provider conversation gets a different cursor secret.
        original = dict(self.binding)
        other = str(uuid.uuid4())
        other_source = self.root / f"rollout-{other}.jsonl"
        other_source.write_text(json.dumps({"type": "session_meta", "payload": {"id": other}}) + "\n")
        other_name = "codex/history-fixture/other"
        self.write_binding(name=other_name, conversation_id=other, transcript=str(other_source))
        self.history(self.request(expected_conversation_id=other, before=cursor), name=other_name, reject=True)
        self.binding = original

    def test_stable_ids_across_repeat_and_same_conversation_resume(self):
        self.write_source([self.message(index) for index in range(8)])
        before = self.complete()
        repeated = self.history()
        self.assertEqual(repeated["events"], before["events"])
        self.run = str(uuid.uuid4())
        self.write_binding(run_id=self.run)
        resumed = self.complete()
        self.assertEqual(resumed["history_epoch"], before["history_epoch"])
        self.assertEqual(resumed["events"], before["events"])
        self.history(self.request(expected_run_id=before["run_id"]), reject=True)
        anchored = self.history(around=before["events"][3]["history_cursor"])
        self.assertEqual(anchored["events"][0]["history_id"], before["events"][3]["history_id"])

    def test_source_reset_rejects_old_epoch_and_cursors(self):
        self.write_source([self.message(index) for index in range(40)])
        before = self.complete()
        old_cursor = before["events"][10]["history_cursor"]
        self.write_source([self.message(999, text="new source after reset")])
        reset = self.history()
        self.assertFalse(reset["head"]["complete"])
        self.assertEqual(reset["source_status"]["provider"], "source_reset")
        self.assertNotEqual(reset["history_epoch"], before["history_epoch"])
        after = self.complete()
        self.assertEqual([row["detail"] for row in after["events"]], ["new source after reset"])
        self.history(self.request(before=old_cursor), reject=True)
        self.history(self.request(around_incoming_seq=0, history_epoch=before["history_epoch"]), reject=True)

    def test_late_public_reply_preserves_stop_identity_and_read_sequence(self):
        self.journal([{"type": "Stop", "at": 100, "detail": "A unique short reply"}])
        before = self.complete()
        hook = before["events"][0]
        self.append_source([self.message(0, at=101, text="A unique short reply with full provider detail")])
        after = self.complete()
        self.assertEqual(len(after["events"]), 1)
        row = after["events"][0]
        self.assertEqual(row["history_id"], hook["history_id"])
        self.assertEqual(row["incoming_seq"], hook["incoming_seq"])
        self.assertEqual(after["history_epoch"], before["history_epoch"])
        self.assertEqual(row["type"], "AgentMessage")
        self.assertEqual(row["detail"], "A unique short reply with full provider detail")
        self.assertTrue(set(hook["original_ids"]).issubset(row["original_ids"]))
        self.assertTrue(any(alias.startswith("provider:codex_transcript:") for alias in row["original_ids"]))

    def test_ambiguous_repeated_replies_are_not_arbitrarily_aliased(self):
        self.journal([{"type": "Stop", "at": 100, "detail": "Repeated reply"},
                      {"type": "Stop", "at": 101, "detail": "Repeated reply"}])
        self.write_source([self.message(0, at=100, text="Repeated reply"), self.message(1, at=101, text="Repeated reply")])
        page = self.complete()
        self.assertEqual(len(page["events"]), 4)
        self.assertEqual(len({row["history_id"] for row in page["events"]}), 4)
        self.assertTrue(all(len(row["original_ids"]) == 1 for row in page["events"]))

    def test_late_stop_does_not_create_phantom_unread_after_provider_reply(self):
        self.write_source([self.message(0, at=100, text="Unique public reply with full details")])
        before = self.complete()
        provider = before["events"][0]
        self.assertEqual(provider["incoming_seq"], 1)
        self.journal([{"type": "Stop", "at": 101, "detail": "Unique public reply"}])
        after = self.complete()
        self.assertEqual(len(after["events"]), 1)
        self.assertEqual(after["head"], before["head"])
        self.assertEqual(after["history_epoch"], before["history_epoch"])
        row = after["events"][0]
        self.assertEqual(row["history_id"], provider["history_id"])
        self.assertEqual(row["incoming_seq"], 1)
        self.assertEqual(row["detail"], provider["detail"])
        self.assertEqual(row["type"], "AgentMessage")
        self.assertTrue(set(provider["original_ids"]).issubset(row["original_ids"]))
        self.assertTrue(any(alias.startswith("journal:") for alias in row["original_ids"]))
        read = self.history(around_incoming_seq=1, history_epoch=before["history_epoch"])
        self.assertEqual(read["head"]["incoming_seq"], 1)

    def test_same_native_message_id_body_revision_preserves_canonical_row(self):
        native_id = "stable-native-message"
        self.write_source([self.message(0, at=100, text="Partial public reply", message_id=native_id)])
        before = self.complete()
        partial = before["events"][0]
        self.append_source([self.message(0, at=100, text="Partial public reply with its final full body", message_id=native_id)])
        after = self.complete()
        self.assertEqual(len(after["events"]), 1)
        self.assertEqual(after["head"], before["head"])
        self.assertEqual(after["history_epoch"], before["history_epoch"])
        full = after["events"][0]
        self.assertEqual(full["history_id"], partial["history_id"])
        self.assertEqual(full["history_cursor"], partial["history_cursor"])
        self.assertEqual(full["incoming_seq"], partial["incoming_seq"])
        self.assertEqual(full["detail"], "Partial public reply with its final full body")
        self.assertEqual(full["message_id"], native_id)
        self.assertEqual(self.history()["events"], after["events"])

    def test_historical_backfill_changes_epoch_before_assigning_old_incoming(self):
        self.write_source([self.message(0, at=100, text="Already indexed first reply"),
                           self.message(1, at=101, text="Already indexed second reply")])
        before = self.complete()
        old_ids = [row["history_id"] for row in before["events"]]
        old_cursor = before["events"][0]["history_cursor"]
        self.assertEqual([row["incoming_seq"] for row in before["events"]], [1, 2])
        # The provider appends an earlier-timestamp public message after the
        # old complete head has already been observed/read. It must not be
        # classified as a newly arriving sequence 3 in that old epoch.
        self.append_source([self.message(2, at=50, text="Historical reply discovered later")])
        after = self.complete()
        self.assertNotEqual(after["history_epoch"], before["history_epoch"])
        self.assertEqual([row["detail"] for row in after["events"]],
                         ["Historical reply discovered later", "Already indexed first reply", "Already indexed second reply"])
        self.assertEqual([row["history_id"] for row in after["events"][1:]], old_ids)
        self.assertEqual([row["incoming_seq"] for row in after["events"]], [1, 2, 3])
        self.assertEqual(after["head"]["total_incoming"], 3)
        self.assertEqual(self.history()["source_status"]["canonical_order_reset"], "historical_backfill")
        self.history(self.request(before=old_cursor), reject=True)
        self.history(self.request(around_incoming_seq=2, history_epoch=before["history_epoch"]), reject=True)

    def test_attachment_only_public_receipt_hides_native_paths_and_bytes(self):
        request_id = str(uuid.uuid4())
        receipts = self.state / "input_receipts"
        receipts.mkdir(mode=0o700)
        private_path = str(self.root / "never-open-this-private-file")
        receipt = {"status": "submitted", "request_id": request_id, "run_id": self.run,
                   "conversation_id": self.conversation, "agent_id": "", "at": 100,
                   "text": "", "submitted_text": f'Read the attached file at "{private_path}"',
                   "attachments": [{"name": "synthetic.txt", "mime": "text/plain", "bytes": 7,
                                    "reference": "[File #1]", "path": private_path, "data_base64": "PRIVATE BYTES"}]}
        path = receipts / f"{request_id}.json"
        path.write_text(json.dumps(receipt));path.chmod(0o600)
        page = self.complete()
        self.assertEqual(len(page["events"]), 1)
        row = page["events"][0]
        self.assertEqual(row["detail"], "")
        self.assertEqual(row["message_id"], request_id)
        self.assertIsNone(row["incoming_seq"])
        self.assertEqual(row["attachments"][0]["name"], "synthetic.txt")
        self.assertNotIn(private_path, json.dumps(page))
        self.assertNotIn("PRIVATE BYTES", json.dumps(page))

    def test_delayed_attachment_acknowledgement_is_one_public_message_in_either_order(self):
        receipts = self.state / "input_receipts"
        receipts.mkdir(mode=0o700)
        for native_first in (False, True):
            request_id = str(uuid.uuid4())
            private_path = str(self.root / "attachments" / request_id / "synthetic.png")
            text = f"Compare synthetic images {native_first}"
            submitted = text + f'\n\nInspect the attached image at "{private_path}"'
            native = self.message(10 + int(native_first), incoming=False, at=1700000202 + int(native_first), text=submitted)
            receipt = {"status": "submitted", "request_id": request_id, "run_id": self.run,
                       "conversation_id": self.conversation, "agent_id": "", "submitted_at": 1700000100,
                       "text": text, "submitted_text": submitted,
                       "attachments": [{"name": "synthetic.png", "mime": "image/png", "bytes": 120, "path": private_path}]}
            if native_first:
                self.append_source([native]); self.complete()
            path = receipts / f"{request_id}.json"
            path.write_text(json.dumps(receipt)); path.chmod(0o600)
            self.complete()
            if not native_first:
                self.append_source([native])
            page = self.complete()
            matches = [row for row in page["events"] if row["detail"] == text]
            self.assertEqual(len(matches), 1)
            self.assertEqual(matches[0]["message_id"], request_id)
            self.assertNotIn(private_path, json.dumps(page))
            self.assertTrue(any(value.startswith("provider:") for value in matches[0]["original_ids"]))
        self.assertEqual(len(self.complete()["events"]), 2)

    def test_cached_delayed_attachment_projection_repairs_without_changing_epoch(self):
        request_id = str(uuid.uuid4())
        private_path = str(self.root / "attachments" / request_id / "synthetic.png")
        submitted = f'Compare images\n\nInspect the attached image at "{private_path}"'
        receipts = self.state / "input_receipts"; receipts.mkdir(mode=0o700)
        receipt = {"status": "submitted", "request_id": request_id, "run_id": self.run,
                   "conversation_id": self.conversation, "agent_id": "", "submitted_at": 100,
                   "text": "Compare images", "submitted_text": submitted,
                   "attachments": [{"name": "synthetic.png", "mime": "image/png", "bytes": 120, "path": private_path}]}
        path = receipts / f"{request_id}.json"; path.write_text(json.dumps(receipt)); path.chmod(0o600)
        before = self.complete()
        # Recreate an already-indexed v2 native row whose delayed hash was absent.
        # Its native source is unchanged: only the bounded upgrade can repair it.
        db_path = next((self.state / "public_history").glob("*.sqlite3"))
        original = "provider:codex_transcript:You:native:cached-native"
        native_id = hashlib.sha256(json.dumps([db_path.stem, original], separators=(",", ":")).encode()).hexdigest()
        native = {"type": "UserMessage", "source": "codex_transcript", "message_id": "cached-native",
                  "detail": submitted, "at": 202, "source_offset": "0:200"}
        with closing(sqlite3.connect(db_path)) as db, db:
            db.execute("INSERT INTO messages(id,at,role,stream,kind,ordinal,body) VALUES(?,?,?,?,?,?,?)",
                       (native_id, 202, "outgoing", "provider", "UserMessage", "1:00000000000000000000:00000000000000000200", json.dumps(native)))
            db.execute("INSERT INTO aliases(original,id) VALUES(?,?)", (original, native_id))
            db.execute("UPDATE meta SET value='2' WHERE key='reconcile_version'")
            db.execute("DELETE FROM reconcile_work")
        page = self.complete()
        self.assertEqual(page["history_epoch"], before["history_epoch"])
        self.assertEqual(len(page["events"]), 1)
        self.assertNotIn(private_path, json.dumps(page))
        old_id = page["events"][0]["history_id"]
        old_epoch = page["history_epoch"]
        with closing(sqlite3.connect(db_path)) as db, db:
            db.execute("UPDATE meta SET value='2' WHERE key='reconcile_version'")
        after = self.complete()
        self.assertEqual(after["history_epoch"], old_epoch)
        self.assertEqual(after["events"][0]["history_id"], old_id)
        self.assertEqual(after["head"]["total_incoming"], 0)

    def test_partial_text_is_disclosed_and_questions_are_outgoing(self):
        self.write_source([self.message(0, text="Ж" * 20000)])
        self.journal([{"type": "QuestionAnswered", "at": 101, "detail": "literal selected answer", "message_id": str(uuid.uuid4())}])
        page = self.complete()
        incoming = next(row for row in page["events"] if row["type"] == "AgentMessage")
        self.assertTrue(incoming["detail_truncated"])
        self.assertLessEqual(len(incoming["detail"].encode()), 32 * 1024)
        self.assertTrue(incoming["detail"].startswith("Ж"))
        answered = next(row for row in page["events"] if row["type"] == "QuestionAnswered")
        self.assertIsNone(answered["incoming_seq"])
        self.assertEqual(page["head"]["total_incoming"], 1)

    def receipt(self, request, conversation=None, at=1_700_000_050.0,
                text="Attached message", attachments=True):
        directory = self.state / "input_receipts"
        directory.mkdir(mode=0o700, exist_ok=True)
        value = dict(request_id=request, status="submitted", name=self.name, run_id=self.run,
                     conversation_id=conversation or self.conversation, agent_id="",
                     digest=hashlib.sha256(request.encode()).hexdigest(), first_message=False,
                     submitted_at=at, text=text, submitted_text=text,
                     attachments=([dict(name="image.png", mime="image/png", bytes=1,
                                        reference="[Image #1]", path="/private/fixture/image.png")]
                                  if attachments else []))
        path = directory / f"{request}.json"
        path.write_text(json.dumps(value), encoding="utf-8")
        path.chmod(0o600)
        return path

    def receipt_worker(self):
        result = subprocess.run([str(HGS), "__state", "receipt-index-worker"],
                                env=self.env, text=True, capture_output=True, timeout=120)
        self.assertEqual(result.returncode, 0, result.stderr)

    def indexed_receipts(self):
        index = next((self.state / "public_history").glob("*.sqlite3"))
        with closing(sqlite3.connect(index)) as db:
            rows = {row[0]: row[1] for row in db.execute(
                "SELECT json_extract(body,'$.message_id'), count(*) FROM messages "
                "WHERE json_extract(body,'$.source')='hgs_delivery' GROUP BY 1")}
            classified = db.execute("SELECT count(*) FROM receipt_seen").fetchone()[0]
        return rows, classified

    def test_canonical_history_imports_only_its_indexed_receipts(self):
        """The 10k foreign originals never enter the canonical import or receipt_seen."""
        foreign = self.state / "input_receipts"
        foreign.mkdir(mode=0o700)
        foreign_ids = set()
        for index in range(10_000):
            identity = str(uuid.uuid4())
            foreign_ids.add(identity)
            path = foreign / f"{identity}.json"
            path.write_text(json.dumps(dict(
                request_id=identity, status="submitted", name="other/foreign",
                run_id=str(uuid.uuid4()), conversation_id="foreign-conversation", agent_id="",
                digest=hashlib.sha256(str(index).encode()).hexdigest(), first_message=False,
                submitted_at=1_600_000_000.0 + index, text="foreign", submitted_text="foreign",
                attachments=[])), encoding="utf-8")
            path.chmod(0o600)
        matching = [str(uuid.uuid4()) for _ in range(2)]
        for request in matching:
            self.receipt(request)
        self.receipt_worker()
        page = self.complete()
        self.assertFalse(page["indexing"])
        self.assertEqual(page["source_status"]["attachments"], "complete")
        rows, classified = self.indexed_receipts()
        self.assertEqual(sorted(rows), sorted(matching))
        self.assertTrue(all(count == 1 for count in rows.values()))
        # The locator scopes classification to this conversation: the old
        # global directory pass would have stored all 10,002 originals here.
        self.assertEqual(classified, 2)
        self.assertFalse(foreign_ids & set(rows))
        self.assertLess(page["source_status"]["progress"]["receipt_files"], 10_000)

    def test_receipt_paging_stays_stable_while_an_original_arrives(self):
        requests = [str(uuid.uuid4()) for _ in range(70)]
        for request in requests:
            self.receipt(request)
        self.receipt_worker()
        first = self.history()
        self.assertFalse(first["head"]["complete"])
        epoch = first["history_epoch"]
        added = str(uuid.uuid4())
        self.receipt(added)
        self.receipt_worker()
        page = first
        for _ in range(20):
            if page["head"]["complete"]:
                break
            page = self.history()
            self.assertEqual(page["history_epoch"], epoch)
        self.assertTrue(page["head"]["complete"])
        rows, classified = self.indexed_receipts()
        self.assertEqual(sorted(rows), sorted(requests + [added]))
        self.assertTrue(all(count == 1 for count in rows.values()))
        self.assertEqual(classified, 71)

    def test_receipt_import_resumes_from_its_stored_frontier(self):
        requests = [str(uuid.uuid4()) for _ in range(2)]
        for request in requests:
            self.receipt(request)
        self.receipt_worker()
        self.complete()
        rows, classified = self.indexed_receipts()
        self.assertEqual(sorted(rows), sorted(requests))
        self.assertEqual(classified, 2)
        added = str(uuid.uuid4())
        self.receipt(added)
        self.receipt_worker()
        self.complete()
        rows, classified = self.indexed_receipts()
        self.assertEqual(sorted(rows), sorted(requests + [added]))
        self.assertTrue(all(count == 1 for count in rows.values()))
        # The resumed frontier imports exactly the new original once.
        self.assertEqual(classified, 3)


    def test_a_rebound_receipt_original_is_a_gap_not_an_import(self):
        request = str(uuid.uuid4())
        path = self.receipt(request)
        self.receipt_worker()
        # The original changes after discovery: the locator still names the
        # discovered binding, so validation must refuse the new one.
        value = json.loads(path.read_text())
        value["digest"] = "changed-after-discovery"
        path.write_text(json.dumps(value), encoding="utf-8")
        page = self.history()
        for _ in range(20):
            if page["source_status"].get("gap") == "receipt_source_gap":
                break
            page = self.history()
            self.assertFalse(page["source_status"]["attachments"] == "indexing")
        self.assertEqual(page["source_status"]["gap"], "receipt_source_gap")
        self.assertFalse(page["head"]["complete"])
        rows, classified = self.indexed_receipts()
        self.assertNotIn(request, rows)
        self.assertEqual(classified, 0)


    def test_v1_receipt_index_migrates_through_the_real_worker(self):
        path = self.state / "delivery-receipts-v1.sqlite3"
        with closing(sqlite3.connect(path)) as db, db:
            db.executescript("""
                CREATE TABLE receipt_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
                CREATE TABLE receipt_sources(source TEXT PRIMARY KEY,stamp TEXT NOT NULL DEFAULT '',
                    complete INTEGER NOT NULL DEFAULT 0,indexed INTEGER NOT NULL DEFAULT 0,
                    errors INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE receipt_rows(seq INTEGER PRIMARY KEY AUTOINCREMENT,source TEXT NOT NULL,
                    request TEXT NOT NULL,name TEXT NOT NULL,run TEXT NOT NULL,conversation TEXT NOT NULL,
                    agent TEXT NOT NULL,first_message INTEGER NOT NULL,at REAL NOT NULL,
                    files INTEGER NOT NULL,binding TEXT NOT NULL,UNIQUE(source,request));
                CREATE INDEX receipt_exact ON receipt_rows(source,name,run,conversation,at DESC,request DESC);
                CREATE INDEX receipt_scope ON receipt_rows(source,agent,conversation,at DESC,request DESC);
                CREATE INDEX receipt_initial ON receipt_rows(source,agent,run,conversation,first_message,at DESC,request DESC);
                CREATE INDEX receipt_file_scope ON receipt_rows(source,agent,conversation,at DESC,request DESC) WHERE files=1;
                CREATE INDEX receipt_file_initial ON receipt_rows(source,agent,run,conversation,first_message,at DESC,request DESC) WHERE files=1;
                CREATE TABLE receipt_errors(source TEXT NOT NULL,name TEXT NOT NULL,reason TEXT NOT NULL,
                    PRIMARY KEY(source,name));
                CREATE TRIGGER receipt_row_count AFTER INSERT ON receipt_rows
                    BEGIN UPDATE receipt_sources SET indexed=indexed+1 WHERE source=NEW.source; END;
                CREATE TRIGGER receipt_error_count AFTER INSERT ON receipt_errors
                    BEGIN UPDATE receipt_sources SET errors=errors+1 WHERE source=NEW.source; END;
                CREATE TRIGGER receipt_error_clear AFTER DELETE ON receipt_errors
                    BEGIN UPDATE receipt_sources SET errors=MAX(0,errors-1) WHERE source=OLD.source; END;
                INSERT INTO receipt_meta VALUES('epoch','v1-epoch'),('cursor_secret','v1-secret');
                INSERT INTO receipt_sources(source,complete,indexed) VALUES('terminal',1,0);
            """)
        path.chmod(0o600)
        self.receipt_worker()
        with closing(sqlite3.connect(path)) as db:
            columns = {row[1] for row in db.execute("PRAGMA table_info(receipt_rows)")}
            self.assertIn("stamp", columns)
            self.assertIn("change", columns)
            indexes = db.execute("SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN "
                                 "('receipt_change','receipt_change_scope','receipt_change_initial')").fetchone()[0]
            self.assertEqual(indexes, 3)
            self.assertEqual(db.execute("SELECT value FROM receipt_meta WHERE key='change_seq'").fetchone()[0], "0")
        request = str(uuid.uuid4())
        self.receipt(request)
        self.receipt_worker()
        self.complete()
        rows, classified = self.indexed_receipts()
        self.assertEqual(rows.get(request), 1)
        self.assertEqual(classified, 1)



if __name__ == "__main__":
    unittest.main()
