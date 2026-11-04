"""Native Claude permissions, using only a localhost API and temporary files."""
import glob, hashlib, json, os, re, shlex, signal, subprocess, sys, tempfile, threading, time, unittest, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import test_claude_questions_native as native
import test_input as transport
import test_questions as questions

@unittest.skipUnless(native.CLAUDE, 'Set HGS_CLAUDE_TEST_BIN for the isolated native approval fixture')
class NativeClaudeApprovals(unittest.TestCase):
    tool_name = 'Bash'
    tool_input = {'command': 'printf approved > approved.txt', 'description': 'Create a fixture file after approval'}
    setUp = native.NativeClaudeQuestions.setUp
    script = native.NativeClaudeQuestions.script
    tmux = native.NativeClaudeQuestions.tmux
    write_record = native.NativeClaudeQuestions.write_record
    wait_for = native.NativeClaudeQuestions.wait_for
    screen = native.NativeClaudeQuestions.screen
    hook = native.NativeClaudeQuestions.hook
    inspect = native.NativeClaudeQuestions.inspect
    answer = questions.Questions.answer
    payload = questions.Questions.payload

    def options(self):
        return self.card['questions'][0]['options']

    def choose(self, label, **updates):
        option = next(o for o in self.options() if o['label'] == label)
        return self.payload(answers=[dict(question_id='approval', selected_option_ids=[option['id']], text='')], **updates)

    def test_native_options_and_approve_once_do_not_change_permission_mode(self):
        self.assertTrue(self.card['approval'])
        self.assertTrue(self.card['can_answer'], self.card['answer_unavailable_reason'])
        self.assertEqual(self.card['tool_call_id'], 'toolu_native_fixture')
        self.assertIn(self.tool_input['command'], self.card['questions'][0]['body'])
        self.assertEqual(self.options()[0], dict(id='1', label='Yes', scope='once'))
        self.assertEqual(self.options()[-1], dict(id=str(len(self.options())), label='No', scope='once'))
        self.assertTrue(all(o['scope'] == 'broader' for o in self.options()[1:-1]), self.options())
        self.answer(self.choose('Yes'))
        self.wait_for(lambda: (self.root/'work/approved.txt').exists())
        self.assertEqual((self.root/'work/approved.txt').read_text(), 'approved')
        self.assertNotIn('bypassPermissions', (self.root/'.claude/settings.json').read_text())

    def test_deny_interrupts_without_running_and_reopens_the_composer(self):
        self.answer(self.choose('No'))
        self.wait_for(lambda: 'What should Claude do instead?' in self.screen())
        self.assertFalse((self.root/'work/approved.txt').exists())
        state = self.inspect()
        self.assertEqual(state['phase'], 'interrupted')
        self.assertEqual(state['pending_questions'], [])

    def test_changed_command_and_hash_send_no_approval(self):
        self.answer(self.choose('Yes', expected_question_hash='0'*64), success=False)
        self.assertIn('Do you want to proceed?', self.screen())
        self.assertFalse((self.root/'work/approved.txt').exists())
        self.hook(dict(hook_event_name='PermissionRequest', tool_name='Bash', tool_input={'command':'rm changed'}))
        changed = self.inspect()['pending_questions'][0]
        self.assertFalse(changed['can_answer'])
        self.assertIn('Terminal', changed['answer_unavailable_reason'])
        self.answer(self.choose('Yes'), success=False)
        self.assertIn('Do you want to proceed?', self.screen())
        self.assertFalse((self.root/'work/approved.txt').exists())


class NativeClaudeReadApproval(NativeClaudeApprovals):
    tool_name = 'Read'
    tool_input = {'file_path': '/outside-fixture/notes.txt'}
    test_native_options_and_approve_once_do_not_change_permission_mode = None
    test_deny_interrupts_without_running_and_reopens_the_composer = None
    test_changed_command_and_hash_send_no_approval = None

    def test_session_grant_is_marked_broader_and_deny_reaches_claude(self):
        self.assertTrue(self.card['can_answer'], self.card['answer_unavailable_reason'])
        self.assertIn('Read(/outside-fixture/notes.txt)', self.card['questions'][0]['body'])
        self.assertEqual([o['scope'] for o in self.options()], ['once', 'broader', 'once'])
        self.answer(self.choose('No'))
        self.wait_for(lambda: 'What should Claude do instead?' in self.screen())
        self.assertEqual(self.inspect()['phase'], 'interrupted')


