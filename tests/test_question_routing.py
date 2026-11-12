"""The per-child switch of the owner-approved question route (zerus-sa9qn.32), in private state."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

HGS = Path(os.environ.get('HGS_TEST_BIN', Path(__file__).resolve().parents[1] / 'target/debug/hgs')).resolve()
PARENT_ID = '22222222-2222-4222-8222-222222222222'
CHILD_ID = '33333333-3333-4333-8333-333333333333'


@unittest.skipIf(os.name == 'nt', 'POSIX native-process ancestry fixture')
class QuestionRouting(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='zq-', dir='/tmp')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.state = self.root / 'state'; self.state.mkdir()
        self.home = self.root / 'home'; self.home.mkdir()
        sockets = self.root / 'sockets'; sockets.mkdir()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('HGS_')}
        self.env.update(HOME=str(self.home), USERPROFILE=str(self.home), TMUX_TMPDIR=str(sockets),
                        HGS_CONFIG_DIR=str(self.root / 'config'), HGS_STATE_DIR=str(self.state),
                        HGS_SELF='fixture', HGS_PEERS='')
        (self.state / 'local-sessions').touch()
        self.start = subprocess.check_output(['ps', '-p', str(os.getpid()), '-o', 'lstart='],
                                             env=dict(self.env, LC_ALL='C', TZ='UTC'), text=True).strip()
        self.parent = self.record('codex/example/parent', 'codex', 'run-parent', 'conversation-parent', PARENT_ID)
        self.child = self.record('claude/example/child', 'claude', 'run-child', 'conversation-child', CHILD_ID)

    def record(self, name, agent, run, conversation, session, owned=False):
        """A session record; `owned` makes this test process its agent, so hgs proves it as the caller."""
        record = dict(version=1, name=name, agent=agent, run_id=run, conversation_id=conversation,
                      session_id=session, account_id='fixture', subagents={},
                      pid=os.getpid() if owned else 1,
                      process_start=self.start if owned else 'not this process')
        self.write(record)
        return record

    def write(self, record):
        path = self.state / (hashlib.sha256(record['name'].encode()).hexdigest() + '.json')
        path.write_text(json.dumps(record))

    def change(self, send, revision, caller=None, env=None):
        environment = dict(env or self.env)
        if caller:
            environment.update(HGS_SESSION=caller['name'], HGS_RUN_ID=caller['run_id'])
        request = dict(expected_run_id=self.child['run_id'], expected_conversation_id=self.child['conversation_id'],
                       expected_revision=revision, send_to_parent=send)
        result = subprocess.run([str(HGS), 'question-routing', self.child['name'], '--json'], env=environment,
                                input=json.dumps(request), capture_output=True, text=True, timeout=10)
        return result.returncode, json.loads(result.stdout) if result.stdout.strip() else result.stderr

    def listed(self):
        result = subprocess.run([str(HGS), 'ls', '--json', '--local'], env=self.env, capture_output=True,
                                text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)
        rows = json.loads(result.stdout)
        rows = rows[0]['sessions'] if isinstance(rows, list) else rows['sessions']
        return {row['name']: row for row in rows}

    def stored(self):
        path = self.state / 'question-routing.json'
        return json.loads(path.read_text()) if path.exists() else None

    def test_the_person_turns_a_child_off_and_on_by_compare_and_swap(self):
        code, off = self.change(False, 0)
        self.assertEqual(code, 0, off)
        self.assertEqual((off['ok'], off['applied'], off['question_routing']),
                         (True, True, {'send_to_parent': False, 'revision': 1}))
        self.assertEqual(self.stored()['sessions'][CHILD_ID], {'send_to_parent': False, 'revision': 1})
        self.assertEqual(self.listed()[self.child['name']]['question_routing'],
                         {'send_to_parent': False, 'revision': 1})
        # Another session keeps the default.
        self.assertEqual(self.listed()[self.parent['name']]['question_routing'],
                         {'send_to_parent': True, 'revision': 0})
        # A second desktop with the old revision is refused, not merged.
        code, conflict = self.change(True, 0)
        self.assertEqual((code, conflict['error']), (1, 'conflict'))
        code, on = self.change(True, 1)
        self.assertEqual((code, on['question_routing']), (0, {'send_to_parent': True, 'revision': 2}))
        # The same value again changes nothing.
        code, same = self.change(True, 2)
        self.assertEqual((code, same['applied'], same['question_routing']['revision']), (0, False, 2))

    def test_neither_a_parent_nor_a_child_agent_changes_the_switch(self):
        self.assertEqual(self.change(False, 0)[0], 0)
        before = self.stored()
        parent = self.record('codex/example/parent', 'codex', 'run-parent', 'conversation-parent', PARENT_ID,
                             owned=True)
        for caller in (parent, None):
            if caller is None:
                # The child itself, as its own agent.
                caller = self.record('claude/example/child', 'claude', 'run-child', 'conversation-child',
                                     CHILD_ID, owned=True)
                self.child = caller
            for send in (True, False):
                with self.subTest(caller=caller['name'], send=send):
                    code, refused = self.change(send, 1, caller=caller)
                    self.assertEqual((code, refused), (1, {'ok': False, 'error': 'agent_refused'}))
                    self.assertEqual(self.stored(), before)

    def test_an_agent_is_refused_before_any_remote_forwarding(self):
        # zerus-sa9qn.47: the peer's hgs cannot see this caller, so the refusal
        # runs here before the ssh command is built, in both target forms.
        agent = self.record('codex/example/parent', 'codex', 'run-parent', 'conversation-parent', PARENT_ID, owned=True)
        request = json.dumps(dict(expected_run_id='run-child', expected_conversation_id='conversation-child',
                                  expected_revision=0, send_to_parent=True))
        forms = (['@peer', 'question-routing', self.child['name'], '--json'],
                 ['@peer', '--expect-machine', '55555555-5555-4555-8555-555555555555', 'question-routing',
                  self.child['name'], '--json'])
        for form in forms:
            def run(extra):
                return subprocess.run([str(HGS), '--dry-run', *form], env=dict(self.env, HGS_PEERS='peer', **extra),
                                      input=request, capture_output=True, text=True, timeout=10)
            with self.subTest(form=form[1]):
                refused = run(dict(HGS_SESSION=agent['name'], HGS_RUN_ID=agent['run_id']))
                self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
                self.assertEqual(json.loads(refused.stdout), {'ok': False, 'error': 'agent_refused'})
                self.assertNotIn('ssh', refused.stdout + refused.stderr)
                # The person's call (no agent process tree) goes on to the
                # forwarding path: the plain form prints its ssh command; the
                # guarded form reaches its own refusal of --dry-run.
                forwarded = run({})
                if form[1] == 'question-routing':
                    self.assertEqual(forwarded.returncode, 0, forwarded.stderr)
                    self.assertIn('ssh', forwarded.stdout)
                    self.assertIn('question-routing', forwarded.stdout)
                else:
                    self.assertEqual(forwarded.returncode, 2, forwarded.stderr)
                    self.assertIn('Guarded calls do not support --dry-run', forwarded.stderr)
        self.assertIsNone(self.stored())

    def test_the_gui_and_the_connector_are_not_agents(self):
        # Their hgs runs outside any agent's process tree. A shell that
        # inherited an agent's variables is still not that agent's process.
        inherited = self.record('codex/example/other', 'codex', 'run-other', 'conversation-other',
                                '44444444-4444-4444-8444-444444444444')
        code, off = self.change(False, 0, caller=inherited)
        self.assertEqual((code, off['question_routing']['send_to_parent']), (0, False), off)
        code, on = self.change(True, 1)
        self.assertEqual((code, on['question_routing']['send_to_parent']), (0, True), on)

    def test_a_session_without_a_durable_id_is_unsupported(self):
        legacy = dict(self.child)
        legacy.pop('session_id')
        self.write(legacy)
        code, refused = self.change(False, 0)
        self.assertEqual((code, refused['error']), (1, 'unsupported'))
        self.assertIsNone(self.stored())

    def test_the_marker_is_advertised(self):
        usage = subprocess.run([str(HGS), '--help'], env=self.env, capture_output=True, text=True, timeout=10).stdout
        self.assertIn('session-question-routing-v1', [line.strip() for line in usage.splitlines()])


if __name__ == '__main__':
    unittest.main()
