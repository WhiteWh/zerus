"""Guarded native compaction: only the command enters the private PTY."""
import json, os, signal, subprocess, time, unittest, uuid
import test_input
from test_clear_context import AGENT, ClearContext

class CompactContext(unittest.TestCase):
 script=ClearContext.script
 write_record=ClearContext.write_record
 tmux=ClearContext.tmux
 wait_for=ClearContext.wait_for
 received=ClearContext.received
 tearDown=ClearContext.tearDown
 def setUp(self):
  original=test_input.FAKE_AGENT;test_input.FAKE_AGENT=AGENT
  try:test_input.InputTransport.setUp(self)
  finally:test_input.FAKE_AGENT=original
 def compact(self,p=None):
  p=p or dict(request_id=str(uuid.uuid4()),expected_run_id=self.run_id,expected_conversation_id=self.conversation_id)
  return subprocess.run([str(test_input.HGS),'compact-context',self.name,'--json'],env=self.env,input=json.dumps(p),text=True,capture_output=True,timeout=12)
 def test_native_exact_command_all_providers(self):
  for agent in ['codex','claude','kimi']:
   with self.subTest(agent=agent):
    # Each provider uses a fresh terminal; never inject into an old draft.
    self.record['agent']=agent;self.write_record();(self.root/'provider').write_text(agent);os.kill(self.record['pid'],signal.SIGUSR1);time.sleep(.12)
    result=self.compact();self.assertEqual(result.returncode,0,result.stderr);self.assertEqual(json.loads(result.stdout)['status'],'submitted')
    # submitted means Enter was sent; the fixture may not have read it yet.
    self.wait_for(lambda:self.received().endswith(b'\r'))
    self.assertEqual(self.received(),b'/compact  \r' if agent=='kimi' else b'/compact\r')
    self.tearDown();self.setUp()
 def test_kimi_argument_hint_does_not_prevent_exact_command_verification(self):
  self.record['agent']='kimi';self.write_record();(self.root/'provider').write_text('kimi')
  (self.root/'kimi-argument-hint').touch();os.kill(self.record['pid'],signal.SIGUSR1)
  self.wait_for(lambda:'│ >' in self.tmux('capture-pane','-p','-t',self.pane))
  request=dict(request_id=str(uuid.uuid4()),expected_run_id=self.run_id,expected_conversation_id=self.conversation_id)
  result=self.compact(request);self.assertEqual(result.returncode,0,result.stderr)
  self.wait_for(lambda:self.received().endswith(b'\r'));self.assertEqual(self.received(),b'/compact  \r')
  self.assertEqual(json.loads(result.stdout)['status'],'submitted')
  current=json.loads(self.record_path.read_text())
  self.assertEqual((current['run_id'],current['conversation_id']),(self.run_id,self.conversation_id))
  self.assertNotEqual(self.compact(request).returncode,0);self.assertEqual(self.received(),b'/compact  \r')
 def test_busy_draft_stale_and_uncertain_never_submit(self):
  self.record.update(activity='busy',phase='working');self.write_record();self.assertNotEqual(self.compact().returncode,0);self.assertEqual(self.received(),b'')
  self.record.update(activity='idle',phase='idle');self.write_record()
  (self.root/'freeze').touch();result=self.compact();self.assertNotEqual(result.returncode,0);self.assertIn('uncertain',result.stderr);self.assertEqual(self.received(),b'/compact')
  self.assertNotEqual(self.compact().returncode,0);self.assertEqual(self.received(),b'/compact')
 def test_continuation_requires_completed_current_request(self):
  p=dict(request_id=str(uuid.uuid4()),expected_run_id=self.run_id,expected_conversation_id=self.conversation_id,expected_compaction_id=str(uuid.uuid4()),text='Continue after compaction',attachments=[])
  result=subprocess.run([str(test_input.HGS),'send',self.name,'--json'],input=json.dumps(p),env=self.env,text=True,capture_output=True)
  self.assertNotEqual(result.returncode,0);self.assertIn('not confirmed',result.stderr);self.assertEqual(self.received(),b'')
if __name__=='__main__':unittest.main()
