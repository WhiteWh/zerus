"""zerus-ppd26.15: a command under a recorded resident DeepSeek host is never
another agent's session and never the person, even when that agent's recorded
process is a live ancestor of the host. The process tree and the host records
decide, never a marker: these commands carry no HGS_AGENT.

A stand-in host (a child of this test process) is recorded in the private
state's dsh/hosts by process ID and birth, as hgs records a real host; the
Claude record names this test process, so it is a live ancestor of every
command the stand-in runs. Private roots; every inherited HGS_, ZERUS_ and
TMUX variable is cleared.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from urllib.parse import urlencode
import uuid

HGS = Path(os.environ.get('HGS_TEST_BIN', Path(__file__).resolve().parents[1] / 'target/debug/hgs')).resolve()
UNDER_HOST = 'This command runs under a DeepSeek agent host'

# The stand-in host: runs each requested command as its own child.
HOST = r'''
import json, subprocess, sys
for line in sys.stdin:
    request = json.loads(line)
    r = subprocess.run(request['argv'], env=request['env'], input=request.get('input'), text=True,
                       capture_output=True, timeout=60, cwd=request['cwd'])
    print(json.dumps({'code': r.returncode, 'stdout': r.stdout, 'stderr': r.stderr}), flush=True)
'''


def birth(pid):
    """The process's start ticks, as platform::process_identity reads them on Linux."""
    stat = Path(f'/proc/{pid}/stat').read_text()
    return stat.rsplit(')', 1)[1].split()[19]


class Result:
    def __init__(self, code, stdout, stderr):
        self.returncode, self.stdout, self.stderr = code, stdout, stderr


