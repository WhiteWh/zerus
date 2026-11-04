"""Verify native clear/input contracts with private homes and localhost providers."""
import http.server, json, os, shlex, subprocess, tempfile, threading, time, unittest, uuid
from pathlib import Path
import test_input
import test_claude_questions_native as native

@unittest.skipUnless(native.CLAUDE, 'Set HGS_CLAUDE_TEST_BIN for the isolated native clear test')
class NativeClearContext(unittest.TestCase):
    setUp = native.NativeClaudeQuestions.setUp
    script = native.NativeClaudeQuestions.script
    write_record = native.NativeClaudeQuestions.write_record
    wait_for = native.NativeClaudeQuestions.wait_for
    screen = native.NativeClaudeQuestions.screen
    hook = native.NativeClaudeQuestions.hook
    inspect = native.NativeClaudeQuestions.inspect
    key = native.NativeClaudeQuestions.key

    def tmux(self, *args, **kwargs):
        if args[0] == 'new-session':
            env = dict(HGS_SESSION=self.name, HGS_RUN_ID=self.run_id,
                       HGS_STATE_DIR=str(self.state), HGS_CONFIG_DIR=str(self.root / 'config'), PATH=self.env['PATH'])
            command = 'env ' + ' '.join(shlex.quote(k+'='+v) for k,v in env.items()) + ' ' + shlex.quote(str(test_input.HGS)) + ' __state hook'
            path = self.root / '.claude/settings.json'
            config = json.loads(path.read_text())
            config['hooks'] = {event: [{'hooks': [{'type':'command', 'command':command}]}] for event in getattr(self,'hook_events',['SessionStart'])}
            path.write_text(json.dumps(config))
        return test_input.InputTransport.tmux(self, *args, **kwargs)

    def test_native_clear_confirms_new_conversation_without_restarting_process(self):
        self.key('Escape'); self.key('Escape'); time.sleep(.5)
        if 'No, keep bypass permissions' in self.screen(): self.key('Enter'); time.sleep(.3)
        self.record.update(phase='idle', activity='idle', active_tools={}); self.write_record()
        request = dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id,
                       expected_conversation_id=self.conversation_id)
        result = subprocess.run([str(test_input.HGS), 'clear-context', self.name, '--json'],
            input=json.dumps(request), env=self.env, text=True, capture_output=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr + '\n' + self.screen())
        self.assertEqual(json.loads(result.stdout)['status'], 'confirmed')
        current = json.loads(self.record_path.read_text())
        self.assertNotEqual(current['conversation_id'], self.conversation_id)
        self.assertEqual(current['pid'], self.record['pid'])
        self.assertEqual(current['run_id'], self.run_id)
        self.assertNotIn('clear_context_request', current)
        self.assertNotIn('Which release should we prepare?', self.screen())
        self.assertNotIn('Ask questions with the tool now.', self.screen())
        fresh = self.inspect()
        self.assertFalse(fresh.get('provider_messages'))
        self.assertFalse(fresh.get('pending_questions'))
        self.assertFalse(fresh.get('cache_hint'))
        self.assertEqual(fresh['phase'], 'idle')

@unittest.skipUnless(native.CLAUDE, 'Set HGS_CLAUDE_TEST_BIN for the isolated native clear test')
class NativeInterruptedClear(unittest.TestCase):
    """Claude reports no hook when Stop ends a turn; its empty prompt still takes /clear."""
    import test_claude_approval_native as approval
    import test_questions as questions
    tool_name = 'Bash'
    tool_input = {'command': 'sleep 30; printf done > slept.txt', 'description': 'Wait for the fixture'}
    setUp = native.NativeClaudeQuestions.setUp
    tmux = NativeClearContext.tmux
    script = native.NativeClaudeQuestions.script
    write_record = native.NativeClaudeQuestions.write_record
    wait_for = native.NativeClaudeQuestions.wait_for
    screen = native.NativeClaudeQuestions.screen
    hook = native.NativeClaudeQuestions.hook
    inspect = native.NativeClaudeQuestions.inspect
    answer = questions.Questions.answer
    payload = questions.Questions.payload
    options = approval.NativeClaudeApprovals.options
    choose = approval.NativeClaudeApprovals.choose
    interrupt = approval.NativeClaudeInterrupt.test_escape_interrupt_is_recorded_without_a_hook
    del approval, questions

    def test_stopped_turn_clears_from_the_empty_native_prompt(self):
        self.interrupt()
        # Claude 2.1.296 can offer auto mode after a stopped approved tool. Its
        # dialog keeps the clear unavailable until it is answered.
        time.sleep(.5)
        if 'Make auto mode your default permission mode?' in self.screen():
            self.assertFalse(self.inspect()['clear_context_supported'])
            self.tmux('send-keys', '-t', self.pane, 'Down'); time.sleep(.15)
            self.tmux('send-keys', '-t', self.pane, 'Enter')
            self.wait_for(lambda: 'Make auto mode' not in self.screen())
        info = self.inspect()
        self.assertTrue(info['clear_context_supported'], info['clear_context_reason'] + '\n' + self.screen())
        self.assertFalse(info['compact_context_supported'])
        request = dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id,
                       expected_conversation_id=self.conversation_id)
        result = subprocess.run([str(test_input.HGS), 'clear-context', self.name, '--json'],
            input=json.dumps(request), env=self.env, text=True, capture_output=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr + '\n' + self.screen())
        self.assertEqual(json.loads(result.stdout)['status'], 'confirmed')
        current = json.loads(self.record_path.read_text())
        self.assertNotEqual(current['conversation_id'], self.conversation_id)
        self.assertEqual((current['run_id'], current['pid']), (self.run_id, self.record['pid']))
        self.assertEqual((current['activity'], current['phase']), ('idle', 'idle'))
        self.assertNotIn('What should Claude do instead?', self.screen())
        self.assertFalse((self.root/'work/slept.txt').exists())

