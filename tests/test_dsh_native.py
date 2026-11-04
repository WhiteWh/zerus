"""Contract tests against the released official harness, with a keyless model.
No user credentials, live terminals, real model requests, or browser opening.
"""
import base64
import concurrent.futures
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import unittest
import uuid
import signal
import sys
import urllib.request
import http.cookiejar
import importlib.util

REPO = Path(__file__).resolve().parents[1]
DSH = shutil.which('dsh')
HGS = Path(os.environ.get('HGS_TEST_BIN', REPO / 'target/debug/hgs'))
_fixture_spec = importlib.util.spec_from_file_location('ci_dsh_fixture', REPO / 'scripts/ci/dsh_fixture.py')
fixture = importlib.util.module_from_spec(_fixture_spec)
_fixture_spec.loader.exec_module(fixture)


@unittest.skipUnless(fixture.required() or DSH and HGS.exists(), 'official dsh and built hgs required')
class NativeHarness(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        fixture.admit_fixture(DSH, HGS)
        cls.temp = tempfile.TemporaryDirectory(prefix='hgs-dsh-', dir='/tmp')
        cls.root = Path(cls.temp.name)
        cls.home = cls.root / 'native'
        cls.state = cls.root / 's'
        host_key = hashlib.sha256(str(cls.home).encode()).hexdigest()[:16]
        cls.host = cls.state / 'dsh/hosts' / host_key
        cls.host.mkdir(parents=True, mode=0o700)
        cls.sock = str(cls.host / 'host.sock')
        cls.workspace = cls.root / 'workspace'
        cls.workspace.mkdir()
        (cls.root / 'holds').mkdir()
        package = fixture.package_root(DSH)
        bridge = REPO / 'src/dsh/bridge.mjs'
        if getattr(cls, 'LEGACY_ADAPTER', False):
            # Same released harness, but the resident bridge predates account
            # permissions. It ignores the new wire field just like that host.
            bridge = cls.root / 'legacy-bridge.mjs'
            source = (REPO / 'src/dsh/bridge.mjs').read_text()
            source = source.replace('accountPermissions: true', 'legacyAdapter: true')
            source = source.replace('applyPermissions(handle.agent, permissionMode);', '')
            source = source.replace('applyPermissions(handle.agent, p.permissionMode);', '')
            bridge.write_text(source)
            shutil.copyfile(REPO / 'src/dsh/command-guard.mjs', cls.root / 'command-guard.mjs')
        patch = [
            {'id': 'llm-deepseek', 'disabled': True},
            {'id': 'agent-default-model', 'config': {'provider': 'hgs-test', 'model': 'test-a'}},
            {'insert': [
                {'id': 'hgs-test-model', 'name': str(REPO / 'tests/fixtures/dsh-model.mjs'), 'config': {'holds': str(cls.root / 'holds')}},
                {'id': 'hgs-native-dsh', 'name': str(bridge), 'config': {'socket': cls.sock, 'recoveryPolicy': str(cls.state / 'recovery/policy.json')}},
            ]},
        ]
        if getattr(cls, 'SCHEDULE_BUNDLE', False):
            # The web bundle includes the Schedule plugin; its time context is optional.
            patch[-1]['insert'] += [{'id': 'time-context', 'name': '@deepseek-ai/dsh-time-context'}]
        else:
            # A profile can still disable it; the other suites keep that profile.
            patch.append({'id': 'schedule', 'disabled': True})
        overlay = cls.root / 'patch.json'
        overlay.write_text(json.dumps(patch))
        # hgs ls and the recovery worker also list tmux sessions. An existing
        # private TMUX_TMPDIR keeps them off the user's default server; a
        # missing directory would silently fall back to it.
        tmux = cls.root / 'tmux'
        tmux.mkdir(mode=0o700)
        cls.env = {'PATH': os.environ['PATH'], 'LANG': 'C.UTF-8', 'HOME': str(cls.root / 'user'),
                   'DSH_HOME': str(cls.home), 'DSH_TELEMETRY_DISABLED': '1',
                   'HGS_DSH_TEST_PACKAGE': str(package), 'HGS_STATE_DIR': str(cls.state),
                   'HGS_CONFIG_DIR': str(cls.root / 'config'), 'HGS_SELF': 'test', 'HGS_PEERS': '',
                   'TMUX_TMPDIR': str(tmux)}
        Path(cls.env['HOME']).mkdir()
        cls.log = (cls.root / 'host.log').open('w')
        cls.proc = subprocess.Popen([DSH, '--profile', 'web', '--patch', str(overlay), '--no-open',
                                     '--host', '127.0.0.1', '--port', '0'], env=cls.env, cwd=cls.workspace,
                                    stdin=subprocess.DEVNULL, stdout=cls.log, stderr=cls.log, start_new_session=True)
        deadline = time.monotonic() + 40
        while not Path(cls.sock).exists() and cls.proc.poll() is None and time.monotonic() < deadline:
            time.sleep(.1)
        if not Path(cls.sock).exists():
            cls.proc.terminate(); cls.proc.wait(timeout=10)
            raise RuntimeError('Native test host did not start: ' + (cls.root / 'host.log').read_text()[-2500:])

    @classmethod
    def tearDownClass(cls):
        cls.proc.terminate()
        try:
            cls.proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            cls.proc.kill(); cls.proc.wait(timeout=5)
        cls.log.close(); cls.temp.cleanup()

    @classmethod
    def rpc(cls, method, params=None):
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(30); client.connect(cls.sock)
            client.sendall((json.dumps({'method': method, 'params': params or {}}) + '\n').encode())
            response = json.loads(client.makefile().readline())
        if not response['ok']:
            raise RuntimeError(response.get('error', str(response)))
        return response['value']

    def create(self, **native):
        sid = 'session-' + str(uuid.uuid4())
        name = 'dsh/test/' + str(uuid.uuid4())[:8]
        self.rpc('create', {'sessionId': sid, 'cwd': str(self.workspace), 'title': name, **native})
        binding = {'backend': 'dsh', 'name': name, 'conversation_id': sid, 'run_id': str(uuid.uuid4()),
                   'agent_home': str(self.home), 'cwd': str(self.workspace), 'created': time.time()}
        directory = self.state / 'dsh/bindings'; directory.mkdir(exist_ok=True)
        (directory / (hashlib.sha256(name.encode()).hexdigest() + '.json')).write_text(json.dumps(binding))
        return binding

    def cli(self, command, binding, request=None):
        proc = subprocess.run([str(HGS), command, binding['name']], input=json.dumps(request) if request else None,
                              env=self.env, cwd=REPO, text=True, capture_output=True, timeout=35)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        return json.loads(proc.stdout) if proc.stdout.strip() else None

    def payload(self, binding, **fields):
        return {'request_id': str(uuid.uuid4()), 'expected_run_id': binding['run_id'],
                'expected_conversation_id': binding['conversation_id'], **fields}

    def wait(self, predicate, timeout=15):
        until = time.monotonic() + timeout
        while time.monotonic() < until:
            value = predicate()
            if value:
                return value
            time.sleep(.1)
        self.fail('Native event did not arrive')

    def test_leading_absolute_path_is_literal_message_text(self):
        b = self.create()
        text = '/help/fixture/capture.png\nWhat is shown in this screenshot?'
        request = self.payload(b, text=text, attachments=[])
        receipt = self.cli('send', b, request)
        self.assertEqual(receipt['status'], 'submitted')
        data = self.wait(lambda: (d if d.get('reply_id') and any(e['type'] == 'AgentMessage' for e in d['events']) else None)
                         if (d := self.cli('inspect', b)) else None)
        self.assertEqual([e['detail'] for e in data['events'] if e['type'] == 'UserPromptSubmit'], [text])
        self.assertTrue(any(text in e['detail'] for e in data['events'] if e['type'] == 'AgentMessage'))
        self.assertEqual(self.cli('send', b, request), receipt)
        after = self.cli('inspect', b)
        self.assertEqual([e['detail'] for e in after['events'] if e['type'] == 'UserPromptSubmit'], [text])

    def test_real_history_goals_private_reasoning_and_idempotent_send(self):
        b = self.create()
        request = self.payload(b, text='Hello native harness', attachments=[])
        self.assertEqual(self.cli('send', b, request)['status'], 'submitted')
        self.cli('send', b, request)
        data = self.wait(lambda: (d if d.get('reply_id') and any(e['type'] == 'AgentMessage' for e in d['events']) else None)
                         if (d := self.cli('inspect', b)) else None)
        replies = [e for e in data['events'] if e['type'] == 'AgentMessage']
        self.assertGreaterEqual(len(replies), 1)
        self.assertEqual(len([e for e in data['events'] if e['type']=='UserPromptSubmit' and e['detail']=='Hello native harness']), 1)
        self.assertIn('Hello native harness', replies[0]['detail'])
        self.assertNotIn('PRIVATE_TEST_REASONING', json.dumps(data))
        self.assertNotIn('Current runtime context', json.dumps(data))
        self.assertEqual(data['goal']['status'], 'paused')
        self.assertEqual(data['goal']['round_budget'], 8)
        self.assertTrue(data['reply_id'])
        usage=data['session_usage']
        self.assertEqual(usage['status'],'ok')
        self.assertEqual(usage['totals']['input'],4)
        self.assertEqual(usage['totals']['output'],5)
        self.assertEqual(usage['totals']['total'],9)
        self.assertIsNone(usage['cost_usd'])

    def test_shell_registry_output_cursor_targeted_stop_and_identity(self):
        b=self.create()
        self.cli('send',b,self.payload(b,text='SHELL_BACKGROUND_FIXTURE',attachments=[]))
        data=self.wait(lambda: (d if d['phase']=='idle' and d.get('processes',{}).get('active_count')==1 else None) if (d:=self.cli('inspect',b)) else None)
        light=subprocess.run([str(HGS),'inspect',b['name'],'--skip-processes'],env=self.env,text=True,capture_output=True,timeout=15)
        self.assertEqual(light.returncode,0,light.stderr);self.assertNotIn('processes',json.loads(light.stdout))
        self.assertNotIn('jobs',self.rpc('inspect',{'sessionId':b['conversation_id'],'includeProcesses':False}))
        jobs=data['processes']['items'];self.assertEqual(len(jobs),1,jobs)
        job=jobs[0];self.assertEqual(job['status'],'running');self.assertTrue(job['capabilities']['stop'])
        def process(action,**overrides):
            params={'run':b['run_id'],'conversation':b['conversation_id'],'generation':data['host_generation'],**overrides}
            args=[str(HGS),'processes',b['name'],action,job['id']]
            for k,v in params.items():args+=['--'+k,v]
            return subprocess.run(args,env=self.env,text=True,capture_output=True,timeout=15)
        for _ in range(2):
            output=process('--output');self.assertEqual(output.returncode,0,output.stderr)
            self.assertIn('independent-output',json.loads(output.stdout)['output'])
        foreign=self.create()
        with self.assertRaisesRegex(RuntimeError,'no longer belongs'):
            self.rpc('job-stop',{'sessionId':foreign['conversation_id'],'owner':b['conversation_id'],'jobId':job['native_id'],'generation':data['host_generation']})
        wrong=process('--stop',generation='old-host');self.assertNotEqual(wrong.returncode,0)
        wrong=process('--stop',run='old-run');self.assertNotEqual(wrong.returncode,0)
        self.cli('send',b,self.payload(b,text='SHELL_READ_FIXTURE',attachments=[]))
        inspected=self.wait(lambda: (d if any(e['type']=='PostToolUse' and 'independent-output' in e['detail'] for e in d['events']) else None) if (d:=self.cli('inspect',b)) else None)
        read=next(e for e in inspected['events'] if e['type']=='PostToolUse' and 'independent-output' in e['detail'])
        self.assertIn('independent-output',read['detail'])
        stopped=process('--stop');self.assertEqual(stopped.returncode,0,stopped.stderr)
        final=self.wait(lambda: (d if any(j['status']=='stopped' for j in d['processes']['items']) else None) if (d:=self.cli('inspect',b)) else None)
        self.assertEqual(final['processes']['active_count'],0)

    def test_foreground_shell_failure_keeps_output_after_registry_removal(self):
        b=self.create();self.cli('send',b,self.payload(b,text='SHELL_FOREGROUND_FIXTURE',attachments=[]))
        data=self.wait(lambda: (d if d['phase']=='idle' and d.get('processes',{}).get('items') else None) if (d:=self.cli('inspect',b)) else None)
        light=subprocess.run([str(HGS),'inspect',b['name'],'--skip-processes'],env=self.env,text=True,capture_output=True,timeout=15)
        self.assertEqual(light.returncode,0,light.stderr);self.assertNotIn('processes',json.loads(light.stdout))
        self.assertNotIn('jobs',self.rpc('inspect',{'sessionId':b['conversation_id'],'includeProcesses':False}))
        jobs=data['processes']['items'];self.assertEqual(len(jobs),1,jobs);job=jobs[0]
        self.assertEqual(job['status'],'failed',job);self.assertEqual(job['exit_code'],7)
        self.assertFalse(job['capabilities']['stop'])
        result=subprocess.run([str(HGS),'processes',b['name'],'--output',job['id'],'--run',b['run_id'],'--conversation',b['conversation_id']],env=self.env,text=True,capture_output=True,timeout=15)
        self.assertEqual(result.returncode,0,result.stderr);self.assertIn('foreground-output',json.loads(result.stdout)['output'])

    def test_command_guard_returns_a_tool_error_even_in_bypass(self):
        binding = self.create(permissionMode='bypass')
        self.cli('send', binding, self.payload(binding, text='SHELL_GUARD_FIXTURE', attachments=[]))
        data = self.wait(lambda: (d if d['phase'] == 'idle' and any(
            e['type'] == 'PostToolUse' for e in d['events']) else None)
            if (d := self.cli('inspect', binding)) else None)
        results = [e['detail'] for e in data['events'] if e['type'] == 'PostToolUse']
        self.assertTrue(any('Zerus blocked this command:' in text for text in results), results)
        self.assertFalse(any('guard-should-not-run' in text for text in results), results)
        native = self.rpc('inspect', {'sessionId': binding['conversation_id']})
        self.assertFalse(native['shellCalls'])
        self.assertFalse(native['jobs'])
        self.assertEqual(data.get('processes', {}).get('active_count', 0), 0)

    def test_native_manual_compaction_is_confirmed_before_continuing(self):
        b=self.create()
        self.assertTrue(self.cli('inspect',b)['compact_context_supported'])
        # Native no-op is distinct from a successful summary.
        payload=self.payload(b)
        proc=subprocess.run([str(HGS),'compact-context',b['name'],'--json'],input=json.dumps(payload),env=self.env,text=True,capture_output=True,timeout=15)
        self.assertEqual(proc.returncode,0,proc.stderr)
        data=self.wait(lambda: (d if d.get('compact_context_request',{}).get('status')=='unchanged' else None) if (d:=self.cli('inspect',b)) else None)
        self.assertEqual(data['compact_context_request']['request_id'],payload['request_id'])
        refused=subprocess.run([str(HGS),'send',b['name'],'--json'],input=json.dumps(self.payload(b,text='Must not send',expected_compaction_id=payload['request_id'])),env=self.env,text=True,capture_output=True,timeout=15)
        self.assertNotEqual(refused.returncode,0)
        # Build enough native history for the real compaction engine to summarize.
        for i in range(5):
            self.cli('send',b,self.payload(b,text=f'COMPACT_HISTORY_{i} '+('history detail '*1200),attachments=[]))
            self.wait(lambda:self.cli('inspect',b)['phase']=='idle')
        payload=self.payload(b)
        proc=subprocess.run([str(HGS),'compact-context',b['name'],'--json'],input=json.dumps(payload),env=self.env,text=True,capture_output=True,timeout=15)
        self.assertEqual(proc.returncode,0,proc.stderr)
        data=self.wait(lambda: (d if d.get('compact_context_request',{}).get('status') not in ['compacting','submitted',None] else None) if (d:=self.cli('inspect',b)) else None)
        self.assertEqual(data['compact_context_request']['status'],'completed',data['compact_context_request'])
        sent=self.cli('send',b,self.payload(b,text='Continue with summary',expected_compaction_id=payload['request_id'],attachments=[]))
        self.assertEqual(sent['status'],'submitted')

    def test_sent_image_is_available_from_persistent_attachment_history(self):
        b=self.create()
        before = len(list((self.state/'attachments').rglob('*.png')))
        encoded='iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAADklEQVR4nGP4DwYMEAoAU7oL9ZisIGcAAAAASUVORK5CYII='
        request=self.payload(b,text='Inspect attached test image',attachments=[dict(name='pixel.png',mime='image/png',data_base64=encoded)])
        receipt=self.cli('send',b,request)
        data=self.cli('inspect',b);message=data['attachment_messages'][0]
        self.assertEqual(message['detail'],'Inspect attached test image')
        self.assertEqual(message['attachments'][0]['request_id'],request['request_id'])
        result=subprocess.run([str(HGS),'attachment',b['name'],'--request',request['request_id'],'--index','0','--conversation',b['conversation_id']],env=self.env,capture_output=True,text=True,timeout=5)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual(base64.b64decode(json.loads(result.stdout)['data_base64']),base64.b64decode(encoded))
        self.assertEqual(len(list((self.state/'attachments').rglob('*.png'))),before + 1)
        self.cli('send',b,request)
        self.assertEqual(len(list((self.state/'attachments').rglob('*.png'))),before + 1)

    def test_inline_images_reach_native_model_between_text_segments(self):
        b = self.create()
        encoded = 'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAADklEQVR4nGP4DwYMEAoAU7oL9ZisIGcAAAAASUVORK5CYII='
        text = 'ATTACHMENT_ORDER_FIXTURE second [Image #2] then first [Image #1] end'
        files = [dict(name=f'{i}.png', mime='image/png', reference=f'[Image #{i}]', data_base64=encoded) for i in (1, 2)]
        self.cli('send', b, self.payload(b, text=text, attachments=files))
        reply = self.wait(lambda: next((e['detail'] for e in self.cli('inspect', b)['events'] if e['type'] == 'AgentMessage'), None))
        self.assertEqual(json.loads(reply), ['ATTACHMENT_ORDER_FIXTURE second [Image #2]', 'image', ' then first [Image #1]', 'image', ' end'])
        saved = self.cli('inspect', b)['attachment_messages'][0]
        self.assertEqual(saved['detail'], text)
        self.assertEqual([f['reference'] for f in saved['attachments']], ['[Image #1]', '[Image #2]'])

    def test_native_provider_failure_attention_and_recovery(self):
        b = self.create()
        self.cli('send', b, self.payload(b, text='PROVIDER_CAPACITY_FIXTURE', attachments=[]))
        failure = self.wait(lambda: d if (d := self.cli('inspect', b))['phase'] == 'error' else None)
        self.assertEqual(failure['activity_summary'], 'Model at capacity')
        self.assertIn('at capacity', failure['activity_detail'])
        self.assertTrue(any(e['type'] == 'StopFailure' and 'at capacity' in e['detail'] for e in failure['events']))
        def listed():
            rows = json.loads(subprocess.check_output([str(HGS),'ls','--local','--json'],env=self.env,text=True))['sessions']
            return next(r for r in rows if r['name']==b['name'])
        self.assertEqual(listed()['attention_id'], failure['attention_id'])
        self.assertEqual(listed()['attention_id'], failure['attention_id'])
        self.cli('send', b, self.payload(b, text='Recover with a successful reply', attachments=[]))
        self.wait(lambda: any(e['type']=='AgentMessage' and 'successful reply' in e['detail'] for e in self.cli('inspect',b)['events']))
        self.assertNotEqual(listed()['phase'], 'error')
        self.assertNotIn('provider_error', listed())

    def test_automatic_recovery_retries_native_step_without_another_user_message(self):
        directory = self.state / 'recovery'; directory.mkdir(exist_ok=True)
        policy = directory / 'policy.json'
        policy.write_text(json.dumps(dict(version=1,revision=1,enabled=True,enabled_at=time.time(),service=True,rate_limit=True,network=True,delays=[1,1])))
        log=(self.root/'recovery.log').open('w')
        worker=subprocess.Popen([str(HGS),'recovery','worker'],env=self.env,stdout=log,stderr=log)
        try:
            self.wait(lambda:(directory/'heartbeat.json').exists())
            b=self.create()
            self.cli('send',b,self.payload(b,text='PROVIDER_RECOVER_ONCE_FIXTURE',attachments=[]))
            done=self.wait(lambda: d if (d:=self.cli('inspect',b)).get('recovery',{}).get('state')=='succeeded' else None)
            self.assertEqual(done['recovery']['attempt'],1)
            self.assertEqual(done['recovery']['action'],'retry_request')
            self.assertTrue(done['recovery']['acknowledged'])
            self.assertEqual(len([e for e in done['events'] if e['type']=='UserPromptSubmit']),1)
            self.assertTrue(any(e['type']=='AgentMessage' for e in done['events']))
            # A second episode exhausts its finite budget despite new native error IDs.
            self.cli('send',b,self.payload(b,text='PROVIDER_CAPACITY_FIXTURE',attachments=[]))
            failed=self.wait(lambda: d if (d:=self.cli('inspect',b)).get('recovery',{}).get('state')=='exhausted' else None)
            self.assertEqual(failed['recovery']['attempt'],2)
        except AssertionError as error:
            raise AssertionError(str(error)+'\n'+json.dumps(self.cli('inspect',b),ensure_ascii=False)+'\n'+(self.root/'recovery.log').read_text()) from error
        finally:
            policy.unlink(missing_ok=True);worker.terminate();worker.wait(timeout=5);log.close()

    def test_automatic_recovery_continues_a_final_failure_without_step_handle(self):
        directory=self.state/'recovery';directory.mkdir(exist_ok=True);policy=directory/'policy.json'
        (directory/'heartbeat.json').unlink(missing_ok=True)
        policy.write_text(json.dumps(dict(version=1,revision=1,enabled=True,enabled_at=time.time(),service=True,rate_limit=True,network=True,delays=[1])))
        worker=None;log=(self.root/'recovery-final.log').open('w')
        try:
            b=self.create();self.cli('send',b,self.payload(b,text='PROVIDER_CAPACITY_FIXTURE',attachments=[]))
            self.wait(lambda:self.cli('inspect',b)['phase']=='error')
            worker=subprocess.Popen([str(HGS),'recovery','worker'],env=self.env,stdout=log,stderr=log)
            done=self.wait(lambda:d if (d:=self.cli('inspect',b)).get('recovery',{}).get('state')=='succeeded' else None)
            self.assertEqual(done['recovery']['action'],'continue_message');self.assertEqual(done['recovery']['attempt'],1)
            messages=[e for e in done['events'] if e['type']=='UserPromptSubmit']
            self.assertEqual(len(messages),2);self.assertIn('[HGS automatic recovery]',messages[-1]['detail'])
        finally:
            policy.unlink(missing_ok=True)
            if worker:worker.terminate();worker.wait(timeout=5)
            log.close()

    def test_interrupt_cancels_turn_and_keeps_native_agent(self):
        b=self.create();self.cli('send',b,self.payload(b,text='INTERRUPT_WAIT_FIXTURE',attachments=[]))
        data=self.wait(lambda: d if (d:=self.cli('inspect',b)).get('interrupt_supported') else None)
        request=self.payload(b,expected_turn_started=data['turn_started'])
        result=subprocess.run([str(HGS),'interrupt',b['name'],'--json'],input=json.dumps(request),env=self.env,text=True,capture_output=True,timeout=15)
        self.assertEqual(result.returncode,0,result.stderr)
        done=self.wait(lambda: d if (d:=self.cli('inspect',b))['phase']=='idle' else None)
        self.assertEqual(done['runtime_state'],'live');self.assertEqual(done['run_id'],b['run_id'])
        self.assertFalse(any(e['type']=='AgentMessage' and 'INTERRUPT_WAIT_FIXTURE' in e['detail'] for e in done['events']))

    def test_approval_requires_an_explicit_native_decision(self):
        def journal(operation, payload):
            result = subprocess.run([str(HGS), 'approval-log', operation], input=json.dumps(payload),
                                    env=self.env, text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)
        for option,outcome in [('allow','allowed-once'),('deny','rejected')]:
            b=self.create();self.cli('send',b,self.payload(b,text='APPROVAL',attachments=[]))
            data=self.wait(lambda: d if (d:=self.cli('inspect',b))['pending_questions'] else None)
            question=data['pending_questions'][0]
            self.assertFalse(question['questions'][0]['allow_other'])
            self.assertEqual(data['phase'],'input')
            raw=self.rpc('inspect',{'sessionId':b['conversation_id']})
            self.assertFalse(any(r.get('event',{}).get('type')=='approval/decided' for r in raw['page']['records']))
            answer=self.payload(b,question_id=question['question_id'],expected_question_hash=question['question_hash'],
                answers=[{'question_id':'decision','selected_option_ids':[option],'text':''}])
            if option == 'allow':
                reservation = journal('begin', dict(host='desktop-a', session=b['name'], provider='dsh', question=question))
                self.assertTrue(reservation['eligible']); self.assertTrue(reservation['fresh'])
                self.assertEqual(reservation['answers'], answer['answers'])
                answer['request_id'] = reservation['request_id']
            self.cli('answer',b,answer)
            if option == 'allow':
                journal('finish', dict(id=reservation['id'], request_id=reservation['request_id'], status='approved'))
            def decided():
                raw=self.rpc('inspect',{'sessionId':b['conversation_id']})
                return next((r['event']['data']['outcome'] for r in raw['page']['records'] if r.get('event',{}).get('type')=='approval/decided'),None)
            self.assertEqual(self.wait(decided),outcome)

    def test_account_permissions_use_native_preset_at_start_and_resume(self):
        self.assertTrue(self.rpc('ping')['accountPermissions'])
        def preset(binding):
            return self.rpc('inspect',{'sessionId':binding['conversation_id']})['baseline']['values']['permissions']['currentValue']
        other=self.create();old=preset(other)
        command=[str(HGS),'account','permissions','native-dsh','--mode']
        try:
            subprocess.run(command+['bypass'],env=self.env,check=True,capture_output=True)
            b=self.create();self.cli('pause',b);self.cli('resume',b)
            self.assertEqual(preset(b),'danger-full-access');self.assertEqual(preset(other),old)
            # A later account change must not change a running agent.
            subprocess.run(command+['provider'],env=self.env,check=True,capture_output=True)
            self.rpc('create',{'sessionId':b['conversation_id'],'cwd':str(self.workspace),'permissionMode':'provider'})
            self.assertEqual(preset(b),'danger-full-access')
            sid='session-'+str(uuid.uuid4())
            self.rpc('create',{'sessionId':sid,'cwd':str(self.workspace),'permissionMode':'bypass'})
            self.assertEqual(preset({'conversation_id':sid}),'danger-full-access')
        finally:
            subprocess.run(command+['provider'],env=self.env,check=True,capture_output=True)

    def test_child_activity_requires_exact_parent_and_keeps_public_history(self):
        parent,other=self.create(),self.create()
        self.cli('send',parent,self.payload(parent,text='SUBAGENT',attachments=[]))
        data=self.wait(lambda: d if (d:=self.cli('inspect',parent))['subagents'] else None)
        child=next(iter(data['subagents']))
        def child_events():
            result=subprocess.run([str(HGS),'inspect',parent['name'],'--agent',child],env=self.env,text=True,capture_output=True,timeout=35)
            self.assertEqual(result.returncode,0,result.stderr)
            data=json.loads(result.stdout)
            return data if any(e['type']=='AgentMessage' for e in data['events']) else None
        child_data=self.wait(child_events)
        # Children are continuable: the live parent forwards messages to them.
        self.assertEqual(child_data['child_mode'],'continuable')
        self.assertFalse(child_data['read_only']);self.assertTrue(child_data['send_supported'])
        self.assertEqual(child_data['parent_conversation_id'],parent['conversation_id'])
        self.assertNotIn('PRIVATE_TEST_REASONING',json.dumps(child_data))
        with self.assertRaises(RuntimeError):
            self.rpc('inspect',{'sessionId':other['conversation_id'],'agentId':child})
        done=self.wait(lambda: d if (d:=self.cli('inspect',parent))['subagents'][child]['state']=='finished' else None)
        self.assertEqual(done['subagent_active_count'],0)
        self.assertEqual(done['subagent_completed_count'],1)
        self.assertTrue(done['subagent_counts_complete'])
        # The completed child must stay idle while a new parent turn is running.
        self.cli('send',parent,self.payload(parent,text='INTERRUPT_WAIT_FIXTURE',attachments=[]))
        running=self.wait(lambda: d if (d:=self.cli('inspect',parent))['phase']=='working' else None)
        child_idle=child_events()
        self.assertEqual(child_idle['phase'],'idle')
        self.assertEqual(child_idle['activity'],'idle')
        self.assertEqual(child_idle['tracking_evidence']['native_id'],child)
        self.rpc('interrupt',{'sessionId':parent['conversation_id'],
            'generation':running['host_generation'],'turnStarted':running['turn_started']})

    def test_starting_child_has_no_history_until_its_first_step(self):
        # The engine records a child's descriptor when it creates the child, so a
        # child whose first step the fixture holds already accepts steering.
        parent,other=self.create(),self.create();hold='HOLD_CHILD_'+uuid.uuid4().hex
        def child_inspect(child):
            result=subprocess.run([str(HGS),'inspect',parent['name'],'--agent',child],env=self.env,text=True,capture_output=True,timeout=35)
            self.assertEqual(result.returncode,0,result.stderr);return json.loads(result.stdout)
        self.cli('send',parent,self.payload(parent,text='SUBAGENT '+hold,attachments=[]))
        try:
            data=self.wait(lambda: d if (d:=self.cli('inspect',parent))['subagents'] else None)
            child=next(iter(data['subagents']))
            starting=child_inspect(child)
            self.assertEqual(starting['events'],[]);self.assertTrue(starting['send_supported'])
            self.assertEqual(starting['parent_conversation_id'],parent['conversation_id'])
            working=self.wait(lambda: d if (d:=self.cli('inspect',parent))['subagents'][child]['state']=='working' else None)
            self.assertEqual(working['subagent_active_count'],1)
            self.assertEqual(working['subagent_completed_count'],0)
            with self.assertRaises(RuntimeError):
                self.rpc('inspect',{'sessionId':other['conversation_id'],'agentId':child})
        finally:
            (self.root/'holds'/hold).touch()
        done=self.wait(lambda: next((e for e in child_inspect(child)['events'] if e['type']=='AgentMessage'),None))
        self.assertIn(hold,done['detail'])

    def test_parent_interrupt_leaves_its_child_running(self):
        # Children run independently of the turn that created them: stopping the
        # parent ends its turn, while the child keeps its own work and catalog entry.
        # The fixture keeps the parent's turn waiting so the interrupt meets it.
        parent,other=self.create(),self.create();hold='HOLD_CHILD_'+uuid.uuid4().hex
        self.cli('send',parent,self.payload(parent,text='SUBAGENT '+hold+' INTERRUPT_WAIT_FIXTURE',attachments=[]))
        try:
            self.wait(lambda:(self.root/'holds'/(hold+'.waiting')).exists(),timeout=30)
            running=self.wait(lambda: d if (d:=self.cli('inspect',parent)).get('interrupt_supported') and d['subagents'] else None)
            child=next(iter(running['subagents']))
            self.rpc('interrupt',{'sessionId':parent['conversation_id'],
                'generation':running['host_generation'],'turnStarted':running['turn_started']})
            stopped=self.wait(lambda: d if (d:=self.cli('inspect',parent))['phase']!='working' or not d['interrupt_supported'] else None)
            self.assertEqual(stopped['subagents'][child]['state'],'working')
            self.assertEqual(stopped['subagent_active_count'],1)
            row=next(r for r in self.rpc('list')['items'] if r['sessionId']==child)
            self.assertTrue(row['agentAvailable']);self.assertTrue(row['running'])
            with self.assertRaises(RuntimeError):
                self.rpc('inspect',{'sessionId':other['conversation_id'],'agentId':child})
        finally:
            (self.root/'holds'/hold).touch()
        result=subprocess.run([str(HGS),'inspect',parent['name'],'--agent',child],env=self.env,text=True,capture_output=True,timeout=35)
        self.assertEqual(result.returncode,0,result.stderr)
        done=self.wait(lambda: d if (d:=self.cli('inspect',parent))['subagents'][child]['state']=='finished' else None)
        self.assertEqual(done['subagent_active_count'],0)

    def test_stored_child_with_a_damaged_or_missing_descriptor_stays_corrupt(self):
        # A malformed descriptor, or steps without one, remain native errors.
        for damage in ('CORRUPT','UNRECORDED'):
            with self.subTest(damage=damage):
                parent=self.create();marker=damage+'_CHILD_'+uuid.uuid4().hex
                self.cli('send',parent,self.payload(parent,text='SUBAGENT '+marker,attachments=[]))
                data=self.wait(lambda: d if (d:=self.cli('inspect',parent))['subagents'] else None)
                child=next(iter(data['subagents']))
                self.wait(lambda: any(r['sessionId']==child and not r['agentAvailable'] for r in self.rpc('list')['items']))
                # The child ran its steps and returned its result to the parent.
                records=self.rpc('inspect',{'sessionId':parent['conversation_id']})['page']['records']
                self.assertIn('Native reply: Child result fixture '+marker,json.dumps(records))
                result=subprocess.run([str(HGS),'inspect',parent['name'],'--agent',child],env=self.env,text=True,capture_output=True,timeout=35)
                self.assertNotEqual(result.returncode,0,result.stdout)
                self.assertIn('subagent descriptor is corrupt',result.stderr)
                with self.assertRaisesRegex(RuntimeError,'subagent descriptor is corrupt'):
                    self.rpc('inspect',{'sessionId':parent['conversation_id'],'agentId':child})

    def test_reads_do_not_add_effects_while_a_child_scope_unloads(self):
        # Native disposal unloads a finished child's Cordis scope before the
        # child leaves the live registry; the fixture holds that window open.
        # Nothing reads the child before it, so no earlier read can have
        # installed a model selection that later reads would reuse.
        parent=self.create();hold='DISPOSING_CHILD_'+uuid.uuid4().hex
        text='SUBAGENT '+hold
        self.cli('send',parent,self.payload(parent,text=text,attachments=[]))
        try:
            self.wait(lambda:(self.root/'holds'/(hold+'.disposing')).exists())
            rows=self.rpc('list')['items']
            row=next(r for r in rows if r.get('parentSessionId')==parent['conversation_id'] and r.get('origin')=='subagent')
            child=row['sessionId']
            self.assertTrue(row['agentAvailable']);self.assertFalse(row['running'])
            stored=row['projections']['values']['modelSelection']['next']
            self.assertEqual((stored['provider'],stored['model']),('hgs-test','test-a'))
            self.assertEqual(row['selection'],stored)
            parent_row=next(r for r in rows if r['sessionId']==parent['conversation_id'])
            self.assertEqual((parent_row['selection']['provider'],parent_row['selection']['model']),('hgs-test','test-a'))
            native=self.rpc('inspect',{'sessionId':parent['conversation_id'],'agentId':child})
            self.assertTrue(native['live'])
            self.assertEqual(native['selection'],native['baseline']['values']['modelSelection']['next'])
            inspected=self.cli('inspect',parent)
            self.assertEqual(inspected['model'],'test-a');self.assertIn(child,inspected['subagents'])
            # The child may leave the registry while an inspection
            # awaits the account; it keeps what it read before.
            gate=self.root/'holds'/'ACCOUNT_HOLD';gate.with_name('ACCOUNT_HOLD.waiting').unlink(missing_ok=True);gate.touch()
            try:
                with concurrent.futures.ThreadPoolExecutor(1) as pool:
                    late=pool.submit(self.rpc,'inspect',{'sessionId':parent['conversation_id'],'agentId':child})
                    self.wait(lambda:gate.with_name('ACCOUNT_HOLD.waiting').exists())
                    (self.root/'holds'/hold).touch()
                    self.wait(lambda:(self.root/'holds'/(hold+'.disposed')).exists())
                    gate.unlink();late=late.result(timeout=30)
            finally:
                gate.unlink(missing_ok=True)
            self.assertTrue(late['live']);self.assertEqual(late['selection'],stored)
            self.assertEqual(late['goal'],native['goal'])
        finally:
            (self.root/'holds'/hold).touch()
        self.wait(lambda: any(r['sessionId']==child and not r['agentAvailable'] for r in self.rpc('list')['items']))
        done=self.wait(lambda: d if (d:=self.cli('inspect',parent))['subagents'][child]['state']=='finished' else None)
        self.assertEqual(done['subagent_completed_count'],1)

    def test_reads_leave_a_starting_child_on_its_inherited_model(self):
        # Upstream installs model selection only while composing a top-level
        # agent; a child runs the route it inherits from its parent's request.
        # Reads during its first step, which the fixture holds, must neither
        # install the profile default on the child nor report it.
        parent=self.create();hold='HOLD_CHILD_'+uuid.uuid4().hex
        chosen={'provider':'hgs-test','model':'test-b','reasoningEffort':'off'}
        self.cli('settings',parent,self.payload(parent,model='hgs-test/test-b',effort='off'))
        self.cli('send',parent,self.payload(parent,text='SUBAGENT '+hold,attachments=[]))
        child=None
        def held_child_row():
            nonlocal child
            row=next((r for r in self.rpc('list')['items']
                      if r.get('parentSessionId')==parent['conversation_id'] and r.get('origin')=='subagent'),None)
            if row is None:
                return None
            if child is None:
                child=row['sessionId']
            self.assertEqual(row['sessionId'],child)
            # Registry membership precedes the first turn. Admit only this
            # child's held pre-step and running state under the original wait.
            return row if (self.root/'holds'/(hold+'.waiting')).exists() and row.get('running') is True else None
        try:
            row=self.wait(held_child_row)
            self.assertTrue(row['running'])
            self.assertEqual(row['selection'],chosen)
            native=self.rpc('inspect',{'sessionId':parent['conversation_id'],'agentId':child})
            self.assertTrue(native['live']);self.assertEqual(native['selection'],chosen)
            result=subprocess.run([str(HGS),'inspect',parent['name'],'--agent',child],env=self.env,text=True,capture_output=True,timeout=35)
            self.assertEqual(result.returncode,0,result.stderr)
            shown=json.loads(result.stdout)
            self.assertEqual((shown['model_provider'],shown['model'],shown['effort']),('hgs-test','test-b','off'))
            # The parent keeps its live session selection.
            parent_row=next(r for r in self.rpc('list')['items'] if r['sessionId']==parent['conversation_id'])
            self.assertEqual(parent_row['selection'],chosen)
        finally:
            (self.root/'holds'/hold).touch()
        def used():
            native=self.rpc('inspect',{'sessionId':parent['conversation_id'],'agentId':child})
            return native['baseline']['values'].get('modelSelection',{}).get('lastUsed')
        self.assertEqual(self.wait(used),chosen)
        self.assertEqual(self.cli('inspect',parent)['model'],'test-b')

    def test_send_and_settings_refuse_a_child_without_changing_its_model(self):
        # hgs messages a child only through its parent (send-child). A send or
        # settings request that names a live child is refused before it can
        # install or change the child's selection, which would route its first
        # request, held by the fixture, away from the inherited model.
        parent=self.create();hold='HOLD_CHILD_'+uuid.uuid4().hex
        chosen={'provider':'hgs-test','model':'test-b','reasoningEffort':'off'}
        self.cli('settings',parent,self.payload(parent,model='hgs-test/test-b',effort='off'))
        self.cli('send',parent,self.payload(parent,text='SUBAGENT '+hold,attachments=[]))
        def child_row():
            return next((r for r in self.rpc('list')['items']
                         if r.get('parentSessionId')==parent['conversation_id'] and r.get('origin')=='subagent'),None)
        try:
            # The list can show the child before its Agent is live; a send to it
            # then needs a resume rather than reaching the subagent refusal.
            child=self.wait(lambda: r if (r:=child_row()) and r.get('agentAvailable') else None)['sessionId']
            with self.assertRaisesRegex(RuntimeError,'^Use the parent to manage a subagent$'):
                self.rpc('send',{'sessionId':child,'requestId':str(uuid.uuid4()),'mode':'steer',
                                 'content':[{'type':'text','text':'Direct child send'}]})
            row=child_row()
            self.assertTrue(row['running']);self.assertEqual(row['selection'],chosen)
            with self.assertRaisesRegex(RuntimeError,'^Use the parent to manage a subagent$'):
                self.rpc('settings',{'sessionId':child,'provider':'hgs-test','model':'test-a','reasoningEffort':'high'})
            row=child_row()
            self.assertEqual(row['selection'],chosen)
            self.assertIsNone(row['projections']['values'].get('modelSelection',{}).get('next'))
            native=self.rpc('inspect',{'sessionId':parent['conversation_id'],'agentId':child})
            self.assertTrue(native['live']);self.assertEqual(native['selection'],chosen)
            parent_row=next(r for r in self.rpc('list')['items'] if r['sessionId']==parent['conversation_id'])
            self.assertEqual(parent_row['selection'],chosen)
        finally:
            (self.root/'holds'/hold).touch()
        def used():
            native=self.rpc('inspect',{'sessionId':parent['conversation_id'],'agentId':child})
            return native['baseline']['values'].get('modelSelection',{}).get('lastUsed')
        self.assertEqual(self.wait(used),chosen)
        self.assertNotIn('Direct child send',json.dumps(self.rpc('inspect',{'sessionId':parent['conversation_id'],'agentId':child})['page']))
        # A stored child keeps the refusal it gets when settings would resume it.
        self.wait(lambda: any(r['sessionId']==child and not r['agentAvailable'] for r in self.rpc('list')['items']))
        with self.assertRaisesRegex(RuntimeError,'^Use the parent to manage a subagent$'):
            self.rpc('settings',{'sessionId':child,'provider':'hgs-test','model':'test-a'})
        self.assertEqual(self.cli('inspect',parent)['model'],'test-b')

    def test_compact_and_interrupt_refuse_a_child_without_changing_it(self):
        # hgs compacts and interrupts only a top-level session. Upstream offers
        # no commands for a child and interrupts one only through its parent's
        # address. A compact or interrupt request that names a live child, here
        # held in its first step, is refused before it runs a command on the
        # child, records compaction state or touches goals or the parent's turn.
        # The held child's goal is armed, so a disarm would show. A child no
        # longer holds its parent's turn, so the fixture keeps that turn waiting.
        parent=self.create();hold='HOLD_CHILD_ARMED_'+uuid.uuid4().hex
        self.cli('send',parent,self.payload(parent,text='SUBAGENT '+hold+' INTERRUPT_WAIT_FIXTURE',attachments=[]))
        def child_row():
            return next((r for r in self.rpc('list')['items']
                         if r.get('parentSessionId')==parent['conversation_id'] and r.get('origin')=='subagent'),None)
        def goals():
            return (self.rpc('inspect',{'sessionId':parent['conversation_id']})['goal'],
                    self.rpc('inspect',{'sessionId':parent['conversation_id'],'agentId':child})['goal'])
        request_id=str(uuid.uuid4())
        try:
            child=self.wait(child_row)['sessionId']
            running=self.wait(lambda: d if (d:=self.cli('inspect',parent)).get('interrupt_supported') else None)
            before=self.wait(lambda: g if (g:=goals())[1] and g[1]['activation']=='armed' else None)
            # Each request carries the parent's host generation and current turn.
            with self.assertRaisesRegex(RuntimeError,'^Use the parent to manage a subagent$'):
                self.rpc('compact',{'sessionId':child,'generation':running['host_generation'],'requestId':request_id})
            with self.assertRaisesRegex(RuntimeError,'^Use the parent to manage a subagent$'):
                self.rpc('interrupt',{'sessionId':child,'generation':running['host_generation'],'turnStarted':running['turn_started']})
            self.assertTrue(child_row()['running'])
            self.assertEqual(goals(),before)
            still=self.cli('inspect',parent)
            self.assertTrue(still['interrupt_supported']);self.assertEqual(still['turn_started'],running['turn_started'])
        finally:
            (self.root/'holds'/hold).touch()
        def replied():
            page=self.rpc('inspect',{'sessionId':parent['conversation_id'],'agentId':child})['page']
            return page if 'Native reply: Child result fixture '+hold in json.dumps(page['records']) else None
        self.assertNotIn('command/run',json.dumps(self.wait(replied)))
        # Each of the parent's remaining model calls waits in the fixture.
        self.wait(lambda:self.cli('inspect',parent)['phase']=='idle',timeout=40)
        # The parent keeps both operations, and the refused request ID was not consumed.
        compacted=subprocess.run([str(HGS),'compact-context',parent['name'],'--json'],input=json.dumps(self.payload(parent,request_id=request_id)),
                                 env=self.env,text=True,capture_output=True,timeout=15)
        self.assertEqual(compacted.returncode,0,compacted.stderr)
        done=self.wait(lambda: d if (d:=self.cli('inspect',parent)).get('compact_context_request',{}).get('status') not in ['compacting','submitted',None] else None)
        self.assertEqual(done['compact_context_request']['request_id'],request_id)
        self.assertIn(done['compact_context_request']['status'],['unchanged','completed'])
        self.cli('send',parent,self.payload(parent,text='INTERRUPT_WAIT_FIXTURE',attachments=[]))
        busy=self.wait(lambda: d if (d:=self.cli('inspect',parent)).get('interrupt_supported') else None)
        interrupted=subprocess.run([str(HGS),'interrupt',parent['name'],'--json'],input=json.dumps(self.payload(parent,expected_turn_started=busy['turn_started'])),
                                   env=self.env,text=True,capture_output=True,timeout=15)
        self.assertEqual(interrupted.returncode,0,interrupted.stderr)
        self.wait(lambda:self.cli('inspect',parent)['phase']=='idle')

    def test_send_reads_an_uncomposed_root_without_installing_a_selection(self):
        # A webhook root runs its creation route without the model selection
        # upstream installs while composing a session. The send sign-in check
        # only reads one: an installed selection would route the request to
        # the profile default. A model change remains a top-level setting.
        parent=self.create();root='webhook-'+str(uuid.uuid4())
        route={'provider':'hgs-test','model':'test-b'}
        self.cli('send',parent,self.payload(parent,text='UNCOMPOSED_ROOT_FIXTURE '+root,attachments=[]))
        row=self.wait(lambda: next((r for r in self.rpc('list')['items'] if r['sessionId']==root),None))
        self.assertIsNone(row.get('parentSessionId'));self.assertEqual(row['selection'],route)
        self.rpc('send',{'sessionId':root,'requestId':str(uuid.uuid4()),'mode':'steer',
                         'content':[{'type':'text','text':'Uncomposed root message'}]})
        def replied():
            native=self.rpc('inspect',{'sessionId':root})
            return native if 'Native reply: Uncomposed root message' in json.dumps(native['page']['records']) else None
        used=self.wait(replied)['baseline']['values']['modelSelection']['lastUsed']
        self.assertEqual({k:v for k,v in used.items() if k!='reasoningEffort'},route)
        chosen={'provider':'hgs-test','model':'test-a','reasoningEffort':'off'}
        self.assertEqual(self.rpc('settings',{'sessionId':root,**chosen})['selected'],chosen)
        self.assertEqual(next(r for r in self.rpc('list')['items'] if r['sessionId']==root)['selection'],chosen)

    def test_continuable_child_send_is_addressed_and_idempotent(self):
        parent, other = self.create(), self.create()
        self.cli('send', parent, self.payload(parent, text='SUBAGENT', attachments=[]))
        data = self.wait(lambda: d if (d := self.cli('inspect', parent))['subagents'] else None)
        child = next(iter(data['subagents']))
        self.assertEqual(data['subagents'][child]['mode'], 'continuable')
        payload = self.payload(parent, text='Direct child followup', attachments=[], agent_id=child)
        receipt = self.cli('send', parent, payload)
        self.assertEqual(receipt['agent_id'], child)
        repeated = self.cli('send', parent, payload)
        self.assertAlmostEqual(repeated.pop('at'), receipt.pop('at'), places=4)
        self.assertEqual(repeated, receipt)
        def received():
            native = self.rpc('inspect', {'sessionId': parent['conversation_id'], 'agentId': child})
            return native if 'Native reply: Direct child followup' in json.dumps(native['page']['records']) else None
        self.assertTrue(self.wait(received))
        root = self.rpc('inspect', {'sessionId': parent['conversation_id']})
        human = [r for r in root['page']['records'] if r.get('event', {}).get('type') == 'user/message' and r['event']['data'].get('source', {}).get('kind') == 'user']
        self.assertNotIn('Direct child followup', json.dumps(human))
        with self.assertRaises(RuntimeError):
            self.rpc('send-child', {'parentSessionId': other['conversation_id'], 'childSessionId': child,
                'requestId': str(uuid.uuid4()), 'mode': 'continuable', 'delivery': 'steer',
                'content': [{'type': 'text', 'text': 'wrong parent'}]})

    def test_real_cli_startup_and_native_browser_authentication(self):
        with tempfile.TemporaryDirectory(prefix='hgs-dsh-launch-', dir='/tmp') as directory:
            root=Path(directory); home=root/'user';home.mkdir();workspace=root/'project';workspace.mkdir()
            env={**self.env,'HOME':str(home),'DSH_HOME':str(root/'native'),'HGS_STATE_DIR':str(root/'state'),'HGS_CONFIG_DIR':str(root/'config')}
            # Match a desktop service, which does not source the user's shell rc.
            npm_bin=home/'.npm-global/bin';npm_bin.mkdir(parents=True)
            (npm_bin/'dsh').symlink_to(Path(DSH).resolve())
            node=shutil.which('node');self.assertIsNotNone(node)
            env['PATH']=os.pathsep.join(['/usr/bin','/bin',str(Path(node).parent)])
            def run(*args):
                result=subprocess.run([str(HGS),*args],env=env,cwd=workspace,text=True,capture_output=True,timeout=45)
                self.assertEqual(result.returncode,0,result.stderr);return result.stdout
            owner_unit=None
            try:
                if sys.platform=='linux' and shutil.which('systemd-run') and Path(f'/run/user/{os.getuid()}/systemd/private').exists():
                    # Match a GUI service's lifetime: setsid alone does not
                    # escape its control group when the service is restarted.
                    owner_unit='hgs-dsh-test-owner-'+str(uuid.uuid4())[:12]
                    result=subprocess.run(['systemd-run','--user','--quiet','--collect','--unit',owner_unit,
                        '--property=Type=oneshot','--property=RemainAfterExit=yes','--',
                        '/usr/bin/env','-i',*[k+'='+v for k,v in env.items()],str(HGS),
                        'dsh',str(workspace),'--new','-n','startup'],text=True,capture_output=True,timeout=45)
                    self.assertEqual(result.returncode,0,result.stderr)
                else:
                    run('dsh',str(workspace),'--new','-n','startup')
                inspection=json.loads(run('inspect','dsh/project/startup'))
                self.assertEqual(inspection['runtime_state'],'live')
                self.assertEqual(inspection['cwd'],str(workspace.resolve()))
                # The default API-key route is independent of native account
                # login. An empty session has not submitted a model request.
                self.assertEqual(inspection['model_provider'],'deepseek-official')
                self.assertEqual(inspection['account_status'],'signed-out')
                self.assertFalse(inspection['auth_required'])
                self.assertTrue(inspection['send_available'])
                self.assertEqual(inspection['events'],[])
                if owner_unit:
                    subprocess.run(['systemctl','--user','stop',owner_unit],check=True,timeout=15)
                    self.assertEqual(json.loads(run('inspect','dsh/project/startup'))['runtime_state'],'live')
                url=json.loads(run('native-ui','dsh/project/startup','--json'))['url']
                self.assertEqual(url,json.loads(run('__dsh-ui-url','dsh/project/startup'))['url'])
                cookies=http.cookiejar.CookieJar()
                opener=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cookies))
                response=opener.open(url,timeout=10)
                self.assertEqual(response.status,200)
                self.assertTrue(list(cookies),'Native launch URL must establish the normal browser cookie')
                self.assertNotIn('token=',response.url)
                run('pause','dsh/project/startup')
                run('resume','dsh/project/startup')
                self.assertEqual(json.loads(run('inspect','dsh/project/startup'))['conversation_id'],inspection['conversation_id'])
                # A cold restart reuses the native profile rather than trying
                # to create it a second time with --from-default-profile.
                pidfile=next((root/'state/dsh/hosts').glob('*/host.pid'))
                os.kill(int(pidfile.read_text()),signal.SIGTERM)
                sock=pidfile.parent/'host.sock'
                for _ in range(100):
                    try:
                        with socket.socket(socket.AF_UNIX) as probe: probe.connect(str(sock))
                    except OSError: break
                    time.sleep(.05)
                else: self.fail('Isolated native host did not stop')
                run('resume','dsh/project/startup')
                restarted=json.loads(run('inspect','dsh/project/startup'))
                self.assertEqual(restarted['conversation_id'],inspection['conversation_id'])
                self.assertEqual(restarted['runtime_state'],'live')
            finally:
                if owner_unit:
                    subprocess.run(['systemctl','--user','stop',owner_unit],capture_output=True,timeout=15)
                for file in (root/'state/dsh/hosts').glob('*/host.pid'):
                    pid=int(file.read_text())
                    try:os.kill(pid,signal.SIGTERM)
                    except ProcessLookupError:pass

    def test_legacy_workspace_handoff_preserves_live_and_paused_agents(self):
        binding = self.create()
        url = re.findall(r'http://127\.0\.0\.1:\d+/\?token=[^\s]+',
                         (self.root / 'host.log').read_text())[-1]
        payload = json.dumps({'url': url, 'sessionId': binding['conversation_id'],
                              'cwd': str(self.workspace)})
        script = (REPO / 'src/dsh/ui-compat.mjs').read_text()
        for paused in (False, True):
            if paused:
                self.cli('pause', binding)
            before = self.rpc('inspect', {'sessionId': binding['conversation_id']})
            result = subprocess.run(['node', '--input-type=module', '-e', script],
                                    input=payload, env=self.env, text=True,
                                    capture_output=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, '')
            after = self.rpc('inspect', {'sessionId': binding['conversation_id']})
            self.assertEqual(after['live'], not paused)
            self.assertEqual(after['generation'], before['generation'])
            self.assertEqual(after['selection'], before['selection'])
            self.assertEqual(after['page']['records'], before['page']['records'])

    def test_model_settings_do_not_change_default_or_other_session(self):
        a, b = self.create(), self.create()
        before = self.rpc('catalog')['default']
        self.assertEqual(self.cli('inspect', a)['effort'], 'high')
        request = self.payload(a, model='hgs-test/test-b', effort='off')
        self.assertEqual(self.cli('settings', a, request)['scope'], 'session')
        self.assertEqual(self.cli('inspect', a)['model'], 'test-b')
        self.assertEqual(self.cli('inspect', b)['model'], 'test-a')
        self.assertEqual(self.rpc('catalog')['default'], before)

    def test_pause_resume_preserves_identity_and_other_agent(self):
        a, b = self.create(), self.create()
        self.cli('pause', a)
        workspace=self.rpc('workspace',{'sessionId':a['conversation_id']})
        self.assertIn(a['conversation_id'],workspace['sessionIds'])
        self.assertIn(b['conversation_id'],workspace['sessionIds'])
        self.assertFalse(self.rpc('inspect', {'sessionId': a['conversation_id']})['live'])
        self.assertTrue(self.rpc('inspect', {'sessionId': b['conversation_id']})['live'])
        self.cli('resume', a)
        self.assertEqual(self.cli('inspect', a)['conversation_id'], a['conversation_id'])
        self.assertTrue(self.rpc('inspect', {'sessionId': a['conversation_id']})['live'])

    def test_question_answer_is_bound_to_current_native_request(self):
        b = self.create()
        self.cli('send', b, self.payload(b, text='QUESTION', attachments=[]))
        data = self.wait(lambda: d if (d := self.cli('inspect', b))['pending_questions'] else None)
        question = data['pending_questions'][0]
        answer = self.payload(b, question_id=question['question_id'], expected_question_hash=question['question_hash'],
                              answers=[{'question_id':'choice','selected_option_ids':['1'],'text':''}])
        self.assertEqual(self.cli('answer', b, answer)['status'], 'answered')
        self.wait(lambda: not self.cli('inspect', b)['pending_questions'])
        with self.assertRaises(RuntimeError):
            self.rpc('answer', {'sessionId':b['conversation_id'],'id':question['question_id'],
                                'generation':data['host_generation'],'answers':[]})

    def hgs(self, *args, request=None):
        return subprocess.run([str(HGS), *args], input=json.dumps(request) if request is not None else None,
                              env=self.env, cwd=REPO, text=True, capture_output=True, timeout=35)

    def bindings(self):
        return {b['name']: b for b in (json.loads(p.read_text()) for p in (self.state / 'dsh/bindings').glob('*.json'))}

    def test_native_fork_binds_the_exact_new_session_and_keeps_the_source(self):
        source = self.create()
        tag = 'fork-' + uuid.uuid4().hex[:8]
        # Native fork copies a completed turn; an empty conversation has none.
        empty = self.cli('inspect', source)
        self.assertFalse(empty['fork_supported'])
        self.assertEqual(empty['fork_reason'], 'Wait for a completed reply before forking')
        self.assertNotIn('fork', empty['allowed_actions'])
        refused = self.hgs('fork', source['name'], '-n', tag, '-d')
        self.assertNotEqual(refused.returncode, 0)
        self.assertNotIn(source['name'].rsplit('/', 1)[0] + '/' + tag, self.bindings())
        self.cli('send', source, self.payload(source, text='FORK_SOURCE_MARKER', attachments=[]))
        ready = self.wait(lambda: d if (d := self.cli('inspect', source))['fork_supported'] else None)
        self.assertEqual(ready['fork_reason'], '')
        self.assertIn('fork', ready['allowed_actions'])
        stale = self.hgs('fork', source['name'], '-n', tag, '-d', '--expected-run-id', 'old-run')
        self.assertNotEqual(stale.returncode, 0)
        forked = self.hgs('fork', source['name'], '-n', tag, '-d', '--expected-run-id', source['run_id'],
                          '--expected-conversation-id', source['conversation_id'])
        self.assertEqual(forked.returncode, 0, forked.stderr)
        name = source['name'].rsplit('/', 1)[0] + '/' + tag
        self.assertEqual(forked.stdout.strip(), f"hgs: forked {source['name']} as {name}")
        bindings = self.bindings()
        fork = bindings[name]
        self.assertEqual(bindings[source['name']], source)
        self.assertTrue(fork['conversation_id'].startswith('session-'))
        self.assertNotEqual(fork['conversation_id'], source['conversation_id'])
        for key in ('agent_home', 'cwd'):
            self.assertEqual(fork[key], source[key])
        self.assertEqual((fork['fork_parent_id'], fork['fork_source_name']), (source['conversation_id'], source['name']))
        self.assertFalse(list((self.state / 'dsh/forks').glob('*.json')))
        row = next(r for r in self.rpc('list')['items'] if r['sessionId'] == fork['conversation_id'])
        self.assertEqual(row['parentSessionId'], source['conversation_id'])
        self.assertEqual(row['projections']['values']['title'], tag)
        # The fork starts from the source's history and then diverges.
        data = self.cli('inspect', fork)
        self.assertEqual((data['name'], data['conversation_id'], data['run_id']), (name, fork['conversation_id'], fork['run_id']))
        self.assertTrue(any(e['type'] == 'UserPromptSubmit' and e['detail'] == 'FORK_SOURCE_MARKER' for e in data['events']))
        self.assertEqual(data['fork_parent_id'], source['conversation_id'])
        # The native engine owns the forked live agent until its host restarts.
        self.assertEqual(data['runtime_state'], 'live')
        self.assertNotIn('pause', data['allowed_actions'])
        self.cli('send', fork, self.payload(fork, text='FORK_ONLY_MARKER', attachments=[]))
        self.wait(lambda: any(e['type'] == 'AgentMessage' and 'FORK_ONLY_MARKER' in e['detail'] for e in self.cli('inspect', fork)['events']))
        self.assertFalse(any('FORK_ONLY_MARKER' in e['detail'] for e in self.cli('inspect', source)['events']))
        duplicate = self.hgs('fork', source['name'], '-n', tag, '-d')
        self.assertNotEqual(duplicate.returncode, 0)
        self.assertIn('already uses this name', duplicate.stderr)
        # The scoped mobile action reports the exact new identity it created.
        request = {'request_id': str(uuid.uuid4()), 'action': 'fork', 'expected_run_id': source['run_id'],
                   'expected_conversation_id': source['conversation_id'], 'tag': tag + '-scoped'}
        scoped = self.hgs('session-action', source['name'], '--json', request=request)
        self.assertEqual(scoped.returncode, 0, scoped.stderr)
        receipt = json.loads(scoped.stdout)
        self.assertEqual(receipt['status'], 'completed', receipt)
        target = self.bindings()[name + '-scoped']
        self.assertEqual(receipt['result_target'], {'name': target['name'], 'run_id': target['run_id'],
                                                    'conversation_id': target['conversation_id']})
        self.assertEqual(self.bindings()[source['name']], source)

    def test_native_fork_keeps_the_source_permission_preset(self):
        # The fork replays the source's permission events; Zerus applies none.
        def preset(sid):
            return self.rpc('inspect', {'sessionId': sid})['baseline']['values']['permissions']['currentValue']
        provider = self.create()
        bypass = self.create(permissionMode='bypass')
        self.assertEqual(preset(bypass['conversation_id']), 'danger-full-access')
        self.assertNotEqual(preset(provider['conversation_id']), 'danger-full-access')
        for source in (provider, bypass):
            self.cli('send', source, self.payload(source, text='FORK_PERMISSION_MARKER', attachments=[]))
            self.wait(lambda: self.cli('inspect', source)['fork_supported'])
            tag = 'perm-' + uuid.uuid4().hex[:8]
            forked = self.hgs('fork', source['name'], '-n', tag, '-d')
            self.assertEqual(forked.returncode, 0, forked.stderr)
            fork = self.bindings()[source['name'].rsplit('/', 1)[0] + '/' + tag]
            self.assertEqual(preset(fork['conversation_id']), preset(source['conversation_id']))

    def test_history_search_returns_exact_public_deepseek_messages(self):
        needle = 'NEEDLE' + uuid.uuid4().hex[:8]
        target, other = self.create(), self.create()
        self.cli('send', target, self.payload(target, text=f'Find {needle} here', attachments=[]))
        self.cli('send', other, self.payload(other, text='Nothing to find', attachments=[]))
        for b in (target, other):
            self.wait(lambda: any(e['type'] == 'AgentMessage' for e in self.cli('inspect', b)['events']))
        result = self.hgs('search', '--query', needle.lower())
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        hits = data['results']
        self.assertEqual({(h['name'], h['run_id'], h['conversation_id'], h['agent']) for h in hits},
                         {(target['name'], target['run_id'], target['conversation_id'], 'dsh')})
        self.assertEqual(sorted(h['role'] for h in hits), ['assistant', 'user'])
        user = next(h for h in hits if h['role'] == 'user')
        self.assertEqual(user['content'], f'Find {needle} here')
        self.assertEqual(user['event']['source'], 'dsh_native')
        self.assertGreater(user['at'], 0)
        self.assertNotIn('PRIVATE_TEST_REASONING', result.stdout)
        self.assertEqual(data['unavailable_sessions'], 0)
        # Context the harness injects as user messages is not the person's.
        injected = json.loads(self.hgs('search', '--query', 'Current runtime context').stdout)
        self.assertFalse([h for h in injected['results'] if h['agent'] == 'dsh'])

    def test_schedules_refuse_without_the_native_bundle(self):
        b = self.create()
        self.assertFalse(self.rpc('ping')['schedule'])
        result = self.hgs('schedules', b['name'])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Native Schedule is not enabled', result.stderr)
        rows = json.loads(self.hgs('ls', '--local', '--json').stdout)['sessions']
        self.assertNotIn('schedules', next(r for r in rows if r['name'] == b['name']))


