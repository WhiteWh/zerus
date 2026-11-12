"""Real Claude ghost predictions, private homes and a localhost-only provider."""
import json
import hashlib
import re
import subprocess
import unittest

import test_busy_input_native as busy
import test_claude_input_native as native

GHOST = 'A private fixture follow-up'
WARM = 'Prepare the private suggestion fixture.'
BODY = 'Send exactly this fixture message.'


@unittest.skipUnless(native.native.CLAUDE, 'Set HGS_CLAUDE_TEST_BIN for native prompt suggestion coverage')
class NativeClaudeSuggestions(native.NativeClaudeInput):
    test_trailing_backslash_starts_one_turn_with_the_exact_message = None
    test_multiline_message_ending_in_backslashes_is_preserved = None
    test_leading_absolute_path_is_literal_message_text = None
    native_permission_mode = 'bypassPermissions'
    native_env = (*native.NativeClaudeInput.native_env, 'CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION=true')
    # Suggestion generation requires a warm prompt cache. This is provider
    # usage metadata, not an injected screen or a fabricated acceptance event.
    native_response_usage = dict(input_tokens=100, output_tokens=30, cache_read_input_tokens=20000)
    queued_messages = busy.NativeBusyClaude.queued_messages

    def native_settings(self, work):
        settings = super().native_settings(work)
        settings['promptSuggestionEnabled'] = True
        return settings

    def native_response_blocks(self, body):
        if 'SUGGESTION MODE:' in json.dumps(body):
            self.suggestions = getattr(self, 'suggestions', 0) + 1
            return [dict(type='text', text=GHOST)]
        if getattr(self, 'busy_ready', False) and not getattr(self, 'tool_sent', False):
            self.tool_sent = True
            return [dict(type='tool_use', id='toolu_busy', name='Bash',
                         input=dict(command=busy.command(self.root), description='Wait for the private suggestion fixture'))]
        return [dict(type='text', text='Fixture complete.')]

    def ghost(self):
        # Assert the real native style and cursor, never rewrite capture-pane.
        x, y = map(int, self.tmux('display-message', '-p', '-t', self.pane, '#{cursor_x}:#{cursor_y}').split(':'))
        rows = self.tmux('capture-pane', '-p', '-e', '-t', self.pane).splitlines()
        return x == 2 and y < len(rows) and re.search(r'\x1b\[7mA\x1b\[0;2m private fixture follow-up\x1b\[0m', rows[y])

    def prepare_suggestion(self):
        busy.terminal_send(self, WARM)
        self.wait_for(self.ghost)
        self.assertGreater(self.suggestions, 0)
        self.assertEqual(self.accepted(GHOST), 0)
        # Logging forwards mutations to the original tmux; no screen injection.
        native.native.NativeClaudeQuestions.enable_gutter_capture(self, gutter=False)

    def assert_no_acceptance_keys(self):
        keys = [arg for row in (self.root / 'sent-keys').read_text().splitlines()
                for arg in json.loads(row)]
        self.assertFalse({'Tab', 'Right', 'C-f'} & set(keys), keys)
        self.assertEqual(self.accepted(GHOST), 0)

    def test_id_hidden_pending_settings_then_message_makes_one_model_call_and_keeps_defaults(self):
        # Settle the enabled native predictor before measuring this operation.
        busy.terminal_send(self, WARM)
        self.wait_for(self.ghost)
        self.assertGreater(self.suggestions, 0)
        receipt, attempts = self.pending_message()
        before, calls = attempts(), len(self.native_requests)
        request = dict(request_id=receipt['request_id'], expected_run_id=self.run_id,
                       expected_conversation_id=self.conversation_id,
                       text=receipt['text'])
        raw = '{\n ' + json.dumps(request, separators=(',', ':'))[1:-1] + '\n}\n'
        self.assertEqual(hashlib.sha256(raw.encode()).hexdigest(), receipt['digest'])
        replay = subprocess.run([str(native.fixtures.HGS), 'send', self.name, '--session-id',
                                 self.record['session_id'], '--json'], input=raw,
                                env=self.env, text=True, capture_output=True, timeout=30)
        self.assertEqual(replay.returncode, 0, replay.stdout + replay.stderr)
        self.assertEqual(json.loads(replay.stdout), receipt)
        self.assertEqual(attempts(), before)
        self.assertEqual(len(self.native_requests), calls)
        self.assertEqual(self.accepted(receipt['text']), 1)
        self.assertEqual(self.accepted(GHOST), 0)

    def assert_pending_requests(self, before, text, model):
        self.wait_for(self.ghost)
        offered = self.native_requests[before:]
        # Like Codex's explicit thread-title control, count native auxiliary
        # inference separately; never silently filter it from the total.
        self.assertEqual(len(offered), 2, offered)
        primary, suggestion = offered
        self.assertNotIn('SUGGESTION MODE:', json.dumps(primary))
        self.assertIn('SUGGESTION MODE:', json.dumps(suggestion))
        self.assertEqual([body['model'] for body in offered], [model, model])

        def messages(body):
            # The predictor uses string content and removes cache_control.
            # Compare every role and content block after only that encoding
            # normalization, retaining exact text and all other block fields.
            result = []
            for message in body['messages']:
                content = message['content']
                if isinstance(content, str):
                    content = [dict(type='text', text=content)]
                blocks = [{k: v for k, v in block.items() if k != 'cache_control'}
                          for block in content]
                result.append(dict(message, content=blocks))
            return result

        main, auxiliary = messages(primary), messages(suggestion)
        self.assertEqual(auxiliary[:len(main)], main)
        exact = lambda rows: sum(block.get('text') == text for message in rows
                                 if message['role'] == 'user' for block in message['content'])
        copies = lambda rows: sum(block.get('text', '').count(text) for message in rows
                                  for block in message['content'])
        self.assertEqual(exact(main), 1)
        self.assertEqual(copies(main), 1)
        self.assertEqual(copies(auxiliary), 1)
        self.assertTrue(all(copies(messages(body)) == 0 for body in self.native_requests[:before]))
        suffix = auxiliary[len(main):]
        users = [message for message in suffix if message['role'] == 'user']
        self.assertEqual(len(users), 1, suffix)
        self.assertEqual(len(users[0]['content']), 1, suffix)
        self.assertTrue(users[0]['content'][0]['text'].startswith('[SUGGESTION MODE:'))
        self.assertEqual(copies(suffix), 0)
        print('ZERUS_NATIVE_CLAUDE_CALLS=' + json.dumps(dict(
            case=self.id(), before=before, message_calls=1, suggestion_calls=1,
            total_calls=len(offered), retained_history_copies=copies(auxiliary))), flush=True)

    def test_idle_ghost_is_replaced_and_enter_never_accepts_it(self):
        self.prepare_suggestion()
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        self.wait_for(self.ghost)
        payload = self.payload(text=BODY)
        receipt = self.send(payload)
        self.assertEqual(self.send(payload), receipt)
        self.wait_for(lambda: self.accepted(BODY))
        self.assertEqual(self.accepted(BODY), 1)
        self.wait_for(self.ghost)
        # A later real turn exposes any suggestion/duplicate left in the queue.
        control = 'After the private suggestion input.'
        busy.terminal_send(self, control)
        self.wait_for(lambda: self.accepted(control))
        self.assertEqual(self.accepted(BODY), 1)
        self.assert_no_acceptance_keys()

    def test_idle_real_draft_at_start_is_preserved_with_suggestions_enabled(self):
        self.prepare_suggestion()
        draft = 'keep this private user draft'
        self.tmux('send-keys', '-t', self.pane, '-l', draft)
        self.wait_for(lambda: draft in self.screen())
        self.tmux('send-keys', '-t', self.pane, 'C-a')
        self.assertIn('terminal has a draft', self.send(self.payload(text=BODY), success=False))
        self.assertIn(draft, self.screen())
        self.assertEqual(self.accepted(BODY), 0)
        self.assertEqual(self.accepted(draft), 0)
        self.assert_no_acceptance_keys()

    def test_busy_queue_with_suggestions_enabled_submits_only_the_body(self):
        self.prepare_suggestion()
        self.busy_ready = True
        release = self.root / 'tool-release'
        try:
            busy.terminal_send(self, busy.PROMPT)
            self.wait_for((self.root / 'tool-started').exists)
            self.record.update(activity='busy', phase='tool', active_tools={'toolu_busy': {'tool_name': 'Bash'}})
            self.write_record()
            busy.terminal_send(self, busy.FIRST)
            self.wait_for(lambda: 'Press up to edit queued messages' in self.screen())
            draft = 'keep this private busy draft'
            self.tmux('send-keys', '-t', self.pane, '-l', draft)
            self.wait_for(lambda: draft in self.screen())
            self.assertIn('agent busy:', self.send(self.payload(text='must not be typed'), success=False))
            self.assertIn(draft, self.screen())
            self.tmux('send-keys', '-t', self.pane, 'C-u')
            self.wait_for(lambda: draft not in self.screen())
            self.tmux('send-keys', '-t', self.pane, 'Enter')
            payload = self.payload(text=BODY)
            receipt = self.send(payload)
            self.assertEqual(self.send(payload), receipt)
            release.touch()
            self.wait_for(lambda: BODY in self.queued_messages())
            self.assertEqual(self.queued_messages(), [busy.FIRST, BODY])
            self.wait_for(self.ghost)
            busy.terminal_send(self, 'After the private suggestion queue.')
            self.wait_for(lambda: self.accepted('After the private suggestion queue.'))
            self.assertEqual(self.queued_messages(), [busy.FIRST, BODY])
            self.assertEqual(self.accepted(draft), 0)
            self.assertEqual(self.accepted('must not be typed'), 0)
            self.assert_no_acceptance_keys()
        finally:
            release.touch()

    def test_idle_ghost_80x24(self):
        self.test_idle_ghost_is_replaced_and_enter_never_accepts_it()

    def test_busy_suggestions_80x24(self):
        self.test_busy_queue_with_suggestions_enabled_submits_only_the_body()
