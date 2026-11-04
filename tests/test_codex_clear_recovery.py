"""Original process/file/caller proof for an already rejected clear successor.

Native-like C processes and real hgs hooks use only private homes/tmux. This is
not an owner-session operation or a claim about a model/provider response.
"""
import copy
from datetime import datetime, timezone
import json
from pathlib import Path
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
    hook = clear.ClearContext.hook
    inspect = clear.ClearContext.inspect

    def poisoned(self):
        self.successor = str(uuid.uuid4())
        self.account = self.root / 'account'
        self.transcript = self.account / 'sessions' / ('rollout-' + self.successor + '.jsonl')
        self.transcript.parent.mkdir(parents=True)
        self.transcript.touch()
        # The actual tracked C process opens this file itself. Its child execs
        # hgs directly, so the positive has real ancestry rather than a claimed
        # role in a JSON request or a hook launched by this Python controller.
        environment = dict(HGS_SESSION=self.name, HGS_RUN_ID=self.run_id,
                           HGS_STATE_DIR=str(self.state), HGS_CONFIG_DIR=str(self.root / 'config'))
        assignments = ''.join('setenv(' + json.dumps(k) + ',' + json.dumps(v) + ',1);'
                              for k, v in environment.items())
        emitter = r'''
static void emit_hook(void) {
    pid_t child = fork();
    if (!child) {
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
    append("hook-result", result, size);
}
'''.replace('ENVIRONMENT', assignments).replace('EXECUTABLE', json.dumps(str(test_input.HGS)))
        source = clear.RESUMED_CODEX.replace('#include <unistd.h>',
                    '#include <unistd.h>\n#include <stdlib.h>\n#include <signal.h>\n#include <sys/wait.h>')
        source = source.replace('int main(int argc, char **argv)', emitter + '\nint main(int argc, char **argv)')
        source = source.replace('open(argv[2], O_RDONLY) < 0',
                    '(open(argv[2], O_RDONLY) < 0 || open(' + json.dumps(str(self.transcript)) + ', O_RDONLY) < 0)')
        source = source.replace('if (!strcmp(text, "/clear")',
                    'if (!strcmp(text, "fixture-hook")) { emit_hook(); } else if (!strcmp(text, "/clear")')
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

    def emit(self, event=None):
        (self.root / 'hook-result').unlink(missing_ok=True)
        (self.root / 'recovery-event.json').write_text(json.dumps(event or self.event))
        self.tmux('send-keys', '-t', self.pane, '-l', 'fixture-hook')
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        self.wait_for(lambda: (self.root / 'hook-result').exists())
        self.assertEqual((self.root / 'hook-result').read_text(), '0')
        return json.loads(self.record_path.read_text())

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
        self.assertEqual(Path(current['transcript']), self.transcript)
        events = self.inspect()['events']
        self.assertEqual([e['type'] for e in events], ['SessionCleared', 'PreToolUse'])
        self.emit(dict(self.event, hook_event_name='PostToolUse'))
        current = self.emit(dict(self.event, hook_event_name='Stop', last_assistant_message='Fixture finished'))
        self.assertEqual((current['activity'], current['phase']), ('idle', 'idle'))
        self.assertEqual(sum(e['type'] == 'SessionCleared' for e in self.inspect()['events']), 1)

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
