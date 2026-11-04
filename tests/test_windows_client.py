#!/usr/bin/env python3
"""hgs.exe as a Windows control console, against fake and loopback SSH peers.

Runs on Windows only. HGS_TEST_BIN names hgs.exe and HGS_TEST_FAKE_SSH the
fake client built by `cargo build --locked --example fake_ssh`. Every test
points HOME and USERPROFILE at a temporary directory and injects the fake
through HGS_SSH, so no real SSH configuration, key, agent or peer is used.
"""
import ctypes
import json
import os
from pathlib import Path
import queue
import re
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import threading
import time
import unittest

REPO = Path(__file__).resolve().parents[1]
WINDOWS = os.name == 'nt'
HGS = Path(os.environ.get('HGS_TEST_BIN', REPO / 'target/debug/hgs.exe')).resolve()
FAKE_SSH = Path(os.environ.get('HGS_TEST_FAKE_SSH', REPO / 'target/debug/examples/fake_ssh.exe')).resolve()
LOCAL = ('hgs: Local sessions are not enabled on this computer; enable them on the Machines page '
         'or run hgs session-host install, or choose a remote machine.\n')
URI = 'zerus://session?machine=11111111-2222-4333-8444-555555555555&host=peer&name=claude%2Fp&conversation=c1'


def project(id, name, machine, path):
    return dict(id=id, name=name, color='#123456', vivid=False,
                folders=[dict(id='folder-' + id, machine=machine, path=path, name='')], sessions=[])


def exited(pid, seconds):
    """Whether the process is gone within the wait."""
    kernel32 = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel32.OpenProcess.restype = ctypes.c_void_p
    handle = kernel32.OpenProcess(0x00100000, False, pid)  # SYNCHRONIZE
    if not handle:
        return True
    try:
        return kernel32.WaitForSingleObject(ctypes.c_void_p(handle), int(seconds * 1000)) == 0
    finally:
        kernel32.CloseHandle(ctypes.c_void_p(handle))


