"""Original-child protocol custody; all subprocesses are private Python fixtures."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("hold_protocol", ROOT / "scripts/desktop-revoke-hold.py")
protocol = importlib.util.module_from_spec(spec)
spec.loader.exec_module(protocol)

CHILD = r'''
import json,os,sys,subprocess
receipt=json.loads(os.environ['RECEIPT'])
mode=os.environ['MODE']
def send(value):
    print(json.dumps(value),flush=True)
if mode=='refuse':
    send(dict(abi='desktop-revoke-hold-v1',held=False,code='writer_authority_changed'))
    sys.exit(1)
if mode=='partial':
    sys.stdout.write('{"abi":');sys.stdout.flush();sys.exit(0)
if mode=='overflow':
    sys.stdout.write('x'*10000);sys.stdout.flush();sys.exit(0)
if mode=='wrong_epoch':
    receipt['writer_epoch']='00000000-0000-4000-8000-000000000000'
if mode=='numeric_receipt':
    receipt['accepted']=1
if mode=='numeric_held':
    send(dict(abi='desktop-revoke-hold-v1',held=1,receipt=receipt))
elif mode=='held_pipe':
    subprocess.Popen([sys.executable,'-c',"import os; open(os.environ['GATE'],'rb',buffering=0).read(1)",],
                     env=os.environ,stdin=subprocess.DEVNULL,close_fds=True)
    send(dict(abi='desktop-revoke-hold-v1',held=True,receipt=receipt))
elif mode in ('early_release','trailing'):
    ready=json.dumps(dict(abi='desktop-revoke-hold-v1',held=True,receipt=receipt))+'\n'
    extra='{' if mode=='trailing' else json.dumps(dict(abi='desktop-revoke-hold-v1',held=False,released=True,receipt=receipt))+'\n'
    sys.stdout.write(ready+extra);sys.stdout.flush()
elif mode!='silent':
    send(dict(abi='desktop-revoke-hold-v1',held=True,receipt=receipt))
sys.stdin.buffer.read()
if mode=='stderr':
    sys.stderr.write('bounded private fixture stderr\n')
if mode=='wrong_release':
    receipt['request_id']='00000000-0000-4000-8000-000000000000'
send(dict(abi='desktop-revoke-hold-v1',held=False,released=True,receipt=receipt))
sys.exit(1 if mode=='bad_exit' else 0)
'''


@unittest.skipUnless(os.name == "posix", "Unix pipe/selector helper")
class DesktopRevokeHold(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="zerus-hold-protocol-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.receipt = dict(request_id=str(uuid.uuid4()), configuration="a" * 64,
                            writer_epoch=str(uuid.uuid4()), accepted=True, durable=True,
                            revoked=True, checkpointed=True)
        self.children, self.calls = [], []

    def hold(self, mode="normal", **overrides):
        def spawn(argv, **kwargs):
            self.calls.append((argv, kwargs))
            child = subprocess.Popen([sys.executable, "-c", CHILD], **kwargs)
            self.children.append(child)
            return child
        values = dict(hgs=self.root / "selected hgs", env={"PATH": "/usr/bin:/bin", "HOME": str(self.root),
                      "RECEIPT": json.dumps(self.receipt), "MODE": mode}, configuration="a" * 64,
                      request_id=self.receipt["request_id"], receipt=self.receipt,
                      help_text="  " + protocol.MARKER, popen=spawn)
        values.update(overrides)
        value = protocol.Hold(**values)
        self.addCleanup(value.close, validate=False)
        return value

    def test_original_child_is_alive_until_eof_then_same_receipt_exit_and_both_pipes_drain(self):
        hold = self.hold("stderr").acquire()
        hold.check()
        self.assertIsNone(hold.child.poll())
        self.assertFalse(hold.child.stdin.closed)
        argv, kwargs = self.calls[0]
        self.assertEqual(argv, [str(self.root / "selected hgs"), "desktop", "revoke-hold",
                               "--json", "--request-id", self.receipt["request_id"]])
        self.assertIs(kwargs["stdin"], subprocess.PIPE)
        self.assertTrue(kwargs["close_fds"])
        hold.close()
        self.assertTrue(hold.finished)
        self.assertEqual(hold.child.returncode, 0)
        self.assertTrue(all(p.closed for p in (hold.child.stdin, hold.child.stdout, hold.child.stderr)))
        self.assertEqual(hold.frames[-1]["receipt"], self.receipt)
        with self.assertRaises(protocol.Refusal):
            hold.check()

    def test_missing_marker_and_invalid_original_receipts_refuse_without_spawning(self):
        for change in ({"help_text": "Desktop revoke hold ABI: desktop-revoke-hold-v0"},
                       {"receipt": dict(self.receipt, checkpointed=False)},
                       {"receipt": dict(self.receipt, writer_epoch="unknown")},
                       {"receipt": dict(self.receipt, request_id=str(uuid.uuid4()))}):
            with self.subTest(change=change), self.assertRaises(protocol.Refusal):
                self.hold(**change)
        self.assertEqual(self.children, [])

    def test_installed_helper_loads_its_sibling_protocol_outside_the_source_checkout(self):
        helper = self.root / "lib/zerus/install-linux-desktop-service.py"
        helper.parent.mkdir(parents=True)
        library = helper.with_name("desktop-revoke-hold.py")
        shutil.copy2(ROOT / "scripts/install-linux-desktop-service.py", helper)
        shutil.copy2(ROOT / "scripts/desktop-revoke-hold.py", library)
        argv = [sys.executable, str(helper), "--help"]
        env = {"HOME": str(self.root), "PATH": "/usr/bin:/bin"}
        result = subprocess.run(argv, cwd=self.root, env=env, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertIn(b"cancel-rollback", result.stdout)
        library.unlink()
        missing = subprocess.run(argv, cwd=self.root, env=env, capture_output=True, timeout=5)
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn(b"desktop-revoke-hold.py", missing.stderr)

    def test_refused_partial_oversize_and_changed_epoch_frames_never_acquire_a_hold(self):
        for mode in ("refuse", "partial", "overflow", "wrong_epoch", "early_release", "trailing", "numeric_receipt", "numeric_held"):
            with self.subTest(mode=mode), self.assertRaises(protocol.Refusal):
                self.hold(mode).acquire()
            self.assertIsNotNone(self.children[-1].returncode)

    def test_silent_initial_handshake_is_bounded_and_original_child_is_joined(self):
        hold = self.hold("silent", timeout=0.2)
        with self.assertRaises(protocol.Refusal):
            hold.acquire()
        self.assertTrue(hold.finished)
        self.assertIsNotNone(hold.child.returncode)

    def test_wrong_release_or_nonzero_exit_is_not_a_settled_transition(self):
        for mode in ("wrong_release", "bad_exit"):
            hold = self.hold(mode).acquire()
            with self.subTest(mode=mode), self.assertRaisesRegex(protocol.Refusal, "unverified"):
                hold.close()
            self.assertTrue(hold.finished)
            self.assertIsNotNone(hold.child.returncode)

    def test_exited_original_child_cannot_be_replaced_by_copied_ready_metadata(self):
        hold = self.hold().acquire()
        original = hold.child
        original.kill()  # Private protocol fixture only.
        original.wait(timeout=2)
        with self.assertRaises(protocol.Refusal):
            hold.check()
        self.assertIs(hold.child, original)
        self.assertEqual(hold.frames[0]["receipt"], self.receipt)

    def test_logical_release_deadline_keeps_original_pipes_until_actual_eof(self):
        gate = self.root / "pipe-gate"
        os.mkfifo(gate)
        fd = os.open(gate, os.O_RDWR | os.O_NONBLOCK)
        thread = None
        try:
            hold = self.hold("held_pipe", env={"PATH": "/usr/bin:/bin", "HOME": str(self.root),
                             "RECEIPT": json.dumps(self.receipt), "MODE": "held_pipe", "GATE": str(gate)}).acquire()
            # Let the original child observe EOF and exit before expiring the
            # logical close deadline. Only its gated descendant retains the pipes;
            # this test must not race normal process exit against a 50 ms kill.
            hold.child.stdin.close()
            self.assertEqual(hold.child.wait(timeout=2), 0)
            hold.timeout = 0  # Deterministic expiry; startup still has its full bound.
            draining = threading.Event()
            original_pump = hold.pump
            def pump(timeout):
                # The expired close phase reaches pump only in actual cleanup.
                draining.set()
                return original_pump(timeout)
            hold.pump = pump
            errors = []
            def close():
                try:
                    hold.close()
                except protocol.Refusal as error:
                    errors.append(str(error))
            thread = threading.Thread(target=close)
            thread.start()
            self.assertTrue(draining.wait(timeout=2), 'original pipe cleanup did not start')
            self.assertEqual(hold.child.returncode, 0)
            self.assertTrue(thread.is_alive())
            self.assertFalse(hold.finished)
            self.assertFalse(hold.child.stdout.closed)
            self.assertFalse(hold.child.stderr.closed)
        finally:
            os.write(fd, b"x")
            if thread is not None:
                thread.join(timeout=3)
            os.close(fd)
        self.assertFalse(thread.is_alive())
        self.assertTrue(hold.finished)
        self.assertTrue(hold.child.stdout.closed)
        self.assertTrue(hold.child.stderr.closed)
        self.assertEqual(len(errors), 1)
        self.assertIn("did not drain", errors[0])


if __name__ == "__main__":
    unittest.main()
