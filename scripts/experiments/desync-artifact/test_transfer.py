"""Separate-process loopback contract checks. All payloads are synthetic."""
import ctypes
from ctypes import wintypes
import hashlib
import json
import msvcrt
import os
from pathlib import Path
import random
import shutil
import subprocess
import tempfile
import time
import unittest
import urllib.error
import urllib.request
import uuid

from qualify import child, kernel, MIB
from windows_limits import ResourceGate

BINARY = Path(os.environ["ZERUS_DESYNC_TEST_BINARY"]) if os.environ.get("ZERUS_DESYNC_TEST_BINARY") else None


class FileInformation(ctypes.Structure):
    _fields_ = [("attributes", wintypes.DWORD), ("created", wintypes.FILETIME),
                ("accessed", wintypes.FILETIME), ("written", wintypes.FILETIME),
                ("volume", wintypes.DWORD), ("size_high", wintypes.DWORD),
                ("size_low", wintypes.DWORD), ("links", wintypes.DWORD),
                ("index_high", wintypes.DWORD), ("index_low", wintypes.DWORD)]


def identity(path):
    handle = kernel.CreateFileW(str(path), 0x80, 7, None, 3, 0x00200000, None)
    if handle == ctypes.c_void_p(-1).value:
        raise RuntimeError("cannot inspect synthetic receiver")
    try:
        info = FileInformation()
        kernel.GetFileInformationByHandle.argtypes = [wintypes.HANDLE, ctypes.POINTER(FileInformation)]
        if not kernel.GetFileInformationByHandle(handle, ctypes.byref(info)):
            raise RuntimeError("cannot bind synthetic receiver")
        return f"{info.volume:08x}:{info.index_high:08x}{info.index_low:08x}"
    finally:
        kernel.CloseHandle(handle)


def read(path):
    # Rendezvous records are atomically replaced while this harness observes
    # them. A normal CRT read handle omits delete sharing and can perturb the
    # publisher's rename on Windows.
    handle=kernel.CreateFileW(str(path),0x80000000,7,None,3,0x00200000,None)
    if handle==ctypes.c_void_p(-1).value:
        code=ctypes.get_last_error()
        if code in (2,3):raise FileNotFoundError("synthetic record is not published")
        raise OSError("cannot read synthetic record")
    descriptor=msvcrt.open_osfhandle(int(handle),os.O_RDONLY|os.O_BINARY)
    with os.fdopen(descriptor,"rb") as stream:
        return json.loads(stream.read(4*MIB+1))


class Publisher:
    def __init__(self, base, source, scenario="insert", **faults):
        self.work = base / "publisher"
        self.work.mkdir()
        self.config = self.work / "config.json"
        self.value = {"source": str(source), "length": source.stat().st_size,
                      "scenario": scenario, "average": 65536, "stash_bytes": MIB,
                      "memory_bytes": 128*MIB, "serve_seconds": 60, **faults}
        self.process = self.gate = self.log = None
        self.offer = None

        self.command = "serve"
        self.offer_name = "offer.json"

    def start(self):
        previous = self.offer["session"] if self.offer else None
        self.config.write_text(json.dumps(self.value), encoding="utf-8")
        self.gate = ResourceGate(256*MIB)
        self.log = (self.work / "server.log").open("wb")
        env = {k: os.environ[k] for k in ("SystemRoot", "WINDIR", "COMSPEC") if k in os.environ}
        env.update({"ZERUS_DESYNC_QUALIFICATION_GATE": self.gate.name,
                    "GOPROXY": "off", "GOSUMDB": "off", "GOTELEMETRY": "off",
                    "OTEL_SDK_DISABLED": "true", "HTTP_PROXY": "", "HTTPS_PROXY": "",
                    "ALL_PROXY": "", "NO_PROXY": "*", "TEMP": str(self.work), "TMP": str(self.work)})
        self.process = subprocess.Popen([str(BINARY), self.command, str(self.config)],
                                        stdout=self.log, stderr=self.log, env=env,
                                        creationflags=subprocess.CREATE_NO_WINDOW)
        self.gate.admit(self.process)
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                # This harness creates synthetic inputs only. Surface the
                # bounded local diagnostic to make native race failures useful.
                detail=read(self.work/"error.json").get("error", "unavailable") if (self.work/"error.json").is_file() else "unavailable"
                raise RuntimeError("synthetic publisher stopped before publication: "+detail)
            try:
                offer = read(self.work / self.offer_name)
                if offer["session"] != previous:
                    self.offer = offer
                    return self.offer
            except FileNotFoundError:
                pass
            time.sleep(0.02)
        raise RuntimeError("synthetic publisher did not become ready")

    def stop(self):
        if self.process:
            if self.process.poll() is None:
                self.process.kill()
            self.process.wait(timeout=10)
        if self.gate:
            self.gate.close()
        if self.log:
            self.log.close()
        self.process = self.gate = self.log = None


