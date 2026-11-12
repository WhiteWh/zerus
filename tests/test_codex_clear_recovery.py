"""Original process/file/caller proof for requested and Terminal clear successors.

Native-like C processes and real hgs hooks use only private homes/tmux. This is
not an owner-session operation or a claim about a model/provider response.
"""
import copy
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
import uuid

import test_clear_context as clear
import test_input


class CodexClearRecovery(unittest.TestCase):
    setUp = clear.ClearContext.setUp
    tearDown = test_input.InputTransport.tearDown
    script = test_input.InputTransport.script
    write_record = test_input.InputTransport.write_record
    tmux = test_input.InputTransport.tmux
    wait_for = test_input.InputTransport.wait_for
    received = test_input.InputTransport.received
    payload = test_input.InputTransport.payload
    send = test_input.InputTransport.send
    hook = clear.ClearContext.hook
    inspect = clear.ClearContext.inspect

    def poisoned(self, outside=False):
        self.successor = str(uuid.uuid4())
        self.account = self.root / 'account'
        sessions = self.account / 'sessions'
        sessions.mkdir(parents=True)
        directory = self.root / 'other-account' / 'sessions' if outside else sessions
        directory.mkdir(parents=True, exist_ok=True)
        self.transcript = directory / ('rollout-' + self.successor + '.jsonl')
        self.transcript.touch()
        # The actual tracked C process opens this file itself. Its child execs
        # hgs directly, so the positive has real ancestry rather than a claimed
        # role in a JSON request or a hook launched by this Python controller.
        environment = dict(HGS_SESSION=self.name, HGS_RUN_ID=self.run_id,
                           HGS_STATE_DIR=str(self.state), HGS_CONFIG_DIR=str(self.root / 'config'))
        assignments = ''.join('setenv(' + json.dumps(k) + ',' + json.dumps(v) + ',1);'
                              for k, v in environment.items())
        emitter = r'''
static void emit_hook(int nested) {
    pid_t child = fork();
    if (!child) {
        if (nested) { execl(NATIVE, "codex", "fixture-nested-hook", (char *)0); _exit(127); }
        int input = open("recovery-event.json", O_RDONLY);
        if (input < 0 || dup2(input, 0) < 0) _exit(126);
        close(input);
        ENVIRONMENT
        execl(EXECUTABLE, "hgs", "__state", "hook", (char *)0); _exit(127);
    }
    if (child < 0) _exit(125);
    int status, waited = 0;
    for (int i = 0; i < 10000; ++i) {
        if (waitpid(child, &status, WNOHANG) == child) { waited = 1; break; }
        usleep(1000);
    }
    if (!waited) { kill(child, SIGKILL); waitpid(child, &status, 0); }
    char result[32]; int size = snprintf(result, sizeof result, "%d", waited && WIFEXITED(status) ? WEXITSTATUS(status) : 125);
    if (!nested) {
        /* Expose the open/write window deterministically in the reader control. */
        if (!access("delay-hook-result", F_OK)) {
            close(open("hook-result", O_CREAT | O_WRONLY, 0600));
            usleep(100000);
        }
        append("hook-result", result, size);
    }
}
'''.replace('ENVIRONMENT', assignments).replace('EXECUTABLE', json.dumps(str(test_input.HGS)))
        emitter = emitter.replace('NATIVE', json.dumps(str(self.bin / 'codex')))
        source = clear.RESUMED_CODEX.replace('/fixture', str(self.root)).replace('#include <unistd.h>',
                    '#include <unistd.h>\n#include <stdlib.h>\n#include <signal.h>\n#include <sys/wait.h>')
        source = source.replace('int main(int argc, char **argv) {', emitter + '''
int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "fixture-nested-hook")) { emit_hook(0); return 0; }
''')
        source = source.replace('open(argv[2], O_RDONLY) < 0',
                    '(open(argv[2], O_RDONLY) < 0 || open(' + json.dumps(str(self.transcript)) + ', O_RDONLY) < 0)')
        source = source.replace('if (!strcmp(text, "/clear")',
                    'if (!strcmp(text, "fixture-hook")) { emit_hook(!access("nested-caller", F_OK)); } else if (!strcmp(text, "/clear")')
        test_input.InputTransport.resumed_unconfirmed(self, native=source)
        import time
        self.record.update(agent_home=str(self.account), cwd=str(self.root),
                           error='agent resumed a different conversation', conversation_state='ended',
                           active_tools={}, subagents={}, last_event_at=time.time() - .4,
                           session_end=dict(run_id=self.run_id, conversation_id=self.conversation_id, at=time.time() - .5),
                           clear_context_request=dict(request_id=str(uuid.uuid4()),
                                                      conversation_id=self.conversation_id, at=time.time() - 1))
        self.write_record()
        self.header = dict(type='session_meta', payload=dict(id=self.successor, source='cli',
                           timestamp=datetime.now(timezone.utc).isoformat(), cwd=str(self.root)))
        self.write_header()
        self.event = dict(hook_event_name='PreToolUse', session_id=self.successor,
                          transcript_path=str(self.transcript), cwd=str(self.root),
                          tool_name='Bash', tool_use_id='original-call', tool_input=dict(command='fixture only'))

    def write_header(self):
        # Preserve the same inode the original process has open.
        self.transcript.write_text(json.dumps(self.header) + '\n')

    def emit(self, event=None, expected=0):
        (self.root / 'hook-result').unlink(missing_ok=True)
        (self.root / 'recovery-event.json').write_text(json.dumps(event or self.event))
        self.tmux('send-keys', '-t', self.pane, '-l', 'fixture-hook')
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        result = self.root / 'hook-result'
        # open(O_CREAT) publishes the name before the native fixture writes its status.
        self.wait_for(lambda: result.exists() and result.read_text())
        self.assertEqual((self.root / 'hook-result').read_text(), str(expected))
        return json.loads(self.record_path.read_text())

    def terminal_end(self, outside=False):
        self.poisoned(outside=outside)
        for key in ('error', 'clear_context_request', 'session_end'):
            self.record.pop(key, None)
        self.record.update(last_event_at=0, conversation_state='active',
                           prompt='Previous fixture request', last_message='Previous fixture answer')
        self.write_record()
        self.event = dict(hook_event_name='SessionStart', source='clear', session_id=self.successor,
                          transcript_path=str(self.transcript), cwd=str(self.root))
        end = dict(hook_event_name='SessionEnd', session_id=self.conversation_id,
                   transcript_path=self.record['transcript'], cwd=str(self.root), reason='other')
        self.hook(end)
        self.assertNotIn('terminal_resume', json.loads(self.record_path.read_text())['session_end'])
        self.write_record()  # Only the actual supervised caller may create the binding.
        self.record = self.emit(end)
        return self.record

    def assert_supervised_holds_successor(self):
        pid = self.record['pid']
        if sys.platform == 'linux':
            descriptors = list((Path('/proc') / str(pid) / 'fd').iterdir())
            self.assertLess(len(descriptors), 64)
            held = [os.readlink(fd) for fd in descriptors]
        else:
            output = subprocess.check_output(['/usr/sbin/lsof', '-a', '-p', str(pid), '-Fn'],
                                             text=True, timeout=5)
            held = [line[1:] for line in output.splitlines() if line.startswith('n')]
        self.assertIn(str(self.transcript.resolve()), held)

    def assert_nested_caller_is_refused(self):
        self.assert_supervised_holds_successor()
        original = copy.deepcopy(self.record)
        nested = self.root / 'nested-caller'
        nested.touch()
        refused = self.emit()
        self.assertEqual(refused['conversation_id'], self.conversation_id)
        self.assertEqual(refused['transcript'], original['transcript'])
        self.assertEqual(refused['expected_id'], self.conversation_id)
        self.assertFalse(refused.get('session_clear'))
        # The same event and held files succeed when emitted by the parent.
        nested.unlink()
        self.record = original; self.write_record()
        accepted = self.emit()
        self.assertEqual(accepted['conversation_id'], self.successor)
        self.assertNotIn('error', accepted)
        self.assertEqual((accepted['pid'], accepted['run_id']), (original['pid'], self.run_id))

    def test_requested_recovery_requires_the_supervised_caller_even_when_it_holds_the_successor(self):
        self.poisoned()
        self.assert_nested_caller_is_refused()

    def test_terminal_clear_requires_the_supervised_caller_even_when_it_holds_the_successor(self):
        self.terminal_end()
        self.assert_nested_caller_is_refused()

    def assert_outside_successor_is_refused(self):
        self.assertFalse(self.transcript.is_relative_to(self.account / 'sessions'))
        self.assert_supervised_holds_successor()
        original = copy.deepcopy(self.record)
        refused = self.emit()
        self.assertEqual(refused['conversation_id'], self.conversation_id)
        self.assertEqual(refused['transcript'], original['transcript'])
        self.assertEqual(refused['expected_id'], self.conversation_id)
        self.assertEqual((refused['pid'], refused['run_id']), (original['pid'], self.run_id))
        self.assertFalse(refused.get('session_clear'))

    def test_requested_recovery_refuses_an_actually_held_successor_outside_the_account_sessions(self):
        self.poisoned(outside=True)
        self.assert_outside_successor_is_refused()

    def test_terminal_clear_refuses_an_actually_held_successor_outside_the_account_sessions(self):
        self.terminal_end(outside=True)
        self.assert_outside_successor_is_refused()

    def test_terminal_end_needs_a_real_reset_before_input_and_a_real_successor_before_rebinding(self):
        self.terminal_end()
        self.assertIn('terminal_resume', self.record['session_end'])
        info = self.inspect()
        self.assertFalse(info.get('session_clear'))
        self.assertFalse(info.get('resume_message_can_send'))
        self.send(self.payload(text='Blocked before reset'), success=False)
        self.assertNotIn(b'Blocked before reset', self.received())
        self.tmux('send-keys', '-t', self.pane, '-l', '/clear')
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        self.wait_for(lambda: (self.root / 'native-cleared').exists())
        info = self.inspect()
        self.assertTrue(info['session_clear'])
        self.assertTrue(info['resume_message_can_send'], info.get('resume_message_reason'))
        observed = json.loads(self.record_path.read_text())
        self.assertTrue(observed['session_clear']['awaiting_session_start'])
        self.assertNotIn('clear_context_request', observed)
        self.hook(self.event)  # The controller cannot supply the real native caller.
        self.assertEqual(json.loads(self.record_path.read_text())['conversation_id'], self.conversation_id)
        self.record = observed; self.write_record()
        self.send(self.payload(text='Fresh message after reset'))
        self.wait_for(lambda: (self.root / 'submitted').exists()
                      and b'Fresh message after reset' in (self.root / 'submitted').read_bytes())
        current = self.emit()
        self.assertEqual(current['conversation_id'], self.successor)
        self.assertNotIn('error', current)
        self.assertNotIn('expected_id', current)
        self.assertNotIn('session_end', current)
        self.assertEqual((current['pid'], current['run_id']), (self.record['pid'], self.run_id))
        self.assertEqual(current['cleared_conversations'], [self.conversation_id])
        self.assertEqual(sum(row['type'] == 'SessionCleared' for row in self.inspect()['events']), 1)
        self.assertEqual(self.received().count(b'/clear\r'), 1)
        self.assertEqual(self.received().count(b'Fresh message after reset'), 1)

    def test_unrequested_successor_refuses_changed_or_unheld_native_proofs(self):
        import time
        self.terminal_end()
        original = copy.deepcopy(self.record)
        old_copy = self.root / 'unheld-old.jsonl'
        old_copy.write_bytes(Path(self.record['transcript']).read_bytes())
        self.record['transcript'] = str(old_copy)
        self.record['session_end']['terminal_resume']['transcript'] = str(old_copy)
        self.write_record()
        self.assertEqual(self.emit()['conversation_id'], self.conversation_id)
        unheld = self.transcript.with_name('copy-' + self.transcript.name)
        unheld.write_bytes(self.transcript.read_bytes())
        for event in [dict(self.event, transcript_path=str(unheld)), dict(self.event, agent_id='child'),
                      dict(self.event, source='resume'), dict(self.event, hook_event_name='UserPromptSubmit')]:
            with self.subTest(event=event):
                self.record = copy.deepcopy(original); self.write_record()
                self.assertEqual(self.emit(event)['conversation_id'], self.conversation_id)
        for key, value in [('error', 'agent resumed a different conversation'), ('clear_context_request', {}),
                           ('restore_archive_id', str(uuid.uuid4())), ('process_start', 'stale'),
                           ('last_event_at', time.time() + 1)]:
            with self.subTest(record=key):
                self.record = copy.deepcopy(original); self.record[key] = value; self.write_record()
                self.assertEqual(self.emit()['conversation_id'], self.conversation_id)
        self.record = copy.deepcopy(original); self.write_record()
        self.header['payload']['timestamp'] = '2000-01-01T00:00:00Z'; self.write_header()
        self.assertEqual(self.emit()['conversation_id'], self.conversation_id)

    def test_original_native_caller_and_held_successor_recover_the_actual_work_once(self):
        self.poisoned()
        self.hook(self.event)  # The external Python controller is not the native caller.
        self.assertEqual(json.loads(self.record_path.read_text())['conversation_id'], self.conversation_id)
        current = self.emit()
        self.assertEqual(current['conversation_id'], self.successor)
        self.assertEqual((current['run_id'], current['pid']), (self.run_id, self.record['pid']))
        self.assertNotIn('error', current)
        self.assertNotIn('expected_id', current)
        self.assertNotIn('session_end', current)
        self.assertEqual(current['phase'], 'tool')
        self.assertEqual(current['activity'], 'busy')
        self.assertEqual(current['conversation_state'], 'active')
        self.assertIn('original-call', current['active_tools'])
        self.assertEqual(current['cleared_conversations'], [self.conversation_id])
        # The stored proof is canonical even when TMPDIR uses macOS's /var alias.
        self.assertEqual(Path(current['transcript']), self.transcript.resolve())
        events = self.inspect()['events']
        self.assertEqual([e['type'] for e in events], ['SessionCleared', 'PreToolUse'])
        self.emit(dict(self.event, hook_event_name='PostToolUse'))
        current = self.emit(dict(self.event, hook_event_name='Stop', last_assistant_message='Fixture finished'))
        self.assertEqual((current['activity'], current['phase']), ('idle', 'idle'))
        self.assertEqual(sum(e['type'] == 'SessionCleared' for e in self.inspect()['events']), 1)

    @unittest.skipUnless(sys.platform in ('linux', 'darwin'), 'Unix Terminate fence')
    def test_hook_result_waits_for_bytes_and_preserves_a_nonzero_exit(self):
        self.poisoned()
        (self.root / 'delay-hook-result').touch()
        # Both success and refusal are published after the empty file exists.
        self.record = self.emit()
        self.assertEqual(self.record['conversation_id'], self.successor)
        self.record['terminating'] = None
        self.write_record()
        before = self.record_path.read_bytes()
        self.emit(expected=2)
        self.assertEqual(self.record_path.read_bytes(), before)

    @unittest.skipUnless(sys.platform in ('linux', 'darwin'), 'Unix Terminate fence')
    def test_pending_terminate_blocks_successor_recovery_but_keeps_original_completion(self):
        self.poisoned()
        self.assert_supervised_holds_successor()
        original = copy.deepcopy(self.record)
        for marker in (None, False, 'damaged', dict(request_id=str(uuid.uuid4()))):
            with self.subTest(marker=marker):
                self.record = dict(copy.deepcopy(original), terminating=marker)
                self.write_record()
                before = self.record_path.read_bytes()
                # These are emitted by the genuine original C fixture caller
                # holding the successor rollout, not by the Python controller.
                for kind in ('Stop', 'PostToolUse', 'SessionStart', 'PreToolUse'):
                    self.emit(dict(self.event, hook_event_name=kind, source='clear'),
                              expected=2 if kind in ('SessionStart', 'PreToolUse') else 0)
                    self.assertEqual(self.record_path.read_bytes(), before, kind)
                for kind in ('PostToolUse', 'Stop', 'SessionEnd'):
                    current = self.emit(dict(self.event, hook_event_name=kind,
                                            session_id=self.conversation_id,
                                            transcript_path=original['transcript'], reason='other'))
                    for key in ('terminating', 'name', 'run_id', 'conversation_id', 'transcript',
                                'pid', 'process_start', 'supervisor', 'pane', 'expected_id',
                                'clear_context_request', 'error'):
                        self.assertEqual(current.get(key), self.record.get(key), key)
                self.assertEqual(current['session_end']['reason'], 'other')
                self.assertEqual(current['session_end']['conversation_id'], self.conversation_id)
                self.assertFalse(current.get('session_clear'))

    @unittest.skipUnless(sys.platform in ('linux', 'darwin'), 'Unix Terminate fence')
    def test_pending_terminate_refuses_terminal_clear_start_without_changing_the_binding(self):
        self.terminal_end()
        self.record['terminating'] = None
        self.write_record()
        before = self.record_path.read_bytes()
        self.emit(expected=2)
        self.assertEqual(self.record_path.read_bytes(), before)

    def test_changed_or_unowned_proofs_keep_the_original_refusal(self):
        self.poisoned()
        original, header = copy.deepcopy(self.record), copy.deepcopy(self.header)
        unheld = self.account / 'sessions' / 'unheld' / self.transcript.name
        unheld.parent.mkdir()
        unheld.write_text(json.dumps(header) + '\n')
        cases = [('source', 'exec'), ('id', str(uuid.uuid4())), ('cwd', str(self.root / 'other')),
                 ('timestamp', '2000-01-01T00:00:00Z'), ('timestamp', '2999-01-01T00:00:00Z'),
                 ('forked_from_id', self.conversation_id), ('parent_thread_id', self.conversation_id)]
        for key, value in cases:
            with self.subTest(header=key, value=value):
                self.record = copy.deepcopy(original); self.write_record()
                self.header = copy.deepcopy(header); self.header['payload'][key] = value; self.write_header()
                self.assertEqual(self.emit()['conversation_id'], self.conversation_id)
        self.header = header; self.write_header()
        for key, value in [('process_start', 'stale'), ('error', 'another failure'), ('expected_id', str(uuid.uuid4())),
                           ('supervisor', dict(self.record['supervisor'], start='stale')),
                           ('restore_archive_id', str(uuid.uuid4())),
                           ('session_end', dict(self.record['session_end'], clear_request_id=str(uuid.uuid4()))),
                           ('active_tools', {'old-work': dict(name='Bash', started=1)})]:
            with self.subTest(record=key):
                self.record = copy.deepcopy(original); self.record[key] = value; self.write_record()
                self.assertEqual(self.emit()['conversation_id'], self.conversation_id)
        self.record = original; self.write_record()
        for event in [dict(self.event, transcript_path=str(unheld)), dict(self.event, agent_id='child'),
                      dict(self.event, cwd=str(self.root / 'other')),
                      dict(self.event, hook_event_name='SessionStart', source='resume')]:
            with self.subTest(event=event):
                self.record = copy.deepcopy(original); self.write_record()
                self.assertEqual(self.emit(event)['conversation_id'], self.conversation_id)
        for data in [json.dumps(header), ' ' * (256 * 1024 + 1) + '\n']:
            with self.subTest(header_bytes=len(data)):
                self.record = copy.deepcopy(original); self.write_record()
                self.transcript.write_text(data)
                self.assertEqual(self.emit()['conversation_id'], self.conversation_id)
