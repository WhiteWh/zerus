"""Busy native queue contracts: private homes, own tools and localhost providers."""
import http.server
import json
import os
import re
from pathlib import Path
import shlex
import threading
import time
import unittest

import test_codex_input_native as codex
import test_codex_async_questions_native as responses
import test_clear_context_native as clear
import test_claude_input_native as claude

PROMPT = 'Start the private busy queue fixture.'
FIRST = 'First queued fixture message.'
SECOND = 'Second queued fixture message.'
LAST = 'Exact hgs queued follow-up.'


def command(root):
    # Only this fixture's tool runs here. A finite escape also bounds failure
    # cleanup; normal completion is released explicitly by the original test.
    return (f'touch {shlex.quote(str(root / "tool-started"))}; '
            f'for i in $(seq 1 600); do [ -f {shlex.quote(str(root / "tool-release"))} ] '
            '&& break; sleep .1; done; printf "private tool finished\\n"')


def terminal_send(case, text):
    case.tmux('send-keys', '-t', case.pane, '-l', text)
    case.wait_for(lambda: text in case.screen())
    # Native paste detection must settle before the deliberate Enter.
    time.sleep(.2)
    case.tmux('send-keys', '-t', case.pane, 'Enter')


class BusyProvider(responses.Provider):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or b'{}')
        self.server.requests.append(body)
        main = any(i.get('role') == 'user' and any(p.get('text') == PROMPT for p in i.get('content', []) if isinstance(p, dict)) for i in body.get('input', []))
        if main and not self.server.called:
            self.server.called = True
            items = body.get('input') or []
            tools = responses.offered((body.get('tools') or []) + [t for i in items
                                      if i.get('type') == 'additional_tools' for t in i.get('tools', [])])
            args = dict(cmd=self.server.command, yield_time_ms=10000, max_output_tokens=1000)
            # Invoke Codex's native shell subtool directly. The compatibility
            # runner intentionally extracts no separate code-mode host; the
            # actual file marker proves this native tool ran, not a simulated
            # Working screen or a fabricated hook.
            assert 'exec_command' in tools or 'exec' in tools, tools
            item = dict(type='function_call', id='fc_busy', call_id='call_busy', name='exec_command',
                        arguments=json.dumps(args), status='completed')
        else:
            item = dict(type='message', id='msg_busy', role='assistant', status='completed',
                        content=[dict(type='output_text', text='Busy fixture completed.', annotations=[])])
        response = dict(id='resp_busy', object='response', status='completed', output=[item],
                        usage=dict(input_tokens=1, output_tokens=5, total_tokens=6))
        try:
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            self.emit('response.created', response=dict(response, status='in_progress', output=[]))
            self.emit('response.output_item.added', output_index=0, item=dict(item, status='in_progress'))
            self.emit('response.output_item.done', output_index=0, item=item)
            self.emit('response.completed', response=response)
        except OSError:
            pass  # Only this private native child can abandon the response.


@unittest.skipUnless(os.environ.get('HGS_CODEX_TEST_BIN'), 'Set HGS_CODEX_TEST_BIN for the private busy Codex contract')
class NativeBusyCodex(codex.NativeCodexInput):
    # Compatibility CI extracts the CLI, not the separate code-mode host.
    native_features = 'mentions_v2=false\ncode_mode_host=false\n'
    # Do not inherit the ordinary input test methods into this separate suite.
    test_leading_absolute_path_is_literal_message_text = None
    test_literal_at_mentions_reach_the_model_once_without_file_completion = None

    def setUp(self):
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), BusyProvider)
        self.server.requests, self.server.called = [], False
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.provider_url = f'http://127.0.0.1:{self.server.server_port}/v1'
        clear.NativeCodexClear.setUp(self)
        self.server.command = command(self.root)

    def test_busy_tool_queue_preserves_drafts_and_accepts_exactly_once(self):
        release = self.root / 'tool-release'
        try:
            terminal_send(self, PROMPT)
            self.wait_for((self.root / 'tool-started').exists)
            # This fixture binds the observed native tool to its own tracked
            # record; it does not synthesize any acknowledgement for hgs input.
            self.record.update(activity='busy', phase='tool', active_tools={'call_busy': {'tool_name': 'Bash'}})
            self.write_record()
            terminal_send(self, FIRST)
            terminal_send(self, SECOND)
            self.wait_for(lambda: SECOND in self.screen() and 'Messages to be submitted' in self.screen())
            draft = 'keep this user draft'
            self.tmux('send-keys', '-t', self.pane, '-l', draft)
            self.wait_for(lambda: '› ' + draft in self.screen())
            refused = self.payload(text='must not be typed')
            self.assertIn('agent busy:', self.send(refused, success=False))
            self.assertIn('› ' + draft, self.screen())
            self.tmux('send-keys', '-t', self.pane, 'C-a', 'C-k')
            self.wait_for(lambda: '› ' + draft not in self.screen())
            payload = self.payload(text=LAST)
            receipt = self.send(payload)
            self.assertEqual(receipt['status'], 'submitted')
            self.assertEqual(self.send(payload), receipt)
            release.touch()
            def messages():
                return [r['payload']['message'] for r in map(json.loads, Path(self.record['transcript']).read_text().splitlines())
                        if r.get('type') == 'event_msg' and r.get('payload', {}).get('type') == 'user_message']
            self.wait_for(lambda: LAST in messages())
            self.assertEqual(messages(), ['Previous fixture request', PROMPT, FIRST, SECOND, LAST])
            self.wait_for(lambda: 'Busy fixture completed.' in self.screen())
        finally:
            release.touch()


