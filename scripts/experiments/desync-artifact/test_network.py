"""Slow, fragmented and interrupted paired-TLS tree transport; every payload is synthetic.

Each receiver runs through a loopback fault relay (network_faults.py) that sees
only ciphertext. The checks cover transport contracts, not bandwidth or
hardware performance: delay and fragmentation must still reconstruct exact
bytes; a cut or stalled response must never publish Ready or admit a scenario;
a repair needs a fresh publication and keeps the native receiver identity and
one payload tree; a data reconnect never creates or replays a scenario job.
"""
import hashlib
import json
from pathlib import Path
import random
import tempfile
import unittest
import uuid

from network_faults import FaultRelay, through
from qualify import child, MIB
from test_transfer import BINARY, identity, read
from test_tree import TreePublisher

CONTAINER = 12 * MIB
INDEX_RESPONSE, FIRST_CHUNK = 2, 3  # Response 1 is the catalog (see network_faults turns).


@unittest.skipUnless(BINARY and BINARY.is_file(), "set ZERUS_DESYNC_TEST_BINARY to the isolated helper")
class NetworkFaultTests(unittest.TestCase):
    def pair(self, root, name):
        folder = root / name
        folder.mkdir()
        config = folder / "init.json"
        config.write_text("{}", encoding="utf-8")
        self.assertEqual(child(BINARY, "identity-init", config, "identity", memory_bytes=128*MIB, deadline=15)["exit_code"], 0)
        return folder / "identity", read(folder / "identity/public.json")["fingerprint"]

    def fixture(self, extra_files=False):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        server, server_pin = self.pair(root, "server-identity")
        client, client_pin = self.pair(root, "client-identity")
        source = root / "source"
        (source / "packs").mkdir(parents=True)
        (source / "packs/large.container").write_bytes(random.Random(4011).randbytes(CONTAINER))
        if extra_files:
            (source / "app.dll").write_bytes(random.Random(4012).randbytes(65536))
            (source / "notes.txt").write_text("synthetic notes", encoding="utf-8")
            (source / "empty.dat").touch()
        publisher = TreePublisher(root, source)
        self.addCleanup(publisher.stop)
        publisher.value["peer"] = {"identity": str(server), "fingerprint": client_pin, "listen": "127.0.0.1:0"}
        publisher.start()
        slot = root / "slot"
        slot.mkdir()
        config = slot / "config.json"
        value = {"offer": publisher.offer, "delivery_id": uuid.uuid4().hex, "stash_bytes": MIB,
                 "memory_bytes": 128*MIB, "peer": {"identity": str(client), "fingerprint": server_pin}}
        return root, publisher, source, config, value

    def relay(self, publisher, **faults):
        port = int(publisher.offer["endpoint"].rsplit(":", 1)[1])
        relay = FaultRelay(port, **faults)
        self.addCleanup(relay.close)
        return relay

    def write(self, config, value, relay=None):
        written = json.loads(json.dumps(value))
        if relay:
            written["offer"] = through(value["offer"], relay)
        config.write_text(json.dumps(written), encoding="utf-8")

    def invoke(self, config, command="tree-receive", label="tree", deadline=30):
        return child(BINARY, command, config, label, memory_bytes=256*MIB, deadline=deadline)

    def refresh(self, publisher, value):
        """Restart the publication: same artifact, fresh endpoint, token and session."""
        old = dict(value["offer"])
        publisher.stop()
        fresh = publisher.start()
        self.assertTrue(fresh["artifact_id"] == old["artifact_id"])
        self.assertTrue(fresh["session"] != old["session"] and fresh["token"] != old["token"])
        value["offer"] = fresh

    def assert_not_ready(self, config, artifact):
        self.assertNotEqual(self.invoke(config, "tree-status", "unavailable", 15)["exit_code"], 0)
        installed = config.parent / "tree-installed.json"
        if installed.exists():
            self.assertTrue(read(installed)["artifact_id"] != artifact,
                            "interrupted tree was published as installed")

    def assert_ready(self, source, config):
        self.assertEqual(self.invoke(config, "tree-status", "status", 15)["exit_code"], 0)
        build = config.parent / "build"
        wanted = {p.relative_to(source).as_posix() for p in source.rglob("*") if p.is_file()}
        actual = {p.relative_to(build).as_posix() for p in build.rglob("*") if p.is_file()}
        self.assertTrue(wanted == actual, "synthetic tree file set differs")
        for relative in wanted:
            with (source / relative).open("rb") as a, (build / relative).open("rb") as b:
                self.assertTrue(hashlib.file_digest(a, "sha256").digest() == hashlib.file_digest(b, "sha256").digest(),
                                "synthetic tree contents differ")
        self.assert_single_tree(config.parent)
        # Paired TLS on 127.0.0.1 is labelled as such, never as the plain
        # loopback mode and never as an external network (reconstruct.go).
        scopes = {(r.get("transport"), r.get("external_network"))
                  for r in (json.loads(p.read_text(encoding="utf-8")) for p in config.parent.rglob("result.json")
                            if not p.is_relative_to(build))}
        self.assertEqual(scopes, {("paired-tls-loopback", False)})

    def assert_single_tree(self, slot):
        # Exactly one payload copy: outside build only bounded control metadata
        # remains (indexes, manifests, receipts), never staged payload bytes.
        outside = [p for p in slot.rglob("*") if p.is_file() and not p.is_relative_to(slot / "build")]
        self.assertTrue(all(p.stat().st_size < MIB for p in outside), "payload-sized file outside the build tree")
        self.assertLess(sum(p.stat().st_size for p in outside), 2*MIB)
        self.assertEqual([p for p in slot.rglob(".tmp-block*")], [])

    def scenario(self, root, artifact_slot, artifact_id, delivery):
        """Install a plain-loopback scenario bundle and bind a scenario-run job to the artifact slot."""
        executor = root / "executor"
        executor.mkdir()
        init = executor / "init.json"
        init.write_text("{}", encoding="utf-8")
        self.assertEqual(child(BINARY, "identity-init", init, "identity", memory_bytes=128*MIB, deadline=15)["exit_code"], 0)
        base = root / "scenario"
        source = base / "source"
        source.mkdir(parents=True)
        spec = {"protocol": 1, "instructions": "Run the explicit synthetic scenario once.", "runtime": "powershell",
                "entry": "run.ps1", "arguments": [], "timeout_seconds": 15, "memory_bytes": 256*MIB, "guided": False}
        (source / "scenario.json").write_text(json.dumps(spec), encoding="utf-8")
        (source / "run.ps1").write_text("[IO.File]::AppendAllText((Join-Path $env:ZERUS_RESULT_DIR 'runs.txt'), 'executed-once'); exit 0", encoding="utf-8")
        publisher = TreePublisher(base, source)
        self.addCleanup(publisher.stop)
        offer = publisher.start()
        slot = base / "slot"
        slot.mkdir()
        bundle_delivery = uuid.uuid4().hex
        (slot / "config.json").write_text(json.dumps({"offer": offer, "delivery_id": bundle_delivery, "stash_bytes": MIB,
                                                      "memory_bytes": 128*MIB}), encoding="utf-8")
        self.assertEqual(self.invoke(slot / "config.json", label="scenario-bundle")["exit_code"], 0)
        publisher.stop()
        job = {"executor_root": str(executor), "identity": str(executor / "identity"), "job_id": uuid.uuid4().hex,
               "artifact_slot": str(artifact_slot), "artifact_id": artifact_id, "artifact_delivery_id": delivery,
               "scenario_slot": str(slot), "scenario_id": offer["artifact_id"], "scenario_delivery_id": bundle_delivery,
               "device_id": "host", "parameters": {"sample": "synthetic"}}
        config = root / "job.json"
        config.write_text(json.dumps(job), encoding="utf-8")
        return config, executor / "jobs" / job["job_id"]

    def assert_fired(self, relay, response, offset):
        stats = relay.stats()
        self.assertTrue(stats["fired"], "fault did not reach its response: " + json.dumps(stats["sizes"]))
        # The receiver got exactly the bytes before the cut point of the intended turn.
        self.assertEqual(stats["sizes"].get(response), offset)
        self.assertEqual(stats["responses"], response)

    def run_scenario(self, config, label):
        return child(BINARY, "scenario-run", config, label, memory_bytes=512*MIB, deadline=35)["exit_code"]

    def test_response_turns_are_stable_and_long_enough_for_each_cut(self):
        # Turns are inferred from ciphertext direction, so qualify the layout
        # the cut tests rely on, repeatedly, on unfaulted fragmented links.
        for run in range(3):
            with self.subTest(run=run):
                _, publisher, source, config, value = self.fixture()
                relay = self.relay(publisher, seed=run, max_fragment=(1500, 4096, 16384)[run])
                self.write(config, value, relay)
                self.assertEqual(self.invoke(config, label="layout")["exit_code"], 0)
                relay.close()
                sizes = relay.stats()["sizes"]
                last = len(sizes)
                if sizes[last] <= 64:
                    # A final TLS close_notify alert after the receiver closes is
                    # its own turn; it follows every response and is not a chunk.
                    last -= 1
                chunks = last - 2
                index_body = 104 + 40 * chunks
                self.assertLess(sizes[1], 2048)  # Catalog of a one-file tree.
                self.assertTrue(index_body < sizes[INDEX_RESPONSE] < index_body + 1024, "response 2 is not the index")
                self.assertGreater(index_body, 2048)
                short = {n: sizes[n] for n in range(FIRST_CHUNK, last + 1) if sizes[n] <= 16384}
                self.assertEqual(short, {}, "chunk turn shorter than its cut")
                self.assertGreater(last, FIRST_CHUNK + 40)
                self.assertGreater(sum(sizes[n] for n in range(FIRST_CHUNK, last + 1)), CONTAINER)
                self.assert_ready(source, config)
                publisher.stop()

    def test_delayed_fragmented_paired_transfer_reconstructs_exact_tree(self):
        root, publisher, source, config, value = self.fixture(extra_files=True)
        relay = self.relay(publisher, seed=7, max_fragment=4096, first_byte_delay=0.25, rate_bytes=8*MIB)
        self.write(config, value, relay)
        result = self.invoke(config, label="slow")
        self.assertEqual(result["exit_code"], 0)
        stats = relay.stats()
        self.assertFalse(stats["fired"])
        self.assertGreaterEqual(stats["down_bytes"], CONTAINER)
        self.assertGreaterEqual(stats["responses"], 3)
        relay.close()
        self.assert_ready(source, config)
        # The published endpoint was never used; the relay only forwarded ciphertext.
        publisher.stop()
        self.assertEqual(self.invoke(config, label="offline-ready")["exit_code"], 0)
        self.assert_ready(source, config)

    def interrupted_then_repaired(self, cut_response, cut_offset, stall=False, read_idle=None):
        root, publisher, source, config, value = self.fixture()
        if read_idle is not None:
            value["peer"]["read_idle_seconds"] = read_idle
        relay = self.relay(publisher, seed=cut_response, max_fragment=16384, cut_response=cut_response,
                           cut_offset=cut_offset, stall=stall)
        self.write(config, value, relay)
        result = self.invoke(config, label="interrupted", deadline=(8 if read_idle is None else 25) if stall else 30)
        self.assert_fired(relay, cut_response, cut_offset)
        self.assertNotEqual(result["exit_code"], 0)
        if stall and read_idle is None:
            # The default 30 s bound is longer than this outer deadline.
            self.assertTrue(result["deadline_exceeded"])
        elif stall:
            # The receiver's own inactivity bound ended the stall.
            self.assertFalse(result["deadline_exceeded"])
            self.assertTrue(read_idle <= result["seconds"] < read_idle + 12, "inactivity bound out of range")
        relay.close()
        self.assert_not_ready(config, value["offer"]["artifact_id"])
        target = config.parent / "build/packs/large.container"
        reserved = identity(target) if target.exists() else None
        return root, publisher, source, config, value, reserved

    def repair(self, publisher, source, config, value, reserved):
        stale = dict(value["offer"])
        self.refresh(publisher, value)
        # A reconnect with the superseded credentials cannot complete the tree.
        self.write(config, dict(value, offer=stale))
        self.assertNotEqual(self.invoke(config, label="stale-credentials", deadline=15)["exit_code"], 0)
        self.assert_not_ready(config, value["offer"]["artifact_id"])
        relay = self.relay(publisher, seed=99, max_fragment=8192)
        self.write(config, value, relay)
        self.assertEqual(self.invoke(config, label="repair")["exit_code"], 0)
        relay.close()
        self.assert_ready(source, config)
        if reserved is not None:
            self.assertTrue(identity(config.parent / "build/packs/large.container") == reserved,
                            "repair replaced the native receiver")

    def test_truncated_responses_never_publish_ready_and_repair_with_fresh_publication(self):
        # Offsets lie past the wire bytes of the HTTP status, Content-Length
        # and publication headers. A cut inside a TLS record may still withhold
        # that record's plaintext, so the assertions establish the wire turn
        # and offset of the fault, not which plaintext the receiver decoded.
        for name, response, offset in (("index", INDEX_RESPONSE, 2048),
                                       ("first-chunk", FIRST_CHUNK, 12288),
                                       ("later-chunk", FIRST_CHUNK + 40, 12288)):
            with self.subTest(cut=name):
                _, publisher, source, config, value, reserved = self.interrupted_then_repaired(response, offset)
                self.repair(publisher, source, config, value, reserved)
                publisher.stop()

    def test_stalled_response_is_bounded_and_repairs(self):
        _, publisher, source, config, value, reserved = self.interrupted_then_repaired(FIRST_CHUNK + 10, 12288, stall=True)
        self.repair(publisher, source, config, value, reserved)

    def test_native_read_idle_bound_ends_stall_and_repairs(self):
        _, publisher, source, config, value, reserved = self.interrupted_then_repaired(
            FIRST_CHUNK + 10, 12288, stall=True, read_idle=2)
        self.assertIsNotNone(reserved)
        self.repair(publisher, source, config, value, reserved)

    def test_slow_progress_outlasts_read_idle_interval(self):
        _, publisher, source, config, value = self.fixture()
        value["peer"]["read_idle_seconds"] = 1
        # Continuous progress at about 3 MiB/s takes several idle intervals.
        # first_byte_delay applies once per direction, so the TLS handshake
        # sees two of them back to back (ClientHello, then the publisher's
        # flight): 2 x 0.25 s leaves margin below the 1 s inactivity bound,
        # while 2 x 0.5 s would equal it and correctly fail.
        relay = self.relay(publisher, seed=11, max_fragment=4096, first_byte_delay=0.25, rate_bytes=3*MIB)
        self.write(config, value, relay)
        result = self.invoke(config, label="slow-progress", deadline=40)
        errors = [read(path).get("error", "unavailable") for path in config.parent.rglob("error.json")]
        self.assertEqual(result["exit_code"], 0, json.dumps({"errors": errors, "seconds": round(result["seconds"], 2),
                                                             "deadline_exceeded": result["deadline_exceeded"],
                                                             "responses": relay.stats()["responses"]}))
        self.assertFalse(result["deadline_exceeded"])
        self.assertGreater(result["seconds"], 3)
        self.assertFalse(relay.stats()["fired"])
        relay.close()
        self.assert_ready(source, config)
        publisher.stop()

    def test_invalid_read_idle_configuration_is_refused_before_data(self):
        _, publisher, _, config, value = self.fixture()
        relay = self.relay(publisher)
        plain = json.loads(json.dumps(value))
        plain.pop("peer")
        plain["offer"]["endpoint"] = "http://127.0.0.1:9"
        plain["offer"].pop("peer_fingerprint", None)
        plain["peer"] = {"read_idle_seconds": 5}
        variants = [plain]
        for bad in (-1, 301, 1.5, "30", True):
            variant = json.loads(json.dumps(value))
            variant["peer"]["read_idle_seconds"] = bad
            variants.append(variant)
        for variant in variants:
            written = json.loads(json.dumps(variant))
            if "peer_fingerprint" in written["offer"]:
                written["offer"] = through(written["offer"], relay)
            config.write_text(json.dumps(written), encoding="utf-8")
            self.assertNotEqual(self.invoke(config, label="invalid-idle", deadline=15)["exit_code"], 0)
            self.assertFalse((config.parent / "tree-installed.json").exists())
        self.assertEqual(relay.stats()["connections"], 0, "invalid configuration reached the data peer")
        relay.close()
        accepted = json.loads(json.dumps(value))
        accepted["peer"]["read_idle_seconds"] = 300
        relay = self.relay(publisher)
        self.write(config, accepted, relay)
        self.assertEqual(self.invoke(config, label="max-idle")["exit_code"], 0)
        relay.close()
        publisher.stop()

    def test_interrupted_generation_keeps_native_receiver_and_previous_payload_tree(self):
        root, publisher, source, config, value = self.fixture()
        self.write(config, value)
        self.assertEqual(self.invoke(config, label="first")["exit_code"], 0)
        self.assert_ready(source, config)
        original = identity(config.parent / "build/packs/large.container")
        previous = value["offer"]["artifact_id"]
        publisher.stop()  # The publisher leases its source until it stops.
        with (source / "packs/large.container").open("r+b") as stream:
            stream.seek(5*MIB)
            stream.write(random.Random(4013).randbytes(2*MIB))
        value.update(offer=publisher.start(), previous_artifact_id=previous, delivery_id=uuid.uuid4().hex)
        self.assertTrue(value["offer"]["artifact_id"] != previous)
        relay = self.relay(publisher, seed=5, max_fragment=16384, cut_response=FIRST_CHUNK + 2, cut_offset=12288)
        self.write(config, value, relay)
        self.assertNotEqual(self.invoke(config, label="interrupted-generation")["exit_code"], 0)
        self.assert_fired(relay, FIRST_CHUNK + 2, 12288)
        relay.close()
        self.assert_not_ready(config, value["offer"]["artifact_id"])
        self.assertTrue(identity(config.parent / "build/packs/large.container") == original)
        self.assert_single_tree(config.parent)
        self.repair(publisher, source, config, value, original)

    def test_incomplete_tree_refuses_scenario_and_reconnects_never_create_or_replay_jobs(self):
        root, publisher, source, config, value, reserved = self.interrupted_then_repaired(FIRST_CHUNK + 5, 12288)
        job, work = self.scenario(root, config.parent, value["offer"]["artifact_id"], value["delivery_id"])
        # Admission refuses the incomplete tree before any job state exists.
        self.assertNotEqual(self.run_scenario(job, "incomplete-admission"), 0)
        self.assertFalse((work / "job-state.json").exists())
        self.assertFalse((work / "outputs/runs.txt").exists())
        self.repair(publisher, source, config, value, reserved)
        # The data repair alone created and ran nothing.
        self.assertFalse((work / "job-state.json").exists())
        self.assertEqual(self.run_scenario(job, "admitted"), 0)
        state = read(work / "job-state.json")
        self.assertEqual(state["status"], "passed")
        self.assertTrue((work / "outputs/runs.txt").read_text() == "executed-once")
        # A later data reconnect with fresh credentials is a ready no-op and
        # neither creates a job nor replays the finished one.
        self.refresh(publisher, value)
        relay = self.relay(publisher, seed=3, max_fragment=2048)
        self.write(config, value, relay)
        self.assertEqual(self.invoke(config, label="reconnect")["exit_code"], 0)
        relay.close()
        self.assertTrue(read(work / "job-state.json") == state)
        self.assertEqual(sorted(p.name for p in work.parent.iterdir()), [work.name])
        self.assertEqual(self.run_scenario(job, "repeat"), 0)
        self.assertTrue((work / "outputs/runs.txt").read_text() == "executed-once")
        publisher.stop()


if __name__ == "__main__":
    unittest.main()
