#!/usr/bin/env python3
"""AST/memory-only diagnostic controls. Never import test_pause or execute an agent/CLI/tmux/proc read."""
import ast
import copy
import hashlib
import io
import json
from pathlib import Path, PurePosixPath, PureWindowsPath
import re
from types import SimpleNamespace
import unittest


SOURCE = (Path(__file__).resolve().parent / "test_pause.py").read_text()
TREE = ast.parse(SOURCE)
AGENT = next(ast.literal_eval(node.value) for node in TREE.body if isinstance(node, ast.Assign)
             and any(isinstance(target, ast.Name) and target.id == "AGENT" for target in node.targets))
SELECTED = [node for node in TREE.body if isinstance(node, (ast.FunctionDef, ast.ClassDef))
            and node.name in ("direct_signal_agent", "DirectSignalTrace")]
assert {node.name for node in SELECTED} == {"direct_signal_agent", "DirectSignalTrace"}


def load(**values):
    namespace = {"AGENT": AGENT, "json": json, "hashlib": hashlib, "Path": PurePosixPath, "re": re,
                 "time": SimpleNamespace(monotonic_ns=lambda: 100), **values}
    exec(compile(ast.Module(body=copy.deepcopy(SELECTED), type_ignores=[]), "<pause-signal-source>", "exec"), namespace)
    return namespace


class MemoryFile(io.BytesIO):
    def __init__(self, value, reads):
        super().__init__(value)
        self.reads = reads

    def read(self, size=-1):
        if size not in (4097, 8193, 8705):
            raise AssertionError("Unbounded diagnostic read")
        self.reads.append(size)
        return super().read(size)


class MemoryPath:
    def __init__(self, value):
        self.value = value

    def __truediv__(self, other):
        return MemoryPath(self.value + "/" + other)

    def __fspath__(self):
        return self.value

    def __str__(self):
        return self.value

    def mkdir(self, **options):
        if options != {"parents": True, "exist_ok": True}:
            raise AssertionError("Unexpected directory operation")


class MemoryArtifact(io.BytesIO):
    name = "/repo/artifacts/test-results/python/direct-signal-memory.json"

    def __init__(self, retained):
        super().__init__()
        self.retained = retained

    def fileno(self):
        return 17

    def __exit__(self, *args):
        self.retained.append(self.getvalue())
        return super().__exit__(*args)


