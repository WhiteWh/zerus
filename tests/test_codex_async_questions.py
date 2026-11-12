#!/usr/bin/env python3
"""Optional Codex questions: real private PTY, no model/network or user sessions."""
import json
import os
import signal
import subprocess
import time
import unittest
import uuid
import receipt_locator
import test_input as fixtures

FAKE = fixtures.SIGNAL_SAFE_READ + r'''
import json,os,pathlib,signal,sys,termios,tty
root=pathlib.Path(os.environ['INPUT_FIXTURE']);fd=0;original=termios.tcgetattr(fd);tty.setraw(fd)
config=None;draft='';buffer=b'';closed=None
def shown(current):return bool(current and current.get('panel') and current.get('generation')!=closed)
def render():
 global config
 config=json.loads((root/'config.json').read_text()) if (root/'config.json').exists() else None
 lines=[]
 if (root/'queued').exists():
  reply=json.loads((root/'queued').read_text().split('\n')[1])[0]
  lines=['• Messages to be submitted after next tool call (press esc to interrupt and send immediately)',
   '  ↳ '+reply['question']+' → '+reply['answer'],'']
 if shown(config):
  lines += [config['title'],'']
  if config.get('options'):
   lines += ['  '+str(i+1)+'. '+v for i,v in enumerate(config['options'])]
   lines += ['› '+str(len(config['options'])+1)+'. '+(draft or 'Other')]
  else:lines += [draft or 'Type your answer']
  lines += ['','enter submit   ctrl+] skip']
 else:lines += ['› '+draft]
 if (root/'hyperlinks').exists():
  lines=['Artifact: \x1b]8;id=fixture;https://example.invalid/artifact\x1b\\Download\x1b]8;;\x1b\\','']+lines
 # tmux applies output in order: once the pane has this title, the screen is drawn.
 title='codex-question-'+str(config['generation'] if config else 0)
 os.write(1,b'\x1b[?2004h\x1b[2J\x1b[H'+'\r\n'.join(lines).encode()+b'\x1b]2;'+title.encode()+b'\x1b\\')
def handle(value):
 # The test alone writes config.json. One snapshot of the drawn configuration
 # decides this whole input, including which generation it closes.
 global draft,closed
 current=config
 if value==b'\x1d':
  closed=current['generation'];(root/'skipped').touch();render();return
 if value!=b'\r':return
 if shown(current):
  text='<send_user_message_question_reply>\n'+json.dumps([dict(questionItemId=current['native_id'],question=current['title'],answer=draft)])+'\n</send_user_message_question_reply>'
 else:text=draft
 if (root/'wrong-ack').exists():text=text.replace('Accepted answer','Different answer')
 if (root/'queue-answer').exists():
  (root/'queued').write_text(text);(root/'submitted').write_text(text);draft='';closed=current['generation'];render();return
 with open(current['transcript'],'a') as f:f.write(json.dumps(dict(type='event_msg',timestamp='2026-10-07T15:01:00Z',payload=dict(type='user_message',message=text)))+'\n')
 (root/'submitted').write_text(text);draft='';closed=current['generation'];render()
def next_input():
 # SIGUSR1 only leaves a byte on the wakeup pipe, and nothing draws in a signal
 # handler. Drawing after draining the pipe reads every configuration written
 # before those signals; a later signal leaves a new byte for the next pass.
 # Pending input is handled first, against the screen it was typed into.
 while True:
  ready=select.select([fd,wake],[],[])[0]
  if fd in ready:return os.read(fd,65536)
  os.read(wake,4096);render()
signal.signal(signal.SIGUSR1,lambda *args:None);render();(root/'agent-pid').write_text(str(os.getpid()))
try:
 while True:
  value=next_input()
  if not value:break
  with (root/'received').open('ab') as f:f.write(value)
  buffer+=value
  while buffer:
   if buffer.startswith(b'\x1b[200~'):
    end=buffer.find(b'\x1b[201~')
    if end<0:break
    draft+=buffer[6:end].decode();buffer=buffer[end+6:];render()
   elif buffer[0]==27 and len(buffer)<6:break
   else:value,buffer=buffer[:1],buffer[1:];handle(value)
finally:termios.tcsetattr(fd,termios.TCSANOW,original)
'''