class NativeClaudeFetchApproval(NativeClaudeReadApproval):
    tool_name = 'WebFetch'
    # From 2.1.304 WebFetch can be deferred behind ToolSearch, so the fixture's
    # first request would not list it; the approval panel is the same either way.
    native_env = ('ENABLE_TOOL_SEARCH=false',)
    tool_input = {'url': 'https://example.com', 'prompt': 'title?'}
    panel_question = 'Do you want to allow Claude to fetch this content?'
    test_session_grant_is_marked_broader_and_deny_reaches_claude = None

    def test_feedback_denial_without_footer(self):
        self.assertTrue(self.card['can_answer'], self.card['answer_unavailable_reason'])
        self.assertEqual(self.options()[-1]['scope'], 'once')
        self.assertEqual(self.options()[1]['scope'], 'broader')
        self.answer(self.choose(self.options()[-1]['label']))
        self.wait_for(lambda: 'What should Claude do instead?' in self.screen())

class NativeClaudeInterrupt(NativeClaudeReadApproval):
    tool_name = 'Bash'
    test_session_grant_is_marked_broader_and_deny_reaches_claude = None

    def native_tool_input(self, work):
        # Claude 2.1.310 rejects long foreground sleeps with nonessential traffic
        # disabled. Keep real interruptible work: a bounded fixture-owned pipe read.
        script = work / 'interrupt-work.py'
        script.write_text('import os, select\nfrom pathlib import Path\n'
                          'root = Path(__file__).resolve().parent\n'
                          'reader, writer = os.pipe()\n'
                          'try:\n'
                          '    (root / "interrupt-started.txt").write_text("started")\n'
                          '    select.select([reader], [], [], 30)\n'
                          '    (root / "slept.txt").write_text("done")\n'
                          'finally:\n'
                          '    os.close(reader)\n'
                          '    os.close(writer)\n')
        return dict(command=shlex.quote(sys.executable) + ' ' + shlex.quote(str(script)),
                    description='Run bounded fixture I/O until interrupted')

    def test_escape_interrupt_is_recorded_without_a_hook(self):
        import json, time, uuid
        self.assertNotIn('hooks', json.loads((self.root/'.claude/settings.json').read_text()))
        self.answer(self.choose('Yes'))
        self.wait_for(lambda: 'Do you want to proceed?' not in self.screen())
        self.wait_for(lambda: (self.root/'work/interrupt-started.txt').exists())
        self.assertFalse((self.root/'work/slept.txt').exists())
        record = json.loads(self.record_path.read_text())
        record.update(run_identity_version=1, supervisor=dict(pid=record['pid'], start=record['process_start']), activity='busy', turn_started=time.time())
        self.record_path.write_text(json.dumps(record))
        import subprocess, test_input
        result = subprocess.run([str(test_input.HGS), 'interrupt', self.name, '--json'], env=self.env, text=True, capture_output=True, timeout=15,
            input=json.dumps(dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id, expected_conversation_id=self.conversation_id,
                                  expected_turn_started=record['turn_started'])))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(json.loads(result.stdout)['confirmed'], self.screen())
        self.assertIn('What should Claude do instead?', self.screen())
        self.assertEqual(self.inspect()['phase'], 'interrupted')
        self.assertFalse((self.root/'work/slept.txt').exists())

class NativeClaudeTerminalAnswers(NativeClaudeReadApproval):
    """Answers chosen in Terminal itself: Claude reports none of them with a hook."""
    tool_name = 'Bash'
    tool_input = {'command': 'sleep 20; printf done > slept.txt', 'description': 'Wait for the fixture'}
    test_session_grant_is_marked_broader_and_deny_reaches_claude = None
    key = native.NativeClaudeQuestions.key

    def settled(self, phase):
        return self.wait_for(lambda: (state := self.inspect())['phase'] == phase and state)

    def test_yes_settles_once_the_command_shows_it_runs(self):
        self.key('Enter')
        state = self.settled('tool')
        self.assertEqual(state['pending_questions'], [])
        self.assertNotIn('Do you want to proceed?', self.screen())
        self.assertFalse((self.root/'work/slept.txt').exists())

    def test_no_ends_the_turn_as_an_interruption(self):
        for _ in self.options()[1:]: self.key('Down')
        self.key('Enter')
        self.assertEqual(self.settled('interrupted')['pending_questions'], [])
        self.assertIn('What should Claude do instead?', self.screen())

    def test_escape_ends_the_turn_as_an_interruption(self):
        self.key('Escape')
        self.assertEqual(self.settled('interrupted')['pending_questions'], [])
        self.assertIn('What should Claude do instead?', self.screen())