class SignalTraceSource(unittest.TestCase):
    def test_only_the_three_linux_subcases_get_trace_without_changing_the_one_signal(self):
        archive = next(node for node in TREE.body if isinstance(node, ast.ClassDef) and node.name == "Archive")
        method = next(node for node in archive.body if isinstance(node, ast.FunctionDef) and node.name == "test_pause_and_agent_signal_remain_saved")
        text = ast.get_source_segment(SOURCE, method)
        self.assertIn('for agent in ("claude", "codex", "kimi"):', text)
        self.assertIn('trace = DirectSignalTrace(self.root, agent) if sys.platform == "linux" else None', text)
        self.assertEqual(text.count('os.kill(record["pid"], signal.SIGTERM)'), 1)
        self.assertEqual(text.count('self.wait_stopped(name)'), 1)
        self.assertLess(text.index('before_SIGTERM'), text.index('os.kill('))
        self.assertLess(text.index('os.kill('), text.index('after_SIGTERM_returned'))
        self.assertLess(text.index('after_SIGTERM_returned'), text.index('self.wait_stopped(name)'))
        self.assertIn('error.add_note(trace.failure())', text)
        wait = next(node for node in TREE.body if isinstance(node, ast.ClassDef) and node.name == "Harness")
        wait = next(node for node in wait.body if isinstance(node, ast.FunctionDef) and node.name == "wait")
        self.assertIn('until = time.monotonic() + 6', ast.get_source_segment(SOURCE, wait))
        self.assertNotIn('time.sleep(', text)
        self.assertNotIn('signal.SIGKILL', text)

    def test_wrapper_only_adds_failure_swallowing_phase_calls_to_the_exact_original_agent(self):
        wrapped = load()["direct_signal_agent"]()
        tree = ast.parse(wrapped)

        class StripTrace(ast.NodeTransformer):
            def visit_FunctionDef(self, node):
                return None if node.name == "signal_phase" else self.generic_visit(node)

            def visit_Expr(self, node):
                if isinstance(node.value, ast.Call) and isinstance(node.value.func, ast.Name) and node.value.func.id == "signal_phase":
                    return None
                return self.generic_visit(node)

        self.assertEqual(ast.dump(StripTrace().visit(tree)), ast.dump(ast.parse(AGENT)))
        phases = re.findall(r'signal_phase\("([A-Za-z0-9_]+)"\)', wrapped)
        self.assertEqual(phases, ['handler_enter', 'SessionEnd_begin', 'SessionEnd_end', 'handler_exit_143',
                                  'handler_registered', 'SessionStart_begin', 'SessionStart_end'])
        signal_phase = next(node for node in ast.parse(wrapped).body if isinstance(node, ast.FunctionDef) and node.name == "signal_phase")
        self.assertEqual(len(signal_phase.body), 1)
        self.assertIsInstance(signal_phase.body[0], ast.Try)
        exception = signal_phase.body[0].handlers[0].type
        self.assertIsInstance(exception, ast.Name)
        self.assertEqual(exception.id, "Exception")
        self.assertIsInstance(exception.ctx, ast.Load)
        self.assertIsInstance(signal_phase.body[0].handlers[0].body[0], ast.Pass)

    def test_agent_labels_and_paths_are_finite_and_cannot_mix(self):
        namespace = load()
        for agent in ("claude", "codex", "kimi"):
            trace = namespace['DirectSignalTrace'](MemoryPath('/fixture'), agent)
            self.assertEqual(trace.agent, agent)
        for agent in ("opencode", "../codex", "codex/other", "", None, False):
            with self.subTest(agent=agent), self.assertRaises(ValueError):
                namespace['DirectSignalTrace'](MemoryPath('/fixture'), agent)
        self.assertIn('home / ("direct-signal-agent-" + agent + ".jsonl")', namespace['direct_signal_agent']())
        self.assertIn('self.root / ("direct-signal-agent-" + self.agent + ".jsonl")', SOURCE)
        self.assertNotIn('glob(', ast.get_source_segment(SOURCE, SELECTED[1]))

    def test_phase_logging_errors_never_escape_or_send_a_signal(self):
        wrapped = load()['direct_signal_agent']()
        function = next(node for node in ast.parse(wrapped).body if isinstance(node, ast.FunctionDef) and node.name == "signal_phase")
        for failed in ('getpid', 'clock', 'open', 'fstat', 'write', 'close', None):
            calls = []; written = []

            def operation(name, value=None):
                def invoke(*args, **kwargs):
                    calls.append(name)
                    if name == failed:
                        raise OSError("memory-only fault")
                    if name == 'write':
                        written.append(args[1]); return len(args[1])
                    return value
                return invoke

            memory_os = SimpleNamespace(getpid=operation('getpid', 41), environ={'HGS_RUN_ID': 'run'},
                open=operation('open', 19), fstat=operation('fstat', SimpleNamespace(st_size=0)), write=operation('write'), close=operation('close'),
                O_WRONLY=1, O_CREAT=2, O_APPEND=4, O_CLOEXEC=8, O_NOFOLLOW=16)
            namespace = dict(os=memory_os, time=SimpleNamespace(monotonic_ns=operation('clock', 12)), json=json,
                             home=MemoryPath('/fixture'), agent='kimi', sid='conversation')
            exec(compile(ast.Module(body=[copy.deepcopy(function)], type_ignores=[]), '<phase-source>', 'exec'), namespace)
            namespace['signal_phase']('handler_enter')
            if written:
                self.assertEqual(json.loads(written[0])['agent'], 'kimi')
            if failed in ('fstat', 'write'):
                self.assertIn('close', calls)
            self.assertFalse(hasattr(memory_os, 'kill'))

    def test_bounded_status_samples_include_pending_bits_and_preserve_identity_refusal(self):
        def stat(pid, parent, birth):
            return (str(pid) + ' (fixture) ' + ' '.join(['S', str(parent)] + ['0'] * 17 + [str(birth), '0']) + '\n').encode()

        for mode in ('valid', 'changed-after', 'expected-mismatch', 'missing'):
            paths = []; reads = []; stats = 0

            def opened(path, flags):
                nonlocal stats
                paths.append(path)
                if mode == 'missing':
                    raise FileNotFoundError('memory-only missing')
                if path == '/proc/41/stat':
                    stats += 1
                    return MemoryFile(stat(41, 17, 200 if mode == 'changed-after' and stats > 1 else 100), reads)
                if path == '/proc/41/status':
                    return MemoryFile(b'SigBlk:\t0\nSigIgn:\t0\nSigCgt:\t4000\nSigPnd:\t4000\nShdPnd:\t0\nSigQ:\t1/1024\n', reads)
                raise AssertionError('Unexpected process read')

            namespace = load(open=opened)
            expected = {'pid': 41, 'ppid': 17, 'birth': 200} if mode == 'expected-mismatch' else None
            result = namespace['DirectSignalTrace'].process(41, expected)
            if mode == 'valid':
                self.assertEqual(result['masks'], {'SigBlk': '0', 'SigIgn': '0', 'SigCgt': '4000', 'SigPnd': '4000', 'ShdPnd': '0'})
                self.assertEqual(reads, [4097, 8193, 4097])
            else:
                self.assertIn('unknown', result)
            if mode == 'expected-mismatch':
                self.assertEqual(paths, ['/proc/41/stat'])

    def test_capture_keeps_agent_and_before_identity_without_adopting_an_unknown_target(self):
        namespace = load()
        trace = namespace['DirectSignalTrace'](MemoryPath('/fixture'), 'kimi')
        calls = []
        identities = {17: {'pid': 17, 'ppid': 3, 'birth': 90}, 41: {'pid': 41, 'ppid': 17, 'birth': 100}}

        def process(pid, expected=None):
            calls.append((pid, expected))
            return {'identity': identities[pid], 'masks': {'SigPnd': '0'}}

        trace.process = process
        record = {'pid': 41, 'run_id': 'run', 'conversation_id': 'conversation', 'pane': '%0',
                  'process_start': 'synthetic start', 'supervisor': {'pid': 17, 'start': 'synthetic parent'}}
        before = trace.capture('before_SIGTERM', record)
        self.assertEqual(before['agent'], 'kimi')
        self.assertEqual(calls, [(17, None), (41, None)])
        calls.clear()
        after = trace.capture('after_SIGTERM_returned', record, before)
        self.assertEqual(after['agent'], 'kimi')
        self.assertEqual(calls, [(17, identities[17]), (41, identities[41])])
        calls.clear()
        unknown = trace.capture('after_SIGTERM_returned', record, {'child': {'unknown': 'missing'}, 'supervisor': {'unknown': 'missing'}})
        self.assertEqual(calls, [])
        self.assertEqual(unknown['child'], {'unknown': 'no pre-signal identity'})
        self.assertEqual(unknown['supervisor'], {'unknown': 'no pre-signal identity'})

    def test_failure_reader_rejects_other_agent_rows_and_keeps_bounds_without_disk(self):
        row = {'agent': 'kimi', 'phase': 'handler_registered', 'pid': 41, 'run': 'run', 'conversation': 'conversation', 'at_ns': 12}
        raw = (json.dumps(row) + '\n' + json.dumps({**row, 'agent': 'codex'}) + '\n').encode()
        for path_kind in (PurePosixPath, PureWindowsPath):
            with self.subTest(artifact_path=path_kind.__name__):
                paths = []; reads = []; retained = []; syncs = []

                def opened(path, mode):
                    paths.append(str(path))
                    self.assertEqual(str(path), '/fixture/direct-signal-agent-kimi.jsonl')
                    self.assertEqual(mode, 'rb')
                    return MemoryFile(raw, reads)

                def temporary(**options):
                    self.assertEqual(options['prefix'], 'direct-signal-')
                    self.assertEqual(str(options['dir']), '/repo/artifacts/test-results/python')
                    return MemoryArtifact(retained)

                namespace = load(Path=path_kind, open=opened, REPO=MemoryPath('/repo'),
                                 tempfile=SimpleNamespace(NamedTemporaryFile=temporary),
                                 os=SimpleNamespace(open=lambda *args: 23, close=lambda fd: syncs.append(('close', fd)),
                                                    fsync=lambda fd: syncs.append(('sync', fd)), O_RDONLY=0, O_DIRECTORY=1, O_CLOEXEC=2))
                trace = namespace['DirectSignalTrace'](MemoryPath('/fixture'), 'kimi')
                message = trace.failure()
                expected_artifact = path_kind('artifacts/test-results/python/direct-signal-memory.json')
                self.assertIn('artifact=' + str(expected_artifact) + ', persistence=file and parent synced', message)
                self.assertEqual(paths, ['/fixture/direct-signal-agent-kimi.jsonl'])
                self.assertEqual(reads, [8705])
                saved = json.loads(retained[0])
                self.assertEqual(saved['agent'], 'kimi')
                self.assertEqual(saved['agent_phases'], [row])
                self.assertEqual(saved['malformed_or_incomplete_rows'], 1)
                self.assertEqual(syncs, [('sync', 17), ('sync', 23), ('close', 23)])


