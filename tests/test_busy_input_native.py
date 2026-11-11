"""Busy native queue contracts: private homes, own tools and localhost providers."""
import base64
import datetime
import http.server
import json
import struct
import zlib
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


def screenshot_png(seed, width, height):
    """A flat UI-like screenshot at a size Claude must downscale after the paste."""
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff)
    rows = []
    for y in range(height):
        shade = 30 + seed * 20 + ((y // 90) % 2) * 40
        rows.append(b'\x00' + bytes([shade, shade, shade + 10]) * width)
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(b''.join(rows), 6)) + chunk(b'IEND', b''))


IMAGES = {}


def image_message(root):
    """zerus-iitw8: the owner's shape. The text names screenshots on lines of their own,
    which Claude attaches asynchronously, plus two hgs attachments."""
    if not IMAGES:
        IMAGES.update(big=[screenshot_png(n, 6000, 4000) for n in range(2)], small=[screenshot_png(n, 64, 48) for n in range(2)])
    target = root / 'peer-target'
    target.mkdir(exist_ok=True)
    paths = [target / 'confirmation-dark.png', target / 'confirmation-light.png']
    for path, data in zip(paths, IMAGES['big']):
        path.write_bytes(data)
    body = '\n'.join(f'- Item {n}: the peer report explains one change of the fixture in a full sentence.' for n in range(1, 31))
    text = (f'<zerus-message from="codex/fixture/peer" host="arch">\nSettings preview delta is ready.\n{body}\n'
            f'Screenshots:\n{paths[0]}\n{paths[1]}\n</zerus-message>')
    attachments = [dict(name=f'row-{n}.png', mime='image/png', data_base64=base64.b64encode(data).decode())
                   for n, data in enumerate(IMAGES['small'])]
    return text, attachments


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


    def test_busy_attachment_send_is_queued_and_delivered_once(self):
        # zerus-iitw8: Codex shows a pasted image path or a long paste at once
        # (no asynchronous attachment as in Claude), so its single Enter and
        # submitted receipt stay; the message waits in its native queue.
        release = self.root / 'tool-release'
        try:
            terminal_send(self, PROMPT)
            self.wait_for((self.root / 'tool-started').exists)
            self.record.update(activity='busy', phase='tool', active_tools={'call_busy': {'tool_name': 'Bash'}})
            self.write_record()
            text, attachments = image_message(self.root)
            payload = self.payload(text=text, attachments=attachments)
            receipt = self.send(payload)
            self.assertEqual(receipt['status'], 'submitted')
            self.assertEqual(self.send(payload), receipt)
            self.wait_for(lambda: 'Messages to be submitted' in self.screen())
            release.touch()
            def messages():
                return [r['payload']['message'] for r in map(json.loads, Path(self.record['transcript']).read_text().splitlines())
                        if r.get('type') == 'event_msg' and r.get('payload', {}).get('type') == 'user_message']
            self.wait_for(lambda: receipt['submitted_text'] in messages())
            self.wait_for(lambda: 'Busy fixture completed.' in self.screen())
            self.assertEqual(messages().count(receipt['submitted_text']), 1)
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

    def enqueued(self, content):
        return [r for r in self.rows() if r.get('type') == 'queue-operation'
                and r.get('operation') == 'enqueue' and r.get('content') == content]

    def test_an_identical_earlier_queued_prompt_never_proves_a_new_send(self):
        # zerus-iitw8.4: an identical prompt already waits in Claude's queue.
        # Only this send's own new enqueue may prove it, and both arrive.
        release = self.root / 'tool-release'
        self.busy_ready = True
        try:
            terminal_send(self, PROMPT)
            self.wait_for((self.root / 'tool-started').exists)
            self.record.update(activity='busy', phase='tool', active_tools={'toolu_busy': {'tool_name': 'Bash'}})
            self.write_record()
            terminal_send(self, 'continue')
            self.wait_for(lambda: 'Press up to edit queued messages' in self.screen())
            # Claude writes rows late: the earlier enqueue may land after the
            # banner, even after this send's start. The stamps tell them apart.
            sent_at = time.time()
            receipt = self.send(self.payload(text='continue'))
            self.assertEqual((receipt['status'], receipt['native']), ('submitted', 'queue'), self.screen())
            def stamped(row):
                return datetime.datetime.fromisoformat(row['timestamp'].replace('Z', '+00:00')).timestamp()
            self.wait_for(lambda: len(self.enqueued('continue')) == 2)
            earlier, own = self.enqueued('continue')
            self.assertLess(stamped(earlier), sent_at)
            self.assertGreaterEqual(stamped(own), sent_at, 'the proof is this send\'s own enqueue')
            release.touch()
            self.wait_for(lambda: self.queued_messages().count('continue') == 2)
            self.assertEqual(self.queued_messages(), ['continue', 'continue'])
        finally:
            release.touch()

    def rows(self):
        return [json.loads(line) for line in Path(self.record['transcript']).read_text().splitlines()]

    def carrying(self, rows, marker='Settings preview delta is ready.'):
        def text(row):
            content = row.get('message', {}).get('content')
            if isinstance(content, list):
                return '\n'.join(b.get('text', '') for b in content if b.get('type') == 'text')
            return content if isinstance(content, str) else ''
        return [r for r in rows if r.get('type') == 'user' and marker in text(r)]

    def composer(self):
        lines = self.screen().splitlines()
        prompt = max(i for i, line in enumerate(lines) if line.startswith('❯'))
        return lines[prompt].strip()

    def test_pasted_images_settle_before_the_one_enter_when_idle(self):
        # zerus-iitw8: Claude converts the bare image paths after the paste and
        # swallows an Enter while it shows Pasting…; hgs must wait, then prove.
        text, attachments = image_message(self.root)
        payload = self.payload(text=text, attachments=attachments)
        receipt = self.send(payload)
        self.assertEqual((receipt['status'], receipt['native']), ('submitted', 'transcript'), self.screen())
        rows = self.carrying(self.rows())
        self.assertEqual(len(rows), 1)
        self.assertEqual(sum(b.get('type') == 'image' for b in rows[0]['message']['content']), 2)
        self.wait_for(lambda: self.composer() in ('❯', '❯\xa0'))
        self.assertEqual(self.send(payload), receipt)
        self.assertEqual(len(self.carrying(self.rows())), 1)

    def test_busy_pasted_images_are_queued_with_native_proof_and_delivered_once(self):
        release = self.root / 'tool-release'
        self.busy_ready = True
        try:
            terminal_send(self, PROMPT)
            self.wait_for((self.root / 'tool-started').exists)
            self.record.update(activity='busy', phase='tool', active_tools={'toolu_busy': {'tool_name': 'Bash'}})
            self.write_record()
            text, attachments = image_message(self.root)
            payload = self.payload(text=text, attachments=attachments)
            receipt = self.send(payload)
            self.assertEqual((receipt['status'], receipt['native']), ('submitted', 'queue'), self.screen())
            self.assertEqual(self.send(payload), receipt)
            release.touch()
            def delivered():
                rows = self.rows()
                queued = [r for r in rows if r.get('type') == 'attachment' and r.get('attachment', {}).get('type') == 'queued_command'
                          and 'Settings preview delta is ready.' in str(r['attachment'].get('prompt'))]
                return len(self.carrying(rows)) + len(queued)
            self.wait_for(delivered)
            enqueued = [r for r in self.rows() if r.get('type') == 'queue-operation' and r.get('operation') == 'enqueue'
                        and 'Settings preview delta is ready.' in r.get('content', '')]
            self.assertEqual(len(enqueued), 1)
            self.wait_for(lambda: 'Press up to edit queued messages' not in self.screen())
            self.assertEqual(delivered(), 1)
        finally:
            release.touch()


