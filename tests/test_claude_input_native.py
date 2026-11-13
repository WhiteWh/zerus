"""Exact message acceptance by real Claude using a private home and localhost API."""
import re
import time
import unittest
import json
import test_input as fixtures
import test_claude_questions_native as native


@unittest.skipUnless(native.CLAUDE, 'Set HGS_CLAUDE_TEST_BIN for isolated native message coverage')
class NativeClaudeInput(unittest.TestCase):
    message_fixture = True
    native_env = ('DISABLE_AUTOUPDATER=1',)
    def native_settings(self, work):
        if 'pending_settings_then_message' not in self._testMethodName:
            return {}
        import copy
        from claude_catalog_fixture import CatalogFixture
        from test_claude_settings_native import CATALOG, EFFORTS
        catalog = copy.deepcopy(CATALOG)
        catalog['catalog']['config']['models'].append(dict(
            id='claude-fable-5-2', name='Fable 5.2', short_name='Fable', section='overflow',
            thinking=dict(type='effort', effort_options=EFFORTS)))
        (self.root / '.claude/cache/model-catalog').mkdir(parents=True)
        self.catalog_fixture = CatalogFixture(self.root, catalog)
        self.addCleanup(self.catalog_fixture.close)
        self.catalog_fixture.seed(self.root)
        self.native_env = (*self.native_env, *[key + '=' + value for key, value in self.catalog_fixture.environment().items()])
        return dict(model='opus', effortLevel='medium')
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

    def test_id_hidden_pending_settings_then_message_makes_one_model_call_and_keeps_defaults(self):
        self.pending_message()

    def pending_message(self):
        import native_effort_id as identity
        defaults = self.root / '.claude/settings.json'
        before = defaults.read_bytes()
        requests = len(self.native_requests)
        record, path, _ = identity.moved_record(self, self.name, self.record, existing_machine=True)
        self.record, self.record_path = record, path
        attempts = identity.key_log(self)
        text = 'Continue after the pending session model choice\\'
        receipt = identity.pending_then_message(self, self.name, record, path,
                                                'claude-fable-5-2', 'xhigh', text, attempts)
        self.wait_for(lambda: self.accepted(text))
        self.assertEqual(self.accepted(text), 1)
        self.assert_pending_requests(requests, text, 'claude-fable-5-2')
        self.assertEqual(defaults.read_bytes(), before)
        self.assertNotIn('Select model', self.screen())
        self.assertEqual(json.loads(path.read_bytes())['model'], 'claude-fable-5-2')
        return receipt, attempts

    def assert_pending_requests(self, before, text, model):
        self.wait_for(lambda: len(self.native_requests) >= before + 1)
        self.assertEqual(len(self.native_requests), before + 1, self.native_requests[before:])
        self.assertEqual(self.native_requests[-1]['model'], model)
