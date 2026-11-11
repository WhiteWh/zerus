"""Actual public send CLI with private scripted PTYs and exact-source ID proofs.

This is controller qualification; official-agent contracts run separately.
"""
import hashlib
from contextlib import closing
import json
import os
from pathlib import Path
import shlex
import sqlite3
import subprocess
import sys
import unittest
import uuid
import time

import native_effort_id as identity
import test_input
import test_settings


@unittest.skipUnless(os.environ.get('ZERUS_PICKER_EFFORT_SEED_IMAGE'),
                     'Set the exact-source ID seed image for public Send contracts')
class ExistingIdSend(unittest.TestCase):
    proof_kind = 'moved'
    pending_attachments = 1
    pending_attachment_bytes = 1 << 20
    def setUp(self):
        self.case = test_settings.SessionSettings(methodName='runTest')
        self.case.setUp()
        self.addCleanup(self.case.doCleanups)
        self.addCleanup(self.case.tearDown)
        if 'unprimed_codex' in self._testMethodName:
            # Existing records permit short run IDs and a genuinely unprimed
            # native CID. Size admission must cover that small-header shape.
            self.case.run_id = 'r'
            self.case.conversation_id = ''
            self.case.record.update(run_id='r', conversation_id=None,
                                    run_identity_version=1, last_event_at=0)
            if self.proof_kind == 'moved':
                # A supervised legacy launch already has its own SID before
                # transfer. Keep it in the exact nullable-CID before-image.
                self.case.record.update(session_id=str(uuid.uuid4()), supervisor={})
            self.case.tmux('set-option', '-t', '=' + self.case.name + ':', '@hgs_run', 'r')
            self.case.write_record()
        self.record, self.path, _ = identity.moved_record(
            self.case, self.case.name, self.case.record, existing_machine=True, kind=self.proof_kind)
        self.case.record = self.record
        self.case.record_path = self.path
        if 'unprimed_codex' in self._testMethodName and self.proof_kind == 'fresh':
            # This scripted launch publishes Fresh first, then its supervisor
            # metadata, retaining the originally published SID and null CID.
            self.record['supervisor'] = {}
            self.case.write_record()
        self.pending = str(uuid.uuid4())
        self.record['pending_settings'] = dict(
            request_id=self.pending, model='gpt-6-sol', effort='low',
            run_id=self.case.run_id, conversation_id=self.case.conversation_id,
            receipt_name=self.case.name, receipt_run_id=self.case.run_id)
        self.case.write_record()
        directory = self.case.state / 'settings_receipts'
        directory.mkdir(mode=0o700, exist_ok=True)
        self.pending_path = directory / (self.pending + '.json')
        self.pending_path.write_text(json.dumps(dict(
            request_id=self.pending, status='scheduled', name=self.case.name,
            run_id=self.case.run_id, conversation_id=self.case.conversation_id,
            model='gpt-6-sol', effort='low', scope='session')))
        self.pending_path.chmod(0o600)
        self.request = dict(request_id=str(uuid.uuid4()), text='continue the exact fixture',
                            expected_run_id=self.case.run_id,
                            expected_conversation_id=self.case.conversation_id)
        self.raw = ('{\n ' + json.dumps(self.request, separators=(',', ':'))[1:-1] + '\n}\n')
        self.receipt_path = self.case.state / 'input_receipts' / (self.request['request_id'] + '.json')
        self.calls = self.case.root / 'send-calls.jsonl'
        script = self.case.root / 'send-observer.py'
        script.write_text('REAL_TMUX=' + repr(test_input.TMUX) + '\n' + r'''
import fcntl,json,os,pathlib,socket,sqlite3,subprocess,sys,time
root=pathlib.Path(os.environ['INPUT_FIXTURE']);state=pathlib.Path(os.environ['HGS_STATE_DIR']);args=sys.argv[1:]
def read(path):
 try:return json.loads(path.read_bytes())
 except (FileNotFoundError,ValueError):return None
if args[:1] in (['send-keys'],['load-buffer'],['paste-buffer'],['delete-buffer']):
 row=dict(at=time.monotonic(),args=args,receipt=read(pathlib.Path(os.environ['SEND_RECEIPT'])),record=read(pathlib.Path(os.environ['SEND_RECORD'])),pending=read(pathlib.Path(os.environ['SEND_PENDING'])),recovery=read(pathlib.Path(os.environ['SEND_RECOVERY'])) if 'SEND_RECOVERY' in os.environ else None)
 with (root/'send-calls.jsonl').open('a') as out:out.write(json.dumps(row)+'\n')
if os.environ.get('SEND_INDEX_FINAL_FAULT')=='1' and args[:1]==['send-keys'] and args[-1:] == ['Enter'] and read(pathlib.Path(os.environ['SEND_PENDING']))['status']=='applied':
 # This is the message Enter, after both in-progress receipt registrations.
 # The final original must still commit even when cache error clearing fails.
 db=sqlite3.connect(state/'delivery-receipts-v1.sqlite3')
 with db:
  db.execute("INSERT INTO receipt_errors VALUES('terminal',?,'injected')",(pathlib.Path(os.environ['SEND_RECEIPT']).name,))
  db.execute("UPDATE receipt_sources SET complete=1 WHERE source='terminal'")
  db.execute("CREATE TRIGGER refuse_final_registration BEFORE DELETE ON receipt_errors BEGIN SELECT RAISE(ABORT,'injected final registration failure'); END")
 db.close();(root/'index-final-fault-fired').touch()
if os.environ.get('SEND_INDEX_CACHE_LOCK')=='1' and args[:1]==['send-keys'] and args[-1:] == ['Enter'] and read(pathlib.Path(os.environ['SEND_PENDING']))['status']=='applied':
 (root/'index-cache-enter').touch();until=time.monotonic()+3
 while not (root/'index-cache-held').exists():
  assert time.monotonic()<until,'private cache writer did not acquire its transaction'
  time.sleep(.002)
fault=os.environ.get('SEND_FAULT','')
if fault=='pending-receipt' and args[:1]==['list-panes'] and not (root/'fault-fired').exists():
 (root/'fault-fired').touch();p=pathlib.Path(os.environ['SEND_PENDING']);value=read(p);value['status']='applied';p.write_text(json.dumps(value))
if fault=='stage-denied' and args[:1]==['delete-buffer']:
 print('permission denied',file=sys.stderr);raise SystemExit(1)
result=subprocess.call([REAL_TMUX,'-S',str(root/'socket'),'-f','/dev/null',*args])
if os.environ.get('SEND_HOLD_NATIVE')=='1' and args[:1]==['list-panes'] and not (root/'hold-fired').exists():
 (root/'hold-fired').touch();peer=socket.socket(socket.AF_UNIX);peer.connect(str(root/'auto-apply.sock'));peer.sendall(b'ready');assert peer.recv(1)==b'';peer.close()
if fault=='large-payload' and args[:1]==['load-buffer']:(root/'freeze').touch()
if fault in ('stage-source','stage-denied') and args[:1]==['load-buffer']:
 p=pathlib.Path(os.environ['SEND_RECORD']);value=read(p);value['activity']='busy';value['phase']='working';p.write_text(json.dumps(value))
if fault in ('stage-held','paste-held') and args[:1]==(['load-buffer'] if fault=='stage-held' else ['paste-buffer']):
 # Hold this original exchange's stdout/stderr after its tmux process exits.
 # This private descendant has no input authority and only the test releases it.
 if os.fork()==0:
  peer=socket.socket(socket.AF_UNIX);peer.connect(str(root/'held-pipes.sock'));peer.sendall(b'ready');assert peer.recv(1)==b'';peer.close();os._exit(0)
raise SystemExit(result)
''')
        self.case.script('tmux', '#!/bin/sh\nexec ' + shlex.quote(sys.executable) + ' ' +
                         shlex.quote(str(script)) + ' "$@"\n')
        self.case.env.update(SEND_RECEIPT=str(self.receipt_path), SEND_RECORD=str(self.path),
                             SEND_PENDING=str(self.pending_path))

    def call(self, raw=None, selector='label'):
        name = self.case.name
        args = []
        if selector == 'sid':
            args = ['--session-id', self.record['session_id']]
        elif selector == 'reference':
            import urllib.parse
            hello = subprocess.run([str(test_input.HGS), 'swarm', 'hello'], env=self.case.env,
                                   text=True, capture_output=True, timeout=10)
            self.assertEqual(hello.returncode, 0, hello.stderr)
            name = 'zerus://session?' + urllib.parse.urlencode(dict(
                machine=json.loads(hello.stdout)['node_id'], host='test', name=self.case.name,
                session=self.record['session_id'], run=self.record['run_id'],
                conversation=self.record['conversation_id']))
        prefix = []
        if selector in ('state', 'guarded-state', 'client-state'):
            if selector == 'guarded-state':
                hello = subprocess.run([str(test_input.HGS), 'swarm', 'hello'], env=self.case.env,
                                       text=True, capture_output=True, timeout=10)
                self.assertEqual(hello.returncode, 0, hello.stderr)
                prefix = ['--expect-machine', json.loads(hello.stdout)['node_id']]
            elif selector == 'client-state':
                prefix = ['--client', 'desktop-fixture']
            if selector != 'client-state':
                prefix.append('__state')
            args = ['--session-id', self.record['session_id']] if selector == 'client-state' else []
        return subprocess.run([str(test_input.HGS), *prefix, 'send', name, *args, '--json'],
                              input=self.raw if raw is None else raw, env=self.case.env,
                              text=True, capture_output=True, timeout=20)

    def start(self, command, payload, env=None):
        child = subprocess.Popen(command, env=self.case.env if env is None else env, stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        child.stdin.write(payload)
        child.stdin.close()
        child.stdin = None
        def settle():
            if child.poll() is None:
                child.terminate()
            child.communicate(timeout=20)
        self.addCleanup(settle)
        return child

    def wait_for_slot(self):
        import fcntl
        def held():
            path = self.case.state / '.native-picker-host.lock'
            if not path.exists():
                return False
            with path.open('r+b') as file:
                try:
                    fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    fcntl.flock(file, fcntl.LOCK_UN)
                    return False
                except BlockingIOError:
                    return True
        self.case.wait_for(held)

    def rows(self):
        return [json.loads(line) for line in self.calls.read_text().splitlines()] if self.calls.exists() else []

    def assert_submitted(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr + '\n' + json.dumps(dict(
            record=json.loads(self.path.read_bytes()), pending=json.loads(self.pending_path.read_bytes()),
            calls=self.rows()), indent=2))
        receipt = json.loads(result.stdout)
        self.assertEqual(receipt['status'], 'submitted')
        self.assertEqual(receipt['digest'], hashlib.sha256(self.raw.encode()).hexdigest())
        self.assertEqual(receipt['request_id'], self.request['request_id'])
        self.assertEqual(receipt, json.loads(self.receipt_path.read_bytes()))
        return receipt

    def test_pending_settings_then_one_message_reserves_outer_receipt_first(self):
        before = self.case.cache.read_bytes()
        receipt = self.assert_submitted(self.call())
        self.case.wait_for(lambda: self.case.received().endswith(b'\r'))
        rows = self.rows()
        keys = [row for row in rows if row['args'][0] == 'send-keys']
        self.assertGreater(len(keys), 1)
        for row in keys:
            self.assertIsNotNone(row['receipt'], row)
            self.assertEqual(row['receipt']['request_id'], self.request['request_id'])
            self.assertEqual(row['receipt']['status'], 'in_progress')
        first = keys[0]
        self.assertNotIn('input_pending_at', first['record'])
        self.assertEqual(first['pending']['status'], 'in_progress')
        pastes = [row for row in rows if row['args'][0] == 'paste-buffer']
        self.assertEqual(len(pastes), 1)
        self.assertEqual(pastes[0]['pending']['status'], 'applied')
        self.assertEqual(pastes[0]['record']['input_pending_at'], receipt['submitted_at'])
        self.assertNotIn('pending_settings', pastes[0]['record'])
        self.assertNotIn('settings_unconfirmed_at', pastes[0]['record'])
        after = self.case.received()
        self.assertTrue(after.endswith(b'\x1b[200~' + self.request['text'].encode() + b'\x1b[201~\r'), after)
        self.assertEqual(self.assert_submitted(self.call(selector='sid')), receipt)
        self.assertEqual(self.case.received(), after)
        changed = self.call(self.raw + ' ')
        self.assertNotEqual(changed.returncode, 0)
        self.assertIn('already used', changed.stderr)
        self.assertEqual(self.case.received(), after)
        self.assertEqual(self.case.cache.read_bytes(), before)
        self.assertFalse((self.case.root / '.codex/config.toml').exists())

    def prepare_receipt_index(self):
        import fcntl
        self.receipt_path.parent.mkdir(mode=0o700, exist_ok=True)
        migrated = subprocess.run([str(test_input.HGS), '__state', 'receipt-index-worker'],
                                 env=self.case.env, text=True, capture_output=True, timeout=20)
        self.assertEqual(migrated.returncode, 0, migrated.stderr)
        lease = (self.case.state / 'receipt-index-worker.lock').open('a+b')
        self.index_lease = lease
        self.addCleanup(lease.close)
        fcntl.flock(lease, fcntl.LOCK_EX)
        self.index_database = self.case.state / 'delivery-receipts-v1.sqlite3'
        with closing(sqlite3.connect(self.index_database)) as db:
            self.assertEqual(db.execute("SELECT complete FROM receipt_sources WHERE source='terminal'").fetchone(), (1,))

    def receipt_projection(self):
        result = subprocess.run([str(test_input.HGS), '__state', 'receipt-index-page',
                                 self.case.name, '--json'], env=self.case.env,
                                input=json.dumps(dict(expected_run_id=self.case.run_id,
                                                      expected_conversation_id=self.case.conversation_id)),
                                text=True, capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def assert_index_failure_never_replays(self):
        self.assert_submitted(self.call())
        self.case.wait_for(lambda: self.case.received().endswith(b'\r'))
        rows, received = self.rows(), self.case.received()
        self.assertEqual(sum(row['args'][0] == 'paste-buffer' for row in rows), 1)
        self.assert_pending_custodian()
        with closing(sqlite3.connect(self.index_database)) as db:
            self.assertEqual(db.execute("SELECT complete FROM receipt_sources WHERE source='terminal'").fetchone(), (0,))
        projection = self.receipt_projection()['receipt_projection']
        self.assertFalse(projection['migration_complete'])
        self.assertFalse(projection['catalog_complete_at_snapshot'])
        self.assertFalse(projection['absence_confirmed'])
        original = self.receipt_path.read_bytes()
        self.assert_submitted(self.call(selector='sid'))
        self.assertEqual(self.receipt_path.read_bytes(), original)
        self.assertEqual(self.rows(), rows)
        self.assertEqual(self.case.received(), received)

    def test_pending_send_registers_fenced_original_in_locator_and_projection(self):
        self.prepare_receipt_index()
        data = self.attachment_request(17)
        receipt = self.assert_submitted(self.call())
        self.assert_pending_custodian()
        self.assertEqual(Path(receipt['attachments'][0]['path']).read_bytes(), data)
        with closing(sqlite3.connect(self.index_database)) as db:
            self.assertEqual(db.execute("SELECT files FROM receipt_rows WHERE source='terminal' AND request=?",
                                        (self.request['request_id'],)).fetchall(), [(1,)])
            frontier = int(db.execute("SELECT value FROM receipt_meta WHERE key='change_seq'").fetchone()[0])
            change = db.execute("SELECT change FROM receipt_rows WHERE source='terminal' AND request=?",
                                (self.request['request_id'],)).fetchone()[0]
            self.assertEqual(change, frontier)
            self.assertGreater(change, 0)
        projection = self.receipt_projection()
        self.assertEqual([row['message_id'] for row in projection['attachment_messages']], [self.request['request_id']])
        self.assertTrue(projection['receipt_projection']['migration_complete'])
        self.assertTrue(projection['receipt_projection']['catalog_complete_at_snapshot'])
        self.assertFalse((self.case.state / 'receipt-index-attempt.json').exists(), 'successful registration needs no forced reconciliation')

    def test_new_original_index_failure_keeps_send_once_and_coverage_incomplete(self):
        self.prepare_receipt_index()
        with closing(sqlite3.connect(self.index_database)) as db, db:
            db.execute("CREATE TRIGGER refuse_new_registration BEFORE INSERT ON receipt_rows BEGIN SELECT RAISE(ABORT,'injected new registration failure'); END")
        self.assert_index_failure_never_replays()
        with closing(sqlite3.connect(self.index_database)) as db:
            self.assertEqual(db.execute('SELECT COUNT(*) FROM receipt_rows').fetchone(), (0,))

    def test_worker_repair_hint_fifo_cannot_block_the_submitted_send(self):
        import fcntl
        self.prepare_receipt_index()
        with closing(sqlite3.connect(self.index_database)) as db, db:
            db.execute("UPDATE receipt_sources SET complete=0 WHERE source='terminal'")
        self.index_lease.close()
        hint = self.case.state / 'receipt-index-attempt.json'
        self.assertFalse(hint.exists())
        os.mkfifo(hint)
        self.assert_submitted(self.call())
        self.assertTrue(hint.is_file(), 'repair replaced the corrupt throttle hint')
        self.assert_pending_custodian()
        def worker_settled():
            with (self.case.state / 'receipt-index-worker.lock').open('a+b') as lease:
                try:
                    fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    return True
                except BlockingIOError:
                    return False
        self.case.wait_for(worker_settled)
        rows, received = self.rows(), self.case.received()
        self.assert_submitted(self.call(selector='sid'))
        self.assertEqual(self.rows(), rows)
        self.assertEqual(self.case.received(), received)

    def assert_worker_lock_fifo_cannot_block_send(self, filename):
        self.prepare_receipt_index()
        with closing(sqlite3.connect(self.index_database)) as db, db:
            db.execute("UPDATE receipt_sources SET complete=0 WHERE source='terminal'")
        self.index_lease.close()
        fault = self.case.state / filename
        if fault.exists():
            self.assertTrue(fault.is_file())
            fault.unlink()
        os.mkfifo(fault)
        try:
            self.assert_submitted(self.call())
        except subprocess.TimeoutExpired:
            # Pin the regression's real state: this is an accepted send whose
            # frontend is parked in advisory cache work, never a new offer.
            self.assertEqual(json.loads(self.receipt_path.read_bytes())['status'], 'submitted')
            self.assertEqual(sum(row['args'][0] == 'paste-buffer' for row in self.rows()), 1)
            print('ZERUS_CACHE_LOCK_FAULT=' + json.dumps(dict(path=filename, status='submitted', pastes=1)), flush=True)
            raise
        self.assertTrue(fault.is_fifo(), 'refuse the corrupt lock without replacing it')
        self.assertFalse((self.case.state / 'receipt-index-attempt.json').exists())
        rows, received = self.rows(), self.case.received()
        self.assert_submitted(self.call(selector='sid'))
        self.assertEqual(self.rows(), rows)
        self.assertEqual(self.case.received(), received)

    def test_worker_launch_lock_fifo_cannot_block_the_submitted_send(self):
        self.assert_worker_lock_fifo_cannot_block_send('receipt-index-launch.lock')

    def test_worker_probe_lock_fifo_cannot_block_the_submitted_send(self):
        self.assert_worker_lock_fifo_cannot_block_send('receipt-index-worker.lock')

    def test_final_original_index_failure_keeps_send_once_and_coverage_incomplete(self):
        self.prepare_receipt_index()
        self.case.env['SEND_INDEX_FINAL_FAULT'] = '1'
        self.assert_index_failure_never_replays()
        self.assertTrue((self.case.root / 'index-final-fault-fired').exists())
        with closing(sqlite3.connect(self.index_database)) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM receipt_rows WHERE request=?",
                                        (self.request['request_id'],)).fetchone(), (1,))
            self.assertEqual(db.execute("SELECT errors FROM receipt_sources WHERE source='terminal'").fetchone(), (1,))

    def test_failed_cache_invalidation_requests_repair_despite_matching_directory_stamp(self):
        import threading
        self.prepare_receipt_index()
        self.case.env['SEND_INDEX_CACHE_LOCK'] = '1'
        errors = []
        cancelled = threading.Event()
        def hold_cache():
            try:
                # Follow this exact request's lifetime, including its Settings
                # step; do not start a shorter unrelated fixture clock first.
                while not (self.case.root / 'index-cache-enter').exists():
                    if cancelled.wait(.002):
                        return
                with closing(sqlite3.connect(self.index_database)) as db:
                    db.execute('BEGIN IMMEDIATE')
                    stamp = db.execute("SELECT stamp FROM receipt_sources WHERE source='terminal'").fetchone()[0]
                    (self.case.root / 'index-cache-held').touch()
                    self.case.wait_for(lambda: self.receipt_path.exists() and
                        json.loads(self.receipt_path.read_bytes()).get('status') == 'submitted')
                    # Model a coarse filesystem clock without changing the
                    # original's bytes or the operation's original deadline.
                    _, seconds, nanos, _ = stamp.split(':', 3)
                    directory = self.receipt_path.parent
                    os.utime(directory, ns=(directory.stat().st_atime_ns,
                                           int(seconds) * 1_000_000_000 + int(nanos)))
                    time.sleep(.2)  # outlast both bounded advisory SQL writes
                    db.rollback()
            except BaseException as error:
                errors.append(error)
        thread = threading.Thread(target=hold_cache)
        thread.start()
        try:
            self.assert_submitted(self.call())
        finally:
            cancelled.set()
            thread.join(timeout=10)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        self.assertTrue((self.case.root / 'index-cache-held').exists())
        with closing(sqlite3.connect(self.index_database)) as db:
            # The writer prevented invalidation. The directory stamp alone
            # therefore looks complete and cannot communicate the failure.
            self.assertEqual(db.execute("SELECT complete,errors FROM receipt_sources WHERE source='terminal'").fetchone(), (1, 0))
        projection = self.receipt_projection()['receipt_projection']
        self.assertTrue(projection['migration_complete'])
        self.assertTrue((self.case.state / 'receipt-index-launch.lock').exists(),
                        'explicit failed-registration hint must request repair after keeper exit')
        rows, received = self.rows(), self.case.received()
        self.assertEqual(sum(row['args'][0] == 'paste-buffer' for row in rows), 1)
        self.assert_submitted(self.call(selector='sid'))
        self.assertEqual(self.rows(), rows)
        self.assertEqual(self.case.received(), received)

    def attachment_request(self, size, count=1):
        import base64
        data = b'x' * size
        self.request['attachments'] = [dict(name=f'a-{i}', mime='',
            data_base64=base64.b64encode(data).decode()) for i in range(count)]
        self.raw = json.dumps(self.request, separators=(',', ':'))
        return data

    def assert_pending_custodian(self):
        self.assertEqual(json.loads(self.pending_path.read_bytes())['status'], 'applied')
        keys = [r for r in self.rows() if r['args'][0] == 'send-keys']
        self.assertTrue(keys)
        self.assertEqual(keys[0]['receipt']['status'], 'in_progress')

    def test_attachment_byte_bound_uses_the_original_pending_send_custodian(self):
        data = self.attachment_request(self.pending_attachment_bytes)
        receipt = self.assert_submitted(self.call())
        self.assertEqual(Path(receipt['attachments'][0]['path']).read_bytes(), data)
        self.assert_pending_custodian()

    def test_attachment_count_bound_worst_shape_counts_each_tiny_attachment(self):
        self.request['text'] = 'x'
        self.attachment_request(1, self.pending_attachments)
        receipt = self.assert_submitted(self.call())
        self.assertEqual(len(receipt['attachments']), self.pending_attachments)
        for attachment in receipt['attachments']:
            self.assertEqual(Path(attachment['path']).read_bytes(), b'x')
        self.assert_pending_custodian()

    def test_maximum_text_only_uses_the_original_pending_send_custodian(self):
        self.request['text'] = 'x' * (64 << 10)
        self.raw = json.dumps(self.request)
        self.case.env['SEND_FAULT'] = 'large-payload'
        receipt = self.assert_submitted(self.call())
        self.assertEqual(receipt['submitted_text'], self.request['text'])
        self.assert_pending_custodian()
        self.case.wait_for(lambda: self.case.received().endswith(
            b'\x1b[200~' + self.request['text'].encode() + b'\x1b[201~\r'))

    def test_unprimed_codex_maximum_text_keeps_the_legacy_route(self):
        self.request['text'] = 'x' * (64 << 10)
        self.raw = json.dumps(self.request)
        self.case.env['SEND_FAULT'] = 'large-payload'
        receipt = self.assert_submitted(self.call())
        self.assertEqual(receipt['submitted_text'], self.request['text'])
        self.assert_legacy_settings_route()

    def test_unprimed_codex_tiny_attachment_keeps_the_legacy_route(self):
        self.attachment_request(1)
        receipt = self.assert_submitted(self.call())
        self.assertEqual(Path(receipt['attachments'][0]['path']).read_bytes(), b'x')
        self.assert_legacy_settings_route()

    def test_maximum_text_and_admitted_attachment_use_the_pending_send_custodian(self):
        self.request['text'] = 'x' * (64 << 10)
        data = self.attachment_request(self.pending_attachment_bytes)
        self.case.env['SEND_FAULT'] = 'large-payload'
        receipt = self.assert_submitted(self.call())
        self.assertEqual(Path(receipt['attachments'][0]['path']).read_bytes(), data)
        self.assert_pending_custodian()
        self.case.wait_for(lambda: self.case.received().endswith(
            b'\x1b[200~' + receipt['submitted_text'].encode() + b'\x1b[201~\r'))

    def assert_legacy_settings_route(self):
        keys = [r for r in self.rows() if r['args'][0] == 'send-keys']
        self.assertTrue(keys)
        self.assertIsNone(keys[0]['receipt'])
        self.assertEqual(json.loads(self.pending_path.read_bytes())['status'], 'applied')

    def test_attachment_byte_bound_plus_one_keeps_the_legacy_route(self):
        data = self.attachment_request(self.pending_attachment_bytes + 1)
        receipt = self.assert_submitted(self.call())
        self.assertEqual(Path(receipt['attachments'][0]['path']).read_bytes(), data)
        self.assert_legacy_settings_route()

    def test_attachment_count_bound_plus_one_keeps_the_legacy_route(self):
        self.attachment_request(1, self.pending_attachments + 1)
        receipt = self.assert_submitted(self.call())
        self.assertEqual(len(receipt['attachments']), self.pending_attachments + 1)
        self.assert_legacy_settings_route()

    def test_maximum_legal_attachments_keep_the_legacy_route_and_exact_payload(self):
        import base64
        data = b'x' * ((20 << 20) // 8)
        attachments = [dict(name=f'file-{i}.txt', mime='text/plain', reference=f'[File #{i}]',
                            data_base64=base64.b64encode(data).decode()) for i in range(1, 9)]
        text = ' '.join(file['reference'] for file in attachments)
        text = (text * ((64 << 10) // len(text) + 1))[:64 << 10]
        self.request.update(text=text, attachments=attachments)
        self.raw = json.dumps(self.request)
        self.assertLessEqual(len(self.raw.encode()), 29 << 20)
        # Freeze this scripted PTY's rendering only after Settings. Its raw
        # transport log still records the complete bracketed paste and Enter.
        self.case.env['SEND_FAULT'] = 'large-payload'
        started = time.monotonic()
        receipt = self.assert_submitted(self.call())
        seconds = time.monotonic() - started
        self.assertLess(seconds, 15)
        self.assert_legacy_settings_route()
        self.assertLessEqual(len(receipt['submitted_text'].encode()), 1 << 20)
        self.assertEqual(len(receipt['attachments']), 8)
        for file in receipt['attachments']:
            self.assertEqual(Path(file['path']).read_bytes(), data)
            self.assertEqual(receipt['submitted_text'].count(json.dumps(file['path'])), 1)
        self.case.wait_for(lambda: self.case.received().endswith(
            b'\x1b[200~' + receipt['submitted_text'].encode() + b'\x1b[201~\r'))
        self.assertEqual(sum(row['args'][0] == 'load-buffer' for row in self.rows()), 1)
        self.assertEqual(sum(row['args'][0] == 'paste-buffer' for row in self.rows()), 1)
        print('ZERUS_MAX_SEND=' + json.dumps(dict(kind=self.proof_kind, raw_bytes=len(self.raw.encode()),
              decoded_bytes=8*len(data), pasted_bytes=len(receipt['submitted_text'].encode()),
              reply_bytes=len(json.dumps(receipt).encode()), seconds=seconds)), flush=True)

    def recovery_job(self):
        parent = self.case.state / 'recovery/jobs'
        parent.mkdir(parents=True, mode=0o700, exist_ok=True)
        parent.parent.chmod(0o700)
        path = parent / (self.record['session_id'] + '.json')
        job = dict(id=str(uuid.uuid4()), state='waiting', attempt=0, agent='codex',
                   name=self.case.name, identity=[self.case.run_id, self.case.conversation_id,
                                                 self.record['model'], None, None],
                   history=[dict(state='waiting', attempt=0, at=i) for i in range(32)])
        path.write_text(json.dumps(job))
        path.chmod(0o600)
        self.case.env['SEND_RECOVERY'] = str(path)
        return path

    def test_child_refusal_keeps_recovery_and_input_marker_untouched(self):
        recovery = self.recovery_job()
        before = recovery.read_bytes()
        self.record['pending_settings']['model'] = 'fixture-model-not-offered'
        self.case.write_record()
        pending = json.loads(self.pending_path.read_bytes())
        pending['model'] = 'fixture-model-not-offered'
        self.pending_path.write_text(json.dumps(pending))
        result = self.call()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stderr.lower().count('delivery uncertain'), 1)
        self.assertEqual(result.stderr, "hgs: delivery uncertain: model is not in this account's provider catalog\n")
        self.assertEqual(recovery.read_bytes(), before)
        self.assertNotIn('input_pending_at', json.loads(self.path.read_bytes()))
        self.assertEqual(json.loads(self.receipt_path.read_bytes())['status'], 'in_progress')
        self.assertEqual(json.loads(self.pending_path.read_bytes())['status'], 'scheduled')
        self.assertEqual(self.rows(), [])

    def test_recovery_is_cancelled_after_the_child_and_immediately_before_message_staging(self):
        recovery = self.recovery_job()
        self.assert_submitted(self.call())
        rows = self.rows()
        keys = [row for row in rows if row['args'][0] == 'send-keys']
        self.assertEqual(keys[0]['recovery']['state'], 'waiting')
        stage = next(row for row in rows if row['args'][0] == 'load-buffer')
        self.assertEqual(stage['pending']['status'], 'applied')
        self.assertEqual(stage['recovery']['state'], 'cancelled')
        self.assertNotIn('input_pending_at', stage['record'])
        job = json.loads(recovery.read_bytes())
        self.assertEqual(job['state'], 'cancelled')
        self.assertEqual(len(job['history']), 32)
        self.assertEqual(job['history'][-1]['reason'], 'New input cancelled automatic recovery')

    def test_draft_after_admission_is_uncertain_and_has_no_input_effect(self):
        import signal
        screen = '\x1b[2J\x1b[27;1H\x1b[48;5;234m› KEEP DRAFT\x1b[28;1H\x1b[49m\x1b[29;1H GPT-6-Astra xhigh · /fixture\x1b[30;1H ? for shortcuts\x1b[27;13H'
        (self.case.root / 'screen').write_text(screen)
        os.kill(self.record['pid'], signal.SIGUSR1)
        self.case.wait_for(lambda: 'KEEP DRAFT' in self.case.tmux('capture-pane', '-p', '-t', self.case.pane))
        result = self.call()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stderr.lower().count('delivery uncertain'), 1, result.stderr)
        self.assertEqual(self.rows(), [])
        self.assertFalse(self.receipt_path.exists())
        self.assertNotIn('input_pending_at', json.loads(self.path.read_bytes()))
        self.assertEqual(json.loads(self.pending_path.read_bytes())['status'], 'scheduled')

    def test_reference_preserves_machine_sid_run_and_conversation_through_pending_send(self):
        self.assert_submitted(self.call(selector='reference'))
        self.assertEqual(json.loads(self.pending_path.read_bytes())['status'], 'applied')
        self.assertEqual(sum(row['args'][0] == 'paste-buffer' for row in self.rows()), 1)

    def test_state_dispatch_keeps_public_pending_send_provenance(self):
        self.assert_submitted(self.call(selector='state'))
        self.assertEqual(json.loads(self.pending_path.read_bytes())['status'], 'applied')

    def test_guarded_state_dispatch_keeps_public_pending_send_provenance(self):
        self.assert_submitted(self.call(selector='guarded-state'))
        self.assertEqual(json.loads(self.pending_path.read_bytes())['status'], 'applied')

    def test_client_state_dispatch_keeps_public_pending_send_provenance(self):
        self.assert_submitted(self.call(selector='client-state'))
        self.assertEqual(json.loads(self.pending_path.read_bytes())['status'], 'applied')

    def test_state_dispatch_preserves_the_existing_no_session_id_flag_abi(self):
        result = subprocess.run([str(test_input.HGS), '__state', 'send', self.case.name,
                                 '--session-id', self.record['session_id'], '--json'],
                                input=self.raw, env=self.case.env, text=True, capture_output=True, timeout=20)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('usage: hgs send', result.stderr)
        self.assertEqual(self.rows(), [])
        self.assertFalse(self.receipt_path.exists())

    def test_background_with_one_pending_uses_legacy_and_keeps_schedule(self):
        self.record['active_tools'] = {'owned': {}}
        self.case.write_record()
        self.assert_submitted(self.call())
        self.assertEqual(json.loads(self.pending_path.read_bytes())['status'], 'scheduled')
        rows = self.rows()
        self.assertFalse(any(row['args'][0] == 'send-keys' and '-l' in row['args'] for row in rows))
        self.assertIsNone(next(row for row in rows if row['args'][0] == 'load-buffer')['receipt'])

    def test_matching_existing_receipt_replays_without_a_picker_or_slot(self):
        self.receipt_path.parent.mkdir(mode=0o700, exist_ok=True)
        receipt = dict(status='submitted', name=self.case.name,
                       digest=hashlib.sha256(self.raw.encode()).hexdigest(), opaque=['kept'])
        self.receipt_path.write_text(json.dumps(receipt))
        result = self.call()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), receipt)
        self.assertEqual(self.rows(), [])

    def test_missing_account_catalog_refuses_before_offering_input_or_receipt(self):
        self.case.cache.unlink()
        result = self.call()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('No model catalog is available for this account', result.stderr)
        self.assertNotIn('delivery uncertain', result.stderr.lower())
        self.assertFalse(self.receipt_path.exists())
        self.assertEqual(self.rows(), [])
        self.assertNotIn('input_pending_at', json.loads(self.path.read_bytes()))
        self.assertEqual(json.loads(self.pending_path.read_bytes())['status'], 'scheduled')

    def test_empty_account_catalog_refuses_before_offering_input_or_receipt(self):
        self.case.cache.write_text(json.dumps(dict(models=[])))
        result = self.call()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('No model catalog is available for this account', result.stderr)
        self.assertNotIn('delivery uncertain', result.stderr.lower())
        self.assertFalse(self.receipt_path.exists())
        self.assertEqual(self.rows(), [])
        self.assertNotIn('input_pending_at', json.loads(self.path.read_bytes()))
        self.assertEqual(json.loads(self.pending_path.read_bytes())['status'], 'scheduled')

    def test_changed_pending_receipt_after_admission_does_not_offer_input(self):
        self.case.env['SEND_FAULT'] = 'pending-receipt'
        result = self.call()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('delivery uncertain', result.stderr)
        self.assertTrue((self.case.root / 'fault-fired').exists())
        self.assertEqual(self.rows(), [])
        self.assertNotIn('input_pending_at', json.loads(self.path.read_bytes()))

    def test_settled_stage_source_refusal_discards_exact_buffer_once(self):
        self.case.env['SEND_FAULT'] = 'stage-source'
        result = self.call()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('delivery uncertain', result.stderr)
        rows = self.rows()
        self.assertEqual(sum(row['args'][0] == 'load-buffer' for row in rows), 1)
        self.assertFalse(any(row['args'][0] == 'paste-buffer' for row in rows))
        discarded = [row['args'] for row in rows if row['args'][0] == 'delete-buffer']
        self.assertEqual(discarded, [['delete-buffer', '-b', 'hgs-input-' + self.request['request_id']]])
        self.assertEqual(json.loads(self.receipt_path.read_bytes())['status'], 'in_progress')
        self.assertNotIn('input_pending_at', json.loads(self.path.read_bytes()))
        buffers = self.case.tmux('list-buffers', '-F', '#{buffer_name}', check=False)
        self.assertNotIn('hgs-input-' + self.request['request_id'], buffers.splitlines())

    def test_discard_failure_stays_uncertain_and_does_not_retry_or_delete_other_buffers(self):
        other = 'hgs-input-' + str(uuid.uuid4())
        self.case.tmux('set-buffer', '-b', other, 'another attempt')
        self.case.env['SEND_FAULT'] = 'stage-denied'
        result = self.call()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stderr.lower().count('delivery uncertain'), 1)
        self.assertIn('buffer cleanup uncertain', result.stderr)
        rows = self.rows()
        self.assertEqual([row['args'] for row in rows if row['args'][0] == 'delete-buffer'],
                         [['delete-buffer', '-b', 'hgs-input-' + self.request['request_id']]])
        self.assertFalse(any(row['args'][0] == 'paste-buffer' for row in rows))
        self.assertEqual(self.case.tmux('show-buffer', '-b', other), 'another attempt')
        self.assertIn('hgs-input-' + self.request['request_id'],
                      self.case.tmux('list-buffers', '-F', '#{buffer_name}').splitlines())
        self.assertEqual(json.loads(self.receipt_path.read_bytes())['status'], 'in_progress')

    def held_pipe_cleanup(self, fault):
        import fcntl
        import socket
        listener = socket.socket(socket.AF_UNIX)
        self.addCleanup(listener.close)
        listener.bind(str(self.case.root / 'held-pipes.sock'))
        listener.listen(1)
        listener.settimeout(12)
        sender = self.start([str(test_input.HGS), 'send', self.case.name, '--json'], self.raw,
                            env=dict(self.case.env, SEND_FAULT=fault))
        peer, _ = listener.accept()
        self.addCleanup(peer.close)
        self.assertEqual(peer.recv(5), b'ready')
        # The frontend's original 15 s expires, independently of the physical
        # exchange. Wait through the fixed cleanup tail while keeping its pipes.
        stdout, stderr = sender.communicate(timeout=20)
        self.assertNotEqual(sender.returncode, 0)
        self.assertEqual(stderr.lower().count('delivery uncertain'), 1, stderr)
        time.sleep(2.2)
        lock = (self.case.state / '.lock').open('r+b')
        self.addCleanup(lock.close)
        with self.assertRaises(BlockingIOError):
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.assertEqual(json.loads(self.receipt_path.read_bytes())['status'], 'in_progress')
        rows = self.rows()
        self.assertEqual(sum(row['args'][0] == 'load-buffer' for row in rows), 1)
        after_stage = rows[next(i for i, row in enumerate(rows) if row['args'][0] == 'load-buffer') + 1:]
        self.assertFalse(any(row['args'][-1:] == ['Enter'] for row in after_stage
                             if row['args'][0] == 'send-keys' and '-l' not in row['args']))
        discards = [row['args'] for row in rows if row['args'][0] == 'delete-buffer']
        if fault == 'stage-held':
            self.assertEqual(discards, [], 'discard raced an unsettled Stage')
            self.assertFalse(any(row['args'][0] == 'paste-buffer' for row in rows))
            self.assertNotIn('input_pending_at', json.loads(self.path.read_bytes()))
        else:
            self.assertEqual(discards, [['delete-buffer', '-b', 'hgs-input-' + self.request['request_id']]])
            self.assertEqual(sum(row['args'][0] == 'paste-buffer' for row in rows), 1)
            self.assertIn('input_pending_at', json.loads(self.path.read_bytes()))
        peer.close()
        def released():
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                return True
            except BlockingIOError:
                return False
        self.case.wait_for(released)
        fcntl.flock(lock, fcntl.LOCK_UN)
        self.assertEqual([row['args'] for row in self.rows() if row['args'][0] == 'delete-buffer'], discards)

    def test_unsettled_stage_keeps_root_and_skips_discard_after_cleanup_lease(self):
        self.held_pipe_cleanup('stage-held')

    def test_unsettled_paste_allows_one_discard_and_retains_root_until_pipe_settlement(self):
        self.held_pipe_cleanup('paste-held')

    def test_racing_large_symlink_cache_uses_existing_semantics_after_keeper_settlement(self):
        import fcntl
        with (self.case.state / '.lock').open('r+b') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            sender = self.start([str(test_input.HGS), 'send', self.case.name, '--json'], self.raw)
            self.wait_for_slot()
            self.receipt_path.parent.mkdir(mode=0o700, exist_ok=True)
            target = self.case.root / 'large-cache.json'
            value = dict(status='submitted', name=self.case.name,
                         digest=hashlib.sha256(self.raw.encode()).hexdigest(),
                         opaque='x' * (2 << 20), float=0.12345678901234568)
            target.write_text(json.dumps(value))
            self.receipt_path.symlink_to(target)
            fcntl.flock(lock, fcntl.LOCK_UN)
        stdout, stderr = sender.communicate(timeout=20)
        self.assertEqual(sender.returncode, 0, stderr)
        self.assertGreater(len(stdout), 1 << 20)
        self.assertEqual(json.loads(stdout), value)
        self.assertEqual(self.rows(), [])
        self.assertEqual(json.loads(self.pending_path.read_bytes())['status'], 'scheduled')

    def test_zero_pending_different_sids_ignore_a_foreign_slot_and_keep_blocking_root(self):
        import fcntl
        self.record.pop('pending_settings')
        self.case.write_record()
        directory = self.case.root / 'second-agent'
        directory.mkdir()
        script = directory / 'agent.py'
        script.write_text(test_input.FAKE_AGENT)
        name = 'codex/second-input-test'
        run, cid = str(uuid.uuid4()), str(uuid.uuid4())
        self.case.tmux('new-session', '-d', '-s', name, '-x', '100', '-y', '30',
                       'env', 'INPUT_FIXTURE=' + str(directory), sys.executable, str(script))
        self.case.wait_for(lambda: (directory / 'agent-pid').exists()
                           and (directory / 'agent-pid').read_text().strip())
        pid = int((directory / 'agent-pid').read_text())
        start = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart='],
                                       env=dict(self.case.env, LC_ALL='C', TZ='UTC'), text=True).strip()
        pane = self.case.tmux('display-message', '-p', '-t', '=' + name + ':', '#{pane_id}').strip()
        self.case.tmux('set-option', '-t', '=' + name + ':', '@hgs_run', run)
        record = dict(version=1, name=name, agent='codex', run_id=run, conversation_id=cid,
                      pid=pid, process_start=start, pane=pane, activity='idle', phase='idle',
                      active_tools={}, subagents={}, last_event_at=time.time(), created=time.time())
        legacy = self.case.state / (hashlib.sha256(name.encode()).hexdigest() + '.json')
        legacy.write_text(json.dumps(record))
        record, _, _ = identity.moved_record(self.case, name, record, existing_machine=True, kind=self.proof_kind)
        self.assertNotEqual(record['session_id'], self.record['session_id'])
        body = json.dumps(dict(request_id=str(uuid.uuid4()), expected_run_id=run,
                               expected_conversation_id=cid, text='second exact session'))
        slot_path = self.case.state / '.native-picker-host.lock'
        with slot_path.open('w+b') as slot, (self.case.state / '.lock').open('r+b') as lock:
            slot_path.chmod(0o600)
            fcntl.flock(slot, fcntl.LOCK_EX)
            fcntl.flock(lock, fcntl.LOCK_EX)
            first = self.start([str(test_input.HGS), 'send', self.case.name, '--json'], self.raw)
            second = self.start([str(test_input.HGS), 'send', name, '--json'], body)
            time.sleep(.25)
            self.assertIsNone(first.poll())
            self.assertIsNone(second.poll())
            self.assertEqual(self.rows(), [])
            fcntl.flock(lock, fcntl.LOCK_UN)
            stdout, stderr = first.communicate(timeout=20)
            self.assert_submitted(subprocess.CompletedProcess(first.args, first.returncode, stdout, stderr))
            stdout, stderr = second.communicate(timeout=20)
            self.assertEqual(second.returncode, 0, stdout + stderr)
            self.assertEqual(json.loads(stdout)['status'], 'submitted')
            # The unrelated slot is still held during both successful sends.
            fcntl.flock(slot, fcntl.LOCK_UN)
        pastes = [row['args'] for row in self.rows() if row['args'][0] == 'paste-buffer']
        self.assertEqual(sorted(args[-1] for args in pastes), sorted([self.case.pane, pane]))
        self.case.wait_for(lambda: (directory / 'received').exists()
                           and (directory / 'received').read_bytes().endswith(b'\x1b[201~\r'))

    def test_zero_pending_parallel_sends_keep_blocking_root_and_submit_once(self):
        import fcntl
        self.record.pop('pending_settings')
        self.case.write_record()
        with (self.case.state / '.lock').open('r+b') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            command = [str(test_input.HGS), 'send', self.case.name, '--json']
            first = self.start(command, self.raw)
            second = self.start(command, self.raw)
            time.sleep(.25)
            self.assertIsNone(first.poll())
            self.assertIsNone(second.poll())
            self.assertFalse((self.case.state / '.native-picker-host.lock').exists())
            self.assertEqual(self.rows(), [])
            fcntl.flock(lock, fcntl.LOCK_UN)
        receipts = []
        for sender in (first, second):
            stdout, stderr = sender.communicate(timeout=20)
            receipts.append(self.assert_submitted(subprocess.CompletedProcess(
                sender.args, sender.returncode, stdout, stderr)))
        self.assertEqual(receipts[0], receipts[1])
        self.assertEqual(sum(row['args'][0] == 'paste-buffer' for row in self.rows()), 1)
        self.assertIsNone(next(row for row in self.rows() if row['args'][0] == 'load-buffer')['receipt'])

    def test_gui_auto_apply_slot_winner_keeps_sends_blocking_legacy_route(self):
        self.gui_auto_apply_race(failure=False)

    def test_gui_auto_apply_failure_keeps_the_original_legacy_send_refusal(self):
        self.gui_auto_apply_race(failure=True)

    def gui_auto_apply_race(self, *, failure):
        import socket
        auto = dict(request_id=str(uuid.uuid4()), expected_run_id=self.case.run_id,
                    expected_conversation_id=self.case.conversation_id, model='gpt-6-sol',
                    effort='low', expected_pending_id=self.pending)
        listener = socket.socket(socket.AF_UNIX)
        self.addCleanup(listener.close)
        listener.bind(str(self.case.root / 'auto-apply.sock'))
        listener.listen(1)
        listener.settimeout(5)
        settings = self.start([str(test_input.HGS), 'settings', self.case.name,
                               '--session-id', self.record['session_id'], '--json'], json.dumps(auto),
                              env=dict(self.case.env, SEND_HOLD_NATIVE='1'))
        peer, _ = listener.accept()
        self.addCleanup(peer.close)
        self.assertEqual(peer.recv(5), b'ready')
        self.wait_for_slot()
        self.assertEqual(json.loads(self.pending_path.read_bytes())['status'], 'scheduled')
        sender = self.start([str(test_input.HGS), 'send', self.case.name, '--json'], self.raw)
        time.sleep(.2)
        self.assertIsNone(sender.poll(), 'send stopped blocking on the legacy Root path')
        self.assertFalse(self.receipt_path.exists())
        self.assertEqual(self.rows(), [])
        if failure:
            changed = json.loads(self.pending_path.read_bytes())
            changed['status'] = 'applied'
            self.pending_path.write_text(json.dumps(changed))
        peer.close()
        stdout, stderr = settings.communicate(timeout=20)
        if failure:
            self.assertNotEqual(settings.returncode, 0)
            self.assertIn('pending settings are not safe to retry', stderr)
            stdout, stderr = sender.communicate(timeout=20)
            self.assertNotEqual(sender.returncode, 0)
            self.assertIn('pending settings are not safe to retry', stderr)
            self.assertNotIn('delivery uncertain', stderr)
            self.assertEqual(self.rows(), [])
            self.assertFalse(self.receipt_path.exists())
            self.assertNotIn('input_pending_at', json.loads(self.path.read_bytes()))
            return
        self.assertEqual(settings.returncode, 0, stdout + stderr)
        self.assertEqual(json.loads(stdout)['status'], 'applied')
        stdout, stderr = sender.communicate(timeout=20)
        self.assert_submitted(subprocess.CompletedProcess(sender.args, sender.returncode, stdout, stderr))
        rows = self.rows()
        first = next(row for row in rows if row['args'][0] == 'send-keys')
        self.assertIsNone(first['receipt'], 'a Send keeper or early outer receipt appeared on fallback')
        self.assertIsNone(next(row for row in rows if row['args'][0] == 'load-buffer')['receipt'])
        self.assertEqual(sum(row['args'][0] == 'paste-buffer' for row in rows), 1)


class ExistingIdSendFresh(ExistingIdSend):
    proof_kind = 'fresh'


if __name__ == '__main__':
    unittest.main()
