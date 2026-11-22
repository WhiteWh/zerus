"""Bootstrap receipts from the real send writer and private native-like hooks.

The C fixture owns its actual PID, rollout descriptor and direct hook child.
Launch-record metadata and native-provider JSONL rows are explicit synthetic
seams, never owner state or a claim about a released Codex/model execution.
"""
from datetime import datetime, timezone
import json
import unittest
import uuid

import test_codex_clear_recovery as recovery


class CodexFirstInput(unittest.TestCase):
    setUp = recovery.CodexClearRecovery.setUp
    tearDown = recovery.CodexClearRecovery.tearDown
    script = recovery.CodexClearRecovery.script
    write_record = recovery.CodexClearRecovery.write_record
    tmux = recovery.CodexClearRecovery.tmux
    wait_for = recovery.CodexClearRecovery.wait_for
    received = recovery.CodexClearRecovery.received
    payload = recovery.CodexClearRecovery.payload
    send = recovery.CodexClearRecovery.send
    hook = recovery.CodexClearRecovery.hook
    inspect = recovery.CodexClearRecovery.inspect
    write_header = recovery.CodexClearRecovery.write_header
    emit = recovery.CodexClearRecovery.emit

    def bootstrap(self, bind=True):
        # Use the existing kernel/caller fixture with a fresh empty composer.
        original = recovery.clear.RESUMED_CODEX
        recovery.clear.RESUMED_CODEX = original.replace(
            'static int cleared;', 'static int cleared = 1;', 1)
        try:
            recovery.CodexClearRecovery.poisoned(self)
        finally:
            recovery.clear.RESUMED_CODEX = original
        self.main = self.account / 'sessions' / ('rollout-' + self.conversation_id + '.jsonl')
        self.rollout.rename(self.main)  # Same inode held by the tracked owner.
        # Native-provider header seam; receipt and hook journals are real writers.
        self.main.write_text(json.dumps(dict(type='session_meta', payload=dict(
            id=self.conversation_id, source='cli', cwd=str(self.root),
            timestamp=datetime.now(timezone.utc).isoformat(), cli_version='0.169.0'))) + '\n')
        for key in ('expected_id', 'error', 'session_end', 'clear_context_request',
                    'input_pending_at', 'input_pending_request', 'input_seen_at',
                    'codex_binding_recovery'):
            self.record.pop(key, None)
        self.record.update(startup_kind='new', conversation_id='', transcript=None,
                           activity='unknown', phase='unknown', last_event_at=0,
                           active_tools={}, subagents={})
        self.write_record()  # Explicit launch-record metadata seam.
        self.receipt = self.send(self.payload(expected_conversation_id='',
                                              text='Synthetic exact first prompt'))
        self.assertTrue(self.receipt['first_message'])
        self.assertEqual(self.receipt['conversation_id'], '')
        self.assertEqual(self.receipt['transport'], 'terminal')
        self.wait_for(lambda: self.receipt['submitted_text'].encode() in self.received())
        self.receipt_path = self.state / 'input_receipts' / (self.receipt['request_id'] + '.json')
        self.original_receipt = self.receipt_path.read_bytes()
        self.record = json.loads(self.record_path.read_text())
        self.assertEqual(self.record['input_seen_at'], 0)
        self.start = dict(hook_event_name='SessionStart', source='startup',
                          session_id=self.conversation_id, transcript_path=str(self.main),
                          cwd=str(self.root))
        if bind:
            self.record = self.emit(self.start)
            self.assertEqual(self.record['conversation_id'], self.conversation_id)
            self.assertEqual(self.record['input_seen_at'], 0)
        return self.record

    def prompt(self, kind='UserPromptSubmit', text=None):
        return dict(self.start, hook_event_name=kind,
                    prompt=self.receipt['submitted_text'] if text is None else text)

    def unchanged_receipt(self):
        self.assertEqual(self.receipt_path.read_bytes(), self.original_receipt)

    def test_first_send_then_own_start_then_exact_prompt_confirms(self):
        self.bootstrap()
        current = self.emit(self.prompt())
        self.assertGreater(current['input_seen_at'], self.receipt['submitted_at'])
        self.unchanged_receipt()

    def test_unregistered_codex_hook_kinds_do_not_confirm_input(self):
        self.bootstrap()
        # These generic acceptance kinds are not registered Codex hook events.
        # Exercise the real route without expanding its native event contract.
        for kind in ('UserPromptQueued', 'TurnStarted', 'TaskStarted'):
            with self.subTest(kind=kind):
                self.assertEqual(self.emit(self.prompt(kind))['input_seen_at'], 0)
        self.unchanged_receipt()

    def test_different_prompt_stays_unconfirmed(self):
        self.bootstrap()
        for text in ('A different native prompt', '',
                     self.receipt['submitted_text'] + ' '):
            with self.subTest(text=text):
                self.assertEqual(self.emit(self.prompt(text=text))['input_seen_at'], 0)
        self.unchanged_receipt()

    def test_nonempty_foreign_receipt_is_not_a_bootstrap_exception(self):
        self.bootstrap()
        # Explicit receipt-scope corruption control, seeded by the real writer.
        for first_message in (False, True):
            with self.subTest(first_message=first_message):
                foreign = dict(self.receipt, conversation_id=str(uuid.uuid4()),
                               first_message=first_message)
                self.receipt_path.write_text(json.dumps(foreign))
                self.assertEqual(self.emit(self.prompt())['input_seen_at'], 0)
                self.native_user_row()
                self.assertEqual(self.reconciled_record()['input_seen_at'], 0)
                self.assertEqual(json.loads(self.receipt_path.read_text()), foreign)

    def test_empty_conversation_without_first_message_flag_cannot_confirm(self):
        self.bootstrap()
        ordinary = dict(self.receipt, first_message=False)
        self.receipt_path.write_text(json.dumps(ordinary))
        self.assertEqual(self.emit(self.prompt())['input_seen_at'], 0)
        self.native_user_row()
        self.assertEqual(self.reconciled_record()['input_seen_at'], 0)
        self.assertEqual(json.loads(self.receipt_path.read_text()), ordinary)

    def test_queue_transport_cannot_borrow_a_terminal_bootstrap_binding(self):
        self.bootstrap()
        queued = dict(self.receipt, transport='codex_queue',
                      native_queued_message_id=str(uuid.uuid4()))
        self.receipt_path.write_text(json.dumps(queued))
        self.assertEqual(self.emit(self.prompt())['input_seen_at'], 0)
        self.native_user_row()
        self.assertEqual(self.reconciled_record()['input_seen_at'], 0)
        self.assertEqual(json.loads(self.receipt_path.read_text()), queued)

    def test_recovered_binding_cannot_use_bootstrap_hook(self):
        self.bootstrap()
        self.record['codex_binding_recovery'] = dict(source='original_native_prompt',
            run_id=self.run_id, to=self.conversation_id, **{'from': str(uuid.uuid4())},
            pending_request=self.receipt['request_id'], pending_at=self.receipt['submitted_at'])
        self.write_record()  # Explicit recovered-binding fault seam.
        current = self.emit(self.prompt())
        self.assertEqual(current['input_seen_at'], 0)
        self.unchanged_receipt()

    def bound_without_start(self):
        self.bootstrap(bind=False)
        # Metadata alone is not the product-written main SessionStart journal.
        self.record.update(conversation_id=self.conversation_id,
                           conversation_state='active', transcript=str(self.main))
        self.write_record()

    def test_hook_needs_this_runs_product_written_main_start(self):
        self.bound_without_start()
        self.assertEqual(self.emit(self.prompt())['input_seen_at'], 0)
        self.unchanged_receipt()

    def test_background_journal_cannot_stand_in_for_main_start(self):
        self.bound_without_start()
        self.emit(dict(self.start, agent_id='native-child'))
        self.assertEqual(self.emit(self.prompt())['input_seen_at'], 0)
        self.native_user_row()
        self.assertEqual(self.reconciled_record()['input_seen_at'], 0)
        self.unchanged_receipt()

    def test_reconcile_needs_this_runs_product_written_main_start(self):
        self.bound_without_start()
        self.native_user_row()
        self.assertEqual(self.reconciled_record()['input_seen_at'], 0)
        self.unchanged_receipt()

    def test_recovered_binding_without_pending_marker_is_not_bootstrap(self):
        self.bootstrap()
        self.record['codex_binding_recovery'] = dict(run_id=self.run_id,
            to=self.conversation_id, **{'from': str(uuid.uuid4())})
        self.write_record()  # Explicit recovered-binding fault seam.
        self.assertEqual(self.emit(self.prompt())['input_seen_at'], 0)
        self.native_user_row()
        self.assertEqual(self.reconciled_record()['input_seen_at'], 0)
        self.unchanged_receipt()

    def test_background_start_and_prompt_cannot_confirm_bootstrap(self):
        self.bootstrap(bind=False)
        child = dict(self.start, session_id=self.successor, agent_id='native-child')
        current = self.emit(child)
        self.assertFalse(current['conversation_id'])
        current = self.emit(dict(child, hook_event_name='UserPromptSubmit',
                                 prompt=self.receipt['submitted_text']))
        self.assertEqual(current['input_seen_at'], 0)
        self.unchanged_receipt()

    def test_codex_native_task_start_never_substitutes_for_main_acceptance(self):
        self.bootstrap()
        creating = dict(self.start, hook_event_name='PreToolUse',
                        tool_name='mcp__codex_tui__create_thread', tool_use_id='bootstrap-child')
        self.emit(creating)
        self.emit(dict(creating, hook_event_name='PostToolUse', tool_response=dict(
            isError=False, content=[dict(type='text', text=json.dumps(dict(threadId=self.successor)))])))
        child = dict(self.start, session_id=self.successor, transcript_path=str(self.transcript))
        current = self.emit(child)
        self.assertEqual(current['conversation_id'], self.conversation_id)
        self.assertEqual(current['input_seen_at'], 0)
        current = self.emit(dict(child, hook_event_name='UserPromptSubmit',
                                 prompt=self.receipt['submitted_text']))
        self.assertEqual(current['input_seen_at'], 0)
        self.unchanged_receipt()

    def test_stop_without_consumed_user_row_never_confirms_bootstrap(self):
        self.bootstrap()
        current = self.emit(dict(self.start, hook_event_name='Stop',
                                 last_assistant_message='Synthetic completion alone'))
        self.assertEqual(current['input_seen_at'], 0)
        self.assertEqual(self.reconciled_record()['input_seen_at'], 0)
        self.unchanged_receipt()

    def reconciled_record(self):
        self.inspect()  # Product inspection reconciles before projecting its view.
        return json.loads(self.record_path.read_text())

    def native_user_row(self, text=None):
        # Handwritten Codex-native transcript schema, not an hgs receipt/DB row.
        stamp = lambda: datetime.now(timezone.utc).isoformat()
        rows = [dict(type='event_msg', timestamp=stamp(),
                     payload=dict(type='task_started', turn_id='bootstrap-native-turn')),
                dict(type='response_item', timestamp=stamp(), payload=dict(type='message',
                     role='user', content=[dict(type='input_text', text=(
                         self.receipt['submitted_text'] if text is None else text))]))]
        with self.main.open('a') as output:
            for row in rows:
                output.write(json.dumps(row) + '\n')

    def test_reconcile_heals_missed_bootstrap_hook_from_exact_consumed_row(self):
        self.bootstrap()  # No UserPromptSubmit: input_seen_at remains zero.
        self.native_user_row()
        current = self.reconciled_record()
        self.assertGreater(current['input_seen_at'], self.receipt['submitted_at'])
        self.assertEqual(current['input_acceptance']['source'], 'native_user_input')
        self.assertEqual(current['input_acceptance']['request_id'], self.receipt['request_id'])
        self.assertEqual(current['input_acceptance']['receipt_conversation_id'], '')
        self.unchanged_receipt()

    def test_reconcile_bootstrap_rejects_a_recovered_binding(self):
        self.bootstrap()
        self.record['codex_binding_recovery'] = dict(source='original_native_prompt',
            run_id=self.run_id, to=self.conversation_id, **{'from': str(uuid.uuid4())},
            pending_request=self.receipt['request_id'], pending_at=self.receipt['submitted_at'])
        self.write_record()  # Explicit recovered-binding fault seam.
        self.native_user_row()
        self.assertEqual(self.reconciled_record()['input_seen_at'], 0)
        self.unchanged_receipt()

    def test_reconcile_refuses_a_different_consumed_row(self):
        self.bootstrap()
        self.native_user_row('Different consumed native input')
        self.assertEqual(self.reconciled_record()['input_seen_at'], 0)
        self.unchanged_receipt()


if __name__ == '__main__':
    unittest.main()