MAC_PROTECTED = {'direct_signal_agent': '0e0e76129449c8490a15cb5efadccb631f46aefe8d62b8081a8bf3364c2fd9bd', 'DirectSignalTrace': 'fb9bc39f72f835553e15c00b5744337a23fb3c6ba7d800143e87964c1a32c880', 'Lifecycle': '0c75dcf0a29b6aeacd206e972daccb084584cb9396080baba237f83beae17f87', 'Archive': 'e4bb6ea60a035bb09a2b05a33350d8c69d310b53e8742f59c851deaaf2af74a9'}
# The fake agent also records HGS_SESSION_ID (zerus-ucck P1, approved addition).
# Lifecycle gains only zerus-b64dl's plain-environment hook health/repair checks in
# test_wrapper_named_hgs_executable_keeps_one_hook_entry_per_event (25 inserted lines).
MAC_AGENT_SHA256 = '0177d1ac2bbb07117e2524e77a585b340297f2b9e2bcb6734151a1901a81a588'

# BEGIN private Mac pause observer controls. Authored separately from native gates.
MAC_NODES = [node for node in TREE.body if isinstance(node, (ast.ClassDef, ast.Assign)) and
             (getattr(node, "name", None) == "MacPauseProbe" or isinstance(node, ast.Assign) and
              any(isinstance(target, ast.Name) and target.id in
                  ("MAC_PAUSE_CASE", "MAC_PAUSE_PHASES", "MAC_PAUSE_RECEIVER") for target in node.targets))]