class CodexAsync(unittest.TestCase):
    script=fixtures.InputTransport.script
    tmux=fixtures.InputTransport.tmux
    wait_for=fixtures.InputTransport.wait_for
    write_record=fixtures.InputTransport.write_record
    received=fixtures.InputTransport.received
    tearDown=fixtures.InputTransport.tearDown
    def setUp(self):
        original=fixtures.FAKE_AGENT
        try:
            fixtures.FAKE_AGENT=FAKE;fixtures.InputTransport.setUp(self)
        finally:fixtures.FAKE_AGENT=original
        self.transcript=self.root/('rollout-'+self.conversation_id+'.jsonl')
        self.transcript.write_text(json.dumps(dict(type='session_meta',payload=dict(id=self.conversation_id)))+'\n')
        self.record['transcript']=str(self.transcript);self.write_record()
        self.generation=0;self.configure()
    def configure(self,panel=False,options=None):
        self.options=['One','Two'] if options is None else options
        self.generation+=1
        config=dict(title='Which option?',options=self.options,panel=panel,generation=self.generation,transcript=str(self.transcript),native_id='["request_user_input_async","call_one",0]')
        # Replace the whole file: the agent may still be reading the previous one.
        (self.root/'config.tmp').write_text(json.dumps(config));os.replace(self.root/'config.tmp',self.root/'config.json')
        os.kill(self.record['pid'],signal.SIGUSR1)
        # The agent titles the pane after each screen, so once the pane has this
        # generation's title a matching footer cannot belong to an earlier screen.
        title='codex-question-'+str(self.generation)
        drawn=lambda:self.title()==title and ('ctrl+] skip' in self.tmux('capture-pane','-p','-t',self.pane))==panel
        try:self.wait_for(drawn)
        except self.failureException:self.fail('fixture timed out: '+self.readiness(title))
    def title(self,check=True):
        return self.tmux('display-message','-p','-t',self.pane,'#{pane_title}',check=check).strip()
    def readiness(self,title):
        try:os.kill(self.record['pid'],0);alive=True
        except OSError:alive=False
        rows=[row[:100] for row in self.tmux('capture-pane','-p','-t',self.pane,check=False).splitlines() if row.strip()]
        return json.dumps(dict(expected=title,title=self.title(check=False)[:100],agent_alive=alive,footer=any('ctrl+] skip' in row for row in rows),screen=rows[-6:]))
    def append(self,event):
        with self.transcript.open('a') as file:file.write(json.dumps(event)+'\n')
    def request(self):
        self.append(dict(type='response_item',timestamp='2026-10-07T15:00:00Z',payload=dict(type='function_call',name='request_user_input_async',call_id='call_one',arguments=json.dumps(dict(questions=[dict(title='Which option?',**({'options':self.options} if self.options else {}))])))))
        self.append(dict(type='response_item',payload=dict(type='function_call_output',call_id='call_one',output='{"accepted":true}')))
        return self.inspect()['pending_questions'][0]
    def inspect_once(self):
        result=subprocess.run([str(fixtures.HGS),'inspect',self.name],env=self.env,text=True,capture_output=True,timeout=15)
        self.assertEqual(result.returncode,0,result.stderr);return json.loads(result.stdout)
    def inspect(self):
        # The receipt locator indexes each fixture's fresh question receipts in
        # the background (zerus-a4mub.11); until then the questions are
        # explicitly unknown, never a list to act on (zerus-a4mub.8).
        until=time.monotonic()+20
        while True:
            result=self.inspect_once()
            if not (result.get('pending_questions_complete') is False
                    and 'still being indexed' in result.get('pending_questions_error','')):
                return result
            self.assertLess(time.monotonic(),until,'questions stayed unknown')
            time.sleep(0.2)
    def optional(self,settled=True):
        until=time.monotonic()+20
        while True:
            result=subprocess.run([str(fixtures.HGS),'ls','--json','--local'],env=self.env,text=True,capture_output=True,timeout=15)
            self.assertEqual(result.returncode,0,result.stderr)
            value=next(row for row in json.loads(result.stdout)['sessions'] if row['name']==self.name)['optional_questions']
            if value is not None or not settled or time.monotonic()>=until:
                return value
            time.sleep(0.2)
    def answer(self,card,answer=None,request_id=None):
        payload=dict(request_id=request_id or str(uuid.uuid4()),expected_run_id=self.run_id,expected_conversation_id=self.conversation_id,question_id=card['question_id'],expected_question_hash=card['question_hash'],answers=[answer or dict(question_id='q_0',text='Accepted answer')])
        result=subprocess.run([str(fixtures.HGS),'answer',self.name,'--json'],input=json.dumps(payload),env=self.env,text=True,capture_output=True,timeout=15)
        return result,payload
    def test_questions_before_recent_tail_are_recovered_and_legacy_cache_migrates(self):
        # No prior inspection: the pending question is already outside 8 MiB.
        self.append(dict(type='response_item',timestamp='2026-10-07T15:00:00Z',payload=dict(type='function_call',name='request_user_input_async',call_id='call_one',arguments=json.dumps(dict(questions=[dict(title='Which option?',options=self.options)])))))
        self.append(dict(type='response_item',payload=dict(type='function_call_output',call_id='call_one',output='{"accepted":true}')))
        filler=json.dumps(dict(type='event_msg',payload=dict(type='irrelevant',message='x'*32768)))+'\n'
        with self.transcript.open('a') as f:f.write(filler*270)
        result=self.inspect();self.assertEqual(len(result['pending_questions']),1)
        card=result['pending_questions'][0]
        cache_path=next((self.state/'codex_questions').glob('*.json'))
        cache=json.loads(cache_path.read_text());self.assertEqual(cache['offset'],self.transcript.stat().st_size)
        # An already-installed tail-only cache must be repaired automatically.
        cache.pop('scan_version');cache['items']={};cache_path.write_text(json.dumps(cache))
        result=self.inspect();self.assertEqual(result['pending_questions'][0]['question_id'],card['question_id'])
        self.assertEqual(json.loads(cache_path.read_text())['scan_version'],3)
        # A durable local Skip survives reindexing, even for an old question.
        response,_=self.answer(card,dict(question_id='q_0',skip=True));self.assertEqual(response.returncode,0,response.stderr)
        cache=json.loads(cache_path.read_text());cache.pop('scan_version');cache_path.write_text(json.dumps(cache))
        self.assertEqual(self.inspect()['pending_questions'],[])
        self.assertEqual(self.received(),b'')

    def test_question_index_failure_keeps_public_activity_and_reports_error(self):
        self.append(dict(type='response_item',timestamp='2026-10-07T15:00:00Z',payload=dict(type='message',role='assistant',channel='commentary',content=[dict(type='output_text',text='Public activity remains visible')])) )
        (self.state/'codex_questions').write_text('not a directory')
        result=self.inspect()
        self.assertIn('pending_questions_error',result)
        self.assertTrue(any(m.get('detail')=='Public activity remains visible' for m in result['provider_messages']))
        self.assertIsNone(self.optional(settled=False))  # Unknown, never a count of 0.

    def test_inspection_never_waits_for_question_indexing_and_shows_unknown(self):
        # zerus-a4mub.8: while another process holds the question index (as a
        # long cold scan or receipt recovery does), recent activity still
        # returns at once and the questions are unknown, never absent.
        import fcntl,time
        card=self.request()
        self.append(dict(type='response_item',timestamp='2026-10-07T15:02:00Z',payload=dict(type='message',role='assistant',channel='commentary',content=[dict(type='output_text',text='Recent activity while indexing')])))
        lock_path=next((self.state/'codex_questions').glob('*.json')).with_suffix('.lock')
        with open(lock_path,'a') as held:
            fcntl.flock(held,fcntl.LOCK_EX)
            try:
                started=time.monotonic();result=self.inspect_once();elapsed=time.monotonic()-started
                optional=self.optional(settled=False)
            finally:fcntl.flock(held,fcntl.LOCK_UN)
        self.assertLess(elapsed,10,'inspection waited for the question index')
        self.assertTrue(any(m.get('detail')=='Recent activity while indexing' for m in result['provider_messages']))
        self.assertEqual(result['pending_questions'],[])
        self.assertIs(result['pending_questions_complete'],False)
        self.assertIn('still being indexed',result['pending_questions_error'])
        self.assertIsNone(optional)  # Unknown, never a count of 0.
        # Released: the same exact question is listed and answerable again.
        current=self.inspect()
        self.assertIs(current['pending_questions_complete'],True)
        self.assertNotIn('pending_questions_error',current)
        self.assertEqual([c['question_id'] for c in current['pending_questions']],[card['question_id']])
        self.assertTrue(current['pending_questions'][0]['can_answer'])
        # Without a live agent no question is pending, but the question rows
        # of its history are still unknown while the index is held.
        os.kill(self.record['pid'],signal.SIGKILL)
        self.wait_for(lambda:self.inspect().get('pending_questions')==[])
        with open(lock_path,'a') as held:
            fcntl.flock(held,fcntl.LOCK_EX)
            try:stopped=self.inspect_once()
            finally:fcntl.flock(held,fcntl.LOCK_UN)
        self.assertIs(stopped['pending_questions_complete'],False)
        self.assertIn('still being indexed',stopped['pending_questions_error'])
        self.assertTrue(any(m.get('detail')=='Recent activity while indexing' for m in stopped['provider_messages']))

    def receipt_worker(self):
        worker=subprocess.run([str(fixtures.HGS),'__state','receipt-index-worker'],env=self.env,text=True,capture_output=True,timeout=15)
        self.assertEqual(worker.returncode,0,worker.stderr)
        import sqlite3
        return sqlite3.connect('file:'+str(self.state/'delivery-receipts-v1.sqlite3')+'?mode=ro',uri=True)
    def warm_with_old_marker(self):
        # A completed, warm question cache, plus an older build's completion
        # marker for this run: neither may stand in for current receipts.
        card=self.request()
        self.assertIs(self.inspect()['pending_questions_complete'],True)
        cache_path=next((self.state/'codex_questions').glob('*.json'))
        cache=json.loads(cache_path.read_text());cache['submission_scan_run']=self.run_id
        cache_path.write_text(json.dumps(cache))
        return card,cache_path
    def test_a_completed_cache_never_answers_over_a_newly_indexed_submission(self):
        # zerus-a4mub.8 review P1: an exact submitted receipt indexed after the
        # cache completed makes a different request refuse, with no input.
        card,cache_path=self.warm_with_old_marker()
        original=str(uuid.uuid4())
        receipt=dict(status='submitted',request_id=original,name=self.name,run_id=self.run_id,conversation_id=self.conversation_id,
            question_id=card['question_id'],question_hash=card['question_hash'],digest='f'*64,submitted_at=time.time(),
            answers=[dict(question_id='q_0',text='Already submitted')])
        folder=self.state/'question_receipts';folder.mkdir(exist_ok=True)
        (folder/(original+'.json')).write_text(json.dumps(receipt))
        with self.receipt_worker() as db:
            indexed=db.execute("SELECT COUNT(*) FROM receipt_rows WHERE source='question' AND request=?",[original]).fetchone()[0]
        self.assertEqual(indexed,1,'the earlier receipt was not indexed')
        attempt,_=self.answer(card,answer=dict(question_id='q_0',text='Accepted answer'))
        self.assertNotEqual(attempt.returncode,0,attempt.stdout)
        self.assertIn('already submitted',attempt.stderr.lower())
        self.assertEqual(self.received(),b'')
        self.assertFalse((self.root/'submitted').exists(),'an answer reached the native terminal')
        # Inspection shows the same submission, never an answerable question.
        current=self.inspect()['pending_questions'][0]
        self.assertFalse(current['can_answer']);self.assertEqual(current['answer_delivery']['request_id'],original)
    def test_a_completed_cache_reports_a_newly_recorded_receipt_gap(self):
        # zerus-a4mub.8 review P1: a gap the locator records after the cache
        # completed makes inspection incomplete, never complete and answerable.
        card,_=self.warm_with_old_marker()
        folder=self.state/'question_receipts';folder.mkdir(exist_ok=True)
        (folder/'corrupt.json').write_text('{')
        with self.receipt_worker() as db:
            errors=db.execute("SELECT errors FROM receipt_sources WHERE source='question'").fetchone()[0]
        self.assertGreater(errors,0,'the control did not establish a known locator gap')
        value=self.inspect_once()
        self.assertIs(value.get('pending_questions_complete'),False,'a completed cache hid the new receipt gap')
        self.assertEqual(value['pending_questions'],[])
        self.assertIn('cannot all be checked',value.get('pending_questions_error',''))
        attempt,_=self.answer(card,answer=dict(question_id='q_0',text='Accepted answer'))
        self.assertNotEqual(attempt.returncode,0,attempt.stdout)
        self.assertEqual(self.received(),b'')

    def test_a_missing_original_is_a_known_gap_after_reconciliation_not_permanent_indexing(self):
        card,_=self.warm_with_old_marker()
        request=str(uuid.uuid4())
        receipt=dict(status='submitted',request_id=request,name=self.name,run_id=self.run_id,conversation_id=self.conversation_id,
            question_id=card['question_id'],question_hash=card['question_hash'],digest='f'*64,submitted_at=time.time(),
            answers=[dict(question_id='q_0',text='Already submitted')])
        folder=self.state/'question_receipts';folder.mkdir(exist_ok=True)
        path=folder/(request+'.json');original=json.dumps(receipt);path.write_text(original)
        with self.receipt_worker() as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM receipt_rows WHERE source='question'").fetchone()[0],1)
        path.unlink()
        with self.receipt_worker() as db:
            self.assertEqual(db.execute("SELECT indexed,complete,errors FROM receipt_sources WHERE source='question'").fetchone(),(1,1,1))
            self.assertEqual(db.execute("SELECT reason FROM receipt_errors WHERE source='question'").fetchone(),('missing',))
        current=self.inspect_once()
        self.assertIs(current['pending_questions_complete'],False)
        self.assertEqual(current['pending_questions'],[])
        self.assertIn('cannot all be checked',current['pending_questions_error'])
        self.assertNotIn('still being indexed',current['pending_questions_error'])
        attempt,_=self.answer(card,answer=dict(question_id='q_0',text='Accepted answer'))
        self.assertNotEqual(attempt.returncode,0,attempt.stdout)
        self.assertEqual(self.received(),b'')
        path.write_text(original)
        with self.receipt_worker() as db:
            self.assertEqual(db.execute("SELECT indexed,complete,errors FROM receipt_sources WHERE source='question'").fetchone(),(1,1,0))
        restored=self.inspect()['pending_questions'][0]
        self.assertFalse(restored['can_answer'])
        self.assertEqual(restored['answer_delivery']['request_id'],request)
        self.assertEqual(self.received(),b'')

    def test_answer_from_normal_prompt_has_native_identity_and_removes_only_that_question(self):
        card=self.request();self.assertTrue(card['optional']);self.assertTrue(card['can_answer']);self.assertTrue(card['can_skip'])
        result,payload=self.answer(card);self.assertEqual(result.returncode,0,result.stderr)
        sent=(self.root/'submitted').read_text();self.assertIn('questionItemId',sent);self.assertIn('call_one',sent)
        self.assertEqual(self.inspect()['pending_questions'],[])
        replay,_=self.answer(card,request_id=payload['request_id']);self.assertEqual(replay.returncode,0,replay.stderr)
    def test_transcript_hyperlink_does_not_block_optional_answer(self):
        (self.root/'hyperlinks').touch()
        for panel in [False,True]:
            with self.subTest(panel=panel):
                self.transcript.write_text(json.dumps(dict(type='session_meta',payload=dict(id=self.conversation_id)))+'\n')
                for path in (self.state/'codex_questions').glob('*.json'):path.unlink()
                self.configure(panel=panel,options=[])
                self.wait_for(lambda:'\x1b]8;' in self.tmux('capture-pane','-p','-e','-t',self.pane))
                card=self.request();self.assertTrue(card['can_answer'],card['answer_unavailable_reason'])
                result,payload=self.answer(card);self.assertEqual(result.returncode,0,result.stderr)
                self.assertEqual(self.inspect()['pending_questions'],[])
                before=self.received()
                replay,_=self.answer(card,request_id=payload['request_id']);self.assertEqual(replay.returncode,0,replay.stderr)
                self.assertEqual(self.received(),before)
    def test_visible_native_question_supports_options_and_freeform(self):
        for options in [['One','Two'],[]]:
            with self.subTest(options=options):
                self.configure(panel=True,options=options)
                # Separate interaction identities to avoid reusing a resolved question.
                self.transcript.write_text(json.dumps(dict(type='session_meta',payload=dict(id=self.conversation_id)))+'\n')
                for path in (self.state/'codex_questions').glob('*.json'):path.unlink()
                card=self.request();self.assertTrue(card['can_answer'],card)
                result,_=self.answer(card);self.assertEqual(result.returncode,0,result.stderr)
                self.assertEqual(self.inspect()['pending_questions'],[])
    def test_skip_without_a_panel_sends_nothing_and_survives_a_new_run(self):
        card=self.request();before=self.received()
        self.assertEqual(self.optional(),{'count':1,'ids':[card['question_id']],'latest_at':card['created_at']})
        result,_=self.answer(card,dict(question_id='q_0',skip=True));self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual(self.received(),before);self.assertEqual(self.inspect()['pending_questions'],[])
        self.run_id=str(uuid.uuid4());self.record['run_id']=self.run_id;self.write_record();self.tmux('set-option','-t','='+self.name+':','@hgs_run',self.run_id)
        self.assertEqual(self.inspect()['pending_questions'],[]);self.assertEqual(self.optional(),{'count':0})
    def test_native_skip_dismisses_panel_and_never_sends_an_answer(self):
        self.configure(panel=True);card=self.request()
        result,_=self.answer(card,dict(question_id='q_0',skip=True));self.assertEqual(result.returncode,0,result.stderr)
        self.wait_for(lambda:(self.root/'skipped').exists());self.assertFalse((self.root/'submitted').exists());self.assertEqual(self.inspect()['pending_questions'],[])
    def test_existing_native_draft_is_not_overwritten(self):
        (self.root/'hyperlinks').touch()
        self.configure(panel=True);card=self.request()
        self.tmux('send-keys','-t',self.pane,'-l','\x1b[200~existing draft\x1b[201~')
        self.wait_for(lambda:'existing draft' in self.tmux('capture-pane','-p','-t',self.pane))
        before=self.received();current=self.inspect()['pending_questions'][0]
        self.assertFalse(current['can_answer']);self.assertIn('draft in Terminal',current['answer_unavailable_reason'])
        result,_=self.answer(card);self.assertNotEqual(result.returncode,0);self.assertEqual(self.received(),before)
    def test_pending_settings_refuse_an_answer_before_any_key(self):
        # zerus-cqkoh: pending model or effort settings would be typed before
        # the answer. The CLI refuses first, so its rejected receipt is exact.
        card=self.request();before=self.received()
        self.record['pending_settings']=dict(request_id=str(uuid.uuid4()),run_id=self.run_id,conversation_id=self.conversation_id,
            model='gpt-5.5',effort='high',receipt_name=self.name,receipt_run_id=self.run_id)
        self.write_record()
        result,payload=self.answer(card);self.assertNotEqual(result.returncode,0)
        self.assertIn('settings would be typed first',result.stderr)
        self.assertEqual(self.received(),before)
        receipt=json.loads((self.state/'question_receipts'/(payload['request_id']+'.json')).read_text())
        self.assertEqual(receipt['status'],'rejected',receipt)
    def test_different_native_ack_is_uncertain_and_never_retried(self):
        card=self.request();(self.root/'wrong-ack').touch()
        result,payload=self.answer(card);self.assertNotEqual(result.returncode,0);self.assertIn('delivery uncertain',result.stderr)
        before=self.received();result,_=self.answer(card,request_id=payload['request_id']);self.assertNotEqual(result.returncode,0);self.assertEqual(self.received(),before)

    def test_queued_answers_preserve_identity_block_duplicates_and_later_resolve(self):
        for panel in [False,True]:
            with self.subTest(panel=panel):
                self.transcript.write_text(json.dumps(dict(type='session_meta',payload=dict(id=self.conversation_id)))+'\n')
                for path in (self.state/'codex_questions').glob('*.json'):path.unlink()
                # The next subcase is a distinct fixture interaction, including
                # its durable transport evidence (native IDs stay deterministic).
                for directory in ['question_receipts','input_receipts']:
                    if (self.state/directory).exists():
                        for path in (self.state/directory).glob('*.json'):path.unlink()
                # A locator that still lists deleted originals can never verify
                # its coverage again; production never deletes them.
                receipt_locator.reset(self.state)
                self.configure(panel=panel);card=self.request();(self.root/'queue-answer').touch()
                result,payload=self.answer(card);self.assertEqual(result.returncode,0,result.stderr)
                receipt=json.loads(result.stdout);self.assertEqual(receipt['status'],'submitted');self.assertNotIn('answered_at',receipt)
                pending=self.inspect()['pending_questions'];self.assertEqual(len(pending),1)
                self.assertFalse(pending[0]['can_answer']);self.assertFalse(pending[0]['can_skip'])
                # Submitted / Awaiting agent no longer counts as waiting for the person.
                self.assertEqual(self.optional(),{'count':0})
                delivery=pending[0]['answer_delivery'];self.assertEqual(delivery['request_id'],payload['request_id'])
                self.assertEqual(delivery['answers'][0]['text'],'Accepted answer')
                # A fresh question index restores either submission path from
                # the scoped durable receipt, without touching native input.
                for path in (self.state/'codex_questions').glob('*.json'):path.unlink()
                restored=self.inspect()['pending_questions'][0]
                self.assertFalse(restored['can_answer']);self.assertEqual(restored['answer_delivery']['request_id'],payload['request_id'])
                before=self.received()
                replay,_=self.answer(card,request_id=payload['request_id']);self.assertEqual(replay.returncode,0,replay.stderr)
                self.assertEqual(json.loads(replay.stdout)['status'],'submitted');self.assertEqual(self.received(),before)
                for answer in [dict(question_id='q_0',text='Different answer'),dict(question_id='q_0',skip=True)]:
                    duplicate,_=self.answer(card,answer);self.assertNotEqual(duplicate.returncode,0)
                    self.assertIn('already submitted',duplicate.stderr);self.assertEqual(self.received(),before)
                self.append(dict(type='event_msg',timestamp='2026-10-07T15:01:00Z',payload=dict(type='user_message',message=(self.root/'queued').read_text())))
                self.assertEqual(self.inspect()['pending_questions'],[])
                (self.root/'queued').unlink();(self.root/'queue-answer').unlink()

    def test_legacy_terminal_submission_is_recovered_only_with_exact_receipt_evidence(self):
        card=self.request();request_id=str(uuid.uuid4())
        answers=[dict(question_id='q_0',text='Accepted answer')]
        envelope='<send_user_message_question_reply>\n'+json.dumps([dict(questionItemId=card['native_question_id'],question=card['questions'][0]['question'],answer='Accepted answer')],sort_keys=True,separators=(',',':'))+'\n</send_user_message_question_reply>'
        question=dict(status='in_progress',request_id=request_id,name=self.name,run_id=self.run_id,conversation_id=self.conversation_id,
            question_id=card['question_id'],question_hash=card['question_hash'],answers=answers)
        terminal=dict(status='submitted',request_id=request_id,name=self.name,run_id=self.run_id,conversation_id=self.conversation_id,text=envelope)
        (self.state/'question_receipts').mkdir(exist_ok=True);(self.state/'input_receipts').mkdir(exist_ok=True)
        (self.state/'question_receipts'/f'{request_id}.json').write_text(json.dumps(question))
        path=self.state/'input_receipts'/f'{request_id}.json';cache_path=next((self.state/'codex_questions').glob('*.json'))
        for mode in ['missing','wrong-run','wrong-text','exact']:
            with self.subTest(mode=mode):
                if mode!='missing':path.write_text(json.dumps(dict(terminal,**({'run_id':'other'} if mode=='wrong-run' else {'text':'unrelated input'} if mode=='wrong-text' else {}))))
                cache=json.loads(cache_path.read_text());cache.pop('submission_scan_run',None);cache['items'][card['question_id']].pop('delivery',None);cache_path.write_text(json.dumps(cache))
                current=self.inspect()['pending_questions'][0]
                if mode=='exact':
                    self.assertFalse(current['can_answer']);self.assertEqual(current['answer_delivery']['request_id'],request_id)
                else:self.assertTrue(current['can_answer']);self.assertNotIn('answer_delivery',current)

if __name__=='__main__':unittest.main()
