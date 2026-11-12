#!/usr/bin/env python3
"""Private real-CLI receipt projection, paging and no-replay controls."""
import base64
from contextlib import closing
import hashlib
import json
import os
import sqlite3
import subprocess
import time
import unittest
import uuid

import test_input

if os.name != "nt":
    import fcntl


@unittest.skipUnless(os.name != "nt" and test_input.TMUX, "private Unix tmux fixture")
class ReceiptIndex(unittest.TestCase):
    def setUp(self):
        self.fixture = test_input.InputTransport()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.addCleanup(self.fixture.tearDown)
        self.env = self.fixture.env
        for key in ("CLAUDE_CONFIG_DIR", "CODEX_HOME", "KIMI_CODE_HOME", "DSH_HOME", "OPENCODE_HOME"):
            self.env.pop(key, None)
        self.state = self.fixture.state
        self.directory = self.state / "input_receipts"
        self.directory.mkdir()
        self.database = self.state / "delivery-receipts-v1.sqlite3"
        self.lease = (self.state / "receipt-index-worker.lock").open("a+b")
        self.addCleanup(self.lease.close)
        fcntl.flock(self.lease, fcntl.LOCK_EX)

    def call(self, *args, payload=None, success=True):
        result = subprocess.run([str(test_input.HGS), *args], env=self.env,
                                input=None if payload is None else json.dumps(payload),
                                text=True, capture_output=True, timeout=30)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout) if result.stdout.strip() else None
        self.assertNotEqual(result.returncode, 0, result.stdout)
        return result.stderr

    def original(self, n, **updates):
        request = str(uuid.UUID(int=n))
        value = dict(request_id=request, status="submitted", name=self.fixture.name,
                     run_id=self.fixture.run_id, conversation_id=self.fixture.conversation_id,
                     digest=hashlib.sha256(str(n).encode()).hexdigest(), submitted_at=float(n),
                     text=f"receipt {n}", submitted_text=f"receipt {n}",
                     attachments=[dict(name="image.png", mime="image/png", bytes=1,
                                       reference="[Image #1]", path="/private/fixture/image.png")])
        value.update(updates)
        path = self.directory / f"{request}.json"
        path.write_text(json.dumps(value))
        return path

    def migrate(self):
        fcntl.flock(self.lease, fcntl.LOCK_UN)
        try:
            self.call("__state", "receipt-index-worker")
        finally:
            fcntl.flock(self.lease, fcntl.LOCK_EX)

    def inspect(self):
        return self.call("__state", "inspect", self.fixture.name, "--skip-processes")

    def page(self, **updates):
        payload = dict(expected_run_id=self.fixture.run_id,
                       expected_conversation_id=self.fixture.conversation_id, limit=3)
        payload.update(updates)
        return self.call("__state", "receipt-index-page", self.fixture.name,
                         "--json", payload=payload)

    @staticmethod
    def ids(value):
        return [row["message_id"] for row in value["attachment_messages"]]

    def test_inspection_filters_before_paging_and_exposes_original_gaps(self):
        paths = [self.original(n) for n in range(1, 5)]
        self.original(5, conversation_id="foreign")
        self.original(6, agent_id="child")
        self.original(7, status="in_progress")
        self.migrate()
        initial = self.inspect()
        self.assertEqual(set(self.ids(initial)), {path.stem for path in paths})
        self.assertEqual(initial["receipt_projection"]["gaps"], [
            dict(request_id=str(uuid.UUID(int=7)), state="unconfirmed")])
        paths[0].unlink()
        paths[1].write_text("{")
        changed = json.loads(paths[2].read_text())
        changed["digest"] = "different-original-request"
        paths[2].write_text(json.dumps(changed))
        projected = self.inspect()
        self.assertEqual(self.ids(projected), [paths[3].stem])
        gaps = {row["request_id"]: row["state"] for row in projected["receipt_projection"]["gaps"]}
        self.assertEqual(gaps.get(paths[0].stem), "missing")
        self.assertEqual(gaps.get(paths[1].stem), "corrupt")
        self.assertEqual(gaps.get(paths[2].stem), "binding_changed")
        self.assertEqual(projected["receipt_projection"]["state"], "partial")
        self.assertFalse(projected["receipt_projection"]["absence_confirmed"])
        self.assertEqual(self.ids(self.page(agent_id="child")), [str(uuid.UUID(int=6))])
        self.call("__state", "receipt-index-page", self.fixture.name, "--json",
                  payload=dict(expected_run_id="wrong", expected_conversation_id=self.fixture.conversation_id),
                  success=False)

    def test_worker_seals_missing_originals_as_explicit_gaps_and_recovers_restoration(self):
        paths = [self.original(n) for n in range(1, 4)]
        original = paths[0].read_bytes()
        kept = {p: (p.read_bytes(), p.stat().st_mtime_ns) for p in paths[1:]}
        self.migrate()
        paths[0].unlink()
        self.migrate()
        projected = self.inspect()
        coverage = projected["receipt_projection"]
        self.assertTrue(coverage["migration_complete"], coverage)
        self.assertEqual(coverage["state"], "partial")
        self.assertEqual(coverage["unattributed_errors"], 1)
        self.assertIn(dict(request_id=paths[0].stem, state="missing"), coverage["gaps"])
        with closing(sqlite3.connect(self.database)) as db:
            self.assertEqual(db.execute("SELECT reason FROM receipt_errors WHERE source='terminal' AND name=?",
                                        (paths[0].name,)).fetchone(), ("missing",))
            self.assertEqual(db.execute("SELECT indexed FROM receipt_sources WHERE source='terminal'").fetchone(), (3,))
        self.assertEqual(self.ids(projected), [paths[2].stem, paths[1].stem])
        self.assertFalse(coverage["absence_confirmed"])
        for p, expected in kept.items():
            self.assertEqual((p.read_bytes(), p.stat().st_mtime_ns), expected)
        paths[0].write_bytes(original)
        self.migrate()
        restored = self.inspect()
        self.assertEqual(restored["receipt_projection"]["state"], "indexed")
        self.assertEqual(restored["receipt_projection"]["gaps"], [])
        self.assertEqual(self.ids(restored), [p.stem for p in reversed(paths)])

    def test_worker_seals_a_disappeared_family_without_dropping_its_bindings(self):
        paths = [self.original(n) for n in range(1, 3)]
        self.migrate()
        for p in paths:
            p.unlink()
        self.directory.rmdir()
        self.migrate()
        projected = self.inspect()["receipt_projection"]
        self.assertTrue(projected["migration_complete"], projected)
        self.assertEqual(projected["state"], "partial")
        self.assertEqual(projected["unattributed_errors"], 2)
        exact = [g for g in projected["gaps"] if "request_id" in g]
        self.assertEqual({g["request_id"] for g in exact}, {p.stem for p in paths})
        self.assertEqual({g["state"] for g in exact}, {"missing"})
        with closing(sqlite3.connect(self.database)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM receipt_rows WHERE source='terminal'").fetchone(), (2,))
        self.assertFalse(self.directory.exists())

    def test_worker_cannot_seal_an_unrestored_cache_scope(self):
        # id-settings-cx's real-CLI cache-scope probe (zerus-a4mub.42).
        path = self.original(1)
        before = path.read_bytes(), path.stat().st_mtime_ns
        self.migrate()
        self.assertEqual(self.ids(self.inspect()), [path.stem])
        with closing(sqlite3.connect(self.database)) as db, db:
            db.execute("UPDATE receipt_rows SET agent='foreign-child' WHERE request=?", (path.stem,))
        invalid = self.inspect()["receipt_projection"]
        self.assertFalse(invalid["catalog_complete_at_snapshot"])
        self.migrate()
        restored = self.inspect()
        self.assertEqual((path.read_bytes(), path.stat().st_mtime_ns), before)
        self.assertEqual(self.ids(restored), [path.stem],
                         "worker declared complete coverage without restoring the original scope")
        self.assertTrue(restored["receipt_projection"]["migration_complete"])
        self.assertEqual(restored["receipt_projection"]["gaps"], [])

    def test_external_first_prompt_pin_is_a_gap_before_reconciliation(self):
        path = self.original(1, conversation_id="", first_message=True)
        self.migrate()
        original = path.read_bytes(), path.stat().st_mtime_ns
        with closing(sqlite3.connect(self.database)) as db, db:
            before = db.execute("SELECT value FROM receipt_meta WHERE key='epoch'").fetchone()[0]
            db.execute("UPDATE receipt_rows SET conversation='foreign-conversation' WHERE request=?", [path.stem])
        with closing(sqlite3.connect(self.database)) as db:
            self.assertEqual(db.execute("SELECT complete,errors FROM receipt_sources WHERE source='terminal'").fetchone(), (0, 1))
            self.assertEqual(db.execute("SELECT reason FROM receipt_errors WHERE source='terminal'").fetchone(), ('binding_changed',))
            self.assertNotEqual(db.execute("SELECT value FROM receipt_meta WHERE key='epoch'").fetchone()[0], before)
        projection = self.inspect()["receipt_projection"]
        self.assertFalse(projection["catalog_complete_at_snapshot"])
        self.assertEqual((path.read_bytes(), path.stat().st_mtime_ns), original)

    def test_validated_original_pin_keeps_an_existing_page_chain(self):
        path = self.original(1, conversation_id="", first_message=True)
        self.original(2)
        self.original(3)
        self.migrate()
        page = self.page(limit=2)
        self.assertTrue(page['receipt_projection']['catalog_complete_at_snapshot'])
        cursor = page['receipt_projection']['next_cursor']
        self.assertTrue(cursor)
        with closing(sqlite3.connect(self.database)) as db:
            epoch = db.execute("SELECT value FROM receipt_meta WHERE key='epoch'").fetchone()[0]
        record = json.loads(path.read_text())
        record['resolved_conversation_id'] = self.fixture.conversation_id
        path.write_text(json.dumps(record))
        original = path.read_bytes(), path.stat().st_mtime_ns
        self.migrate()
        with closing(sqlite3.connect(self.database)) as db:
            self.assertEqual(db.execute("SELECT value FROM receipt_meta WHERE key='epoch'").fetchone()[0], epoch)
            self.assertEqual(db.execute("SELECT complete,errors FROM receipt_sources WHERE source='terminal'").fetchone(), (1, 0))
        resumed = self.page(cursor=cursor, limit=3)
        self.assertEqual(self.ids(resumed), [path.stem])
        self.assertEqual((path.read_bytes(), path.stat().st_mtime_ns), original)

    def test_snapshot_pages_survive_backdated_arrivals_and_reject_other_scopes(self):
        originals = [self.original(n) for n in range(1, 13)]
        self.migrate()
        first = self.page()
        cursor = first["receipt_projection"]["next_cursor"]
        seen = self.ids(first)
        for n in range(100, 500):
            self.original(n, submitted_at=9.5)
        self.migrate()
        for _ in range(20):
            page = self.page(cursor=cursor)
            seen.extend(self.ids(page))
            self.assertLessEqual(page["receipt_projection"]["examined"], 256)
            if not page["receipt_projection"]["has_more"]:
                break
            next_cursor = page["receipt_projection"]["next_cursor"]
            self.assertNotEqual(next_cursor, cursor)
            cursor = next_cursor
        else:
            self.fail("snapshot did not finish")
        self.assertEqual(seen, [p.stem for p in reversed(originals)])
        for update in (dict(agent_id="foreign"), dict(limit=0), dict(cursor="bad")):
            payload = dict(expected_run_id=self.fixture.run_id,
                           expected_conversation_id=self.fixture.conversation_id, cursor=cursor)
            payload.update(update)
            self.call("__state", "receipt-index-page", self.fixture.name,
                      "--json", payload=payload, success=False)

    def test_real_send_updates_locator_and_original_receipt_prevents_replay(self):
        self.migrate()
        payload = self.fixture.payload(text="indexed input [Image #1]", attachments=[
            dict(name="image.png", mime="image/png", reference="[Image #1]",
                 data_base64=base64.b64encode(b"private fixture image").decode())])
        submitted = self.fixture.send(payload)
        self.assertEqual(submitted["status"], "submitted")
        with closing(sqlite3.connect(self.database)) as db, db:
            count = db.execute("SELECT COUNT(*) FROM receipt_rows WHERE source='terminal' AND request=?",
                               (payload["request_id"],)).fetchone()[0]
        self.assertEqual(count, 1)
        self.assertEqual(self.ids(self.inspect()), [payload["request_id"]])
        original = self.directory / (payload["request_id"] + ".json")
        before = original.read_bytes()
        self.fixture.wait_for(lambda: b"indexed input" in self.fixture.received())
        received = self.fixture.received()
        # Index loss cannot grant retry authority: the durable original still wins.
        with closing(sqlite3.connect(self.database)) as db, db:
            db.execute("DELETE FROM receipt_rows WHERE request=?", (payload["request_id"],))
        duplicate = self.fixture.send(payload)
        self.assertEqual(duplicate["status"], "submitted")
        time.sleep(.05)
        self.assertEqual(self.fixture.received(), received)
        self.assertEqual(original.read_bytes(), before)
        uncertain = self.fixture.payload(text="never replay this")
        marker = self.directory / (uncertain["request_id"] + ".json")
        marker.write_text(json.dumps(dict(request_id=uncertain["request_id"], status="in_progress")))
        self.fixture.send(uncertain, success=False)
        self.assertEqual(self.fixture.received(), received)

    def test_lost_locator_invalidates_coverage_and_worker_restores_the_original(self):
        path = self.original(1)
        before = path.read_bytes()
        self.migrate()
        self.assertEqual(self.ids(self.inspect()), [path.stem])
        with closing(sqlite3.connect(self.database)) as db, db:
            db.execute("DELETE FROM receipt_rows WHERE request=?", (path.stem,))
        missing = self.inspect()
        self.assertEqual(self.ids(missing), [])
        self.assertNotEqual(missing["receipt_projection"]["state"], "indexed")
        self.assertFalse(missing["receipt_projection"]["migration_complete"])
        self.assertFalse(missing["receipt_projection"]["catalog_complete_at_snapshot"])
        self.assertTrue(missing["receipt_projection"]["gaps"])
        self.migrate()
        restored = self.inspect()
        self.assertEqual(self.ids(restored), [path.stem])
        self.assertEqual(restored["receipt_projection"]["state"], "indexed")
        self.assertEqual(restored["receipt_projection"]["indexed"], 1)
        self.assertEqual(restored["receipt_projection"]["gaps"], [])
        self.assertEqual(path.read_bytes(), before)

    def test_worker_reconciles_a_loss_that_did_not_notify_the_index(self):
        path = self.original(1)
        second = self.original(2)
        before = path.read_bytes()
        self.migrate()
        first = self.page(limit=1)
        self.assertEqual(self.ids(first), [second.stem])
        cursor = first["receipt_projection"]["next_cursor"]
        # Models row loss outside the current writer's invalidation protocol.
        # Keep the installed guards and complete metadata to exercise startup
        # reconciliation independently from the ordinary DELETE notification.
        with closing(sqlite3.connect(self.database)) as db, db:
            trigger = db.execute("SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='receipt_row_removed'").fetchone()
            if trigger:
                db.execute("DROP TRIGGER receipt_row_removed")
            db.execute("DELETE FROM receipt_rows WHERE request=?", (path.stem,))
            if trigger:
                db.execute(trigger[0])
        self.migrate()
        restored = self.inspect()
        self.assertEqual(self.ids(restored), [second.stem, path.stem])
        self.assertEqual(restored["receipt_projection"]["indexed"], 2)
        self.call("__state", "receipt-index-page", self.fixture.name, "--json",
                  payload=dict(expected_run_id=self.fixture.run_id,
                               expected_conversation_id=self.fixture.conversation_id,
                               cursor=cursor), success=False)
        self.assertEqual(path.read_bytes(), before)

    def test_an_old_unverified_cache_is_unknown_until_worker_reconciliation(self):
        path = self.original(1)
        before = path.read_bytes()
        self.migrate()
        with closing(sqlite3.connect(self.database)) as db, db:
            db.execute("DROP TRIGGER IF EXISTS receipt_row_removed")
            db.execute("DROP TRIGGER IF EXISTS receipt_row_rebound")
            db.execute("DELETE FROM receipt_meta WHERE key='rowset_protocol'")
            db.execute("DELETE FROM receipt_rows WHERE request=?", (path.stem,))
        unknown = self.inspect()
        self.assertFalse(unknown["receipt_projection"]["migration_complete"])
        self.assertTrue(unknown["receipt_projection"]["gaps"])
        self.migrate()
        self.assertEqual(self.ids(self.inspect()), [path.stem])
        self.assertEqual(path.read_bytes(), before)

    def test_cold_inspection_is_unknown_and_background_worker_discovers_originals(self):
        paths = [self.original(n) for n in range(1, 201)]
        before = {p.name: p.read_bytes() for p in paths}
        cold = self.inspect()
        self.assertEqual(self.ids(cold), [])
        self.assertEqual(cold["receipt_projection"]["state"], "unavailable")
        self.assertFalse(cold["receipt_projection"]["absence_confirmed"])
        fcntl.flock(self.lease, fcntl.LOCK_UN)
        try:
            deadline = time.monotonic() + 10
            while True:
                value = self.inspect()
                if value["receipt_projection"].get("migration_complete"):
                    break
                self.assertLess(time.monotonic(), deadline, "background migration did not finish")
                time.sleep(.02)
        finally:
            # Only our worker owns this private state; settle it before teardown.
            fcntl.flock(self.lease, fcntl.LOCK_EX)
        self.assertEqual(len(self.ids(value)), 100)
        self.assertEqual(before, {p.name: p.read_bytes() for p in paths})

    def test_interrupted_worker_resumes_committed_locators_without_touching_originals(self):
        paths = [self.original(n) for n in range(1, 1201)]
        before = {p.name: p.read_bytes() for p in paths}
        fcntl.flock(self.lease, fcntl.LOCK_UN)
        worker = subprocess.Popen([str(test_input.HGS), "__state", "receipt-index-worker"],
                                  env=self.env, stdin=subprocess.DEVNULL,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            deadline = time.monotonic() + 5
            while True:
                committed = []
                if self.database.exists():
                    try:
                        with closing(sqlite3.connect(f"file:{self.database}?mode=ro", uri=True, timeout=.05)) as db:
                            committed = db.execute("SELECT request,seq FROM receipt_rows WHERE source='terminal'").fetchall()
                    except sqlite3.OperationalError:
                        pass  # Schema creation has not committed yet.
                if committed:
                    self.assertLess(len(committed), 1200, "worker finished before interruption control")
                    break
                self.assertLess(time.monotonic(), deadline, "no committed migration progress")
                time.sleep(.002)
            worker.terminate()  # Only this Popen-owned fixture migration worker.
            worker.wait(timeout=5)
        finally:
            if worker.poll() is None:
                worker.kill()
                worker.wait(timeout=5)
            fcntl.flock(self.lease, fcntl.LOCK_EX)
        self.migrate()
        with closing(sqlite3.connect(self.database)) as db, db:
            resumed = dict(db.execute("SELECT request,seq FROM receipt_rows WHERE source='terminal'").fetchall())
        self.assertEqual(len(resumed), 1200)
        self.assertTrue(all(resumed[request] == sequence for request, sequence in committed))
        self.assertEqual(before, {p.name: p.read_bytes() for p in paths})
        self.assertTrue(self.inspect()["receipt_projection"]["migration_complete"])


if __name__ == "__main__":
    unittest.main()