@unittest.skipUnless(claude.native.CLAUDE, 'Set HGS_CLAUDE_TEST_BIN for the private busy Claude contract')
class NativeBusyClaude(claude.NativeClaudeInput):
    native_permission_mode = 'bypassPermissions'
    test_trailing_backslash_starts_one_turn_with_the_exact_message = None
    test_multiline_message_ending_in_backslashes_is_preserved = None
    test_leading_absolute_path_is_literal_message_text = None

    def queued_messages(self):
        # Claude steers queued inputs into the original Bash tool result, not
        # standalone native user turns. Inspect that exact model-bound envelope.
        for request in reversed(self.native_requests):
            results = [block.get('content', '') for message in request.get('messages', [])
                       if isinstance(message.get('content'), list) for block in message['content']
                       if block.get('type') == 'tool_result' and block.get('tool_use_id') == 'toolu_busy']
            if results:
                return re.findall(r'The user sent a new message while you were working:\n(.*?)\n\nThis is how Claude Code',
                                  '\n'.join(results), flags=re.S)
        return []

    def native_response_blocks(self, body):
        if getattr(self, 'busy_ready', False) and not getattr(self, 'tool_sent', False):
            self.tool_sent = True
            return [dict(type='tool_use', id='toolu_busy', name='Bash',
                         input=dict(command=command(self.root), description='Wait for the private queue fixture'))]
        return [dict(type='text', text='Fixture complete.')]

    def test_busy_tool_queue_preserves_drafts_and_accepts_exactly_once(self):
        release = self.root / 'tool-release'
        self.busy_ready = True
        try:
            terminal_send(self, PROMPT)
            self.wait_for((self.root / 'tool-started').exists)
            self.record.update(activity='busy', phase='tool', active_tools={'toolu_busy': {'tool_name': 'Bash'}})
            self.write_record()
            terminal_send(self, FIRST)
            terminal_send(self, SECOND)
            self.wait_for(lambda: 'Press up to edit queued messages' in self.screen())
            draft = 'keep this user draft'
            self.tmux('send-keys', '-t', self.pane, '-l', draft)
            self.wait_for(lambda: draft in self.screen())
            self.assertIn('agent busy:', self.send(self.payload(text='must not be typed'), success=False))
            self.assertIn(draft, self.screen())
            self.tmux('send-keys', '-t', self.pane, 'C-u')
            self.wait_for(lambda: draft not in self.screen())
            payload = self.payload(text=LAST)
            receipt = self.send(payload)
            self.assertEqual(receipt['status'], 'submitted')
            self.assertEqual(self.send(payload), receipt)
            release.touch()
            self.wait_for(lambda: LAST in self.queued_messages())
            self.assertEqual(self.queued_messages(), [FIRST, SECOND, LAST])
            self.assertEqual(self.accepted(PROMPT), 1)
            self.assertEqual(self.accepted('must not be typed'), 0)
            self.assertEqual(self.accepted(draft), 0)
            # A later ordinary turn exposes any duplicate left in the queue.
            control = 'After the private busy tool.'
            self.wait_for(lambda: 'Press up to edit queued messages' not in self.screen())
            terminal_send(self, control)
            self.wait_for(lambda: self.accepted(control))
            self.assertEqual(self.queued_messages(), [FIRST, SECOND, LAST])
        finally:
            release.touch()

    def test_busy_tool_queue_80x24(self):
        self.test_busy_tool_queue_preserves_drafts_and_accepts_exactly_once()
