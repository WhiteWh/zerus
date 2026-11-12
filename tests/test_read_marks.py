"""Shared read marks on the session's host: `hgs ls --json` fields and `hgs read`."""
import fcntl
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import subprocess
import time
import threading
import unittest
import uuid

import receipt_locator
import test_input as fixtures

HGS = fixtures.HGS
HEX = re.compile(r'[0-9a-f]{64}')


class ReadMarks(unittest.TestCase):
    setUp = fixtures.InputTransport.setUp
    tearDown = fixtures.InputTransport.tearDown
    script = fixtures.InputTransport.script
    tmux = fixtures.InputTransport.tmux
    wait_for = fixtures.InputTransport.wait_for
    write_record = fixtures.InputTransport.write_record

    def listing(self):
        result = subprocess.run([str(HGS), 'ls', '--json', '--local'], env=self.env,
                                text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        return next(row for row in json.loads(result.stdout)['sessions'] if row['name'] == self.name)

    def reply(self, at):
        db = sqlite3.connect(self.state / 'events.sqlite3')
        with db:
            db.execute('CREATE TABLE IF NOT EXISTS events (seq INTEGER PRIMARY KEY, name TEXT, conversation TEXT, payload TEXT)')
            db.execute('INSERT INTO events(name, conversation, payload) VALUES (?, ?, ?)',
                       (self.name, self.conversation_id, json.dumps({'type': 'Stop', 'agent_id': '', 'at': at})))
        db.close()
        return self.listing()['reply_id']

    def waiting(self, tool='call-1'):
        self.record.update(phase='input', activity='busy',
                           active_tools={tool: dict(name='request_user_input', started=1.0, detail='')})
        self.write_record()

    def test_idle_session_lists_null_read_and_signature(self):
        row = self.listing()
        self.assertIn('read', row)
        self.assertIsNone(row['read'])
        self.assertIsNone(row['attention_signature'])
        self.assertEqual(row['optional_questions'], {'count': 0})

    def test_exact_inspection_observes_raw_host_mark_without_presentation_replacement(self):
        self.record_path.chmod(0o600)
        first = self.reply(100.5)
        self.waiting()
        signature = self.listing()['attention_signature']
        written = self.mark(reply_id=first, attention_signature=signature)['read']
        self.reply(200.5)
        uri_result = subprocess.run([str(HGS), 'reference', self.name, '--json'], env=self.env,
                                    text=True, capture_output=True, timeout=10)
        self.assertEqual(uri_result.returncode, 0, uri_result.stderr)
        uri = json.loads(uri_result.stdout)['uri']
        path = self.state / 'read-marks.json'
        original = path.read_bytes()
        for target in ([self.name, '--read-observation'],
                       [self.name, '--after', '0', '--skip-processes', '--read-observation'], [uri]):
            result = subprocess.run([str(HGS), 'inspect', *target], env=self.env,
                                    text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            row = json.loads(result.stdout)
            self.assertEqual(row['read'], written)
            self.assertNotEqual(row['reply_id'], first)
            observation = row['read_observation']
            self.assertTrue(observation['complete'])
            self.assertEqual(observation['freshness'], 'observed')
            self.assertEqual(observation['generation'], 'sha256:' + hashlib.sha256(original).hexdigest())
            self.assertEqual(observation['target'], dict(name=self.name, run_id=self.run_id,
                                                        conversation_id=self.conversation_id))
            self.assertGreater(observation['observed_at'], 0)
        self.assertEqual(path.read_bytes(), original)

    def test_inspection_distinguishes_absent_from_invalid_raw_mark_source(self):
        self.record_path.chmod(0o600)
        def inspect():
            result = subprocess.run([str(HGS), 'inspect', self.name, '--read-observation'], env=self.env,
                                    text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)
        row = inspect()
        self.assertIsNone(row['read'])
        self.assertEqual(row['read_observation']['source_state'], 'absent')
        self.assertTrue(row['read_observation']['complete'])
        path = self.state / 'read-marks.json'
        path.write_text('{broken source')
        path.chmod(0o600)
        row = inspect()
        self.assertIsNone(row['read'])
        self.assertFalse(row['read_observation']['complete'])
        self.assertEqual(row['read_observation']['freshness'], 'unknown')
        self.assertEqual(path.read_text(), '{broken source')

    def test_activity_and_process_poll_shapes_never_include_read_observation(self):
        self.record_path.chmod(0o600)
        reply = self.reply(100.5)
        self.mark(reply_id=reply)
        path = self.state / 'read-marks.json'
        marks = json.loads(path.read_text())
        # A shared store sized like a real fleet must not enter a detail poll.
        marks['sessions'].update({f'unrelated-{i}': {self.conversation_id: {
            'conversation_id': self.conversation_id, 'reply_id': 'unrelated-reply',
            'attention_signature': None, 'at': 1.0}} for i in range(20000)})
        path.write_text(json.dumps(marks))
        original = path.read_bytes()
        for args in ([], ['--after', '0', '--skip-processes'],
                     ['--after', '1', '--skip-processes'],
                     ['--after', str(2**63 - 1)]):
            with self.subTest(args=args):
                result = subprocess.run([str(HGS), 'inspect', self.name, *args], env=self.env,
                                        text=True, capture_output=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                row = json.loads(result.stdout)
                self.assertNotIn('read', row)
                self.assertNotIn('read_observation', row)
        self.assertEqual(path.read_bytes(), original)

    def inspect(self, *args):
        result = subprocess.run([str(HGS), 'inspect', self.name, *args], env=self.env,
                                text=True, capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_current_read_names_exactly_what_read_accepts_now(self):
        first = self.reply(100.5)
        self.waiting()
        row = self.listing()
        self.assertEqual(row['reply_id'], first)
        self.assertRegex(row['attention_signature'], HEX)
        expected = dict(schema=1, run_id=self.run_id, conversation_id=self.conversation_id, reply_id=first,
                        attention_signature=row['attention_signature'], attention_complete=True)
        for args in ([], ['--after', '0', '--skip-processes']):
            with self.subTest(args=args):
                self.assertEqual(self.inspect(*args, '--current-read')['current_read'], expected)
        for args in ([], ['--after', '0', '--skip-processes'], ['--read-observation']):
            with self.subTest(args=args):
                self.assertNotIn('current_read', self.inspect(*args))
        self.assertFalse((self.state / 'read-marks.json').exists())  # Inspecting acknowledges nothing.
        current = self.inspect('--current-read')['current_read']
        self.assertEqual(self.mark(reply_id=current['reply_id'], attention_signature=current['attention_signature'])['applied'],
                         {'reply_id': True, 'attention_signature': True})
        # A newer reply and request make the earlier values stale; they are never applied.
        self.reply(200.5)
        self.waiting('call-2')
        self.assertEqual(self.mark(reply_id=current['reply_id'], attention_signature=current['attention_signature'])['applied'],
                         {'reply_id': False, 'attention_signature': False})
        newer = self.inspect('--current-read')['current_read']
        self.assertNotEqual((newer['reply_id'], newer['attention_signature']),
                            (current['reply_id'], current['attention_signature']))
        self.assertEqual(self.mark(reply_id=newer['reply_id'], attention_signature=newer['attention_signature'])['applied'],
                         {'reply_id': True, 'attention_signature': True})

    def test_current_read_never_signs_unindexed_questions(self):
        self.codex_question()
        known = self.listing()['attention_signature']
        self.assertRegex(known, HEX)
        self.assertEqual(self.inspect('--current-read')['current_read']['attention_signature'], known)
        digest = hashlib.sha256(json.dumps([None, self.conversation_id], separators=(',', ':')).encode()).hexdigest()
        path = self.state / 'codex_questions' / f'{digest}.lock'
        # Hold the question index longer than the one-second budget, then let the
        # inspection's own (unbudgeted) question listing finish. A slow start can
        # reach the signed read after the release: then it must be complete and
        # equal to the known signature, and a longer hold is tried.
        for hold in (2.5, 5.0, 10.0):
            with self.subTest(hold=hold), path.open('a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                child = subprocess.Popen([str(HGS), 'inspect', self.name, '--current-read'], env=self.env,
                                         text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                time.sleep(hold)
                fcntl.flock(lock, fcntl.LOCK_UN)
                out, err = child.communicate(timeout=60)
                self.assertEqual(child.returncode, 0, err)
                current = json.loads(out)['current_read']
                if current['attention_complete']:
                    self.assertEqual(current['attention_signature'], known)
                    continue
                self.assertNotIn(current['attention_signature'], (known, None))
                self.assertFalse(self.mark(attention_signature=current['attention_signature'])['applied']['attention_signature'])
                break
        else:
            self.fail('the signed question read never ran while the index was held')

    def test_signature_ignores_status_updates_and_changes_for_a_new_request(self):
        self.waiting()
        first = self.listing()['attention_signature']
        self.assertRegex(first, HEX)
        self.record.update(last_event_at=self.record['last_event_at'] + 50, prompt='A different status line',
                           subagents={'worker': dict(state='working', name='Worker', updated=5.0)})
        self.write_record()
        self.assertEqual(self.listing()['attention_signature'], first)
        self.waiting('call-2')
        second = self.listing()['attention_signature']
        self.assertRegex(second, HEX)
        self.assertNotEqual(second, first)
        self.record.update(phase='working', active_tools={})
        self.write_record()
        self.assertIsNone(self.listing()['attention_signature'])

    def test_repeated_identity_less_errors_have_distinct_signatures(self):
        self.record.update(phase='error', activity='attention', active_tools={}, turn_started=100.0)
        self.write_record()
        first = self.listing()['attention_signature']
        self.assertRegex(first, HEX)
        self.record.update(phase='working', activity='busy', turn_started=200.0)
        self.write_record()
        self.assertIsNone(self.listing()['attention_signature'])
        self.record.update(phase='error', activity='attention')
        self.write_record()
        second = self.listing()['attention_signature']
        self.assertRegex(second, HEX)
        self.assertNotEqual(second, first)

    def read(self, name=None, remote=False, **fields):
        payload = {'run_id': self.run_id, 'conversation_id': self.conversation_id,
                   'reply_id': None, 'attention_signature': None, **fields}
        return subprocess.run([str(HGS), *(['@remote'] if remote else []), 'read', name or self.name, '--json'],
                              input=json.dumps(payload), env=self.env, text=True, capture_output=True, timeout=10)

    def mark(self, **fields):
        result = self.read(**fields)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_mark_applies_only_current_values_and_old_marks_stay_stale(self):
        first = self.reply(100.5)
        result = self.mark(reply_id=first)
        self.assertEqual(result['ok'], True)
        self.assertEqual(result['applied'], {'reply_id': True, 'attention_signature': False})
        self.assertEqual(set(result['read']), {'conversation_id', 'reply_id', 'attention_signature', 'at'})
        self.assertEqual(self.listing()['read']['reply_id'], first)
        second = self.reply(200.5)
        self.assertEqual(self.mark(reply_id=first)['applied'], {'reply_id': False, 'attention_signature': False})
        row = self.listing()
        self.assertEqual((row['reply_id'], row['read']['reply_id']), (second, first))
        self.assertFalse(self.mark(reply_id=second, run_id='another-run')['applied']['reply_id'])
        self.assertFalse(self.mark(reply_id=second, conversation_id='another-conversation')['applied']['reply_id'])
        self.assertEqual(self.listing()['read']['reply_id'], first)

    def test_waiting_old_reader_preserves_a_newer_writer_mark(self):
        first = self.reply(100.5)
        self.mark(reply_id=first)
        lock_path = self.state / 'read-marks.lock'
        payload = dict(run_id=self.run_id, conversation_id=self.conversation_id,
                       reply_id=first, attention_signature=None)
        with lock_path.open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            reader = subprocess.Popen([str(HGS), 'read', self.name, '--json'], env=self.env,
                                      stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            def cleanup():
                if reader.poll() is None:
                    reader.terminate()
                reader.wait(timeout=5)
            self.addCleanup(cleanup)
            reader.stdin.write(json.dumps(payload))
            reader.stdin.close()
            reader.stdin = None
            def waiting_on_writer():
                self.assertIsNone(reader.poll())
                descriptors = Path(f'/proc/{reader.pid}/fd')
                if descriptors.is_dir():
                    return any(fd.resolve() == lock_path.resolve() for fd in descriptors.iterdir())
                # macOS exposes the same open-file evidence through lsof.
                files = subprocess.run(['lsof', '-a', '-p', str(reader.pid), '-Fn'],
                                       capture_output=True, text=True, timeout=5).stdout.splitlines()
                return any(line.startswith('n') and Path(line[1:]).resolve() == lock_path.resolve() for line in files)
            self.wait_for(waiting_on_writer)
            # The writer holding this lock observes a newer reply and stores
            # its mark before releasing the waiting reader.
            second = self.reply(200.5)
            path = self.state / 'read-marks.json'
            marks = json.loads(path.read_text())
            mark = next(iter(marks['sessions'].values()))[self.conversation_id]
            mark.update(reply_id=second, at=time.time())
            path.write_text(json.dumps(marks))
            fcntl.flock(lock, fcntl.LOCK_UN)
        output, error = reader.communicate(timeout=10)
        self.assertEqual(reader.returncode, 0, error)
        self.assertFalse(json.loads(output)['applied']['reply_id'])
        self.assertEqual(self.listing()['read']['reply_id'], second)

    def test_attention_mark_holds_across_status_updates_until_a_new_request(self):
        self.waiting()
        signature = self.listing()['attention_signature']
        self.assertTrue(self.mark(attention_signature=signature)['applied']['attention_signature'])
        self.record.update(last_event_at=self.record['last_event_at'] + 10)
        self.write_record()
        row = self.listing()
        self.assertEqual(row['read']['attention_signature'], row['attention_signature'])
        self.waiting('call-2')
        row = self.listing()
        self.assertNotEqual(row['read']['attention_signature'], row['attention_signature'])

    def test_repeated_current_mark_keeps_the_exact_legacy_file(self):
        reply = self.reply(100.5)
        first = self.mark(reply_id=reply)
        path = self.state / 'read-marks.json'
        before = path.read_bytes(), path.stat()
        duplicate = self.mark(reply_id=reply)
        self.assertTrue(duplicate['applied']['reply_id'])
        self.assertEqual(duplicate['read'], first['read'])
        self.assertEqual(path.read_bytes(), before[0])
        self.assertEqual((path.stat().st_ino, path.stat().st_mtime_ns),
                         (before[1].st_ino, before[1].st_mtime_ns))

    def test_existing_owner_retains_other_marks_until_new_owner_admission(self):
        self.mark(reply_id=self.reply(100.5))
        path = self.state / 'read-marks.json'
        marks = json.loads(path.read_text())
        abandoned = {'old-conversation': {'reply_id': 'old', 'at': 1.0}}
        marks['sessions']['codex/abandoned/session'] = abandoned
        path.write_text(json.dumps(marks))
        self.mark(reply_id=self.reply(200.5))
        self.assertEqual(json.loads(path.read_text())['sessions']['codex/abandoned/session'], abandoned)
        # Removing this owner's entry makes its next write a new admission.
        marks = json.loads(path.read_text())
        del marks['sessions'][self.name]
        path.write_text(json.dumps(marks))
        self.mark(reply_id=self.listing()['reply_id'])
        self.assertNotIn('codex/abandoned/session', json.loads(path.read_text())['sessions'])

    def test_corrupt_existing_store_is_not_reset_by_a_current_read(self):
        reply = self.reply(100.5)
        path = self.state / 'read-marks.json'
        for text in ('{', '{"version":2,"sessions":{}}', '{"version":1,"sessions":[]}'):
            path.write_text(text)
            path.chmod(0o600)
            before = path.stat()
            result = self.read(reply_id=reply)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(result.stdout, '')
            self.assertIn('Shared read marks could not be read', result.stderr)
            self.assertEqual(path.read_text(), text)
            self.assertEqual((path.stat().st_ino, path.stat().st_mtime_ns),
                             (before.st_ino, before.st_mtime_ns))

    def test_read_does_not_reconcile_an_unrelated_completed_binding(self):
        reply = self.reply(100.5)
        # This complete stopped legacy Claude binding is eligible for the normal
        # global archive sweep, but not for this selected-session read command.
        other = dict(version=1, name='claude/other/finished', agent='claude',
                     run_id=str(uuid.uuid4()), conversation_id=str(uuid.uuid4()),
                     conversation_state='ended', pid=0, process_start='',
                     phase='idle', activity='idle', created=1.0)
        other['session_end'] = dict(run_id=other['run_id'],
                                   conversation_id=other['conversation_id'], reason='prompt_input_exit')
        path = self.state / (hashlib.sha256(other['name'].encode()).hexdigest() + '.json')
        path.write_text(json.dumps(other))
        before = path.read_bytes()
        self.mark(reply_id=reply)
        self.assertEqual(path.read_bytes(), before)
        self.assertFalse((self.state / 'archives').exists())
        self.listing()  # Ordinary lifecycle/listing reconciliation still runs.
        self.assertFalse(path.exists())
        self.assertTrue(list((self.state / 'archives').glob('*.json')))

    def test_selected_archived_identity_is_refused_even_with_a_live_pane(self):
        reply = self.reply(100.5)
        self.mark(reply_id=reply)
        marks = (self.state / 'read-marks.json').read_bytes()
        archives = self.state / 'archives'
        archives.mkdir()
        archive_id = str(uuid.uuid4())
        archived = dict(self.record, archive_id=archive_id, archived_at=1.0, state='archived')
        (archives / (archive_id + '.json')).write_text(json.dumps(archived))
        # No intervening listing primes the archive metadata index.
        result = self.read(reply_id=reply)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(json.loads(result.stdout), {'ok': False, 'error': 'not_found'})
        self.assertEqual((self.state / 'read-marks.json').read_bytes(), marks)
        self.assertTrue(self.record_path.exists())

    def test_selected_clean_completion_is_still_archived_before_reading(self):
        reply = self.reply(100.5)
        self.mark(reply_id=reply)
        marks = (self.state / 'read-marks.json').read_bytes()
        self.tmux('kill-session', '-t', '=' + self.name)
        self.record.update(conversation_state='ended', pid=0, process_start='',
                           completion_source='supervisor', completion_reason='clean_exit', exit_code=0,
                           session_end=dict(run_id=self.run_id, conversation_id=self.conversation_id, reason='other'))
        self.write_record()
        result = self.read(reply_id=reply)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(json.loads(result.stdout), {'ok': False, 'error': 'not_found'})
        self.assertFalse(self.record_path.exists())
        self.assertTrue(list((self.state / 'archives').glob('*.json')))
        self.assertEqual((self.state / 'read-marks.json').read_bytes(), marks)

    def test_second_identity_less_error_is_unacknowledged(self):
        self.record.update(phase='error', activity='attention', active_tools={}, turn_started=100.0)
        self.write_record()
        self.mark(attention_signature=self.listing()['attention_signature'])
        self.record.update(phase='working', activity='busy', turn_started=200.0)
        self.write_record()
        self.record.update(phase='error', activity='attention')
        self.write_record()
        row = self.listing()
        self.assertNotEqual(row['attention_signature'], row['read']['attention_signature'])

    def hook(self, kind, session_id=None, **fields):
        event = dict(session_id=session_id or self.conversation_id, hook_event_name=kind, **fields)
        result = subprocess.run([str(HGS), '__state', 'hook'], input=json.dumps(event), text=True, capture_output=True,
                                env=dict(self.env, HGS_SESSION=self.name, HGS_RUN_ID=self.run_id), timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)

    def acknowledged(self):
        row = self.listing()
        self.assertRegex(row['attention_signature'], HEX)
        return row['attention_signature'] == (row['read'] or {}).get('attention_signature')

    def acknowledge(self):
        self.assertTrue(self.mark(attention_signature=self.listing()['attention_signature'])['applied']['attention_signature'])
        self.assertTrue(self.acknowledged())

    def test_second_identity_less_approval_is_unacknowledged(self):
        self.hook('PermissionRequest', tool_name='Bash')
        self.acknowledge()
        # A child's progress is a status-only update for the pending request.
        self.hook('SubagentStart', agent_id='worker', agent_type='helper')
        self.assertTrue(self.acknowledged())
        self.hook('PostToolUse', tool_name='Bash')
        self.assertIsNone(self.listing()['attention_signature'])
        self.hook('PermissionRequest', tool_name='Bash')
        self.assertFalse(self.acknowledged())
        # Also when the next prompt follows without a working state in between.
        self.acknowledge()
        self.hook('PermissionRequest', tool_name='Bash')
        self.assertFalse(self.acknowledged())

    def test_second_input_of_a_tool_keyed_by_name_is_unacknowledged(self):
        self.hook('PreToolUse', tool_name='request_user_input')
        self.assertEqual(self.listing()['attention_id'], 'request_user_input')
        self.acknowledge()
        self.hook('PostToolUse', tool_name='request_user_input')
        self.hook('PreToolUse', tool_name='request_user_input')
        row = self.listing()
        self.assertEqual((row['phase'], row['attention_id']), ('input', 'request_user_input'))
        self.assertFalse(self.acknowledged())

    def test_second_child_approval_is_unacknowledged(self):
        child = 'session_' + str(uuid.uuid4())
        self.hook('SessionStart', session_id=child)
        self.hook('PermissionRequest', session_id=child, tool_name='Shell')
        children = self.listing()['subagents']
        self.assertEqual([c['display_state'] for c in children.values()], ['approval'])
        self.acknowledge()
        self.hook('PostToolUse', session_id=child, tool_name='Shell')
        self.assertIsNone(self.listing()['attention_signature'])
        self.hook('PermissionRequest', session_id=child, tool_name='Shell')
        self.assertFalse(self.acknowledged())

    def codex_question(self):
        """A Codex rollout with one accepted optional question that is still unanswered."""
        rollout = self.root / '.codex/sessions' / f'rollout-2026-10-11T00-00-00-{self.conversation_id}.jsonl'
        rollout.parent.mkdir(parents=True)
        call = dict(type='function_call', name='request_user_input_async', call_id='call-question',
                    arguments=json.dumps({'questions': [{'title': 'Keep the fixture?', 'options': ['Yes', 'No']}]}))
        accepted = dict(type='function_call_output', call_id='call-question', output=json.dumps({'accepted': True}))
        lines = [dict(type='session_meta', payload=dict(id=self.conversation_id)),
                 dict(type='response_item', timestamp='2026-10-11T00:00:01Z', payload=call),
                 dict(type='response_item', timestamp='2026-10-11T00:00:02Z', payload=accepted)]
        rollout.write_text(''.join(json.dumps(line) + '\n' for line in lines))
        self.record.update(transcript=str(rollout), phase='working', activity='busy')
        self.write_record()
        # Question recovery reads the receipt locator (zerus-a4mub.8); a fresh
        # state lists the questions as unknown until it exists, which these
        # read-mark tests do not measure.
        receipt_locator.settle(HGS, self.env)

    def test_unindexed_questions_are_never_acknowledged(self):
        self.codex_question()
        row = self.listing()
        known, optional = row['attention_signature'], row['optional_questions']
        self.assertRegex(known, HEX)  # The pending question needs attention.
        self.assertEqual(optional, {'count': 1, 'ids': ['codex-async:call-question:0'], 'latest_at': 1791676801.0})
        self.assertEqual(row['phase'], 'working')
        self.assertTrue(self.mark(attention_signature=known)['applied']['attention_signature'])
        # Another process holds the question index for longer than a listing's budget.
        digest = hashlib.sha256(json.dumps([None, self.conversation_id], separators=(',', ':')).encode()).hexdigest()
        lock = (self.state / 'codex_questions' / f'{digest}.lock').open('a')
        self.addCleanup(lock.close)
        fcntl.flock(lock, fcntl.LOCK_EX)
        started = time.monotonic()
        row = self.listing()
        self.assertLess(time.monotonic() - started, 5)
        self.assertRegex(row['attention_signature'], HEX)
        self.assertNotIn(row['attention_signature'], (known, row['read']['attention_signature']))
        self.assertIsNone(row['optional_questions'])  # Unknown, never a count of 0.
        # `hgs read` cannot index it either and never acknowledges an unknown state.
        self.assertFalse(self.mark(attention_signature=row['attention_signature'])['applied']['attention_signature'])
        fcntl.flock(lock, fcntl.LOCK_UN)
        row = self.listing()
        self.assertEqual((row['attention_signature'], row['read']['attention_signature']), (known, known))
        self.assertEqual(row['optional_questions'], optional)

    def test_unreadable_question_index_lists_unknown_optional_questions(self):
        self.codex_question()
        self.assertEqual(self.listing()['optional_questions']['count'], 1)
        # A transcript of another conversation cannot be indexed. As before,
        # the listing adds no attention for it; the questions are unknown.
        rollout = Path(self.record['transcript'])
        lines = rollout.read_text().splitlines(keepends=True)
        rollout.write_text(json.dumps(dict(type='session_meta', payload=dict(id=str(uuid.uuid4())))) + '\n'
                           + ''.join(lines[1:]))
        row = self.listing()
        self.assertIsNone(row['optional_questions'])
        self.assertIsNone(row['attention_signature'])
        self.assertEqual(row['phase'], 'working')

    def test_current_read_reports_an_unreadable_question_index_as_incomplete(self):
        self.codex_question()
        self.assertTrue(self.inspect('--current-read')['current_read']['attention_complete'])
        # zerus-a4mub.28: an index that cannot be read leaves the questions unknown.
        # The listing and `hgs read` keep counting it as none.
        rollout = Path(self.record['transcript'])
        lines = rollout.read_text().splitlines(keepends=True)
        rollout.write_text(json.dumps(dict(type='session_meta', payload=dict(id=str(uuid.uuid4())))) + '\n'
                           + ''.join(lines[1:]))
        self.assertIsNone(self.listing()['attention_signature'])
        row = self.inspect('--current-read')
        self.assertIn('pending_questions_error', row)
        current = row['current_read']
        self.assertFalse(current['attention_complete'])
        self.assertRegex(current['attention_signature'], HEX)
        self.assertFalse(self.mark(attention_signature=current['attention_signature'])['applied']['attention_signature'])
        self.assertFalse((self.state / 'read-marks.json').exists())

    def test_saved_session_lists_no_optional_questions(self):
        self.codex_question()
        self.assertEqual(self.listing()['optional_questions']['count'], 1)
        self.tmux('kill-session', '-t', '=' + self.name)
        row = self.listing()
        self.assertEqual(row['state'], 'stopped')
        self.assertEqual(row['optional_questions'], {'count': 0})

    def test_unknown_session_reports_not_found(self):
        result = self.read(name='codex/missing-session', reply_id='1:1.0')
        self.assertEqual(result.returncode, 1)
        self.assertEqual(json.loads(result.stdout), {'ok': False, 'error': 'not_found'})

    def test_malformed_marks_change_nothing(self):
        reply = self.reply(100.5)
        for fields in (dict(run_id=''), dict(reply_id=7), dict(attention_signature='A' * 64),
                       dict(attention_signature='a' * 63), dict(request_id='fixture')):
            result = self.read(**{'reply_id': reply, **fields})
            self.assertEqual(result.returncode, 1, fields)
            self.assertEqual(result.stdout, '', fields)
        self.assertIsNone(self.listing()['read'])
        self.assertFalse((self.state / 'read-marks.json').exists())

    def test_concurrent_writers_keep_both_fields(self):
        reply = self.reply(100.5)
        self.waiting()
        signature = self.listing()['attention_signature']
        base = {'run_id': self.run_id, 'conversation_id': self.conversation_id, 'reply_id': None, 'attention_signature': None}
        writers = []
        for fields in (dict(reply_id=reply), dict(attention_signature=signature)):
            process = subprocess.Popen([str(HGS), 'read', self.name, '--json'], stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=self.env, text=True)
            writers.append((process, json.dumps({**base, **fields})))
        for process, payload in writers:
            _, error = process.communicate(payload, timeout=10)
            self.assertEqual(process.returncode, 0, error)
        read = self.listing()['read']
        self.assertEqual((read['reply_id'], read['attention_signature']), (reply, signature))

    def test_marks_follow_a_renamed_session(self):
        reply = self.reply(100.5)
        self.mark(reply_id=reply)
        subprocess.run([str(HGS), 'rename', self.name, 'codex/input-test/renamed'], env=self.env,
                       text=True, capture_output=True, check=True, timeout=10)
        self.name = 'codex/input-test/renamed'
        row = self.listing()
        self.assertEqual((row['reply_id'], row['read']['reply_id']), (reply, reply))

    def test_remote_mark_forwards_stdin_without_a_terminal(self):
        self.script('ssh', """#!/usr/bin/env python3
import json, os, pathlib, sys
pathlib.Path(os.environ['INPUT_FIXTURE'], 'ssh-args').write_text(json.dumps(sys.argv[1:]))
os.execv('/bin/sh', ['sh', '-c', sys.argv[-1]])
""")
        reply = self.reply(100.5)
        result = self.read(remote=True, reply_id=reply)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(json.loads(result.stdout)['applied']['reply_id'])
        args = json.loads((self.root / 'ssh-args').read_text())
        self.assertNotIn('-t', args)
        self.assertIn('BatchMode=yes', args)
        self.assertEqual(args[-1], '~/.local/bin/hgs read codex/input-test --json')
        missing = self.read(remote=True, name='codex/missing-session', reply_id=reply)
        self.assertEqual(missing.returncode, 1)
        self.assertEqual(json.loads(missing.stdout), {'ok': False, 'error': 'not_found'})

    def test_ordinary_local_and_peer_reads_close_both_output_pipes_after_exit(self):
        # Actual hgs dispatch and remote argv. The isolated SSH shim execs the
        # selected command; it neither starts nor emulates a persistent master.
        self.script('ssh', """#!/usr/bin/env python3
import json, os, pathlib, sys
pathlib.Path(os.environ['INPUT_FIXTURE'], 'ssh-args').write_text(json.dumps(sys.argv[1:]))
os.execv('/bin/sh', ['sh', '-c', sys.argv[-1]])
""")
        reply = self.reply(100.5)
        payload = json.dumps(dict(run_id=self.run_id, conversation_id=self.conversation_id,
                                  reply_id=reply, attention_signature=None)).encode()
        for remote in (False, True):
            with self.subTest(remote=remote):
                process = subprocess.Popen([str(HGS), *(['@remote'] if remote else []),
                    'read', self.name, '--json'], env=self.env, stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                output = [None, None]
                eof = [threading.Event(), threading.Event()]
                def reader(index, pipe):
                    try:
                        output[index] = pipe.read()
                    finally:
                        eof[index].set()
                readers = [threading.Thread(target=reader, args=(i, pipe), daemon=True)
                           for i, pipe in enumerate((process.stdout, process.stderr))]
                for thread in readers:
                    thread.start()
                process.stdin.write(payload)
                process.stdin.close()
                try:
                    self.assertEqual(process.wait(timeout=10), 0)
                    deadline = time.monotonic() + 2
                    for ended in eof:
                        self.assertTrue(ended.wait(max(0, deadline - time.monotonic())),
                                        'ordinary read retained an output pipe after owned exit')
                    self.assertTrue(json.loads(output[0])['applied']['reply_id'])
                    self.assertEqual(output[1], b'')
                finally:
                    if process.poll() is None:
                        process.terminate()  # Only this test's owned helper.
                        process.wait(timeout=5)
                    for i, thread in enumerate(readers):
                        thread.join(timeout=.1)
                        if not thread.is_alive():
                            (process.stdout, process.stderr)[i].close()
                if remote:
                    args = json.loads((self.root / 'ssh-args').read_text())
                    self.assertFalse(any('ControlPersist' in arg for arg in args), args)


if __name__ == '__main__':
    unittest.main()