@unittest.skipUnless(WINDOWS, 'hgs.exe control-console checks run on Windows')
class WindowsClient(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        for path in (HGS, FAKE_SSH):
            if not path.is_file():
                raise AssertionError(f'{path} is missing; build hgs and the fake_ssh example first')

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='hgs-windows-', ignore_cleanup_errors=True)
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.home = self.root / 'home'
        self.home.mkdir()
        self.log = self.root / 'ssh.jsonl'
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(('HGS_', 'FAKE_SSH_')) and key not in ('XDG_STATE_HOME', 'TMUX')}
        # The real ~/.ssh and ~/.config/hgs are never read: home is temporary.
        self.env.update(HOME=str(self.home), USERPROFILE=str(self.home), HGS_SSH=str(FAKE_SSH),
                        FAKE_SSH_LOG=str(self.log), HGS_TAB='0', HGS_SELF='win-client', HGS_PEERS='peer')

    def hgs(self, *args, env=None, input=None, timeout=60, **extra):
        environment = dict(env or self.env, **extra)
        return subprocess.run([str(HGS), *args], env=environment, input=input, capture_output=True,
                              text=True, encoding='utf-8', timeout=timeout)

    def ok(self, *args, **kwargs):
        result = self.hgs(*args, **kwargs)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def json(self, *args, **kwargs):
        return json.loads(self.ok(*args, **kwargs).stdout)

    def calls(self):
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text(encoding='utf-8').splitlines()]

    def test_kimi_login_abort_refreshes_only_a_new_verified_grant(self):
        """Exercise the Windows login command, its exit code and catalog cache."""
        node = shutil.which('node')
        fake = Path(os.environ.get('HGS_TEST_FAKE_AGENT', REPO / 'target/debug/examples/fake_agent.exe'))
        self.assertTrue(node and fake.is_file(), 'build fake_agent and provide Node.js for the login fixture')
        folder = self.home / '.local/bin'
        folder.mkdir(parents=True)
        shutil.copy2(fake, folder / 'curl.exe')
        fixture_hgs = folder / 'hgs.exe'
        shutil.copy2(HGS, fixture_hgs)  # Windows searches the executable's folder before System32.
        script = folder / 'login.js'
        script.write_text('''const fs = require('fs'), path = require('path');
if (process.argv[2] === '--version') { console.log('2.4.1'); }
else if (process.argv[2] === 'login') {
  if (process.env.KIMI_FIXTURE_WRITE === '1') {
    const file = path.join(process.env.KIMI_CODE_HOME, 'credentials/kimi-code.json');
    fs.mkdirSync(path.dirname(file), {recursive:true});
    fs.writeFileSync(file, JSON.stringify({access_token:'new-fixture', expires_at:4102444800}));
  }
  console.error('Logged in to managed:kimi-code.');
  console.error('Assertion failed: !(handle->flags & UV_HANDLE_CLOSING), file src\\\\win\\\\async.c, line 76');
  process.exitCode = Number(process.env.KIMI_FIXTURE_EXIT);
} else { process.exitCode = 1; }
''', encoding='utf-8')
        shim = ('@ECHO off\nGOTO start\n:find_dp0\nSET dp0=%~dp0\nEXIT /b\n:start\nSETLOCAL\nCALL :find_dp0\n\n'
                'IF EXIST "%dp0%\\node.exe" (\n  SET "_prog=%dp0%\\node.exe"\n) ELSE (\n  SET "_prog=node"\n'
                '  SET PATHEXT=%PATHEXT:;.JS;=;%\n)\n\n'
                'endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%"  "%dp0%\\login.js" %*\n')
        (folder / 'kimi.cmd').write_text(shim, encoding='utf-8')
        self.env = {key: value for key, value in self.env.items()
                    if key.upper() not in ('PATH', 'NODE_OPTIONS') and not key.upper().startswith(('KIMI_', 'FAKE_AGENT_'))}
        self.env.update(PATH=os.pathsep.join([str(folder), str(Path(node).parent), str(Path(os.environ['SystemRoot']) / 'System32')]),
                        APPDATA=str(self.root / 'appdata'), HGS_CONFIG_DIR=str(self.root / 'config'),
                        HGS_STATE_DIR=str(self.root / 'state'), FAKE_AGENT_RAW='0', FAKE_AGENT_MODES='none',
                        FAKE_AGENT_RESET_MODES='0', FAKE_AGENT_HOOKS='none', FAKE_AGENT_ON_EOF='exit:0')
        config = self.root / 'config'
        config.mkdir()
        (config / 'local-sessions').touch()  # Private account commands; no host is started.
        self.ok('account', 'add', 'kimi-fixture', '--provider', 'kimi')
        credentials = self.root / 'config/accounts/kimi-fixture/credentials/kimi-code.json'
        credentials.parent.mkdir()
        for write, code, identity, expected in [(True, 3, True, 0), (False, 3, True, 3),
                                               (True, 3, False, 3), (True, 1, True, 1), (True, 130, True, 130)]:
            with self.subTest(write=write, code=code, identity=identity):
                credentials.write_text(json.dumps({'access_token':'old-fixture', 'expires_at':4102444800}))
                # The private native curl stand-in answers both reads. No quota is
                # present, and a profile without a stable id verifies nothing.
                self.env.update(KIMI_FIXTURE_WRITE=str(int(write)), KIMI_FIXTURE_EXIT=str(code),
                                FAKE_AGENT_PROMPT=json.dumps({'user_id':'fixture-user'} if identity else {'nickname':'Fixture'}) + r'\n200')
                result = subprocess.run([str(fixture_hgs), 'account', 'login', 'kimi-fixture'], env=self.env,
                                        cwd=folder, input='', capture_output=True, text=True, encoding='utf-8', timeout=30)
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertEqual('Sign-in verified' in result.stderr, expected == 0)
                if expected == 0:
                    row = next(p for p in self.json('account', 'ls')['profiles'] if p['id'] == 'kimi-fixture')
                    self.assertEqual(row['account_status']['auth_status'], 'signed_in')
                    self.assertEqual(row['account_status']['status'], 'unavailable')
                    self.assertEqual(row['account_status']['windows'], [])

    def canned(self, stdout=None, stderr=None, code=0):
        values = dict(FAKE_SSH_MODE='canned', FAKE_SSH_EXIT=str(code))
        for name, text in (('FAKE_SSH_STDOUT', stdout), ('FAKE_SSH_STDERR', stderr)):
            if text is not None:
                path = self.root / name.lower()
                path.write_bytes(text.encode())
                values[name] = str(path)
        return values

    def source(self):
        """A minimal hgs checkout for machine setup."""
        source = self.root / 'source'
        for name, text in {'Cargo.toml': '[package]\n', 'Cargo.lock': '', 'build.rs': 'fn main() {}\n',
                           'src/main.rs': 'fn main() {}\n', 'scripts/tool.py': 'print(1)\n',
                           'install.sh': '#!/usr/bin/env bash\necho installed\n', 'hgs_state.py': 'pass\n'}.items():
            (source / name).parent.mkdir(parents=True, exist_ok=True)
            (source / name).write_bytes(text.encode())
        return source

    def test_the_fake_client_refuses_streams_that_break_windows_openssh(self):
        # Every other test therefore proves that hgs gives ssh.exe only consoles
        # and overlapped pipes, whatever its own streams are: a CI runner's are
        # synchronous pipes or NUL, and scripts redirect files.
        result = subprocess.run([str(FAKE_SSH), 'peer', 'true'], stdin=subprocess.DEVNULL, capture_output=True,
                                text=True, env=self.env)
        self.assertEqual(result.returncode, 254)
        self.assertIn('synchronous pipe for stdout, stderr', result.stderr)
        self.assertIn('stdin is a device', result.stderr)
        log = self.root / 'kept.log'
        log.write_bytes(b'kept\n')
        with open(log, 'rb') as source, open(log, 'ab') as output:
            result = subprocess.run([str(FAKE_SSH), 'peer', 'true'], stdin=source, stdout=output,
                                    stderr=subprocess.PIPE, env=self.env)
        self.assertEqual(result.returncode, 254)
        self.assertIn(b'stdin is a file', result.stderr)
        self.assertIn(b'stdout is a file with 5 bytes', result.stderr)
        self.assertEqual(log.read_bytes(), b'kept\n')
        # `ssh -G` reads no input, and NUL takes output.
        result = subprocess.run([str(FAKE_SSH), '-G', 'peer'], stdin=subprocess.DEVNULL,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=self.env)
        self.assertEqual(result.returncode, 0)

    def test_the_peer_sees_the_end_of_any_input(self):
        # Windows OpenSSH never sends the end of NUL or of a file to the peer, where
        # a command reading its input would wait forever: hgs relays both.
        received, streams = self.root / 'received', self.root / 'streams.jsonl'
        request = self.root / 'request.json'
        request.write_bytes(b'{"text":"from a file"}')
        env = dict(self.env, FAKE_SSH_STDIN=str(received), FAKE_SSH_STREAMS=str(streams),
                   **self.canned(stdout='{"host":"peer"}'))
        relayed = dict(stdin='pipe', stdout='pipe', stderr='pipe')
        with open(request, 'rb') as source:
            for name, stdin, expected in (('NUL', dict(stdin=subprocess.DEVNULL), b''),
                                          ('file', dict(stdin=source), request.read_bytes()),
                                          ('pipe', dict(input=b'{"text":"piped"}'), b'{"text":"piped"}')):
                for args in (['@peer', 'history', 'claude/p', '--json'], ['@peer', 'ls', '--json']):
                    with self.subTest(stdin=name, args=args):
                        source.seek(0)
                        streams.unlink(missing_ok=True)
                        result = subprocess.run([str(HGS), *args], **stdin, capture_output=True, env=env, timeout=30)
                        self.assertEqual(result.returncode, 0, result.stderr)
                        # A listing (`Command::output` on Unix) never passes input on.
                        self.assertEqual(received.read_bytes(), expected if args[1] == 'history' else b'')
                        self.assertEqual(json.loads(streams.read_text()), relayed)

    def test_redirected_output_files_are_appended_to(self):
        # Windows OpenSSH writes a file from offset 0. hgs writes the client's
        # output through its own handle instead, so `>>` keeps what was there.
        out, err = self.root / 'out.log', self.root / 'err.log'
        reply = json.dumps(dict(name='claude/p'))
        canned = self.canned(stdout=reply, stderr='remote note\n', code=3)
        bundle = dict(FAKE_SSH_MODE='bundle', FAKE_SSH_BUNDLE=str(self.root / 'bundle.tgz'))
        for args, extra, code, expected in (
                (['@peer', 'inspect', 'claude/p', '--json'], canned, 3, (reply, 'remote note\n')),
                (['@peer', 'ls'], canned, 3, ('peer:\n' + reply, 'remote note\n')),
                (['machine', 'setup', 'peer', '--source', str(self.source())], bundle, 0,
                 ('Setup complete: hgs fixture\n', ''))):
            with self.subTest(args=args):
                out.write_bytes(b'kept out\n')
                err.write_bytes(b'kept err\n')
                with open(out, 'ab') as stdout, open(err, 'ab') as stderr:
                    result = subprocess.run([str(HGS), *args], stdin=subprocess.DEVNULL, stdout=stdout,
                                            stderr=stderr, env=dict(self.env, **extra), timeout=60)
                self.assertEqual(result.returncode, code, err.read_bytes())
                self.assertEqual((out.read_bytes(), err.read_bytes()),
                                 (b'kept out\n' + expected[0].encode(), b'kept err\n' + expected[1].encode()))

    def test_a_console_reaches_the_client_as_it_is(self):
        # `ssh -t` and Ctrl+C need the console itself. CREATE_NO_WINDOW gives hgs
        # a console of its own without a window, never the desktop's.
        streams = self.root / 'streams.jsonl'
        env = dict(self.env, FAKE_SSH_STREAMS=str(streams), **self.canned())
        for args in (['@peer', 'a', 'claude/p'], ['@peer', 'history', 'claude/p', '--json']):
            with self.subTest(args=args):
                streams.unlink(missing_ok=True)
                result = subprocess.run([str(HGS), *args], env=env, creationflags=subprocess.CREATE_NO_WINDOW,
                                        timeout=60)
                self.assertEqual(result.returncode, 0)
                self.assertEqual(json.loads(streams.read_text()),
                                 dict(stdin='console', stdout='console', stderr='console'))

    def test_version_and_usage(self):
        self.assertRegex(self.ok('--version').stdout, r'^hgs \d+\.\d+\.\d+\n$')
        self.assertIn('usage: hgs', self.ok('help').stdout)
        result = self.hgs()
        self.assertEqual(result.returncode, 1)
        self.assertIn('usage: hgs', result.stderr)

    def test_local_speech_discovery_is_gracefully_unavailable(self):
        for prefix in ([], ['@win-client']):
            with self.subTest(prefix=prefix):
                reply = self.json(*prefix, 'speech', '--json', input='{"action":"providers"}')
                self.assertEqual({key: reply[key] for key in ('schema', 'profiles', 'available')},
                                 dict(schema=1, profiles=[], available=False))
                self.assertIn('remote computer', reply['reason'])
        self.assertEqual(self.calls(), [])

    def test_remote_speech_preserves_json_stdin_and_noninteractive_streams(self):
        received, streams = self.root / 'speech-input.json', self.root / 'speech-streams.jsonl'
        identity = '11111111-2222-4333-8444-555555555555'
        # Cross the pipe buffer boundary without a microphone or real SSH peer.
        payload = json.dumps(dict(action='submit', request_id=identity, profile_id='gigaam-ru',
                                  audio=dict(format='pcm_s16le', sample_rate=16000, channels=1,
                                             data_base64='AAAA' * 16000)))
        reply = dict(schema=1, job_id=identity, state='queued')
        result = self.json('@peer', 'speech', '--json', input=payload,
                           FAKE_SSH_STDIN=str(received), FAKE_SSH_STREAMS=str(streams),
                           **self.canned(stdout=json.dumps(reply)))
        self.assertEqual(result, reply)
        self.assertEqual(received.read_bytes(), payload.encode())
        self.assertEqual(json.loads(streams.read_text()), dict(stdin='pipe', stdout='pipe', stderr='pipe'))
        self.assertEqual(self.calls(), [['-o', 'BatchMode=yes', '-o', 'ConnectTimeout=3',
                                        'peer', '~/.local/bin/hgs speech --json']])

    def test_targeted_help_reads_the_remote_speech_capability(self):
        legacy = 'usage: hgs [@host] ls\n'
        for command in ('--help', '-h', 'help'):
            with self.subTest(command=command):
                self.assertEqual(self.ok('@peer', command, **self.canned(stdout=legacy)).stdout, legacy)
                self.assertEqual(self.calls()[-1], ['-o', 'BatchMode=yes', '-o', 'ConnectTimeout=3',
                                                   'peer', '~/.local/bin/hgs --help'])
        self.assertIn('speech --json', self.ok('help').stdout)
        self.assertIn('speech --json', self.ok('@win-client', '--help').stdout)
        self.assertEqual(len(self.calls()), 3)

    def test_local_session_commands_exit_with_code_2(self):
        for args in (['a', 'claude/p'], ['kill', 'claude/p'], ['send', 'claude/p', '--json'],
                     ['inspect', 'claude/p'], ['history', 'claude/p', '--json'], ['claude'],
                     ['codex', str(self.root), '--new'], ['resume', '--all'], ['pause', '--all'],
                     ['fork', 'claude/p'], ['reference', 'claude/p', '--json'], ['notify', 'claude/p'],
                     ['clip'], ['paste', 'claude/p', '%1'], ['__dsh-ui-url', 'dsh/p'], ['__state', 'list'],
                     ['--run', '0', 'cmd'], ['--dry-run', 'claude', 'p'], ['@win-client', 'a', 'claude/p'],
                     ['account', 'add', 'work', '--provider', 'codex', '--label', 'Work'],
                     ['account', 'default', 'native-codex'], ['account', 'login', 'native-codex'],
                     ['account', 'inspect', 'native-codex'], ['account', 'install', 'codex'],
                     ['account', 'copy', 'work', '--to', 'peer', '--as', 'copy'],
                     ['account', 'copy', 'work', '--from', 'peer', '--to', '@local', '--as', 'copy'], ['dirs']):
            with self.subTest(args=args):
                result = self.hgs(*args, input='{}')
                self.assertEqual((result.returncode, result.stderr), (2, LOCAL), result.stdout)
        for args, message in ((['native-ui', 'dsh/p'], 'native DeepSeek UI'), (['a', 'dsh/p'], 'native DeepSeek UI'),
                              (['account', 'login', 'native-dsh'], 'native DeepSeek UI'),
                              (['@peer', 'native-ui', 'dsh/p', '--json'], 'native DeepSeek UI'),
                              (['mobile-peers', '--json'], 'One-hop mobile peer routing'),
                              (['swarm', 'mobile-peer', '--json'], 'One-hop mobile peer routing')):
            with self.subTest(args=args):
                result = self.hgs(*args, input='{}')
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn(message, result.stderr)
        self.assertEqual(self.calls(), [])

    def test_repository_preparation_reaches_validation_without_a_session_host(self):
        request = '11111111-2222-4333-8444-555555555555'
        common = ['--url', 'https://example.invalid/team/repository.git',
                  '--parent', str(self.root), '--name', 'NUL', '--json']
        for action in ('check', 'clone'):
            with self.subTest(action=action):
                args = ['repository', action, *common]
                if action == 'clone':
                    args += ['--request-id', request]
                result = self.hgs(*args)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn('reserved Windows device name', result.stderr)
        result = self.hgs('repository', 'status', '--request-id', request, '--json')
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn('Clone receipt unavailable', result.stderr)
        result = self.hgs('--dry-run', 'repository', 'clone', *common, '--request-id', request)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn('Repository operations do not support --dry-run', result.stderr)
        # Public checkout preparation never opens the session-state entry point
        # or enables local agents. Keep both boundaries blocked afterwards.
        for args in (['inspect', 'claude/p'],
                     ['__state', 'repository', 'status', '--request-id', request, '--json']):
            with self.subTest(still_blocked=args):
                result = self.hgs(*args)
                self.assertEqual((result.returncode, result.stderr), (2, LOCAL), result.stdout)
        self.assertEqual(self.calls(), [])

    def test_only_an_isolated_test_host_session_reaches_state_and_run(self):
        # Sessions of an isolated test host run `--run` and `__state` (their
        # hooks) where local sessions are off (src/host/gate.rs): with
        # HGS_HOST_ALLOW_UNSTAGED=1 and an existing HGS_STATE_DIR that is not
        # the profile's default one.
        state, default = self.root / 'state', self.home / '.local' / 'state' / 'hgs' / 'sessions'
        state.mkdir()
        default.mkdir(parents=True)
        session = dict(HGS_SESSION='claude/p/t', HGS_RUN_ID='run')
        isolated = dict(HGS_HOST_ALLOW_UNSTAGED='1', HGS_STATE_DIR=str(state))
        result = self.hgs('__state', 'hook', input='{}', **session, **isolated)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn('hook did not supply a valid session_id', result.stderr)
        # Every other command stays a control console's.
        for args in (['a', 'claude/p'], ['inspect', 'claude/p'], ['kill', 'claude/p'], ['claude']):
            with self.subTest(isolated=args):
                result = self.hgs(*args, input='{}', **isolated)
                self.assertEqual((result.returncode, result.stderr), (2, LOCAL), result.stdout)
        # Without the override, or for any other state directory, nothing opens.
        missing = self.root / 'missing'
        for extra in (dict(HGS_STATE_DIR=str(state)),
                      dict(HGS_HOST_ALLOW_UNSTAGED='true', HGS_STATE_DIR=str(state)),
                      dict(HGS_HOST_ALLOW_UNSTAGED='1'),
                      dict(HGS_HOST_ALLOW_UNSTAGED='1', HGS_STATE_DIR=str(default)),
                      dict(HGS_HOST_ALLOW_UNSTAGED='1', HGS_STATE_DIR=str(missing))):
            for args in (['__state', 'hook'], ['--run', '0', 'cmd', '/c', 'exit', '0']):
                with self.subTest(env=extra, args=args):
                    result = self.hgs(*args, input='{}', **session, **extra)
                    self.assertEqual((result.returncode, result.stderr), (2, LOCAL), result.stdout)
        self.assertFalse(missing.exists())
        self.assertEqual(self.calls(), [])

    def test_local_box_is_control_only(self):
        box = self.json('ls', '--json', '--local')
        self.assertEqual({key: box[key] for key in ('host', 'ok', 'projects', 'sessions', 'peers', 'local_sessions')},
                         dict(host='win-client', ok=True, projects={}, sessions=[], peers=['peer'], local_sessions=False))
        self.assertRegex(box['hgs_version'], r'^\d+\.\d+\.\d+')
        self.assertIn('mobile_delivery', box['preferences'])
        metrics = box['metrics']
        self.assertEqual(metrics['platform'], 'windows')
        self.assertGreater(metrics['memory_total_bytes'], 0)
        self.assertLessEqual(metrics['memory_used_bytes'], metrics['memory_total_bytes'])
        self.assertGreater(metrics['uptime_seconds'], 0)
        self.assertGreaterEqual(metrics['cpu_count'], 1)
        self.assertIsNone(metrics['load_1'])
        self.assertIn(metrics['cpu_state'], ('warming', 'available'))
        # The default layout below the home directory, with Windows separators.
        self.assertTrue((self.home / '.local/state/hgs/sessions/telemetry/cpu.json').is_file())
        time.sleep(0.3)
        self.assertEqual(self.json('ls', '--json', '--local')['metrics']['cpu_state'], 'available')

        self.json('presence', 'set', '--json', input=json.dumps(dict(idle_seconds=3, locked=False)))
        self.assertFalse(self.json('ls', '--json', '--local')['desktop_presence']['locked'])

        env = dict(self.env, HGS_PEERS='')
        del env['HGS_SELF']
        self.assertEqual(self.json('ls', '--json', '--local', env=env)['host'], os.environ['COMPUTERNAME'].lower())
        fleet = self.json('ls', '--json', env=env)
        self.assertEqual([box['ok'] for box in fleet], [True])
        result = self.ok('ls', env=env)
        self.assertEqual(result.stdout, os.environ['COMPUTERNAME'].lower() + ':\n  (control console: sessions run on remote machines)\n')
        self.assertEqual(self.ok('--dry-run', 'ls', '--json', env=env).stdout, '')
        self.assertEqual(self.calls(), [])

    def test_forwarded_commands_keep_the_posix_remote_form(self):
        expect = {
            ('ls', '--json'): 'ssh -o BatchMode=yes -o ConnectTimeout=3 nowhere ~/.local/bin/hgs ls --local --json --dry-run\n',
            ('dirs', '/work/a b'): "ssh -o BatchMode=yes -o ConnectTimeout=3 nowhere ~/.local/bin/hgs dirs '/work/a b' --dry-run\n",
            ('claude', 'sample-project', '-d'): 'ssh -o BatchMode=yes nowhere ~/.local/bin/hgs --client win-client claude sample-project -d --dry-run\n',
            ('inspect', 'claude/p', '--after', '42'): 'ssh -o BatchMode=yes -o ConnectTimeout=3 nowhere ~/.local/bin/hgs inspect claude/p --after 42 --dry-run\n',
        }
        for args, line in expect.items():
            with self.subTest(args=args):
                self.assertEqual(self.ok('--dry-run', '@nowhere', *args).stdout, line)
        # An interactive attach marks a link that came up; cmd.exe runs the marker
        # and reads its path from the client's environment.
        self.assertEqual(self.ok('--dry-run', '@nowhere', 'a', 'claude/p').stdout,
                         """ssh -t -o PermitLocalCommand=yes -o 'LocalCommand=type nul > "%%HGS_LINK_MARKER%%"' """
                         'nowhere ~/.local/bin/hgs --client win-client a claude/p --dry-run\n')
        # A remote folder is a POSIX path, whatever this machine is.
        result = self.ok('--dry-run', 'machine', 'ssh', 'nowhere', '--directory', '/work/a b')
        self.assertEqual(shlex.split(result.stdout)[1:3], ['-t', 'nowhere'])
        self.assertEqual(self.calls(), [])

    def test_peer_listing_reports_offline_bad_and_good_peers(self):
        offline = self.root / 'offline'
        offline.mkdir()
        (offline / 'peer').write_text('')
        box = self.json('@peer', 'ls', '--json', FAKE_SSH_OFFLINE_DIR=str(offline))
        self.assertEqual(box, dict(host='peer', ok=False, error='offline', error_detail='fixture offline', projects={}, sessions=[]))
        refused = 'ssh: connect to host peer port 22: Connection refused\r\n'
        box = self.json('@peer', 'ls', '--json', **self.canned(stderr=refused, code=255))
        self.assertEqual((box['error'], box['error_detail']), ('offline', refused.strip()))
        box = self.json('@peer', 'ls', '--json', **self.canned(stdout='  claude/legacy   0 client(s)\n'))
        self.assertEqual((box['ok'], box['error']), (False, 'bad_response'))
        remote = dict(host='peer-self', ok=True, projects={'p': '/work/p'}, sessions=[dict(name='claude/p')])
        self.assertEqual(self.json('@peer', 'ls', '--json', **self.canned(stdout=json.dumps(remote))), remote)
        argv = self.calls()[-1]
        self.assertEqual(argv[-2:], ['peer', '~/.local/bin/hgs ls --local --json'])
        self.assertEqual(argv[:4], ['-o', 'BatchMode=yes', '-o', 'ConnectTimeout=3'])
        fleet = self.json('ls', '--json', HGS_PEERS='peer other', **self.canned(stdout=json.dumps(remote)))
        self.assertEqual([box['host'] for box in fleet], ['win-client', 'peer-self', 'peer-self'])
        self.assertEqual([call[-2] for call in self.calls()[-2:]], ['peer', 'other'])

    def test_exit_codes_and_link_states_pass_through(self):
        result = self.hgs('@peer', 'send', 'claude/p', '--json', input='{"text":"hi"}', **self.canned(code=7))
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(self.calls()[-1][-1], '~/.local/bin/hgs send claude/p --json')
        result = self.hgs('@peer', 'a', 'claude/p', **self.canned(code=255))
        self.assertEqual((result.returncode, result.stderr), (3, 'hgs: peer unreachable\n'))
        # The peer accepted the connection (LocalCommand ran), then the link died.
        result = self.hgs('@peer', 'a', 'claude/p', FAKE_SSH_LOCAL_COMMAND='1', **self.canned(code=255))
        self.assertEqual((result.returncode, result.stderr),
                         (3, 'hgs: link to peer dropped; the session is still there, rerun to reattach\n'))
        argv = self.calls()[-1]
        self.assertEqual(argv[0], '-t')
        self.assertEqual(argv[-1], '~/.local/bin/hgs --client win-client a claude/p')

    def test_link_marker_survives_percent_signs_in_the_temporary_folder(self):
        # A Windows account name, and so the profile and TEMP paths, may contain
        # '%'. OpenSSH fails on an unknown %-token and cmd.exe expands %NAME%.
        temp = self.root / '50%off %OS% a&b'
        temp.mkdir()
        local = dict(TMP=str(temp), TEMP=str(temp), FAKE_SSH_LOCAL_COMMAND='1')
        result = self.hgs('@peer', 'a', 'claude/p', **local, **self.canned(code=255))
        self.assertEqual((result.returncode, result.stderr),
                         (3, 'hgs: link to peer dropped; the session is still there, rerun to reattach\n'))
        self.assertEqual(self.hgs('@peer', 'a', 'claude/p', **local, **self.canned(code=0)).returncode, 0)
        self.assertEqual(list(temp.iterdir()), [])

    def test_a_killed_attach_leaves_its_link_marker_only_until_the_next_one(self):
        # The desktop app ends an embedded terminal's hgs.exe with its job when the
        # client does not exit on CTRL_CLOSE_EVENT in time; no destructor runs then.
        temp = self.root / 'temp'
        temp.mkdir()
        pid = self.root / 'ssh.pid'
        local = dict(TMP=str(temp), TEMP=str(temp), FAKE_SSH_LOCAL_COMMAND='1')
        env = dict(self.env, FAKE_SSH_MODE='sleep', FAKE_SSH_PID=str(pid), FAKE_SSH_SECONDS='60', **local)
        process = subprocess.Popen([str(HGS), '@peer', 'a', 'claude/p', '--existing'], env=env,
                                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            deadline = time.monotonic() + 20
            while not (pid.exists() and pid.read_text() and list(temp.glob('hgs-link-*/up'))) \
                    and time.monotonic() < deadline:
                time.sleep(0.1)
            child = int(pid.read_text())
        finally:
            process.kill()
            process.wait(timeout=10)
        self.assertTrue(exited(child, 5))
        left = list(temp.glob('hgs-link-*'))
        self.assertEqual([path.name.split('-')[2] for path in left], [str(process.pid)])
        # The next interactive link removes it along with its own.
        self.assertEqual(self.hgs('@peer', 'a', 'claude/p', **local, **self.canned(code=0)).returncode, 0)
        self.assertEqual(list(temp.iterdir()), [])

    def test_relayed_output_waits_for_a_late_reader(self):
        # The client exits at once; this reader takes the output only after the
        # relay's idle limit (5 s). Unix ssh would block on its inherited stream,
        # so nothing may be lost here either.
        reply = json.dumps(dict(name='claude/p', lines=[f'line {n:05d} ' + 'x' * 60 for n in range(600)]))
        self.assertGreater(len(reply), 30_000)
        process = subprocess.Popen([str(HGS), '@peer', 'inspect', 'claude/p', '--json'],
                                   env=dict(self.env, **self.canned(stdout=reply)), stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            time.sleep(8)
            stdout, stderr = process.communicate(timeout=60)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()
        self.assertEqual(process.returncode, 0, stderr)
        self.assertEqual(stdout.decode(), reply)

    def test_a_helper_holding_the_relay_open_does_not_keep_hgs_waiting(self):
        # A helper the client leaves behind (a connection multiplexer) keeps the
        # pipe open without writing: hgs relays everything and then stops waiting.
        reply = json.dumps(dict(name='claude/p'))
        helper = self.root / 'helper.pid'
        started = time.monotonic()
        result = self.hgs('@peer', 'inspect', 'claude/p', '--json', FAKE_SSH_HELPER_PID=str(helper),
                          **self.canned(stdout=reply))
        self.assertEqual((result.returncode, result.stdout), (0, reply), result.stderr)
        self.assertLess(time.monotonic() - started, 30)
        self.assertTrue(exited(int(helper.read_text()), 10), 'the job object should end the helper with hgs')

    def test_session_references_route_to_their_peer(self):
        reply = json.dumps(dict(name='claude/p'))
        self.assertEqual(self.json('@peer', 'inspect', URI, **self.canned(stdout=reply)), dict(name='claude/p'))
        self.assertEqual(shlex.split(self.calls()[-1][-1]), ['~/.local/bin/hgs', 'inspect', URI, '--local'])
        # Without @host the reference names its configured computer. Resolving
        # it records this machine's identity in the private swarm store.
        self.assertEqual(self.json('inspect', URI, **self.canned(stdout=reply)), dict(name='claude/p'))
        self.assertEqual(self.calls()[-1][-2:], ['peer', f"~/.local/bin/hgs inspect '{URI}' --local"])
        self.assertTrue((self.home / '.config/hgs/swarm/catalog.json').is_file())
        result = self.hgs('inspect', URI, HGS_PEERS='')
        self.assertEqual(result.returncode, 1)
        self.assertIn('not configured here', result.stderr)
        result = self.hgs('inspect', URI, '--local')
        self.assertEqual(result.returncode, 1)
        self.assertIn('belongs to another computer', result.stderr)

    def test_private_configuration_lives_below_the_home_directory(self):
        config = self.home / '.config/hgs'
        profile = dict(hostname='peer.example', user='me', port=2222, identity_file='~/.ssh/id_ed25519')
        self.json('machine', 'set', 'peer', '--json', json.dumps(profile))
        self.assertTrue((config / 'machines.local.json').is_file())
        listing = self.json('machine', 'ls')
        self.assertEqual((listing['local'], listing['machines'][0]['alias']), ('win-client', 'peer'))
        self.json('@peer', 'ls', '--json', **self.canned(stdout='{"host":"peer"}'))
        argv = self.calls()[-1]
        self.assertEqual(argv[argv.index('-i') + 1], str(self.home / '.ssh' / 'id_ed25519'))
        self.assertEqual(argv[argv.index('-p') + 1], '2222')
        for key in (r'C:\keys\id_ed25519', r'~\.ssh\id_rsa'):
            with self.subTest(key=key):
                self.json('machine', 'set', 'other', '--json', json.dumps(dict(profile, identity_file=key)))
        result = self.hgs('machine', 'set', 'other', '--json', json.dumps(dict(profile, identity_file='keys/id')))
        self.assertEqual(result.returncode, 1)
        self.json('machine', 'remove', 'other')

        folder = self.root / 'a project'
        folder.mkdir()
        self.ok('project', 'add', 'work', str(folder))
        projects = self.json('project', 'ls', '--json')
        self.assertEqual([(p['name'], p['dir'], p['exists']) for p in projects], [('work', str(folder), True)])
        self.ok('project', 'rm', 'work')
        self.assertEqual(self.json('project', 'ls', '--json'), [])
        self.assertEqual(sorted(path.name for path in config.iterdir() if path.name.startswith('.tmp')), [])

    def test_machine_resolution_is_bounded_and_setup_uploads_a_posix_bundle(self):
        resolved = 'hostname peer.example\nuser me\nport 22\nidentityfile ~/.ssh/id_ed25519\n'
        result = self.json('machine', 'resolve', 'peer', **self.canned(stdout=resolved))
        self.assertEqual(result['effective']['hostname'], 'peer.example')
        self.assertEqual(self.calls()[-1][-3:], ['-G', '-T', 'peer'])
        started = time.monotonic()
        pid = self.root / 'resolve.pid'
        result = self.hgs('machine', 'resolve', 'peer', FAKE_SSH_MODE='sleep', FAKE_SSH_PID=str(pid))
        self.assertEqual(result.returncode, 1)
        self.assertIn('timed out', result.stderr)
        self.assertLess(time.monotonic() - started, 15)
        self.assertTrue(exited(int(pid.read_text()), 5))

        source = self.source()
        bundle = self.root / 'bundle.tgz'
        result = self.ok('machine', 'setup', 'peer', '--source', str(source), FAKE_SSH_MODE='bundle', FAKE_SSH_BUNDLE=str(bundle))
        self.assertIn('Setup complete', result.stdout)
        self.assertTrue(self.calls()[-1][-1].startswith("sh -c 'set -eu"))
        with tarfile.open(bundle) as archive:
            # The private source snapshot is archived as `-C snapshot .`.
            members = {member.name.removeprefix('./'): member for member in archive.getmembers()}
            names = set(members)
            self.assertTrue({'Cargo.toml', 'src/main.rs', 'scripts/tool.py', 'install.sh', 'hgs_state.py'} <= names, names)
            self.assertFalse([name for name in names if '\\' in name])
            self.assertEqual(archive.extractfile(members['install.sh']).read(), (source / 'install.sh').read_bytes())
        (source / 'install.sh').write_bytes(b'#!/usr/bin/env bash\r\necho installed\r\n')
        calls = len(self.calls())
        result = self.hgs('machine', 'setup', 'peer', '--source', str(source), FAKE_SSH_MODE='bundle', FAKE_SSH_BUNDLE=str(bundle))
        self.assertEqual(result.returncode, 1)
        self.assertIn('Windows line endings', result.stderr)
        self.assertEqual(len(self.calls()), calls)

    def test_accounts_are_remote_and_copies_run_between_peers(self):
        for args in (['account', 'ls'], ['account', 'ls', '--json'], ['account', 'ls', 'x'],
                     ['account', 'default'], ['account']):
            with self.subTest(args=args):
                catalog = self.json(*args)
                self.assertEqual((catalog['host'], catalog['profiles'], catalog['defaults']), ('win-client', [], {}))
        # The export answers with this payload; the import reads it and answers the same.
        receipt = dict(ok=True, id='copy', provider='codex', host='other')
        received = self.root / 'import-stdin'
        result = self.json('account', 'copy', 'work', '--from', 'peer', '--to', 'other', '--as', 'copy',
                           HGS_PEERS='peer other', FAKE_SSH_STDIN=str(received),
                           **self.canned(stdout=json.dumps(receipt)))
        self.assertEqual(result, receipt)
        self.assertEqual(json.loads(received.read_text()), receipt)
        self.assertEqual([call[-2:] for call in self.calls()],
                         [['peer', '~/.local/bin/hgs account _export work'],
                          ['other', '~/.local/bin/hgs account _import copy --label copy']])

    def test_swarm_synchronizes_isolated_nodes_over_ssh(self):
        nodes = self.root / 'nodes'
        for node, peers in (('a', 'b'), ('b', 'a c'), ('c', 'b')):
            (nodes / node).mkdir(parents=True)
            (nodes / node / 'config').write_text(f'HGS_SELF="{node}"\nHGS_PEERS="{peers}"\n')
        env = {key: value for key, value in self.env.items() if key not in ('HGS_SELF', 'HGS_PEERS')}
        env.update(FAKE_SSH_MODE='loopback', FAKE_SSH_HGS=str(HGS), FAKE_SSH_ROOT=str(nodes))

        def call(node, *args, data=None):
            return self.json('swarm', *args, env=env, input=None if data is None else json.dumps(data),
                             HGS_CONFIG_DIR=str(nodes / node), HGS_STATE_DIR=str(nodes / node / 'state'))

        for node in 'abc':
            call(node, 'initialize', data=dict(projects=[project(node, 'Shared ' + node, node, '/' + node)]))
        for node, peer in (('a', 'b'), ('b', 'a'), ('c', 'b'), ('b', 'c')):
            preview = call(node, 'preview', peer)
            call(node, 'join', peer, data=dict(node_id=preview['node_id'], swarm_id=preview['swarm_id'],
                                                 project_map={}, prefer_peer_memberships=False))
        for node in 'abcbab':
            call(node, 'sync')
        snapshots = [call(node, 'get') for node in 'abc']
        self.assertEqual(len({snapshot['digest'] for snapshot in snapshots}), 1)
        self.assertEqual(snapshots[0]['worker'], dict(running=False, current=False))
        preference = call('c', 'preference', 'set', 'mobile_delivery', 'away_5', '--json')
        self.assertEqual(preference['mobile_delivery'], 'away_5')
        for node in 'cba':
            call(node, 'sync')
        self.assertEqual(call('a', 'preference', 'get', '--json')['mobile_delivery'], 'away_5')

    def test_swarm_worker_holds_its_lock_on_windows(self):
        env = dict(self.env, HGS_PEERS='')
        worker = subprocess.Popen([str(HGS), 'swarm', 'worker'], env=env, stdout=subprocess.DEVNULL,
                                  stderr=subprocess.PIPE, text=True, encoding='utf-8')
        try:
            deadline = time.monotonic() + 20
            status = None
            while time.monotonic() < deadline:
                status = self.json('swarm', 'get', env=env)['worker']
                if status == dict(running=True, current=True):
                    break
                time.sleep(0.2)
            self.assertEqual(status, dict(running=True, current=True))
            second = self.hgs('swarm', 'worker', env=env, timeout=30)
            self.assertNotEqual(second.returncode, 0)
            self.assertIn('already running', second.stderr)
        finally:
            worker.kill()
            worker.communicate(timeout=10)
        self.assertEqual(self.json('swarm', 'get', env=env)['worker'], dict(running=False, current=False))

    def test_a_watch_stream_runs_both_ways_until_its_input_ends(self):
        # `hgs @peer watch --json` holds stdin open for hours: control lines and
        # frames must pass as they are written, not when input ends, and the end
        # of input must reach the peer's watcher (zerus-604d.9). A desktop client
        # gives hgs synchronous pipes, which hgs relays rather than pass on.
        streams = self.root / 'streams.jsonl'
        env = dict(self.env, FAKE_SSH_MODE='stream', FAKE_SSH_STREAMS=str(streams))
        process = subprocess.Popen([str(HGS), '@peer', 'watch', '--json'], env=env, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        lines = queue.Queue()
        reader = threading.Thread(target=lambda: [lines.put(line) for line in process.stdout] + [lines.put(None)], daemon=True)
        reader.start()
        def frame(seconds=10):
            line = lines.get(timeout=seconds)
            if line is None:
                # Read only once hgs ended: stderr stays open while it runs.
                self.fail('the stream ended early: ' + process.stderr.read().decode(errors='replace'))
            return json.loads(line)
        try:
            self.assertEqual(frame(20), {'t': 'hello'})
            for index in range(3):
                started = time.monotonic()
                process.stdin.write(b'{"op":"ping","id":%d}\n' % index)
                process.stdin.flush()
                self.assertEqual(frame(), {'t': 'echo', 'line': '{"op":"ping","id":%d}' % index})
                self.assertLess(time.monotonic() - started, 5)
            process.stdin.close()
            self.assertEqual(frame(), {'t': 'end'})
            self.assertEqual(process.wait(timeout=10), 0)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=10)
            reader.join(timeout=10)
            for pipe in (process.stdin, process.stdout, process.stderr):
                if not pipe.closed:
                    pipe.close()
        self.assertEqual(json.loads(streams.read_text().splitlines()[-1]), dict(stdin='pipe', stdout='pipe', stderr='pipe'))
        argv = self.calls()[-1]
        self.assertIn('ServerAliveInterval=10', argv)
        remote = argv[-1]
        self.assertIn('Event stream ABI: watch-v1', remote)
        self.assertIn('exec ~/.local/bin/hgs watch --json', remote)

    def test_a_stopped_hgs_takes_its_ssh_client_along(self):
        pid = self.root / 'ssh.pid'
        env = dict(self.env, FAKE_SSH_MODE='sleep', FAKE_SSH_PID=str(pid), FAKE_SSH_SECONDS='60')
        process = subprocess.Popen([str(HGS), '@peer', 'ls', '--json'], env=env,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            deadline = time.monotonic() + 20
            while not (pid.exists() and pid.read_text()) and time.monotonic() < deadline:
                time.sleep(0.1)
            child = int(pid.read_text())
            self.assertFalse(exited(child, 0.2))
        finally:
            process.kill()  # TerminateProcess, as the desktop app stops a stuck poll
            process.wait(timeout=10)
        self.assertTrue(exited(child, 5), 'the job object should end the SSH client with hgs')


if __name__ == '__main__':
    unittest.main()
