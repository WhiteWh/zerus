"""Native Claude safety confirmations in private homes against a local API.

The rm operation is fenced to this fixture's generated working directory.
No live session, real account or model service is used.
"""
import hashlib
import json
import shlex
import subprocess
import time
import unittest
import uuid

import test_claude_questions_native as native
import test_input as transport
import test_questions as questions


@unittest.skipUnless(native.CLAUDE, 'Set HGS_CLAUDE_TEST_BIN for private native safety qualification')
class NativeClaudeSafety(unittest.TestCase):
    tool_name = 'Bash'
    native_permission_mode = 'bypassPermissions'
    script = native.NativeClaudeQuestions.script
    tmux = native.NativeClaudeQuestions.tmux
    write_record = native.NativeClaudeQuestions.write_record
    wait_for = native.NativeClaudeQuestions.wait_for
    screen = native.NativeClaudeQuestions.screen
    hook = native.NativeClaudeQuestions.hook
    inspect = native.NativeClaudeQuestions.inspect
    answer = questions.Questions.answer
    payload = questions.Questions.payload

    def native_tool_input(self, work):
        prefix = ':\n' * 90 if 'clipped' in self._testMethodName else ''
        if '_80x24' in self._testMethodName:
            # One long line of statements, as agents write them, wraps past 24 rows.
            prefix = ''.join(f'test -d {shlex.quote(str(work))} || echo missing fixture directory {i}; '
                             for i in range(8))
        # Keep the native variable-path rm warning and an exact directory fence.
        # Claude 2.1.317's Bash 3.2 guard refuses the former substitution/quote form.
        command = ('T=$PWD; test . -ef ' + shlex.quote(str(work)) +
                   ' && rm -f $T/*.jsonl $T/*.port && printf approved > approved.txt')
        return dict(command=prefix + command, description='Remove owned fixture files after confirmation')

    def setUp(self):
        prepare = (self.attach_viewer if '_viewed_' in self._testMethodName else
                   self.hold_answer_lock if '_answering_' in self._testMethodName else None)
        if prepare:
            # Before the first hook or inspection can read the panel.
            tmux = self.tmux
            def marked(*args, **kwargs):
                result = tmux(*args, **kwargs)
                if '@hgs_run' in args:
                    prepare()
                return result
            self.tmux = marked
        native.NativeClaudeQuestions.setUp(self)
        self.assertIn('Dangerous rm operation on possibly-empty variable path:', self.screen())

    def attach_viewer(self):
        # A real client on the agent's session, as Terminal attaches one.
        self.tmux('new-session', '-d', '-s', 'viewer', '-x', '120', '-y', '45',
                  'env', '-u', 'TMUX', str(self.bin / 'tmux'), 'attach', '-t', '=' + self.name)
        self.wait_for(lambda: self.window('session_attached') == '1')
        # The window takes the client's size and Claude redraws its whole clipped
        # panel; a capture during that redraw can miss the footer and warning.
        def settled():
            first = self.screen()
            time.sleep(.15)
            second = self.screen()
            return first == second and 'Esc to cancel · Tab to amend' in second and 'Dangerous rm operation' in second
        self.wait_for(settled)

    def hold_answer_lock(self):
        # The lock an answer driving this run's terminal holds.
        import fcntl
        locks = self.state / 'question_locks'
        locks.mkdir(mode=0o700, exist_ok=True)
        self.answer_lock = open(locks / (hashlib.sha256(self.run_id.encode()).hexdigest() + '.lock'), 'w')
        self.addCleanup(self.answer_lock.close)
        fcntl.flock(self.answer_lock, fcntl.LOCK_EX)

    def window(self, field):
        return self.tmux('display-message', '-p', '-t', '=' + self.name + ':', '#{' + field + '}').strip()

    def choose(self, label, **updates):
        item = self.card['questions'][0]
        option = next(value for value in item['options'] if value['label'] == label)
        return self.payload(answers=[dict(question_id=item['id'], selected_option_ids=[option['id']], text='')], **updates)

    def without_permission_hook(self, phase='working'):
        record = json.loads(self.record_path.read_text())
        record.pop('pending_approval', None)
        record.pop('pending_approval_at', None)
        record['phase'] = phase
        self.record_path.write_text(json.dumps(record))
        self.card = self.inspect()['pending_questions'][0]

    def test_hook_backed_safety_shows_warning_and_approves_once(self):
        self.assertTrue(self.card['can_answer'], self.card['answer_unavailable_reason'])
        self.assertIn('Dangerous rm operation', self.card['questions'][0]['body'])
        self.assertEqual([option['scope'] for option in self.card['questions'][0]['options']], ['once', 'once'])
        payload = self.choose('Yes')
        self.answer(payload)
        self.wait_for(lambda: (self.root / 'work/approved.txt').exists())
        self.assertEqual((self.root / 'work/approved.txt').read_text(), 'approved')
        self.assertEqual(self.answer(payload)['status'], 'answered')

    def test_deny_does_not_run_the_command(self):
        self.assertTrue(self.card['can_answer'], self.card['answer_unavailable_reason'])
        self.answer(self.choose('No'))
        self.wait_for(lambda: 'What should Claude do instead?' in self.screen())
        self.assertFalse((self.root / 'work/approved.txt').exists())
        self.assertEqual(self.inspect()['pending_questions'], [])

    def test_wrapped_safety_warning_and_denial_at_87_columns(self):
        self.assertTrue(self.card['can_answer'], self.card['answer_unavailable_reason'])
        self.assertIn('Dangerous rm operation', self.card['questions'][0]['body'])
        self.answer(self.choose('No'))
        self.wait_for(lambda: 'What should Claude do instead?' in self.screen())
        self.assertFalse((self.root / 'work/approved.txt').exists())
        self.assertEqual(self.inspect()['pending_questions'], [])

    def test_countdown_does_not_change_the_question_identity(self):
        original = self.card['question_hash']
        time.sleep(1.2)
        self.assertEqual(self.inspect()['pending_questions'][0]['question_hash'], original)

    def test_missing_hook_projects_attention_and_blocks_send_and_answer(self):
        self.without_permission_hook()
        state = self.inspect()
        self.assertEqual(state['phase'], 'approval')
        self.assertEqual(self.card['source'], 'claude_terminal_safety')
        self.assertFalse(self.card['can_answer'])
        self.assertIn('Dangerous rm operation', self.card['questions'][0]['body'])
        self.assertEqual(json.loads(self.record_path.read_text())['phase'], 'working')
        self.answer(self.choose('Yes'), success=False)
        payload = dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id,
                       expected_conversation_id=self.conversation_id, text='Keep this unsent fixture message')
        result = subprocess.run([str(transport.HGS), 'send', self.name, '--json'], env=self.env,
                                input=json.dumps(payload), text=True, capture_output=True, timeout=15)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('safety approval', result.stderr)
        self.assertIn('Do you want to proceed?', self.screen())
        self.assertFalse((self.root / 'work/approved.txt').exists())

    def test_changed_command_and_hash_never_approve(self):
        self.answer(self.choose('Yes', expected_question_hash='0' * 64), success=False)
        self.hook(dict(hook_event_name='PermissionRequest', tool_name='Bash', tool_input={'command':'printf different'}))
        changed = self.inspect()['pending_questions'][0]
        self.assertFalse(changed['can_answer'])
        self.assertIn('Dangerous rm operation', changed['questions'][0]['body'])
        self.answer(self.choose('Yes'), success=False)
        self.assertIn('Do you want to proceed?', self.screen())
        self.assertFalse((self.root / 'work/approved.txt').exists())

    def test_clipped_command_grows_an_unviewed_pane_and_approves_once(self):
        self.assertTrue(self.card['can_answer'], self.card['answer_unavailable_reason'])
        self.assertIn('Dangerous rm operation', self.card['questions'][0]['body'])
        self.assertEqual(self.window('window_width'), '120')
        self.assertGreater(int(self.window('window_height')), 45)
        # The window takes the size of the next client that attaches.
        self.assertEqual(self.tmux('show-options', '-w', '-v', '-t', '=' + self.name + ':', 'window-size').strip(), '')
        self.answer(self.choose('Yes'))
        self.wait_for(lambda: (self.root / 'work/approved.txt').exists())
        self.assertEqual((self.root / 'work/approved.txt').read_text(), 'approved')

    def test_default_80x24_pane_shows_the_complete_long_command_and_denies(self):
        self.assertTrue(self.card['can_answer'], self.card['answer_unavailable_reason'])
        body = self.card['questions'][0]['body']
        self.assertTrue(body.startswith('Bash command'), body)
        self.assertIn('Dangerous rm operation', body)
        self.assertEqual(self.window('window_width'), '80')
        self.assertGreater(int(self.window('window_height')), 24)
        self.answer(self.choose('No'))
        self.wait_for(lambda: 'What should Claude do instead?' in self.screen())
        self.assertFalse((self.root / 'work/approved.txt').exists())
        self.assertEqual(self.inspect()['pending_questions'], [])

    def test_clipped_command_never_grows_while_an_answering_run_holds_its_lock(self):
        self.assertFalse(self.card['can_answer'])
        self.assertEqual(self.window('window_height'), '45')
        import fcntl
        fcntl.flock(self.answer_lock, fcntl.LOCK_UN)
        card = self.inspect()['pending_questions'][0]
        self.assertTrue(card['can_answer'], card['answer_unavailable_reason'])
        self.assertGreater(int(self.window('window_height')), 45)

    def test_clipped_command_in_a_viewed_pane_keeps_warning_with_hook_and_without_hook(self):
        self.assertFalse(self.card['can_answer'])
        self.assertIn('Enlarge Terminal', self.card['answer_unavailable_reason'])
        self.assertLessEqual(int(self.window('window_height')), 45)
        self.assertIn('Dangerous rm operation', self.card['questions'][0]['body'])
        self.without_permission_hook('tool')
        self.assertFalse(self.card['can_answer'])
        self.assertEqual(self.inspect()['phase'], 'approval')
        self.assertIn('Dangerous rm operation', self.card['questions'][0]['body'])
        self.assertIn('Terminal', self.card['answer_unavailable_reason'])
        self.answer(self.choose('Yes'), success=False)
        self.assertFalse((self.root / 'work/approved.txt').exists())


