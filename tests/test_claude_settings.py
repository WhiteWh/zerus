#!/usr/bin/env python3
"""Claude session model settings against a private tmux socket, never real agents.

The fake mirrors Claude Code 2.1.296: /model opens a picker whose Enter saves a
new default and whose `s` key applies the selection to this session only. Each
applied choice writes Claude's receipt to the conversation transcript.
ClaudeSettings327 repeats every case with the picker text of Claude Code 2.1.327.
"""
import json
import os
import signal
import subprocess
import unittest
import uuid

import test_input

AGENT = test_input.SIGNAL_SAFE_READ + r'''
import json, os, pathlib, subprocess, sys, termios, tty
root = pathlib.Path(os.environ['INPUT_FIXTURE'])
children = []
def launch_hook(*args):
    request = json.loads((root / 'nested-launch.json').read_text())
    with (root / 'nested.log').open('ab') as log:
        children.append(subprocess.Popen(request['argv'], env=request['env'],
            stdin=subprocess.DEVNULL, stdout=log, stderr=log))
signal.signal(signal.SIGUSR2, launch_hook)
fd = sys.stdin.fileno(); original = termios.tcgetattr(fd); tty.setraw(fd)
ROWS = [('Default (recommended)', 'Use the default model (currently Opus 5.5) · $4/$20 per Mtok', None),
        ('Opus', 'Opus 5.5 · Best for everyday, complex tasks · $4/$20 per Mtok', 'Opus 5.5'),
        ('Sonnet', 'Sonnet 5.5 · Efficient for routine tasks · $2/$10 per Mtok', 'Sonnet 5.5'),
        ('Haiku', 'Haiku 5.5 · Fastest for quick answers · $0.10/$0.50 per Mtok', 'Haiku 5.5')]
LEVELS = ['low', 'medium', 'high', 'xhigh', 'max']
GLYPHS = dict(low='○', medium='◐', high='●', xhigh='◉', max='◈')
current = 0; effort = 'medium'; text = ''; stage = ''; selected = 0; level = 1; dialog = ''
def show():
    rule = '─' * 100
    if stage == 'picker':
        # Claude Code 2.1.327 rewords the subtitle and Enter's action and moves
        # the arrows' hint from the effort line into the legend.
        layout_327 = (root / 'layout-2.1.327').exists()
        session_only = not (root / 'no-session-only').exists()
        subtitle = ('Your pick is the default for new sessions. For other models, use --model.' if layout_327
                    else 'Switch between Claude models. Your pick becomes the default for new sessions.')
        screen = '\x1b[2J\x1b[8;1H' + '▔' * 100 + '\r\n   Select model\r\n   ' + subtitle + '\r\n\r\n'
        for i, (label, description, _) in enumerate(ROWS):
            mark = label + (' ✔' if i == current else '')
            screen += ('   ❯ ' if i == selected else '     ') + str(i + 1) + '. ' + mark.ljust(25) + description + '\r\n'
        name = LEVELS[level]
        screen += '\r\n   ' + GLYPHS[name] + ' ' + ('xHigh' if name == 'xhigh' else name.title()) + ' effort'
        screen += '\r\n\r\n' if layout_327 else ' ←/→ to adjust\r\n\r\n'
        if layout_327:
            screen += '   ←/→ to adjust · ' + ('s to use this session only · ' if session_only else '') + 'Enter to save'
        else:
            screen += '   Enter to set as default'
            if session_only: screen += ' · s to use this session only'
        screen += ' · Esc to cancel'
    elif stage == 'dialog':
        hook = dialog == 'hook'
        screen = '\x1b[2J\x1b[12;1H' + '▔' * 100 + '\r\n   Switch model?\r\n   ' + ('A PreModelSwitch hook asked you to confirm' if hook else 'Your next response will be slower and use more tokens')
        screen += '\r\n   This conversation is cached for the current model.\r\n   ❯ 1. Yes, switch to ' + ROWS[selected][2] + '\r\n     2. No, go back'
    else:
        completion = '  ❯ /model                      Set the AI model for Claude Code\r\n' if text == '/model' else '\r\n'
        indicator = GLYPHS[effort] + ' ' + effort + ' · /effort'
        screen = '\x1b[2J\x1b[1;1H● Hi!\x1b[22;1H' + completion + ' ' * (99 - len(indicator)) + indicator + '\r\n' + rule
        screen += '\r\n❯ ' + text + '\r\n' + rule + '\r\n  ⏸ manual mode on\x1b[25;' + str(3 + len(text)) + 'H'
    os.write(1, screen.encode())
def receipt(message):
    conversation = (root / 'conversation').read_text().strip()
    row = dict(type='user', sessionId=conversation, isSidechain=False, message=dict(role='user', content='<local-command-stdout>' + message + '</local-command-stdout>'))
    with (root / (conversation + '.jsonl')).open('a') as f: f.write(json.dumps(row) + '\n')
def commit():
    global current, effort, stage
    changed = LEVELS[level] != effort
    current = selected; effort = LEVELS[level]; stage = ''
    name = ROWS[current][2] or 'Default'
    receipt('Set model to `' + name + '` for this session only' + (' with `' + effort + '` effort' if changed else ''))
def key(value):
    global text, stage, selected, level, dialog
    if stage == 'picker':
        if value == b'\x1b[B': selected = (selected + 1) % len(ROWS)
        elif value == b'\x1b[A': selected = (selected - 1) % len(ROWS)
        elif value == b'\x1b[C': level = min(len(LEVELS) - 1, level + 1)
        elif value == b'\x1b[D': level = max(0, level - 1)
        elif value == b'\x1b': stage = ''; receipt('Kept model as `' + (ROWS[current][2] or 'Default') + '`')
        elif value == b'\r': (root / 'saved-default').write_text(ROWS[selected][0]); stage = ''
        elif value == b's':
            if (root / 'hook').exists(): stage = 'dialog'; dialog = 'hook'
            elif (root / 'dialog').exists() and selected != current: stage = 'dialog'; dialog = 'cache'
            else: commit()
    elif stage == 'dialog':
        if value == b'\r' and dialog == 'cache': (root / 'dialog').unlink(); commit()
        elif value == b'\x1b': stage = 'picker'
    elif value == b'\r' and text == '/model':
        stage = 'picker'; text = ''; selected = current; level = LEVELS.index(effort)
    elif value[:1] != b'\x1b' and value != b'\r':
        text += value.decode('utf8', 'replace')
        if text == '/model' and (root / 'replacement-picker').exists():
            stage = 'picker'; text = ''; selected = current; level = LEVELS.index(effort)
    show()
def external(*args):
    if (root / 'screen').exists(): os.write(1, (root / 'screen').read_bytes())
import signal
signal.signal(signal.SIGUSR1, external)
os.write(1, b'\x1b[?2004h'); show(); (root / 'agent-pid').write_text(str(os.getpid()))
pending = b''
try:
    while True:
        value = read_input(fd)
        if not value: break
        with (root / 'received').open('ab') as f: f.write(value)
        if (root / 'freeze').exists(): continue
        pending += value
        while pending:
            if pending[:1] == b'\x1b':
                n = 3 if pending[1:2] == b'[' else 1
                if len(pending) < n: break
                current_key, pending = pending[:n], pending[n:]
            else:
                current_key, pending = pending[:1], pending[1:]
            key(current_key)
finally:
    termios.tcsetattr(fd, termios.TCSANOW, original)
'''

