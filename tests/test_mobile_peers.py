#!/usr/bin/env python3
"""Direct-peer native boundary, using isolated homes and a synthetic SSH transport."""
import asyncio
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
import uuid
from unittest.mock import patch

REPO = Path(__file__).resolve().parents[1]
HGS = Path(os.environ.get('HGS_TEST_BIN', REPO / 'target/debug/hgs')).resolve()
SSH = r'''#!/usr/bin/env python3
import json, os, pathlib, shlex, stat, subprocess, sys, time
if sys.argv[-1].startswith('echo "hgs-remote-v2|'): print('hgs-remote-v2|%OS%|%SystemRoot%|:OS|:SystemRoot'); sys.exit(0)
root = pathlib.Path(os.environ['MOBILE_PEERS_TEST_ROOT'])
alias = sys.argv[-2]
args = shlex.split(sys.argv[-1])[1:]
with (root / 'calls').open('a') as file:
    file.write(json.dumps([alias, args]) + '\n')
if (root / ('offline-' + alias)).exists():
    print('synthetic private error', file=sys.stderr); sys.exit(255)
if (root / ('old-' + alias)).exists() or (root / ('flip-' + alias)).exists():
    if args[0] == '--help':
        if (root / ('flip-' + alias)).exists():
            print('mobile-peer ABI 1'); sys.exit(0)
        print('usage: hgs [@host] <cmd>'); sys.exit(0)
    if args[0] == 'swarm':
        print('unknown swarm command', file=sys.stderr); sys.exit(1)
    (root / 'old-launch').touch(); sys.exit(1)
if (root / ('delay-' + alias)).exists() and args[:2] == ['swarm', '__mobile-peer-identity']:
    time.sleep(4)
if (root / 'discovery-hang').exists() and args[:2] == ['swarm', '__mobile-peer-identity']:
    # Pipes beyond stdio would be another transport's ends.
    inherited = []
    for name in os.listdir('/dev/fd'):
        try:
            if int(name) > 2 and stat.S_ISFIFO(os.fstat(int(name)).st_mode):
                inherited.append(int(name))
        except OSError:
            pass  # The listing's own descriptor is closed by now.
    # The test reads the file as soon as it exists: publish it whole.
    staged = root / ('discovery-pids-' + alias + '.tmp')
    staged.write_text(json.dumps(dict(pids=[os.getpid(), os.getppid()], inherited=inherited)))
    staged.replace(root / ('discovery-pids-' + alias))
    time.sleep(30); sys.exit(0)
if args[:2] == ['swarm', '__mobile-peer-local']:
    if (root / 'hang').exists():
        if (root / 'ssh-child').exists():
            child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(2)'],
                                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            (root / 'ssh-child-pid').write_text(str(child.pid))
        (root / 'hanging-pids').write_text(json.dumps([os.getpid(), os.getppid()]))
        time.sleep(30); sys.exit(0)
    if (root / 'repository-probe').exists():
        print('transport accepted'); sys.stderr.write('fixture'); sys.exit(7)
    if (root / 'binary').exists():
        sys.stdout.buffer.write(bytes([0, 255, 128, 10, 13, 42]))
        sys.stderr.write('fixture'); sys.exit(7)
    if (root / 'stdout-overflow').exists():
        sys.stdout.buffer.write(b'x' * (33 * 1024 * 1024)); sys.exit(0)
    if (root / 'stderr-overflow').exists():
        sys.stderr.write('x' * 70000); sys.exit(0)
    if (root / 'inherited-pipe').exists():
        subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(31)'])
        sys.exit(0)
env = dict(os.environ, HGS_CONFIG_DIR=str(root / alias), HGS_STATE_DIR=str(root / alias / 'state'))
os.execve(os.environ['MOBILE_PEERS_TEST_HGS'], [os.environ['MOBILE_PEERS_TEST_HGS'], *args], env)
'''
# Keep all tmux interactions synthetic, including snapshots that intentionally
# reach the native CLI. No fixture can address a development tmux server.
TMUX = '''#!/usr/bin/env python3
import os, pathlib
pathlib.Path(os.environ['MOBILE_PEERS_TEST_ROOT'], 'native-called').touch()
raise SystemExit(1)
'''


