"""Memory/source controls; none starts a process, native agent or OS service."""
import ast
import ctypes
import hashlib
import importlib.util
import json
import os
import sys
from pathlib import Path, PurePosixPath, PureWindowsPath
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import kimi_windows_process as process

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('kimi_approvals_gate', ROOT / 'scripts/ci/kimi-approvals.py')
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class ApprovalGateControls(unittest.TestCase):
    def test_one_latest_release_runs_all_selected_cases_or_discovery_fails_without_fallback(self):
        for system, platform in (('posix', 'darwin-arm64'), ('nt', 'win32-x64')):
            selected = gate.selected_methods(platform)
            for failed in (False, True):
                with self.subTest(system=system, discovery_failed=failed), tempfile.TemporaryDirectory(prefix='kimi-approval-selection-') as directory:
                    root = Path(directory).resolve(); runtime = root / 'runtime'; runtime.mkdir()
                    output = root / 'artifacts/test-results/fixture'
                    hgs, tmux = root / 'hgs', root / 'tmux'
                    hgs.write_bytes(b'hgs'); tmux.write_bytes(b'tmux')
                    conpty = root / 'conpty'; conpty.mkdir()
                    for name in ('conpty.dll', 'OpenConsole.exe'):
                        (conpty / name).write_bytes(b'synthetic tool; never executed')
                    git = root / 'git'
                    for name in ('bin/bash.exe', 'usr/bin/bash.exe', 'usr/bin/msys-2.0.dll'):
                        path = git / name; path.parent.mkdir(parents=True, exist_ok=True)
                        path.write_bytes(b'synthetic tool; never executed')
                    record = dict(version='2.3.1', image_sha256='b' * 64)
                    events = []
                    oracle_methods = ('test_kimi_approvals_windows.WindowsEnvironmentOracle.'
                                      'test_a_known_private_child_environment_matches_the_bounded_reader',)
                    def stage(images, evidence, target):
                        self.assertEqual(target, platform)
                        self.assertEqual(events, ['oracle'] if system == 'nt' else [])
                        events.append('stage')
                        if failed:
                            raise ValueError('unknown official latest')
                        filename = 'kimi-code-' + platform + ('.exe' if system == 'nt' else '')
                        image, admission = images / filename, images / 'selected-release.json'
                        for path in (image, admission, images / 'manifest.json'):
                            path.write_bytes(b'synthetic'); path.chmod(0o400)
                        return record, image, admission
                    def invoke(evidence, version, env, methods, **kwargs):
                        if kwargs:
                            self.assertEqual(kwargs, {'oracle': True})
                            self.assertEqual(system, 'nt')
                            self.assertEqual((version, methods), ('windows-environment-oracle', oracle_methods))
                            self.assertEqual(events, [])
                            self.assertFalse((evidence / 'reservation.json').exists())
                            self.assertFalse((evidence.parent / 'images/selected-release.json').exists())
                            events.append('oracle')
                            return
                        self.assertEqual(events, ['oracle', 'stage'] if system == 'nt' else ['stage'])
                        plan = json.loads((evidence / 'plan.json').read_text())
                        self.assertEqual((plan['native_cases'], plan['mac_shim_compiles'], tuple(plan['methods'])),
                                         (16, 16 if system == 'posix' else 0, selected))
                        reservation = json.loads((evidence / 'reservation.json').read_text())
                        self.assertEqual((reservation['state'], reservation['versions'], reservation['native_cases']), ('SPENT', ['2.3.1'], 16))
                        self.assertEqual((version, methods), ('2.3.1', selected))
                        self.assertIn('HGS_KIMI_TEST_RECORD', env)
                        events.append('native')
                    # Replace only the subject module's OS view: pathlib and the
                    # host platform keep their own actual implementation.
                    subject_os = SimpleNamespace(**dict(vars(os), name=system))
                    with mock.patch.dict(os.environ, dict(GITHUB_ACTIONS='true', RUNNER_ENVIRONMENT='github-hosted',
                                  RUNNER_TEMP=str(root), GITHUB_WORKSPACE=str(root), SystemRoot=str(root / 'system'))), \
                            mock.patch.object(sys, 'argv', [str(gate.__file__), '--source-sha', 'a' * 40, '--hgs', str(hgs),
                                  '--tmux', str(tmux), '--conpty', str(conpty), '--git-bash', str(git / 'bin/bash.exe'), '--output', str(output)]), \
                            mock.patch.object(gate, 'os', subject_os), \
                            mock.patch.object(gate, 'ROOT', root), mock.patch.object(gate, 'target', return_value=platform), \
                            mock.patch.object(gate, 'selected_methods', return_value=selected), \
                            mock.patch.object(gate.tempfile, 'mkdtemp', return_value=str(runtime)), \
                            mock.patch.object(gate.subprocess, 'check_output', return_value='a' * 40), \
                            mock.patch.object(gate.retained, 'source_map', return_value={}), \
                            mock.patch.object(gate, 'digest', return_value='b' * 64), \
                            mock.patch.object(gate.retained, 'stage_latest', side_effect=stage) as choose, \
                            mock.patch.object(gate, 'invoke', side_effect=invoke) as run, \
                            mock.patch.object(gate, 'retain_declared_roots', return_value={}):
                        if failed:
                            with self.assertRaises(ValueError):
                                gate.main()
                        else:
                            self.assertEqual(gate.main(), 0)
                        self.assertEqual(run.call_count, int(system == 'nt') + int(not failed))
                        choose.assert_called_once()
                    self.assertEqual(events, (['oracle'] if system == 'nt' else []) + ['stage'] + ([] if failed else ['native']))
                    receipt = json.loads((output / 'evidence/receipt.json').read_text())
                    self.assertEqual((receipt['outcome'], receipt['native_cases']), ('FAIL', None) if failed else ('PASS', 16))

    def test_both_platforms_keep_the_exact_seven_semantic_names(self):
        for platform in ('darwin-arm64', 'win32-x64'):
            methods = gate.methods(platform)
            self.assertEqual(len(methods), 7)
            self.assertEqual(tuple(item.rsplit('.', 1)[-1] for item in methods), gate.NAMES)

    def test_both_platforms_keep_the_exact_goal_names(self):
        for platform in ('darwin-arm64', 'win32-x64'):
            methods = gate.goal_methods(platform)
            self.assertEqual(len(methods), 8)
            self.assertEqual(tuple(item.rsplit('.', 1)[-1] for item in methods), gate.GOAL_NAMES)

    def test_a_missing_goal_method_fails_selection(self):
        source = (ROOT / 'tests/test_kimi_approvals_native.py').read_text(encoding='utf-8')
        missing = '    def test_goal_yolo_cancel_does_not_start(self):\n'
        self.assertIn(missing, source)
        with tempfile.TemporaryDirectory(prefix='kimi-goal-selection-') as directory:
            root = Path(directory)
            path = root / 'tests/test_kimi_approvals_native.py'
            path.parent.mkdir()
            path.write_text(source.replace(missing, '    def removed_goal(self):\n', 1), encoding='utf-8')
            with mock.patch.object(gate, 'ROOT', root), self.assertRaisesRegex(ValueError, 'exact eight goal'):
                gate.goal_methods('darwin-arm64')

    def test_both_platforms_keep_the_exact_plan_name(self):
        for platform in ('darwin-arm64', 'win32-x64'):
            methods = gate.plan_methods(platform)
            self.assertEqual(len(methods), 1)
            self.assertEqual(tuple(item.rsplit('.', 1)[-1] for item in methods), gate.PLAN_NAMES)

    def test_a_missing_plan_method_fails_selection(self):
        source = (ROOT / 'tests/test_kimi_approvals_native.py').read_text(encoding='utf-8')
        missing = '    def test_plan_review_approve(self):\n'
        self.assertIn(missing, source)
        with tempfile.TemporaryDirectory(prefix='kimi-plan-selection-') as directory:
            root = Path(directory)
            path = root / 'tests/test_kimi_approvals_native.py'
            path.parent.mkdir()
            path.write_text(source.replace(missing, '    def removed_plan(self):\n', 1), encoding='utf-8')
            with mock.patch.object(gate, 'ROOT', root), self.assertRaisesRegex(ValueError, 'exact one plan'):
                gate.plan_methods('darwin-arm64')

    def test_a_partial_or_skipped_result_never_qualifies(self):
        methods = gate.selected_methods('win32-x64')
        good = dict(tests=16, started=list(methods), completed=list(methods), failures=0, errors=0, skipped=0,
                    expected_failures=0, unexpected_successes=0)
        self.assertTrue(gate.qualifies(good, methods))
        for key in ('failures', 'errors', 'skipped', 'expected_failures', 'unexpected_successes'):
            self.assertFalse(gate.qualifies(dict(good, **{key: 1}), methods), key)
        for value in (15, True, '16'):
            self.assertFalse(gate.qualifies(dict(good, tests=value), methods))
        self.assertFalse(gate.qualifies(dict(good, completed=list(methods[:-1])), methods))
        self.assertFalse(gate.qualifies(dict(good, started=list(reversed(methods))), methods))

    def test_only_the_two_reviewed_host_architectures_are_admitted(self):
        for system, machine, expected in (('Darwin', 'arm64', 'darwin-arm64'),
                                           ('Windows', 'AMD64', 'win32-x64')):
            with mock.patch.object(gate.platform, 'system', return_value=system), \
                    mock.patch.object(gate.platform, 'machine', return_value=machine):
                self.assertEqual(gate.target(), expected)
        for system, machine in (('Windows', 'arm64'), ('Linux', 'x86_64'), ('Darwin', 'x86_64')):
            with mock.patch.object(gate.platform, 'system', return_value=system), \
                    mock.patch.object(gate.platform, 'machine', return_value=machine):
                with self.assertRaises(ValueError):
                    gate.target()

    def test_windows_pins_are_the_retained_official_manifest_entries(self):
        pins = gate.retained.checked_pins('win32-x64')
        self.assertEqual([pins[v]['win32-x64']['size'] for v in gate.retained.HISTORICAL_VERSIONS], [159601440, 160725792])
        self.assertEqual(pins['2.2.0']['win32-x64']['sha256'],
                         '5120f69812bc6437354d7d99e801c1b38fee1eeefdc858e2ad726356f1a245b0')
        original_read = Path.read_bytes
        manifest = ROOT / 'scripts/ci/kimi-native-manifests/2.1.1.json'
        changed = original_read(manifest).replace(b'\n', b'\r\n')
        def read(path):
            return changed if path == manifest else original_read(path)
        with mock.patch.object(Path, 'read_bytes', read), self.assertRaises(ValueError) as refused:
            gate.retained.checked_pins('win32-x64')
        diagnostic = json.loads(str(refused.exception).split(': ', 1)[1])
        self.assertEqual(diagnostic, dict(version='2.1.1', expected_sha256=pins['2.1.1']['manifest_sha256'],
                         observed_sha256=hashlib.sha256(changed).hexdigest(), size=len(changed),
                         crlf=changed.count(b'\r\n'), lf=changed.count(b'\n'), cr=changed.count(b'\r')))

    def test_windows_layout_proof_precedes_every_native_download_and_launch(self):
        tree = ast.parse((ROOT / 'scripts/ci/kimi-approvals.py').read_text(encoding='utf-8'))
        main = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == 'main')
        calls = [(node.lineno, ast.unparse(node.func), ast.unparse(node)) for node in ast.walk(main)
                 if isinstance(node, ast.Call)]
        oracle = next(line for line, func, text in calls if func == 'invoke' and 'oracle=True' in text)
        download = next(line for line, func, _ in calls if func == 'retained.stage_latest')
        launches = [line for line, func, text in calls if func == 'invoke' and 'oracle=True' not in text]
        self.assertLess(oracle, download)
        self.assertTrue(launches and all(download < line for line in launches))

    def test_mac_uses_the_existing_oracle_without_an_alternative_predicate(self):
        source = (ROOT / 'tests/test_kimi_approvals_native.py').read_text(encoding='utf-8')
        self.assertIn('guard.shim_source(self.captures, self.image)', source)
        self.assertIn('guard.NativeKimi.assert_process(self, self.name, stage)', source)
        for name in ('process_sample', 'environment', 'launched_environment'):
            self.assertIn(name + ' = guard.NativeKimi.' + name, source)
        import test_kimi_approvals_native as native
        case = native.NativeKimiApprovals('test_approve_once_runs_the_command')
        case.project = mock.Mock()
        case.project.resolve.return_value = PurePosixPath('/canonical/project with spaces')
        case.command = 'printf approved'
        item = dict(question='Run this command?', header='Bash',
                    body='cwd: ' + str(case.project.resolve()) + '\n$ printf approved',
                    options=[dict(label=label) for label in ('Approve once', 'Approve for this session', 'Reject')])
        card = dict(can_answer=True, answer_unavailable_reason='', answer_transport='kimi_tui', questions=[item])
        case.card = lambda: card
        self.assertIs(case.answerable(), card)
        item['body'] = 'cwd: /different/project with spaces\n$ printf approved'
        with self.assertRaises(AssertionError):
            case.answerable()
        case.version, case.name = '2.2.0', 'kimi/p/approval'
        case.project.resolve.return_value = PurePosixPath('/tmp/hgs-pause-x/project with spaces')
        case.until = lambda predicate: native.FixtureControls.CHOOSER
        case.t = mock.Mock()
        with mock.patch('builtins.print'):
            case.trust()
        case.t.assert_called_once_with('send-keys', '-t', '=kimi/p/approval:', 'Enter')
        case.t.reset_mock()
        case.project.resolve.return_value = Path('/different/project')
        with mock.patch('builtins.print'), self.assertRaises(AssertionError):
            case.trust()
        case.t.assert_not_called()

    def test_cwd_disclosure_preserves_the_whole_canonical_display_path_and_its_case(self):
        import test_kimi_approvals_native as native
        case = native.NativeKimiApprovals('test_approve_once_runs_the_command')
        case.project, case.command = mock.Mock(), 'printf approved'
        item = dict(question='Run this command?', header='Bash', options=[dict(label=label)
                    for label in ('Approve once', 'Approve for this session', 'Reject')])
        card = dict(can_answer=True, answer_unavailable_reason='', answer_transport='kimi_tui', questions=[item])
        case.card = lambda: card
        for folder, display in ((PureWindowsPath('D:/fixture/project with spaces'), 'D:/fixture/project with spaces'),
                                (PurePosixPath('/fixture/project with spaces'), '/fixture/project with spaces')):
            with self.subTest(folder=folder):
                case.project.resolve.return_value = folder
                self.assertEqual(folder.as_posix(), display)
                if isinstance(folder, PurePosixPath):
                    self.assertEqual(str(folder), display, 'Unix display bytes stay unchanged')
                for body in ('cwd: ' + display + '\n$ printf approved', '$ printf approved\ncwd: ' + display):
                    item['body'] = body
                    self.assertIs(case.answerable(), card)
                wrong = ['different/project', display + '-sibling', display + ' child']
                if display.startswith('D:'):
                    wrong.append(display.lower())
                for other in wrong:
                    item['body'] = 'cwd: ' + other + '\n$ printf approved'
                    with self.subTest(other=other), self.assertRaises(AssertionError):
                        case.answerable()

    def test_workflow_uses_exact_pr_head_or_manual_standard_hosted_without_write_permissions(self):
        source = (ROOT / '.github/workflows/kimi-approvals.yml').read_text(encoding='utf-8')
        self.assertIn('workflow_dispatch:', source)
        self.assertIn('pull_request:\n    branches: [main]\n    paths:', source)
        self.assertEqual(source.count('${{ github.event.pull_request.head.sha || github.sha }}'), 2)
        self.assertNotIn("'src/**'", source)
        self.assertNotIn("'tests/**'", source)
        for path in ('src/state/kimi_approval.rs', 'src/state/kimi_goal.rs', 'src/state/kimi_plan.rs',
                     'src/state/questions.rs', 'src/state/question_terminal.rs', 'src/state/mod.rs'):
            self.assertIn("- '" + path + "'", source)
        self.assertEqual(source.count('seven Bash, eight CreateGoal and one ExitPlanMode approval cases'), 2)
        self.assertNotIn('pull_request_target:', source)
        self.assertNotIn('self-hosted', source)
        self.assertIn('os: ["macos-15", "windows-2025"]', source)
        self.assertIn('contents: read', source)
        self.assertNotIn('contents: write', source)
        self.assertIn('cache-mode: none', source)
        self.assertIn("env:\n  PYTHONUTF8: '1'\n", source)
        self.assertIn('cargo +1.99.1 build --locked --bin hgs -j 2', source)