@unittest.skipUnless(native.CLAUDE, 'Set HGS_CLAUDE_TEST_BIN for real native workflow approvals')
class NativeClaudeWorkflowSafety(unittest.TestCase):
    """Real Agent workflow, real child hooks and safety panel; localhost API only."""
    tool_name = 'Bash'
    native_permission_mode = 'bypassPermissions'
    script = native.NativeClaudeQuestions.script
    tmux = native.NativeClaudeQuestions.tmux
    write_record = native.NativeClaudeQuestions.write_record
    wait_for = native.NativeClaudeQuestions.wait_for
    screen = native.NativeClaudeQuestions.screen
    inspect = native.NativeClaudeQuestions.inspect
    answer = questions.Questions.answer
    payload = questions.Questions.payload
    choose = NativeClaudeSafety.choose
    native_tool_input = NativeClaudeSafety.native_tool_input

    def native_settings(self, work):
        import sys
        config = self.root / '.claude.json'
        config.write_text(json.dumps(dict(json.loads(config.read_text()), hasSeenAutoDefaultNudge=True)))
        script = self.root / 'capture-hook.py'
        script.write_text('import fcntl,json,pathlib,sys\n'
                          'event=json.load(sys.stdin)\n'
                          'with pathlib.Path(__file__).with_name("native-hooks.jsonl").open("a") as out:\n'
                          ' fcntl.flock(out,fcntl.LOCK_EX)\n'
                          ' out.write(json.dumps(event)+"\\n")\n')
        command = shlex.quote(sys.executable) + ' ' + shlex.quote(str(script))
        return {'hooks': {name: [{'hooks': [{'type': 'command', 'command': command}]}]
                          for name in ('PreToolUse', 'PermissionRequest', 'PostToolUse', 'PostToolUseFailure', 'SubagentStop')}}

    def native_response_blocks(self, body):
        messages = body.get('messages', [])
        child = any('WORKFLOW_CHILD_FIXTURE' in str(m.get('content', '')) and m.get('role') == 'user' for m in messages)
        tool_id = 'toolu_child_fixture' if child else 'toolu_parent_fixture'
        answered = any(isinstance(m.get('content'), list) and any(
            part.get('type') == 'tool_result' and part.get('tool_use_id') == tool_id
            for part in m['content']) for m in messages)
        if answered:
            return [dict(type='text', text='Fixture complete.')]
        if child:
            return [dict(type='tool_use', id=tool_id, name='Bash', input=self.tool_input)]
        if any(t.get('name') == 'Agent' for t in body.get('tools', [])):
            return [dict(type='tool_use', id=tool_id, name='Agent', input=dict(
                description='fixture-checks', subagent_type='general-purpose',
                prompt='WORKFLOW_CHILD_FIXTURE: run the requested fixture tool.', run_in_background=True))]
        return [dict(type='text', text='Fixture complete.')]

    def hook(self, event):
        # Base setup synthesizes main-agent hooks for older contracts. This
        # fixture instead replays the real native child's hooks verbatim below.
        pass

    def replay_hooks(self):
        path = self.root / 'native-hooks.jsonl'
        events = native.records(path) if path.exists() else []
        for event in events[self.hook_offset:]:
            self.assertEqual(event['session_id'], self.conversation_id)
            questions.Questions.hook(self, event)
        self.hook_offset = len(events)
        return events

    def setUp(self):
        self.hook_offset = 0
        native.NativeClaudeQuestions.setUp(self)
        events = self.replay_hooks()
        requests = [e for e in events if e['hook_event_name'] == 'PermissionRequest']
        self.assertEqual(len(requests), 1, events)
        self.child_id = requests[0].get('agent_id')
        self.assertTrue(self.child_id, events)
        self.assertEqual(requests[0]['tool_input']['command'], self.tool_input['command'])
        self.card = self.inspect()['pending_questions'][0]
        self.assertEqual(self.card['agent_id'], self.child_id)
        self.assertTrue(self.card['can_answer'], self.card)
        self.assertIn('Bash command · from the ', self.card['questions'][0]['body'])

    def test_workflow_approve_once_and_native_completion(self):
        original = self.card['question_hash']
        time.sleep(1.1)
        self.assertEqual(self.inspect()['pending_questions'][0]['question_hash'], original)
        payload = self.choose('Yes')
        self.answer(payload)
        self.wait_for(lambda: (self.root / 'work/approved.txt').exists())
        self.assertEqual(self.answer(payload)['status'], 'answered')
        self.replay_hooks()
        self.assertEqual(self.inspect()['pending_questions'], [])
        self.assertNotEqual(json.loads(self.record_path.read_text())['phase'], 'interrupted')

    def test_workflow_deny_does_not_interrupt_parent_or_siblings(self):
        # An unrelated live sibling and parent's active tool are retained.
        record = json.loads(self.record_path.read_text())
        parent_tools = record['active_tools']
        record.update(phase='idle', main_done=True)
        record.setdefault('subagents', {})['fixture-sibling'] = dict(name='sibling', state='working')
        self.record_path.write_text(json.dumps(record))
        payload = self.choose('No')
        self.answer(payload)
        self.assertFalse((self.root / 'work/approved.txt').exists())
        record = json.loads(self.record_path.read_text())
        self.assertEqual(record['active_tools'], parent_tools)
        self.assertEqual(record['subagents']['fixture-sibling']['state'], 'working')
        self.assertNotEqual(record['phase'], 'interrupted')
        self.assertEqual(self.answer(payload)['status'], 'answered')

    def test_wrapped_workflow_deny_at_87_columns(self):
        self.test_workflow_deny_does_not_interrupt_parent_or_siblings()

    def test_workflow_duplicate_command_is_not_routed_to_arbitrary_child(self):
        questions.Questions.hook(self, dict(hook_event_name='PermissionRequest', agent_id='fixture-other-child',
                                          tool_name='Bash', tool_input=self.tool_input))
        self.assertFalse(self.inspect()['pending_questions'][0]['can_answer'])
        self.answer(self.choose('Yes'), success=False)
        self.assertIn('Do you want to proceed?', self.screen())
        self.assertFalse((self.root / 'work/approved.txt').exists())


if __name__ == '__main__':
    unittest.main()
