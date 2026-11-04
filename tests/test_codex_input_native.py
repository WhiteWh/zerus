#!/usr/bin/env python3
"""Literal Codex message delivery against a keyless localhost Responses provider."""
import http.server
import json
import os
from pathlib import Path
import threading
import unittest

import test_clear_context_native as native
import test_codex_async_questions_native as questions
import test_codex_update
import test_input


@unittest.skipUnless(os.environ.get('HGS_CODEX_TEST_BIN'), 'Set HGS_CODEX_TEST_BIN for isolated native Codex input')
class NativeCodexInput(unittest.TestCase):
    script = test_input.InputTransport.script
    write_record = test_input.InputTransport.write_record
    tmux = test_input.InputTransport.tmux
    tearDown = test_input.InputTransport.tearDown
    payload = test_input.InputTransport.payload
    send = test_input.InputTransport.send
    fail = native.NativeCodexClear.fail
    wait_for = test_codex_update.NativeCodexUpdate.wait_for
    native_features = 'mentions_v2=false\n'

    def setUp(self):
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), questions.Provider)
        self.server.requests, self.server.release, self.server.questions = [], threading.Event(), []
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.addCleanup(self.server.release.set)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.provider_url = f'http://127.0.0.1:{self.server.server_port}/v1'
        native.NativeCodexClear.setUp(self)
        (self.root / 'literalreviewer.log').write_text('Synthetic file-completion target.\n')

    def screen(self):
        return self.tmux('capture-pane', '-p', '-t', self.pane)

    def test_leading_absolute_path_is_literal_message_text(self):
        text = '/help/fixture/capture.png\nWhat is shown in this screenshot?'
        payload = self.payload(text=text)
        receipt = self.send(payload)
        self.assertEqual(receipt['submitted_text'], text)
        self.wait_for(lambda: any(request['texts'][-1:] == [text] for request in self.server.requests))
        self.wait_for(lambda: 'Fixture turn complete.' in self.screen())
        self.assertEqual(self.send(payload), receipt)
        rows = map(json.loads, Path(self.record['transcript']).read_text().splitlines())
        messages = [row['payload']['message'] for row in rows if row.get('type') == 'event_msg'
                    and row.get('payload', {}).get('type') == 'user_message']
        self.assertEqual(messages, ['Previous fixture request', text])

    def test_literal_at_mentions_reach_the_model_once_without_file_completion(self):
        # Positive control: the native completion menu really has a matching file.
        # Warm the exact query before hgs sends, so its fixed paste delay cannot
        # accidentally avoid the regression through a slow first file search.
        text = 'Send a fixture test to @literalreviewer'
        self.tmux('send-keys', '-t', self.pane, '-l', text)
        self.wait_for(lambda: 'literalreviewer.log' in self.screen())
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        self.wait_for(lambda: 'Send a fixture test to literalreviewer.log' in self.screen())
        self.assertEqual(self.server.requests, [], 'Completion must not submit a native turn')
        self.tmux('send-keys', '-t', self.pane, 'C-a', 'C-k')
        self.wait_for(lambda: 'Send a fixture test to' not in self.screen())

        payload = self.payload(text=text)
        receipt = self.send(payload)
        self.assertEqual(receipt['submitted_text'], text)
        self.wait_for(lambda: self.server.requests)
        self.assertEqual(self.server.requests[0]['texts'][-1], text)
        self.wait_for(lambda: 'Fixture turn complete.' in self.screen())
        self.assertEqual(self.send(payload), receipt)
        # A later native request exposes any replayed/queued duplicate too. The
        # reused fixture has no progress hooks, so send this control in Terminal
        # rather than fabricate an acknowledgement to unlock another hgs send.
        control = 'After the literal mention.'
        self.tmux('set-buffer', '-b', 'literal-control', control)
        self.tmux('paste-buffer', '-p', '-r', '-d', '-b', 'literal-control', '-t', self.pane)
        self.wait_for(lambda: control in self.screen())
        self.tmux('send-keys', '-t', self.pane, 'Enter')
        self.wait_for(lambda: any(request['texts'][-1:] == [control] for request in self.server.requests))
        # Codex may also ask this provider for an automatic task title. Check
        # typed user-message events in this exact native conversation, so
        # auxiliary model requests and injected environment context cannot
        # impersonate an accepted or duplicate turn.
        rows = map(json.loads, Path(self.record['transcript']).read_text().splitlines())
        messages = [row['payload']['message'] for row in rows if row.get('type') == 'event_msg'
                    and row.get('payload', {}).get('type') == 'user_message']
        self.assertEqual(messages, ['Previous fixture request', text, control])


class NativeCodexMentionsV2Input(NativeCodexInput):
    native_features = 'mentions_v2=true\n'
