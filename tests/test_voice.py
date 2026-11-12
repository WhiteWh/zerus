"""Real voice-worker lifecycle using private ASR and scripted terminal fixtures."""
import base64
import fcntl
import json
import os
from pathlib import Path
import sqlite3
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import uuid

import test_input as input_fixture
from test_speech import Server, load

HGS, TMUX = input_fixture.HGS, input_fixture.TMUX


class StatusBusy(Exception):
    """A definite unavailable voice-status read, never a submit/retry result."""


class VoiceInstaller(unittest.TestCase):
    def test_help_and_invalid_binary_never_write_user_configuration(self):
        script=Path(__file__).resolve().parents[1]/"scripts/install-voice-service.py"
        for arguments,expected in [(["--help"],0),(["/nonexistent/zerus-voice-test"],2),([],2)]:
            with tempfile.TemporaryDirectory(prefix="hgs-voice-install-") as home:
                # Apple Python stores its own import cache under HOME. Exclude
                # interpreter bytecode writes from the installer's side effects.
                run=subprocess.run([sys.executable,str(script),*arguments],
                    env={**os.environ,"HOME":home,"PYTHONDONTWRITEBYTECODE":"1"},
                    capture_output=True,text=True,timeout=5)
                self.assertEqual(run.returncode,expected,run.stderr)
                self.assertEqual(list(Path(home).iterdir()),[])