class WindowsWrapperControls(unittest.TestCase):
    def test_each_native_version_uses_the_reviewed_custody_only_wrapper_with_the_original_cap(self):
        for oracle in (False, True):
            with self.subTest(oracle=oracle), tempfile.TemporaryDirectory(prefix='kimi-wrapper-control-') as directory:
                out, selected, child = Path(directory), ('synthetic.case',), object()
                result = dict(tests=1, started=list(selected), completed=list(selected), failures=0, errors=0,
                              skipped=0, expected_failures=0, unexpected_successes=0)
                (out / 'fixture-result.json').write_text(json.dumps(result), encoding='utf-8')
                if not oracle:
                    (out / 'fixture-outside-job.log').write_bytes(b'synthetic native log')
                env = dict(SystemRoot=str(Path('/fixture/windows')), PATH='original-private-path')
                with mock.patch.object(gate, 'target', return_value='win32-x64'), \
                        mock.patch.object(gate.subprocess, 'Popen', return_value=child) as launch, \
                        mock.patch.object(gate.retained, 'await_child', return_value=0) as wait:
                    self.assertEqual(gate.invoke(out, 'fixture', env, selected, oracle=oracle), result)
                launch.assert_called_once()
                wait.assert_called_once_with(child, out / 'fixture.log', seconds=900)
                argv = launch.call_args.args[0]
                self.assertNotIn('--end', argv)
                if oracle:
                    self.assertNotIn('--custody-only', argv)
                    self.assertEqual(argv[-1], '--oracle')
                    self.assertEqual(launch.call_args.kwargs['env'], env)
                else:
                    self.assertEqual(argv[3], str(ROOT / 'scripts/ci/outside-job.py'))
                    self.assertEqual(argv[4:7], ['--custody-only', '--timeout', '900'])
                    self.assertIn('--suite-child', argv)
                    self.assertEqual(launch.call_args.kwargs['env']['PATH'], os.pathsep.join(
                        (str(Path(env['SystemRoot']) / 'System32'),
                         str(Path(env['SystemRoot']) / 'System32/WindowsPowerShell/v1.0'))))

    def test_an_unknown_wrapper_timeout_is_never_retried_or_accepted(self):
        with tempfile.TemporaryDirectory(prefix='kimi-wrapper-timeout-control-') as directory:
            out, child = Path(directory), object()
            with mock.patch.object(gate, 'target', return_value='win32-x64'), \
                    mock.patch.object(gate.subprocess, 'Popen', return_value=child) as launch, \
                    mock.patch.object(gate.retained, 'await_child', side_effect=TimeoutError('unknown fixture')) as wait, \
                    mock.patch.object(gate, 'qualifies') as accept:
                with self.assertRaisesRegex(TimeoutError, 'unknown fixture'):
                    gate.invoke(out, 'fixture', dict(SystemRoot=str(Path('/fixture/windows'))), ('synthetic.case',))
            launch.assert_called_once()
            wait.assert_called_once_with(child, out / 'fixture.log', seconds=900)
            accept.assert_not_called()


