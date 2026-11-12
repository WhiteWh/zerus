"""Finite scheduler controls: memory processes and private stdlib unittest children only."""
import collections
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('native_suite_workers', ROOT / 'scripts/ci/native_suite_workers.py')
workers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(workers)


def passing(ids):
    return dict(assigned=ids, started=ids, completed=ids, canceled=False, tests=len(ids),
                failures=0, errors=0, skipped=0, expected_failures=0, unexpected_successes=0,
                successful=True, temporary_entries=[], error=None)


class Clock:
    def __init__(self):
        self.now = 0

    def monotonic(self):
        self.now += .001
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class Process:
    def __init__(self, argv, mode, inventory, **options):
        self.pid = 100 + int(argv[3].rsplit('/', 1)[-1])
        self.returncode = None
        self.argv, self.mode, self.inventory = argv, mode, inventory
        self.terminated = self.killed = False
        self.plans = []
        process = self

        class Input(io.BytesIO):
            def close(self):
                if not self.closed:
                    raw = self.getvalue()
                    if raw:
                        plan = json.loads(raw)
                        process.plans.append(plan)
                        ids = [name for index in plan['suites'] for name in inventory[index]]
                        report = passing(ids)
                        if mode == 'wrong-result':
                            report['completed'] = ids[:-1]
                        if mode == 'setup-error':
                            report['errors'] = 1
                        if mode == 'skip':
                            report['skipped'] = 1
                        if mode == 'leftover':
                            report['temporary_entries'] = ['fixture-not-drained']
                        if mode != 'missing-result':
                            workers.write_json(Path(argv[5]), report)
                        marker = 'ZERUS_NATIVE_WORKER_RESULT=' + json.dumps(report) + '\n'
                        if mode == 'missing-marker':
                            marker = ''
                        elif mode == 'duplicate-marker':
                            marker *= 2
                        elif mode == 'conflicting-marker':
                            marker = 'ZERUS_NATIVE_WORKER_RESULT={}\n'
                        Path(options['stdout'].name).write_text(marker)
                        if mode not in ('stuck', 'unreaped'):
                            process.returncode = int(mode == 'setup-error')
                    elif mode not in ('stuck', 'unreaped'):
                        process.returncode = 1
                super().close()

        self.stdin = Input()
        if mode == 'early-exit':
            self.returncode = 1
        elif mode != 'no-discovery':
            value = dict(names=argv[7:], suites=inventory)
            if mode == 'different-discovery':
                value['suites'] = [['other.case'], *inventory[1:]]
            workers.write_json(Path(argv[4]), value)

    def poll(self):
        return self.returncode

    def terminate(self):
        self.terminated = True
        if self.mode not in ('stuck', 'unreaped'):
            self.returncode = -15

    def kill(self):
        self.killed = True
        if self.mode != 'unreaped':
            self.returncode = -9


