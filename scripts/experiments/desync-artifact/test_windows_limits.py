"""Native hard-limit and startup-gate tests using synthetic metadata only."""
import json
import os
from pathlib import Path
import random
import subprocess
import tempfile
import time
import unittest

from qualify import child, MIB, private_error
from windows_limits import ResourceGate


BINARY = Path(os.environ["ZERUS_DESYNC_TEST_BINARY"]) if os.environ.get("ZERUS_DESYNC_TEST_BINARY") else None


@unittest.skipUnless(BINARY and BINARY.is_file(), "set ZERUS_DESYNC_TEST_BINARY to the isolated helper")
class NativeResourceTests(unittest.TestCase):
    def test_delivery_recovers_under_hard_gate_and_ready_needs_no_publisher(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            source = base / "synthetic-container.other"
            source.write_bytes(random.Random(17).randbytes(4*MIB))
            work = base / "slot"
            work.mkdir()
            config = work / "config.json"
            value = {"source": str(source), "length": 4*MIB, "scenario": "insert",
                     "average": 65536, "stash_bytes": MIB, "memory_bytes": 128*MIB}
            config.write_text(json.dumps(value), encoding="utf-8")
            def invoke(command, label):
                return child(BINARY, command, config, label, memory_bytes=256*MIB, deadline=30)
            self.assertEqual(invoke("prepare", "prepare")["exit_code"], 0)
            original = json.loads((work / "manifest.json").read_text(encoding="utf-8"))
            value["crash_phase"] = "before_ready"
            config.write_text(json.dumps(value), encoding="utf-8")
            self.assertEqual(invoke("apply", "crash")["exit_code"], 86)
            # A successful diagnostic result cannot substitute for readiness.
            self.assertTrue(json.loads((work / "result.json").read_text())["ok"])
            self.assertEqual(json.loads((work / "state.json").read_text())["state"], "verifying")
            self.assertNotEqual(invoke("status", "unavailable")["exit_code"], 0)
            value.pop("crash_phase")
            config.write_text(json.dumps(value), encoding="utf-8")
            self.assertEqual(invoke("apply", "repair")["exit_code"], 0)
            current = json.loads((work / "manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(original["delivery_id"], current["delivery_id"])
            source.unlink()
            self.assertEqual(invoke("status", "ready")["exit_code"], 0)
            self.assertEqual(list(work.glob(".tmp-block*")), [])
            self.assertEqual([p.name for p in work.glob("*.bin")], ["receiver.bin"])

    def test_validation_failure_does_not_write_to_unapproved_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            private_error(work, "synthetic diagnostic")
            self.assertEqual(list(work.iterdir()), [])

    def config(self, work, count=128):
        path = work / "config.json"
        path.write_text(json.dumps({"probe_chunks": count, "average": 65536,
                                   "scenario": "reverse", "memory_bytes": 32*MIB}), encoding="utf-8")
        return path

    def test_helper_waits_before_reading_configuration(self):
        with tempfile.TemporaryDirectory() as temporary, ResourceGate(128*MIB) as gate:
            work = Path(temporary)
            path = self.config(work)
            env = {k: os.environ[k] for k in ("SystemRoot", "WINDIR") if k in os.environ}
            env["ZERUS_DESYNC_QUALIFICATION_GATE"] = gate.name
            with (work / "test.log").open("wb") as log:
                proc = subprocess.Popen([str(BINARY), "metadata", str(path)], env=env,
                                        stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW)
                try:
                    time.sleep(0.2)
                    self.assertIsNone(proc.poll())
                    self.assertFalse((work / "metadata-result.json").exists())
                    gate.admit(proc)
                    self.assertEqual(proc.wait(timeout=30), 0)
                finally:
                    if proc.poll() is None:
                        proc.kill()
                    proc.wait(timeout=10)

    def test_memory_cap_refuses_oversized_indexes(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            path = self.config(work, 6_000_000)
            result = child(BINARY, "metadata", path, "bounded", memory_bytes=64*MIB, deadline=30)
            self.assertNotEqual(result["exit_code"], 0)
            self.assertFalse((work / "metadata-result.json").exists())
            self.assertFalse((work / "receiver.bin").exists())
            text = (work / "bounded.log").read_text(encoding="utf-8", errors="replace").lower()
            self.assertTrue("out of memory" in text or "virtualalloc" in text)

    def test_deadline_stops_only_owned_helper_and_records_refusal(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            path = self.config(work, 2_000_000)
            result = child(BINARY, "metadata", path, "deadline", memory_bytes=256*MIB, deadline=0)
            self.assertTrue(result["deadline_exceeded"])
            self.assertNotEqual(result["exit_code"], 0)
            self.assertFalse((work / "metadata-result.json").exists())

    def test_closing_job_terminates_its_owned_helper(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            path = self.config(work, 2_000_000)
            gate = ResourceGate(256*MIB)
            env = {k: os.environ[k] for k in ("SystemRoot", "WINDIR") if k in os.environ}
            env["ZERUS_DESYNC_QUALIFICATION_GATE"] = gate.name
            with (work / "test.log").open("wb") as log:
                proc = subprocess.Popen([str(BINARY), "metadata", str(path)], env=env,
                                        stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW)
                try:
                    gate.admit(proc)
                    gate.close()
                    proc.wait(timeout=10)
                    # Windows may report exit code zero for kill-on-job-close;
                    # completion requires the actual result, never just that code.
                    self.assertFalse((work / "metadata-result.json").exists())
                finally:
                    gate.close()
                    if proc.poll() is None:
                        proc.kill()
                    proc.wait(timeout=10)


if __name__ == "__main__":
    unittest.main()
