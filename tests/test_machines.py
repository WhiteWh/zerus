#!/usr/bin/env python3
"""Machine profile and setup integration tests. SSH, HOME and install targets are isolated."""
import concurrent.futures
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[1]
HGS = Path(os.environ.get("HGS_TEST_BIN", REPO / "target/debug/hgs")).resolve()
FAKE_SSH = r'''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
root = pathlib.Path(os.environ['MACHINE_FIXTURE'])
(root / 'ssh-args.json').write_text(json.dumps(sys.argv[1:]))
mode = os.environ.get('MACHINE_SSH_MODE', 'probe')
if '-G' in sys.argv:
    if mode == 'resolve-hang':
        import time
        time.sleep(30)
    if mode == 'resolve-large':
        print('x' * (150 * 1024))
        sys.exit(0)
    if mode == 'resolve-config':
        sys.exit(subprocess.run([os.environ['MACHINE_REAL_SSH'], '-F', str(root/'ssh-config'), *sys.argv[1:]]).returncode)
    if mode == 'denied':
        print('Bad configuration option: fixture', file=sys.stderr)
        sys.exit(255)
    print('hostname inherited.example\nuser alice\nport 2222\nidentityfile ~/.ssh/id_ed25519\nidentityfile /Keys/with spaces\nproxyjump jump.example\nproxycommand token=DO_NOT_EXPOSE\nsetenv API_TOKEN=DO_NOT_EXPOSE')
    sys.exit(0)
if mode == 'denied':
    print('Permission denied (publickey).', file=sys.stderr)
    sys.exit(255)
if mode == 'probe':
    print('Welcome / login banner\n\n__HGS_MACHINE__\nos=Darwin\narch=arm64\nhgs=hgs 1.17.0\ntmux=tmux 3.5\ncargo=/opt/homebrew/bin/cargo')
elif mode == 'no-marker':
    print('Unexpected remote output')
elif mode == 'setup':
    payload = sys.stdin.buffer.read()
    (root / 'bundle.tar.gz').write_bytes(payload)
    env = dict(os.environ, HOME=str(root / 'remote'))
    sys.exit(subprocess.run(['/bin/sh', '-c', sys.argv[-1]], input=payload, env=env).returncode)
'''


