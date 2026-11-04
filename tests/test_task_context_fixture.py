"""Owned native task-context cleanup controls; no model calls or tmux server."""
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

import task_context_process as worker
import test_task_context_native as native


class CodexFixtureDrain(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="codex-drain-control-")
        self.addCleanup(self.temporary.cleanup)
        self.control = Path(self.temporary.name)
        self.fixture = native.NativeCodexTaskContext("test_codex_hands_the_task_instructions_to_the_model")
        self.fixture.root = self.control / "fixture"
        self.fixture.root.mkdir()
        self.env = dict(HOME=str(self.fixture.root), PATH=os.environ["PATH"])

    @unittest.skipUnless(sys.platform == "linux", "private subreaper runtime is Linux only")
    def test_detached_delayed_writer_settles_before_root_removal(self):
        script = self.control / "native.py"
        script.write_text('''import os, pathlib, subprocess, sys, time
root, control = map(pathlib.Path, sys.argv[1:3])
if len(sys.argv) == 3:
    (root / 'plugins').mkdir()
    subprocess.Popen([sys.executable, __file__, str(root), str(control), str(os.getpid())],
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                     start_new_session=True)
else:
    while os.getppid() == int(sys.argv[3]): time.sleep(.005)
    (control / 'adopted').touch()
    end = time.monotonic() + 5
    while not (control / 'release').exists():
        assert time.monotonic() < end
        time.sleep(.005)
    (root / 'plugins/late').write_text('owned delayed plugin writer')
''')
        with ThreadPoolExecutor(max_workers=1) as executor:
            result = executor.submit(self.fixture.run_owned_codex,
                [sys.executable, str(script), str(self.fixture.root), str(self.control)], self.env)
            try:
                end = time.monotonic() + 5
                while not (self.control / "adopted").exists():
                    self.assertLess(time.monotonic(), end, "owned writer did not become an orphan")
                    time.sleep(.005)
                self.assertFalse(result.done(), "original native exit is not descendant exit")
                with self.assertRaisesRegex(AssertionError, "settlement unknown"):
                    self.fixture.remove_fixture_root()
                self.assertTrue(self.fixture.root.exists())
            finally:
                (self.control / "release").touch()
            run = result.result(timeout=5)
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertEqual((self.fixture.root / "plugins/late").read_text(), "owned delayed plugin writer")
        receipt, = self.fixture.root.glob("native-settlement-*.json")
        self.assertEqual(json.loads(receipt.read_text())["reaped_descendants"], 1)
        self.fixture.remove_fixture_root()
        self.assertFalse(self.fixture.root.exists())

    @unittest.skipUnless(sys.platform == "linux", "private subreaper runtime is Linux only")
    def test_original_nonzero_exit_is_preserved_after_confirmed_settlement(self):
        run = self.fixture.run_owned_codex([sys.executable, "-c", "print('native output');raise SystemExit(7)"], self.env)
        self.assertEqual((run.returncode, run.stdout), (7, "native output\n"))
        self.fixture.remove_fixture_root()
        self.assertFalse(self.fixture.root.exists())

    def test_macos_uses_original_direct_subprocess_route(self):
        command = ["fixture-codex", "exec", "--ephemeral", "Hello."]
        completed = subprocess.CompletedProcess(command, 0, "native output", "")
        with mock.patch.object(native.sys, "platform", "darwin"), \
                mock.patch.object(native.subprocess, "run", return_value=completed) as direct, \
                mock.patch.object(native.subprocess, "Popen", side_effect=AssertionError("macOS entered Linux custody")) as wrapper:
            self.assertIs(self.fixture.run_owned_codex(command, self.env), completed)
        direct.assert_called_once_with(command, env=self.env, stdin=subprocess.DEVNULL,
                                       capture_output=True, text=True, timeout=120)
        wrapper.assert_not_called()
        self.assertFalse(hasattr(self.fixture, "native_settled"))
        self.fixture.remove_fixture_root()
        self.assertFalse(self.fixture.root.exists())

    def test_wrong_wrapper_receipt_retains_root(self):
        def start(command, **kwargs):
            path = Path(command[command.index("--receipt") + 1])
            nonce = command[command.index("--nonce") + 1]
            path.write_text(json.dumps(dict(settled=True, wrapper_pid=999, nonce=nonce)))
            return mock.Mock(pid=123, wait=mock.Mock(return_value=0))
        with mock.patch.object(native.sys, "platform", "linux"), \
                mock.patch.object(native.subprocess, "Popen", side_effect=start):
            with self.assertRaisesRegex(AssertionError, "identity mismatch"):
                self.fixture.run_owned_codex(["unused-native"], self.env)
        with self.assertRaisesRegex(AssertionError, "settlement unknown"):
            self.fixture.remove_fixture_root()
        self.assertTrue(self.fixture.root.exists())

    def test_missing_settlement_receipt_retains_root(self):
        with mock.patch.object(native.sys, "platform", "linux"), \
                mock.patch.object(native.subprocess, "Popen", return_value=mock.Mock(pid=123, wait=mock.Mock(return_value=0))):
            with self.assertRaisesRegex(AssertionError, "settlement missing"):
                self.fixture.run_owned_codex(["unused-native"], self.env)
        with self.assertRaisesRegex(AssertionError, "settlement unknown"):
            self.fixture.remove_fixture_root()
        self.assertTrue(self.fixture.root.exists())

    def test_timed_out_original_wrapper_retains_root_and_stops_only_that_child(self):
        child = mock.Mock(pid=123, wait=mock.Mock(side_effect=[subprocess.TimeoutExpired("wrapper", 1), 0]))
        with mock.patch.object(native.sys, "platform", "linux"), \
                mock.patch.object(native.subprocess, "Popen", return_value=child):
            with self.assertRaises(subprocess.TimeoutExpired):
                self.fixture.run_owned_codex(["unused-native"], self.env)
        child.kill.assert_called_once_with()
        self.assertEqual(child.wait.call_count, 2)
        with self.assertRaisesRegex(AssertionError, "settlement unknown"):
            self.fixture.remove_fixture_root()
        self.assertTrue(self.fixture.root.exists())

    def run_worker(self, waits, **patches):
        receipt = self.control / "receipt.json"
        child = mock.Mock(pid=123, wait=mock.Mock(return_value=0))
        with mock.patch.object(worker.sys, "platform", "linux"), \
                mock.patch.object(worker.ctypes, "CDLL") as library, \
                mock.patch.object(worker.subprocess, "Popen", return_value=child), \
                mock.patch.object(worker.os, "waitpid", side_effect=waits), \
                mock.patch.object(worker.time, "monotonic", **patches.get("clock", dict(return_value=0))), \
                mock.patch.object(worker.time, "sleep"):
            library.return_value.prctl.return_value = 0
            worker.run(["unused-native"], receipt, "nonce", 1, 1)
        return receipt

    def test_unknown_live_descendant_reaches_deadline_without_a_cleanup_receipt(self):
        with self.assertRaisesRegex(TimeoutError, "did not settle"):
            self.run_worker([(0, 0)], clock=dict(side_effect=[0, 2]))
        self.assertFalse((self.control / "receipt.json").exists())

    def test_descendant_count_is_bounded_without_a_cleanup_receipt(self):
        with self.assertRaisesRegex(RuntimeError, "count exceeded"):
            self.run_worker([(pid, 0) for pid in range(1, worker.MAX_DESCENDANTS + 2)])
        self.assertFalse((self.control / "receipt.json").exists())

    def test_subreaper_admission_failure_starts_no_native_process(self):
        with mock.patch.object(worker.sys, "platform", "linux"), \
                mock.patch.object(worker.ctypes, "CDLL") as library, \
                mock.patch.object(worker.subprocess, "Popen") as start:
            library.return_value.prctl.return_value = -1
            with self.assertRaisesRegex(OSError, "PR_SET_CHILD_SUBREAPER"):
                worker.run(["unused-native"], self.control / "receipt", "nonce", 1, 1)
        start.assert_not_called()


if __name__ == "__main__":
    unittest.main()
