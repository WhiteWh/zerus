"""Exact read-only ordinary-send evidence, private state and scripted tmux only."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock
import uuid

HGS = Path(os.environ.get('HGS_TEST_BIN', Path(__file__).resolve().parents[1] / 'target/debug/hgs')).resolve()
MACHINE = '22222222-2222-4222-8222-222222222222'
SESSION = '33333333-3333-4333-8333-333333333333'
MESSAGE = '44444444-4444-4444-8444-444444444444'
RUN = '55555555-5555-4555-8555-555555555555'
CONVERSATION = '66666666-6666-4666-8666-666666666666'
NAME = 'codex/example/send'
DIGEST = hashlib.sha256(b'exact original native stdin').hexdigest()


class DeliveryReceipt(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='hgs-delivery-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.state = self.root / 'state'
        self.env = dict(PATH=os.environ.get('PATH', os.defpath), HOME=str(self.root), USERPROFILE=str(self.root),
                        HGS_CONFIG_DIR=str(self.root / 'config'), HGS_STATE_DIR=str(self.state), HGS_SELF='fixture',
                        HGS_PEERS='', HGS_TAB='0', TMPDIR=str(self.root), TMP=str(self.root), TEMP=str(self.root))
        if os.name == 'nt':
            for key in ('SystemRoot', 'SYSTEMROOT', 'WINDIR', 'PATHEXT'):
                if key in os.environ: self.env[key] = os.environ[key]
        folder = self.root / 'config/swarm'
        folder.mkdir(parents=True, mode=0o700)
        (self.root / 'config').chmod(0o700)
        catalog = folder / 'catalog.json'
        catalog.write_text(json.dumps(dict(node_id=MACHINE, catalog=dict(schema=1, swarm_id=SESSION, operations={}), bindings={})))
        catalog.chmod(0o600)
        self.query = dict(request_id=str(uuid.uuid4()), message_request_id=MESSAGE, expected_machine_id=MACHINE,
                          expected_session_id=SESSION, expected_run_id=RUN, expected_conversation_id=CONVERSATION,
                          provider='codex', native_digest=DIGEST)

    def call(self, query=None, name=NAME, success=True, prefix=()):
        result = subprocess.run([str(HGS), *prefix, 'delivery', 'receipt', name, '--json'],
                                input=json.dumps(query or self.query), env=self.env, cwd=self.root,
                                text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return json.loads(result.stdout) if success else result

    def write(self, provider='codex', **changes):
        self.state.mkdir(mode=0o700, exist_ok=True)
        directory = self.state / {'dsh': 'dsh/receipts', 'opencode': 'opencode/receipts'}.get(provider, 'input_receipts')
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        for parent in directory.parents:
            if parent == self.root: break
            parent.chmod(0o700)
        value = dict(request_id=MESSAGE, name=NAME, run_id=RUN, conversation_id=CONVERSATION,
                     first_message=False, digest=DIGEST, status='submitted', submitted_at=1.0, text='private original body',
                     submitted_text='private original body', attachments=[], transport={'dsh': 'native_dsh', 'opencode': 'native_opencode'}.get(provider, 'terminal'),
                     delivery_binding=dict(schema=1, machine_id=MACHINE, session_id=SESSION, provider=provider,
                                           namespace='account:' + 'a' * 64))
        if provider == 'opencode': value['native_message_id'] = 'msg_VALID_123'
        value.update(changes)
        path = directory / (MESSAGE + '.json')
        path.write_text(json.dumps(value)); path.chmod(0o600)
        return path

    def snapshot(self):
        return {str(p.relative_to(self.root)): (hashlib.sha256(p.read_bytes()).hexdigest(), p.stat().st_mtime_ns)
                for p in self.root.rglob('*') if p.is_file() and not p.is_symlink()}

    def test_exact_text_only_outcomes_are_body_free_and_never_write(self):
        for provider in ('codex', 'claude', 'kimi', 'dsh', 'opencode'):
            self.write(provider)
            before = self.snapshot()
            response = self.call({**self.query, 'provider': provider})
            self.assertEqual(response['status'], 'submitted')
            self.assertEqual(response['message_request_id'], MESSAGE)
            self.assertEqual(response['session_id'], SESSION)
            self.assertEqual(response['machine_id'], MACHINE)
            self.assertEqual(response['native_digest'], DIGEST)
            self.assertNotIn('private original body', json.dumps(response))
            self.assertNotIn('text', response)
            self.assertEqual(self.snapshot(), before)

    def test_source_specific_native_ids_are_required_and_never_leak_paths(self):
        query = {**self.query, 'provider': 'opencode'}
        for ident in (None, '', 'msg_', 'not-an-opencode-message-id', '/private/fixture/session.json',
                      'msg_bad/name', 'msg_bad\\name', 'msg_bad\nname', 'msg_' + 'a' * 125, 17):
            with self.subTest(ident=ident):
                path = self.write('opencode', native_message_id=ident)
                before = self.snapshot()
                response = self.call(query)
                self.assertEqual(response['status'], 'unknown')
                self.assertNotIn('proof', response)
                self.assertNotIn('native_message_id', response)
                self.assertEqual(self.snapshot(), before)
        path = self.write('opencode'); value = json.loads(path.read_text()); del value['native_message_id']
        path.write_text(json.dumps(value))
        self.assertEqual(self.call(query)['status'], 'unknown')
        for ident in ('msg_VALID_123', 'msg_' + 'a' * 124):
            self.write('opencode', native_message_id=ident)
            response = self.call(query)
            self.assertEqual(response['status'], 'submitted')
            self.assertEqual(response['native_message_id'], ident)
        for provider in ('codex', 'claude', 'kimi', 'dsh'):
            self.write(provider, native_message_id='/private/fixture/session.json')
            response = self.call({**self.query, 'provider': provider})
            self.assertEqual(response['status'], 'unknown')
            self.assertNotIn('native_message_id', response)

    def test_every_original_binding_and_digest_mismatch_stays_unknown(self):
        path = self.write(); original = json.loads(path.read_text())
        for field, value in [('name', 'codex/foreign/send'), ('run_id', 'foreign'), ('conversation_id', 'foreign'),
                             ('digest', '0' * 64), ('request_id', str(uuid.uuid4())), ('agent_id', 'foreign-child')]:
            path.write_text(json.dumps({**original, field: value}))
            self.assertEqual(self.call()['status'], 'unknown', field)
        for field, value in [('machine_id', str(uuid.uuid4())), ('session_id', str(uuid.uuid4())), ('provider', 'claude'), ('schema', 2)]:
            data = {**original, 'delivery_binding': {**original['delivery_binding'], field: value}}
            path.write_text(json.dumps(data))
            self.assertEqual(self.call()['status'], 'unknown', field)
        path.write_text(json.dumps(original)); before = self.snapshot()
        self.call({**self.query, 'expected_machine_id': str(uuid.uuid4())}, success=False)
        self.assertEqual(self.snapshot(), before)

    def test_pending_corrupt_missing_and_legacy_evidence_never_means_not_sent(self):
        for status in ('pending', 'in_progress', 'refused', 'future'):
            self.write(status=status)
            self.assertEqual(self.call()['status'], 'unknown')
        path = self.write(delivery_binding=None)
        self.assertEqual(self.call()['status'], 'unknown')
        for raw in ('{', '{}', ' ' * (256 * 1024 + 1)):
            path.write_text(raw)
            self.assertEqual(self.call()['status'], 'unknown')
        path.unlink()
        self.assertEqual(self.call()['status'], 'unknown')

    def test_no_index_or_session_alias_can_redirect_the_original_read(self):
        self.write(); (self.state / 'delivery-receipts-v1.sqlite3').write_text('corrupt ignored index')
        replacement = {**self.query, 'expected_session_id': str(uuid.uuid4())}
        (self.state / 'misleading-session.json').write_text(json.dumps(dict(name=NAME, session_id=replacement['expected_session_id'], rename_target='foreign')))
        before = self.snapshot()
        self.assertEqual(self.call()['status'], 'submitted')
        self.assertEqual(self.call(replacement)['status'], 'unknown')
        self.assertEqual(self.snapshot(), before)

    def test_absent_root_and_invalid_requests_create_nothing(self):
        self.assertEqual(self.call()['status'], 'unknown')
        self.assertFalse(self.state.exists())
        for field, value in [('message_request_id', '../escape'), ('native_digest', 'bad'), ('expected_session_id', ''), ('provider', 'future')]:
            self.call({**self.query, field: value}, success=False)
            self.assertFalse(self.state.exists())

    def test_duplicate_identity_fields_and_contradictory_graph_are_unknown(self):
        path = self.write(); original = path.read_text()
        path.write_text('{"digest":"foreign",' + original[1:])
        self.assertEqual(self.call()['status'], 'unknown')
        endpoint = dict(machine=MACHINE, provider='codex', namespace='account:' + 'a' * 64,
                        conversation=CONVERSATION, provisional_run='')
        target = dict(schema=1, name=NAME, run_id=RUN, endpoint=endpoint,
                      node='node:' + hashlib.sha256(json.dumps(endpoint, sort_keys=True, separators=(',', ':')).encode()).hexdigest())
        self.write(graph_target=target)
        self.assertEqual(self.call()['status'], 'submitted')
        target['endpoint']['machine'] = str(uuid.uuid4())
        self.write(graph_target=target)
        self.assertEqual(self.call()['status'], 'unknown')

    def test_startup_and_child_proofs_keep_the_original_scope(self):
        self.write(conversation_id='', first_message=True)
        self.assertEqual(self.call({**self.query, 'expected_conversation_id': ''})['status'], 'submitted')
        self.assertEqual(self.call()['status'], 'unknown')
        self.write(agent_id='exact-child')
        self.assertEqual(self.call({**self.query, 'agent_id': 'exact-child'})['status'], 'submitted')
        self.assertEqual(self.call()['status'], 'unknown')
        self.assertEqual(self.call({**self.query, 'agent_id': 'other-child'})['status'], 'unknown')

    def test_machine_guard_help_and_limits_never_bootstrap_or_write(self):
        path = self.write(); before = self.snapshot()
        self.assertEqual(self.call(prefix=('--expect-machine', MACHINE))['status'], 'submitted')
        self.call(prefix=('--expect-machine', SESSION), success=False)
        self.assertEqual(self.snapshot(), before)
        result = subprocess.run([str(HGS), '--help'], env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual([line.strip() for line in result.stdout.splitlines()].count('delivery-receipt-v1'), 1)
        original = path.read_bytes(); path.write_bytes(original + b' ' * (256 * 1024 - len(original)))
        self.assertEqual(self.call()['status'], 'submitted')
        with path.open('ab') as stream: stream.write(b' ')
        self.assertEqual(self.call()['status'], 'unknown')
        catalog = self.root / 'config/swarm/catalog.json'; catalog.unlink(); before = self.snapshot()
        self.call(success=False)
        self.assertEqual(self.snapshot(), before)
        self.assertFalse(catalog.exists())

    @unittest.skipIf(os.name == 'nt', 'POSIX synthetic SSH; native Windows path checked separately')
    def test_remote_capability_is_exact_and_forwarding_has_no_terminal(self):
        import shlex
        self.write()
        ssh = self.root / 'ssh'
        ssh.write_text("#!/usr/bin/env python3\nimport os,sys,pathlib,json\n"
                       "pathlib.Path(os.environ['HOME'],'ssh-args').write_text(json.dumps(sys.argv[1:]))\n"
                       "os.execv('/bin/sh',['sh','-c',sys.argv[-1]])\n")
        ssh.chmod(0o700); self.env['HGS_SSH'] = str(ssh)
        binary = self.root / '.local/bin/hgs'; binary.parent.mkdir(parents=True, mode=0o700)
        binary.write_text(f'#!/bin/sh\nif [ "$1" = --help ]; then cat "$HOME/help"; else touch "$HOME/forwarded"; exec {shlex.quote(str(HGS))} "$@"; fi\n')
        binary.chmod(0o700); help_file = self.root / 'help'; forwarded = self.root / 'forwarded'
        for marker in ('', 'prefix-delivery-receipt-v1\n', 'delivery-receipt-v1-suffix\n'):
            help_file.write_text(marker)
            self.call(prefix=('@synthetic-peer',), success=False)
            self.assertFalse(forwarded.exists())
        help_file.write_text('delivery-receipt-v1\n')
        self.assertEqual(self.call(prefix=('@synthetic-peer',))['status'], 'submitted')
        self.assertTrue(forwarded.exists())
        argv = json.loads((self.root / 'ssh-args').read_text())
        self.assertNotIn('-t', argv); self.assertIn('BatchMode=yes', argv)

    @unittest.skipIf(os.name == 'nt', 'POSIX no-follow control; Windows has native ACL controls')
    def test_link_fifo_and_nonprivate_receipts_are_not_followed(self):
        path = self.write(); original = path.read_bytes(); foreign = self.root / 'foreign.json'; path.rename(foreign)
        path.symlink_to(foreign)
        self.assertEqual(self.call()['status'], 'unknown'); path.unlink()
        os.mkfifo(path, 0o600)
        self.assertEqual(self.call()['status'], 'unknown'); path.unlink()
        path.write_bytes(original); path.chmod(0o666)
        self.assertEqual(self.call()['status'], 'unknown')


@unittest.skipIf(os.name == 'nt', 'private scripted tmux fixture')
class ProducedDeliveryReceipt(unittest.TestCase):
    def test_actual_send_retains_binding_and_lookup_never_replays(self):
        self.actual_send()

    def test_actual_send_with_symlinked_tmpdir_retains_binding_and_never_replays(self):
        # Keep this socket path short on macOS as well as Linux.
        with tempfile.TemporaryDirectory(prefix='hgs-dr-', dir='/tmp') as scratch:
            root = Path(scratch).resolve()
            physical, alias = root / 'p', root / 'a'
            physical.mkdir(mode=0o700)
            alias.symlink_to(physical, target_is_directory=True)
            with mock.patch.object(tempfile, 'tempdir', str(alias)):
                self.actual_send()

    def actual_send(self):
        from test_input import InputTransport
        # InputTransport keeps its temporary path verbatim. macOS prefixes
        # such as /var alias /private/var, while the identity reader deliberately
        # refuses links in any ancestor. Allocate this fixture's entire namespace
        # under the physical prefix, before its env, paths and socket are built.
        with mock.patch.object(tempfile, 'tempdir', str(Path(tempfile.gettempdir()).resolve())):
            fixture = InputTransport(methodName='runTest'); fixture.setUp()
        try:
            for key, folder in [('CODEX_HOME', 'codex'), ('CLAUDE_CONFIG_DIR', 'claude'), ('KIMI_CODE_HOME', 'kimi'),
                                ('DSH_HOME', 'dsh'), ('OPENCODE_HOME', 'opencode'), ('XDG_DATA_HOME', 'data'),
                                ('XDG_CONFIG_HOME', 'xdg-config'), ('XDG_CACHE_HOME', 'cache')]:
                path = fixture.root / ('native-' + folder); path.mkdir(mode=0o700)
                fixture.env[key] = str(path)
            fixture.record['session_id'] = SESSION; fixture.write_record()
            request = fixture.payload(text='  e\u0301\nexact trailing space  ')
            receipt = fixture.send(request)
            self.assertEqual(receipt['digest'], hashlib.sha256(json.dumps(request).encode()).hexdigest())
            self.assertIn('delivery_binding', receipt)
            self.assertEqual(receipt['delivery_binding']['session_id'], SESSION)
            original = fixture.state / 'input_receipts' / (request['request_id'] + '.json')
            fixture.wait_for(lambda: fixture.received().endswith(b'\r'))
            before = original.read_bytes(); keys = fixture.received()
            query = dict(request_id=str(uuid.uuid4()), message_request_id=request['request_id'],
                         expected_machine_id=receipt['delivery_binding']['machine_id'], expected_session_id=SESSION,
                         expected_run_id=fixture.run_id, expected_conversation_id=fixture.conversation_id,
                         provider='codex', native_digest=receipt['digest'])
            for _ in range(2):
                result = subprocess.run([str(HGS), 'delivery', 'receipt', fixture.name, '--json'], input=json.dumps(query),
                                        env=fixture.env, cwd=fixture.root, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout)['status'], 'submitted', result.stdout)
            self.assertEqual(original.read_bytes(), before)
            self.assertEqual(fixture.received(), keys)
            # Model a pre-binding original; a normal idempotent send must not
            # retrofit history or type again, even though the live record has an ID.
            legacy = json.loads(before); legacy.pop('delivery_binding')
            original.write_text(json.dumps(legacy)); legacy_before = original.read_bytes(), original.stat().st_mtime_ns
            self.assertEqual(fixture.send(request), legacy)
            self.assertEqual((original.read_bytes(), original.stat().st_mtime_ns), legacy_before)
            result = subprocess.run([str(HGS), 'delivery', 'receipt', fixture.name, '--json'], input=json.dumps(query),
                                    env=fixture.env, cwd=fixture.root, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)['status'], 'unknown')
            self.assertEqual(fixture.received(), keys)
        finally:
            fixture.tearDown(); fixture.doCleanups()


if __name__ == '__main__': unittest.main()
