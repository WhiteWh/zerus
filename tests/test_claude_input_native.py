"""Exact message acceptance by real Claude using a private home and localhost API."""
import re
import time
import unittest
import test_input as fixtures
import test_claude_questions_native as native


@unittest.skipUnless(native.CLAUDE, 'Set HGS_CLAUDE_TEST_BIN for isolated native message coverage')
class NativeClaudeInput(unittest.TestCase):
    message_fixture = True
    def setUp(self):
        native.NativeClaudeQuestions.setUp(self)
        dismissed = False
        def ready():
            nonlocal dismissed
            screen = self.screen()
            if not dismissed and 'Make auto mode your default permission mode?' in screen and 'No, keep' in screen:
                self.tmux('send-keys', '-t', self.pane, 'Down')
                self.wait_for(lambda: re.search(r'❯ No, keep', self.screen()))
                time.sleep(.2)
                self.tmux('send-keys', '-t', self.pane, 'Enter')
                dismissed = True
                return False
            return re.search(r'^❯\s*$', screen, re.M)
        self.wait_for(ready)
    script = fixtures.InputTransport.script
    tmux = fixtures.InputTransport.tmux
    write_record = fixtures.InputTransport.write_record
    wait_for = native.NativeClaudeQuestions.wait_for
    screen = native.NativeClaudeQuestions.screen
    send = fixtures.InputTransport.send
    payload = fixtures.InputTransport.payload

    def accepted(self, text):
        return sum(row.get('type') == 'user' and row.get('message', {}).get('content') == text
                   for row in native.records(self.record['transcript']))

    def test_trailing_backslash_starts_one_turn_with_the_exact_message(self):
        text = 'Continue the fixture\\'
        payload = self.payload(text=text)
        try:
            receipt = self.send(payload)
        except AssertionError as error:
            raise AssertionError(str(error) + '\n' + self.screen()) from error
        self.assertEqual(receipt['text'], text)
        self.wait_for(lambda: self.accepted(text))
        self.assertEqual(self.accepted(text), 1)
        self.assertEqual(self.send(payload), receipt)
        self.assertEqual(self.accepted(text), 1)

    def test_multiline_message_ending_in_backslashes_is_preserved(self):
        text = 'Literal paths:\nfixture\\\\'
        self.send(self.payload(text=text))
        self.wait_for(lambda: self.accepted(text))

    def test_leading_absolute_path_is_literal_message_text(self):
        # A command-named directory must not invoke /help or file completion.
        text = '/help/fixture/capture.png\nWhat is shown in this screenshot?'
        payload = self.payload(text=text)
        receipt = self.send(payload)
        self.assertEqual(receipt['text'], text)
        self.wait_for(lambda: self.accepted(text))
        self.assertEqual(self.accepted(text), 1)
        self.assertEqual(self.send(payload), receipt)
        self.assertEqual(self.accepted(text), 1)