@unittest.skipUnless(native.CLAUDE, 'Set HGS_CLAUDE_TEST_BIN for the isolated native approval fixture')
class NativeClaudeApprovalChannel(unittest.TestCase):
    """hgs's own hooks, as it registers them for Claude Code 2.1.296: the
    journal and the answer channel that waits beside the native panel."""
    script = native.NativeClaudeQuestions.script
    tmux = native.NativeClaudeQuestions.tmux
    write_record = native.NativeClaudeQuestions.write_record
    wait_for = native.NativeClaudeQuestions.wait_for
    screen = native.NativeClaudeQuestions.screen
    key = native.NativeClaudeQuestions.key
    inspect = native.NativeClaudeQuestions.inspect
    answer = questions.Questions.answer

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='hgs-claude-channel-')
        self.root = Path(self.temp.name).resolve()
        self.bin = self.root / '.local/bin'; self.bin.mkdir(parents=True)
        self.state = self.root / 'state'; self.state.mkdir()
        self.work = work = self.root / 'work'; work.mkdir(); (self.root / '.claude').mkdir()
        name = self._testMethodName
        safety = '_rm_' in name
        self.command = ((':\n' * 90 if '_clipped_' in name else '')
                        # Keep the variable-path warning and exact directory
                        # fence in syntax Claude 2.1.317 accepts on Bash 3.2.
                        + ('T=$PWD; test . -ef ' + shlex.quote(str(work)) + ' && rm -f $T/*.jsonl $T/*.port && ' if safety else '')
                        + ('sleep 6; ' if '_terminal_' in name else '') + 'printf approved > approved.txt')
        self.tool_input = dict(command=self.command, description='Create a fixture file after approval')
        # Two Reads of one file name in different folders, outside the project.
        self.reads = '_two_reads_' in name
        self.target_A, self.target_B = self.root / 'outsideA/notes.txt', self.root / 'outsideB/notes.txt'
        if self.reads:
            for path, text in [(self.target_A, 'SYNTHETIC_A_ONLY'), (self.target_B, 'SYNTHETIC_B_ONLY')]:
                path.parent.mkdir(); path.write_text(text)
        (self.root / '.claude.json').write_text(json.dumps(dict(
            hasCompletedOnboarding=True, bypassPermissionsModeAccepted=True, theme='dark', hasSeenAutoDefaultNudge=True,
            customApiKeyResponses=dict(approved=['fixture-key'], rejected=[]),
            projects={str(work): dict(hasTrustDialogAccepted=True, hasCompletedProjectOnboarding=True)})))
        self.name = 'claude/channel-test'; self.run_id = str(uuid.uuid4()); self.conversation_id = str(uuid.uuid4())
        hook_env = dict(HGS_SESSION=self.name, HGS_RUN_ID=self.run_id, HGS_STATE_DIR=str(self.state),
                        HGS_CONFIG_DIR=str(self.root / 'config'), PATH=str(self.bin) + ':' + os.environ['PATH'])
        program = ('env ' + ' '.join(shlex.quote(k + '=' + v) for k, v in hook_env.items()) + ' '
                   + shlex.quote(str(transport.HGS)) + ' __state ')
        hooks = {event: [dict(hooks=[dict(type='command', command=program + 'hook', timeout=5)])] for event in
                 ('SessionStart', 'UserPromptSubmit', 'PreToolUse', 'PostToolUse', 'PostToolUseFailure', 'PermissionRequest', 'Notification', 'Stop')}
        # The channel group hgs registers: every tool, a timeout for a person.
        hooks['PermissionRequest'].append(dict(hooks=[dict(type='command', command=program + 'answer-channel', timeout=86400)]))
        mode = 'bypassPermissions' if safety else 'default'
        permissions = dict(defaultMode=mode, **(dict(ask=['Read']) if self.reads else {}))
        self.settings = json.dumps(dict(permissions=permissions, hooks=hooks))
        (self.root / '.claude/settings.json').write_text(self.settings)
        fixture = self; self.requests = []
        class Server(BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or '{}')
                fixture.requests.append(body)
                asks = any(t.get('name') == ('Read' if fixture.reads else 'Bash') for t in body.get('tools', []))
                answered = any(isinstance(m.get('content'), list) and any(b.get('type') == 'tool_result' for b in m['content'])
                               for m in body.get('messages', []))
                calls = ([dict(type='tool_use', id='toolu_read_' + key, name='Read', input=dict(file_path=str(path)))
                          for key, path in [('A', fixture.target_A), ('B', fixture.target_B)]] if fixture.reads
                         else [dict(type='tool_use', id='toolu_native_channel', name='Bash', input=fixture.tool_input)])
                blocks = calls if asks and not answered else [dict(type='text', text='Fixture complete.')]
                tool = blocks[0]['type'] == 'tool_use'
                message = dict(id='msg_fixture', type='message', role='assistant', model=body.get('model', 'claude-sonnet-4-6'), content=blocks,
                               stop_reason='tool_use' if tool else 'end_turn', stop_sequence=None, usage=dict(input_tokens=100, output_tokens=30))
                self.send_response(200)
                if not body.get('stream'):
                    self.send_header('Content-Type', 'application/json'); self.end_headers(); self.wfile.write(json.dumps(message).encode()); return
                self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
                def emit(kind, **data):
                    self.wfile.write(('event: ' + kind + '\ndata: ' + json.dumps(dict(type=kind, **data)) + '\n\n').encode()); self.wfile.flush()
                emit('message_start', message=dict(message, content=[], stop_reason=None))
                for i, b in enumerate(blocks):
                    emit('content_block_start', index=i, content_block=dict(b, input={}) if tool else dict(b, text=''))
                    emit('content_block_delta', index=i, delta=dict(type='input_json_delta', partial_json=json.dumps(b['input'])) if tool
                         else dict(type='text_delta', text=b['text']))
                    emit('content_block_stop', index=i)
                emit('message_delta', delta=dict(stop_reason=message['stop_reason'], stop_sequence=None), usage=dict(output_tokens=30))
                emit('message_stop')
        server = ThreadingHTTPServer(('127.0.0.1', 0), Server)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.socket = self.root / 'socket'
        # Log the keys hgs sends; the fixture's own keys are logged too.
        logger = self.bin / 'tmux-log.py'
        logger.write_text('import json,pathlib,subprocess,sys\nargs=sys.argv[1:]\n'
                          "if args and args[0]=='send-keys':\n"
                          ' with pathlib.Path(' + repr(str(self.root / 'sent-keys')) + ").open('a') as log:log.write(json.dumps(args)+'\\n')\n"
                          'sys.exit(subprocess.call(' + repr([transport.TMUX, '-S', str(self.socket), '-f', '/dev/null']) + '+args))\n')
        self.script('tmux', '#!/bin/sh\nexec ' + shlex.quote(sys.executable) + ' ' + shlex.quote(str(logger)) + ' "$@"\n')
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('HGS_') and k not in ('TMUX', 'TMUX_PANE')}
        self.env.update(HOME=str(self.root), HGS_STATE_DIR=str(self.state), HGS_CONFIG_DIR=str(self.root / 'config'), HGS_SELF='test',
                        PATH=str(self.bin) + ':' + os.environ['PATH'])
        self.addCleanup(self.stop, server)
        columns, rows = ('80', '24') if '_80x24' in name else ('120', '45')
        self.tmux('new-session', '-d', '-s', self.name, '-x', columns, '-y', rows, '-c', str(work), 'env', '-i', 'HOME=' + str(self.root),
                  'PATH=' + os.environ['PATH'], 'SHELL=/bin/bash', 'TERM=xterm-256color', 'LANG=C.UTF-8', 'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1',
                  'DISABLE_AUTOUPDATER=1', 'ANTHROPIC_API_KEY=fixture-key', 'ANTHROPIC_BASE_URL=http://127.0.0.1:' + str(server.server_port),
                  native.CLAUDE, *(['--dangerously-skip-permissions'] if safety else []), '--model', 'claude-sonnet-4-6', '--session-id', self.conversation_id)
        self.pane = self.tmux('display-message', '-p', '-t', '=' + self.name + ':', '#{pane_id}').strip()
        self.wait_for(lambda: '/effort' in self.screen())
        pid = int(self.tmux('display-message', '-p', '-t', self.pane, '#{pane_pid}'))
        start = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart='], env=dict(self.env, LC_ALL='C', TZ='UTC'), text=True).strip()
        self.transcript = self.root / '.claude/projects' / re.sub(r'[^A-Za-z0-9]', '-', str(work)) / (self.conversation_id + '.jsonl')
        self.record_path = self.state / (hashlib.sha256(self.name.encode()).hexdigest() + '.json')
        self.record = dict(version=1, name=self.name, agent='claude', run_id=self.run_id, conversation_id=self.conversation_id,
                           transcript=str(self.transcript), pane=self.pane, pid=pid, process_start=start, activity='idle', phase='idle',
                           active_tools={}, subagents={}, last_event_at=time.time(), created=time.time())
        self.write_record()
        self.tmux('set-option', '-t', '=' + self.name + ':', '@hgs_run', self.run_id)
        if '_viewed_' in name:
            # A real client on the agent's session, as Terminal attaches one.
            self.tmux('new-session', '-d', '-s', 'viewer', '-x', columns, '-y', rows,
                      'env', '-u', 'TMUX', str(self.bin / 'tmux'), 'attach', '-t', '=' + self.name)
            self.wait_for(lambda: self.tmux('display-message', '-p', '-t', '=' + self.name + ':', '#{session_attached}').strip() == '1')
            self.viewed_height = self.tmux('display-message', '-p', '-t', '=' + self.name + ':', '#{window_height}').strip()
        self.tmux('send-keys', '-t', self.pane, '-l', 'Use the tool now.'); time.sleep(.3)
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        self.wait_for(lambda: 'Esc to cancel' in self.screen())
        self.keys_before = len(self.keys())
        self.settings_before = (self.root / '.claude/settings.json').read_text()

    def stop(self, server):
        workers = {json.loads(Path(path).read_text())['pid'] for path in glob.glob(str(self.state / 'native_answers/*/*/waiting.json'))}
        try:
            pids = {pid for pid in self.tmux('list-panes', '-a', '-F', '#{pane_pid}', check=False).splitlines() if pid.isdigit()}
            self.tmux('kill-server', check=False)
            # A channel ends once its agent is gone; wait for it and the panes.
            deadline = time.monotonic() + 5
            alive = {str(pid) for pid in workers} | pids
            while alive and time.monotonic() < deadline:
                status = subprocess.run(['ps', '-p', ','.join(sorted(alive)), '-o', 'pid=,stat='], capture_output=True, text=True, timeout=5)
                alive = {parts[0] for line in status.stdout.splitlines() if len(parts := line.split()) >= 2 and not parts[1].startswith(('Z', 'X'))}
                time.sleep(.05)
            self.assertFalse(alive, 'fixture panes or answer channels survived')
        finally:
            server.shutdown(); server.server_close(); self.temp.cleanup()

    def keys(self):
        path = self.root / 'sent-keys'
        return [json.loads(line)[3:] for line in path.read_text().splitlines()] if path.exists() else []
    def channel_card(self):
        return self.wait_for(lambda: next((card for card in self.inspect()['pending_questions'] if card.get('answer_transport') == 'claude_hook'), None))
    def channels(self, stage):
        return glob.glob(str(self.state / 'native_answers/*/*' / Path(stage)))
    def payload(self, card, label=None, text=''):
        ids = [o['id'] for o in card['questions'][0]['options'] if o['label'] == label]
        self.assertEqual(len(ids), 0 if label is None else 1, card['questions'][0]['options'])
        return dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id, expected_conversation_id=self.conversation_id,
                    question_id=card['question_id'], expected_question_hash=card['question_hash'],
                    answers=[dict(question_id='approval', selected_option_ids=ids, text=text)])
    def tool_results(self):
        return [part for body in self.requests for message in body.get('messages', []) if isinstance(message.get('content'), list)
                for part in message['content'] if part.get('type') == 'tool_result' and part.get('tool_use_id') == 'toolu_native_channel']
    def approved(self):
        return (self.work / 'approved.txt').exists()

    def test_yes_runs_once_through_the_hook_without_keys_or_settings(self):
        card = self.channel_card()
        self.assertTrue(card['can_answer'], card['answer_unavailable_reason'])
        self.assertEqual((card['tool_call_id'], card['panel_verified']), ('toolu_native_channel', True))
        self.assertEqual([o['label'] for o in card['questions'][0]['options']][0], 'Yes')
        receipt = self.answer(self.payload(card, 'Yes'))
        self.assertEqual((receipt['status'], receipt['transport']), ('answered', 'claude_hook'))
        self.wait_for(lambda: 'Fixture complete.' in self.screen())
        self.assertEqual((self.work / 'approved.txt').read_text(), 'approved')
        self.assertEqual(self.keys()[self.keys_before:], [], 'no key reached the terminal')
        # Claude writes its own startup flag; the answer grants nothing beyond it.
        self.assertEqual((self.root / '.claude/settings.json').read_text(), self.settings_before)
        self.assertEqual(json.loads(self.settings_before)['permissions'], dict(defaultMode='default'))
        self.assertFalse((self.work / '.claude').exists())
        self.assertEqual(len(self.channels('emitted.json')), 1)
        self.assertEqual(len(self.tool_results()) and self.tool_results()[-1].get('is_error'), False)

    def test_no_ends_the_turn_like_terminal_no(self):
        card = self.channel_card()
        receipt = self.answer(self.payload(card, 'No'))
        self.assertEqual((receipt['status'], receipt['transport']), ('answered', 'claude_hook'))
        self.wait_for(lambda: 'What should Claude do instead?' in self.screen())
        self.assertFalse(self.approved())
        self.assertEqual(self.tool_results(), [])
        state = self.inspect()
        self.assertEqual((state['phase'], state['pending_questions']), ('interrupted', []))

    def test_feedback_denies_and_claude_continues_with_the_message(self):
        card = self.channel_card()
        self.assertEqual(card['questions'][0]['allow_other'], True)
        receipt = self.answer(self.payload(card, text='Use printf other instead'))
        self.assertEqual((receipt['status'], receipt['transport']), ('answered', 'claude_hook'))
        self.wait_for(lambda: 'Fixture complete.' in self.screen())
        self.assertFalse(self.approved())
        result = self.tool_results()[-1]
        self.assertTrue(result['is_error'])
        self.assertIn('Use printf other instead', json.dumps(result['content']))
        self.wait_for(lambda: self.inspect()['phase'] == 'idle')

    def test_clipped_rm_in_a_viewed_pane_is_answered_through_the_hook(self):
        # The owner's report: a viewed pane keeps its size, so the panel stays
        # clipped and the screen alone cannot answer it.
        self.assertIn('Dangerous rm operation', self.screen())
        card = self.channel_card()
        self.assertTrue(card['can_answer'], card['answer_unavailable_reason'])
        self.assertFalse(card['panel_verified'])
        self.assertEqual([(o['label'], o['scope']) for o in card['questions'][0]['options']], [('Yes', 'once'), ('No', 'once')])
        body = card['questions'][0]['body']
        self.assertIn(self.command, body); self.assertIn('Dangerous rm operation', body)
        self.assertEqual(self.tmux('display-message', '-p', '-t', '=' + self.name + ':', '#{window_height}').strip(), self.viewed_height,
                         'a viewed pane keeps its client\'s size')
        receipt = self.answer(self.payload(card, 'Yes'))
        self.assertEqual((receipt['status'], receipt['transport']), ('answered', 'claude_hook'))
        self.wait_for(self.approved)
        self.assertEqual(self.keys()[self.keys_before:], [])

    def test_clipped_rm_80x24_in_an_unviewed_pane_is_answered_without_growth(self):
        # tmux's default size for a pane no client has attached to.
        self.assertEqual(self.tmux('display-message', '-p', '-t', '=' + self.name + ':', '#{window_height}').strip(), '24')
        card = self.channel_card()
        self.assertTrue(card['can_answer'], card['answer_unavailable_reason'])
        self.assertFalse(card['panel_verified'])
        receipt = self.answer(self.payload(card, 'Yes'))
        self.assertEqual((receipt['status'], receipt['transport']), ('answered', 'claude_hook'))
        self.wait_for(self.approved)
        self.assertEqual(self.tmux('display-message', '-p', '-t', '=' + self.name + ':', '#{window_height}').strip(), '24',
                         'a waiting hook needs no taller pane')
        self.assertEqual(self.keys()[self.keys_before:], [])

    def test_broader_option_goes_through_the_verified_screen(self):
        card = self.channel_card()
        broader = next(o for o in card['questions'][0]['options'] if o['scope'] == 'broader')
        receipt = self.answer(self.payload(card, broader['label']))
        self.assertEqual((receipt['status'], receipt['transport']), ('answered', 'claude_tui'))
        self.assertTrue(self.keys()[self.keys_before:], "Claude's own panel took the broader choice")
        self.assertEqual(self.channels('answer.json') + self.channels('claimed.json') + self.channels('emitted.json'), [])
        self.wait_for(self.approved)

    def test_terminal_yes_first_leaves_nothing_to_answer(self):
        card = self.channel_card()
        self.key('Enter')
        self.wait_for(lambda: 'Esc to cancel' not in self.screen())
        self.assertEqual(self.inspect()['pending_questions'], [])
        error = self.answer(self.payload(card, 'No'), success=False)
        self.assertIn('no longer pending', error)
        self.assertEqual(self.channels('answer.json') + self.channels('claimed.json'), [])
        self.wait_for(self.approved)
        # The command's end closes the channel; Claude never signalled it.
        self.wait_for(lambda: self.channels('closed.json'))
        self.assertFalse(json.loads(Path(self.channels('closed.json')[0]).read_text())['emitted'])

    def test_terminal_no_closes_the_channel(self):
        card = self.channel_card()
        self.key('Escape')
        self.wait_for(lambda: 'What should Claude do instead?' in self.screen())
        self.wait_for(lambda: self.channels('closed.json'))
        self.assertEqual(self.inspect()['pending_questions'], [])
        self.answer(self.payload(card, 'Yes'), success=False)
        self.assertFalse(self.approved())

    def test_killed_channel_falls_back_to_the_verified_screen(self):
        self.channel_card()
        worker = json.loads(Path(self.channels('waiting.json')[0]).read_text())['pid']
        os.kill(worker, 9)
        card = self.wait_for(lambda: next((c for c in self.inspect()['pending_questions'] if c.get('answer_transport') == 'claude_tui'), None))
        self.assertTrue(card['can_answer'], card['answer_unavailable_reason'])
        receipt = self.answer(self.payload(card, 'Yes'))
        self.assertEqual(receipt['transport'], 'claude_tui')
        self.wait_for(self.approved)

    def test_second_submit_never_reaches_claude_again(self):
        card = self.channel_card()
        first = self.payload(card, 'Yes')
        self.assertEqual(self.answer(first)['transport'], 'claude_hook')
        self.assertEqual(self.answer(first)['status'], 'answered', 'the same request returns its receipt')
        self.answer(self.payload(card, 'No'), success=False)
        self.wait_for(self.approved)
        self.assertEqual(len(self.channels('emitted.json')), 1)

    def refused_offer(self, refuse):
        # An offer made while all was well, then a refusal the agent's identity
        # check knows about before the hook would take it.
        self.channel_card()
        directory = Path(self.channels('waiting.json')[0]).parent
        worker = json.loads((directory / 'waiting.json').read_text())['pid']
        refuse()
        staged = directory / 'offer.new'
        staged.write_text(json.dumps(dict(request_id=str(uuid.uuid4()), decision='allow', message='')))
        staged.replace(directory / 'answer.json')
        time.sleep(2.5)
        self.assertFalse((directory / 'claimed.json').exists(), 'a refused identity never takes the offer')
        self.assertFalse((directory / 'emitted.json').exists())
        self.assertFalse(self.approved())
        os.kill(worker, signal.SIGTERM)
        self.wait_for(lambda: (directory / 'closed.json').exists())
        closed = json.loads((directory / 'closed.json').read_text())
        self.assertEqual((closed['reason'], closed['emitted']), ('terminated', False))
        self.assertIn('Esc to cancel', self.screen())
        self.assertFalse(self.approved())

    def mutate(self, **fields):
        record = json.loads(self.record_path.read_text()); record.update(fields)
        staged = self.record_path.with_suffix('.new'); staged.write_text(json.dumps(record)); staged.replace(self.record_path)

    def test_paused_session_never_takes_an_offered_decision(self):
        start = subprocess.check_output(['ps', '-p', str(os.getpid()), '-o', 'lstart='], env=dict(self.env, LC_ALL='C', TZ='UTC'), text=True).strip()
        self.refused_offer(lambda: self.mutate(pausing=dict(pid=os.getpid(), start=start)))

    def test_failed_session_never_takes_an_offered_decision(self):
        self.refused_offer(lambda: self.mutate(error='Synthetic provider failure'))

    def test_terminal_of_another_run_never_takes_an_offered_decision(self):
        self.refused_offer(lambda: self.tmux('set-option', '-t', '=' + self.name + ':', '@hgs_run', str(uuid.uuid4())))

    def results(self):
        # Claude records each tool result in its transcript once the tool ran.
        return [part['tool_use_id'] for row in native.records(self.transcript) if row.get('type') == 'user'
                for part in (row.get('message', {}).get('content') or []) if isinstance(part, dict) and part.get('type') == 'tool_result']

    def test_two_reads_of_one_name_never_grant_the_request_not_shown(self):
        # Claude shows the first Read ("1 of 2") while hgs records the latest
        # request, which may be the other one. Read the cards only once both
        # requests are recorded: the second replaces the first card.
        self.wait_for(lambda: '1 of 2' in self.screen())
        def settled():
            before = json.loads(self.record_path.read_text())
            time.sleep(.5)
            after = json.loads(self.record_path.read_text())
            return (after.get('pending_approval_at') and after.get('pending_approval_at') == before.get('pending_approval_at')
                    and len(after.get('claude_tool_uses', {})) == 2)
        self.wait_for(settled)
        cards = self.wait_for(lambda: [c for c in self.inspect()['pending_questions'] if c['can_answer']])
        bound = json.loads(self.record_path.read_text())['pending_approval']['tool_input']['file_path']
        other = str(self.target_A) if bound == str(self.target_B) else str(self.target_B)
        for card in cards:
            body = card['questions'][0]['body']
            if card['answer_transport'] == 'claude_hook':
                self.assertIn(bound, body); self.assertNotIn(other, body)
                self.assertFalse(card['panel_verified'], 'no Read panel proves the whole request')
        card = cards[0]
        body = card['questions'][0]['body']
        # No Read panel proves the whole request, so the screen answers its panel.
        self.assertEqual((card['answer_transport'], card.get('panel_verified')), ('claude_tui', None))
        displayed = [key for key, path in [('A', self.target_A), ('B', self.target_B)] if str(path) in body]
        self.assertEqual(len(displayed), 1, body)
        # The screen path may report its queued panel as uncertain; either way
        # the decision must act on the request the card showed.
        result = subprocess.run([str(transport.HGS), 'answer', self.name, '--json'], input=json.dumps(self.payload(card, 'Yes')),
                                text=True, capture_output=True, env=self.env, timeout=60)
        self.assertTrue(result.returncode == 0 or 'delivery uncertain' in result.stderr, result.stderr)
        first = self.wait_for(lambda: self.results())[0]
        self.assertEqual(first, 'toolu_read_' + displayed[0], 'the decision acts on the request the card shows')
        self.assertEqual(self.channels('answer.json') + self.channels('claimed.json') + self.channels('emitted.json'), [],
                         'nothing was offered to a hook')

    def test_claimed_decision_is_never_sent_again_through_any_transport(self):
        card = self.channel_card()
        # An offer the channel takes but cannot hand to Claude stays its own.
        directory = Path(self.channels('waiting.json')[0]).parent
        (directory / 'answer.json').write_text(json.dumps(dict(request_id=str(uuid.uuid4()), decision='always')))
        self.wait_for(lambda: (directory / 'closed.json').exists())
        self.assertEqual(json.loads((directory / 'closed.json').read_text())['reason'], 'invalid answer')
        locked = self.inspect()['pending_questions'][0]
        self.assertFalse(locked['can_answer'])
        self.assertIn('permission hook', locked['answer_unavailable_reason'])
        self.assertIn('already handed', self.answer(self.payload(card, 'Yes'), success=False))
        self.assertEqual(self.keys()[self.keys_before:], [])
        self.assertIn('Esc to cancel', self.screen())
        self.assertFalse(self.approved())

if __name__ == '__main__': unittest.main()