class Machines(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='hgs-machines-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / 'home'
        self.config = self.home / '.config/hgs'
        self.config.mkdir(parents=True)
        self.original_config = '# managed by Ansible\nHGS_SELF="test-local"\nHGS_PEERS="inherited"\n'
        (self.config / 'config').write_text(self.original_config)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.env = dict(os.environ, HOME=str(self.home), HGS_CONFIG_DIR=str(self.config),
                        HGS_STATE_DIR=str(self.root / 'state'), MACHINE_FIXTURE=str(self.root),
                        HGS_TAB='0', PATH=str(self.bin) + ':' + os.environ['PATH'])
        for key in ('HGS_PEERS', 'HGS_SELF', 'TMUX', 'TMUX_PANE', 'HGS_RUN_ID',
                    'HGS_SESSION', 'HGS_EXECUTABLE', 'CARGO_TARGET_DIR', 'HGS_INSTALL_PREFIX'):
            self.env.pop(key, None)
        self.script('ssh', FAKE_SSH)
        self.script('tmux', '#!/bin/sh\nif [ "$1" = -V ]; then echo "tmux 3.5"; fi\n')

    def script(self, name, value):
        path = self.bin / name
        path.write_text(value)
        path.chmod(0o755)

    def invoke(self, *args, success=True, env=None):
        result = subprocess.run([str(HGS), *args], env=env or self.env, text=True,
                                capture_output=True, timeout=15)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual((self.config / 'config').read_text(), self.original_config)
        return result

    def run_machine(self, *args, success=True, env=None):
        return self.invoke('machine', *args, success=success, env=env)

    def listing(self):
        return json.loads(self.run_machine('ls').stdout)

    def set_profile(self, name='studio', profile=None, extra=(), **options):
        return self.run_machine('set', name, '--json', json.dumps(profile or {'hostname': 'studio.local'}), *extra, **options)

    def ssh_args(self):
        return json.loads((self.root / 'ssh-args.json').read_text())

    def test_new_profiles_and_ssh_overrides_preserve_managed_files(self):
        initial = self.listing()
        self.assertEqual(initial['machines'][0]['source'], 'config')
        self.assertEqual(initial['revision'], hashlib.sha256(b'').hexdigest())
        key = '~/Keys/space key$(literal)'
        self.set_profile(profile=dict(hostname='2001:db8::12', user='alice', port=2222, identity_file=key),
                         extra=('--revision', initial['revision'], '--create'))
        listing = self.listing()
        self.assertNotEqual(listing['revision'], initial['revision'])
        self.assertEqual([m['alias'] for m in listing['machines']], ['inherited', 'studio'])
        self.assertEqual((self.config / 'machines.local.json').stat().st_mode & 0o777, 0o600)
        result = json.loads(self.run_machine('check', 'studio').stdout)
        self.assertTrue(result['ok'])
        args = self.ssh_args()
        self.assertEqual(args[:8], ['-o', 'HostName=2001:db8::12', '-l', 'alice', '-p', '2222', '-i', str(self.home / 'Keys/space key$(literal)')])
        self.assertIn('StrictHostKeyChecking=yes', args)
        self.assertIn('BatchMode=yes', args)
        self.assertEqual(args[-2], 'studio')
        peers = json.loads(self.invoke('ls', '--local', '--json').stdout)['peers']
        self.assertEqual(peers, ['inherited', 'studio'])

    def test_folder_shell_uses_profile_and_quotes_path(self):
        self.set_profile(profile=dict(hostname='studio.local', user='alice', port=2222))
        folder="/work/a folder/it's $(touch bad); `echo bad`"
        self.run_machine('ssh', 'studio', '--directory', folder)
        args=self.ssh_args()
        self.assertEqual(args[:6], ['-o', 'HostName=studio.local', '-l', 'alice', '-p', '2222'])
        self.assertEqual(args[-3:-1], ['-t', 'studio'])
        import shlex
        shell, flag, command = shlex.split(args[-1])
        self.assertEqual((shell, flag), ('sh', '-c'))
        self.assertEqual(shlex.split(command)[:3], ['cd', '--', folder])
        self.assertIn('&& exec "${SHELL:-/bin/sh}" -l', command)
        self.run_machine('ssh', 'studio', '--directory', 'relative', success=False)

    def test_inherited_disable_reset_and_local_removal(self):
        self.set_profile('inherited', {'hostname': 'changed.example'})
        self.run_machine('remove', 'inherited')
        self.assertFalse(self.listing()['machines'][0]['enabled'])
        self.assertEqual(json.loads(self.invoke('ls', '--local', '--json').stdout)['peers'], [])
        self.run_machine('reset', 'inherited')
        self.assertEqual(self.listing()['machines'][0]['source'], 'config')
        self.assertTrue(self.listing()['machines'][0]['enabled'])
        self.set_profile()
        self.run_machine('remove', 'studio')
        self.assertEqual([m['alias'] for m in self.listing()['machines']], ['inherited'])

    def test_stale_revision_and_create_never_overwrite_existing_profile(self):
        revision = self.listing()['revision']
        self.set_profile(extra=('--revision', revision, '--create'))
        before = (self.config / 'machines.local.json').read_bytes()
        result = self.set_profile(profile={'hostname': 'wrong.example'}, extra=('--revision', revision), success=False)
        self.assertIn('changed elsewhere', result.stderr)
        result = self.set_profile(profile={'hostname': 'wrong.example'}, extra=('--create',), success=False)
        self.assertIn('already exists', result.stderr)
        self.assertEqual((self.config / 'machines.local.json').read_bytes(), before)
        self.set_profile('inherited', extra=('--create',), success=False)

    def test_concurrent_edits_with_same_revision_only_one_wins(self):
        revision = self.listing()['revision']
        def save(name):
            return subprocess.run([str(HGS), 'machine', 'set', name, '--json', '{}', '--revision', revision],
                                  env=self.env, capture_output=True, text=True, timeout=10)
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(save, ('one', 'two')))
        self.assertEqual(sorted(r.returncode for r in results), [0, 1])
        self.assertEqual(len(self.listing()['machines']), 2)

    def test_environment_peers_keep_precedence(self):
        self.set_profile()
        env = dict(self.env, HGS_PEERS='external')
        result = json.loads(self.run_machine('ls', env=env).stdout)
        self.assertTrue(result['environment_override'])
        self.assertEqual([r['alias'] for r in result['machines'] if r['enabled']], ['external'])
        self.assertIn('environment', self.run_machine('remove', 'external', env=env, success=False).stderr)
        self.assertEqual(json.loads(self.invoke('ls', '--local', '--json', env=env).stdout)['peers'], ['external'])

    def test_invalid_inputs_are_rejected_without_any_write(self):
        for alias in ('-oProxyCommand=bad', 'space name', 'host;id', 'test-local'):
            with self.subTest(alias=alias):
                self.set_profile(alias, success=False)
        for profile in ({'hostname': '-bad'}, {'hostname': 'host;id'}, {'hostname': 'two hosts'},
                        {'user': 'user;id'}, {'port': 0}, {'port': 65536},
                        {'identity_file': 'relative/key'}, {'identity_file': '/key\nnewline'},
                        {'unknown': 'field'}):
            with self.subTest(profile=profile):
                self.set_profile(profile=profile, success=False)
        self.assertFalse((self.config / 'machines.local.json').exists())
        self.assertFalse((self.root / 'ssh-args.json').exists())
        for arguments in (('ls', 'extra'), ('check', 'studio', 'extra'), ('ssh', 'studio', 'extra')):
            self.run_machine(*arguments, success=False)

    def test_probe_failures_are_structured_and_keep_ssh_diagnostics(self):
        denied = json.loads(self.run_machine('check', 'studio', env=dict(self.env, MACHINE_SSH_MODE='denied')).stdout)
        self.assertFalse(denied['ok'])
        self.assertFalse(denied['ssh_ok'])
        self.assertIn('Permission denied', denied['error'])
        no_marker = json.loads(self.run_machine('check', 'studio', env=dict(self.env, MACHINE_SSH_MODE='no-marker')).stdout)
        self.assertFalse(no_marker['ok'])
        self.assertTrue(no_marker['ssh_ok'])
        self.assertIn('setup report', no_marker['error'])

    def test_effective_ssh_settings_are_read_only_and_allowlisted(self):
        resolved = json.loads(self.run_machine('resolve', 'inherited').stdout)
        self.assertEqual(resolved['alias'], 'inherited')
        self.assertEqual(resolved['effective']['hostname'], 'inherited.example')
        self.assertEqual(resolved['effective']['port'], '2222')
        self.assertEqual(resolved['effective']['identity_files'], ['~/.ssh/id_ed25519', '/Keys/with spaces'])
        self.assertEqual(resolved['effective']['proxy_jump'], 'jump.example')
        self.assertTrue(resolved['effective']['proxy_command'])
        self.assertNotIn('DO_NOT_EXPOSE', json.dumps(resolved))
        self.assertEqual(self.ssh_args(), ['-G', '-T', 'inherited'])
        self.assertFalse((self.config / 'machines.local.json').exists())
        self.run_machine('resolve', '-oProxyCommand=bad', success=False)
        self.run_machine('resolve', 'test-local', success=False)
        self.run_machine('resolve', 'inherited', 'extra', success=False)

    def test_resolution_uses_openssh_config_with_zerus_overrides(self):
        real_ssh = shutil.which('ssh')
        if not real_ssh:
            self.skipTest('OpenSSH not installed')
        (self.root / 'ssh-config').write_text('Host inherited\n  HostName 192.0.2.5\n  User inherited-user\n  Port 2222\n  IdentityFile "/Keys/with spaces"\n  ProxyJump bastion.example\n')
        env = dict(self.env, MACHINE_SSH_MODE='resolve-config', MACHINE_REAL_SSH=real_ssh)
        initial = json.loads(self.run_machine('resolve', 'inherited', env=env).stdout)['effective']
        self.assertEqual((initial['hostname'], initial['user'], initial['port']), ('192.0.2.5', 'inherited-user', '2222'))
        self.assertEqual(initial['identity_files'], ['/Keys/with spaces'])
        self.assertEqual(initial['proxy_jump'], 'bastion.example')
        self.set_profile('inherited', {'hostname': 'override.example', 'user': 'bob', 'port': 2200})
        current = json.loads(self.run_machine('resolve', 'inherited', env=env).stdout)['effective']
        self.assertEqual((current['hostname'], current['user'], current['port']), ('override.example', 'bob', '2200'))
        self.assertEqual(current['identity_files'], ['/Keys/with spaces'])

    def test_resolution_has_bounded_time_output_and_clear_errors(self):
        for mode, error in [('denied', 'Bad configuration option'), ('resolve-large', 'exceeded'), ('resolve-hang', 'timed out')]:
            with self.subTest(mode=mode):
                started = time.monotonic()
                result = self.run_machine('resolve', 'inherited', env=dict(self.env, MACHINE_SSH_MODE=mode), success=False)
                self.assertIn(error, result.stderr)
                self.assertLess(time.monotonic() - started, 6)

    def test_dry_run_does_not_write_or_connect(self):
        self.invoke('--dry-run', 'machine', 'set', 'studio', '--json', '{}')
        self.invoke('--dry-run', 'machine', 'ssh', 'studio')
        self.assertFalse((self.config / 'machines.local.json').exists())
        self.assertFalse((self.root / 'ssh-args.json').exists())

    def prepare_setup(self):
        source = self.root / 'source with spaces'
        (source / 'src').mkdir(parents=True)
        for filename in ('Cargo.lock', 'build.rs', 'install.sh', 'hgs_state.py'):
            shutil.copy2(REPO / filename, source / filename)
        shutil.copytree(REPO / 'scripts', source / 'scripts', ignore=shutil.ignore_patterns('__pycache__'))
        (source / 'Cargo.toml').write_text("[package]\nname='setup-fixture'\nversion='0.1.0'\n[dependencies]\nfixture={path='crates/fixture'}\n")
        child = source / 'crates/fixture'
        (child / 'src').mkdir(parents=True)
        (child / 'Cargo.toml').write_text("[package]\nname='fixture'\nversion='0.1.0'\n[dependencies]\ncore={path='../../client/core'}\n")
        (child / 'src/lib.rs').write_text('// fixture')
        core = source / 'client/core'
        (core / 'src').mkdir(parents=True)
        (core / 'Cargo.toml').write_text("[package]\nname='core'\nversion='0.1.0'\n")
        (core / 'src/lib.rs').write_text('// core')
        (core / 'target').mkdir()
        (core / 'target/private.secret').write_text('must not travel')
        (source / 'src/main.rs').write_text('// fixture source')
        (source / 'not-for-upload.secret').write_text('must not travel')
        self.script('cargo', r'''#!/usr/bin/env python3
import os,pathlib,sys
if os.environ.get('MACHINE_BUILD_FAIL'): sys.exit(12)
manifest = pathlib.Path(sys.argv[sys.argv.index('--manifest-path') + 1])
target = manifest.parent / 'target/release'
target.mkdir(parents=True)
(target / 'hgs').write_text('#!/bin/sh\necho "hgs fixture-installed"\n')
''')
        remote = self.root / 'remote'
        (remote / '.local/src/hgs').mkdir(parents=True)
        (remote / '.local/src/hgs/untouched').write_text('checkout stays')
        (remote / '.local/bin').mkdir(parents=True)
        (remote / '.local/bin/zerus-mobile-connector').write_text('keep connector unchanged')
        return source, remote, dict(self.env, MACHINE_SSH_MODE='setup')

    def test_setup_native_install_uploads_only_cli_and_preserves_remote_config(self):
        source, remote, env = self.prepare_setup()
        (remote / '.config/hgs').mkdir(parents=True)
        existing = remote / '.config/hgs/config'
        existing.write_text('HGS_SELF="keep-name"\nHGS_PEERS="keep-peer"\n')
        # An unrelated remote installer environment must not redirect the app's installation.
        env['HGS_INSTALL_PREFIX'] = str(remote / 'wrong-place')
        result = self.run_machine('setup', 'studio', '--source', str(source), env=env)
        self.assertIn('Setup complete: hgs fixture-installed', result.stdout)
        self.assertTrue((remote / '.local/bin/hgs').is_file())
        self.assertTrue((remote / '.local/bin/hgs_state.py').is_file())
        self.assertEqual((remote / '.local/bin/zerus-mobile-connector').read_text(), 'keep connector unchanged')
        self.assertFalse((remote / 'wrong-place').exists())
        self.assertEqual(existing.read_text(), 'HGS_SELF="keep-name"\nHGS_PEERS="keep-peer"\n')
        self.assertEqual((remote / '.local/src/hgs/untouched').read_text(), 'checkout stays')
        self.assertEqual(list((remote / '.local/share/hgs').glob('setup.*')), [])
        with tarfile.open(fileobj=io.BytesIO((self.root / 'bundle.tar.gz').read_bytes())) as archive:
            names = [name.removeprefix('./') for name in archive.getnames()]
        self.assertIn('src/main.rs', names)
        self.assertIn('crates/fixture/Cargo.toml', names)
        self.assertIn('client/core/src/lib.rs', names)
        self.assertNotIn('client/core/target/private.secret', names)
        self.assertIn('build.rs', names)
        self.assertIn('scripts/install-macos-session-service.py', names)
        self.assertNotIn('not-for-upload.secret', names)
        self.assertIn('BatchMode=yes', self.ssh_args())
        self.assertIn('StrictHostKeyChecking=yes', self.ssh_args())

    def test_setup_revision_is_additive_and_refuses_stale_or_missing_alias_before_ssh(self):
        source, remote, env = self.prepare_setup()
        self.set_profile('studio', {'hostname': 'peer.invalid'})
        expected = json.loads(self.run_machine('ls').stdout)['revision']
        self.run_machine('setup', 'studio', '--source', str(source), '--revision', expected, env=env)
        (self.root / 'ssh-args.json').unlink()
        self.set_profile('studio', {'hostname': 'changed.invalid'})
        result = self.run_machine('setup', 'studio', '--source', str(source), '--revision', expected, env=env, success=False)
        self.assertIn('Reload the page before setup', result.stderr)
        self.assertFalse((self.root / 'ssh-args.json').exists())
        current = json.loads(self.run_machine('ls').stdout)['revision']
        self.run_machine('setup', 'missing.invalid', '--source', str(source), '--revision', current, env=env, success=False)
        self.assertFalse((self.root / 'ssh-args.json').exists())

    def test_setup_rechecks_the_original_profile_after_source_packaging(self):
        source, remote, env = self.prepare_setup()
        self.set_profile('studio', {'hostname': 'peer.invalid'})
        expected = json.loads(self.run_machine('ls').stdout)['revision']
        real_tar = shutil.which('tar')
        if not real_tar:
            self.skipTest('tar not installed')
        self.script('tar', f'''#!/usr/bin/python3
import subprocess,sys,pathlib
r=subprocess.run([{real_tar!r},*sys.argv[1:]])
pathlib.Path({str(self.config / 'machines.local.json')!r}).write_text('{{}}\\n')
sys.exit(r.returncode)
''')
        self.run_machine('setup', 'studio', '--source', str(source), '--revision', expected, env=env, success=False)
        self.assertFalse((self.root / 'ssh-args.json').exists())

    def test_setup_initializes_config_only_for_new_machine(self):
        source, remote, env = self.prepare_setup()
        self.run_machine('setup', 'studio', '--source', str(source), env=env)
        self.assertEqual((remote / '.config/hgs/config').read_text(), 'HGS_SELF="studio"\n')

    def test_failed_build_keeps_installed_binary_and_cleans_staging(self):
        source, remote, env = self.prepare_setup()
        (remote / '.local/bin').mkdir(parents=True, exist_ok=True)
        installed = remote / '.local/bin/hgs'
        installed.write_text('old binary')
        result = self.run_machine('setup', 'studio', '--source', str(source), env=dict(env, MACHINE_BUILD_FAIL='1'), success=False)
        self.assertEqual(result.returncode, 12)
        self.assertEqual(installed.read_text(), 'old binary')
        self.assertEqual(list((remote / '.local/share/hgs').glob('setup.*')), [])
        self.assertFalse((remote / '.config/hgs/config').exists())


if __name__ == '__main__':
    unittest.main()
