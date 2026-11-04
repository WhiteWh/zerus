"""Exact untracked-shell termination; private tmux and no native agents."""
import copy
import json
import hashlib
import subprocess
import shlex
import sys
import unittest
from unittest import mock
import time
import uuid

import test_pause


@unittest.skipUnless(test_pause.TMUX and sys.platform in ('linux', 'darwin'), 'Private Unix tmux required')
class UntrackedShellTermination(test_pause.Harness):
    def setUp(self):
        super().setUp()
        self.name = 'private-shell'
        self.create(self.name)
        self.create('private-neighbour')

    def create(self, name):
        self.t('new-session', '-d', '-s', name, '-c', str(self.project), '/bin/bash', '--noprofile', '--norc')
        # new-session can return while the forked pane still has tmux's image.
        # Require the actual shell, without changing production's allowlist.
        deadline = time.monotonic() + 3
        command = [str(self.bin/'tmux'), 'display-message', '-p', '-t', '='+name+':', '#{pane_current_command}']
        while time.monotonic() < deadline:
            result = subprocess.run(command, env=self.env, capture_output=True, text=True,
                                    check=True, timeout=max(.001, min(1, deadline-time.monotonic())))
            if result.stdout.strip() == 'bash':
                return
            time.sleep(.01)
        self.fail('The private pane did not exec its shell within 3 seconds')

    def identity(self, name=None):
        result = json.loads(self.hgs('inspect', name or self.name, '--after', str(2**63-1), '--skip-processes'))
        self.assertFalse(result['tracked'])
        self.assertNotIn('run_id', result)
        return result['untracked_terminal']

    def request(self, identity, request_id=None, ok=True):
        body = dict(request_id=request_id or str(uuid.uuid4()), action='terminate_shell',
                    expected_run_id='', expected_conversation_id='', terminal_identity=identity)
        p = subprocess.run([str(test_pause.HGS), 'session-action', self.name, '--json'], input=json.dumps(body),
                           env=self.env, capture_output=True, text=True, timeout=15)
        self.assertEqual(p.returncode, 0, p.stderr)
        receipt = json.loads(p.stdout)
        self.assertEqual(receipt['status'], 'completed' if ok else 'failed', receipt)
        return receipt

    def alive(self, name=None):
        return subprocess.run([str(self.bin/'tmux'), 'has-session', '-t', '='+ (name or self.name)],
                              env=self.env, capture_output=True).returncode == 0

    def test_exact_shell_exits_without_archive_or_folder_cleanup_and_receipt_never_replays(self):
        identity = self.identity()
        kept = self.project/'keep.txt'; kept.write_text('keep the working folder')
        request_id = str(uuid.uuid4())
        receipt = self.request(identity, request_id)
        self.assertTrue(receipt['terminal_closed'])
        self.assertEqual(receipt['terminal_identity'], identity)
        self.assertFalse(self.alive())
        self.assertTrue(self.alive('private-neighbour'))
        self.assertTrue(kept.is_file())
        self.assertFalse(list((self.root/'state/archives').glob('*.json')))
        self.create(self.name)
        self.assertNotEqual(self.identity()['session_id'], identity['session_id'])
        self.assertEqual(self.request(identity, request_id), receipt)
        self.assertTrue(self.alive(), 'a completed original request must not terminate a replacement')

    def test_changed_terminal_fields_refuse_without_touching_either_private_shell(self):
        identity = self.identity()
        for field, changed in [('session_id', '$99999'), ('session_created', identity['session_created']+1),
                               ('pane_id', '%99999'), ('pane_pid', identity['pane_pid']+1),
                               ('pane_start', 'different-birth'), ('server_pid', identity['server_pid']+1),
                               ('server_start', 'different-server'), ('socket_ino', str(int(identity['socket_ino'])+1)),
                               ('socket_dev', str(int(identity['socket_dev'])+1))]:
            with self.subTest(field=field):
                bad = copy.deepcopy(identity); bad[field] = changed
                self.request(bad, ok=False)
                self.assertTrue(self.alive()); self.assertTrue(self.alive('private-neighbour'))

    def test_a_same_name_replacement_is_refused(self):
        identity = self.identity()
        self.t('kill-session', '-t', identity['session_id'])
        self.create(self.name)
        self.request(identity, ok=False)
        self.assertTrue(self.alive())

    def test_agent_binding_and_native_run_marker_exclude_this_path(self):
        identity = self.identity()
        self.t('set-option', '-t', identity['session_id'], '@hgs_run', str(uuid.uuid4()))
        self.request(identity, ok=False); self.assertTrue(self.alive())
        self.t('set-option', '-u', '-t', identity['session_id'], '@hgs_run')
        state = self.root/'state'; state.mkdir(exist_ok=True)
        binding = state/(hashlib.sha256(self.name.encode()).hexdigest()+'.json')
        binding.write_text(json.dumps(dict(name=self.name, agent='codex', run_id='agent-run', conversation_id='agent-conversation')))
        self.request(identity, ok=False); self.assertTrue(self.alive())
        self.assertEqual(json.loads(binding.read_text())['run_id'], 'agent-run')

    def assert_server_queue_refusal(self, *arguments):
        identity = self.identity()
        # Mutate only after Rust's fresh read, before the real server fence.
        wrapper = self.bin/'tmux'
        before = wrapper.read_text()
        marker = self.root/'mutation-reached'
        mutate = shlex.join([test_pause.TMUX, '-S', self.socket, *arguments])
        wrapper.write_text(before.replace('exec ',
            'case " $* " in *" if-shell "*) '+mutate+' && touch '+shlex.quote(str(marker))+' ;; esac\nexec ', 1))
        try:
            receipt = self.request(identity, ok=False)
        finally:
            self.assertTrue(marker.exists(), 'the exact before-server mutation must be reached')
        self.assertIn('could not be verified', receipt['error'])
        self.assertTrue(self.alive()); self.assertTrue(self.alive('private-neighbour'))

    def test_server_queue_fence_refuses_a_run_marker_added_after_the_fresh_read(self):
        self.assert_server_queue_refusal('set-option', '-t', self.identity()['session_id'], '@hgs_run', 'fixture-run')

    def test_a_grouped_session_refuses_before_closing_either_terminal(self):
        identity = self.identity()
        self.t('new-session', '-d', '-s', 'private-group', '-t', '='+self.name)
        self.assertEqual(self.t('display-message', '-p', '-t', '='+self.name+':', '#{session_grouped}').strip(), '1')
        result = json.loads(self.hgs('inspect', self.name, '--after', str(2**63-1), '--skip-processes'))
        self.assertNotIn('untracked_terminal', result)
        self.request(identity, ok=False)
        self.assertTrue(self.alive()); self.assertTrue(self.alive('private-group'))

    def test_a_linked_window_refuses_before_closing_either_terminal(self):
        identity = self.identity()
        self.t('link-window', '-s', '='+self.name+':', '-t', '=private-neighbour:1')
        # This independently exercises window_linked: the session is not grouped.
        self.assertEqual(self.t('display-message', '-p', '-t', '='+self.name+':', '#{session_grouped} #{window_linked}').strip(), '0 1')
        result = json.loads(self.hgs('inspect', self.name, '--after', str(2**63-1), '--skip-processes'))
        self.assertNotIn('untracked_terminal', result)
        self.request(identity, ok=False)
        self.assertTrue(self.alive()); self.assertTrue(self.alive('private-neighbour'))

    def test_server_queue_fence_refuses_a_group_added_after_the_fresh_read(self):
        self.assert_server_queue_refusal('new-session', '-d', '-s', 'private-group', '-t', '='+self.name)
        self.assertTrue(self.alive('private-group'))

    def test_server_queue_fence_refuses_a_link_added_after_the_fresh_read(self):
        self.assert_server_queue_refusal('link-window', '-s', '='+self.name+':', '-t', '=private-neighbour:1')

    def test_create_waits_past_the_pre_exec_tmux_image(self):
        run, reads = subprocess.run, []
        def observe(command, **kwargs):
            if command[1:] == ['display-message', '-p', '-t', '=private-starting:', '#{pane_current_command}']:
                reads.append(command)
                if len(reads) == 1:
                    return subprocess.CompletedProcess(command, 0, stdout='tmux\n', stderr='')
            return run(command, **kwargs)
        with mock.patch.object(subprocess, 'run', side_effect=observe):
            self.create('private-starting')
        self.assertGreaterEqual(len(reads), 2, 'pre-exec is not readiness')
        self.assertEqual(self.identity('private-starting')['shell'], 'bash')

    def test_multiple_panes_have_no_shell_admission(self):
        self.t('split-window', '-d', '-t', '='+self.name+':', '/bin/bash', '--noprofile', '--norc')
        result = json.loads(self.hgs('inspect', self.name, '--after', str(2**63-1), '--skip-processes'))
        self.assertNotIn('untracked_terminal', result)
        self.assertIn('exit', result['untracked_terminal_reason'])
        self.assertTrue(self.alive())

    def test_an_untracked_non_shell_program_has_no_shell_admission(self):
        name = 'private-program'
        self.t('new-session', '-d', '-s', name, 'python3', '-c', 'import time; time.sleep(60)')
        result = json.loads(self.hgs('inspect', name, '--after', str(2**63-1), '--skip-processes'))
        self.assertNotIn('untracked_terminal', result)
        self.assertIn('exit', result['untracked_terminal_reason'])
        self.assertTrue(self.alive(name))
