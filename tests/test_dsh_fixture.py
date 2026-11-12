"""Latest official DeepSeek selection, frozen sources and mandatory admission.

All install/version subprocesses and registry responses are stand-ins. These
controls execute no npm package, native agent, model or installed application.
"""
import base64
from contextlib import contextmanager
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch
import urllib.error

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('dsh_fixture', ROOT / 'scripts/ci/dsh_fixture.py')
F = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(F)


@contextmanager
def private_root(**kwargs):
    # macOS TemporaryDirectory may use /var, whose canonical name is /private/var.
    with tempfile.TemporaryDirectory(**kwargs) as root:
        yield Path(root).resolve()


@unittest.skipUnless(hasattr(os, 'O_DIRECTORY'), 'Unix private-directory receipt controls')
class LatestFixture(unittest.TestCase):
    def setUp(self):
        self.seal = 'sha512-' + base64.b64encode(bytes(range(64))).decode()
        self.version = '0.4.0-rc.1'
        self.url = F.REGISTRY + '/@deepseek-ai/dsh/-/dsh-' + self.version + '.tgz'
        self.metadata = dict(name=F.PACKAGE, version=self.version, bin=dict(dsh='lib/bin.js'),
                             dist=dict(tarball=self.url, integrity=self.seal))
        self.raw = json.dumps(self.metadata).encode()
        self.chosen = F.selection(self.raw)
        self.lock = dict(lockfileVersion=3, packages={
            '': dict(dependencies={F.PACKAGE: self.version}),
            'node_modules/' + F.PACKAGE: dict(version=self.version, resolved=self.url, integrity=self.seal),
            'node_modules/example': dict(version='1.2.3', resolved=F.REGISTRY + '/example/-/example-1.2.3.tgz',
                                        integrity=self.seal),
        })

    def test_latest_resolution_has_no_old_version_fallback_and_checks_exact_identity(self):
        self.assertEqual(self.chosen['version'], self.version)
        self.assertNotEqual(self.version, '0.2.1-rc.2')
        for change in [dict(name='another'), dict(version='latest'), dict(version='../bad'),
                       dict(bin={'dsh': '../outside'}), dict(bin={'dsh': '/absolute'})]:
            with self.subTest(change=change), self.assertRaises((ValueError, TypeError)):
                F.selection(json.dumps({**self.metadata, **change}).encode())
        for dist in [dict(tarball='https://example.invalid/dsh.tgz', integrity=self.seal),
                     dict(tarball=self.url, integrity='sha512-short'),
                     dict(tarball=self.url.replace('https:', 'http:'), integrity=self.seal),
                     dict(tarball=self.url + '?other', integrity=self.seal)]:
            with self.subTest(dist=dist), self.assertRaises(ValueError):
                F.selection(json.dumps({**self.metadata, 'dist': dist}).encode())
        for spec in ('git+ssh://example.invalid/x', 'file:../x', '../x', 'https://example.invalid/x.tgz', 'workspace:*'):
            with self.subTest(spec=spec), self.assertRaisesRegex(ValueError, 'registry version'):
                F.selection(json.dumps({**self.metadata, 'dependencies': {'example': spec}}).encode())

    def test_official_metadata_fetch_is_single_bounded_and_redirects_refuse(self):
        response = io.BytesIO(self.raw)
        with patch.object(F.urllib.request, 'build_opener') as opener:
            opener.return_value.open.return_value = response
            self.assertEqual(F.discover(time.monotonic() + 30), self.raw)
            request = opener.return_value.open.call_args.args[0]
            self.assertEqual(request.full_url, F.LATEST)
            self.assertGreater(opener.return_value.open.call_args.kwargs['timeout'], 29)
            self.assertLessEqual(opener.return_value.open.call_args.kwargs['timeout'], 30)
            opener.return_value.open.assert_called_once()
        with patch.object(F.urllib.request, 'build_opener') as opener:
            opener.return_value.open.return_value = io.BytesIO(b'x' * (F.MAX_METADATA + 1))
            with self.assertRaisesRegex(ValueError, 'metadata exceeds'):
                F.discover(time.monotonic() + 30)
        with self.assertRaisesRegex(ValueError, 'must not redirect'):
            F.NoRedirect().redirect_request(None, None, 302, '', {}, 'https://example.invalid/x')

    def test_transitive_lock_preserves_exact_latest_and_all_official_sha512_sources(self):
        self.assertEqual(F.validate_lock(json.dumps(self.lock).encode(), self.chosen), 2)
        for key, value in [('version', '0.2.1-rc.2'), ('resolved', 'https://example.invalid/x.tgz'),
                           ('integrity', 'sha512-short'), ('link', True)]:
            lock = copy.deepcopy(self.lock)
            lock['packages']['node_modules/' + F.PACKAGE][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                F.validate_lock(json.dumps(lock).encode(), self.chosen)
        for path in ['../escape', 'node_modules/../escape', 'node_modules/example']:
            lock = copy.deepcopy(self.lock)
            lock['packages'][path] = dict(version='1.2.3', resolved='file:../outside', integrity=self.seal)
            with self.subTest(path=path), self.assertRaises(ValueError):
                F.validate_lock(json.dumps(lock).encode(), self.chosen)
        with self.assertRaises(ValueError):
            F.validate_lock(b' ' * (F.MAX_LOCK + 1), self.chosen)

    def test_security_overrides_are_required_and_vulnerable_pins_are_rejected(self):
        for package, fixed in F.SECURITY_OVERRIDES.items():
            path = 'node_modules/' + package
            self.lock['packages'][path] = dict(version=fixed, resolved='https://example.invalid/x.tgz', integrity=self.seal)
        F.validate_security_pins(json.dumps(self.lock).encode())
        for package, vulnerable in [('@modelcontextprotocol/client', '2.2.0'), ('fflate', '0.8.2')]:
            lock = copy.deepcopy(self.lock)
            lock['packages']['node_modules/' + package]['version'] = vulnerable
            with self.subTest(package=package), self.assertRaisesRegex(ValueError, 'security'):
                F.validate_security_pins(json.dumps(lock).encode())

    def fake_npm(self, command, **kwargs):
        prefix = Path(command[command.index('--prefix') + 1])
        evidence = self.evidence
        self.commands.append(command)
        self.environments.append(kwargs['env'])
        self.assertEqual(kwargs['stdin'], subprocess.DEVNULL)
        self.assertEqual(kwargs['cleanup_end'] - kwargs['work_end'], F.CLEANUP_SECONDS)
        self.assertEqual(command[0], '/fixture/npm')
        self.assertIn('--registry=' + F.REGISTRY, command)
        if command[1] == '--version':
            self.assertFalse(any(flag in command for flag in F.SOURCE_FLAGS))
            kwargs['stdout'].write(b'11.19.0\n')
            return type('Completed', (), {'returncode': 0})()
        if command[1] == 'config':
            denied = any(flag in command for flag in F.SOURCE_FLAGS)
            if denied:
                self.assertTrue(all(flag in command for flag in F.SOURCE_FLAGS))
            kwargs['stdout'].write(json.dumps(dict.fromkeys(F.SOURCE_KEYS, 'none' if denied else 'all')).encode())
            return type('Completed', (), {'returncode': 0})()
        self.assertTrue(all(flag in command for flag in F.SOURCE_FLAGS))
        if command[1] == 'install':
            self.assertIn('--ignore-scripts', command)
            self.assertIn('--package-lock-only', command)
            self.assertFalse((evidence / 'selection.json').exists())
            (prefix / 'package-lock.json').write_text(json.dumps(self.lock))
        else:
            self.assertEqual(command[1], 'ci')
            self.assertNotIn('--ignore-scripts', command)  # Existing lifecycle policy.
            selection = json.loads((evidence / 'selection.json').read_text())
            policy = json.loads((evidence / 'npm-policy.json').read_text())
            self.assertEqual(policy['version'], '11.19.0')
            self.assertEqual(policy['denied_sources'], list(F.SOURCE_FLAGS))
            self.assertEqual(selection['package']['version'], self.version)
            self.assertEqual(json.loads((evidence / 'package-lock.json').read_text()), self.lock)
            self.assertTrue((evidence / 'latest.json').is_file())
            package = prefix / 'node_modules' / F.PACKAGE
            (package / 'lib').mkdir(parents=True)
            (package / 'package.json').write_text(json.dumps(self.metadata))
            (package / 'lib/bin.js').write_text('synthetic fixture entry, never executed\n')
            (prefix / 'node_modules/.bin').mkdir()
            (prefix / 'node_modules/.bin/dsh').symlink_to(package / 'lib/bin.js')
        return type('Completed', (), {'returncode': 0})()

    def fixture(self, root):
        self.prefix, self.evidence = Path(root) / 'prefix', Path(root) / 'evidence'
        self.commands, self.environments = [], []
        with patch.object(F.shutil, 'which', return_value='/fixture/npm'), patch.object(F, 'discover', return_value=self.raw) as discover, \
                patch.object(F, 'owned_command', side_effect=self.fake_npm):
            receipt = F.install(self.prefix, self.evidence)
        discover.assert_called_once()
        return receipt

    def test_one_resolution_is_sealed_before_install_and_runtime_identity_is_bound(self):
        with private_root() as root:
            receipt = self.fixture(root)
            self.assertEqual([c[1] for c in self.commands], ['--version', 'config', 'config', 'install', 'ci'])
            manifest = json.loads((self.prefix / 'package.json').read_text())
            self.assertEqual(manifest['overrides'], F.SECURITY_OVERRIDES)
            executable = self.prefix / 'node_modules/.bin/dsh'
            self.assertEqual(F.verify_install(receipt, executable), self.version)
            self.assertEqual(F.package_root(executable, {F.RECEIPT_ENV: str(receipt)}), self.prefix / 'node_modules' / F.PACKAGE)
            with self.assertRaisesRegex(ValueError, 'different DeepSeek executable'):
                F.verify_install(receipt, Path(root) / 'other')
            entry = executable.resolve()
            entry.write_text('changed')
            with self.assertRaisesRegex(ValueError, 'bytes changed'):
                F.verify_install(receipt, executable)

    def test_temporary_root_alias_is_resolved_without_weakening_prefix_admission(self):
        with private_root() as parent:
            target, alias = parent / 'actual', parent / 'alias'
            target.mkdir()
            alias.symlink_to(target, target_is_directory=True)
            with private_root(dir=alias) as root:
                self.assertEqual(root.parent, target)
                self.assertEqual(root, root.resolve())
                self.assertEqual((alias / root.name).resolve(), root)
                with patch.object(F.shutil, 'which') as npm, \
                        self.assertRaisesRegex(ValueError, 'Canonical private fixture prefix'):
                    F.install(alias / root.name / 'prefix', root / 'evidence')
                npm.assert_not_called()
                self.assertEqual(list(root.iterdir()), [])
                receipt = self.fixture(root)
                self.assertEqual(F.verify_install(receipt, self.prefix / 'node_modules/.bin/dsh'), self.version)

    def test_latest_metadata_lock_and_receipt_tampering_all_refuse_before_version_execution(self):
        for target in ('latest.json', 'package-lock.json', 'selection.json', 'installed.json', 'npm-policy.json', 'npm-config-denied.log'):
            with self.subTest(target=target), private_root() as root:
                receipt = self.fixture(root)
                path = self.evidence / target
                path.write_bytes(path.read_bytes() + b' ')
                # JSON whitespace is semantic equality but must still fail byte seals.
                if target == 'installed.json':
                    path.write_text(json.dumps(dict(schema=1, status='installing')))
                with self.assertRaises(ValueError), patch.object(F.subprocess, 'check_output') as probe:
                    F.verify_install(receipt, self.prefix / 'node_modules/.bin/dsh')
                probe.assert_not_called()

    def test_discovery_failure_stops_without_install_or_historical_lock(self):
        with private_root() as root:
            evidence = Path(root) / 'evidence'
            with patch.object(F.shutil, 'which', return_value='/fixture/npm'), \
                    patch.object(F, 'discover', side_effect=urllib.error.URLError('offline')) as discover, \
                    patch.object(F, 'npm_policy'), patch.object(F, 'owned_command') as npm, self.assertRaises(urllib.error.URLError):
                F.install(Path(root) / 'prefix', evidence)
            discover.assert_called_once()
            npm.assert_not_called()
            self.assertFalse((evidence / 'installed.json').exists())

    def test_install_failure_retains_selection_but_never_admits_native_fixture(self):
        with private_root() as root:
            self.prefix, self.evidence = Path(root) / 'prefix', Path(root) / 'evidence'
            self.commands, self.environments = [], []
            def fail(command, **kwargs):
                result = self.fake_npm(command, **kwargs)
                result.returncode = 1 if command[1] == 'ci' else 0
                return result
            with patch.object(F.shutil, 'which', return_value='/fixture/npm'), patch.object(F, 'discover', return_value=self.raw), \
                    patch.object(F, 'owned_command', side_effect=fail), self.assertRaisesRegex(ValueError, 'no fallback'):
                F.install(self.prefix, self.evidence)
            self.assertTrue((self.evidence / 'selection.json').is_file())
            self.assertFalse((self.evidence / 'installed.json').exists())

    def test_nonregistry_or_mismatched_resolution_never_reaches_install(self):
        for change in ('foreign', 'old'):
            with self.subTest(change=change), private_root() as root:
                self.prefix, self.evidence = Path(root) / 'prefix', Path(root) / 'evidence'
                self.commands, self.environments = [], []
                lock = copy.deepcopy(self.lock)
                if change == 'foreign':
                    lock['packages']['node_modules/example']['resolved'] = 'https://example.invalid/x.tgz'
                else:
                    lock['packages']['node_modules/' + F.PACKAGE]['version'] = '0.2.1-rc.2'
                def resolve(command, **kwargs):
                    result = self.fake_npm(command, **kwargs)
                    (self.prefix / 'package-lock.json').write_text(json.dumps(lock))
                    return result
                with patch.object(F.shutil, 'which', return_value='/fixture/npm'), patch.object(F, 'discover', return_value=self.raw), \
                        patch.object(F, 'owned_command', side_effect=resolve), self.assertRaises(ValueError):
                    F.install(self.prefix, self.evidence)
                self.assertEqual([c[1] for c in self.commands], ['--version', 'config', 'config', 'install'])
                self.assertFalse((self.evidence / 'selection.json').exists())
                self.assertFalse((self.evidence / 'installed.json').exists())

    def test_original_deadline_failure_never_admits_partial_install(self):
        with private_root() as root:
            self.prefix, self.evidence = Path(root) / 'prefix', Path(root) / 'evidence'
            with patch.object(F.shutil, 'which', return_value='/fixture/npm'), patch.object(F, 'discover', return_value=self.raw), \
                    patch.object(F, 'owned_command', side_effect=subprocess.TimeoutExpired('npm', 600)) as npm:
                with self.assertRaises(subprocess.TimeoutExpired):
                    F.install(self.prefix, self.evidence)
            npm.assert_called_once()
            self.assertFalse((self.evidence / 'installed.json').exists())

    def test_unsupported_npm_or_unknown_source_policy_refuses_before_discovery(self):
        for version, missing, ignored in [('11.15.9', False, False), ('12.0.0', False, False),
                                          ('11.19.0', True, False), ('11.19.0', False, True)]:
            with self.subTest(version=version, missing=missing, ignored=ignored), private_root() as root:
                self.prefix, self.evidence = Path(root) / 'prefix', Path(root) / 'evidence'
                self.commands, self.environments = [], []
                def probe(command, **kwargs):
                    self.commands.append(command)
                    if command[1] == '--version':
                        data = (version + '\n').encode()
                    else:
                        denied = any(flag in command for flag in F.SOURCE_FLAGS)
                        values = dict.fromkeys(F.SOURCE_KEYS, 'none' if denied and not ignored else 'all')
                        if missing:
                            values.pop('allow-git')
                        data = json.dumps(values).encode()
                    kwargs['stdout'].write(data)
                    return type('Completed', (), {'returncode': 0})()
                with patch.object(F.shutil, 'which', return_value='/fixture/npm'), patch.object(F, 'discover') as discover, \
                        patch.object(F, 'owned_command', side_effect=probe), self.assertRaises(ValueError):
                    F.install(self.prefix, self.evidence)
                discover.assert_not_called()
                self.assertFalse(any(command[1] in ('install', 'ci') for command in self.commands))
                self.assertFalse((self.evidence / 'selection.json').exists())

    def test_current_historical_lock_is_only_a_pure_registry_scope_control(self):
        raw = (ROOT / 'scripts/ci/dsh-fixture/package-lock.json').read_bytes()
        package = json.loads(raw)['packages']['node_modules/' + F.PACKAGE]
        chosen = dict(version=package['version'], url=package['resolved'], integrity=package['integrity'])
        self.assertGreater(F.validate_lock(raw, chosen), 100)

    def test_discovery_and_file_reads_reject_late_chunks_and_close_before_refusal(self):
        clock = [0.0]
        class Late(io.BytesIO):
            def read(self, size=-1):
                value = super().read(size)
                clock[0] = 6.0
                return value
        response = Late(self.raw)
        with patch.object(F.time, 'monotonic', side_effect=lambda: clock[0]), \
                patch.object(F.urllib.request, 'build_opener') as opener:
            opener.return_value.open.return_value = response
            with self.assertRaisesRegex(ValueError, 'deadline expired'):
                F.discover(5.0)
            self.assertEqual(opener.return_value.open.call_args.kwargs['timeout'], 5.0)
        self.assertTrue(response.closed)
        clock[0] = 0.0
        source = Late(b'fixture')
        with patch.object(F.time, 'monotonic', side_effect=lambda: clock[0]), patch.object(Path, 'open', return_value=source):
            with self.assertRaisesRegex(ValueError, 'deadline expired'):
                F.bounded(Path('memory'), 20, 5.0)
        self.assertTrue(source.closed)

    def test_late_pending_publication_fsync_or_final_close_cannot_leave_installed_receipt(self):
        fsync, close = F.os.fsync, F.os.close
        for stage in ('pending', 'published', 'closed'):
            with self.subTest(stage=stage), private_root() as root:
                self.prefix, self.evidence = Path(root) / 'prefix', Path(root) / 'evidence'
                self.commands, self.environments = [], []
                clock = [0.0]
                def target_path(descriptor):
                    info = os.fstat(descriptor)
                    for path in (self.evidence / 'installed-pending.json', self.evidence):
                        if path.exists() and (path.stat().st_dev, path.stat().st_ino) == (info.st_dev, info.st_ino):
                            return path
                    return None
                def late(descriptor):
                    fsync(descriptor)
                    target = target_path(descriptor)
                    if stage == 'pending' and target == self.evidence / 'installed-pending.json' or \
                            stage == 'published' and target == self.evidence and (self.evidence / 'installed.json').exists():
                        clock[0] = 986.0
                def late_close(descriptor):
                    target = target_path(descriptor)
                    close(descriptor)
                    if stage == 'closed' and target == self.evidence and (self.evidence / 'installed.json').exists():
                        clock[0] = 986.0
                with patch.object(F.shutil, 'which', return_value='/fixture/npm'), patch.object(F, 'discover', return_value=self.raw), \
                        patch.object(F, 'owned_command', side_effect=self.fake_npm), \
                        patch.object(F.time, 'monotonic', side_effect=lambda: clock[0]), patch.object(F.os, 'fsync', side_effect=late), \
                        patch.object(F.os, 'close', side_effect=late_close):
                    with self.assertRaisesRegex(ValueError, 'deadline expired'):
                        F.install(self.prefix, self.evidence, end=1000.0)
                self.assertFalse((self.evidence / 'installed.json').exists())

    @unittest.skipUnless(sys.platform == 'linux' and hasattr(os, 'WNOWAIT'), 'Linux owned-group custody')
    def test_owned_npm_descendant_timeout_cannot_make_a_delayed_installed_receipt(self):
        with private_root() as root:
            root = Path(root)
            receipt, ready = root / 'installed.json', root / 'ready'
            child = 'import pathlib,time;time.sleep(1.5);pathlib.Path(' + repr(str(receipt)) + ').write_text("late")'
            script = ('import pathlib,subprocess,sys,time;'
                      'subprocess.Popen([sys.executable,"-c",' + repr(child) + ']);'
                      'pathlib.Path(' + repr(str(ready)) + ').write_text("started");time.sleep(30)')
            with (root / 'log').open('wb') as log:
                started = time.monotonic()
                with self.assertRaisesRegex(ValueError, 'deadline expired'):
                    F.owned_command([sys.executable, '-c', script], cwd=root, env={'PATH': os.environ['PATH']},
                                    stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                    work_end=started + .8, cleanup_end=started + 5)
            self.assertTrue(ready.exists(), 'Synthetic npm must actually spawn its descendant')
            time.sleep(1.6)
            self.assertFalse(receipt.exists(), 'An owned descendant must not keep writing after refusal')

    @unittest.skipUnless(sys.platform == 'linux' and hasattr(os, 'WNOWAIT'), 'Linux owned-group custody')
    def test_owned_npm_reaps_only_after_group_cleanup_and_never_signals_after_reap(self):
        popen, killpg = F.subprocess.Popen, F.os.killpg
        events = []
        def spawn(*args, **kwargs):
            proc = popen(*args, **kwargs)
            wait = proc.wait
            def reap(*args, **kwargs):
                events.append('reap')
                return wait(*args, **kwargs)
            proc.wait = reap
            return proc
        def signal(group, value):
            self.assertNotIn('reap', events)
            events.append('signal')
            return killpg(group, value)
        with private_root() as root, open(os.devnull, 'wb') as log:
            started = time.monotonic()
            with patch.object(F.subprocess, 'Popen', side_effect=spawn), patch.object(F.os, 'killpg', side_effect=signal):
                result = F.owned_command([sys.executable, '-c', 'pass'], cwd=root, env={'PATH': os.environ['PATH']},
                                         stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                         work_end=started + 3, cleanup_end=started + 5)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(events[-1], 'reap')
        self.assertIn('signal', events)

    @unittest.skipUnless(sys.platform == 'linux' and hasattr(os, 'WNOWAIT'), 'Linux owned-group custody')
    def test_unknown_or_reaped_leader_never_authorizes_group_signal_or_success(self):
        proc = type('Process', (), {'pid': 654321, 'wait': lambda *a, **kw: self.fail('Unknown leader must not be waited on')})()
        with patch.object(F.subprocess, 'Popen', return_value=proc), patch.object(F, 'process_stat', return_value=('Z', proc.pid, proc.pid, 123)), \
                patch.object(F.os, 'waitid', side_effect=ChildProcessError('already reaped')), patch.object(F.os, 'killpg') as signal:
            started = time.monotonic()
            with self.assertRaisesRegex(ValueError, 'cleanup is unknown'):
                F.owned_command(['synthetic'], cwd='.', env={}, stdin=subprocess.DEVNULL, stdout=None, stderr=None,
                                work_end=started + 1, cleanup_end=started + 3)
            signal.assert_not_called()

    def test_prefix_is_fresh_and_npm_has_private_configuration_without_credentials(self):
        with private_root() as root, patch.dict(os.environ, {'NPM_TOKEN': 'synthetic-secret',
                'DEEPSEEK_API_KEY': 'synthetic-secret', 'npm_config_registry': 'https://example.invalid'}):
            self.fixture(root)
            for env in self.environments:
                self.assertNotIn('NPM_TOKEN', env)
                self.assertNotIn('DEEPSEEK_API_KEY', env)
                self.assertEqual(env['npm_config_registry'], F.REGISTRY)
                self.assertEqual(Path(env['HOME']).parent, self.prefix / '.private')
                self.assertEqual(Path(env['npm_config_userconfig']).read_bytes(), b'')
            with patch.object(F, 'discover') as discover, self.assertRaisesRegex(ValueError, 'Fresh'):
                F.install(self.prefix, self.evidence)
            discover.assert_not_called()

    def test_mandatory_ci_missing_or_wrong_version_is_an_error_not_a_skip(self):
        with private_root() as root:
            receipt = self.fixture(root)
            hgs = Path(root) / 'hgs'; hgs.write_text('synthetic CLI')
            executable = str(self.prefix / 'node_modules/.bin/dsh')
            env = {F.RECEIPT_ENV: str(receipt)}
            with patch.object(F.subprocess, 'check_output', return_value=self.version + '\n') as probe:
                self.assertEqual(F.admit_fixture(executable, hgs, env), self.version)
                self.assertEqual(probe.call_args.kwargs['env']['HOME'], str(self.prefix / '.private/home'))
                self.assertEqual(probe.call_args.kwargs['env']['DSH_TELEMETRY_DISABLED'], '1')
                self.assertNotIn(F.RECEIPT_ENV, probe.call_args.kwargs['env'])
            with patch.object(F.subprocess, 'check_output', return_value='0.2.1-rc.2\n'), self.assertRaisesRegex(ValueError, 'latest DeepSeek version'):
                F.admit_fixture(executable, hgs, env)
            with patch.object(F.sys, 'platform', 'linux'), patch.object(F.subprocess, 'check_output') as probe:
                with self.assertRaisesRegex(ValueError, 'receipt'):
                    F.admit_fixture(executable, hgs, {'GITHUB_ACTIONS': 'true'})
                with self.assertRaisesRegex(ValueError, 'Official dsh'):
                    F.admit_fixture(None, hgs, {'GITHUB_ACTIONS': 'true'})
                probe.assert_not_called()


if __name__ == '__main__':
    unittest.main()