@unittest.skipUnless(os.environ.get('HGS_CODEX_TEST_BIN'), 'Set HGS_CODEX_TEST_BIN for the isolated native Codex clear test')
class NativeCodexClear(unittest.TestCase):
    script = test_input.InputTransport.script
    write_record = test_input.InputTransport.write_record
    tmux = test_input.InputTransport.tmux
    wait_for = test_input.InputTransport.wait_for
    received = test_input.InputTransport.received
    tearDown = test_input.InputTransport.tearDown

    def fail(self, msg=None):
        # Only this credential-free fixture owns the captured terminal. Retain
        # its actual panel and cursor when a hosted contract fails, before the
        # private tmux server and home are cleaned up.
        try:
            screen = self.tmux('capture-pane', '-ep', '-t', self.pane, check=False)
            cursor = self.tmux('display-message', '-p', '-t', self.pane, '#{cursor_x}:#{cursor_y}', check=False).strip()
            record = json.loads(self.record_path.read_text())
            state = {key: record.get(key) for key in ('activity', 'phase', 'session_clear')}
            msg = f'{msg}\nNative fixture cursor: {cursor}; state: {state}\nNative fixture panel: {screen!r}'
        except (AttributeError, OSError, ValueError, subprocess.SubprocessError):
            pass
        # Explicit, so suites reusing this fixture can share its diagnostics.
        unittest.TestCase.fail(self, msg)

    def setUp(self):
        import test_clear_context
        test_input.InputTransport.setUp(self)
        self.tmux('kill-session', '-t', self.name)
        native_home = self.root / '.codex'
        native_home.mkdir()
        version = subprocess.check_output([os.environ['HGS_CODEX_TEST_BIN'], '--version'], text=True).strip()
        self.native_version = version.removeprefix('codex-cli ')
        width, height = os.environ.get('HGS_CODEX_TEST_SIZE', '110x35').split('x')
        self.assertTrue(width.isdigit() and height.isdigit(), 'Expected WIDTHxHEIGHT fixture geometry')
        self.geometry = (int(width), int(height))
        self.animations = os.environ.get('HGS_CODEX_TEST_ANIMATIONS', 'true')
        self.assertIn(self.animations, ('true', 'false'))
        # Plugins stay off (see assert_plugin_sync_off). Suites reusing this
        # fixture may serve a localhost provider and enable more features.
        (native_home / 'config.toml').write_text('''model="gpt-6.1-sol"
model_provider="fixture"
check_for_update_on_startup=false
[features]
plugins=false
''' + getattr(self, 'native_features', '') + '''[model_providers.fixture]
name="Fixture"
base_url="''' + getattr(self, 'provider_url', 'http://127.0.0.1:1/v1') + '''"
wire_api="responses"
''' + '[projects.' + json.dumps(str(self.root.resolve())) + ']\ntrust_level="trusted"\n')
        NativeCodexClear.assert_plugin_sync_off(self, native_home)
        path = native_home / 'sessions/2026/10/09' / ('rollout-fixture-' + self.conversation_id + '.jsonl')
        path.parent.mkdir(parents=True)
        usage_path = test_clear_context.ClearContext.usage_history(self, self.conversation_id, 109000, 100000)
        events = Path(usage_path).read_text().splitlines()
        metadata = json.loads(events[0])
        metadata.update(timestamp='2026-10-09T20:00:00Z')
        metadata['payload'].update(timestamp='2026-10-09T20:00:00Z', cwd=str(self.root), originator='codex_cli_rs',
            cli_version=self.native_version, source='cli', model_provider='fixture', base_instructions={'text':'Fixture.'})
        messages = [dict(type='response_item', timestamp='2026-10-09T20:00:01Z', payload=dict(
            type='message', role=role, content=[dict(type=kind, text=text)]))
            for role, kind, text in [('user', 'input_text', 'Previous fixture request'),
                                     ('assistant', 'output_text', 'Previous fixture answer')]]
        messages.extend([
            dict(type='event_msg', timestamp='2026-10-09T20:00:01Z', payload=dict(
                type='user_message', message='Previous fixture request', images=[], local_images=[])),
            dict(type='event_msg', timestamp='2026-10-09T20:00:02Z', payload=dict(
                type='agent_message', message='Previous fixture answer'))])
        path.write_text('\n'.join([json.dumps(metadata), *map(json.dumps, messages), events[1]]) + '\n')
        self.native_home = native_home
        # Suites reusing this setUp are not subclasses: call this class's launch.
        NativeCodexClear.launch(self, self.root, 'resume', self.conversation_id)
        self.wait_for(lambda: 'Previous fixture answer' in self.tmux('capture-pane', '-p', '-t', self.pane))
        self.record.update(transcript=str(path), last_message='Previous fixture answer')
        self.write_record()

    def assert_plugin_sync_off(self, home):
        # With plugins on, Codex clones its curated plugins from the network into
        # CODEX_HOME through a git child that can outlive the TUI or `codex exec`
        # and write into the fixture while its cleanup removes it ("Directory not
        # empty"). Every native fixture home turns them off; Codex reports the
        # effective value, so a renamed or ignored setting fails here instead.
        listing = subprocess.run([os.environ['HGS_CODEX_TEST_BIN'], 'features', 'list'],
            env=dict(self.env, CODEX_HOME=str(home)), stdin=subprocess.DEVNULL,
            capture_output=True, text=True, timeout=30)
        state = next((row[-1] for row in map(str.split, listing.stdout.splitlines()) if row[:1] == ['plugins']), None)
        self.assertEqual(state, 'false', 'Codex plugins in a fixture home:\n' + listing.stdout + listing.stderr)

    def launch(self, project, *arguments):
        width, height = map(str, self.geometry)
        self.tmux('new-session', '-d', '-s', self.name, '-x', width, '-y', height,
            'env', 'CODEX_HOME=' + str(self.native_home), os.environ['HGS_CODEX_TEST_BIN'], '--no-daemon',
            '-c', 'tui.animations=' + self.animations,
            '--dangerously-bypass-approvals-and-sandbox', '--dangerously-bypass-hook-trust',
            '-C', str(project), *arguments)
        self.pane = self.tmux('display-message', '-p', '-t', '=' + self.name + ':', '#{pane_id}').strip()
        self.tmux('set-option', '-t', '=' + self.name + ':', '@hgs_run', self.run_id)
        self.wait_for(lambda: '? for shortcuts' in self.tmux('capture-pane', '-p', '-t', self.pane))
        pid = int(self.tmux('display-message', '-p', '-t', self.pane, '#{pane_pid}').strip())
        start = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart='],
            env=dict(self.env, LC_ALL='C', TZ='UTC'), text=True).strip()
        self.record.update(pid=pid, process_start=start, pane=self.pane, cwd=str(project), launch_dir=str(project),
            agent_home=str(self.native_home))

    def assert_reset_decoration(self):
        logo = [row for row in (test_input.REPO / 'src/state/data/codex-empty-state-60x21.txt').read_text().splitlines()
                if not row.startswith('#')]
        def visible():
            rows = [row.strip() for row in self.tmux('capture-pane', '-p', '-t', self.pane).splitlines()
                    if row.strip()]
            return any(rows[index:index + len(logo)] == logo for index in range(len(rows)))
        if self.animations == 'true' and self.geometry == (95, 47):
            self.wait_for(visible)
        elif self.animations == 'false':
            self.assertFalse(visible(), 'The reduced-motion fixture must not draw the native logo')

    def test_ultra_picker_returns_to_a_sendable_composer_and_clears(self):
        import test_clear_context
        screen=lambda:self.tmux('capture-pane','-p','-t',self.pane)
        models=[dict(slug='gpt-6.1-sol',display_name='GPT-6.1-Sol',default_reasoning_level='medium',
                     supported_reasoning_levels=[dict(effort=e) for e in ['low','medium','high','xhigh','max','ultra']])]
        (self.native_home/'models_cache.json').write_text(json.dumps(dict(models=models)))
        self.record.update(model='gpt-6.1-sol',phase='idle',activity='idle',active_tools={})
        self.write_record()
        def change(level):
            request=dict(request_id=str(uuid.uuid4()),expected_run_id=self.run_id,
                         expected_conversation_id=self.conversation_id,model='gpt-6.1-sol',effort=level)
            result=subprocess.run([str(test_input.HGS),'settings',self.name,'--json'],
                                  input=json.dumps(request),env=self.env,text=True,capture_output=True,timeout=20)
            self.assertEqual(result.returncode,0,result.stderr+'\n'+screen())
            self.assertEqual(json.loads(result.stdout)['status'],'applied')
            current=json.loads(self.record_path.read_text())
            self.assertNotIn('settings_unconfirmed_at',current)
            self.assertEqual(current['resume_settings'],dict(model='gpt-6.1-sol',effort=level))
        before=(self.native_home/'config.toml').read_bytes()
        change('ultra')
        self.assertTrue(any(row.startswith('»') for row in screen().splitlines()),screen())
        change('high')
        change('ultra')
        self.assertTrue(test_clear_context.ClearContext.inspect(self)['clear_context_supported'])
        self.assertEqual(test_clear_context.ClearContext.clear(self)['status'],'confirmed')
        self.assertEqual(json.loads(self.record_path.read_text())['pid'],self.record['pid'])
        self.assertEqual((self.native_home/'config.toml').read_bytes(),before,'Session choices never save account defaults')

    def test_clear_is_visible_before_the_deferred_native_hook(self):
        import test_clear_context
        before = test_clear_context.ClearContext.inspect(self)
        self.assertEqual(before['session_usage']['context']['used'], 109000)
        self.assertFalse(before.get('session_clear'))
        receipt = test_clear_context.ClearContext.clear(self)
        self.assert_reset_decoration()
        self.assertEqual(receipt['status'], 'confirmed')
        fresh = test_clear_context.ClearContext.inspect(self)
        self.assertEqual(fresh['session_clear']['type'], 'SessionCleared')
        self.assertEqual(fresh['session_usage']['status'], 'unavailable')
        self.assertEqual(fresh['conversation_id'], self.conversation_id)
        current = json.loads(self.record_path.read_text())
        self.assertEqual(current['pid'], self.record['pid'])
        self.assertEqual(current['run_id'], self.run_id)
        self.assertTrue(current['session_clear']['awaiting_session_start'])
        self.assertEqual(sum(e['type']=='SessionCleared' for e in fresh['events']), 1)

    def test_clear_from_terminal_is_observed_once_without_resending(self):
        import test_clear_context
        self.tmux('send-keys', '-t', self.pane, '-l', '/clear')
        self.wait_for(lambda: '/clear' in self.tmux('capture-pane', '-p', '-t', self.pane))
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        self.assert_reset_decoration()
        self.wait_for(lambda: bool(test_clear_context.ClearContext.inspect(self).get('session_clear')))
        first = test_clear_context.ClearContext.inspect(self)
        second = test_clear_context.ClearContext.inspect(self)
        self.assertEqual(first['session_clear'], second['session_clear'])
        self.assertEqual(second['conversation_id'], self.conversation_id)
        self.assertEqual(second['session_usage']['status'], 'unavailable')
        self.assertEqual(sum(e['type']=='SessionCleared' for e in second['events']), 1)
        current = json.loads(self.record_path.read_text())
        self.assertEqual(current['pid'], self.record['pid'])
        self.assertEqual(current['run_id'], self.run_id)
        self.assertNotIn('clear_context_request', current)

    def test_native_draft_is_preserved_and_prevents_clear(self):
        import test_clear_context
        draft = 'Keep this unsent native draft'
        self.tmux('send-keys', '-t', self.pane, '-l', draft)
        self.wait_for(lambda: draft in self.tmux('capture-pane', '-p', '-t', self.pane))
        error = test_clear_context.ClearContext.clear(self, success=False)
        self.assertIn('draft', error.lower())
        screen = self.tmux('capture-pane', '-p', '-t', self.pane)
        self.assertIn(draft, screen)
        self.assertNotIn('/clear', screen)
        fresh = test_clear_context.ClearContext.inspect(self)
        self.assertFalse(fresh.get('session_clear'))
        self.assertEqual(fresh['session_usage']['context']['used'], 109000)

    def test_native_failed_turn_can_clear_without_an_idle_hook(self):
        import test_clear_context
        class Rejected(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def do_POST(self):
                length = int(self.headers.get('Content-Length', '0'))
                if not 0 <= length <= 1024 * 1024:
                    self.send_error(413)
                    return
                self.rfile.read(length)
                self.server.requests += 1
                body = json.dumps(dict(error=dict(message='Synthetic fixture request rejected',
                                  type='invalid_request_error', code='fixture_rejected'))).encode()
                self.send_response(400)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Rejected)
        server.requests = 0
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        config = self.native_home / 'config.toml'
        config.write_text(config.read_text().replace('http://127.0.0.1:1/v1',
                          f'http://127.0.0.1:{server.server_port}/v1')
                          + 'request_max_retries=0\nstream_max_retries=0\n')
        previous = self.record['pid']
        self.tmux('kill-session', '-t', '=' + self.name)
        for _ in range(250):
            if subprocess.run(['ps', '-p', str(previous)], capture_output=True).returncode:
                break
            time.sleep(.02)
        self.launch(self.root, 'resume', self.conversation_id)
        started = time.time()
        self.record.update(activity='busy', phase='working', main_done=False,
                           turn_started=started, last_main_progress_at=started)
        self.write_record()
        message = 'Exercise one synthetic provider failure.'
        self.tmux('send-keys', '-t', self.pane, '-l', message)
        self.wait_for(lambda: message in self.tmux('capture-pane', '-p', '-t', self.pane))
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        def failed():
            info = test_clear_context.ClearContext.inspect(self)
            return info if info['phase'] == 'error' and info['clear_context_supported'] else None
        info = self.settle(failed, 'The native failed completion did not enable explicit clear')
        self.assertFalse(info['compact_context_supported'])
        # A native release may make several transport attempts for one turn.
        # Clearing must add none; the transcript proves one submitted user turn.
        requests_before_clear = server.requests
        self.assertGreaterEqual(requests_before_clear, 1)
        rows=[json.loads(line) for line in Path(self.record['transcript']).read_text().splitlines()]
        submitted=[row for row in rows if row.get('type')=='event_msg'
                   and row.get('payload',{}).get('type')=='user_message'
                   and row['payload'].get('message')==message]
        self.assertEqual(len(submitted),1)
        self.assertEqual(json.loads(self.record_path.read_text())['phase'], 'working',
                         'The fixture intentionally has no idle hook')
        receipt = test_clear_context.ClearContext.clear(self)
        self.assertEqual(receipt['status'], 'confirmed')
        self.assert_reset_decoration()
        current = json.loads(self.record_path.read_text())
        self.assertEqual((current['pid'], current['run_id']), (self.record['pid'], self.run_id))
        self.assertEqual(server.requests, requests_before_clear, 'Clear never replays the failed model request')

    def test_header_above_an_answered_first_message_is_not_a_clear(self):
        # A new Codex session keeps its header, here with a folder outside the
        # fixture home printed in full, above the first answered message.
        # Only the empty state after an actual /clear is a reset.
        import test_clear_context, test_external_agents_native
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), test_external_agents_native.Response)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        outside = tempfile.TemporaryDirectory(prefix='hgs-project-')
        self.addCleanup(outside.cleanup)
        project = Path(outside.name).resolve()
        self.assertFalse(project.is_relative_to(self.root))
        # This session lives through a turn: keep it on the localhost provider
        # without the plugin marketplace sync, whose git checkout into the
        # private home could otherwise still be running at fixture cleanup.
        config = self.native_home / 'config.toml'
        config.write_text(config.read_text().replace('http://127.0.0.1:1/v1', f'http://127.0.0.1:{server.server_port}/v1')
            + 'request_max_retries=0\nstream_max_retries=0\n[projects.' + json.dumps(str(project)) + ']\ntrust_level="trusted"\n')
        previous = self.record['pid']
        self.tmux('kill-session', '-t', '=' + self.name)
        for _ in range(250):
            if subprocess.run(['ps', '-p', str(previous)], capture_output=True).returncode: break
            time.sleep(.02)
        self.launch(project)
        def wait(predicate, message):
            deadline, previous_screen = time.monotonic() + 30, None
            while True:
                screen = self.tmux('capture-pane', '-p', '-t', self.pane)
                if predicate(screen, previous_screen): return screen
                self.assertLess(time.monotonic(), deadline, message)
                previous_screen = screen
                time.sleep(.3)
        header = lambda screen: str(project) in [row.strip() for row in screen.splitlines()[:4]]
        wait(lambda screen, _: header(screen), 'Codex must print the folder outside its home in full')
        message, answer = 'First fixture message', 'Isolated review complete.'
        self.tmux('send-keys', '-t', self.pane, '-l', message)
        self.wait_for(lambda: message in self.tmux('capture-pane', '-p', '-t', self.pane))
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        # The localhost provider answers at once; wait until Codex is idle.
        screen = wait(lambda screen, previous: answer in screen and screen == previous
                      and 'esc to interrupt' not in screen.lower(), 'The first message was not answered')
        self.assertTrue(header(screen), screen)
        self.record.update(prompt=message, last_message=answer)
        self.write_record()
        for _ in range(5):
            fresh = test_clear_context.ClearContext.inspect(self)
            self.assertFalse(fresh.get('session_clear'), screen)
            self.assertEqual(fresh['conversation_id'], self.conversation_id)
            time.sleep(.1)
        self.assertNotIn('session_clear', json.loads(self.record_path.read_text()))
        receipt = test_clear_context.ClearContext.clear(self)
        self.assert_reset_decoration()
        self.assertEqual(receipt['status'], 'confirmed')
        self.assertNotIn(message, self.tmux('capture-pane', '-p', '-t', self.pane))
        fresh = test_clear_context.ClearContext.inspect(self)
        self.assertEqual(sum(e['type']=='SessionCleared' for e in fresh['events']), 1)
        current = json.loads(self.record_path.read_text())
        self.assertTrue(current['session_clear']['awaiting_session_start'])
        self.assertEqual(current['pid'], self.record['pid'])
        self.assertEqual(current['run_id'], self.run_id)

    def settle(self, predicate, message, timeout=30):
        deadline = time.monotonic() + timeout
        while not (value := predicate()):
            self.assertLess(time.monotonic(), deadline, message)
            time.sleep(.2)
        return value

    def resumed(self, hook=None):
        """Relaunch the saved conversation as `hgs resume` leaves it: Codex defers
        SessionStart to the next turn, so the binding still expects the saved
        conversation. A localhost provider answers that turn and the native hooks
        reach hgs; the inspection once Activity may send the first message."""
        import test_clear_context, test_external_agents_native
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), test_external_agents_native.Response)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        config = self.native_home / 'config.toml'
        config.write_text(config.read_text().replace('http://127.0.0.1:1/v1', f'http://127.0.0.1:{server.server_port}/v1')
            + 'request_max_retries=0\nstream_max_retries=0\n')
        env = dict(HGS_SESSION=self.name, HGS_RUN_ID=self.run_id, HGS_STATE_DIR=str(self.state),
                   HGS_CONFIG_DIR=str(self.root / 'config'), PATH=self.env['PATH'])
        command = ('env ' + ' '.join(shlex.quote(k + '=' + v) for k, v in env.items()) + ' '
                   + (hook or shlex.quote(str(test_input.HGS)) + ' __state hook'))
        (self.native_home / 'hooks.json').write_text(json.dumps(dict(hooks={kind: [dict(hooks=[
            dict(type='command', command=command)])] for kind in ('SessionStart', 'SessionEnd', 'UserPromptSubmit', 'Stop')})))
        previous = self.record['pid']
        self.tmux('kill-session', '-t', '=' + self.name)
        for _ in range(250):
            if subprocess.run(['ps', '-p', str(previous)], capture_output=True).returncode: break
            time.sleep(.02)
        self.launch(self.root, 'resume', self.conversation_id)
        self.settle(lambda: 'Previous fixture answer' in self.tmux('capture-pane', '-p', '-t', self.pane),
                    'Codex did not show the resumed conversation')
        # The tmux server is this fixture's supervisor: the parent of the exact Codex process.
        parent = subprocess.check_output(['ps', '-p', str(self.record['pid']), '-o', 'ppid='], text=True).strip()
        start = subprocess.check_output(['ps', '-p', parent, '-o', 'lstart='],
            env=dict(self.env, LC_ALL='C', TZ='UTC'), text=True).strip()
        self.record.update(run_identity_version=1, startup_kind='resume', supervisor=dict(pid=int(parent), start=start),
            expected_id=self.conversation_id, activity='unknown', phase='unknown', last_event_at=0,
            base=['codex'], prompt='Previous fixture request')
        self.write_record()
        def ready():
            info = test_clear_context.ClearContext.inspect(self)
            return info if info.get('resume_message_can_send') else None
        return self.settle(ready, 'Activity could not verify the resumed conversation')

    def test_resumed_conversation_clears_before_its_deferred_session_start(self):
        import test_clear_context
        info = self.resumed()
        self.assertEqual(info['expected_id'], self.conversation_id)
        self.assertTrue(info['clear_context_supported'], info)
        self.assertFalse(info['compact_context_supported'])
        self.assertIn('pause', info['allowed_actions'])
        receipt = test_clear_context.ClearContext.clear(self)
        self.assert_reset_decoration()
        self.assertEqual(receipt['status'], 'confirmed')
        current = json.loads(self.record_path.read_text())
        self.assertEqual((current['conversation_id'], current['expected_id']), (self.conversation_id, self.conversation_id))
        self.assertTrue(current['session_clear']['awaiting_session_start'])
        self.assertFalse(current.get('error'))
        fresh = test_clear_context.ClearContext.inspect(self)
        self.assertEqual(fresh['session_clear']['type'], 'SessionCleared')
        self.assertTrue(fresh['resume_message_can_send'], fresh.get('resume_message_reason'))
        # Codex reports the cleared conversation only with the next turn's SessionStart.
        test_input.InputTransport.send(self, test_input.InputTransport.payload(self, text='First fixture message'))
        self.assert_new_conversation(fresh['session_clear']['at'])

    def test_archive_restored_conversation_clears_and_settles_on_its_real_start(self):
        import test_clear_context
        test_clear_context.ResumedClearContext.restored(self)
        info = test_clear_context.ClearContext.inspect(self)
        self.assertTrue(info['resume_message_can_send'], info.get('resume_message_reason'))
        self.assertTrue(info['clear_context_supported'], info.get('clear_context_reason'))
        self.assertFalse(info['compact_context_supported'])
        self.assertFalse(info['fork_supported'])
        self.assertNotIn('pause', info['allowed_actions'])
        receipt = test_clear_context.ClearContext.clear(self)
        self.assertEqual(receipt['status'], 'confirmed')
        current = json.loads(self.record_path.read_text())
        self.assertEqual(self.archive_path.read_bytes(), self.archive_bytes)
        self.assertEqual((current['conversation_id'], current['expected_id']),
                         (self.conversation_id, self.conversation_id))
        self.assertTrue(current['session_clear']['awaiting_session_start'])
        self.assertNotIn('resume_pending', current)
        fresh = test_clear_context.ClearContext.inspect(self)
        test_input.InputTransport.send(self, test_input.InputTransport.payload(self, text='First restored fixture message'))
        self.assert_new_conversation(fresh['session_clear']['at'])
        current = json.loads(self.record_path.read_text())
        self.assertFalse(self.archive_path.exists())
        self.assertNotIn('restore_archive_id', current)
        self.assertNotIn('resume_pending', current)

    def test_resumed_terminal_clear_is_confirmed_by_its_deferred_session_start(self):
        self.resumed()
        self.tmux('send-keys', '-t', self.pane, '-l', '/clear')
        self.wait_for(lambda: '/clear' in self.tmux('capture-pane', '-p', '-t', self.pane))
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        self.settle(lambda: 'Previous fixture answer' not in self.tmux('capture-pane', '-p', '-t', self.pane),
                    'Codex did not clear the resumed conversation')
        # No inspection observes the reset panel before the message's own hook.
        message = 'First fixture message'
        self.tmux('send-keys', '-t', self.pane, '-l', message)
        self.wait_for(lambda: message in self.tmux('capture-pane', '-p', '-t', self.pane))
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        self.assert_new_conversation(None)

    def test_already_rejected_resumed_clear_recovers_from_a_real_hook_after_the_lost_start(self):
        import sys
        hook = self.root / 'skip-rejected-start.py'
        rejected = self.root / 'rejected-start.json'
        hook.write_text('''import json,pathlib,subprocess,sys
data=sys.stdin.buffer.read(1024*1024+1)
if len(data)>1024*1024:raise SystemExit(2)
event=json.loads(data)
if event.get('hook_event_name')=='SessionStart' and event.get('session_id')!=OLD:
 pathlib.Path(REJECTED).write_text(json.dumps({'session_id':event['session_id']}))
else:
 result=subprocess.run([HGS,'__state','hook'],input=data,timeout=10)
 raise SystemExit(result.returncode)
'''.replace('OLD', repr(self.conversation_id)).replace('REJECTED', repr(str(rejected)))
            .replace('HGS', repr(str(test_input.HGS))))
        self.resumed(shlex.quote(sys.executable) + ' ' + shlex.quote(str(hook)))
        import test_clear_context
        self.record['clear_context_request'] = dict(request_id=str(uuid.uuid4()),
                                                  conversation_id=self.conversation_id, at=time.time())
        self.write_record()
        self.tmux('send-keys', '-t', self.pane, '-l', '/clear')
        self.wait_for(lambda: '/clear' in self.tmux('capture-pane', '-p', '-t', self.pane))
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        self.settle(lambda: 'Previous fixture answer' not in self.tmux('capture-pane', '-p', '-t', self.pane),
                    'The original fixture clear did not finish')
        # Recreate the precise older binding after its rejected start. The
        # native successor's real SessionStart is deliberately discarded, not
        # replayed. A later real hook must prove and publish the current ID.
        self.record = json.loads(self.record_path.read_text())
        self.assertNotIn('session_clear', self.record)
        self.record.update(error='agent resumed a different conversation', activity='unknown', phase='unknown',
                           conversation_state='ended', last_event_at=time.time(),
                           session_end=dict(run_id=self.run_id, conversation_id=self.conversation_id, at=time.time()))
        self.write_record()
        message = 'First fixture message after the rejected start'
        self.tmux('send-keys', '-t', self.pane, '-l', message)
        self.wait_for(lambda: message in self.tmux('capture-pane', '-p', '-t', self.pane))
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        self.assert_new_conversation(None)
        current = json.loads(self.record_path.read_text())
        self.assertEqual(current['conversation_id'], json.loads(rejected.read_text())['session_id'])
        self.assertEqual(current['pid'], self.record['pid'])
        events = test_clear_context.ClearContext.inspect(self)['events']
        self.assertEqual(sum(event['type'] == 'SessionCleared' for event in events), 1)
        self.assertFalse(any(event['type'] == 'SessionStart' and event['conversation_id'] == current['conversation_id']
                             for event in events))

    def assert_new_conversation(self, cleared_at):
        import test_clear_context
        current = self.settle(lambda: (lambda record: record.get('conversation_id') != self.conversation_id and record)(
            json.loads(self.record_path.read_text())), 'Codex did not report the cleared conversation')
        self.settle(lambda: 'Isolated review complete.' in self.tmux('capture-pane', '-p', '-t', self.pane),
                    'The first message after the clear was not answered')
        self.assertFalse(current.get('error'), current)
        self.assertNotIn('expected_id', current)
        self.assertEqual((current['pid'], current['run_id']), (self.record['pid'], self.run_id))
        history = Path(current['transcript'])
        self.assertEqual(json.loads(history.read_text().splitlines()[0])['payload']['id'], current['conversation_id'])
        self.assertEqual(current['session_clear']['previous_conversation_id'], self.conversation_id)
        fresh = test_clear_context.ClearContext.inspect(self)
        self.assertEqual(fresh['cleared_conversations'], [self.conversation_id])
        if cleared_at is not None:
            self.assertEqual(fresh['session_clear']['at'], cleared_at)
        self.assertEqual(sum(e['type'] == 'SessionCleared' for e in fresh['events']), 1)

    def test_resumed_conversation_pauses_before_its_deferred_session_start(self):
        info = self.resumed()
        self.assertIn('pause', info['allowed_actions'])
        histories = sorted(self.native_home.glob('sessions/**/*.jsonl'))
        result = subprocess.run([str(test_input.HGS), 'pause', self.name], env=self.env,
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        paused = json.loads(self.record_path.read_text())
        self.assertTrue(paused['paused'])
        self.assertFalse(paused.get('error'), paused)
        # A later resume restores the same saved conversation: Codex started no other one.
        self.assertEqual((paused['conversation_id'], paused['expected_id']), (self.conversation_id, self.conversation_id))
        self.assertNotEqual(subprocess.run(['ps', '-p', str(self.record['pid'])], capture_output=True).returncode, 0)
        self.assertEqual(sorted(self.native_home.glob('sessions/**/*.jsonl')), histories)
        self.assertIn('Previous fixture answer', Path(paused['transcript']).read_text())

    def test_foreign_directory_does_not_confirm_a_terminal_clear(self):
        import test_clear_context
        self.record['cwd'] = str(self.root / 'other-project')
        self.write_record()
        self.tmux('send-keys', '-t', self.pane, '-l', '/clear')
        self.wait_for(lambda: '/clear' in self.tmux('capture-pane', '-p', '-t', self.pane))
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        time.sleep(.3)
        fresh = test_clear_context.ClearContext.inspect(self)
        self.assertFalse(fresh.get('session_clear'))
        self.assertEqual(fresh['session_usage']['context']['used'], 109000)

if __name__ == '__main__': unittest.main()
