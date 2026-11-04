"""Portable finite controls; these do not qualify the Windows kernel adapter."""
import unittest
import gc
from pathlib import Path
import tempfile
from unittest.mock import patch
import test_windows_host as windows_host
from windows_gateway_custody import ConnectorCustody, CustodyUnknown, guard_root_cleanup, termination_adapter


class Custody(unittest.TestCase):
    def setUp(self):
        self.control = None
        self.waits = []
        self.closed, self.stopped, self.pinned = [], [], []
        self.terminated = True
        self.custody = ConnectorCustody("a"*64, lambda: self.control, self.pin,
            lambda handle, timeout: self.waits.pop(0), lambda handle: self.terminated,
            self.closed.append, self.stopped.append)

    def pin(self, control):
        self.pinned.append(control)
        return "owned-image-handle"

    def admit(self):
        self.control = {"revision": "a"*64, "generation": "b"*32, "pid": 42}

    def test_launcher_failure_before_admission_preserves_root(self):
        self.custody.begin()
        with self.assertRaises(CustodyUnknown): self.custody.cleanup()
        self.assertFalse(self.custody.may_remove_root)
        self.assertEqual(self.pinned, [])

    def test_preexisting_control_is_not_adopted_or_deleted(self):
        self.admit()
        with self.assertRaises(CustodyUnknown): self.custody.begin()
        with self.assertRaises(CustodyUnknown): self.custody.cleanup()
        self.assertEqual(self.pinned, []); self.assertEqual(self.stopped, [])
        self.assertFalse(self.custody.may_remove_root)

    def test_timed_out_launcher_recovers_private_child_and_confirms_exit(self):
        self.custody.begin(); self.admit(); self.waits = [0]
        self.assertFalse(self.custody.cleanup())
        self.assertTrue(self.custody.may_remove_root)
        self.assertEqual(self.closed, ["owned-image-handle"])

    def test_failed_image_pin_keeps_custody_and_root(self):
        self.custody.begin(); self.admit()
        self.custody.pin = lambda control: None
        with self.assertRaises(CustodyUnknown): self.custody.cleanup()
        self.assertFalse(self.custody.may_remove_root)

    def test_foreign_revision_or_changed_generation_is_never_controlled(self):
        self.custody.begin(); self.admit(); self.control["revision"] = "c"*64
        with self.assertRaises(CustodyUnknown): self.custody.cleanup()
        self.assertEqual(self.pinned, []); self.assertEqual(self.stopped, [])
        self.control["revision"] = "a"*64; self.custody.recover()
        self.control = {**self.control, "generation": "d"*32}; self.waits = [0]
        with self.assertRaises(RuntimeError): self.custody.cleanup()
        self.assertEqual(self.stopped, [])
        self.assertFalse(self.custody.may_remove_root)

    def test_new_control_after_old_handle_exit_preserves_root(self):
        self.custody.begin(); self.admit(); self.custody.recover()
        def exited(handle, timeout):
            self.control = {**self.control, "generation": "c"*32}
            return 0
        self.custody.wait = exited
        with self.assertRaises(CustodyUnknown): self.custody.cleanup()
        self.assertTrue(self.custody.exited)
        self.assertFalse(self.custody.may_remove_root)

    def test_control_change_while_pinning_closes_handle_and_preserves_root(self):
        self.custody.begin(); self.admit()
        def pin(control):
            self.control = {**self.control, "generation": "c"*32}
            return "owned-image-handle"
        self.custody.pin = pin
        with self.assertRaises(CustodyUnknown): self.custody.cleanup()
        self.assertFalse(self.custody.may_remove_root)
        self.assertEqual(self.closed, ["owned-image-handle"])

    def test_failed_termination_and_final_wait_preserve_pinned_handle_and_root(self):
        for terminated in (False, True):
            for final in (0x102, 0xffffffff):
                with self.subTest(terminated=terminated, final=final):
                    self.setUp(); self.custody.begin(); self.admit(); self.custody.recover()
                    self.terminated = terminated; self.waits = [0x102, final]
                    with self.assertRaises(CustodyUnknown): self.custody.cleanup()
                    self.assertFalse(self.custody.may_remove_root)
                    self.assertIsNotNone(self.custody.handle); self.assertEqual(self.closed, [])

    def test_forced_exit_requires_final_handle_signal(self):
        self.custody.begin(); self.admit(); self.waits = [0x102, 0]
        self.assertTrue(self.custody.cleanup())
        self.assertTrue(self.custody.may_remove_root)

    def test_native_termination_adapter_passes_handle_and_exit_code(self):
        calls = []
        class Kernel:
            # The real ctypes binding requires both arguments. A raw function
            # passed as the one-argument callback must fail this boundary.
            def TerminateProcess(self, handle, exit_code):
                calls.append((handle, exit_code))
                return True
        self.custody.terminate = termination_adapter(Kernel())
        self.custody.begin(); self.admit(); self.waits = [0x102, 0]
        self.assertTrue(self.custody.cleanup())
        self.assertEqual(calls, [("owned-image-handle", 1)])
        self.assertEqual(self.closed, ["owned-image-handle"])
        self.assertTrue(self.custody.may_remove_root)

    def test_no_launch_allows_root_cleanup(self):
        self.assertFalse(self.custody.cleanup())
        self.assertTrue(self.custody.may_remove_root)

    def test_unknown_root_survives_unittest_cleanup_and_gc(self):
        temporary = tempfile.TemporaryDirectory(prefix="gateway-custody-control-")
        root = Path(temporary.name)
        case = unittest.TestCase()
        case.addCleanup(temporary.cleanup)
        guard_root_cleanup(case, temporary, lambda: False)
        with self.assertRaises(CustodyUnknown):
            callback, args, kwargs = case._cleanups.pop()
            callback(*args, **kwargs)
        del callback, temporary
        gc.collect()
        self.assertTrue(root.is_dir())
        root.rmdir()  # Pure fixture has no child; dispose only after the assertion.

    def test_confirmed_root_runs_original_cleanup(self):
        temporary = tempfile.TemporaryDirectory(prefix="gateway-custody-control-")
        root = Path(temporary.name)
        case = unittest.TestCase()
        case.addCleanup(temporary.cleanup)
        guard_root_cleanup(case, temporary, lambda: True)
        self.assertTrue(case.doCleanups())
        self.assertFalse(root.exists())

    def host_fixture(self, retain=False):
        # Exercise the real inherited setup/cleanup registration without a CLI
        # call or Windows process. The selected file is checked, never executed.
        class Fixture(windows_host.HostCase):
            def stop_host(self):
                pass

            def cleanup_temp(self):
                self.cleanup_calls += 1
                if retain:
                    self.temp._finalizer.detach()
                else:
                    super().cleanup_temp()

        case = Fixture()
        case.cleanup_calls = 0
        with patch.object(windows_host, 'HGS', Path(__file__).resolve()):
            case.setUp()
        return case

    def test_actual_host_cleanup_is_fenced_until_custody_is_known(self):
        case = self.host_fixture()
        root = case.root
        self.addCleanup(case.temp.cleanup)  # Pure fixture has no child.
        confirmed = False
        guard_root_cleanup(case, case.temp, lambda: confirmed,
                           cleanup_callback=case.cleanup_temp)
        callback, args, kwargs = case._cleanups[0]
        with self.assertRaises(CustodyUnknown):
            callback(*args, **kwargs)
        self.assertEqual(case.cleanup_calls, 0)
        gc.collect()
        self.assertTrue(root.is_dir())
        confirmed = True
        self.assertTrue(case.doCleanups())
        self.assertEqual(case.cleanup_calls, 1)
        self.assertFalse(root.exists())

    def test_confirmed_custody_preserves_the_registered_cleanup_policy(self):
        case = self.host_fixture(retain=True)
        root = case.root
        self.addCleanup(case.temp.cleanup)  # Dispose only this child-free fixture.
        guard_root_cleanup(case, case.temp, lambda: True,
                           cleanup_callback=case.cleanup_temp)
        self.assertTrue(case.doCleanups())
        self.assertEqual(case.cleanup_calls, 1)
        gc.collect()
        self.assertTrue(root.is_dir())


if __name__ == "__main__": unittest.main()