@unittest.skipUnless(BINARY and BINARY.is_file(), "set ZERUS_DESYNC_TEST_BINARY to the isolated helper")
class SeparatedDeliveryTests(unittest.TestCase):
    def fixture(self, scenario="insert", seed=True, **faults):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        base = Path(temporary.name)
        source = base / "synthetic-container.unusual"
        source.write_bytes(random.Random(71).randbytes(8*MIB))
        slot = base / "receiver"
        slot.mkdir()
        receiver = slot / "receiver.bin"
        if seed:
            shutil.copyfile(source, receiver)
        publisher = Publisher(base, source, scenario, **faults)
        self.addCleanup(publisher.stop)
        offer = publisher.start()
        value = {"offer": offer, "delivery_id": uuid.uuid4().hex,
                 "stash_bytes": MIB, "memory_bytes": 128*MIB}
        if seed:
            value["adopt_receiver_identity"] = identity(receiver)
        config = slot / "config.json"
        self.assertNotIn(str(source), json.dumps(value))
        self.assertNotIn(str(publisher.work), json.dumps(value))
        config.write_text(json.dumps(value), encoding="utf-8")
        return publisher, config, value, source

    def invoke(self, config, command="receive", label="receive"):
        return child(BINARY, command, config, label, memory_bytes=256*MIB, deadline=30)["exit_code"]

    def assert_ready(self, config, value):
        self.assertEqual(self.invoke(config, "receive-status", "status"), 0)
        slot = config.parent
        self.assertEqual(read(slot / "state.json")["state"], "ready")
        self.assertTrue(read(slot / "result.json")["source_access_required"] is False)
        paired = bool(value["offer"].get("peer_fingerprint"))
        self.assertEqual((read(slot / "result.json")["transport"], read(slot / "result.json")["external_network"]),
                         ("paired-tls-loopback", False) if paired else ("authenticated-loopback-only", False))
        with (slot / "receiver.bin").open("rb") as stream:
            actual = hashlib.file_digest(stream, "sha256").hexdigest()
        self.assertTrue(actual == value["offer"]["artifact"]["sha256"], "synthetic output mismatch")
        self.assertEqual(list(slot.glob(".tmp-block*")), [])

    def test_seeded_edits_and_initial_provisioning(self):
        for scenario, seed in (("insert", True), ("swap", True), ("overwrite", True), ("all_changed", False)):
            with self.subTest(scenario=scenario, seed=seed):
                publisher, config, value, source = self.fixture(scenario, seed)
                self.assertEqual(self.invoke(config), 0)
                self.assert_ready(config, value)
                # Windows source lease covers the complete server lifetime.
                with self.assertRaises(PermissionError):
                    with source.open("r+b"):
                        pass
                publisher.stop()
                self.assertEqual(self.invoke(config, label="repeat-without-publisher"), 0)
                self.assertEqual(read(config.parent / "result.json")["downloaded_payload_bytes"], 0)
                self.assertEqual(read(config.parent / "result.json")["downloaded_index_bytes"], 0)
                self.assert_ready(config, value)

    def test_publisher_deadline_settles_requests_and_releases_source(self):
        publisher, config, _, source = self.fixture("all_changed", serve_seconds=1, rate_bytes=1)
        self.assertNotEqual(self.invoke(config, label="expired-publication"), 0)
        self.assertEqual(publisher.process.wait(timeout=5), 0)
        with source.open("r+b"):
            pass
        self.assertNotEqual(read(config.parent / "state.json")["state"], "ready")

    def test_changed_request_or_artifact_cannot_retarget_mixed_slot(self):
        publisher, config, value, _ = self.fixture("all_changed", crash_after=2)
        self.assertNotEqual(self.invoke(config, label="initial-interruption"), 0)
        before = (config.parent / "receiver.bin").read_bytes()
        changed = json.loads(json.dumps(value))
        changed["delivery_id"] = uuid.uuid4().hex
        config.write_text(json.dumps(changed), encoding="utf-8")
        self.assertNotEqual(self.invoke(config, label="different-request"), 0)
        publisher.stop()
        publisher.value["crash_after"] = 0
        publisher.value["scenario"] = "insert"
        value["offer"] = publisher.start()
        config.write_text(json.dumps(value), encoding="utf-8")
        self.assertNotEqual(self.invoke(config, label="different-artifact"), 0)
        self.assertTrue((config.parent / "receiver.bin").read_bytes() == before)

    def test_sender_restart_rotates_connection_but_not_delivery(self):
        publisher, config, value, _ = self.fixture("all_changed", crash_after=2)
        self.assertNotEqual(self.invoke(config, label="sender-crash"), 0)
        self.assertNotEqual(read(config.parent / "state.json")["state"], "ready")
        original = read(config.parent / "manifest.json")
        self.assertNotEqual(self.invoke(config, "receive-status", "unavailable"), 0)
        publisher.stop()
        publisher.value["crash_after"] = 0
        replacement = publisher.start()
        self.assertTrue(replacement["artifact_id"] == value["offer"]["artifact_id"])
        self.assertTrue(replacement["session"] != value["offer"]["session"])
        value["offer"] = replacement
        config.write_text(json.dumps(value), encoding="utf-8")
        self.assertEqual(self.invoke(config, label="repair"), 0)
        self.assertTrue(read(config.parent / "manifest.json") == original)
        self.assert_ready(config, value)

    def test_receiver_crash_and_corrupt_transport_repair(self):
        for kind in ("receiver_crash", "corruption"):
            with self.subTest(kind=kind):
                faults = {"corrupt_after": 1} if kind == "corruption" else {}
                publisher, config, value, _ = self.fixture("all_changed", **faults)
                if kind == "receiver_crash":
                    value["crash_phase"] = "before_ready"
                    config.write_text(json.dumps(value), encoding="utf-8")
                self.assertNotEqual(self.invoke(config, label="interrupted"), 0)
                self.assertNotEqual(read(config.parent / "state.json")["state"], "ready")
                value.pop("crash_phase", None)
                if kind == "corruption":
                    publisher.stop()
                    publisher.value["corrupt_after"] = 0
                    value["offer"] = publisher.start()
                config.write_text(json.dumps(value), encoding="utf-8")
                self.assertEqual(self.invoke(config, label="repair"), 0)
                self.assert_ready(config, value)

    def test_wrong_capabilities_and_unknown_receiver_are_refused(self):
        publisher, config, value, _ = self.fixture()
        original = (config.parent / "receiver.bin").read_bytes()
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        request = urllib.request.Request(publisher.offer["endpoint"]+"/v1/index")
        with self.assertRaises(urllib.error.HTTPError) as failure:
            opener.open(request, timeout=5)
        self.assertEqual(failure.exception.code, 403)
        failure.exception.close()
        for change in ("token", "protocol", "endpoint", "unknown_receiver"):
            modified = json.loads(json.dumps(value))
            if change == "token":
                modified["offer"]["token"] = "0"*64
            elif change == "protocol":
                modified["offer"]["protocol"] += 1
            elif change == "endpoint":
                modified["offer"]["endpoint"] = "http://192.0.2.1:8080"
            else:
                modified.pop("adopt_receiver_identity")
            config.write_text(json.dumps(modified), encoding="utf-8")
            self.assertNotEqual(self.invoke(config, label=change), 0)
            self.assertTrue((config.parent / "receiver.bin").read_bytes() == original)


if __name__ == "__main__":
    unittest.main()
