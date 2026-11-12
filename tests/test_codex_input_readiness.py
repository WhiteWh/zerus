"""Portable scheduling controls for the unchanged real-Codex hook-review case."""
import json
from pathlib import Path
from types import SimpleNamespace
import subprocess
import tempfile
import unittest
from unittest import mock

import test_codex_input_native as native

CASE = 'test_resumed_and_restored_hook_review_blocks_input_until_the_person_answers'


def fixture(deliver=True, copies=1, composer_ready=True):
    class Fixture(native.NativeCodexStartup):
        def setUp(self):
            self.temp = tempfile.TemporaryDirectory()
            self.root = Path(self.temp.name)
            self.native_home = self.root / 'native'
            self.native_home.mkdir()
            (self.native_home / 'hooks.json').write_text(json.dumps(
                {'hooks': {'SessionStart': [{'hooks': [{'timeout': 1}]}]}}))
            (self.root / 'state/archives').mkdir(parents=True)
            self.env = {}
            self.server = SimpleNamespace(requests=[])
            self.record = dict(run_id='run-0', conversation_id='original-conversation', activity='idle')
            self.pending = None
            self.card = None
            self.timeout = 1
            self.generation = 0
            self.sends = []
            self.polls_after_send = []
            self.waits = []

        def tearDown(self):
            self.temp.cleanup()

        def binding(self, name):
            return dict(self.record)

        def launch(self, trust=False):
            self.name = 'codex/p/startup'
            self.card = self.question('initial')

        def question(self, fingerprint):
            return dict(source='codex_hooks_trust', question_id='hook-review',
                        question_hash=fingerprint, run_id=self.record['run_id'],
                        conversation_id=self.record['conversation_id'])

        def inspect(self):
            # The maintained case requires native readiness even when the
            # composer glyph is visible. Model that separately from delivery.
            return dict(pending_questions=[self.card] if self.card else [],
                        resume_message_can_send=composer_ready and self.card is None)

        def screen(self):
            return '› 1. Review hooks' if self.card else '› Native composer'

        def deliver_first(self):
            pass  # The fixture starts after a previously completed original turn.

        def pause_settled_original(self):
            self.assertIsNone(self.pending, 'The test paused before its request arrived')

        def hgs(self, command, *args):
            if command == 'archive':
                (self.root / 'state/archives/original.json').write_text(json.dumps(
                    dict(name=self.name, archive_id='original-archive')))
            elif command == 'resume':
                self.generation += 1
                self.record['run_id'] = 'run-' + str(self.generation)
                timeout = json.loads((self.native_home / 'hooks.json').read_text())['hooks']['SessionStart'][0]['hooks'][0]['timeout']
                if timeout != self.timeout:
                    self.timeout = timeout
                    self.card = self.question('changed-' + str(self.generation))
            else:
                raise AssertionError('Unexpected native operation: ' + command)
            return ''

        def command(self, argv, **kwargs):
            payload = json.loads(kwargs['input'])
            self.assertEqual(payload['expected_run_id'], self.record['run_id'])
            self.assertEqual(payload['expected_conversation_id'], self.record['conversation_id'])
            if argv[1] == 'answer':
                self.assertEqual(payload['expected_question_hash'], self.card['question_hash'])
                self.card = None
                return subprocess.CompletedProcess(argv, 0, '{}', '')
            self.assertEqual(argv[1], 'send')
            text = payload['text']
            if self.card:
                self.assertIn('MUST NOT ENTER', text)
                return subprocess.CompletedProcess(argv, 1, '', 'hook trust review')
            self.sends.append((text, payload['request_id']))
            if text == 'Synthetic unchanged-hook resume.':
                self.server.requests.append(dict(texts=[text]))
            else:
                self.pending = [text, 0]
            return subprocess.CompletedProcess(argv, 0, '{"status":"submitted"}', '')

        def until(self, label, predicate):
            self.waits.append(label)
            # The hook record stays idle while the original request is in
            # transit. Advance only this fixture scheduler, with no sleeps or
            # input; the real fixture keeps its existing 20-second deadline.
            for _ in range(20):
                if self.pending:
                    self.pending[1] += 1
                    if deliver and self.pending[1] == 3:
                        self.server.requests.extend(dict(texts=[self.pending[0]]) for _ in range(copies))
                        self.polls_after_send.append(self.pending[1])
                        self.pending = None
                value = predicate()
                if value:
                    return value
            self.fail('Fixture deadline: ' + label)

    return Fixture(CASE)


class HookReviewReadiness(unittest.TestCase):
    def exercise(self, case):
        case.setUp()
        self.addCleanup(case.tearDown)
        with mock.patch.object(native.subprocess, 'run', side_effect=case.command):
            getattr(case, CASE)()  # The actual maintained call site/assertions.

    def test_idle_before_the_exact_request_cannot_finish_resume_or_restore(self):
        case = fixture()
        self.exercise(case)
        self.assertEqual(case.polls_after_send, [3, 3])
        self.assertEqual([text for text, _ in case.sends], [
            'Synthetic unchanged-hook resume.',
            'Synthetic after resume hook review.',
            'Synthetic after restore hook review.'])
        self.assertEqual(len({request for _, request in case.sends}), 3)

    def test_a_request_that_never_arrives_still_fails_without_resending(self):
        case = fixture(deliver=False)
        with self.assertRaisesRegex(AssertionError, 'Fixture deadline: The original resumed conversation'):
            self.exercise(case)
        self.assertEqual(len(case.sends), 2)
        self.assertEqual(case.waits[-1], 'The original resumed conversation must complete its turn')
        self.assertEqual(case.pending, ['Synthetic after resume hook review.', 20])

    def test_an_unready_composer_still_blocks_before_any_send(self):
        case = fixture(composer_ready=False)
        with self.assertRaisesRegex(AssertionError, 'Fixture deadline: The unchanged resume must reach its native composer'):
            self.exercise(case)
        self.assertEqual(case.sends, [])
        self.assertIsNone(case.pending)
        self.assertEqual(case.waits[-1], 'The unchanged resume must reach its native composer')

    def test_two_deliveries_still_fail_the_original_exactly_once_assertion(self):
        case = fixture(copies=2)
        with self.assertRaisesRegex(AssertionError, '2 != 1'):
            self.exercise(case)
        self.assertEqual(len(case.sends), 2)
        self.assertEqual(case.waits[-1], 'The original resumed conversation must complete its turn')
        self.assertEqual(sum(row['texts'] == ['Synthetic after resume hook review.']
                             for row in case.server.requests), 2)


if __name__ == '__main__':
    unittest.main()
