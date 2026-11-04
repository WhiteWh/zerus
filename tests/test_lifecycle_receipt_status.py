"""Read original receipts in private state, without a native agent or real SSH."""
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest

HGS = Path(os.environ.get("HGS_TEST_BIN", Path(__file__).resolve().parents[1] / "target/debug/hgs")).resolve()
NAME = "codex/example/source"
REQUEST = "11111111-1111-4111-8111-111111111111"
MACHINE = "22222222-2222-4222-8222-222222222222"
ARCHIVE = "33333333-3333-4333-8333-333333333333"


class LifecycleReceiptStatus(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="hgs-receipt-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        for folder in ("home", "config", "data", "state", "cache", "runtime", "tmp", "tmux"):
            (self.root / folder).mkdir(mode=0o700)
        self.env = {"PATH": os.environ.get("PATH", os.defpath), "HOME": str(self.root / "home"),
                    "USERPROFILE": str(self.root / "home"), "HGS_CONFIG_DIR": str(self.root / "config"),
                    "HGS_STATE_DIR": str(self.root / "state" / "sessions"), "HGS_SELF": "fixture", "HGS_PEERS": "",
                    "HGS_TAB": "0", "XDG_CONFIG_HOME": str(self.root / "config"), "XDG_DATA_HOME": str(self.root / "data"),
                    "XDG_STATE_HOME": str(self.root / "state"), "XDG_CACHE_HOME": str(self.root / "cache"),
                    "XDG_RUNTIME_DIR": str(self.root / "runtime"), "TMPDIR": str(self.root / "tmp"),
                    "TMP": str(self.root / "tmp"), "TEMP": str(self.root / "tmp"), "TMUX_TMPDIR": str(self.root / "tmux")}
        if os.name == "nt":
            for key in ("SystemRoot", "SYSTEMROOT", "WINDIR", "PATHEXT"):
                if key in os.environ:
                    self.env[key] = os.environ[key]
        self.payload = {"request_id": REQUEST, "action": "archive", "expected_run_id": "run-1", "expected_conversation_id": "conversation-1"}
        self.state = Path(self.env["HGS_STATE_DIR"])

    def call(self, *args, payload=None, raw=None, success=True):
        data = raw if raw is not None else json.dumps(self.payload if payload is None else payload)
        result = subprocess.run([str(HGS), *args], input=data, env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return json.loads(result.stdout) if success else result

    def status(self, **kwargs):
        return self.call("session-action-status", NAME, "--json", **kwargs)

    def write(self, status="completed"):
        directory = self.state / "action_receipts"
        directory.mkdir(parents=True, mode=0o700, exist_ok=True)
        self.state.chmod(0o700)
        digest = hashlib.sha256(json.dumps([NAME, self.payload], sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()).hexdigest()
        value = {"request_id": REQUEST, "name": NAME, "run_id": "run-1", "conversation_id": "conversation-1",
                 "request_hash": digest, "status": status, "at": 1.0}
        if status == "completed":
            value["result_target"] = {"name": NAME, "run_id": "run-1", "conversation_id": "conversation-1", "archive_id": ARCHIVE}
        path = directory / f"{REQUEST}.json"
        path.write_text(json.dumps(value))
        path.chmod(0o600)
        return path

    def snapshot(self):
        return {str(p.relative_to(self.root)): (p.read_bytes(), p.stat().st_mtime_ns) for p in self.root.rglob("*") if p.is_file()}

    def test_absent_root_is_not_initialized(self):
        self.assertEqual(self.status()["status"], "unavailable")
        self.assertFalse(self.state.exists())

    def test_help_exposes_one_exact_marker_without_initializing_state(self):
        result = subprocess.run([str(HGS), "--help"], env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([line.strip() for line in result.stdout.splitlines()].count("lifecycle-receipt-status-v1"), 1)
        self.assertIn("--expect-machine ID session-action-status", result.stdout)
        self.assertFalse(self.state.exists())

    def test_completed_and_claimed_reads_do_not_rewrite_or_create_locks(self):
        for status, expected in (("completed", "completed"), ("claimed", "uncertain")):
            self.write(status)
            before = self.snapshot()
            response = self.status()
            self.assertEqual(response["status"], expected)
            self.assertNotIn("request_hash", response)
            self.assertEqual(self.snapshot(), before)
            self.assertFalse(list(self.root.rglob("*.lock")))

    def test_exact_target_hash_and_future_state_fail_without_mutation(self):
        path = self.write()
        original = path.read_text()
        for field, value in (("name", "replacement"), ("run_id", "replacement"), ("request_hash", "0" * 64),
                             ("status", "future"), ("result_target", {"name": "replacement"})):
            data = json.loads(original)
            data[field] = value
            path.write_text(json.dumps(data))
            before = self.snapshot()
            self.status(success=False)
            self.assertEqual(self.snapshot(), before)

    def test_input_and_receipt_limits_and_clear_context_are_fail_closed(self):
        self.status(raw=" " * 8193, success=False)
        self.status(payload={**self.payload, "action": "clear_context"}, success=False)
        self.assertFalse(self.state.exists())
        path = self.write()
        text = path.read_text()
        path.write_text(text + " " * (65536 - len(text.encode())))
        self.assertEqual(self.status()["status"], "completed")
        with path.open("a") as output:
            output.write(" ")
        self.status(success=False)

    def test_machine_guard_never_initializes_or_updates_the_catalog(self):
        args = ("--expect-machine", MACHINE, "session-action-status", NAME, "--json")
        before = self.snapshot()
        self.call(*args, success=False)
        self.assertEqual(self.snapshot(), before)
        self.assertFalse((self.root / "config/swarm").exists())
        self.catalog()
        self.write()
        before = self.snapshot()
        self.assertEqual(self.call(*args)["status"], "completed")
        self.call("--expect-machine", ARCHIVE, "session-action-status", NAME, "--json", success=False)
        self.assertEqual(self.snapshot(), before)
        self.assertFalse(list(self.root.rglob("*.lock")))

    def catalog(self):
        directory = self.root / "config/swarm"
        directory.mkdir(mode=0o700)
        path = directory / "catalog.json"
        path.write_text(json.dumps({"node_id": MACHINE, "catalog": {"schema": 1, "swarm_id": ARCHIVE, "operations": {}}, "bindings": {}}))
        path.chmod(0o600)

    def test_machine_guard_refuses_unsupported_or_malformed_existing_catalogs(self):
        self.catalog()
        path = self.root / "config/swarm/catalog.json"
        original = json.loads(path.read_text())
        for field, value in (("node_id", "bad"), ("future_field", True), ("catalog", {}),
                             ("catalog", {"schema": 2, "swarm_id": ARCHIVE, "operations": {}})):
            path.write_text(json.dumps({**original, field: value}))
            before = self.snapshot()
            self.call("--expect-machine", MACHINE, "session-action-status", NAME, "--json", success=False)
            self.assertEqual(self.snapshot(), before)
            self.assertFalse(self.state.exists())

    @unittest.skipIf(os.name == "nt", "POSIX synthetic transport; native Windows transport is qualified separately")
    def test_remote_requires_exact_machine_and_capability_before_forwarding_json(self):
        self.catalog()
        self.write()
        self.env["HGS_SSH"] = str(self.root / "ssh")
        ssh = self.root / "ssh"
        ssh.write_text("#!/usr/bin/env python3\nimport os,sys,pathlib,json\n"
                       "pathlib.Path(os.environ['HOME'],'ssh-args').write_text(json.dumps(sys.argv[1:]))\n"
                       "os.execv('/bin/sh',['sh','-c',sys.argv[-1]])\n")
        ssh.chmod(0o700)
        binary = self.root / "home/.local/bin/hgs"
        binary.parent.mkdir(parents=True, mode=0o700)
        binary.write_text(f"#!/bin/sh\nif [ \"$1\" = --help ]; then cat \"$HOME/help\"; else touch \"$HOME/forwarded\"; exec {shlex.quote(str(HGS))} \"$@\"; fi\n")
        binary.chmod(0o700)
        help_file = self.root / "home/help"
        forwarded = self.root / "home/forwarded"
        args = ("@synthetic-peer", "--expect-machine", MACHINE, "session-action-status", NAME, "--json")
        for markers in ("guarded-create-v1\n", "lifecycle-receipt-status-v1\n", "guarded-create-v1\nprefix-lifecycle-receipt-status-v1\n"):
            help_file.write_text(markers)
            self.call(*args, success=False)
            self.assertFalse(forwarded.exists())
        help_file.write_text("guarded-create-v1\nlifecycle-receipt-status-v1\n")
        self.assertEqual(self.call(*args)["status"], "completed")
        self.assertTrue(forwarded.exists())
        argv = json.loads((self.root / "home/ssh-args").read_text())
        self.assertNotIn("-t", argv)
        self.assertIn("BatchMode=yes", argv)

    @unittest.skipIf(os.name == "nt", "POSIX synthetic transport; native Windows transport is qualified separately")
    def test_unguarded_remote_status_never_contacts_ssh(self):
        self.env["HGS_SSH"] = str(self.root / "must-not-run")
        response = self.call("@synthetic-peer", "session-action-status", NAME, "--json", success=False)
        self.assertIn("requires --expect-machine", response.stderr)
        self.assertFalse(self.state.exists())


if __name__ == "__main__":
    unittest.main()