def load_mac(**values):
    namespace = dict(AGENT=AGENT, json=json, hashlib=hashlib, Path=PurePosixPath, re=re, **values)
    exec(compile(ast.Module(body=copy.deepcopy(MAC_NODES), type_ignores=[]), "<mac-pause-source>", "exec"), namespace)
    return namespace


def mac_probe(namespace):
    probe = namespace["MacPauseProbe"].__new__(namespace["MacPauseProbe"])
    probe.root, probe.root_id, probe.nonce = PurePosixPath("/fixture/private"), (2, 3), "a" * 32
    probe.context, probe.unknown, probe.created_at = None, None, 0
    return probe


class MacPauseObserverSource(unittest.TestCase):
    def test_exact_darwin_case_only_and_original_agent_lifecycle_linux_bodies_are_preserved(self):
        namespace = load_mac()
        selected = namespace['MacPauseProbe'].selected
        for platform in ('darwin', 'linux', 'win32'):
            for case in (namespace['MAC_PAUSE_CASE'], 'test_pause.Archive.test_pause_and_agent_signal_remain_saved',
                         '__main__.Lifecycle.test_claude_status_line_is_not_registered_where_claude_would_differ'):
                for legacy in (None, '/fixture/legacy'):
                    self.assertEqual(selected(platform, case, legacy), platform == 'darwin' and
                                     case == namespace['MAC_PAUSE_CASE'] and not legacy)
        for name, expected in MAC_PROTECTED.items():
            node = next(node for node in TREE.body if getattr(node, 'name', None) == name)
            text = ast.get_source_segment(SOURCE, node)
            self.assertEqual(hashlib.sha256(text.encode()).hexdigest(), expected, name)
        self.assertEqual(hashlib.sha256(AGENT.encode()).hexdigest(), MAC_AGENT_SHA256)
        probe = mac_probe(namespace)
        wrapped = ast.parse(probe.agent())

        class StripMac(ast.NodeTransformer):
            def visit_FunctionDef(self, node):
                return None if node.name == 'mac_pause_phase' else self.generic_visit(node)

            def visit_Import(self, node):
                return None if any(alias.asname in ('_mp_hashlib', '_mp_stat') for alias in node.names) else node

            def visit_Assign(self, node):
                names = [item.id for target in node.targets for item in ast.walk(target) if isinstance(item, ast.Name)]
                return None if names and all(name.startswith('_mp_') for name in names) else node

            def visit_Expr(self, node):
                if isinstance(node.value, ast.Call) and isinstance(node.value.func, ast.Name) and node.value.func.id == 'mac_pause_phase':
                    return None
                return self.generic_visit(node)

        self.assertEqual(ast.dump(StripMac().visit(wrapped)), ast.dump(ast.parse(AGENT)))
        harness = next(node for node in TREE.body if isinstance(node, ast.ClassDef) and node.name == 'Harness')
        hgs = next(node for node in harness.body if getattr(node, 'name', '') == 'hgs')
        text = ast.get_source_segment(SOURCE, hgs)
        self.assertEqual(text.count('subprocess.run('), 1)
        self.assertIn('timeout=20', text)
        self.assertIn('self.assertEqual(p.returncode, rc,', text)
        self.assertIn('else self.env', text)

    def test_one_original_kill_captures_errno_before_all_observer_io_and_keeps_result(self):
        rust = (Path(__file__).resolve().parents[1] / 'src/platform/unix.rs').read_text()
        request = rust.split('pub fn request_exit(pid: u32)', 1)[1].split('// BEGIN private Mac fixture observer.', 1)[0]
        self.assertEqual(request.count('libc::kill(pid, libc::SIGTERM)'), 1)
        self.assertLess(request.index('.filter(|pid| *pid > 0)'), request.index('libc::kill('))
        self.assertLess(request.index('pause_signal_observer::before'), request.index('libc::kill('))
        self.assertLess(request.index('libc::kill('), request.index('Some(io::Error::last_os_error())'))
        self.assertLess(request.index('Some(io::Error::last_os_error())'), request.index('pause_signal_observer::after'))
        self.assertIn('Some(error) => Err(error)', request)
        self.assertIn('None => Ok(())', request)
        self.assertEqual(request.count('std::panic::catch_unwind'), 2)
        before = rust.split('pub(super) fn before(pid: u32)', 1)[1].split('fn private(', 1)[0]
        self.assertIn('sender: std::process::id()', before)
        self.assertIn('at: clock()', before)
        for forbidden in ('open(', 'process_identity(', 'write(', 'read(', 'Command::', 'thread::', 'fsync', 'kill('):
            self.assertNotIn(forbidden, before)
        module = rust.split('// BEGIN private Mac fixture observer.', 1)[1].split('// END private Mac fixture observer.', 1)[0]
        self.assertIn('#[cfg(all(debug_assertions, target_os = "macos"))]', module)
        self.assertIn('#[serde(deny_unknown_fields)]', module)
        for bound in ('metadata.uid() == unsafe { libc::geteuid() }', 'metadata.mode() & 0o7777 == mode',
                      'metadata.nlink() == 1', 'libc::O_NOFOLLOW', 'libc::O_NONBLOCK', 'libc::O_EXCL',
                      'initial.len() > 1024', '.take(1025)', 'row.len() > MAX_ROW',
                      'context.target_pid != before.pid', 'context.root_ino != root.ino()',
                      '!hex(&context.nonce, 32)', 'birth_before":"unknown"',
                      'if result == 0 { None } else { errno }'):
            self.assertIn(bound, module)
        for forbidden in ('Command::', 'process_start_fallback(', 'thread::', 'sleep(', 'fsync', 'libc::kill('):
            self.assertNotIn(forbidden, module)

    @staticmethod
    def context():
        return dict(version=1, nonce='a' * 32, target_pid=41, root_dev=2, root_ino=3,
                    run='b' * 64, conversation='c' * 64, pane='d' * 64, saved_start='e' * 64)

    @classmethod
    def rows(cls, role):
        context = cls.context()
        common = dict(v=1, role=role, seq=1, phase='handler_registered' if role == 'receiver' else 'kill_before',
                      nonce=context['nonce'], pid=41 if role == 'receiver' else 17, clock='CLOCK_MONOTONIC',
                      at=100, run=context['run'], conversation=context['conversation'])
        if role == 'receiver':
            return [dict(common, ppid=17, handler='stop_callback', term_blocked=False, term_pending=True,
                         ignore_marker=False, delay_marker=False)]
        first = dict(common, target=41, pane=context['pane'], saved_start=context['saved_start'],
                     birth_before='unknown', sender_birth_after='100:2', target_birth_after=None, rc=None, errno=None)
        return [first, dict(first, seq=2, phase='kill_return_or_errno', at=101, rc=0)]

    @staticmethod
    def encoded(rows):
        return b''.join((json.dumps(row, separators=(',', ':')) + '\n').encode() for row in rows)

    def test_decoder_retains_false_true_unknown_and_refuses_partial_stale_forged_or_future_rows(self):
        decode = load_mac()['MacPauseProbe'].decode
        context = self.context()
        for role in ('sender', 'receiver'):
            rows = self.rows(role)
            self.assertEqual(decode(self.encoded(rows), role, context, 0, 1000), rows)
            variants = [b'', self.encoded(rows)[:-1], b'x' * (768 * (2 if role == 'sender' else 7) + 1),
                        self.encoded(rows).replace(b'{"v":1,', b'{"v":1,"v":1,', 1)]
            for key, value in (('v', True), ('role', 'other'), ('seq', 2), ('nonce', 'f' * 32),
                               ('pid', True), ('clock', 'actor_local'), ('at', -1), ('at', 1001),
                               ('at', True), ('run', 'f' * 64), ('conversation', 'f' * 64), ('extra', 0)):
                changed = copy.deepcopy(rows); changed[0][key] = value
                variants.append(self.encoded(changed))
            for raw in variants:
                with self.subTest(role=role, raw_size=len(raw)), self.assertRaises((ValueError, TypeError)):
                    decode(raw, role, context, 0, 1000)
        row = self.rows('receiver')[0]
        for value in (False, True, None):
            changed = dict(row, term_blocked=value, term_pending=value, at=None)
            self.assertEqual(decode(self.encoded([changed]), 'receiver', context, None, None), [changed])
        for key, value in (('handler', 'address:0x1'), ('term_blocked', 1), ('term_pending', 'false'),
                           ('ignore_marker', None), ('delay_marker', 0), ('phase', 'made_up'), ('pid', 99)):
            with self.subTest(field=key), self.assertRaises(ValueError):
                decode(self.encoded([dict(row, **{key: value})]), 'receiver', context, 0, 1000)
        for key, value in (('birth_before', '100:2'), ('target', True), ('rc', 1), ('errno', 22), ('sender_birth_after', 'ps text')):
            rows = self.rows('sender'); rows[1][key] = value
            with self.subTest(field=key), self.assertRaises(ValueError):
                decode(self.encoded(rows), 'sender', context, 0, 1000)
        rows = self.rows('sender'); rows[1].update(rc=-1, errno=3)
        self.assertEqual(decode(self.encoded(rows), 'sender', context, 0, 1000), rows)
        with self.assertRaises(ValueError):
            decode(self.encoded(self.rows('sender')[:1]), 'sender', context, 0, 1000)
        with self.assertRaises(ValueError):
            decode(self.encoded([row, dict(row, seq=2)]), 'receiver', context, 0, 1000)

    def test_invalid_or_repeated_context_leaves_original_pause_environment_and_never_writes(self):
        def forbidden(*args, **kwargs):
            raise AssertionError('memory-only invalid context must not open anything')
        namespace = load_mac(os=SimpleNamespace(open=forbidden))
        valid = dict(name='claude/p/own', pid=41, run_id='run', conversation_id='conversation', pane='%1', process_start='start')
        env = {'fixture': 'unchanged'}
        for field, value in (('name', 'claude/p/other'), ('pid', False), ('pid', 0), ('run_id', None),
                             ('conversation_id', ''), ('pane', 'x\n'), ('process_start', 'x' * 257)):
            probe = mac_probe(namespace)
            record = dict(valid, **{field: value})
            self.assertIs(probe.environment(('pause', 'claude/p/own'), env, lambda name: record), env)
            self.assertIsNotNone(probe.unknown)
        probe = mac_probe(namespace); probe.context = self.context()
        self.assertIs(probe.environment(('pause', 'claude/p/own'), env, lambda name: valid), env)
        self.assertEqual(probe.unknown, 'ValueError')
        for args in (('pause', 'claude/p/other'), ('resume', 'claude/p/own'), ('pause', 'claude/p/own', '--force')):
            probe = mac_probe(namespace)
            self.assertIs(probe.environment(args, env, forbidden), env)
            self.assertIsNone(probe.unknown)

    def test_receiver_faults_cannot_replace_original_stop_event_or_exit143(self):
        import stat
        namespace = load_mac()
        probe = mac_probe(namespace)
        tree = ast.parse(probe.agent())
        functions = [node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name in ('mac_pause_phase', 'stop')]
        original_stop = next(node for node in ast.parse(AGENT).body if isinstance(node, ast.FunctionDef) and node.name == 'stop')
        self.assertEqual(len(functions), 2)

        class MarkerPath(PurePosixPath):
            def exists(self):
                return False

        for fault in (None, 'clock', 'getpid', 'open', 'fstat', 'wrong_owner', 'wrong_root', 'wrong_mode',
                      'linked', 'wrong_type', 'full', 'short', 'write', 'close'):
            calls, events, writes = [], [], []
            def operation(name, result=None):
                def invoke(*args, **kwargs):
                    calls.append(name)
                    if fault == name:
                        raise OSError('in-memory observer fault')
                    if name == 'write':
                        writes.append(args[1]); return len(args[1]) - (fault == 'short')
                    if name == 'open':
                        return 19 if len(args) == 2 else 20
                    if name == 'fstat':
                        directory = args[0] == 19
                        mode = (stat.S_IFDIR | 0o700) if directory else (stat.S_IFREG | 0o600)
                        if fault == 'wrong_mode': mode |= 0o004
                        if fault == 'wrong_type' and not directory: mode = stat.S_IFIFO | 0o600
                        return SimpleNamespace(st_dev=2, st_ino=99 if fault == 'wrong_root' else 3 if directory else 4,
                            st_uid=99 if fault == 'wrong_owner' else 7, st_mode=mode,
                            st_nlink=2 if fault == 'linked' else 1, st_size=6000 if fault == 'full' else 0)
                    return result
                return invoke
            memory_os = SimpleNamespace(environ={'HGS_SESSION': 'claude/p/own'}, getpid=operation('getpid', 41),
                getppid=lambda: 17, geteuid=lambda: 7, open=operation('open'), fstat=operation('fstat'),
                write=operation('write'), close=operation('close'), O_RDONLY=0, O_DIRECTORY=1, O_NOFOLLOW=2,
                O_CLOEXEC=4, O_NONBLOCK=8, O_WRONLY=16, O_CREAT=32, O_EXCL=64, O_APPEND=128)
            memory_signal = SimpleNamespace(SIGTERM=15, SIG_BLOCK=0, SIG_IGN=1, SIG_DFL=0,
                getsignal=lambda signal: state['stop'], pthread_sigmask=lambda how, mask: [], sigpending=lambda: set())
            state = dict(os=memory_os, signal=memory_signal, time=SimpleNamespace(CLOCK_MONOTONIC=6,
                         clock_gettime_ns=operation('clock', 100)), json=json, _mp_stat=stat,
                         _mp_root=MarkerPath('/fixture/private'), _mp_root_id=(2, 3), _mp_nonce='a' * 32,
                         _mp_file_id=None, _mp_seq=0, _mp_busy=False, _mp_off=False,
                         _mp_run='b' * 64, _mp_conversation='c' * 64, home=MarkerPath('/fixture'), sid='conversation',
                         agent='claude', event=lambda *args, **kwargs: events.append((args, kwargs)),
                         sys=SimpleNamespace(exit=lambda code: (_ for _ in ()).throw(SystemExit(code))))
            exec(compile(ast.Module(body=copy.deepcopy(functions), type_ignores=[]), '<mac-phase-memory>', 'exec'), state)
            with self.subTest(fault=fault), self.assertRaises(SystemExit) as exited:
                state['stop']()
            self.assertEqual(exited.exception.code, 143)
            self.assertEqual(events, [(('SessionEnd',), {'reason': 'other'})])
            self.assertNotIn('kill', calls)
            self.assertLessEqual(len(writes), 4)
            self.assertTrue(all(len(data) <= 768 for data in writes))
            self.assertNotIn('fsync', calls)
            self.assertNotIn('sleep', calls)
            if fault is None:
                rows = [json.loads(data) for data in writes]
                self.assertEqual([row['phase'] for row in rows],
                                 ['term_handler_enter', 'SessionEnd_begin', 'SessionEnd_returned', 'handler_exit143'])
                self.assertEqual([row['seq'] for row in rows], [1, 2, 3, 4])
            elif fault == 'short':
                self.assertEqual(len(writes), 1)  # Never repair or retry a partial append.
                self.assertTrue(state['_mp_off'])
        # The removed observer calls leave exactly the old handler body.
        wrapped_stop = next(node for node in functions if node.name == 'stop')
        stripped = copy.deepcopy(wrapped_stop)
        stripped.body = [node for node in stripped.body if not isinstance(node, ast.Expr) or
                         not isinstance(node.value, ast.Call) or not isinstance(node.value.func, ast.Name) or
                         node.value.func.id != 'mac_pause_phase']
        self.assertEqual(ast.dump(stripped), ast.dump(original_stop))

    def test_receiver_has_no_added_signal_poll_thread_fsync_or_mask_mutation(self):
        namespace = load_mac()
        function = next(node for node in ast.parse(mac_probe(namespace).agent()).body
                        if isinstance(node, ast.FunctionDef) and node.name == 'mac_pause_phase')
        text = ast.unparse(function)
        self.assertEqual(text.count('os.write('), 1)
        self.assertIn('signal.pthread_sigmask(signal.SIG_BLOCK, [])', text)
        for forbidden in ('SIG_SETMASK', 'SIG_UNBLOCK', 'signal.signal(', 'os.kill(', 'fsync(', 'Thread(', 'sleep(',
                          'Popen(', 'subprocess.', 'while ', 'repr('):
            self.assertNotIn(forbidden, text)
        self.assertIn('_mp_seq >= 7', text)
        self.assertIn('len(data) > 768', text)
        self.assertIn('os.O_CREAT | os.O_EXCL', text)
        self.assertIn('os.O_NOFOLLOW', text)

    def test_failure_collector_error_stays_unknown_with_original_deadline_and_assertion(self):
        probe = mac_probe(load_mac())
        self.assertEqual(probe.failure(100), 'Mac pause diagnostic UNKNOWN: ValueError')
        harness = next(node for node in TREE.body if isinstance(node, ast.ClassDef) and node.name == 'Harness')
        report = next(node for node in harness.body if getattr(node, 'name', '') == 'stop_report')
        text = ast.get_source_segment(SOURCE, report)
        self.assertEqual(text.count('time.monotonic() + STOP_REPORT_SECONDS'), 1)
        self.assertIn('probe.failure(deadline)', text)
        self.assertIn('if len(report) > STOP_REPORT_CHARS:', text)
        self.assertIn('step("Mac pause trace"', text)
        failure = next(node for node in MAC_NODES if isinstance(node, ast.ClassDef))
        failure = next(node for node in failure.body if getattr(node, 'name', '') == 'failure')
        text = ast.get_source_segment(SOURCE, failure)
        self.assertIn('limit + 1', text)
        self.assertIn('len(encoded) > 8192', text)
        self.assertIn('len(rows) > 9', text)
        self.assertIn('during_stall_thread_state', text)
        self.assertNotIn('subprocess.', text)
# END private Mac pause observer controls.


if __name__ == '__main__':
    unittest.main()
