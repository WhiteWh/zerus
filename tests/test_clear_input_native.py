"""Terminal's Clear input on real agents (zerus-wkj9l): private homes, localhost providers.

A residue left in the native composer (the owner's shape: a message naming
screenshots, pasted while hgs reads nothing) is read through `hgs terminal`'s
composer action and cleared only as the exact confirmed text. End and Ctrl+U
clear it in Claude Code and Codex; Ctrl+C is never sent, since Codex quits on
it once the composer is empty. The agent keeps running and takes a message.
"""
import json
import os
import subprocess
import time
import unittest
import uuid

import test_busy_input_native as busy
import test_input as fixtures


class ClearInput:
    def terminal_request(self, action, **fields):
        request = dict(request_id=str(uuid.uuid4()), action=action, expected_run_id=self.run_id,
                       expected_conversation_id=self.conversation_id, **fields)
        result = subprocess.run([str(fixtures.HGS), 'terminal', self.name, '--json'], input=json.dumps(request),
                                text=True, capture_output=True, env=self.env, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        return request, json.loads(result.stdout)

    def composer(self):
        return self.terminal_request('composer')[1]

    def residue_then_clear(self):
        text, _ = busy.image_message(self.root)
        self.tmux('set-buffer', '-b', 'residue', text)
        self.tmux('paste-buffer', '-p', '-r', '-d', '-b', 'residue', '-t', self.pane)
        self.wait_for(lambda: self.composer()['composer']['state'] == 'text')
        time.sleep(0.5)
        state = self.composer()
        self.assertEqual((state['composer']['clearable'], state['composer']['reason']), (True, ''), state)
        shown = state['composer']['text']
        request, receipt = self.terminal_request('clear-input', terminal_binding_id=state['terminal_binding_id'],
                                                 expires_at=time.time() + 5, text=shown)
        self.assertEqual((receipt['status'], receipt['discarded']), ('cleared', shown), receipt)
        self.assertEqual(self.composer()['composer']['state'], 'empty', self.screen())
        # Never repeated: the same request returns its receipt.
        result = subprocess.run([str(fixtures.HGS), 'terminal', self.name, '--json'], input=json.dumps(request),
                                text=True, capture_output=True, env=self.env, timeout=30)
        self.assertEqual(json.loads(result.stdout), receipt)
        return shown


@unittest.skipUnless(busy.claude.native.CLAUDE, 'Set HGS_CLAUDE_TEST_BIN for the private Clear input contract')
class NativeClearInputClaude(ClearInput, busy.NativeBusyClaude):
    test_busy_tool_queue_preserves_drafts_and_accepts_exactly_once = None
    test_busy_tool_queue_80x24 = None
    test_pasted_images_settle_before_the_one_enter_when_idle = None
    test_busy_pasted_images_are_queued_with_native_proof_and_delivered_once = None

    def test_clear_input_discards_a_residue_and_claude_keeps_running(self):
        shown = self.residue_then_clear()
        self.assertTrue(shown.startswith('[Image #1] [Image #2][Pasted text #'), shown)
        receipt = self.send(self.payload(text='After the cleared residue.'))
        self.assertEqual((receipt['status'], receipt['native']), ('submitted', 'transcript'))
        self.wait_for(lambda: self.accepted('After the cleared residue.'))


@unittest.skipUnless(os.environ.get('HGS_CODEX_TEST_BIN'), 'Set HGS_CODEX_TEST_BIN for the private Clear input contract')
class NativeClearInputCodex(ClearInput, busy.NativeBusyCodex):
    test_busy_tool_queue_preserves_drafts_and_accepts_exactly_once = None
    test_busy_attachment_send_is_queued_and_delivered_once = None

    def launch(self, project, *arguments):
        # Codex shows its home folder itself as a bare `~`, which hgs never takes
        # for its status footer; a real session runs in a project folder, shown
        # as a path. Its configuration stays in CODEX_HOME.
        home = self.root / 'codex-user-home'
        home.mkdir(exist_ok=True)
        tmux = self.tmux
        def launching(*args, **kwargs):
            if args and args[0] == 'new-session':
                args = list(args)
                args.insert(args.index('env') + 1, 'HOME=' + str(home))
            return tmux(*args, **kwargs)
        self.tmux = launching
        try:
            return busy.clear.NativeCodexClear.launch(self, project, *arguments)
        finally:
            del self.tmux

    def test_clear_input_discards_a_residue_and_codex_keeps_running(self):
        shown = self.residue_then_clear()
        self.assertTrue(shown.startswith('[Pasted Content'), shown)
        receipt = self.send(self.payload(text='After the cleared residue.'))
        self.assertEqual(receipt['status'], 'submitted')
        def messages():
            return [r['payload']['message'] for r in map(json.loads, open(self.record['transcript']).read().splitlines())
                    if r.get('type') == 'event_msg' and r.get('payload', {}).get('type') == 'user_message']
        self.wait_for(lambda: 'After the cleared residue.' in messages())


if __name__ == '__main__':
    unittest.main()