@unittest.skipUnless(TMUX and HGS.exists(), "Build hgs and install tmux for isolated voice contracts")
class VoiceWorker(unittest.TestCase):
    # Reuse only fixture methods, not the unrelated input test collection.
    script = input_fixture.InputTransport.script
    write_record = input_fixture.InputTransport.write_record
    tmux = input_fixture.InputTransport.tmux
    wait_for = input_fixture.InputTransport.wait_for
    received = input_fixture.InputTransport.received
    screen = input_fixture.InputTransport.screen

    def setUp(self):
        input_fixture.InputTransport.setUp(self)
        self.hgs=self.root / "voice-hgs"
        shutil.copy2(HGS,self.hgs)
        self.transcript=self.root / f"rollout-{self.conversation_id}.jsonl"
        self.transcript.write_text(json.dumps({"type":"session_meta","payload":{"id":self.conversation_id}})+"\n")
        self.record["transcript"]=str(self.transcript)
        self.write_record()
        self.env["HGS_PEERS"] = ""
        keep = {"HGS_CONFIG_DIR", "HGS_STATE_DIR", "HGS_SELF", "HGS_PEERS", "HGS_TAB"}
        for key in list(self.env):
            if key.startswith("HGS_") and key not in keep:
                self.env.pop(key)
        config = Path(self.env["HGS_CONFIG_DIR"])
        config.mkdir()
        settings = config / "speech.json"
        settings.write_text(json.dumps({"schema": 1, "state_dir": str(self.root / "asr"),
            "profiles": [{"id": "fixture", "name": "Fixture", "adapter": "fixture",
                          "model": "fixture", "language": "ru", "text": "Voice fixture transcript"}]}))
        self.server = Server(load(settings), test_mode=True)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.worker = None
        self.addCleanup(self.close_voice)
        self.machine_id = self.command(["swarm", "__mobile-peer-identity", "--json"])["machine_id"]
        self.start_worker()

    def tearDown(self):
        self.close_voice()
        input_fixture.InputTransport.tearDown(self)

    def close_voice(self):
        self.stop_worker()
        if self.server is not None:
            self.server.shutdown()
            self.server.server_close()
            self.thread.join(timeout=5)
            self.server = None

    def stop_worker(self):
        if self.worker is not None:
            self.worker.terminate()
            self.worker.wait(timeout=10)
            self.worker = None

    def start_worker(self):
        with (self.root / "worker.log").open("ab") as log:
            self.worker = subprocess.Popen([str(self.hgs), "voice", "worker"], env=self.env,
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=log)
        def locked():
            if self.worker.poll() is not None:
                self.fail("private voice worker exited")
            path = self.state / "voice/worker.lock"
            if not path.exists():
                return False
            with path.open("rb") as stream:
                try:
                    fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    return True
            return False
        self.wait_for(locked)

    def command(self, argv, payload=None, *, timeout=15):
        run = subprocess.run([str(self.hgs), *argv], input=json.dumps(payload) if payload else None,
            env=self.env, text=True, capture_output=True, timeout=timeout)
        if argv[:1] == ["voice"] and isinstance(payload, dict) and payload.get("action") == "status" \
                and run.returncode == 1 and not run.stdout and run.stderr.strip() == "hgs: database is locked":
            raise StatusBusy("voice status database is locked")
        self.assertEqual(run.returncode, 0, run.stderr)
        return json.loads(run.stdout)

    def payload(self):
        return dict(action="submit", request_id=str(uuid.uuid4()), expected_run_id=self.run_id,
            expected_conversation_id=self.conversation_id,
            speech=dict(machine_id=self.machine_id, profile_id="fixture"),
            audio=dict(format="pcm_s16le", sample_rate=16000, channels=1,
                       data_base64=base64.b64encode(b"\x10\x01" * 16000).decode()))

    def voice(self, payload, *, timeout=15):
        return self.command(["voice", self.name, "--json"], payload, timeout=timeout)["message"]

    def status(self, message, action="status", *, timeout=15):
        return self.voice(dict(action=action, request_id=str(uuid.uuid4()), message_id=message,
            expected_run_id=self.run_id, expected_conversation_id=self.conversation_id), timeout=timeout)

    def settle(self, message, states, timeout=12):
        until = time.monotonic() + timeout
        result = None
        busy = False
        while time.monotonic() < until:
            if self.worker.poll() is not None:
                self.fail("private voice worker exited: " + (self.root / "worker.log").read_text())
            remaining = until - time.monotonic()
            if remaining <= 0:
                break
            try:
                result = self.status(message, timeout=min(15, remaining))
                busy = False
            except StatusBusy:
                # Only this observational poll may repeat, within its ORIGINAL
                # deadline. Submissions, cancels and unknown results still fail.
                busy = True
                time.sleep(.1)
                continue
            if result["state"] in states:
                return result
            time.sleep(.1)
        self.fail(f"voice did not settle: {result}" + ("; status database is locked" if busy else ""))

    def emit_native(self, text, index, include_id=True):
        at=time.time()
        payload={"type":"user_message","message":text}
        if include_id:
            payload["id"]=f"native-{index}"
        with self.transcript.open("a") as stream:
            stream.write(json.dumps({"type":"event_msg","timestamp":at,"payload":payload})+"\n")
        with sqlite3.connect(self.state / "events.sqlite3") as db:
            db.execute("INSERT INTO events(name,conversation,payload) VALUES(?,?,?)",(self.name,self.conversation_id,
                json.dumps({"type":"UserPromptSubmit","agent_id":"","at":at,"detail":text})))
        self.record=json.loads(self.record_path.read_text())
        self.record["last_event_at"]=at
        # The UserPromptSubmit hook this stands in for also records the acceptance
        # that confirms the typed input (journal update_activity, zerus-iitw8).
        self.record["input_seen_at"]=at
        self.write_record()

    def history(self):
        for _ in range(12):
            page=self.command(["history",self.name,"--json"],dict(request_id=str(uuid.uuid4()),
                expected_run_id=self.run_id,expected_conversation_id=self.conversation_id,limit=100))
            if page["head"]["complete"]:
                return page["events"]
        self.fail("voice history did not finish indexing: " + json.dumps(page))

    def test_closed_submit_client_delivers_once_and_retains_original(self):
        payload = self.payload()
        accepted = self.voice(payload)
        self.assertIn(accepted["state"], {"queued", "transcribing"})
        self.assertEqual(accepted["audio"]["waveform"], [255] * 64)
        # The submitting CLI has already exited; only the independent worker remains.
        result = self.settle(accepted["id"], {"submitted"})
        self.assertEqual(result["transcript"], "Voice fixture transcript")
        duplicate = self.voice(payload)
        self.assertEqual(duplicate["id"], accepted["id"])
        self.assertEqual(duplicate["state"], "submitted")
        time.sleep(.3)
        self.assertEqual(self.received().count(b"Voice fixture transcript"), 1)
        self.assertNotIn(b"voice.wav", self.received())
        self.assertNotIn(b"Read the attached", self.received())
        audio = self.command(["attachment", self.name, "--request", accepted["id"], "--index", "0",
                              "--conversation", self.conversation_id])
        data = base64.b64decode(audio["data_base64"])
        self.assertEqual(data[:4], b"RIFF")
        self.assertEqual(data[44:], base64.b64decode(payload["audio"]["data_base64"]))
        with sqlite3.connect(self.state / "voice/messages.sqlite3") as db:
            job = json.loads(db.execute("SELECT body FROM messages WHERE id=?", (accepted["id"],)).fetchone()[0])
        self.assertEqual(json.loads(job["native_payload"])["request_id"], accepted["id"])
        self.assertEqual(job["native_receipt_status"], "submitted")
        self.assertIsInstance(job["boundary"]["journal_seq"], int)
        self.assertEqual(self.server.jobs.handle({"action": "stats"})["totals"]["submitted"], 1)
        self.emit_native(result["transcript"],1)
        inspected=self.command(["inspect",self.name])
        self.assertEqual(len(inspected["voice_messages"]),1)
        self.assertEqual(inspected["voice_messages"][0]["voice"]["transcript"],result["transcript"])
        self.assertIn("journal_seq",inspected["voice_messages"][0]["voice"]["native_binding"])
        events=self.history()
        self.assertEqual(len(events),1,events)
        self.assertEqual(events[0]["source"],"hgs_voice")
        self.assertEqual(events[0]["message_id"],accepted["id"])
        self.assertEqual(events[0]["attachments"][0]["waveform"], accepted["audio"]["waveform"])

    def test_legacy_waveform_backfill_updates_index_without_redelivery(self):
        payload = self.payload()
        accepted = self.voice(payload)
        completed = self.settle(accepted["id"], {"submitted"})
        self.stop_worker()
        # Represent an accepted message from the previous schema, then build
        # its public history before the new worker can backfill metadata.
        database = self.state / "voice/messages.sqlite3"
        with sqlite3.connect(database) as db:
            body = json.loads(db.execute("SELECT body FROM messages WHERE id=?", (accepted["id"],)).fetchone()[0])
            body["audio"].pop("waveform")
            db.execute("UPDATE messages SET body=?,waveform_version=0 WHERE id=?", (json.dumps(body),accepted["id"]))
        self.emit_native(completed["transcript"], 1)
        previous = self.history()
        self.assertEqual(len(previous), 1)
        self.assertNotIn("waveform", previous[0]["attachments"][0])
        before = self.status(accepted["id"])
        with sqlite3.connect(database) as db:
            old_sequence = db.execute("SELECT change_seq FROM messages WHERE id=?", (accepted["id"],)).fetchone()[0]
        self.start_worker()
        self.wait_for(lambda: len(self.status(accepted["id"])["audio"].get("waveform", [])) == 64)
        updated = self.status(accepted["id"])
        self.assertEqual(updated["state"], "submitted")
        self.assertGreater(updated["revision"], before["revision"])
        self.assertEqual(updated["audio"]["waveform"], [255] * 64)
        with sqlite3.connect(database) as db:
            self.assertGreater(db.execute("SELECT change_seq FROM messages WHERE id=?", (accepted["id"],)).fetchone()[0], old_sequence)
        history = self.history()
        self.assertEqual(len(history), 1)
        self.assertEqual(history[0]["message_id"], accepted["id"])
        self.assertEqual(history[0]["attachments"][0]["waveform"], updated["audio"]["waveform"])
        self.assertEqual(history[0]["voice"]["audio"]["waveform"], updated["audio"]["waveform"])
        audio = self.command(["attachment", self.name, "--request", accepted["id"], "--index", "0",
                              "--conversation", self.conversation_id])
        self.assertEqual(base64.b64decode(audio["data_base64"])[44:], base64.b64decode(payload["audio"]["data_base64"]))
        self.assertEqual(self.received().count(completed["transcript"].encode()), 1)
        self.assertEqual(self.server.jobs.handle({"action": "stats"})["totals"]["submitted"], 1)

    def test_repeated_voice_keeps_ordinary_identical_text_separate(self):
        one=self.voice(self.payload());first=self.settle(one["id"],{"submitted"})
        self.emit_native(first["transcript"],1)
        self.command(["inspect",self.name])
        self.assertEqual(len(self.history()),1)
        # An ordinary identical prompt precedes the next voice's source boundary.
        self.emit_native(first["transcript"],2)
        second=self.voice(self.payload());self.settle(second["id"],{"submitted"})
        self.emit_native(first["transcript"],3)
        inspected=self.command(["inspect",self.name])
        self.assertEqual(len(inspected["voice_messages"]),2)
        events=self.history()
        voices=[r for r in events if r["source"]=="hgs_voice"]
        self.assertEqual(len(voices),2,events)
        self.assertEqual(len(events),3,events)
        self.assertEqual({r["message_id"] for r in voices},{one["id"],second["id"]})
        self.assertEqual(self.received().count(b"Voice fixture transcript"),2)

    def test_worker_restart_recovers_recognition_without_duplicate_submit(self):
        self.server.jobs.profiles["fixture"]["delay"] = 1.0
        accepted = self.voice(self.payload())
        self.settle(accepted["id"], {"transcribing"})
        self.stop_worker()
        time.sleep(1.2)
        self.start_worker()
        result = self.settle(accepted["id"], {"submitted"})
        self.assertEqual(result["transcript"], "Voice fixture transcript")
        self.assertEqual(self.received().count(b"Voice fixture transcript"), 1)
        self.assertEqual(self.server.jobs.handle({"action": "stats"})["totals"]["submitted"], 1)

    def test_source_replacement_never_reuses_anonymous_provider_offset(self):
        accepted = self.voice(self.payload())
        result = self.settle(accepted["id"], {"submitted"})
        self.emit_native(result["transcript"], 1, include_id=False)
        self.command(["inspect", self.name])
        self.assertEqual(len(self.history()), 1)
        # Providers may omit message IDs. A replaced source can put unrelated
        # text at the same byte offset, which must remain its own visible row.
        lines = self.transcript.read_text().splitlines()
        replaced = json.loads(lines[-1])
        replaced["payload"]["message"] = "A distinct ordinary prompt"
        lines[-1] = json.dumps(replaced)
        temporary = self.transcript.with_suffix(".replacement")
        temporary.write_text("\n".join(lines) + "\n")
        temporary.replace(self.transcript)
        events = self.history()
        self.assertEqual(len(events), 2, events)
        self.assertEqual(len([row for row in events if row["source"] == "hgs_voice"]), 1)
        self.assertIn("A distinct ordinary prompt", [row["detail"] for row in events])

    def test_cancelled_recognition_never_reaches_native_input(self):
        self.server.jobs.profiles["fixture"]["delay"] = 5.0
        payload = self.payload()
        payload["audio"]["data_base64"] = base64.b64encode(b"\x00\x00").decode()
        accepted = self.voice(payload)
        self.assertEqual(accepted["audio"]["duration_ms"], 1)
        self.settle(accepted["id"], {"transcribing"})
        self.assertEqual(self.status(accepted["id"], "cancel")["state"], "cancelled")
        time.sleep(2.5)
        self.assertEqual(self.status(accepted["id"])["state"], "cancelled")
        self.assertEqual(self.received(), b"")
        self.assertNotIn("transcript", self.status(accepted["id"]))

    def test_changed_conversation_fails_and_preserves_audio(self):
        self.server.jobs.profiles["fixture"]["delay"] = 1.0
        accepted = self.voice(self.payload())
        self.settle(accepted["id"], {"transcribing"})
        self.record["conversation_id"] = str(uuid.uuid4())
        self.write_record()
        # The worker notices the change after the delayed transcription, which a
        # loaded CI runner can stretch past any fixed wait. Status requests name
        # the old conversation and are refused now, so read the store directly.
        until = time.monotonic() + 12
        while True:
            with sqlite3.connect(self.state / "voice/messages.sqlite3") as db:
                body, audio = db.execute("SELECT body,audio FROM messages WHERE id=?", (accepted["id"],)).fetchone()
            if json.loads(body)["state"] != "transcribing" or time.monotonic() >= until:
                break
            time.sleep(.1)
        self.assertEqual(json.loads(body)["state"], "failed")
        self.assertEqual(json.loads(body)["error_code"], "target_changed")
        self.assertGreater(len(audio), 44)
        self.assertEqual(self.received(), b"")

    def test_changed_profile_revision_fails_before_asr_and_keeps_original(self):
        # Keep this fixture's worker lock while delaying its next iteration;
        # submit otherwise starts another worker automatically.
        self.worker.send_signal(signal.SIGSTOP)
        try:
            managed = self.command(["speech", "--json"], {"action": "config_list"})
            payload = self.payload()
            payload["speech"]["profile_revision"] = managed["profiles"][0]["revision"]
            accepted = self.voice(payload)
            path = Path(self.env["HGS_CONFIG_DIR"]) / "speech-profiles/profiles.sqlite3"
            with sqlite3.connect(path) as db:
                db.execute("UPDATE profiles SET revision=revision+1 WHERE id='fixture'")
                db.execute("UPDATE metadata SET value=CAST(value AS INTEGER)+1 WHERE key='revision'")
        finally:
            self.worker.send_signal(signal.SIGCONT)
        result = self.settle(accepted["id"], {"failed"})
        self.assertNotIn("transcript", result)
        self.assertEqual(self.server.jobs.stats()["totals"]["submitted"], 0)
        self.assertEqual(self.received(), b"")
        with sqlite3.connect(self.state / "voice/messages.sqlite3") as db:
            body, audio = db.execute("SELECT body,audio FROM messages WHERE id=?", (accepted["id"],)).fetchone()
        self.assertEqual(json.loads(body)["speech"]["profile_revision"], payload["speech"]["profile_revision"])
        self.assertEqual(audio[44:], base64.b64decode(payload["audio"]["data_base64"]))

    def children(self):
        """This process's voice hgs children, exited ones included."""
        found = set()
        for entry in Path("/proc").iterdir():
            if not entry.name.isdigit():
                continue
            try:
                stat = (entry / "stat").read_text()
            except OSError:
                continue
            comm, fields = stat[stat.index("(") + 1:stat.rindex(")")], stat[stat.rindex(")") + 2:].split()
            if comm == self.hgs.name[:15] and int(fields[1]) == os.getpid():
                found.add(int(entry.name))
        return found

    def submitted(self):
        """Submits a voice message and returns how many workers the submit
        started. A worker started detached exits at once when it is not needed,
        so this process adopts it as a subreaper and counts it even after it
        exited."""
        import ctypes
        prctl = ctypes.CDLL(None, use_errno=True).prctl
        before = self.children()
        self.assertEqual(prctl(36, 1, 0, 0, 0), 0, "PR_SET_CHILD_SUBREAPER")
        try:
            self.accepted = self.voice(self.payload())
            spawned = self.children() - before
        finally:
            prctl(36, 0, 0, 0, 0)
            for pid in self.children() - before:
                try:
                    os.kill(pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                os.waitpid(pid, 0)
        self.assertEqual(self.accepted["state"], "queued")
        return len(spawned)

    def held(self, name):
        path = self.state / "voice" / name
        if not path.exists():
            return False
        with path.open("rb") as stream:
            try:
                fcntl.flock(stream, fcntl.LOCK_SH | fcntl.LOCK_NB)
            except BlockingIOError:
                return True
        return False

    @unittest.skipUnless(sys.platform.startswith("linux"), "counts the started workers through /proc")
    def test_submit_starts_no_worker_while_a_worker_runs_or_the_service_worker_waits(self):
        # The fixture's worker holds the lock: a submit needs no detached one,
        # and that worker delivers the message.
        self.assertEqual(self.submitted(), 0, "a running worker needs no detached one")
        self.settle(self.accepted["id"], {"submitted"})
        self.stop_worker()
        self.wait_for(lambda: not self.held("worker.lock"))
        # Without a worker a submit starts one, so the counts here are observed.
        self.assertEqual(self.submitted(), 1)
        self.wait_for(lambda: not self.held("worker.lock"))
        service = (self.state / "voice/service.lock").open("a")
        self.addCleanup(service.close)
        fcntl.flock(service, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.assertEqual(self.submitted(), 0, "the waiting service worker needs no detached one")
        self.assertFalse(self.held("worker.lock"))


@unittest.skipIf(sys.platform == "win32", "voice messages are not supported on Windows")
@unittest.skipUnless(HGS.exists(), "Build hgs for isolated voice worker contracts")
class VoiceWorkerOwnership(unittest.TestCase):
    """launchd and systemd start the service's worker again 10 s after it exits,
    so it waits for a worker a client started, and that worker gives way to it.
    These workers are not the service's; the service's own waiting is pinned by
    the state::service_worker unit tests."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="hgs-voice-worker-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.state = self.root / "state"
        # The worker refuses voice storage that other users can reach.
        (self.state / "voice").mkdir(parents=True)
        (self.state / "voice").chmod(0o700)
        tools = self.root / "bin"
        tools.mkdir()
        # No test here reaches a tmux server; never fall through to the user's.
        (tools / "tmux").write_text("#!/bin/sh\nexit 1\n")
        (tools / "tmux").chmod(0o755)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("HGS_", "CLAUDE_", "TMUX"))}
        self.env.update(HOME=str(self.root), HGS_CONFIG_DIR=str(self.root / "config"),
                        HGS_STATE_DIR=str(self.state), HGS_PEERS="",
                        PATH=str(tools) + os.pathsep + os.environ["PATH"])
        self.started = []
        self.addCleanup(self.stop)

    def stop(self):
        for worker in self.started:
            if worker.poll() is None:
                worker.terminate()
                worker.wait(timeout=10)

    def hold(self, name):
        handle = open(self.state / "voice" / name, "a")
        self.addCleanup(handle.close)
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return handle

    held = VoiceWorker.held

    def worker(self):
        worker = subprocess.Popen([str(HGS), "voice", "worker"], env=self.env, stdin=subprocess.DEVNULL,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        self.started.append(worker)
        return worker

    def finished(self, worker):
        try:
            _, errors = worker.communicate(timeout=15)
        except subprocess.TimeoutExpired:
            self.fail("the worker kept running")
        self.assertEqual(worker.returncode, 0, errors)

    def wait_for(self, check, seconds=15):
        until = time.monotonic() + seconds
        while not check():
            if time.monotonic() > until:
                self.fail("condition not reached")
            time.sleep(.05)

    def test_worker_exits_while_another_worker_holds_the_lock(self):
        self.hold("worker.lock")
        self.finished(self.worker())
        self.assertFalse(self.held("service.lock"))

    def test_worker_leaves_the_lock_to_a_waiting_service_worker(self):
        self.hold("service.lock")
        self.finished(self.worker())
        self.assertFalse(self.held("worker.lock"))

    def test_worker_refuses_a_linked_lock(self):
        elsewhere = self.root / "elsewhere.lock"
        elsewhere.touch()
        (self.state / "voice/worker.lock").symlink_to(elsewhere)
        worker = self.worker()
        try:
            _, errors = worker.communicate(timeout=15)
        except subprocess.TimeoutExpired:
            self.fail("the worker kept running")
        self.assertNotEqual(worker.returncode, 0)
        self.assertIn("hgs:", errors)
        with elsewhere.open("rb") as stream:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_worker_gives_way_once_the_service_worker_waits(self):
        worker = self.worker()
        self.wait_for(lambda: self.held("worker.lock"))
        time.sleep(2.5)
        self.assertIsNone(worker.poll(), "a worker without a waiting service keeps running")
        self.hold("service.lock")
        self.finished(worker)
        self.assertFalse(self.held("worker.lock"))


if __name__ == "__main__":
    unittest.main()