@unittest.skipUnless(sys.platform == 'linux', 'Linux /proc births for the stand-in host record')
class UnderARecordedHost(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='zerus-host-ancestry-',
                                                dir=os.environ.get('ZERUS_TEST_SHORT_TMPDIR'))
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        for name in ('state', 'config', 'home', 'tmux', 'ws'):
            (self.root / name).mkdir(mode=0o700)
        self.state = self.root / 'state'
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(('HGS_', 'ZERUS_', 'TMUX', 'CLAUDE'))}
        self.env.update(HOME=str(self.root / 'home'), HGS_CONFIG_DIR=str(self.root / 'config'),
                        HGS_STATE_DIR=str(self.state), TMUX_TMPDIR=str(self.root / 'tmux'), HGS_PEERS='',
                        HGS_SELF='test')
        self.call('swarm', 'initialize', input=json.dumps({'projects': [
            {'id': 'project-1', 'name': 'orchard', 'color': '#123456', 'folders': [], 'sessions': []}]}))
        self.ref = json.loads(self.call('task', 'add', '--project', 'orchard', '--title', 'host ancestry',
                                        '--json', input='zerus-ppd26.15 probe').stdout)['task']['ref']
        # The claimed agent: this test process, a live ancestor of the host.
        start = subprocess.check_output(['ps', '-p', str(os.getpid()), '-o', 'lstart='],
                                        env={'LC_ALL': 'C', 'TZ': 'UTC', 'PATH': '/usr/bin:/bin'}, text=True).strip()
        self.lead = dict(version=1, name='claude/example/lead', agent='claude', run_id='run-lead',
                         conversation_id=str(uuid.uuid4()), pid=os.getpid(), process_start=start,
                         activity='idle', phase='idle', active_tools={}, subagents={},
                         last_event_at=time.time(), created=time.time())
        (self.state / (hashlib.sha256(self.lead['name'].encode()).hexdigest() + '.json')).write_text(
            json.dumps(self.lead))
        self.claim = dict(self.env, HGS_SESSION=self.lead['name'], HGS_RUN_ID=self.lead['run_id'])
        self.host = subprocess.Popen([sys.executable, '-I', '-c', HOST], stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, text=True)
        self.addCleanup(self.stop)
        self.folder = self.state / 'dsh/hosts' / hashlib.sha256(b'/agent/home').hexdigest()[:16]
        self.folder.mkdir(parents=True, mode=0o700)
        self.record = self.folder / 'host.identity.json'
        self.record.write_text(json.dumps({'state': 'running', 'version': 1, 'pid': self.host.pid,
                                           'birth': birth(self.host.pid), 'pid_file': [0, 0, 0, 0]}))

    def stop(self):
        self.host.stdin.close()
        self.host.wait(timeout=10)
        self.host.stdout.close()

    def call(self, *args, env=None, input=None, ok=True):
        result = subprocess.run([str(HGS), *args], input=input, env=env or self.env, text=True,
                                capture_output=True, timeout=60, cwd=self.root / 'ws')
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def hosted(self, *args, env, input=None):
        """`hgs args` run by the stand-in host."""
        self.host.stdin.write(json.dumps({'argv': [str(HGS), *args], 'env': env, 'input': input,
                                          'cwd': str(self.root / 'ws')}) + '\n')
        self.host.stdin.flush()
        reply = json.loads(self.host.stdout.readline())
        return Result(reply['code'], reply['stdout'], reply['stderr'])

    def history(self):
        log = json.loads(self.call('task', 'get', self.ref, '--history-limit', '20').stdout)['task']['log']
        return [entry['kind'] for entry in log]

    def test_task_commands_act_as_no_agent_and_never_as_the_person(self):
        claimed = self.hosted('task', 'comment', self.ref, env=self.claim, input='as the lead')
        self.assertNotEqual(claimed.returncode, 0, claimed.stdout)
        self.assertIn(UNDER_HOST, claimed.stderr)
        self.assertIn('claude/example/lead', claimed.stderr)
        for args in (('task', 'comment', self.ref), ('task', 'add', '--project', 'orchard', '--title', 'x'),
                     ('task', 'start-plan', self.ref, '--json')):
            person = self.hosted(*args, env=self.env, input='{}' if 'start-plan' in args else 'as the person')
            self.assertNotEqual(person.returncode, 0, (args, person.stdout))
            self.assertIn(UNDER_HOST, person.stderr, args)
        self.assertEqual(self.history(), ['created'])
        # The person outside the host is still the person.
        self.call('task', 'comment', self.ref, input='from the person')
        self.assertEqual(self.history(), ['created', 'comment'])

    def test_hgs_self_names_no_other_agent(self):
        outside = json.loads(self.call('self', '--json', env=self.claim).stdout)
        self.assertEqual((outside['name'], outside['identity_source']), ('claude/example/lead', 'process'))
        hosted = self.hosted('self', '--json', env=self.claim)
        self.assertEqual(hosted.returncode, 1, hosted.stdout)
        self.assertIn('hgs self: ' + UNDER_HOST, hosted.stderr)

    def test_question_routing_refuses_the_hosted_command_here_and_before_forwarding(self):
        request = json.dumps(dict(expected_run_id='run-child', expected_conversation_id='conversation-child',
                                  expected_revision=0, send_to_parent=False))
        # The public command, its @host form before forwarding, and hooks'
        # private state entry, which only the change's own check guards.
        forms = (['question-routing', 'claude/example/child', '--json'],
                 ['--dry-run', '@peer', 'question-routing', 'claude/example/child', '--json'],
                 ['__state', 'question-routing', 'claude/example/child', '--json'])
        for form in forms:
            hosted = self.hosted(*form, env=dict(self.env, HGS_PEERS='peer'), input=request)
            self.assertEqual(hosted.returncode, 1, (form, hosted.stderr))
            self.assertEqual(json.loads(hosted.stdout), {'ok': False, 'error': 'agent_refused'}, form)
            self.assertNotIn('ssh', hosted.stdout + hosted.stderr)
            outside = self.call(*form, env=dict(self.env, HGS_PEERS='peer'), input=request, ok=False)
            self.assertNotIn('agent_refused', outside.stdout, form)

    def test_a_wait_is_declared_for_no_agent(self):
        producer = 'zerus://session?' + urlencode(dict(machine='22222222-2222-4222-8222-222222222222', host='peer',
                                                       name='claude/example/worker', conversation='c2', run='r2'))
        body = lambda: json.dumps(dict(request_id=str(uuid.uuid4()), producer_reference=producer, label='probe'))
        outside = json.loads(self.call('wait', 'begin', '--json', env=self.claim, input=body()).stdout)
        self.assertEqual(outside['reply_ticket']['expected_run_id'], 'run-lead')
        hosted = self.hosted('wait', 'begin', '--json', env=self.claim, input=body())
        self.assertNotEqual(hosted.returncode, 0, hosted.stdout)
        self.assertNotIn('run-lead', hosted.stdout)

    def test_desktop_mode_registration_is_refused(self):
        hosted = self.hosted('desktop', 'registration', 'mode-check', '--json', env=self.env)
        self.assertNotEqual(hosted.returncode, 0)
        self.assertIn(UNDER_HOST, hosted.stderr)
        self.assertIn('Change desktop mode from Settings or a terminal outside Zerus', hosted.stderr)
        outside = self.call('desktop', 'registration', 'mode-check', '--json', ok=False)
        self.assertNotIn('DeepSeek agent host', outside.stderr)

    def unshared(self, *args, env, input=None):
        """`hgs args` as PID 1 of a private user and PID namespace, as in an agent's sandbox."""
        return subprocess.run(['unshare', '--user', '--map-current-user', '--pid', '--mount-proc', '--fork',
                               str(HGS), *args], input=input, env=env, text=True, capture_output=True, timeout=60,
                              cwd=self.root / 'ws')

    def test_a_separate_pid_namespace_is_named_for_marked_and_unmarked_commands(self):
        # zerus-ppd26.17: a host outside a separate PID namespace cannot be seen,
        # so the check refuses there with its own sentence, for a marked claim
        # (whose run is not visible inside either) as for the person.
        if shutil.which('unshare') is None or subprocess.run(
                ['unshare', '--user', '--map-current-user', '--pid', '--mount-proc', '--fork', 'true'],
                capture_output=True, timeout=30).returncode != 0:
            self.skipTest('unprivileged user and PID namespaces are not available here')
        sentence = "This command runs inside a separate process namespace (for example an agent's sandbox)"
        claimed = self.unshared('task', 'comment', self.ref, env=self.claim, input='as the lead')
        self.assertNotEqual(claimed.returncode, 0, claimed.stdout)
        self.assertIn('claude/example/lead', claimed.stderr)
        self.assertIn(sentence, claimed.stderr)
        person = self.unshared('task', 'add', '--project', 'orchard', '--title', 'x', env=self.env,
                               input='as the person')
        self.assertNotEqual(person.returncode, 0, person.stdout)
        self.assertIn(sentence, person.stderr)
        own = self.unshared('self', '--json', env=self.claim)
        self.assertEqual(own.returncode, 1, own.stdout)
        self.assertIn('hgs self: ' + sentence, own.stderr)
        request = json.dumps(dict(expected_run_id='r', expected_conversation_id='c', expected_revision=0,
                                  send_to_parent=False))
        routing = self.unshared('question-routing', 'claude/example/child', '--json', env=self.env, input=request)
        reply = json.loads(routing.stdout)
        self.assertEqual((routing.returncode, reply['error']), (1, 'host_unverified'))
        self.assertTrue(reply['message'].startswith(sentence), reply)
        self.assertEqual(self.history(), ['created'])

    def test_an_unreadable_host_record_refuses_both_directions_and_names_it(self):
        self.record.write_text('{')
        path = str(self.record.resolve())
        clear = f'remove {path}.'
        person = self.call('task', 'comment', self.ref, input='from the person', ok=False)
        self.assertNotEqual(person.returncode, 0)
        self.assertIn(path, person.stderr)
        self.assertIn(clear, person.stderr)
        claimed = self.call('self', '--json', env=self.claim, ok=False)
        self.assertEqual(claimed.returncode, 1)
        self.assertIn(clear, claimed.stderr)
        request = json.dumps(dict(expected_run_id='r', expected_conversation_id='c', expected_revision=0,
                                  send_to_parent=False))
        routing = self.call('question-routing', 'claude/example/child', '--json', input=request, ok=False)
        reply = json.loads(routing.stdout)
        self.assertEqual((routing.returncode, reply['error']), (1, 'host_unverified'))
        self.assertIn(clear, reply['message'])
        # Reads resolve their caller too, so they are refused with the same sentence.
        read = self.call('task', 'get', self.ref, ok=False)
        self.assertNotEqual(read.returncode, 0)
        self.assertIn(clear, read.stderr)
        # A record from an earlier boot never blocks anyone.
        boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
        other = str(uuid.UUID(int=uuid.UUID(boot).int ^ 1))
        self.record.write_text(json.dumps({'state': 'running', 'version': 2, 'pid': self.host.pid,
                                           'birth': birth(self.host.pid), 'pid_file': [0, 0, 0, 0], 'boot': other}))
        self.assertEqual(self.history(), ['created'])
        self.call('task', 'comment', self.ref, input='after the old boot record')
        hosted = self.hosted('task', 'comment', self.ref, env=self.env, input='hosted, old boot record')
        self.assertEqual(hosted.returncode, 0, hosted.stderr)


if __name__ == '__main__':
    unittest.main()
