"""Durable worker and terminal delivery contracts, with private fake agents only."""
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
import uuid
from pathlib import Path
import test_input as fixtures

class Recovery(unittest.TestCase):
    setUp=fixtures.InputTransport.setUp
    tearDown=fixtures.InputTransport.tearDown
    script=fixtures.InputTransport.script
    tmux=fixtures.InputTransport.tmux
    wait_for=fixtures.InputTransport.wait_for
    write_record=fixtures.InputTransport.write_record
    screen=fixtures.InputTransport.screen
    received=fixtures.InputTransport.received
    claude_composer=fixtures.InputTransport.claude_composer

    def command(self,*args,payload=None,ok=True):
        result=subprocess.run([str(fixtures.HGS),*args],env=self.env,text=True,capture_output=True,
                              input=json.dumps(payload) if payload is not None else None,timeout=15)
        if ok:self.assertEqual(result.returncode,0,result.stderr)
        else:self.assertNotEqual(result.returncode,0)
        return json.loads(result.stdout) if result.stdout.strip() else result.stderr

    def prepare(self,agent='codex'):
        self.record.update(agent=agent,phase='error',activity='attention',recovery_user_at=1,
            provider_error=dict(type='StopFailure',source='provider_hook',message_id='failure',at=time.time(),detail='503 Service unavailable',error_kind='provider'))
        self.write_record()
        # Claude shows a paste in its composer and records the submitted row; hgs
        # counts a Claude delivery only on that native proof (zerus-iitw8).
        if agent=='claude':self.claude_composer();self.screen('\x1b[2J\x1b[H❯ ','2:0')
        if agent=='kimi':self.screen('\x1b[2J\x1b[H ╭──────────────╮\r\n │ >          │\r\n ╰──────────────╯\x1b[2;6H','5:1')
        directory=self.state/'recovery';directory.mkdir(exist_ok=True)
        self.policy=directory/'policy.json'
        self.policy.write_text(json.dumps(dict(version=1,revision=1,enabled=True,enabled_at=1,service=True,rate_limit=True,network=True,delays=[15,30,60,300])))

    def inspect(self):return self.command('inspect',self.name)
    def tick(self):self.command('recovery','tick')
    def hook(self,kind,**values):
        result=subprocess.run([str(fixtures.HGS),'__state','hook'],env=dict(self.env,HGS_SESSION=self.name,HGS_RUN_ID=self.run_id),
            input=json.dumps(dict(hook_event_name=kind,session_id=self.conversation_id,**values)),text=True,capture_output=True,timeout=10)
        self.assertEqual(result.returncode,0,result.stderr)

    def exercise(self,agent):
        self.prepare(agent);self.tick();job=self.inspect()['recovery'];self.assertEqual(job['state'],'waiting')
        self.assertEqual(self.received(),b'')
        self.command('recovery','action',payload=dict(name=self.name,id=job['id'],action='now'))
        self.tick();job=self.inspect()['recovery'];self.assertEqual(job['state'],'retrying',job)
        self.assertEqual(job['attempt'],1)
        self.wait_for(lambda:self.received().endswith(b'\r'))
        raw=self.received();self.assertEqual(raw.count(b'\r'),1)
        message=job['receipt']['text'];self.assertIn('[HGS automatic recovery]',message)
        self.hook('UserPromptSubmit',prompt=message);self.tick()
        self.assertTrue(self.inspect()['recovery']['acknowledged'])
        if agent=='codex':
            import sqlite3
            home=self.root/'.codex';home.mkdir(exist_ok=True);at=time.time()
            with sqlite3.connect(home/'logs_2.sqlite') as db:
                db.execute('CREATE TABLE logs(id INTEGER, ts INTEGER, ts_nanos INTEGER, thread_id TEXT, target TEXT, feedback_log_body TEXT)')
                db.execute('INSERT INTO logs VALUES(1,?,?,?,?,?)',(int(at),int((at%1)*1e9),self.conversation_id,'codex_core::session::turn','run_turn: Turn error: 529 Overloaded'))
        else:self.hook('StopFailure',error_message='529 Overloaded',error_type='overloaded_error')
        self.tick()
        following=self.inspect()['recovery'];self.assertEqual(following['state'],'waiting');self.assertEqual(following['attempt'],1)
        self.assertEqual(self.received(),raw)
        self.command('recovery','action',payload=dict(name=self.name,id=job['id'],action='cancel'))
        self.tick();self.assertEqual(self.inspect()['recovery']['state'],'cancelled');self.assertEqual(self.received(),raw)

    def test_rate_limit_hooks_while_the_agent_keeps_working_never_restart_the_loop(self):
        # A rate-limit hook can arrive right after the agent accepts the
        # continuation while the agent keeps working and later answers.
        self.prepare('claude')
        self.record['provider_error'].update(detail='429 Too many requests',error_kind='rate_limit');self.write_record()
        policy=json.loads(self.policy.read_text());policy.update(version=3,schedules=dict(rate_limit=[15,30,60,300,0]))
        self.policy.write_text(json.dumps(policy))
        self.tick();job=self.inspect()['recovery'];self.assertEqual(job['class'],'rate_limit')
        self.command('recovery','action',payload=dict(name=self.name,id=job['id'],action='now'))
        self.tick();self.wait_for(lambda:self.received().endswith(b'\r'));raw=self.received()
        sent=self.inspect()['recovery'];self.assertEqual(sent['state'],'retrying')
        self.hook('UserPromptSubmit',prompt=sent['receipt']['text'])
        self.hook('StopFailure',error='rate_limit',error_details='429 Too many requests')
        self.tick();failed=self.inspect()['recovery'];self.assertEqual(failed['state'],'waiting')
        # Even a retry that is already due never reaches a working agent.
        self.command('recovery','action',payload=dict(name=self.name,id=job['id'],action='now'))
        self.hook('PreToolUse',tool_name='Bash',tool_use_id='toolu_fixture',tool_input=dict(command='true'))
        self.tick();working=self.inspect()
        self.assertEqual(working['phase'],'tool');self.assertEqual(working['recovery']['state'],'retrying',working['recovery'])
        self.hook('StopFailure',error='rate_limit',error_details='429 Too many requests');self.tick()
        self.hook('PostToolUse',tool_name='Bash',tool_use_id='toolu_fixture');self.tick()
        current=self.inspect()['recovery'];self.assertEqual(current['state'],'retrying',current)
        self.assertEqual((current['attempt'],current['attempts']),(1,dict(rate_limit=1)))
        self.hook('Stop',last_assistant_message='Fixture answer')
        for _ in range(2):
            self.tick();finished=self.inspect()['recovery']
            self.assertEqual((finished['state'],finished['attempt']),('succeeded',1),finished)
        self.assertEqual(self.received(),raw);self.assertEqual(raw.count(b'[HGS automatic recovery]'),1)

    def test_codex_delivery_acknowledgement_and_cancel(self):self.exercise('codex')
    def test_claude_delivery_acknowledgement_and_cancel(self):self.exercise('claude')
    def test_kimi_delivery_acknowledgement_and_cancel(self):self.exercise('kimi')

    def pending_codex_with_age(self, age):
        self.prepare('codex');self.tick();job=self.inspect()['recovery']
        self.command('recovery','action',payload=dict(name=self.name,id=job['id'],action='now'))
        self.tick();self.wait_for(lambda:self.received().endswith(b'\r'))
        path=next((self.state/'recovery/jobs').glob('*.json'))
        job=json.loads(path.read_text());sent=time.time()-age
        # Supplied fixture time: no real381s sleep and no extra delivery.
        job.update(started_at=sent-1,sent_at=sent,due_at=sent)
        job['receipt']['submitted_at']=sent+.01
        path.write_text(json.dumps(job))
        return job,path

    def test_submitted_codex_waits_for_late_native_hook_without_resending(self):
        job,_=self.pending_codex_with_age(381);received=self.received()
        self.assertEqual(received.count(b'\r'),1)
        for _ in range(3):
            self.tick();pending=self.inspect()['recovery']
            self.assertEqual(pending['state'],'retrying');self.assertFalse(pending['acknowledged'])
            for key in ('id','request_id','attempt','attempts'):self.assertEqual(pending[key],job[key])
            self.assertEqual(self.received(),received)
        self.hook('UserPromptSubmit',prompt=job['receipt']['submitted_text']);self.tick()
        self.assertTrue(self.inspect()['recovery']['acknowledged'])
        import sqlite3
        home=self.root/'.codex';home.mkdir(exist_ok=True);at=time.time()
        with sqlite3.connect(home/'logs_2.sqlite') as db:
            db.execute('CREATE TABLE logs(id INTEGER, ts INTEGER, ts_nanos INTEGER, thread_id TEXT, target TEXT, feedback_log_body TEXT)')
            db.execute('INSERT INTO logs VALUES(1,?,?,?,?,?)',(int(at),int((at%1)*1e9),self.conversation_id,
                'codex_core::session::turn','run_turn: Turn error: Selected model is at capacity. Please try a different model.'))
        self.tick();following=self.inspect()['recovery']
        self.assertEqual(following['state'],'waiting');self.assertTrue(following['acknowledged'])
        for key in ('id','attempt','attempts','class_attempt'):self.assertEqual(following[key],job[key])
        self.assertGreaterEqual(following['due_at'],at+30)
        self.assertEqual(self.received(),received)

    def test_pending_codex_ack_cancels_on_new_submitted_input(self):
        job,_=self.pending_codex_with_age(381);self.tick()
        self.assertEqual(self.inspect()['recovery']['state'],'retrying')
        # A native Terminal message consumes pending input. The next actual
        # scoped send must run cancel_for_input before any later old marker.
        self.hook('UserPromptSubmit',prompt='An explicit new task')
        self.command('send',self.name,'--json',payload=dict(request_id=str(uuid.uuid4()),
            expected_run_id=self.run_id,expected_conversation_id=self.conversation_id,text='Follow the new task'))
        self.wait_for(lambda:self.received().count(b'\r')==2)
        self.assertEqual(self.inspect()['recovery']['state'],'cancelled')
        received=self.received()
        self.hook('UserPromptSubmit',prompt=job['receipt']['submitted_text']);self.tick()
        self.assertEqual(self.inspect()['recovery']['state'],'cancelled')
        self.assertEqual(self.received(),received)

    def test_pending_codex_ack_stays_cancelled_after_policy_disable_and_reenable(self):
        import fcntl
        job,_=self.pending_codex_with_age(381);self.tick()
        self.assertEqual(self.inspect()['recovery']['state'],'retrying')
        # Hold only the fixture executor lock so policy refresh never spawns a
        # detached worker or contacts an owner service manager on either Unix.
        self.assertFalse((self.root/'.config/systemd/user/hgs-recovery.service').exists())
        with (self.state/'recovery/worker.lock').open('a') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            policy=json.loads(self.policy.read_text());policy['service']=False
            disabled=self.command('recovery','set',payload=policy)['policy']
            self.assertEqual(self.inspect()['recovery']['state'],'cancelled')
            disabled['service']=True
            self.command('recovery','set',payload=disabled)
            self.hook('UserPromptSubmit',prompt=job['receipt']['submitted_text']);self.tick()
            self.assertEqual(self.inspect()['recovery']['state'],'cancelled')

    def test_pending_codex_ack_expiry_releases_without_another_delivery(self):
        job,_=self.pending_codex_with_age(601);received=self.received()
        self.tick();expired=self.inspect()['recovery']
        self.assertEqual(expired['state'],'uncertain');self.assertEqual(expired['attempt'],1)
        self.hook('UserPromptSubmit',prompt=job['receipt']['submitted_text']);self.tick()
        self.assertEqual(self.inspect()['recovery']['state'],'uncertain')
        self.assertEqual(self.received(),received)

    def test_pending_codex_ack_serializes_the_account_until_finite_expiry(self):
        job,path=self.pending_codex_with_age(381);received=self.received()
        other=self.root/'second-agent';other.mkdir()
        name='codex/other-recovery';run=str(uuid.uuid4());conversation=str(uuid.uuid4())
        # This child belongs to the already-owned fixture tmux server, whose
        # registered cleanup also covers setup failure before the PID read.
        self.tmux('new-session','-d','-s',name,'-x','100','-y','30',
                  'env','INPUT_FIXTURE='+str(other),'python3',str(self.root/'agent.py'))
        self.wait_for(lambda:(other/'agent-pid').exists() and (other/'agent-pid').read_text().strip())
        pid=int((other/'agent-pid').read_text())
        start=subprocess.check_output(['ps','-p',str(pid),'-o','lstart='],
            env=dict(self.env,LC_ALL='C',TZ='UTC'),text=True).strip()
        pane=self.tmux('display-message','-p','-t','='+name+':','#{pane_id}').strip()
        self.tmux('set-option','-t','='+name+':','@hgs_run',run)
        record=dict(self.record,name=name,run_id=run,conversation_id=conversation,pid=pid,process_start=start,pane=pane)
        import hashlib
        (self.state/(hashlib.sha256(name.encode()).hexdigest()+'.json')).write_text(json.dumps(record))
        self.tick();second=self.command('inspect',name)['recovery']
        self.command('recovery','action',payload=dict(name=name,id=second['id'],action='now'))
        self.tick();self.assertEqual(self.command('inspect',name)['recovery']['state'],'waiting')
        self.assertFalse((other/'received').exists());self.assertEqual(self.received(),received)
        first=self.inspect()['recovery'];sent=time.time()-601
        first.update(started_at=sent-1,sent_at=sent);first['receipt']['submitted_at']=sent+.01
        path.write_text(json.dumps(first))
        self.tick();self.assertEqual(self.inspect()['recovery']['state'],'uncertain')
        # The next observation no longer reserves the expired job's account.
        self.tick();self.wait_for(lambda:(other/'received').exists() and (other/'received').read_bytes().endswith(b'\r'))
        self.assertEqual((other/'received').read_bytes().count(b'\r'),1)
        self.assertEqual(self.command('inspect',name)['recovery']['attempt'],1)
        self.assertEqual(self.received(),received)

    def test_kimi_saved_rolling_quota_recovers_once_under_the_existing_rate_policy(self):
        self.prepare('kimi')
        self.record['provider_error'].update(error_kind='quota',detail=
            "429 You've reached your 5-hour usage limit. Your quota will reset when the current 5-hour window ends. "
            "To continue now, purchase extra usage or upgrade your plan: https://example.invalid/?tab=quota")
        self.write_record()
        policy=json.loads(self.policy.read_text());policy.update(version=2,schedules=dict(rate_limit=[15,30,60,300,0]))
        self.policy.write_text(json.dumps(policy));self.tick()
        info=self.inspect();job=info['recovery']
        self.assertEqual(info['provider_error']['error_kind'],'rate_limit')
        self.assertEqual((job['state'],job['class'],job['not_before']),('waiting','rate_limit',0.0))
        self.assertEqual(job['delays'],[15,30,60,300,0]);self.assertEqual(self.received(),b'')
        self.command('recovery','action',payload=dict(name=self.name,id=job['id'],action='now'))
        self.tick();sent=self.inspect()['recovery'];self.assertEqual((sent['state'],sent['attempt']),('retrying',1))
        self.wait_for(lambda:self.received().endswith(b'\r'));raw=self.received()
        self.assertEqual(raw.count(b'\r'),1)
        self.assertIn('[HGS automatic recovery]',sent['receipt']['text'])
        self.hook('UserPromptSubmit',prompt=sent['receipt']['text'])
        self.hook('Stop',last_assistant_message='Fixture answer')
        for _ in range(2):
            self.tick();info=self.inspect()
            self.assertEqual((info['recovery']['state'],info['recovery']['attempt']),('succeeded',1))
            self.assertEqual((info['run_id'],info['conversation_id']),(self.run_id,self.conversation_id))
            self.assertEqual(self.received(),raw)
        self.assertEqual(json.loads(self.policy.read_text()),policy)

    def test_kimi_permanent_quota_never_sends_a_continuation(self):
        self.prepare('kimi')
        for detail in ('429 insufficient quota','429 monthly quota exhausted','429 billing limit'):
            self.hook('StopFailure',error_message=detail,error_type='quota')
            self.tick();self.assertFalse(self.inspect().get('recovery'));self.assertEqual(self.received(),b'')

    def test_kimi_structured_hook_evidence_is_manual_without_a_job_or_input(self):
        self.prepare('kimi')
        window="You've reached your 5-hour usage limit. Your quota will reset when the current 5-hour window ends."
        for detail in (window, '429 '+window):
            for code in ({'code':'insufficient_quota','message':'billing account exhausted'},
                         401, ['provider_policy'], None, False):
                with self.subTest(detail=detail,code=code):
                    self.hook('StopFailure',error_type='APIProviderRateLimitError',error_message=detail,error=code)
                    for _ in range(2):
                        self.tick();info=self.inspect()
                        self.assertEqual(info['recovery_error_class'],'manual')
                        self.assertFalse(info.get('recovery'));self.assertEqual(self.received(),b'')
                        self.assertEqual(list((self.state/'recovery/jobs').glob('*.json')),[])
                        self.assertIn('native_error_code',info['provider_error'])
                        self.assertIsNone(info['provider_error']['native_error_code'])
                        self.assertEqual(info['provider_error']['detail'],detail)
        for kind in ({'code':'provider_policy'}, 401, ['provider_policy'], None, False):
            with self.subTest(error_type=kind):
                self.hook('StopFailure',error_type=kind,error_message='429 '+window)
                self.tick();info=self.inspect()
                self.assertEqual(info['recovery_error_class'],'manual')
                self.assertFalse(info.get('recovery'));self.assertEqual(self.received(),b'')
                self.assertEqual(list((self.state/'recovery/jobs').glob('*.json')),[])
                self.assertIn('native_error_type',info['provider_error'])
                self.assertIsNone(info['provider_error']['native_error_type'])

    def kimi_hook_window_recovers_once(self, prefix):
        self.prepare('kimi')
        detail=prefix+"You've reached your 5-hour usage limit. Your quota will reset when the current 5-hour window ends."
        self.hook('StopFailure',error_type='APIProviderRateLimitError',error_message=detail)
        self.tick();info=self.inspect();job=info['recovery']
        self.assertEqual(info['provider_error']['detail'],detail)
        self.assertEqual(info['provider_error']['error_kind'],'rate_limit')
        self.assertEqual((job['state'],job['class'],job['attempt']),('waiting','rate_limit',0))
        self.assertEqual(self.received(),b'')
        self.command('recovery','action',payload=dict(name=self.name,id=job['id'],action='now'))
        self.tick();self.wait_for(lambda:self.received().endswith(b'\r'));raw=self.received()
        self.assertEqual(raw.count(b'\r'),1);self.assertIn(b'[HGS automatic recovery]',raw)
        self.hook('UserPromptSubmit',prompt=self.inspect()['recovery']['receipt']['text'])
        self.hook('Stop',last_assistant_message='Fixture answer');self.tick()
        info=self.inspect();self.assertEqual(info['recovery']['state'],'succeeded')
        self.assertEqual((info['run_id'],info['conversation_id']),(self.run_id,self.conversation_id))
        self.tick();self.assertEqual(self.received(),raw)

    def test_kimi_typed_window_hook_recovers_once(self):self.kimi_hook_window_recovers_once('')
    def test_kimi_legacy_window_hook_recovers_once(self):self.kimi_hook_window_recovers_once('429 ')

    def test_kimi_explicit_permanent_hook_types_override_the_complete_rolling_message(self):
        self.prepare('kimi')
        detail = ("429 You've reached your 5-hour usage limit. Your quota will reset when the current 5-hour window ends. "
                  "To continue now, purchase extra usage or upgrade your plan: https://example.invalid/?tab=quota")
        for kind in ('auth_error', 'context_limit', 'billing', 'provider_policy', 'provider_policy_error',
                     'model_unavailable', 'model_unavailable_error', 'invalid_request', 'invalid_request_error'):
            for field in ('error_type', 'error'):
                with self.subTest(kind=kind,field=field):
                    self.hook('StopFailure',error_message=detail,**{field:kind})
                    self.tick();info=self.inspect()
                    self.assertEqual(info['provider_error']['native_error_type' if field=='error_type' else 'native_error_code'],kind)
                    self.assertNotEqual(info['provider_error']['error_kind'],'rate_limit')
                    self.assertEqual(info['recovery_error_class'],'manual')
                    self.assertFalse(info.get('recovery'));self.assertEqual(self.received(),b'')
        self.hook('StopFailure',error_type='session_limit',error='provider_policy',error_message=detail)
        self.tick();info=self.inspect()
        self.assertEqual(info['provider_error']['error_kind'],'provider_policy')
        self.assertEqual(info['recovery_error_class'],'manual')
        self.assertFalse(info.get('recovery'));self.assertEqual(self.received(),b'')

    def test_kimi_policy_body_wins_over_both_session_limit_metadata_fields_and_deadline(self):
        self.prepare('kimi')
        detail = ("429 You've reached your 5-hour usage limit. Your quota will reset when the current 5-hour window ends. "
                  "content_filter: request blocked")
        for field in ('error_type', 'error'):
            with self.subTest(field=field):
                self.hook('StopFailure',retry_after_seconds=3600,error_message=detail,**{field:'session_limit'})
                self.tick();info=self.inspect()
                self.assertEqual(info['provider_error']['error_kind'],'provider_policy')
                self.assertEqual(info['recovery_error_class'],'manual')
                self.assertFalse(info.get('recovery'));self.assertEqual(self.received(),b'')

    def test_kimi_explicit_session_limit_without_reset_never_uses_the_rate_schedule(self):
        self.prepare('kimi')
        self.hook('StopFailure',error_type='session_limit_error',error_message=
            "429 You've reached your 5-hour usage limit. Your quota will reset when the current 5-hour window ends.")
        self.tick();info=self.inspect();job=info['recovery']
        self.assertEqual(info['provider_error']['error_kind'],'session_limit')
        self.assertEqual((job['class'],job['state'],job['attempt']),('session_limit','blocked',0))
        self.assertEqual(self.received(),b'')

    def test_kimi_explicit_session_limit_preserves_a_verified_retry_after_deadline(self):
        self.prepare('kimi')
        self.hook('StopFailure',error_type='session_limit',retry_after_seconds=3600,error_message=
            "429 You've reached your 5-hour usage limit. Your quota will reset when the current 5-hour window ends.")
        self.tick();info=self.inspect();job=info['recovery'];deadline=info['provider_error']['retry_not_before']
        self.assertEqual(info['provider_error']['error_kind'],'session_limit')
        self.assertEqual((job['class'],job['state']),('session_limit','waiting'))
        self.assertEqual((job['due_at'],job['not_before']),(deadline,deadline))
        self.command('recovery','action',payload=dict(name=self.name,id=job['id'],action='now'))
        self.tick();self.assertEqual(self.inspect()['recovery']['due_at'],deadline)
        self.assertEqual(self.received(),b'')

    def capacity_sources(self, add_log=False):
        from datetime import datetime, timezone
        import sqlite3
        if not add_log:
            self.prepare()
            self.capacity_at=int(time.time()*1000)/1000+.01
            self.capacity_detail='Selected model is at capacity. Please try a different model.'
            transcript=self.root/f'rollout-{self.conversation_id}.jsonl'
            transcript.write_text('\n'.join(json.dumps(row) for row in [
                dict(type='session_meta',payload=dict(id=self.conversation_id)),
                dict(type='event_msg',timestamp=datetime.fromtimestamp(self.capacity_at,timezone.utc).isoformat(timespec='milliseconds'),
                     payload=dict(type='task_complete',turn_id='fixture-failed-turn',
                                  error=dict(message=self.capacity_detail,codex_error_info='ModelCapacity'))),
            ])+'\n')
            self.record['transcript']=str(transcript);self.write_record()
        else:
            home=self.root/'.codex';home.mkdir(exist_ok=True)
            at=self.capacity_at+.000328
            with sqlite3.connect(home/'logs_2.sqlite') as db:
                db.execute('CREATE TABLE logs(id INTEGER, ts INTEGER, ts_nanos INTEGER, thread_id TEXT, target TEXT, feedback_log_body TEXT)')
                db.execute('INSERT INTO logs VALUES(1,?,?,?,?,?)',(int(at),int((at%1)*1e9),self.conversation_id,
                    'codex_core::session::turn','run_turn: Turn error: '+self.capacity_detail))

    def test_capacity_source_change_keeps_waiting_and_delivers_once(self):
        self.capacity_sources();self.tick();original=self.inspect()['recovery']
        self.assertEqual(original['state'],'waiting')
        self.capacity_sources(add_log=True);self.tick();current=self.inspect()['recovery']
        for key in ('id','due_at','attempt','failure_id','delays'):self.assertEqual(current[key],original[key])
        self.assertEqual(current['state'],'waiting');self.assertEqual(self.received(),b'')
        self.command('recovery','action',payload=dict(name=self.name,id=current['id'],action='now'))
        self.tick();self.wait_for(lambda:self.received().endswith(b'\r'))
        self.assertEqual(self.inspect()['recovery']['state'],'retrying')
        self.assertEqual(self.inspect()['recovery']['attempt'],1)
        received=self.received();self.assertEqual(received.count(b'\r'),1)
        self.tick();self.assertEqual(self.received(),received)

    def test_legacy_zero_attempt_source_cancellation_is_repaired_without_new_episode(self):
        self.capacity_sources();self.tick();original=self.inspect()['recovery']
        path=next((self.state/'recovery/jobs').glob('*.json'))
        legacy=json.loads(path.read_text());legacy.update(state='cancelled',reason='The failed turn is no longer current')
        path.write_text(json.dumps(legacy))
        self.capacity_sources(add_log=True);self.tick();repaired=self.inspect()['recovery']
        self.assertEqual(repaired['state'],'waiting')
        for key in ('id','due_at','attempt','failure_id','delays'):self.assertEqual(repaired[key],original[key])
        self.assertEqual(self.received(),b'')
        self.command('recovery','action',payload=dict(name=self.name,id=repaired['id'],action='cancel'))
        self.tick();self.assertEqual(self.inspect()['recovery']['state'],'cancelled')
        self.tick();self.assertEqual(self.inspect()['recovery']['state'],'cancelled');self.assertEqual(self.received(),b'')

    def test_draft_and_changed_identity_never_receive_recovery(self):
        self.prepare();self.tick();job=self.inspect()['recovery']
        self.screen('\x1b[2J\x1b[H› my unfinished draft',str(len('› my unfinished draft'))+':0')
        self.command('recovery','action',payload=dict(name=self.name,id=job['id'],action='now'))
        self.tick();self.assertEqual(self.inspect()['recovery']['state'],'blocked');self.assertEqual(self.inspect()['recovery']['attempt'],0);self.assertEqual(self.received(),b'')
        self.tick();self.assertEqual(self.received(),b'')

    def test_new_input_cancels_and_stale_action_is_rejected(self):
        self.prepare();self.tick();job=self.inspect()['recovery']
        self.hook('UserPromptSubmit',prompt='User changed the task');self.tick()
        self.assertEqual(self.inspect()['recovery']['state'],'cancelled')
        self.command('recovery','action',payload=dict(name=self.name,id=job['id'],action='now'),ok=False)
        self.assertEqual(self.received(),b'')

    def test_policy_is_off_by_default_and_compare_and_swap_rejects_stale_settings(self):
        policy=self.command('recovery','get')['policy'];self.assertFalse(policy['enabled'])
        self.assertTrue(policy['session_limit']);self.assertEqual(policy['session_limit_mode'],'reset')
        result=self.command('recovery','set',payload=policy);self.assertEqual(result['policy']['revision'],1)
        self.assertEqual(result['policy']['version'],3)
        self.assertIn('changed',self.command('recovery','set',payload=policy,ok=False))
        self.assertEqual(self.received(),b'')

    @unittest.skipUnless(sys.platform.startswith('linux'),'Linux user service ownership')
    def test_policy_refresh_prefers_existing_service_without_starting_detached_worker(self):
        self.prepare()
        unit=self.root/'.config/systemd/user/hgs-recovery.service';unit.parent.mkdir(parents=True)
        unit.write_text('[Service]\nType=simple\n')
        self.script('systemctl',"#!/usr/bin/env python3\nimport json,pathlib,sys\n"
            "with pathlib.Path(__file__+'.calls').open('a') as out:out.write(json.dumps(sys.argv[1:])+'\\n')\n")
        for _ in range(3):self.assertTrue(self.command('recovery','get')['policy']['enabled'])
        calls=[json.loads(line) for line in (self.bin/'systemctl.calls').read_text().splitlines()]
        self.assertEqual(calls,[['--user','start','--no-block','hgs-recovery.service']]*3)
        self.assertFalse((self.state/'recovery/worker.lock').exists())
        self.assertEqual(self.received(),b'')

    def session_limit(self):
        from datetime import datetime, timezone
        reset=int((time.time()+7200)//60)*60
        detail="You've hit your session limit · resets "+datetime.fromtimestamp(reset,timezone.utc).strftime('%H:%M')+' (UTC)'
        self.hook('StopFailure',error='rate_limit',error_details=detail)
        return reset

    def test_session_limit_waits_until_native_reset_and_retry_now_preserves_deadline(self):
        self.prepare('claude');reset=self.session_limit();self.tick()
        info=self.inspect();job=info['recovery']
        self.assertEqual(info['provider_error']['error_kind'],'session_limit')
        self.assertEqual(info['provider_error']['reset_at'],reset)
        self.assertEqual(job['class'],'session_limit');self.assertEqual(job['due_at'],reset)
        self.assertEqual(job['not_before'],reset);self.assertEqual(job['state'],'waiting')
        self.command('recovery','action',payload=dict(name=self.name,id=job['id'],action='now'))
        self.tick();self.assertEqual(self.inspect()['recovery']['due_at'],reset)
        self.assertEqual(self.received(),b'')
        self.hook('UserPromptSubmit',prompt='Changed task');self.tick()
        self.assertEqual(self.inspect()['recovery']['state'],'cancelled');self.assertEqual(self.received(),b'')

    def test_session_limit_interval_can_retry_before_reset_with_native_acknowledgement(self):
        self.prepare('claude')
        policy=json.loads(self.policy.read_text());policy.update(version=3,session_limit=True,
            session_limit_mode='interval',schedules=dict(session_limit=[30,0]))
        self.policy.write_text(json.dumps(policy))
        reset=self.session_limit();before=time.time();self.tick();job=self.inspect()['recovery']
        self.assertEqual(job['class'],'session_limit');self.assertEqual(job['delays'],[30,0])
        self.assertGreaterEqual(job['due_at'],before+30);self.assertLess(job['due_at'],reset)
        self.command('recovery','action',payload=dict(name=self.name,id=job['id'],action='now'))
        self.tick();sent=self.inspect()['recovery'];self.assertEqual(sent['state'],'retrying')
        self.wait_for(lambda:self.received().endswith(b'\r'));raw=self.received()
        self.hook('UserPromptSubmit',prompt=sent['receipt']['text'])
        self.session_limit();self.tick();following=self.inspect()['recovery']
        self.assertEqual(following['state'],'waiting');self.assertEqual(following['class_attempt'],1)
        self.assertEqual(self.received(),raw)

    def test_session_limit_without_reset_is_blocked_without_input(self):
        self.prepare('claude');self.hook('StopFailure',error='rate_limit',error_details="You've hit your session limit")
        self.tick();job=self.inspect()['recovery'];self.assertEqual(job['state'],'blocked')
        self.tick();self.assertEqual(self.received(),b'')

    def test_native_quota_completion_requires_attention_without_automatic_input(self):
        self.native_quota_completion(False)

    def test_native_quota_completion_cancels_a_previous_temporary_failure_retry(self):
        self.native_quota_completion(True)

    def native_quota_completion(self,pending):
        from datetime import datetime, timezone
        self.prepare()
        if pending:
            self.tick()
            self.assertEqual(self.inspect()['recovery']['state'],'waiting')
        at=time.time()-1
        transcript=self.root/f'rollout-{self.conversation_id}.jsonl'
        transcript.write_text('\n'.join(json.dumps(row) for row in [
            dict(type='session_meta',payload=dict(id=self.conversation_id)),
            dict(type='event_msg',timestamp=datetime.fromtimestamp(at,timezone.utc).isoformat(),
                 payload=dict(type='task_complete',turn_id='failed-turn',error=dict(
                     message='7d limit reached. Resets in 4d18h',codex_error_info='usage_limit_exceeded'))),
        ])+'\n')
        self.record.pop('provider_error')
        self.record.update(transcript=str(transcript),phase='compacting',activity='busy',last_main_progress_at=at-1)
        self.write_record()
        before=self.record_path.read_bytes()
        info=self.inspect()
        self.assertEqual(info['phase'],'error')
        self.assertEqual(info['activity'],'attention')
        self.assertEqual(info['activity_summary'],'Usage limit reached')
        self.assertEqual(info['provider_error']['error_kind'],'quota')
        self.assertEqual(self.record_path.read_bytes(),before)
        for _ in range(2):
            self.tick()
            if pending:self.assertEqual(self.inspect()['recovery']['state'],'cancelled')
            else:self.assertFalse(self.inspect().get('recovery'))
            self.assertEqual(self.received(),b'')

    def test_shared_sync_converges_and_stale_writes_cannot_overwrite(self):
        first=self.command('recovery','get')['policy']
        first['session_limit_mode']='interval'
        first['schedules']={'service':[3,7,-1],'network':[5,20,0],'rate_limit':[-1],'session_limit':[600,0]}
        saved=self.command('recovery','set',payload=first)['policy']
        self.assertEqual(saved['version'],3)
        incoming=dict(saved,revision=saved['revision']+1,writer='remote-writer',service=False)
        result=self.command('recovery','sync',payload=incoming)
        self.assertEqual(result['policy']['writer'],'remote-writer')
        self.assertFalse(result['policy']['service'])
        self.assertEqual(result['policy']['session_limit_mode'],'interval')
        self.assertEqual(result['policy']['schedules']['session_limit'],[600,0])
        stale=self.command('recovery','sync',payload=saved)['policy']
        self.assertEqual(stale['writer'],'remote-writer')
        self.command('recovery','set',payload=saved,ok=False)
        peer=dict(incoming,writer='zz-peer')
        self.assertEqual(self.command('recovery','sync',payload=peer)['policy']['writer'],'zz-peer')
        self.assertEqual(self.command('recovery','sync',payload=incoming)['policy']['writer'],'zz-peer')
        for values in ([0],[1,0,2],[1,-1,3],[-2],[86401]):
            invalid=dict(peer,schedules={'network':values})
            self.command('recovery','set',payload=invalid,ok=False)
        self.assertEqual(self.received(),b'')

    def test_rename_preserves_budget_deadline_and_episode(self):
        self.prepare();self.tick();original=self.inspect()['recovery']
        subprocess.run([str(fixtures.HGS),'rename',self.name,'codex/input-test/renamed'],env=self.env,text=True,capture_output=True,check=True)
        self.name='codex/input-test/renamed';self.tick();renamed=self.inspect()['recovery']
        self.assertEqual(renamed['name'],self.name)
        for key in ('id','due_at','attempt','delays','identity'):self.assertEqual(renamed[key],original[key])
        self.assertEqual(self.received(),b'')


@unittest.skipIf(sys.platform=='win32','the Windows session host supervises the worker')
class WorkerOwnership(unittest.TestCase):
    """launchd and systemd start the service's worker again 10 s after it exits, so
    it waits for a worker a client started, and that worker gives way to it. These
    workers are not the service's; the service's own waiting is pinned by the
    state::service_worker unit tests."""
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='hgs-recovery-worker-');self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name);self.state=self.root/'state';(self.state/'recovery').mkdir(parents=True)
        tools=self.root/'bin';tools.mkdir()
        # No test here reaches a tmux server; never fall through to the user's.
        (tools/'tmux').write_text('#!/bin/sh\nexit 1\n');(tools/'tmux').chmod(0o755)
        self.env={k:v for k,v in os.environ.items() if not k.startswith(('HGS_','CLAUDE_','TMUX'))}
        self.env.update(HOME=str(self.root),HGS_CONFIG_DIR=str(self.root/'config'),HGS_STATE_DIR=str(self.state),
                        PATH=str(tools)+os.pathsep+os.environ['PATH'])
        self.started=[];self.addCleanup(self.stop)

    def stop(self):
        for worker in self.started:
            if worker.poll() is None:worker.terminate();worker.wait(timeout=10)
        for pid in self.detached():
            os.kill(pid,15)

    def detached(self):
        """Recovery workers of this fixture, as far as this platform shows them."""
        found=[];proc=Path('/proc')
        if not proc.is_dir():return found
        for entry in proc.iterdir():
            if not entry.name.isdigit():continue
            try:environ=(entry/'environ').read_bytes().split(b'\0');cmdline=(entry/'cmdline').read_bytes().split(b'\0')
            except OSError:continue
            if b'HGS_STATE_DIR='+bytes(self.state) in environ and cmdline[1:3]==[b'recovery',b'worker']:found.append(int(entry.name))
        return found

    def hold(self,name):
        import fcntl
        handle=open(self.state/'recovery'/name,'a');self.addCleanup(handle.close)
        fcntl.flock(handle,fcntl.LOCK_EX|fcntl.LOCK_NB)
        return handle

    def held(self,name):
        import fcntl
        path=self.state/'recovery'/name
        if not path.exists():return False
        with open(path) as handle:
            try:fcntl.flock(handle,fcntl.LOCK_SH|fcntl.LOCK_NB)
            except BlockingIOError:return True
            fcntl.flock(handle,fcntl.LOCK_UN);return False

    def worker(self):
        worker=subprocess.Popen([str(fixtures.HGS),'recovery','worker'],env=self.env,stdin=subprocess.DEVNULL,
                                stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,text=True)
        self.started.append(worker);return worker

    def finished(self,worker):
        try:_,errors=worker.communicate(timeout=15)
        except subprocess.TimeoutExpired:self.fail('the worker kept running')
        self.assertEqual(worker.returncode,0,errors)

    def wait_for(self,check,seconds=15):
        until=time.monotonic()+seconds
        while not check():
            if time.monotonic()>until:self.fail('condition not reached')
            time.sleep(.05)

    def test_restart_keeps_crashed_dispatch_uncertain_despite_a_submitted_receipt(self):
        import hashlib
        name='codex/recovery-fixture';run=str(uuid.uuid4());conversation=str(uuid.uuid4());request=str(uuid.uuid4())
        text='[HGS automatic recovery] The previous turn stopped with a temporary provider error. Continue the current task from the existing conversation. Check any uncertain operation before repeating it.'
        payload=dict(request_id=request,text=text,expected_run_id=run,expected_conversation_id=conversation)
        receipt=dict(request_id=request,name=name,run_id=run,conversation_id=conversation,status='submitted',
            transport='terminal',first_message=False,text=text,submitted_text=text,attachments=[],submitted_at=time.time(),
            digest=hashlib.sha256(json.dumps(payload,sort_keys=True,separators=(',',':')).encode()).hexdigest())
        job=dict(id=str(uuid.uuid4()),name=name,agent='codex',identity=[run,conversation,'model','account',None],
            state='dispatching',action='continue_message',attempt=1,request_id=request,receipt=receipt,
            acknowledged=False,started_at=time.time()-1,sent_at=receipt['submitted_at'])
        directory=self.state/'recovery/jobs';directory.mkdir()
        path=directory/(hashlib.sha256(name.encode()).hexdigest()+'.json');path.write_text(json.dumps(job))
        worker=self.worker()
        self.wait_for(lambda:json.loads(path.read_text()).get('state')=='uncertain')
        stopped=json.loads(path.read_text())
        self.assertEqual(stopped['reason'],'Recovery worker restarted during delivery; inspect the agent')
        for key in ('attempt','request_id','receipt'):self.assertEqual(stopped[key],job[key])
        self.hold('service.lock');self.finished(worker)

    def test_worker_exits_while_another_worker_holds_the_lock(self):
        self.hold('worker.lock')
        self.finished(self.worker())
        self.assertFalse((self.state/'recovery/heartbeat.json').exists())
        self.assertFalse(self.held('service.lock'))

    def test_worker_leaves_the_lock_to_a_waiting_service_worker(self):
        self.hold('service.lock')
        self.finished(self.worker())
        self.assertFalse(self.held('worker.lock'))
        self.assertFalse((self.state/'recovery/heartbeat.json').exists())

    def test_worker_gives_way_once_the_service_worker_waits(self):
        worker=self.worker()
        self.wait_for(lambda:(self.state/'recovery/heartbeat.json').exists() and self.held('worker.lock'))
        time.sleep(2.5);self.assertIsNone(worker.poll(),'a worker without a waiting service keeps running')
        self.hold('service.lock')
        self.finished(worker)
        self.assertFalse(self.held('worker.lock'))

    def children(self):
        """This process's hgs children, exited ones included."""
        found=set()
        for entry in Path('/proc').iterdir():
            if not entry.name.isdigit():continue
            try:stat=(entry/'stat').read_text()
            except OSError:continue
            comm,fields=stat[stat.index('(')+1:stat.rindex(')')],stat[stat.rindex(')')+2:].split()
            if comm=='hgs' and int(fields[1])==os.getpid():found.add(int(entry.name))
        return found

    def refresh(self):
        """Runs `recovery get` and returns how many workers it started. A worker
        started detached exits at once when it is not needed, so this process
        adopts it as a subreaper and counts it even after it exited."""
        import ctypes
        prctl=ctypes.CDLL(None,use_errno=True).prctl
        before=self.children()
        self.assertEqual(prctl(36,1,0,0,0),0,'PR_SET_CHILD_SUBREAPER')
        try:
            result=subprocess.run([str(fixtures.HGS),'recovery','get'],env=self.env,text=True,capture_output=True,timeout=15)
            spawned=self.children()-before
        finally:
            prctl(36,0,0,0,0)
            for pid in self.children()-before:
                try:os.kill(pid,15)
                except ProcessLookupError:pass
                os.waitpid(pid,0)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertTrue(json.loads(result.stdout)['policy']['enabled'])
        return len(spawned)

    @unittest.skipUnless(sys.platform.startswith('linux'),'lists the fixture workers through /proc')
    def test_policy_refresh_starts_no_worker_while_the_service_worker_waits(self):
        (self.state/'recovery/policy.json').write_text(json.dumps(dict(version=3,revision=1,enabled=True,enabled_at=1)))
        # Without a worker a refresh starts one, so the count below is observed.
        self.assertEqual(self.refresh(),1)
        self.wait_for(lambda:not self.held('worker.lock'))
        held=self.hold('worker.lock')
        self.assertEqual(self.refresh(),0,'a running worker needs no detached one')
        held.close()
        self.hold('service.lock')
        self.assertEqual(self.refresh(),0)
        result=subprocess.run([str(fixtures.HGS),'recovery','get'],env=self.env,text=True,capture_output=True,timeout=15)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertTrue(json.loads(result.stdout)['policy']['enabled'])
        time.sleep(1.5)
        self.assertEqual(self.detached(),[])
        self.assertFalse(self.held('worker.lock'))

if __name__=='__main__':unittest.main()