@unittest.skipUnless(claude.native.CLAUDE, 'Set HGS_CLAUDE_TEST_BIN for the private hooked Claude contract')
class NativeHookedImagesClaude(NativeBusyClaude):
    """The idle image send with hgs's own UserPromptSubmit hook, which takes the state lock:
    Claude runs it before recording the message, so the proof wait must not hold that lock."""
    test_busy_tool_queue_preserves_drafts_and_accepts_exactly_once = None
    test_busy_tool_queue_80x24 = None
    test_busy_pasted_images_are_queued_with_native_proof_and_delivered_once = None
    test_an_identical_earlier_queued_prompt_never_proves_a_new_send = None

    @property
    def native_env(self):
        return ('HGS_SESSION=' + self.name, 'HGS_RUN_ID=' + self.run_id, 'HGS_STATE_DIR=' + str(self.state),
                'HGS_CONFIG_DIR=' + str(self.root / 'config'))

    def native_settings(self, work):
        hook = shlex.quote(str(claude.fixtures.HGS)) + ' __state hook'
        return {'hooks': {'UserPromptSubmit': [{'hooks': [{'type': 'command', 'command': hook, 'timeout': 60}]}]}}

    def test_pasted_images_settle_before_the_one_enter_when_idle(self):
        started = time.monotonic()
        super().test_pasted_images_settle_before_the_one_enter_when_idle()
        record = json.loads(self.record_path.read_text())
        self.assertGreater(record['input_seen_at'], record['input_pending_at'], 'the hook recorded the acceptance')
        self.assertLess(time.monotonic() - started, 20)