class WindowsEnvironmentControls(unittest.TestCase):
    def test_exact_utf16_environment_and_hidden_drive_cwd(self):
        block = 'HOME=private\0KIMI_CODE_HOME=private/kimi\0=C:=C:\\fixture\0\0'.encode('utf-16-le')
        self.assertEqual(process.decode_environment(block),
                         {'HOME': 'private', 'KIMI_CODE_HOME': 'private/kimi', '=C:': 'C:\\fixture'})

    def test_duplicates_case_collisions_missing_terminal_and_invalid_utf16_refuse(self):
        for text in ('HOME=a\0HOME=b\0\0', 'home=a\0HOME=b\0\0', 'HOME=a\0',
                     'HOME=a\0\0EXTRA=b\0\0', 'no-separator\0\0', '=bad\0\0'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                process.decode_environment(text.encode('utf-16-le'))
        for data in (b'', b'\x00', b'\x00\xd8\0\0\0\0', b'x' * (process.MAX_ENVIRONMENT + 2)):
            with self.assertRaises((ValueError, UnicodeError)):
                process.decode_environment(data)

    def test_changed_process_or_environment_refuses_before_returning_values(self):
        fake = object.__new__(process.Process)
        snapshot = dict(pid=7, image=os.path.normcase(os.path.realpath(ROOT)), parent=8,
                        parameters=100, environment=200)
        fake.snapshot = mock.Mock(side_effect=[snapshot, dict(snapshot, parent=9)])
        fake.block = mock.Mock(return_value='HOME=a\0\0'.encode('utf-16-le'))
        with self.assertRaisesRegex(OSError, 'changed'):
            fake.environment(ROOT, 8)
        fake.snapshot = mock.Mock(return_value=snapshot)
        fake.block = mock.Mock(side_effect=[b'first', b'second'])
        with self.assertRaisesRegex(OSError, 'changed'):
            fake.environment(ROOT, 8)

    def test_unknown_image_or_parent_never_reads_remote_environment(self):
        fake = object.__new__(process.Process)
        fake.snapshot = lambda: dict(image=os.path.normcase(os.path.realpath(ROOT)), parent=8)
        fake.block = mock.Mock(side_effect=AssertionError('must not read'))
        with self.assertRaisesRegex(OSError, 'image/parent'):
            fake.environment(ROOT, 9)
        fake.block.assert_not_called()


class WindowsCustodyControls(unittest.TestCase):
    def test_startup_trust_uses_the_exact_unbound_envelope_and_keeps_run_identity(self):
        import test_kimi_approvals_windows as windows
        case = windows.NativeWindowsKimiApprovals('test_approve_once_runs_the_command')
        run = '00000000-0000-4000-8000-000000000001'
        card = dict(source='kimi_folder_trust', can_answer=True, conversation_id=None,
                    run_id=run, question_id='synthetic-startup', question_hash='a' * 64)
        binding = dict(run_id=run, conversation_id=None)
        case.name = 'kimi/p/approval'
        case.inspect = mock.Mock(return_value=dict(pending_questions=[card]))
        case.binding = mock.Mock(return_value=binding)
        case.until = lambda predicate: predicate()
        case.screen = mock.Mock(return_value='Trust this folder?')
        case.call = mock.Mock(return_value=dict(status='answered'))
        case.trust()
        command, payload = case.call.call_args.args
        self.assertEqual(command, 'answer')
        self.assertEqual(windows.uuid.UUID(payload.pop('request_id')).version, 4)
        self.assertEqual(payload, dict(expected_run_id=run, expected_conversation_id='',
            question_id='synthetic-startup', expected_question_hash='a' * 64,
            answers=[dict(question_id='trust', selected_option_ids=['trust_0'], text='')]))
        for card_change, binding_change in ((dict(conversation_id=run), {}),
                ({}, dict(conversation_id=run)), (dict(run_id='other'), {})):
            case.inspect.return_value = dict(pending_questions=[dict(card, **card_change)])
            case.binding.return_value = dict(binding, **binding_change)
            case.call.reset_mock()
            with self.subTest(card=card_change, binding=binding_change), self.assertRaises(AssertionError):
                case.trust()
            case.call.assert_not_called()

    def test_only_a_typed_absence_before_launch_acknowledgement_can_omit_the_screen(self):
        import test_kimi_approvals_windows as windows
        with tempfile.TemporaryDirectory(prefix='kimi-absent-control-') as directory:
            case = windows.NativeWindowsKimiApprovals('test_approve_once_runs_the_command')
            case.name, case.home, case.server = 'kimi/p/approval', None, None
            case.launch_acknowledged = False
            case.binding = mock.Mock(return_value={})
            response = dict(error=dict(code='not_found', message='no such private session'))
            case.run_hgs = mock.Mock(return_value=SimpleNamespace(returncode=1,
                                      stdout=json.dumps(response), stderr=''))
            with mock.patch.object(windows.approvals, 'EVIDENCE', directory):
                case.retain()
                out = Path(directory) / case._testMethodName
                self.assertFalse((out / 'screen.txt').exists())
                self.assertEqual(json.loads((out / 'capture-absent.json').read_bytes())['response'], response)
                for acknowledged, record, code in ((True, {}, 'not_found'),
                        (False, dict(pid=7), 'not_found'), (False, {}, 'unknown')):
                    case.launch_acknowledged = acknowledged
                    case.binding.return_value = record
                    case.run_hgs.return_value.stdout = json.dumps(dict(error=dict(code=code)))
                    with self.subTest(acknowledged=acknowledged, record=record, code=code), \
                            self.assertRaises(AssertionError):
                        case.retain()

    def test_a_cleanup_failure_does_not_skip_later_stop_or_seal(self):
        import test_kimi_approvals_windows as windows
        case = windows.NativeWindowsKimiApprovals('test_approve_once_runs_the_command')
        class Root:
            def exists(self):
                return True
            def __str__(self):
                return 'synthetic-kept-root'
        case.root, case.images = Root(), Root()
        seen = []
        for name in ('retain', 'stop_client', 'stop_owned', 'seal'):
            def step(name=name):
                seen.append(name)
                if name == 'stop_client':
                    raise OSError('owned client stop unknown')
            setattr(case, name, step)
        case.server = mock.Mock()
        with mock.patch.object(windows.shutil, 'rmtree') as remove, mock.patch('builtins.print'):
            with self.assertRaisesRegex(AssertionError, 'attached client'):
                case.custody()
        self.assertEqual(seen, ['retain', 'stop_client', 'stop_owned', 'seal'])
        case.server.shutdown.assert_called_once_with()
        case.server.server_close.assert_called_once_with()
        remove.assert_not_called()

    def test_an_unknown_handle_close_does_not_skip_the_next_owned_handle(self):
        import test_kimi_approvals_windows as windows
        case = windows.NativeWindowsKimiApprovals('test_approve_once_runs_the_command')
        first, second = mock.Mock(), mock.Mock()
        first.wait.return_value = second.wait.return_value = True
        first.close.side_effect = OSError('close failed')
        case.pinned = {(1, 'one'): first, (2, 'two'): second}
        with self.assertRaisesRegex(AssertionError, 'handle close'):
            case.stop_owned()
        first.wait.assert_called_once_with(15)
        second.wait.assert_called_once_with(15)
        second.close.assert_called_once_with()
        self.assertEqual(case.pinned, {})


class WindowsSidControls(unittest.TestCase):
    def reader(self, offset=16, revision=1, count=2, length=16):
        from types import SimpleNamespace
        reader = object.__new__(process.Process)
        reader.w = SimpleNamespace(HANDLE=ctypes.c_void_p, DWORD=ctypes.c_uint32)
        reader.k = SimpleNamespace(CloseHandle=mock.Mock(return_value=True))
        def opened(_process, _access, token):
            token._obj.value = 9
            return True
        def information(_token, _kind, data, _size, needed):
            needed._obj.value = 32
            if data is not None:
                base = ctypes.addressof(data)
                ctypes.c_void_p.from_buffer(data).value = base + offset
                if 0 <= offset <= 24:
                    header = bytes((revision, count)) + b'\0' * 6
                    ctypes.memmove(base + offset, header, len(header))
            return True
        reader.a = SimpleNamespace(OpenProcessToken=opened, GetTokenInformation=information,
                                   GetLengthSid=mock.Mock(return_value=length))
        return reader

    def test_invalid_sid_pointer_and_declaration_never_reach_getlengthsid(self):
        for fields in (dict(offset=-1), dict(offset=31), dict(offset=16, revision=2),
                       dict(offset=16, count=16), dict(offset=16, count=3)):
            with self.subTest(fields=fields):
                reader = self.reader(**fields)
                with self.assertRaises(OSError):
                    reader.sid(7)
                reader.a.GetLengthSid.assert_not_called()
                reader.k.CloseHandle.assert_called_once()

    def test_bounded_sid_is_read_and_native_length_is_checked_again(self):
        reader = self.reader()
        self.assertEqual(len(reader.sid(7)), 16)
        reader.a.GetLengthSid.assert_called_once()
        for length in (0, 8, 68, 1024):
            reader = self.reader(length=length)
            with self.assertRaisesRegex(OSError, 'invalid TokenUser SID'):
                reader.sid(7)
            reader.a.GetLengthSid.assert_called_once()
            reader.k.CloseHandle.assert_called_once()


if __name__ == '__main__':
    unittest.main()
