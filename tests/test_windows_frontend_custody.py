"""Portable controls for original-child custody in the Windows native fixture."""
import subprocess
from types import SimpleNamespace
import unittest
from unittest import mock
from pathlib import Path

import windows_frontend_auth as native_fixture


class WindowsFrontendCustody(unittest.TestCase):
    def child(self, *, exited=False, timeout=False):
        events = []
        process = SimpleNamespace(returncode=78 if exited else None)
        handle = SimpleNamespace(closed=False)

        def poll():
            return process.returncode

        def terminate():
            events.append('original_terminate')

        def communicate(*, timeout):
            self.assertEqual(timeout, 8)
            events.append('original_pipe_join')
            if must_timeout:
                raise subprocess.TimeoutExpired('original fixture child', timeout)
            process.returncode = 78 if exited else 0
            return '', ''

        def close():
            self.assertIsNotNone(process.returncode)
            events.append('original_handle_close')
            handle.closed = True

        must_timeout = timeout
        process.poll, process.terminate, process.communicate = poll, terminate, communicate
        handle.Close = close
        process._handle = handle
        return process, events

    def test_already_exited_original_still_drains_pipes_and_closes_its_handle(self):
        process, events = self.child(exited=True)
        native_fixture.WindowsFrontend.stop(self, process)
        self.assertEqual(events, ['original_pipe_join', 'original_handle_close'])
        self.assertTrue(process._handle.closed)

    def test_live_original_is_joined_before_its_handle_closes(self):
        process, events = self.child()
        native_fixture.WindowsFrontend.stop(self, process)
        self.assertEqual(events, ['original_terminate', 'original_pipe_join', 'original_handle_close'])

    def test_uncertain_original_pipe_join_retains_the_original_handle(self):
        process, events = self.child(timeout=True)
        with self.assertRaises(subprocess.TimeoutExpired):
            native_fixture.WindowsFrontend.stop(self, process)
        self.assertEqual(events, ['original_terminate', 'original_pipe_join'])
        self.assertFalse(process._handle.closed)
        self.assertFalse(getattr(process, '_fixture_settled', False))

    def test_repeated_cleanup_does_not_signal_or_join_a_settled_original_again(self):
        process, events = self.child()
        native_fixture.WindowsFrontend.stop(self, process)
        before = events.copy()
        native_fixture.WindowsFrontend.stop(self, process)
        self.assertEqual(events, before)


class WindowsTaskModeCustody(unittest.TestCase):
    def fixture(self):
        case = native_fixture.WindowsTaskMode(
            'test_explicit_mode_round_trip_uses_the_own_task_and_preserves_unrelated_processes')
        case.fixture_fence = native_fixture.TaskModeFence()
        case.root = Path('/synthetic/task-mode')
        case.task = 'hgs-desktop-synthetic-user'
        case.cli_image = case.root / 'hgs.exe'
        case.env = {}
        case.task_may_remain = False
        return case

    def test_retained_task_fences_setup_before_any_probe_or_path_allocation(self):
        first = self.fixture()
        first.task_may_remain = True
        first.retain_task_custody()
        later = self.fixture()
        later.fixture_fence = first.fixture_fence
        with mock.patch.object(native_fixture.tempfile, 'mkdtemp') as allocate, \
                mock.patch.object(native_fixture.subprocess, 'run') as probe:
            with self.assertRaisesRegex(unittest.SkipTest, first.id()):
                later.setUp()
        allocate.assert_not_called()
        probe.assert_not_called()
        later.fixture_fence.retain('later failure', 'another task', 'another root')
        self.assertEqual(later.fixture_fence.retained[0], first.id())

    def test_unknown_install_reply_retains_custody_but_confirmed_remove_releases_it(self):
        case = self.fixture()
        with mock.patch.object(native_fixture.subprocess, 'run',
                               side_effect=subprocess.TimeoutExpired('fixture install', 20)):
            with self.assertRaises(subprocess.TimeoutExpired):
                case.cli('desktop', 'registration', 'install', '--json')
        self.assertTrue(case.task_may_remain)
        with mock.patch.object(native_fixture.subprocess, 'run', return_value=SimpleNamespace(
                returncode=0, stdout='{"registered":false}', stderr='')):
            case.cli('desktop', 'registration', 'remove', '--json')
        case.retain_task_custody()
        case.fixture_fence.check()
        self.assertIsNone(case.fixture_fence.retained)

    def test_first_failure_stays_failed_and_the_two_dependents_are_skipped(self):
        fence = native_fixture.TaskModeFence()

        class Cases(unittest.TestCase):
            def setUp(self):
                fence.check()

            def test_first(self):
                self.addCleanup(fence.retain, self.id(), 'own task', '/synthetic/retained')
                self.fail('mode_transition_unconfirmed')

            def test_second(self):
                self.fail('dependent case must never run')

            def test_third(self):
                self.fail('dependent case must never run')

        result = unittest.TestResult()
        unittest.defaultTestLoader.loadTestsFromTestCase(Cases).run(result)
        self.assertEqual((result.testsRun, len(result.failures), len(result.errors), len(result.skipped)),
                         (3, 1, 0, 2))
        self.assertIn('mode_transition_unconfirmed', result.failures[0][1])
        for _, reason in result.skipped:
            self.assertIn('test_first', reason)
            self.assertIn('retained under custody', reason)


if __name__ == '__main__':
    unittest.main()