@unittest.skipUnless(fixture.required() or DSH and HGS.exists(), 'official dsh and built hgs required')
class NativeSchedules(unittest.TestCase):
    """The official optional Schedule bundle, managed per exact session."""
    SCHEDULE_BUNDLE = True
    setUpClass = NativeHarness.__dict__['setUpClass']
    tearDownClass = NativeHarness.__dict__['tearDownClass']
    rpc = NativeHarness.__dict__['rpc']
    create = NativeHarness.create
    cli = NativeHarness.cli
    payload = NativeHarness.payload
    wait = NativeHarness.wait
    hgs = NativeHarness.hgs

    def test_schedule_list_edit_and_delete_are_bound_to_one_session(self):
        b, other = self.create(), self.create()
        self.cli('send', b, self.payload(b, text='SCHEDULE_FIXTURE', attachments=[]))
        listed = self.wait(lambda: d if (d := json.loads(self.hgs('schedules', b['name']).stdout))['items'] else None)
        self.assertEqual((listed['name'], listed['run_id'], listed['conversation_id']), (b['name'], b['run_id'], b['conversation_id']))
        [item] = listed['items']
        self.assertEqual((item['title'], item['kind'], item['status'], item['recurring']), ('Fixture check', 'every', 'active', True))
        self.assertGreater(item['scheduled_at'], time.time())
        self.assertEqual(json.loads(self.hgs('schedules', other['name']).stdout)['items'], [])
        row = self.wait(lambda: next((r for r in json.loads(self.hgs('ls', '--local', '--json').stdout)['sessions']
                                      if r['name'] == b['name'] and r.get('schedules', {}).get('count')), None))
        self.assertEqual(row['schedules'], {'count': 1, 'recurring': 1, 'next_at': item['scheduled_at']})
        def change(binding, **fields):
            fields = {'expected_generation': listed['generation'], 'id': item['id'], **fields}
            return self.hgs('schedules', binding['name'], '--json', request=self.payload(binding, **fields))
        stale = change(b, action='update', title='Old host', expected_generation='old-generation')
        self.assertNotEqual(stale.returncode, 0)
        renamed = change(b, action='update', title='Renamed check', expected=item['record'])
        self.assertEqual(renamed.returncode, 0, renamed.stderr)
        self.assertTrue(json.loads(renamed.stdout)['result']['updated'])
        conflict = json.loads(change(b, action='update', title='Conflict', expected=item['record']).stdout)
        self.assertEqual(conflict['result']['code'], 'schedule_conflict')
        history = json.loads(change(b, action='history', limit=10).stdout)['result']
        self.assertEqual(history['records'], [])
        # Another session cannot reach this task through its own identity.
        foreign = json.loads(change(other, action='delete').stdout)['result']
        self.assertEqual((foreign['deleted'], foreign['code']), (False, 'schedule_not_found'))
        self.assertEqual([i['title'] for i in json.loads(self.hgs('schedules', b['name']).stdout)['items']], ['Renamed check'])
        deleted = json.loads(change(b, action='delete').stdout)['result']
        self.assertTrue(deleted['deleted'])
        self.assertEqual(json.loads(self.hgs('schedules', b['name']).stdout)['items'], [])
        self.wait(lambda: next(r for r in json.loads(self.hgs('ls', '--local', '--json').stdout)['sessions']
                               if r['name'] == b['name'])['schedules']['count'] == 0)


if __name__ == '__main__':
    unittest.main()