class NativeSuiteWorkers(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='native-worker-controls-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.log = self.root / 'default.log'
        self.hgs, self.claude = self.root / 'hgs-input', self.root / 'claude-input'
        self.hgs.write_bytes(b'standard-library control: no native executable')
        self.claude.write_bytes(b'standard-library control: no provider executable')
        self.env = dict(os.environ, HGS_TEST_BIN=str(self.hgs), HGS_CLAUDE_TEST_BIN=str(self.claude),
                        PYTHONPATH=str(self.root), CONTROL_ROOT=str(self.root))

    def remember_worker_root(self):
        if (path := self.log.with_suffix('.workers.json')).exists():
            root = Path(json.loads(path.read_text())['root'])
            # Only these controls' synthetic direct workers ran; the fixture has
            # neither tmux nor a provider nor any descendant process to drain.
            self.addCleanup(shutil.rmtree, root)
            for temporary in json.loads(path.read_text()).get('temporary_roots', []):
                self.addCleanup(shutil.rmtree, temporary)

    def simulate(self, modes=('pass', 'pass'), *, clock=None, timeout=30,
                 native_variable='HGS_CLAUDE_TEST_BIN', worker_count=2):
        clock = clock or Clock()
        processes = []
        inventory = [['first.case', 'duplicate.clear'], ['second.case'], ['duplicate.clear']]

        def launch(argv, **options):
            mode = modes[len(processes)]
            if mode == 'spawn-error':
                raise OSError('controlled second spawn failure')
            process = Process(argv, mode, inventory, **options)
            processes.append(process)
            return process

        try:
            with patch.object(workers.subprocess, 'Popen', side_effect=launch), \
                    patch.object(workers.time, 'monotonic', side_effect=clock.monotonic), \
                    patch.object(workers.time, 'sleep', side_effect=clock.sleep), \
                    patch('sys.stdout', io.StringIO()):
                status, stdout = workers.run_suites(['first', 'second', 'third'], self.env, timeout,
                                                  self.root, self.log, native_variable=native_variable,
                                                  worker_count=worker_count)
        finally:
            self.remember_worker_root()
        return status, stdout, json.loads(self.log.with_suffix('.workers.json').read_text()), processes

    def test_explicit_short_parent_never_falls_back_and_keeps_socket_bound(self):
        selected = self.root / ('selected-' + 'x' * 100)
        selected.mkdir(mode=0o700)
        self.env['ZERUS_TEST_SHORT_TMPDIR'] = str(selected)
        status, _, record, processes = self.simulate()
        self.assertEqual(Path(record['root']).parent, selected)
        self.assertNotEqual(status, 0)
        self.assertEqual(processes, [])
        self.assertIn('socket bound', ' '.join(record.get('problems', [])) + str(record))

    def test_partition_keeps_whole_suite_order_and_duplicate_occurrences(self):
        inventory = [['duplicate.clear', 'first'], ['middle'], ['duplicate.clear'], ['tail']]
        assignments = workers.partition(inventory)
        self.assertEqual(sorted(index for indexes in assignments for index in indexes), list(range(4)))
        self.assertTrue(all(indexes == sorted(indexes) for indexes in assignments))
        before = collections.Counter(name for group in inventory for name in group)
        after = collections.Counter(name for indexes in assignments for index in indexes for name in inventory[index])
        self.assertEqual(before, after)
        self.assertEqual(after['duplicate.clear'], 2)
        for invalid in ([], [[]], [['one']], [['one'], []], [[None], ['other']]):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                workers.partition(invalid)
        for count in (0, 1, 4, True):
            with self.subTest(count=count), self.assertRaises(ValueError):
                workers.partition(inventory, count)

    def test_private_environment_precedes_imports_and_does_not_redirect_case_claude_config(self):
        # Match run_suites' short root even when macOS supplies a long TMPDIR.
        private = tempfile.TemporaryDirectory(prefix='nwenv-', dir=os.environ.get('ZERUS_TEST_SHORT_TMPDIR', '/tmp'))
        self.addCleanup(private.cleanup)
        base = Path(private.name).resolve()
        envs = []
        for index in range(2):
            root = base / str(index)
            root.mkdir()
            envs.append(workers.worker_environment(dict(self.env, CLAUDE_CONFIG_DIR='/unowned', TMUX='unowned'), root))
        for key in ('HOME', 'TMPDIR', 'HGS_STATE_DIR', 'HGS_CONFIG_DIR', 'XDG_CONFIG_HOME',
                    'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME', 'XDG_RUNTIME_DIR', 'CODEX_HOME'):
            self.assertNotEqual(envs[0][key], envs[1][key], key)
            self.assertTrue(all(Path(env[key]).is_dir() for env in envs), key)
        self.assertTrue(all(env['PYTHONDONTWRITEBYTECODE'] == '1' for env in envs))
        self.assertTrue(all('CLAUDE_CONFIG_DIR' not in env and 'TMUX' not in env for env in envs))
        long_root = base / ('x' * 90)
        long_root.mkdir()
        with self.assertRaisesRegex(ValueError, 'socket bound'):
            workers.worker_environment(self.env, long_root)

    def test_success_has_one_aggregate_marker_and_one_shared_original_end(self):
        status, stdout, record, processes = self.simulate()
        self.assertEqual(status, 0, record['problems'])
        self.assertEqual(stdout.count('ZERUS_NATIVE_RESULT='), 1)
        self.assertEqual(json.loads(stdout.split('=', 1)[1]), dict(tests=4, skipped=0, workers=2, qualified=True))
        self.assertEqual([process.argv[6] for process in processes], [str(record['deadline'])] * 2)
        self.assertEqual(record['deadline'] - record['work_deadline'], 10)
        self.assertEqual(record['timeout'], 30)
        self.assertEqual(len(processes), 2)
        self.assertTrue(all(len(process.plans) == 1 and not process.terminated for process in processes))
        self.assertEqual([self.hgs.read_bytes(), self.claude.read_bytes()],
                         [b'standard-library control: no native executable', b'standard-library control: no provider executable'])
        for row in record['binaries'].values():
            staged = Path(row['staged'])
            self.assertEqual(staged.stat().st_mode & 0o777, 0o500)
            self.assertEqual(staged.read_bytes(), Path(row['source']).read_bytes())

    def test_three_workers_keep_one_original_end_and_every_duplicate_occurrence(self):
        status, stdout, record, processes = self.simulate(('pass', 'pass', 'pass'), worker_count=3)
        self.assertEqual(status, 0, record['problems'])
        self.assertEqual(json.loads(stdout.split('=', 1)[1]), dict(tests=4, skipped=0, workers=3, qualified=True))
        self.assertEqual(len(processes), 3)
        self.assertEqual([process.argv[6] for process in processes], [str(record['deadline'])] * 3)
        actual = collections.Counter(name for row in record['workers'] for name in row['result']['completed'])
        self.assertEqual(actual, collections.Counter(['first.case', 'second.case', 'duplicate.clear', 'duplicate.clear']))

    def test_missing_partial_error_skipped_or_undrained_worker_never_qualifies(self):
        for mode in ('early-exit', 'different-discovery', 'wrong-result', 'setup-error', 'skip', 'leftover',
                     'missing-result', 'missing-marker', 'duplicate-marker', 'conflicting-marker'):
            with self.subTest(mode=mode):
                # Fresh artifact paths for each finite arm; never replay an arm.
                self.log = self.root / (mode + '.log')
                status, stdout, record, processes = self.simulate(('pass', mode))
                self.assertEqual(status, 1)
                self.assertFalse(json.loads(stdout.split('=', 1)[1])['qualified'])
                self.assertTrue(record['problems'])
                self.assertTrue(Path(record['root']).exists())
                self.assertIn('UNKNOWN', record['cleanup'])
                self.assertEqual(len(processes), 2)
                self.assertTrue(all(len(process.plans) <= 1 for process in processes))

    def test_third_worker_mismatch_partial_result_or_skip_never_qualifies(self):
        for mode in ('different-discovery', 'wrong-result', 'skip'):
            with self.subTest(mode=mode):
                self.log = self.root / ('third-' + mode + '.log')
                status, stdout, record, processes = self.simulate(('pass', 'pass', mode), worker_count=3)
                self.assertEqual(status, 1, record)
                self.assertFalse(json.loads(stdout.split('=', 1)[1])['qualified'])
                self.assertEqual(len(processes), 3)
                if mode == 'different-discovery':
                    self.assertTrue(all(process.plans == [] for process in processes))

    def test_parent_cancellation_and_late_final_record_never_return_success(self):
        original_write = workers.write_json
        for kind in ('cancel', 'late'):
            with self.subTest(kind=kind):
                self.log = self.root / (kind + '.log')
                clock = Clock()

                def write(path, value):
                    if path == self.log.with_suffix('.workers.json'):
                        if kind == 'cancel':
                            raise workers.WorkerCanceled('controlled cancellation during final record')
                        clock.now = value['deadline'] + 1
                    original_write(path, value)

                with patch.object(workers, 'write_json', side_effect=write):
                    if kind == 'cancel':
                        previous = workers.signal.getsignal(workers.signal.SIGTERM)
                        with self.assertRaises(workers.WorkerCanceled):
                            self.simulate(clock=clock)
                        self.assertIs(workers.signal.getsignal(workers.signal.SIGTERM), previous)
                    else:
                        status, stdout, record, _ = self.simulate(clock=clock)
                        self.assertEqual(status, 1)
                        self.assertFalse(json.loads(stdout.split('=', 1)[1])['qualified'])
                        self.assertTrue(any('final recording' in problem for problem in record['problems']))

    def test_only_linux_codex_and_macos_claude_ui_select_the_partitioned_runner(self):
        compat_spec = importlib.util.spec_from_file_location('compat_scheduler_control', ROOT / 'scripts/ci/agent-compatibility.py')
        compat = importlib.util.module_from_spec(compat_spec)
        compat_spec.loader.exec_module(compat)
        for agent, target in (('claude', 'darwin-arm64'), ('claude', 'linux-x64'),
                              ('claude', 'win32-x64'), ('codex', 'linux-x64'), ('codex', 'win32-x64'),
                              ('opencode', 'darwin-arm64')):
            with self.subTest(agent=agent, target=target):
                settings = compat.target_spec(agent, target)
                version = '1.2.3'
                package = dict(target=target, version=version, integrity='stand-in-only',
                               url=compat.AGENTS[agent]['tarball'].format(version=version, target=target))
                results = self.root / (agent + '-' + target)
                result = (0, 'ZERUS_NATIVE_RESULT={"tests":95,"skipped":0}\n')
                with patch.object(compat, 'install_native', return_value=self.claude), \
                        patch.object(compat, 'probe', return_value=settings['version_output'].format(version)), \
                        patch.object(compat, 'schema_snapshot', return_value={}), \
                        patch.object(compat, 'run_suites', return_value=result) as single, \
                        patch.object(compat, 'run_partitioned_suites', return_value=result) as paired, \
                        patch('sys.stdout', io.StringIO()):
                    row = compat.qualify(agent, package, self.root, results, self.hgs)
                self.assertEqual(row['status'], 'passed', row.get('error'))
                expected = paired if (agent, target) in (('claude', 'darwin-arm64'), ('codex', 'linux-x64')) else single
                rejected = single if expected is paired else paired
                self.assertTrue(expected.called)
                rejected.assert_not_called()
                self.assertTrue(all(call.args[2] == settings['timeout'] for call in expected.call_args_list))
                if expected is paired:
                    self.assertTrue(all(call.kwargs == {'native_variable': settings['variable']}
                                        for call in expected.call_args_list))

    def test_codex_stages_only_its_selected_private_native_image(self):
        codex = self.root / 'codex-input'
        codex.write_bytes(b'standard-library control: selected private Codex')
        self.env['HGS_CODEX_TEST_BIN'] = str(codex)
        status, _, record, _ = self.simulate(native_variable='HGS_CODEX_TEST_BIN')
        self.assertEqual(status, 0, record['problems'])
        self.assertEqual(set(record['binaries']), {'HGS_TEST_BIN', 'HGS_CODEX_TEST_BIN'})
        staged = Path(record['binaries']['HGS_CODEX_TEST_BIN']['staged'])
        self.assertEqual(staged.name, 'codex')
        self.assertEqual(staged.read_bytes(), codex.read_bytes())
        self.assertEqual(staged.stat().st_mode & 0o777, 0o500)
        self.assertEqual(len(record['temporary_roots']), 2)
        self.assertEqual(len(set(record['temporary_roots'])), 2)
        for temporary in map(Path, record['temporary_roots']):
            self.assertEqual(temporary.parent, Path(self.env.get('ZERUS_TEST_SHORT_TMPDIR', '/tmp')).resolve())
            self.assertEqual(temporary.stat().st_mode & 0o777, 0o700)
            self.assertFalse(temporary.is_relative_to(record['root']))

    def test_codex_partition_preserves_every_original_native_method(self):
        compat_spec = importlib.util.spec_from_file_location('compat_inventory_control', ROOT / 'scripts/ci/agent-compatibility.py')
        compat = importlib.util.module_from_spec(compat_spec)
        compat_spec.loader.exec_module(compat)
        # Discovery only: the real fixtures and their opt-in guards never run.
        with patch.object(sys, 'path', [str(ROOT / 'tests'), *sys.path]):
            loader = unittest.TestLoader()
            inventory = [workers.case_ids(group) for group in loader.loadTestsFromNames(compat.AGENTS['codex']['suites'])]
        self.assertEqual(loader.errors, [])
        original = collections.Counter(name for group in inventory for name in group)
        assignments = workers.partition(inventory, 3)
        actual = collections.Counter(name for indexes in assignments for index in indexes for name in inventory[index])
        self.assertEqual(actual, original)
        self.assertEqual(sum(original.values()), 57)
        self.assertEqual([sum(len(inventory[index]) for index in indexes) for indexes in assignments], [19, 19, 19])
        for name in ('test_codex_input_native.NativeCodexStartup.test_resumed_and_restored_hook_review_blocks_input_until_the_person_answers',
                     'test_clear_input_native.NativeClearInputCodex.test_clear_input_discards_a_residue_and_codex_keeps_running'):
            self.assertEqual(actual[name], 1)

    def test_cli_staging_does_not_widen_the_provider_image_bound(self):
        with patch.object(workers, 'MAX_HGS_BYTES', len(self.hgs.read_bytes())), \
                patch.object(workers, 'MAX_BINARY_BYTES', len(self.claude.read_bytes()) - 1):
            status, _, record, processes = self.simulate()
        self.assertEqual(status, 1)
        self.assertEqual(processes, [])
        self.assertEqual(set(record['binaries']), {'HGS_TEST_BIN'})
        self.assertTrue(any('binary exceeds its bound' in problem for problem in record['problems']))

    def test_startup_fixture_waits_for_its_own_turn_before_accepting_idle(self):
        with patch.object(sys, 'path', [str(ROOT / 'tests'), *sys.path]):
            import test_codex_input_native as native
        case = native.NativeCodexStartup('test_fresh_native_launch_reaches_its_first_turn_through_the_real_hooks')
        case.name, case.run_id, case.env = 'codex/fixture/startup', 'fixture-original-run', {}
        case.server = SimpleNamespace(requests=[])
        record = dict(conversation_id='fixture-original-conversation', run_id=case.run_id,
                      activity='idle', transcript='fixture-native-rollout')
        events = []
        case.binding = lambda name: record
        case.inspect = lambda: dict(first_message_can_send=True, events=events)
        case.screen = lambda: 'Synthetic startup frame'

        def until(label, predicate):
            if label.startswith('Fresh native composer'):
                self.assertTrue(predicate())
                return
            # SessionStart can publish idle before the submitted request and Stop.
            for stage in range(3):
                case.server.requests = [] if stage == 0 else [dict(texts=['Synthetic first native message.'])]
                record['activity'] = 'busy' if stage == 1 else 'idle'
                events[:] = [dict(type=kind) for kind in
                             (('SessionStart',) if stage < 2 else ('SessionStart', 'UserPromptSubmit', 'Stop'))]
                if predicate():
                    self.assertEqual(stage, 2, label)
                    return
            self.fail('The fixture never admitted its exact completed turn')

        case.until = until
        result = SimpleNamespace(returncode=0, stdout='{"status":"submitted"}', stderr='')
        with patch.object(native.subprocess, 'run', return_value=result) as send:
            case.deliver_first()
        self.assertEqual(send.call_count, 2)  # Original request and its existing replay check.

    def test_partial_spawn_retains_custody_and_attempts_earlier_child_drain(self):
        status, _, record, processes = self.simulate(('stuck', 'spawn-error'))
        self.assertEqual(status, 1)
        self.assertEqual(len(processes), 1)
        self.assertTrue(processes[0].terminated)
        self.assertTrue(processes[0].killed)
        self.assertEqual(record['workers'][0]['returncode'], -9)
        self.assertTrue(Path(record['root']).exists())

    def test_discovery_and_worker_timeout_drain_inside_the_original_end(self):
        for mode in ('no-discovery', 'stuck', 'unreaped'):
            with self.subTest(mode=mode):
                self.log = self.root / (mode + '.log')
                clock = Clock()
                status, _, record, processes = self.simulate((mode, mode), clock=clock)
                self.assertEqual(status, 1)
                self.assertTrue(all(process.stdin.closed for process in processes))
                if mode != 'no-discovery':
                    self.assertTrue(all(process.terminated for process in processes))
                self.assertTrue(Path(record['root']).exists())
                self.assertLess(clock.now, record['deadline'] + .1)  # Deterministic memory clock, no timing benchmark.
                if mode == 'unreaped':
                    self.assertTrue(all(row['returncode'] is None for row in record['workers']))
                    self.assertTrue(any('UNKNOWN' in problem for problem in record['problems']))

    def test_result_requires_order_and_all_cleanup_and_failure_fields(self):
        expected = ['first', 'duplicate', 'duplicate']
        self.assertTrue(workers.healthy_result(passing(expected), expected))
        for key, value in [('assigned', ['first', 'duplicate']), ('started', list(reversed(expected))),
                           ('completed', expected[:-1]), ('tests', True), ('tests', 2), ('errors', 1),
                           ('failures', 1), ('skipped', 1), ('expected_failures', 1), ('unexpected_successes', 1),
                           ('canceled', True), ('successful', False), ('temporary_entries', ['unknown']), ('error', 'setup')]:
            with self.subTest(key=key, value=value):
                report = passing(expected)
                report[key] = value
                self.assertFalse(workers.healthy_result(report, expected))

    def test_real_stdlib_children_cross_the_start_barrier_and_preserve_duplicate_ids(self):
        source = '''import json, os, pathlib, tempfile, time, unittest
class First(unittest.TestCase):
    def test_private_worker(self):
        root = pathlib.Path(os.environ['CONTROL_ROOT'])
        home = pathlib.Path(os.environ['HOME'])
        self.assertEqual(pathlib.Path(tempfile.gettempdir()), pathlib.Path(os.environ['TMPDIR']))
        self.assertEqual(os.environ['PYTHONDONTWRITEBYTECODE'], '1')
        self.assertNotIn('CLAUDE_CONFIG_DIR', os.environ)
        for key in ('HGS_TEST_BIN', 'HGS_CLAUDE_TEST_BIN'):
            self.assertEqual(pathlib.Path(os.environ[key]).stat().st_mode & 0o777, 0o500)
        (root / ('ready-' + home.parent.name)).touch()
        end = time.monotonic() + 5
        while len(list(root.glob('ready-*'))) < 2:
            if time.monotonic() >= end: self.fail('the other worker never reached its case')
            time.sleep(.01)
        with tempfile.TemporaryDirectory() as private:
            self.assertTrue(pathlib.Path(private).is_relative_to(os.environ['TMPDIR']))
'''
        (self.root / 'first.py').write_text(source)
        (self.root / 'second.py').write_text('from first import First\n')
        try:
            with patch('sys.stdout', io.StringIO()):
                status, stdout = workers.run_suites(['first', 'second'], self.env, 30, self.root, self.log)
        finally:
            self.remember_worker_root()
        record = json.loads(self.log.with_suffix('.workers.json').read_text())
        self.assertEqual(status, 0, record)
        self.assertEqual(record['inventory'], [['first.First.test_private_worker']] * 2)
        self.assertEqual(json.loads(stdout.split('=', 1)[1])['tests'], 2)
        self.assertEqual(sorted(row['result']['completed'] for row in record['workers']),
                         [['first.First.test_private_worker']] * 2)
        self.assertFalse((self.root / '__pycache__').exists())


if __name__ == '__main__':
    unittest.main()
