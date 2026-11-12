"""Peer commands keep their meaning in every login shell, including fish.

ssh hands its last argument to the peer's login shell as `<shell> -c <command>`.
POSIX snippets such as `if ...; then ...; fi` and `${SHELL:-/bin/sh}` are syntax
errors in fish, and fish also reads `\\\\` and `\\'` inside single quotes.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
HGS = Path(os.environ.get('HGS_TEST_BIN', ROOT / 'target/debug/hgs'))

FAKE_SSH = r'''#!/usr/bin/env python3
import json, os, sys
fixture = os.environ['REMOTE_FIXTURE']
with open(os.path.join(fixture, 'ssh-args.json'), 'w') as file:
    json.dump(sys.argv[1:], file)
shell = os.environ['REMOTE_LOGIN_SHELL']
env = dict(os.environ, HOME=os.path.join(fixture, 'peer'), SHELL=os.path.join(fixture, 'peer-shell'))
os.execve(shell, [shell, '-c', sys.argv[-1]], env)
'''

# The peer's hgs lists every capability and records its exact arguments and input.
PEER_HGS = r'''#!/bin/sh
case "$1" in
--help)
  printf '%s\n' 'hgs [@host] wait' '  session-notifications-v1' 'hooks-health-v1' \
    'hgs [@host] analytics --json' 'hgs [@host] guide' 'hgs [@host] status'
  exit 0;;
--version) echo 'hgs 9.9.9'; exit 0;;
esac
python3 -c 'import json, os, sys; json.dump(sys.argv[1:], open(os.path.expanduser("~/argv.json"), "w"))' "$@"
cat > "$HOME/stdin"
printf '{"ok":true,"services":{}}\n'
'''

# What `exec "${SHELL:-/bin/sh}" -l` starts on the peer.
PEER_SHELL = r'''#!/bin/sh
pwd > "$HOME/shell-pwd"
printf '%s\n' "$@" > "$HOME/shell-args"
'''

PEER_TOOL = r'''#!/bin/sh
if [ "$1" = -V ]; then echo 'tmux 3.5'; fi
'''

TRICKY = "it's a\\b \\' \\\\ $HOME \"q\" ; echo bad `x` \\"


def login_shells():
    shells = {'sh': '/bin/sh'}
    for name in ('bash', 'zsh'):
        if path := shutil.which(name):
            shells[name] = path
    if fish := os.environ.get('HGS_TEST_FISH') or shutil.which('fish'):
        shells['fish'] = fish
    return shells


@unittest.skipIf(os.name == 'nt', 'Peers are Linux and macOS computers')
class RemoteLoginShells(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='zerus-remote-shells-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.peer = self.root / 'peer'
        for path, text in ((self.root / 'fake-ssh', FAKE_SSH), (self.peer / '.local/bin/hgs', PEER_HGS),
                           (self.root / 'peer-shell', PEER_SHELL), (self.peer / '.cargo/bin/tmux', PEER_TOOL)):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
            path.chmod(0o755)
        home = self.root / 'home'
        (home / '.config/hgs').mkdir(parents=True)
        (home / '.config/hgs/config').write_text('HGS_SELF="test-local"\nHGS_PEERS="peer"\n')
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith('HGS_') and key not in ('TMUX', 'TMUX_PANE', 'SSH_AUTH_SOCK')}
        self.env.update(HOME=str(home), HGS_CONFIG_DIR=str(home / '.config/hgs'), HGS_STATE_DIR=str(self.root / 'state'),
                        HGS_SSH=str(self.root / 'fake-ssh'), HGS_TAB='0', REMOTE_FIXTURE=str(self.root))

    def hgs(self, shell, *args, body=''):
        return subprocess.run([str(HGS), *args], input=body, text=True, capture_output=True, timeout=30,
                              env=dict(self.env, REMOTE_LOGIN_SHELL=shell))

    def received(self):
        argv = json.loads((self.peer / 'argv.json').read_text())
        stdin = (self.peer / 'stdin').read_text()
        for name in ('argv.json', 'stdin'):
            (self.peer / name).unlink()
        return argv, stdin

    def test_fish_is_present_where_ci_requires_it(self):
        if os.environ.get('GITHUB_ACTIONS') == 'true' and sys.platform.startswith('linux'):
            self.assertIn('fish', login_shells())
        elif 'fish' not in login_shells():
            self.skipTest('fish is not installed; set HGS_TEST_FISH to run the fish cases')

    def test_capability_probes_run_under_sh_and_keep_json_input(self):
        body = json.dumps(dict(request_id='r-1', text=TRICKY))
        cases = [(('@peer', 'wait', 'ack', '--json'), ['wait', 'ack', '--json']),
                 (('@peer', 'notifications', '--json'), ['notifications', '--json']),
                 (('@peer', 'hooks', 'claude/example/task', '--json'), ['hooks', 'claude/example/task', '--json']),
                 (('@peer', 'analytics', '--json'), ['analytics', '--json', '--read-only']),
                 (('@peer', 'guide'), ['guide'])]
        for name, shell in login_shells().items():
            for args, expected in cases:
                with self.subTest(shell=name, command=args[1]):
                    result = self.hgs(shell, *args, body=body)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(self.received(), (expected, body))

    def test_path_info_reaches_only_a_peer_whose_help_names_it(self):
        # An older hgs would take the unknown verb as a session launch (zerus-5wmh.13.18).
        executable = self.peer / '.local/bin/hgs'
        path = '/srv/' + TRICKY
        for name, shell in login_shells().items():
            with self.subTest(shell=name):
                executable.write_text(PEER_HGS)
                old = self.hgs(shell, '@peer', 'path-info', '--path', path)
                self.assertNotEqual(old.returncode, 0)
                self.assertIn('update the selected computer', old.stderr.lower())
                self.assertFalse((self.peer / 'argv.json').exists())
                executable.write_text(PEER_HGS.replace("'hooks-health-v1'", "'hooks-health-v1' 'hgs [@host] path-info --path <path>'"))
                result = self.hgs(shell, '@peer', 'path-info', '--path', path)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.received(), (['path-info', '--path', path], ''))
                # ~ arrives unexpanded: the peer's hgs expands ~/ and refuses ~user.
                for tilde in ("~/plan's notes.md", '~other/plan.md'):
                    self.assertEqual(self.hgs(shell, '@peer', 'path-info', '--path', tilde).returncode, 0)
                    self.assertEqual(self.received(), (['path-info', '--path', tilde], ''))
                self.assertNotIn('-t', json.loads((self.root / 'ssh-args.json').read_text()))

    def test_a_guarded_path_info_needs_both_markers_and_keeps_its_tilde(self):
        # --expect-machine wraps the actual command; guarded-create-v1 alone
        # never authorizes path-info (zerus-3skhm), and the wrapped path's
        # ~ arrives as written too (zerus-ie12o).
        machine = '22222222-2222-4222-8222-222222222222'
        executable = self.peer / '.local/bin/hgs'
        for name, shell in login_shells().items():
            with self.subTest(shell=name):
                executable.write_text(PEER_HGS.replace("'hooks-health-v1'", "'hooks-health-v1' 'guarded-create-v1'"))
                old = self.hgs(shell, '@peer', '--expect-machine', machine, 'path-info', '--path', '~/plan.md')
                self.assertNotEqual(old.returncode, 0)
                self.assertIn('update the selected computer', old.stderr.lower())
                self.assertFalse((self.peer / 'argv.json').exists())
                executable.write_text(PEER_HGS.replace("'hooks-health-v1'", "'hooks-health-v1' 'hgs [@host] path-info --path <path>'"))
                unguarded = self.hgs(shell, '@peer', '--expect-machine', machine, 'path-info', '--path', '~/plan.md')
                self.assertNotEqual(unguarded.returncode, 0, "a peer without guarded-create-v1 cannot check the machine")
                self.assertFalse((self.peer / 'argv.json').exists())
                executable.write_text(PEER_HGS.replace("'hooks-health-v1'", "'hooks-health-v1' 'guarded-create-v1' 'hgs [@host] path-info --path <path>'"))
                for path in ('~/plan.md', "~/plan's notes.md", '~other/plan.md', '/srv/' + TRICKY):
                    result = self.hgs(shell, '@peer', '--expect-machine', machine, 'path-info', '--path', path)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    argv = self.received()[0]
                    self.assertEqual((argv[:2], argv[-3:]), (['--expect-machine', machine], ['path-info', '--path', path]))
                self.assertNotIn('-t', json.loads((self.root / 'ssh-args.json').read_text()))

    def test_plain_commands_deliver_quotes_and_backslashes_exactly(self):
        for name, shell in login_shells().items():
            with self.subTest(shell=name):
                result = self.hgs(shell, '@peer', 'search', TRICKY)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.received()[0], ['search', TRICKY])
                version = self.hgs(shell, '@peer', 'version')
                self.assertEqual((version.returncode, version.stdout), (0, 'hgs 9.9.9\n'), version.stderr)

    def test_guarded_creation_requires_exact_peer_capability_and_keeps_identity(self):
        identity='11111111-1111-4111-8111-111111111111'
        for name,shell in login_shells().items():
            with self.subTest(shell=name):
                executable=self.peer/'.local/bin/hgs'
                executable.write_text(PEER_HGS)
                old=self.hgs(shell,'@peer','--expect-machine',identity,'dirs',TRICKY)
                self.assertNotEqual(old.returncode,0);self.assertIn('update the selected computer',old.stderr)
                self.assertFalse((self.peer/'argv.json').exists())
                executable.write_text(PEER_HGS.replace("'hooks-health-v1'","'hooks-health-v1' 'guarded-create-v1'"))
                result=self.hgs(shell,'@peer','--expect-machine',identity,'dirs',TRICKY)
                self.assertEqual(result.returncode,0,result.stderr)
                self.assertEqual(self.received(),(['--expect-machine',identity,'--client','test-local','dirs',TRICKY],''))
                options=json.loads((self.root/'ssh-args.json').read_text())
                self.assertIn('BatchMode=yes',options);self.assertNotIn('-t',options)

    def test_a_session_id_reaches_only_a_peer_that_advertises_the_selector(self):
        # zerus-ucck P1: an older peer would read --session-id as a session name,
        # so it never sees the flag; a current peer receives it unchanged.
        identity = '22222222-2222-4222-8222-222222222222'
        for name, shell in login_shells().items():
            with self.subTest(shell=name):
                executable = self.peer / '.local/bin/hgs'
                executable.write_text(PEER_HGS)
                old = self.hgs(shell, '@peer', 'inspect', '--session-id', identity)
                self.assertNotEqual(old.returncode, 0)
                self.assertIn('update the selected computer to select sessions by ID', old.stderr)
                self.assertFalse((self.peer / 'argv.json').exists())
                for invalid in (['--session-id'], ['--session-id', 'NOT-A-UUID'],
                                ['--session-id', identity, '--session-id', identity]):
                    refused = self.hgs(shell, '@peer', 'inspect', *invalid)
                    self.assertEqual(refused.returncode, 2, refused.stderr)
                    self.assertFalse((self.peer / 'argv.json').exists())
                executable.write_text(PEER_HGS.replace("'hooks-health-v1'", "'hooks-health-v1' '  session-id-selector-v1'"))
                result = self.hgs(shell, '@peer', 'inspect', '--session-id', identity)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.received()[0], ['inspect', '--session-id', identity])

    def test_every_selector_call_negotiates_the_selector_with_its_peer(self):
        # #1355 review: every session command, a guarded call that wraps one and
        # the native UI's own peer call require the selector; a peer that only
        # lists what the call needed before never receives the flag.
        import re
        identity = '22222222-2222-4222-8222-222222222222'
        machine = '11111111-1111-4111-8111-111111111111'
        table = re.search(r'const SESSION_COMMANDS: &\[&str\] = &\[(.*?)\];', (ROOT / 'src/cli.rs').read_text(), re.S)
        commands = re.findall(r'"([a-z_-]+)"', table.group(1))
        self.assertIn('terminate', commands)
        self.assertIn('send-now', commands)
        # A remote lifecycle receipt read is refused unless a guarded call wraps it.
        calls = [['@peer', command, '--session-id', identity] for command in commands if command != 'session-action-status']
        calls.append(['@peer', '--expect-machine', machine, 'inspect', '--session-id', identity])
        calls.append(['@peer', '--expect-machine', machine, 'session-action-status', '--session-id', identity, '--json'])
        calls.append(['@peer', 'native-ui', '--session-id', identity, '--json'])
        # The DeepSeek attach alias opens the same native UI (#1355 review R3).
        calls.append(['@peer', 'a', 'dsh/p/example', '--session-id', identity])
        # zerus-ucck P1c: a reference to the peer's session, with @peer and
        # routed by the reference's own host.
        reference = ('zerus://session?machine=33333333-3333-4333-8333-333333333333&host=peer'
                     '&name=codex%2Fp%2Fa&conversation=c&run=r')
        calls.append(['@peer', 'send', reference, '--json'])
        calls.append(['routed', 'pause', reference])
        executable = self.peer / '.local/bin/hgs'
        older = PEER_HGS.replace("'hooks-health-v1'", "'hooks-health-v1' 'Desktop creation ABI: guarded-create-v1' '  lifecycle-receipt-status-v1'")
        executable.write_text(older)
        def run(call):
            return self.hgs('/bin/sh', *(call[1:] if call[0] == 'routed' else call))
        for call in calls:
            with self.subTest(call=call[1:3], peer='older'):
                refused = run(call)
                self.assertNotEqual(refused.returncode, 0, refused.stdout)
                self.assertIn('update the selected computer to select sessions by ID', refused.stderr)
                self.assertFalse((self.peer / 'argv.json').exists())
        # A current peer lists every capability these calls need, including
        # keep-warm's own (#1686, zerus-on03r).
        executable.write_text(older.replace("'  lifecycle-receipt-status-v1'",
                                            "'  lifecycle-receipt-status-v1' '  session-id-selector-v1' '  session-reference-selector-v1'"
                                            " '  session-keep-warm-v1' '  session-question-routing-v1'"))
        for call in calls:
            with self.subTest(call=call[1:3], peer='current'):
                run(call)
                argv = self.received()[0]
                if call[1] == 'native-ui':
                    self.assertEqual(argv, ['__dsh-ui-url', '--session-id', identity])
                elif call[1:3] == ['a', 'dsh/p/example']:
                    self.assertEqual(argv, ['__dsh-ui-url', 'dsh/p/example', '--session-id', identity])
                elif call[1] == '--expect-machine':
                    self.assertEqual(argv[:2], call[1:3], argv)
                    self.assertEqual(argv[-len(call[3:]):], call[3:], argv)
                else:
                    if reference in call:
                        # The one verified hop is guarded by the reference's machine.
                        self.assertEqual(argv[:2], ['--expect-machine', '33333333-3333-4333-8333-333333333333'], argv)
                    self.assertEqual(argv[-len(call[1:]):], call[1:], argv)

    def test_a_reference_takes_exactly_one_verified_hop(self):
        # #1376 review: a reference reaches its computer through one SSH hop the
        # peer verifies; a guarded call never forwards a reference again.
        machine = '33333333-3333-4333-8333-333333333333'
        reference = (f'zerus://session?machine={machine}&host=peer&name=codex%2Fp%2Fa&conversation=c&run=r')
        executable = self.peer / '.local/bin/hgs'
        executable.write_text(PEER_HGS.replace("'hooks-health-v1'", "'hooks-health-v1' '  session-reference-selector-v1'"))
        dry = self.hgs('/bin/sh', '--dry-run', 'kill', reference)
        self.assertEqual(dry.returncode, 0, dry.stderr)
        self.assertIn(f'--expect-machine {machine} kill', dry.stdout)
        self.assertNotIn(' -t ', dry.stdout, 'the hop keeps the SSH options of the command')
        local = json.loads((self.root / 'home/.config/hgs/swarm/catalog.json').read_text())['node_id']
        self.assertNotEqual(local, machine)
        # A guarded call that verified this computer refuses another one's reference.
        for call in (['--expect-machine', local, 'send', reference, '--json'],
                     ['--expect-machine', local, '--client', 'peer', 'pause', reference],
                     ['@test-local', '--expect-machine', local, 'kill', reference],
                     ['--expect-machine', local, 'inspect', reference]):
            with self.subTest(call=call):
                refused = self.hgs('/bin/sh', *call)
                self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
                self.assertIn('a guarded call acts only on this computer', refused.stderr)
                self.assertFalse((self.root / 'ssh-args.json').exists())
        # The receiving side of a hop that reached another computer refuses it.
        hop = self.hgs('/bin/sh', '--expect-machine', machine, '--client', 'peer', 'pause', reference)
        self.assertEqual(hop.returncode, 1, hop.stdout + hop.stderr)
        self.assertIn('the selected SSH alias does not match', hop.stderr)
        self.assertFalse((self.root / 'ssh-args.json').exists())
        # The hop itself: one SSH call to the alias, guarded by the reference's machine.
        result = self.hgs('/bin/sh', '@peer', 'send', reference, '--json', body='{}')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.received(), (['--expect-machine', machine, 'send', reference, '--json'], '{}'))

    def test_peer_status_reads_the_status_command(self):
        for name, shell in login_shells().items():
            with self.subTest(shell=name):
                result = self.hgs(shell, '@peer', 'status', '--json')
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.received()[0], ['status', '--local', '--json'])

    def test_machine_check_reads_the_setup_report(self):
        for name, shell in login_shells().items():
            with self.subTest(shell=name):
                result = self.hgs(shell, 'machine', 'check', 'peer')
                self.assertEqual(result.returncode, 0, result.stderr)
                report = json.loads(result.stdout)
                self.assertTrue(report['ok'], report)
                self.assertEqual((report['hgs'], report['tmux']), ('hgs 9.9.9', 'tmux 3.5'))

    def test_machine_shell_opens_the_folder_in_the_login_shell(self):
        folder = self.root / "a folder/it's \\ $(x)"
        folder.mkdir(parents=True)
        for name, shell in login_shells().items():
            with self.subTest(shell=name):
                result = self.hgs(shell, 'machine', 'ssh', 'peer', '--directory', str(folder))
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((self.peer / 'shell-pwd').read_text(), f'{folder}\n')
                self.assertEqual((self.peer / 'shell-args').read_text(), '-l\n')
                (self.peer / 'shell-pwd').unlink()


if __name__ == '__main__':
    unittest.main()