CATALOG = {"version": 2, "catalog": {"surface": "cc", "config": {"models": [
    {"id": "claude-opus-5-5", "name": "Opus 5.5", "short_name": "Opus", "section": "main",
     "thinking": {"type": "effort", "effort_options": [{"id": v} for v in ["low", "medium", "high", "xhigh", "max"]]}},
    {"id": "claude-sonnet-5-5", "name": "Sonnet 5.5", "short_name": "Sonnet", "section": "main",
     "thinking": {"type": "effort", "effort_options": [{"id": v} for v in ["low", "medium", "high", "xhigh", "max"]]}},
    {"id": "claude-haiku-5-5", "name": "Haiku 5.5", "short_name": "Haiku", "section": "main",
     "thinking": {"type": "effort", "effort_options": [{"id": v} for v in ["low", "medium", "high", "xhigh", "max"]]}},
    {"id": "claude-fable-5-1", "name": "Fable 5.1", "short_name": "Fable", "section": "main",
     "thinking": {"type": "effort", "effort_options": [{"id": v} for v in ["low", "medium", "high"]]}},
    {"id": "claude-opus-4-8", "name": "Opus 4.8", "short_name": "Opus", "section": "overflow"},
]}, "state": {}}}


class ClaudeSettings(unittest.TestCase):
    script = test_input.InputTransport.script
    write_record = test_input.InputTransport.write_record
    tmux = test_input.InputTransport.tmux
    wait_for = test_input.InputTransport.wait_for
    received = test_input.InputTransport.received
    tearDown = test_input.InputTransport.tearDown

    def setUp(self):
        original = test_input.FAKE_AGENT
        test_input.FAKE_AGENT = AGENT
        try: test_input.InputTransport.setUp(self)
        finally: test_input.FAKE_AGENT = original
        self.home = self.root / '.claude'
        (self.home / 'cache/model-catalog').mkdir(parents=True)
        (self.home / 'cache/model-catalog/org-0123456789ab-cc.json').write_text(json.dumps(CATALOG))
        self.settings_file = self.home / 'settings.json'
        self.settings_file.write_text('{"model":"opus"}\n')
        (self.root / 'conversation').write_text(self.conversation_id)
        self.transcript = self.root / (self.conversation_id + '.jsonl')
        self.transcript.write_text(json.dumps(dict(type='assistant', sessionId=self.conversation_id, requestedModel='claude-opus-5-5',
                                                   effort='medium', message=dict(model='claude-opus-5-5', content=[]))) + '\n')
        self.record.update(agent='claude', model='claude-opus-5-5', effort='medium', agent_home=str(self.home),
                           transcript=str(self.transcript))
        self.write_record()
        self.wait_for(lambda: '◐ medium · /effort' in self.tmux('capture-pane', '-p', '-t', self.pane))

    def payload(self, model, effort):
        return dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id,
                    expected_conversation_id=self.conversation_id, model=model, effort=effort)

    def settings(self, payload, success=True):
        result = subprocess.run([str(test_input.HGS), 'settings', self.name, '--json'], env=self.env,
                                input=json.dumps(payload), text=True, capture_output=True, timeout=30)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        return result.stderr

    def inspect(self):
        result = subprocess.run([str(test_input.HGS), 'inspect', self.name], env=self.env, text=True,
                                capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def assertDefaultsUntouched(self):
        self.assertEqual(self.settings_file.read_text(), '{"model":"opus"}\n')
        self.assertFalse((self.root / 'saved-default').exists())

    def test_inspect_offers_the_pickers_catalog_and_its_notice(self):
        info = self.inspect()
        self.assertTrue(info['settings_change_supported'], info['settings_change_reason'])
        self.assertEqual([m['id'] for m in info['model_options']],
                         ['claude-opus-5-5', 'claude-sonnet-5-5', 'claude-haiku-5-5', 'claude-fable-5-1', 'claude-opus-4-8'])
        self.assertEqual((info['model'], info['effort']), ('claude-opus-5-5', 'medium'))
        self.assertIn('re-reads the full history', info['settings_change_notice'])
        self.assertTrue(info['effort_change_supported'], info['effort_change_reason'])
        self.assertEqual(self.received(), b'')

    def test_model_and_effort_apply_to_this_session_only(self):
        p = self.payload('claude-sonnet-5-5', 'high')
        result = self.settings(p)
        self.assertEqual(result['status'], 'applied')
        # Open the picker, walk from the current row to Sonnet, raise effort, apply session-only.
        self.assertEqual(self.received(), b'/model\r' + b'\x1b[B' * 2 + b'\x1b[C' + b's')
        info = self.inspect()
        self.assertEqual((info['model'], info['effort']), ('claude-sonnet-5-5', 'high'))
        self.assertDefaultsUntouched()
        sent = self.received(); self.settings(p); self.assertEqual(self.received(), sent)
        # Already shown: nothing is typed again.
        self.settings(self.payload('claude-sonnet-5-5', 'high'))
        self.assertEqual(self.received(), sent)

    def test_effort_only_change_keeps_the_current_model(self):
        self.settings(self.payload('claude-opus-5-5', 'low'))
        # The session runs the default model: select its own row, then lower effort.
        self.assertEqual(self.received(), b'/model\r\x1b[B\x1b[D' + b's')
        info = self.inspect()
        self.assertEqual((info['model'], info['effort']), ('claude-opus-5-5', 'low'))
        self.assertDefaultsUntouched()

    def test_cache_notice_is_confirmed_once(self):
        (self.root / 'dialog').touch()
        self.settings(self.payload('claude-haiku-5-5', 'medium'))
        self.assertEqual(self.received(), b'/model\r' + b'\x1b[B' * 3 + b's\r')
        self.assertEqual(self.inspect()['model'], 'claude-haiku-5-5')
        self.assertDefaultsUntouched()

    def test_hook_confirmation_is_declined_and_reported(self):
        (self.root / 'hook').touch()
        error = self.settings(self.payload('claude-haiku-5-5', 'medium'), success=False)
        self.assertIn('PreModelSwitch', error)
        self.assertEqual(self.received(), b'/model\r' + b'\x1b[B' * 3 + b's\x1b\x1b')
        self.assertEqual(self.inspect()['model'], 'claude-opus-5-5')
        self.assertNotIn('Haiku 5.5` for this session', self.transcript.read_text())
        self.assertDefaultsUntouched()

    def test_model_missing_from_the_picker_cancels_without_applying(self):
        error = self.settings(self.payload('claude-fable-5-1', 'high'), success=False)
        self.assertIn('not offered', error)
        received = self.received()
        self.assertTrue(received.startswith(b'/model\r'))
        self.assertTrue(received.endswith(b'\x1b'))
        self.assertNotIn(b's', received.removeprefix(b'/model\r'))
        self.assertEqual(self.inspect()['model'], 'claude-opus-5-5')
        self.assertDefaultsUntouched()
        # Claude read every key and closed its picker at the empty prompt, so
        # nothing is left unconfirmed: the next change is accepted.
        self.assertNotIn('settings_unconfirmed_at', json.loads(self.record_path.read_text()))
        self.assertEqual(self.settings(self.payload('claude-sonnet-5-5', 'high'))['status'], 'applied')
        self.assertEqual(self.inspect()['model'], 'claude-sonnet-5-5')

    def test_picker_without_session_only_action_cancels_before_selection(self):
        (self.root / 'no-session-only').touch()
        error = self.settings(self.payload('claude-sonnet-5-5', 'high'), success=False)
        self.assertIn('does not offer a session-only choice', error)
        self.assertEqual(self.received(), b'/model\r\x1b')
        self.assertNotIn('Select model', self.tmux('capture-pane', '-p', '-t', self.pane))
        self.assertDefaultsUntouched()

    def test_picker_replacing_the_command_is_not_cancelled_as_ours(self):
        (self.root / 'replacement-picker').touch()
        error = self.settings(self.payload('claude-sonnet-5-5', 'high'), success=False)
        self.assertIn('provider did not confirm the expected change', error)
        self.assertEqual(self.received(), b'/model', 'cleanup cancelled a picker we never opened')
        self.assertIn('Select model', self.tmux('capture-pane', '-p', '-t', self.pane))
        self.assertDefaultsUntouched()

    def test_unconfirmed_change_is_not_replayed(self):
        (self.root / 'freeze').touch()
        p = self.payload('claude-sonnet-5-5', 'high')
        self.assertIn('not confirmed', self.settings(p, success=False))
        self.assertEqual(self.received(), b'/model')
        self.assertIn('already', self.settings(p, success=False))
        self.assertEqual(self.received(), b'/model')
        # Claude has not read the typed keys: a new change or message types nothing.
        record = json.loads(self.record_path.read_text())
        self.assertIn('settings_unconfirmed_at', record)
        self.assertFalse(record.get('input_pending_at'))
        self.assertIn('has not shown the last model', self.settings(self.payload('claude-haiku-5-5', 'high'), success=False))
        message = dict(request_id=str(uuid.uuid4()), text='hello', attachments=[], expected_run_id=self.run_id,
                       expected_conversation_id=self.conversation_id)
        result = subprocess.run([str(test_input.HGS), 'send', self.name, '--json'], env=self.env,
                                input=json.dumps(message), text=True, capture_output=True, timeout=10)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn('has not shown the last model', result.stderr)
        self.assertEqual(self.received(), b'/model')

    def test_native_event_releases_the_guard_but_never_replays_the_failed_request(self):
        (self.root / 'freeze').touch()
        payload = self.payload('claude-sonnet-5-5', 'high')
        self.assertIn('not confirmed', self.settings(payload, success=False))
        before = self.received()
        self.assertEqual(before, b'/model')
        event = dict(hook_event_name='Stop', session_id=self.conversation_id,
                     transcript_path=str(self.transcript))
        # A foreign run's hook cannot settle this run's uncertain keys.
        for run in ('old-run', self.run_id):
            import hook_caller_fixtures
            result = hook_caller_fixtures.run(self, event,
                dict(self.env, HGS_SESSION=self.name, HGS_RUN_ID=run))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(self.received(), before)
            if run == 'old-run':
                self.assertIn('has not shown the last model', self.settings(self.payload('claude-haiku-5-5', 'high'), success=False))
        record = json.loads(self.record_path.read_text())
        self.assertGreater(record['last_event_at'], record['settings_unconfirmed_at'])
        self.assertFalse(record.get('input_pending_at'))
        (self.root / 'freeze').unlink()
        self.assertIn('already', self.settings(payload, success=False))
        self.assertEqual(self.received(), before)
        self.assertEqual(self.settings(self.payload('claude-sonnet-5-5', 'high'))['status'], 'applied')
        self.assertEqual(self.inspect()['model'], 'claude-sonnet-5-5')
        self.assertDefaultsUntouched()

    def test_draft_and_foreign_transcript_send_no_input(self):
        self.record['transcript'] = str(self.root / 'another-conversation.jsonl'); self.write_record()
        self.assertFalse(self.inspect()['settings_change_supported'])
        self.settings(self.payload('claude-sonnet-5-5', 'high'), success=False)
        self.record['transcript'] = str(self.transcript); self.write_record()
        (self.root / 'screen').write_text('\x1b[2J\x1b[24;1H' + '─' * 100 + '\r\n❯ keep my draft\r\n' + '─' * 100)
        os.kill(self.record['pid'], signal.SIGUSR1)
        self.wait_for(lambda: 'keep my draft' in self.tmux('capture-pane', '-p', '-t', self.pane))
        self.settings(self.payload('claude-sonnet-5-5', 'high'), success=False)
        self.assertEqual(self.received(), b'')

    def test_busy_session_saves_the_choice_until_ready(self):
        self.record.update(activity='busy', phase='working'); self.write_record()
        result = self.settings(self.payload('claude-haiku-5-5', 'xhigh'))
        self.assertEqual(result['apply_when'], 'ready')
        self.assertEqual(self.received(), b'')
        info = self.inspect()
        self.assertEqual((info['pending_model'], info['pending_effort']), ('claude-haiku-5-5', 'xhigh'))

    # Task #7: the owner's stuck lead (2026-11-10). A change the agent did not
    # visibly take leaves `settings_unconfirmed_at`; it now clears from the
    # agent's own evidence, and an unknown state offers an action instead of
    # sending the owner to Terminal.

    def message(self, text='hello'):
        payload = dict(request_id=str(uuid.uuid4()), text=text, attachments=[], expected_run_id=self.run_id,
                       expected_conversation_id=self.conversation_id)
        return subprocess.run([str(test_input.HGS), 'send', self.name, '--json'], env=self.env,
                              input=json.dumps(payload), text=True, capture_output=True, timeout=10)

    def check(self, *flags, success=True):
        result = subprocess.run([str(test_input.HGS), 'settings', self.name, '--check', *flags, '--json'], env=self.env,
                                text=True, capture_output=True, timeout=10)
        if not success:
            self.assertNotEqual(result.returncode, 0, result.stdout)
            return result.stderr
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def redraw(self, screen, shown):
        (self.root / 'screen').write_text(screen)
        os.kill(self.record['pid'], signal.SIGUSR1)
        self.wait_for(lambda: shown in self.tmux('capture-pane', '-p', '-t', self.pane))

    def prompt_screen(self, effort):
        glyphs = dict(low='○', medium='◐', high='●', xhigh='◉', max='◈')
        indicator = glyphs[effort] + ' ' + effort + ' · /effort'
        rule = '─' * 100
        return ('\x1b[2J\x1b[1;1H● Hi!\x1b[22;1H\r\n' + ' ' * (99 - len(indicator)) + indicator + '\r\n' + rule
                + '\r\n❯ \r\n' + rule + '\r\n  ⏸ manual mode on\x1b[25;3H')

    def stuck(self):
        (self.root / 'freeze').touch()
        self.assertIn('not confirmed', self.settings(self.payload('claude-sonnet-5-5', 'high'), success=False))
        record = json.loads(self.record_path.read_text())
        self.assertEqual((record['settings_unconfirmed_model'], record['settings_unconfirmed_effort']),
                         ('claude-sonnet-5-5', 'high'))
        result = self.message()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn('has not shown the last model', result.stderr)
        self.assertIn('Check again', result.stderr)
        self.assertEqual(self.received(), b'/model')
        (self.root / 'freeze').unlink()

    def test_unconfirmed_change_clears_when_the_agent_shows_the_requested_settings(self):
        self.stuck()
        # The agent took the change: its transcript has Claude's receipt and its
        # prompt is back, empty, with the new effort in the status line.
        with self.transcript.open('a') as f:
            f.write(json.dumps(dict(type='user', sessionId=self.conversation_id, isSidechain=False, message=dict(
                role='user', content='<local-command-stdout>Set model to `Sonnet 5.5` for this session only with `high` effort'
                                     '</local-command-stdout>'))) + '\n')
        self.redraw(self.prompt_screen('high'), '● high · /effort')
        shown = self.inspect()['settings_confirmation']
        self.assertEqual((shown['state'], shown['shown_model'], shown['shown_effort'], shown['at_prompt']),
                         ('confirmed', 'claude-sonnet-5-5', 'high', True))
        # Check again settles it in the record; an ordinary message goes through.
        self.assertEqual(self.check()['state'], 'confirmed')
        record = json.loads(self.record_path.read_text())
        self.assertNotIn('settings_unconfirmed_at', record)
        self.assertEqual((record['model'], record['effort']), ('claude-sonnet-5-5', 'high'))
        self.assertIsNone(self.inspect()['settings_confirmation'])
        self.assertEqual(self.check()['state'], 'none')
        result = self.message()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.wait_for(lambda: self.received().endswith(b'hello\r'))

    def test_a_send_checks_the_evidence_again_without_check(self):
        self.stuck()
        with self.transcript.open('a') as f:
            f.write(json.dumps(dict(type='user', sessionId=self.conversation_id, isSidechain=False, message=dict(
                role='user', content='<local-command-stdout>Set model to `Sonnet 5.5` for this session only with `high` effort'
                                     '</local-command-stdout>'))) + '\n')
        self.redraw(self.prompt_screen('high'), '● high · /effort')
        result = self.message()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.wait_for(lambda: self.received().endswith(b'hello\r'))

    def test_unknown_state_explains_and_continues_only_after_confirmation_at_an_empty_prompt(self):
        self.stuck()
        # The keys were lost: the prompt is back, empty, with the old settings.
        self.redraw(self.prompt_screen('medium'), '◐ medium · /effort')
        shown = self.inspect()['settings_confirmation']
        self.assertEqual((shown['state'], shown['requested_model'], shown['requested_effort'], shown['at_prompt']),
                         ('unknown', 'claude-sonnet-5-5', 'high', True))
        self.assertIn('not the requested claude-sonnet-5-5', shown['reason'])
        self.assertEqual(self.check()['state'], 'unknown')
        self.assertIn('settings_unconfirmed_at', json.loads(self.record_path.read_text()))
        self.assertNotEqual(self.message().returncode, 0)
        self.assertEqual(self.received(), b'/model')
        # Continue anyway: the person accepts what the agent shows at its empty prompt.
        self.assertEqual(self.check('--accept')['state'], 'accepted')
        record = json.loads(self.record_path.read_text())
        self.assertNotIn('settings_unconfirmed_at', record)
        self.assertEqual((record['model'], record['effort']), ('claude-opus-5-5', 'medium'))
        self.assertEqual(self.received(), b'/model')
        result = self.message()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.wait_for(lambda: self.received().endswith(b'hello\r'))

    def test_a_confirmed_send_never_types_into_a_visible_dialog(self):
        self.stuck()
        self.redraw('\x1b[2J\x1b[12;1H' + '▔' * 100 + '\r\n   Switch model?\r\n   Your next response will be slower and use more tokens'
                    '\r\n   This conversation is cached for the current model.\r\n   ❯ 1. Yes, switch to Sonnet 5.5\r\n     2. No, go back',
                    'Switch model?')
        shown = self.inspect()['settings_confirmation']
        self.assertEqual((shown['state'], shown['at_prompt']), ('unknown', False))
        self.assertEqual(self.check()['state'], 'unknown')
        self.assertIn('Nothing was changed', self.check('--accept', success=False))
        self.assertIn('settings_unconfirmed_at', json.loads(self.record_path.read_text()))
        self.assertNotEqual(self.message().returncode, 0)
        self.assertEqual(self.received(), b'/model')

    def test_pending_choice_types_nothing_into_a_pane_too_short_for_the_composer(self):
        # zerus-szz00: a choice saved while Claude works applies before the
        # next message. In a pane whose last row is Claude's prompt, with its
        # closing rule cut off, the send gate refuses before any settings key.
        self.record.update(activity='busy', phase='working'); self.write_record()
        self.settings(self.payload('claude-haiku-5-5', 'xhigh'))
        self.record = json.loads(self.record_path.read_text())
        self.record.update(activity='idle', phase='idle'); self.write_record()
        indicator = '◐ medium · /effort'
        (self.root / 'screen').write_text('\x1b[2J\x1b[28;1H' + ' ' * (99 - len(indicator)) + indicator
                                          + '\r\n' + '─' * 100 + '\r\n❯ ')
        os.kill(self.record['pid'], signal.SIGUSR1)
        self.wait_for(lambda: self.tmux('capture-pane', '-p', '-t', self.pane).rstrip('\n').split('\n')[-1].strip() == '❯')
        message = dict(request_id=str(uuid.uuid4()), expected_run_id=self.run_id,
                       expected_conversation_id=self.conversation_id, text='continue')
        result = subprocess.run([str(test_input.HGS), 'send', self.name, '--json'], env=self.env,
                                input=json.dumps(message), text=True, capture_output=True, timeout=30)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("Claude's pane is too short to show its whole composer; nothing was sent", result.stderr)
        self.assertEqual(self.received(), b'')
        info = self.inspect()
        self.assertEqual((info['pending_model'], info['pending_effort']), ('claude-haiku-5-5', 'xhigh'))
        self.assertDefaultsUntouched()


class ClaudeSettings327(ClaudeSettings):
    """The same contract against the reworded picker of Claude Code 2.1.327."""

    def setUp(self):
        super().setUp()
        (self.root / 'layout-2.1.327').write_text('')


if __name__ == '__main__':
    unittest.main()