class MobilePeers(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='hgs-mobile-peers-')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        home = self.root / 'home'
        bin_dir = home / '.local/bin'
        bin_dir.mkdir(parents=True)
        (self.root / 'tmux').mkdir()
        for name, source in [('ssh', SSH), ('tmux', TMUX)]:
            path = bin_dir / name
            path.write_text(source)
            path.chmod(0o755)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('HGS_')}
        self.env.update(HOME=str(home), PATH=str(bin_dir) + ':' + os.environ['PATH'],
                        MOBILE_PEERS_TEST_ROOT=str(self.root), MOBILE_PEERS_TEST_HGS=str(HGS),
                        HGS_TAB='0', TMUX_TMPDIR=str(self.root / 'tmux'))
        self.env.pop('TMUX', None)
        for node, peers in [('a', 'b d'), ('b', 'c'), ('c', ''), ('d', '')]:
            directory = self.root / node
            directory.mkdir()
            (directory / 'config').write_text(f'HGS_SELF="{node}"\nHGS_PEERS="{peers}"\n')
        (self.root / 'a/machines.local.json').write_text(json.dumps({'machines': {
            'b': dict(hostname='private-fixture.invalid', user='private-fixture', identity_file='/synthetic/private-key', enabled=True),
            'd': dict(enabled=False)}}))
        self.ids = {node: self.call(node, '__mobile-peer-identity', '--json').json()['machine_id'] for node in ['a', 'b', 'c', 'd']}

    def call(self, node, *argv, data=None, ok=True, timeout=15):
        env = dict(self.env, HGS_CONFIG_DIR=str(self.root / node), HGS_STATE_DIR=str(self.root / node / 'state'))
        result = subprocess.run([str(HGS), *argv], input=json.dumps(data) if data is not None else None,
                                env=env, capture_output=True, text=True, timeout=timeout)
        result.json = lambda: json.loads(result.stdout)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result

    def request(self, argv=None, target=None, via='b', payload=None):
        return dict(schema=1, via=via, target_machine_id=target or self.ids['b'], argv=argv or ['--help'], payload=payload)

    def calls(self):
        path = self.root / 'calls'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    @staticmethod
    def running(pid):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return False
        # Linux briefly reports X (dead) while a parent reaps a zombie; both are gone.
        proc = Path(f'/proc/{pid}/stat')
        if proc.exists():
            try:
                return proc.read_text().split(') ', 1)[1][0] not in 'ZX'
            except (FileNotFoundError, ProcessLookupError):
                return False
        status = subprocess.run(['ps', '-p', str(pid), '-o', 'stat='], capture_output=True, text=True)
        return status.returncode == 0 and not status.stdout.strip().startswith(('Z', 'X'))

    def wait_hanging_pids(self):
        until = time.monotonic() + 5
        path = self.root / 'hanging-pids'
        while time.monotonic() < until:
            if path.exists():
                try:
                    return json.loads(path.read_text())
                except ValueError:
                    pass
            time.sleep(.01)
        self.fail('owned SSH fixture did not start')

    def assert_owned_transports_exit(self, pids):
        until = time.monotonic() + 2
        while time.monotonic() < until and any(self.running(pid) for pid in pids):
            time.sleep(.01)
        self.assertFalse(any(self.running(pid) for pid in pids), 'owned SSH/supervisor survived cancellation')

    def test_discovery_is_direct_enabled_and_does_not_export_connection_settings(self):
        result = self.call('a', 'mobile-peers', '--json').json()
        self.assertEqual(result['local'], dict(machine_id=self.ids['a'], name='a'))
        self.assertEqual(result['peers'], [dict(via='b', machine_id=self.ids['b'], name='b', online=True, error=None, identity='verified')])
        self.assertEqual({alias for alias, _ in self.calls()}, {'b'})
        self.assertNotIn('c', [row['via'] for row in result['peers']])
        for private in ['private-fixture', 'private-key', 'hostname', 'identity_file']:
            self.assertNotIn(private, json.dumps(result))
        self.assertFalse((self.root / 'native-called').exists())

    def test_offline_and_old_peers_are_generic_and_never_attempt_native_launch(self):
        for marker in ['offline-b', 'old-b']:
            path = self.root / marker
            path.touch()
            result = self.call('a', 'mobile-peers', '--json').json()['peers'][0]
            self.assertEqual(result, dict(via='b', machine_id=None, name='b', online=False, error='unavailable', identity=None))
            self.call('a', 'mobile-peer', '--json', data=self.request(), ok=False)
            self.assertFalse((self.root / 'old-launch').exists())
            path.unlink()
        self.assertFalse((self.root / 'native-called').exists())

    def remembered(self):
        path = self.root / 'a/swarm/mobile-peers.json'
        return json.loads(path.read_text()) if path.exists() else None

    def test_unreachable_peer_keeps_last_verified_identity_without_becoming_usable(self):
        self.call('a', 'mobile-peers', '--json')
        path = self.root / 'a/swarm/mobile-peers.json'
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.remembered(), dict(schema=1, local=self.ids['a'],
                                                 peers=dict(b=dict(machine_id=self.ids['b'], name='b'))))
        for private in ['private-fixture', 'private-key', 'hostname', 'identity_file']:
            self.assertNotIn(private, path.read_text())
        for marker in ['offline-b', 'old-b']:
            (self.root / marker).touch()
            row = self.call('a', 'mobile-peers', '--json').json()['peers'][0]
            self.assertEqual(row, dict(via='b', machine_id=self.ids['b'], name='b', online=False,
                                       error='unavailable', identity='last_verified'))
            # The remembered identity is presentation only: dispatch still probes
            # and fails closed without reaching the native launcher.
            self.call('a', 'mobile-peer', '--json', data=self.request(), ok=False)
            self.call('a', 'mobile-peer', '--json', data=self.request(['ls', '--json', '--local']), ok=False)
            self.assertFalse((self.root / 'old-launch').exists())
            (self.root / marker).unlink()
        self.assertFalse((self.root / 'native-called').exists())

    def test_changed_identity_replaces_remembered_identity_and_removed_edges_forget_it(self):
        self.call('a', 'mobile-peers', '--json')
        catalog = self.root / 'b/swarm/catalog.json'
        data = json.loads(catalog.read_text())
        data['node_id'] = replaced = str(uuid.uuid4())
        catalog.write_text(json.dumps(data))
        self.assertEqual(self.call('a', 'mobile-peers', '--json').json()['peers'][0]['machine_id'], replaced)
        (self.root / 'offline-b').touch()
        row = self.call('a', 'mobile-peers', '--json').json()['peers'][0]
        self.assertEqual((row['machine_id'], row['identity']), (replaced, 'last_verified'))
        # Disabling the edge forgets it; enabling it again does not revive it.
        machines = self.root / 'a/machines.local.json'
        settings = json.loads(machines.read_text())
        settings['machines']['b']['enabled'] = False
        machines.write_text(json.dumps(settings))
        self.assertEqual(self.call('a', 'mobile-peers', '--json').json()['peers'], [])
        self.assertEqual(self.remembered()['peers'], {})
        settings['machines']['b']['enabled'] = True
        machines.write_text(json.dumps(settings))
        row = self.call('a', 'mobile-peers', '--json').json()['peers'][0]
        self.assertEqual((row['machine_id'], row['identity']), (None, None))

    def test_copied_or_reset_gateway_identity_does_not_inherit_remembered_peers(self):
        self.call('a', 'mobile-peers', '--json')
        (self.root / 'offline-b').touch()
        catalog = self.root / 'a/swarm/catalog.json'
        data = json.loads(catalog.read_text())
        data['node_id'] = str(uuid.uuid4())
        catalog.write_text(json.dumps(data))
        row = self.call('a', 'mobile-peers', '--json').json()['peers'][0]
        self.assertEqual((row['machine_id'], row['identity']), (None, None))

    def test_only_a_locally_verified_swarm_binding_can_seed_an_offline_identity(self):
        (self.root / 'offline-b').touch()
        catalog = self.root / 'a/swarm/catalog.json'
        data = json.loads(catalog.read_text())
        # Shared catalog machine names are replicated data, never identity evidence.
        data['bindings']['b'] = dict(id=self.ids['c'], verified=False)
        catalog.write_text(json.dumps(data))
        row = self.call('a', 'mobile-peers', '--json').json()['peers'][0]
        self.assertEqual((row['machine_id'], row['identity']), (None, None))
        data['bindings']['b'] = dict(id=self.ids['b'], verified=True)
        catalog.write_text(json.dumps(data))
        row = self.call('a', 'mobile-peers', '--json').json()['peers'][0]
        self.assertEqual(row, dict(via='b', machine_id=self.ids['b'], name='b', online=False,
                                   error='unavailable', identity='last_verified'))
        self.assertIsNone(self.remembered())
        self.call('a', 'mobile-peer', '--json', data=self.request(), ok=False)
        self.assertFalse((self.root / 'native-called').exists())

    def test_native_downgrade_after_probe_uses_fail_closed_swarm_namespace(self):
        (self.root / 'flip-b').touch()
        self.call('a', 'swarm', 'mobile-peer', '--json', data=self.request(), ok=False)
        result = self.call('a', 'swarm', 'mobile-peers', '--json').json()
        self.assertFalse(result['peers'][0]['online'])
        self.assertIn(['b', ['swarm', '__mobile-peer-local', '--json']], self.calls())
        self.assertFalse((self.root / 'old-launch').exists())
        self.assertFalse((self.root / 'native-called').exists())

    def test_remote_and_local_calls_pin_uuid_and_preserve_text_output(self):
        direct = self.call('b', '--help').stdout
        remote = self.call('a', 'mobile-peer', '--json', data=self.request()).stdout
        self.assertEqual(remote, direct)
        local = self.request(target=self.ids['a'])
        del local['via']
        self.assertEqual(self.call('a', 'swarm', '__mobile-peer-local', '--json', data=local).stdout, direct)
        bad = self.request(['ls', '--json', '--local'], target=self.ids['c'])
        result = self.call('a', 'mobile-peer', '--json', data=bad, ok=False)
        self.assertIn('identity changed', result.stderr)
        self.assertFalse((self.root / 'native-called').exists())
        local['target_machine_id'] = self.ids['b']
        self.call('a', '__mobile-peer-local', '--json', data=local, ok=False)

    def test_cli_version_travels_with_the_exact_local_and_peer_snapshot(self):
        version = self.call('a', '--version').stdout.strip().removeprefix('hgs ')
        local = self.call('a', 'ls', '--json', '--local').json()
        peer = self.call('a', 'mobile-peer', '--json', data=self.request(['ls', '--json', '--local'])).json()
        self.assertEqual(local['host'], 'a')
        self.assertEqual(peer['host'], 'b')
        self.assertEqual(local['hgs_version'], version)
        self.assertEqual(peer['hgs_version'], version)

    def test_git_status_help_marker_travels_with_the_exact_native_peer(self):
        marker = 'Mobile Git status ABI: git-status-v1'
        direct = self.call('b', '--help').stdout
        remote = self.call('a', 'mobile-peer', '--json', data=self.request()).stdout
        self.assertIn(marker, direct)
        self.assertEqual(remote, direct)
        self.assertFalse((self.root / 'native-called').exists())

    def test_git_status_reads_the_exact_checkout_without_mutating_git(self):
        checkout = self.root / "checkout ' проект"
        env = {key: value for key, value in self.env.items() if not key.startswith('GIT_')}
        env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null')
        def git(*args):
            return subprocess.check_output(['git', *args], env=env, stderr=subprocess.PIPE)
        git('init', '-q', '-b', 'main', str(checkout))
        (checkout / 'tracked').write_text('initial\n')
        git('-C', str(checkout), 'add', '.')
        git('-C', str(checkout), '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.test',
            'commit', '-qm', 'fixture')
        (checkout / 'tracked').write_text('changed\n')
        index = (checkout / '.git/index').read_bytes()
        refs = git('-C', str(checkout), 'show-ref')
        argv = ['git-status', '--path', str(checkout), '--json']
        remote = self.call('a', 'mobile-peer', '--json', data=self.request(argv)).json()
        direct = self.call('b', *argv).json()
        local = self.request(argv, target=self.ids['a'])
        del local['via']
        local = self.call('a', '__mobile-peer-local', '--json', data=local).json()
        for result in [direct, remote, local]:
            self.assertEqual(result['path'], str(checkout.resolve()))
            self.assertEqual(result['root'], str(checkout.resolve()))
            self.assertEqual((result['state'], result['branch'], result['changed_files'], result['remote_state']),
                             ('ok', 'main', 1, 'no_upstream'))
        self.assertEqual((checkout / '.git/index').read_bytes(), index)
        self.assertEqual(git('-C', str(checkout), 'show-ref'), refs)
        self.assertEqual((checkout / 'tracked').read_text(), 'changed\n')
        self.assertFalse((checkout / '.git/FETCH_HEAD').exists())
        self.assertEqual({alias for alias, _ in self.calls()}, {'b'})
        self.assertFalse((self.root / 'native-called').exists())

    def test_git_status_invalid_shapes_and_targets_never_reach_git(self):
        called = self.root / 'git-called'
        fake = Path(self.env['HOME']) / '.local/bin/git'
        fake.write_text('#!/usr/bin/env python3\nimport os, pathlib\n'
                        "pathlib.Path(os.environ['MOBILE_PEERS_TEST_ROOT'], 'git-called').touch()\nraise SystemExit(1)\n")
        fake.chmod(0o755)
        path = str(self.root / 'checkout')
        argv = ['git-status', '--path', path, '--json']
        invalid = [self.request(argv, payload={}), self.request(argv + ['--json']),
                   self.request(argv + ['--refresh']), self.request(argv + ['--path', path]),
                   self.request(['git-status', '--path', 'relative', '--json']),
                   self.request(['git-status', '--path', '/fixture/é' + 'é' * 2048, '--json']),
                   self.request(['@c', *argv])]
        for request in invalid:
            with self.subTest(argv=request['argv'], payload=request['payload']):
                self.call('a', 'mobile-peer', '--json', data=request, ok=False)
        self.assertEqual(self.calls(), [])
        self.call('a', 'mobile-peer', '--json', data=self.request(argv, target=self.ids['c']), ok=False)
        self.call('a', 'mobile-peer', '--json', data=self.request(argv, via='c'), ok=False)
        wrong_local = self.request(argv)
        del wrong_local['via']
        self.call('a', '__mobile-peer-local', '--json', data=wrong_local, ok=False)
        self.assertFalse(called.exists())
        self.assertFalse((self.root / 'native-called').exists())

    def test_remote_directory_creation_pins_target_and_never_overwrites_or_replays(self):
        target = self.root / "new ' проект $(touch injected)"
        identity = str(uuid.uuid4())
        argv = ['dirs', '--create', str(target), '--request-id', identity]
        wrong = self.request(argv, target=self.ids['c'])
        self.call('a', 'mobile-peer', '--json', data=wrong, ok=False)
        self.assertFalse(target.exists())
        request = self.request(argv)
        result = self.call('a', 'mobile-peer', '--json', data=request).json()
        self.assertEqual(result['request_id'], identity)
        self.assertEqual(result['requested_path'], str(target))
        self.assertEqual(result['path'], str(target.resolve()))
        self.assertTrue(target.is_dir())
        self.assertFalse((target / '.git').exists())
        self.assertFalse((self.root / 'injected').exists())
        (target / 'retained').write_text('retained')
        self.call('a', 'mobile-peer', '--json', data=request, ok=False)
        self.assertEqual((target / 'retained').read_text(), 'retained')
        self.assertFalse((self.root / 'native-called').exists())

    def test_replaced_persistent_identity_cannot_execute_old_bound_request(self):
        path = self.root / 'b/swarm/catalog.json'
        data = json.loads(path.read_text())
        data['node_id'] = str(uuid.uuid4())
        path.write_text(json.dumps(data))
        self.call('a', 'mobile-peer', '--json', data=self.request(['ls', '--json', '--local']), ok=False)
        self.assertFalse((self.root / 'native-called').exists())

    def test_unknown_routes_fields_and_forwarding_fail_before_ssh(self):
        for via in ['c', 'd', 'a', '-oProxyCommand=fixture', 'b;fixture', 'b/c']:
            self.call('a', 'mobile-peer', '--json', data=self.request(via=via), ok=False)
        for argv in [['@c', 'ls', '--json', '--local'], ['swarm', 'sync'], ['account', 'export', 'native-codex'],
                     ['sh', '-c', 'echo fixture'], ['mobile-peer', '--json'], ['send', 'codex/fixture', 'text']]:
            self.call('a', 'mobile-peer', '--json', data=self.request(argv), ok=False)
        for field in ['path', 'depth', 'command']:
            request = self.request()
            request[field] = 'fixture'
            self.call('a', 'mobile-peer', '--json', data=request, ok=False)
        self.assertEqual(self.calls(), [])
        request = self.request()
        self.call('b', '__mobile-peer-local', '--json', data=request, ok=False)
        self.assertFalse((self.root / 'native-called').exists())

    def test_payload_reaches_existing_scoped_json_dispatch(self):
        payload = dict(request_id=str(uuid.uuid4()), expected_run_id='fixture-run', expected_conversation_id='fixture-conversation', action='snapshot')
        request = self.request(['terminal', 'codex/missing', '--json'], payload=payload)
        result = self.call('a', 'mobile-peer', '--json', data=request, ok=False)
        self.assertNotIn('EOF', result.stderr)
        self.assertNotIn('scoped native payload is required', result.stderr)
        self.assertNotIn('native operation requires scoped JSON', result.stderr)

    def test_discovery_timeout_remains_bounded_and_leaves_local_identity_available(self):
        (self.root / 'delay-b').touch()
        start = time.monotonic()
        result = self.call('a', 'mobile-peers', '--json').json()
        self.assertLess(time.monotonic() - start, 5)
        self.assertEqual(result['local']['machine_id'], self.ids['a'])
        self.assertFalse(result['peers'][0]['online'])

    def test_transport_preserves_binary_stdout_and_native_exit_status(self):
        (self.root / 'binary').touch()
        env = dict(self.env, HGS_CONFIG_DIR=str(self.root / 'a'), HGS_STATE_DIR=str(self.root / 'a/state'))
        result = subprocess.run([str(HGS), 'swarm', 'mobile-peer', '--json'], input=json.dumps(self.request()).encode(),
                                env=env, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 7)
        self.assertEqual(result.stdout, bytes([0, 255, 128, 10, 13, 42]))
        self.assertEqual(result.stderr, b'fixture')

    def test_transport_caps_stdout_and_stderr_before_forwarding(self):
        for marker in ['stdout-overflow', 'stderr-overflow']:
            path = self.root / marker
            path.touch()
            result = self.call('a', 'swarm', 'mobile-peer', '--json', data=self.request(), ok=False)
            self.assertIn('output is too large', result.stderr)
            self.assertEqual(result.stdout, '')
            path.unlink()

    def test_whole_ssh_deadline_covers_inherited_descendant_output_pipes(self):
        (self.root / 'inherited-pipe').touch()
        start = time.monotonic()
        result = self.call('a', 'swarm', 'mobile-peer', '--json', data=self.request(), ok=False, timeout=35)
        self.assertIn('timed out', result.stderr)
        self.assertGreater(time.monotonic() - start, 29)
        self.assertLess(time.monotonic() - start, 33)

    def test_gateway_kill_reaps_only_owned_ssh_and_supervisor(self):
        (self.root / 'hang').touch()
        (self.root / 'ssh-child').touch()
        env = dict(self.env, HGS_CONFIG_DIR=str(self.root / 'a'), HGS_STATE_DIR=str(self.root / 'a/state'))
        process = subprocess.Popen([str(HGS), 'swarm', 'mobile-peer', '--json'], env=env,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.addCleanup(lambda: process.kill() if process.poll() is None else None)
        process.stdin.write(json.dumps(self.request()).encode())
        process.stdin.close()
        pids = self.wait_hanging_pids()
        process.kill()
        process.wait(timeout=5)
        process.stdout.close()
        process.stderr.close()
        self.assert_owned_transports_exit(pids)
        # A stand-in child outside the owned transport boundary must survive.
        # It exits naturally after two seconds; no process group is signalled.
        child = int((self.root / 'ssh-child-pid').read_text())
        self.assertTrue(self.running(child))

    def test_supervisor_exits_if_gateway_dies_during_partial_input(self):
        env = dict(self.env, HGS_CONFIG_DIR=str(self.root / 'a'), HGS_STATE_DIR=str(self.root / 'a/state'))
        script = '''import os, pathlib, subprocess, sys, time
child = subprocess.Popen([sys.argv[1], 'swarm', '__mobile-peer-transport', '--json'],
                         stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
child.stdin.write(b'{'); child.stdin.flush()
# Renamed into place: the test reads the file as soon as it exists.
staged = pathlib.Path(os.environ['MOBILE_PEERS_TEST_ROOT'], 'partial-pid.tmp')
staged.write_text(str(child.pid))
os.replace(staged, staged.with_name('partial-pid'))
time.sleep(10)
'''
        process = subprocess.Popen([sys.executable, '-c', script, str(HGS)], env=env)
        self.addCleanup(lambda: process.kill() if process.poll() is None else None)
        path = self.root / 'partial-pid'
        until = time.monotonic() + 3
        while not path.exists() and time.monotonic() < until:
            time.sleep(.01)
        self.assertTrue(path.exists())
        pid = int(path.read_text())
        process.kill()
        process.wait(timeout=3)
        self.assert_owned_transports_exit([pid])
        self.assertEqual(self.calls(), [])

    def test_discovery_fanout_transports_exit_after_gateway_kill(self):
        (self.root / 'a/config').write_text('HGS_SELF="a"\nHGS_PEERS="b c d"\n')
        (self.root / 'a/machines.local.json').write_text('{"machines":{}}')
        (self.root / 'discovery-hang').touch()
        env = dict(self.env, HGS_CONFIG_DIR=str(self.root / 'a'), HGS_STATE_DIR=str(self.root / 'a/state'))
        paths = [self.root / ('discovery-pids-' + alias) for alias in ['b', 'c', 'd']]
        # Each peer's identity transport starts after the local identity and its own
        # support probe, all separate processes, each stage within its own 3 s cap.
        # A stage stalled beyond its cap leaves that peer offline and the gateway
        # then finishes: bounded discovery, not the reaping under test. Only such
        # a finished discovery is started again, as is one that reaped its own
        # transports before this test could kill it.
        stalled, receipts, owned = [], [], False
        for _ in range(3):
            for path in paths:
                path.unlink(missing_ok=True)
            process = subprocess.Popen([str(HGS), 'swarm', 'mobile-peers', '--json'], env=env,
                                       stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.addCleanup(lambda process=process: process.kill() if process.poll() is None else None)
            # The gateway's own exit ends the wait; the bound only guards a hang.
            until = time.monotonic() + 20
            while not all(path.exists() for path in paths) and process.poll() is None and time.monotonic() < until:
                time.sleep(.01)
            receipts += [json.loads(path.read_text()) for path in paths if path.exists()]
            if all(path.exists() for path in paths):
                pids = [pid for path in paths for pid in json.loads(path.read_text())['pids']]
                owned = process.poll() is None and all(self.running(pid) for pid in pids)
                if owned:
                    break
                # Its own deadline already stops them; the discovery deadline bounds the rest.
                process.wait(timeout=10)
            self.assertIsNotNone(process.poll(), 'discovery outlived its deadline')
            stalled.append((process.returncode, process.communicate(timeout=5)[1].decode(errors='replace'),
                            [path.name for path in paths if not path.exists()]))
        self.assertTrue(owned, stalled)
        process.kill()
        process.wait(timeout=5)
        process.stdout.close()
        process.stderr.close()
        self.assert_owned_transports_exit(pids)
        # Concurrent workers never hand one transport another's pipe ends.
        self.assertEqual([receipt['inherited'] for receipt in receipts], [[]] * len(receipts))

    def repository_clone_argv(self, name="checkout"):
        return ['repository', 'clone', '--json', '--url', 'https://example.invalid/team/source.git',
                '--parent', str(self.root), '--name', name, '--request-id', str(uuid.uuid4())]

    def test_repository_clone_crosses_both_transport_deadline_guards(self):
        source = self.root / 'source'
        env = dict(self.env, GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=str(self.root / 'gitconfig'))
        self.env.update(GIT_CONFIG_NOSYSTEM=env['GIT_CONFIG_NOSYSTEM'], GIT_CONFIG_GLOBAL=env['GIT_CONFIG_GLOBAL'])
        def git(*argv):
            subprocess.run(['git', *argv], env=env, check=True, capture_output=True, timeout=10)
        git('init', '-q', '-b', 'main', str(source))
        (source / 'content').write_text('retained source\n')
        git('-C', str(source), 'add', '.')
        git('-C', str(source), '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.test', 'commit', '-qm', 'fixture')
        git('config', '--global', 'url.' + source.as_uri() + '.insteadOf', 'https://example.invalid/team/source.git')
        argv = self.repository_clone_argv()
        receipt = self.call('a', 'mobile-peer', '--json', data=self.request(argv)).json()
        self.assertEqual(receipt['status'], 'cloned')
        self.assertEqual((self.root / 'checkout/content').read_text(), 'retained source\n')
        self.assertEqual(self.call('a', 'mobile-peer', '--json', data=self.request(argv)).json(), receipt)
        self.assertFalse((self.root / 'native-called').exists())

    def transport_envelope(self, helper, request, budget=77000):
        return dict(schema=1, via='b', helper=helper, request=request, parent_pid=os.getpid(),
                    deadline_ms=time.clock_gettime_ns(time.CLOCK_MONOTONIC)//1_000_000+budget)

    def test_transport_uses_the_wire_clock_when_python_monotonic_differs(self):
        (self.root / 'repository-probe').touch()
        local = self.request(self.repository_clone_argv())
        del local['via']
        # On a Mac that has slept, mach_absolute_time (Python's local elapsed
        # clock) and the protocol's CLOCK_MONOTONIC have different origins.
        with patch.object(time, 'monotonic', return_value=0):
            valid = self.transport_envelope('local', local)
        result = self.call('a', 'swarm', '__mobile-peer-transport', '--json', data=valid, ok=False)
        self.assertEqual((result.returncode, result.stderr), (7, 'fixture'))
        self.assertTrue(self.calls())

    @unittest.skipUnless(sys.platform.startswith('linux'), 'Linux-only short transport-budget control')
    def test_delayed_shell_discovery_cannot_outlive_the_original_transport_budget(self):
        script = self.root/'home/.local/bin/ssh'
        script.write_text("#!/usr/bin/env python3\nimport os,pathlib,sys,time\nr=pathlib.Path(os.environ['MOBILE_PEERS_TEST_ROOT'])\nif sys.argv[-1].startswith('echo '+chr(34)+'hgs-remote-v2|'):\n (r/'short-probe-started').touch();time.sleep(3);print('hgs-remote-v2|%OS%|%SystemRoot%|:OS|:SystemRoot');sys.exit(0)\n(r/'short-operation-started').touch();print('unexpected operation')\n")
        started = time.monotonic()
        result = self.call('a', 'swarm', '__mobile-peer-transport', '--json',
                           data=self.transport_envelope('help', None, 150), ok=False, timeout=5)
        elapsed = time.monotonic()-started
        self.assertLess(elapsed, 1.0, result.stderr)
        self.assertIn('timed out', result.stderr)
        self.assertTrue((self.root/'short-probe-started').exists())
        self.assertFalse((self.root/'short-operation-started').exists())

    def test_supervisor_extended_deadline_accepts_valid_local_clone(self):
        (self.root / 'repository-probe').touch()
        local = self.request(self.repository_clone_argv())
        del local['via']
        valid = self.transport_envelope('local', local)
        result = self.call('a', 'swarm', '__mobile-peer-transport', '--json', data=valid, ok=False)
        self.assertEqual(result.returncode, 7)
        self.assertEqual(result.stderr, 'fixture')
        self.assertTrue(self.calls())

    def test_supervisor_extended_deadline_refuses_other_helpers_and_malformed_clone(self):
        (self.root / 'repository-probe').touch()
        local = self.request(self.repository_clone_argv())
        del local['via']
        envelope = self.transport_envelope
        before = self.calls()
        for helper in ['help', 'identity', 'own_identity']:
            self.call('a', 'swarm', '__mobile-peer-transport', '--json', data=envelope(helper, None), ok=False)
        for request in [None, dict(local, payload={}), dict(local, argv=['--help']),
                        dict(local, argv=local['argv'][:-2]), dict(local, target_machine_id='unknown')]:
            self.call('a', 'swarm', '__mobile-peer-transport', '--json', data=envelope('local', request), ok=False)
        self.call('a', 'swarm', '__mobile-peer-transport', '--json', data=envelope('local', local, 81000), ok=False)
        self.assertEqual(self.calls(), before)
        # The ordinary Help transport remains usable at its original short budget.
        self.call('a', 'swarm', '__mobile-peer-transport', '--json', data=envelope('help', None, 2000))
        self.assertGreater(len(self.calls()), len(before))

    def test_supervisor_rejects_arbitrary_helpers_and_deadline_overrides(self):
        original = self.transport_envelope('help', None, 2000)
        for field, value in [('helper', 'sh -c fixture'), ('deadline_ms', 0),
                             ('deadline_ms', time.clock_gettime_ns(time.CLOCK_MONOTONIC)//1_000_000+60000), ('parent_pid', 1), ('command', 'fixture')]:
            request = {**original, field: value}
            self.call('a', 'swarm', '__mobile-peer-transport', '--json', data=request, ok=False)
        self.assertEqual(self.calls(), [])

    @unittest.skipUnless(importlib.util.find_spec('aiohttp'), 'requires the mobile service test environment')
    def test_connector_timeout_and_cancellation_reap_owned_transports(self):
        sys.path.insert(0, str(REPO / 'services/mobile'))
        from zerus_mobile.connector import Connector, ConnectorError

        # The native gateway probes peer support before launching the owned SSH
        # operation. Cold Mac/CI interpreter startup must fit inside this budget;
        # the synthetic SSH then stalls for 30 seconds, well beyond our timeout.
        operation_timeout = 5

        async def scenario(cancel):
            connector = Connector.__new__(Connector)
            connector.hgs, connector.timeout, connector.max_bytes = str(HGS), operation_timeout, 32 * 1024 * 1024
            (self.root / 'hang').touch()
            path = self.root / 'hanging-pids'
            path.unlink(missing_ok=True)
            task = asyncio.create_task(connector._native(['swarm', 'mobile-peer', '--json'], self.request(),
                                                       timeout=operation_timeout, json_output=False))
            try:
                until = time.monotonic() + operation_timeout
                pids = None
                while time.monotonic() < until and pids is None:
                    if path.exists():
                        try:
                            pids = json.loads(path.read_text())
                        except ValueError:
                            pass  # The fixture may still be writing its ownership receipt.
                    if pids is None:
                        if task.done():
                            break
                        await asyncio.sleep(.01)
                self.assertIsNotNone(pids, 'owned SSH must be running before deadline or cancellation')
                if cancel:
                    task.cancel()
                    with self.assertRaises(asyncio.CancelledError):
                        await task
                else:
                    with self.assertRaises(ConnectorError):
                        await task
                self.assert_owned_transports_exit(pids)
            finally:
                if not task.done():
                    task.cancel()
                await asyncio.gather(task, return_exceptions=True)

        env = dict(self.env, HGS_CONFIG_DIR=str(self.root / 'a'), HGS_STATE_DIR=str(self.root / 'a/state'))
        with patch.dict(os.environ, env, clear=True):
            asyncio.run(scenario(False))
            asyncio.run(scenario(True))


if __name__ == '__main__':
    unittest.main()
