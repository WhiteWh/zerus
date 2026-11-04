"""Native clear commands over a private tmux socket; never real user sessions."""
import json, subprocess, time, unittest, uuid
import test_input

AGENT = test_input.SIGNAL_SAFE_READ + r'''
import os, pathlib, sys, termios, tty, json, uuid, signal
root=pathlib.Path(os.environ['INPUT_FIXTURE']); fd=sys.stdin.fileno(); old=termios.tcgetattr(fd);tty.setraw(fd)
text='';agent='codex'
def show(*args):
 global agent
 agent=(root/'provider').read_text() if (root/'provider').exists() else 'codex'
 if (root/'screen').exists():os.write(1,(root/'screen').read_bytes());return
 row=(' │ > '+text+' '*(29-len(text))+'│') if agent=='kimi' else ('❯ ' if agent=='claude' else '› ')+text
 screen='\x1b[2J\x1b[25;1H'+(' ╭─────────────────────────────────╮' if agent=='kimi' else '──────────────────────────────────')+'\x1b[26;1H'+row+'\x1b[27;1H'+(' ╰─────────────────────────────────╯' if agent=='kimi' else '──────────────────────────────────')+'\x1b[28;1H'+(' Ask When Needed  K3 /fixture' if agent=='kimi' else '')+'\x1b[26;'+str((6 if agent=='kimi' else 3)+len(text))+'H'
 if agent=='codex':
  screen='\x1b[2J\x1b[27;1H\x1b[48;5;234m› '+text+' '*80+'\x1b[28;1H   \x1b[49m\x1b[29;1H  GPT-6-Astra high · /fixture\x1b[30;1H  ? for shortcuts\x1b[27;'+str(3+len(text))+'H'
  if (root/'native-cleared').exists():screen=screen.replace('\x1b[2J','\x1b[2J\x1b[2;1H >_ OpenAI Codex (v0.162.0)\x1b[3;1H /fixture\x1b[4;1H permissions: default')
 os.write(1,screen.encode())
signal.signal(signal.SIGUSR1,show);os.write(1,b'\x1b[?2004h');show();(root/'agent-pid').write_text(str(os.getpid()))
try:
 while True:
  value=read_input(fd)
  with (root/'received').open('ab') as f:f.write(value)
  if (root/'freeze').exists():continue
  for c in value:
   if c==13:
    if text=='/clear':
     p=next((root/'state').glob('*.json'));r=json.loads(p.read_text())
     if (root/'defer-clear-hook').exists():(root/'native-cleared').touch()
     else:r['conversation_id']=str(uuid.uuid4())
     # Match real hooks: readers must see a complete binding during confirmation.
     staged=p.with_suffix('.next');staged.write_text(json.dumps(r));staged.replace(p);text=''
   else:text+=chr(c)
  show()
finally:termios.tcsetattr(fd,termios.TCSANOW,old)
'''
# A resumed Codex before its deferred SessionStart: the native executable holds
# the saved rollout open, echoes typed text and shows the reset panel after /clear.
RESUMED_CODEX = r'''
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <termios.h>
#include <unistd.h>
static char text[512]; static size_t length; static int cleared;
static void show(void) {
    char out[2048];
    int n = snprintf(out, sizeof out, "\033[2J%s\033[27;1H\033[48;5;234m\342\200\272 %s%80s\033[28;1H   \033[49m"
        "\033[29;1H  GPT-6-Astra high \302\267 /fixture\033[30;1H  ? for shortcuts\033[27;%zuH",
        cleared ? "\033[2;1H >_ OpenAI Codex (v0.162.0)\033[3;1H /fixture\033[4;1H permissions: default"
                : "\033[2;1H\342\200\272 Previous fixture request\033[4;1H\342\200\242 Previous fixture answer",
        text, "", 3 + length);
    write(1, out, n);
}
static void append(const char *name, const char *bytes, size_t size) {
    int fd = open(name, O_CREAT | O_APPEND | O_WRONLY, 0600); write(fd, bytes, size); close(fd);
}
int main(int argc, char **argv) {
    if (argc != 3 || chdir(argv[1]) || open(argv[2], O_RDONLY) < 0) return 2;
    struct termios t; tcgetattr(0, &t); cfmakeraw(&t); tcsetattr(0, TCSANOW, &t);
    write(1, "\033[?2004h", 8); show();
    FILE *f = fopen("agent-pid", "w"); fprintf(f, "%d", getpid()); fclose(f);
    char b[4096]; ssize_t n; int escape = 0;
    while ((n = read(0, b, sizeof b)) > 0) {
        append("received", b, n);
        for (ssize_t i = 0; i < n; ++i) {
            unsigned char c = b[i];
            /* Bracketed paste and other control sequences are not text. */
            if (escape) { if (escape == 1 && c == '[') escape = 2; else if (escape == 1 || (c >= 0x40 && c <= 0x7e)) escape = 0; continue; }
            if (c == 0x1b) { escape = 1; continue; }
            if (c == '\r') {
                if (!strcmp(text, "/clear") && access("refuse-clear", F_OK)) {
                    if (!access("end-before-reset", F_OK)) {
                        append("clear-waiting", "1", 1);
                        int i; for (i = 0; i < 5000 && access("end-observed", F_OK); ++i) usleep(1000);
                        if (i == 5000) return 3;
                    }
                    cleared = 1; close(open("native-cleared", O_CREAT | O_WRONLY, 0600));
                }
                else { append("submitted", text, length); append("submitted", "\n", 1); }
                length = 0; text[0] = 0;
            } else if (c >= 0x20 && length + 1 < sizeof text) { text[length++] = c; text[length] = 0; }
        }
        show();
    }
    return 0;
}
'''
class ClearContext(unittest.TestCase):
 script=test_input.InputTransport.script
 write_record=test_input.InputTransport.write_record
 tmux=test_input.InputTransport.tmux
 wait_for=test_input.InputTransport.wait_for
 received=test_input.InputTransport.received
 tearDown=test_input.InputTransport.tearDown
 def setUp(self):
  original=test_input.FAKE_AGENT;test_input.FAKE_AGENT=AGENT
  try:test_input.InputTransport.setUp(self)
  finally:test_input.FAKE_AGENT=original
 def clear(self,payload=None,success=True,remote=False):
  payload=payload or dict(request_id=str(uuid.uuid4()),expected_run_id=self.run_id,expected_conversation_id=self.conversation_id)
  r=subprocess.run([str(test_input.HGS),*(['@remote'] if remote else []),'clear-context',self.name,'--json'],env=self.env,input=json.dumps(payload),text=True,capture_output=True,timeout=12)
  if success:self.assertEqual(r.returncode,0,r.stderr);return json.loads(r.stdout)
  self.assertNotEqual(r.returncode,0);return r.stderr
 def inspect(self,*args):
  result=subprocess.run([str(test_input.HGS),'inspect',self.name,*args],env=self.env,text=True,capture_output=True,timeout=12)
  self.assertEqual(result.returncode,0,result.stderr);return json.loads(result.stdout)
 def hook(self,event):
  env=dict(self.env,HGS_SESSION=self.name,HGS_RUN_ID=self.run_id)
  result=subprocess.run([str(test_input.HGS),'__state','hook'],env=env,input=json.dumps(event),text=True,capture_output=True,timeout=12)
  self.assertEqual(result.returncode,0,result.stderr)
 def usage_history(self,sid,tokens,cache):
  path=self.root/('rollout-'+sid+'.jsonl')
  path.write_text(json.dumps(dict(type='session_meta',payload=dict(id=sid)))+'\n'+json.dumps(dict(type='event_msg',payload=dict(type='token_count',info=dict(model_context_window=258400,total_token_usage=dict(input_tokens=tokens,cached_input_tokens=cache,output_tokens=0,total_tokens=tokens),last_token_usage=dict(input_tokens=tokens,cached_input_tokens=cache,output_tokens=0,total_tokens=tokens)))))+'\n')
  return str(path)

 def failed_native_turn(self,kind='task_complete',newer=False):
  from datetime import datetime,timezone
  now=time.time();at=lambda value:datetime.fromtimestamp(value,timezone.utc).isoformat()
  path=self.root/('rollout-'+self.conversation_id+'.jsonl')
  rows=[dict(type='session_meta',payload=dict(id=self.conversation_id)),
        dict(type='event_msg',timestamp=at(now-1),payload=dict(type=kind,turn_id='fixture-failed-turn',
          message='Synthetic fixture request rejected',error=dict(message='Synthetic fixture request rejected')))]
  if newer:rows.append(dict(type='event_msg',timestamp=at(now),payload=dict(type='task_started',turn_id='newer-turn')))
  path.write_text(''.join(json.dumps(row)+'\n' for row in rows))
  self.record.update(activity='busy',phase='working',main_done=False,active_tools={},
                     turn_started=now-2,last_main_progress_at=now-2,transcript=str(path))
  self.write_record()

 def test_explicit_clear_after_native_failed_completion_does_not_need_a_missing_idle_hook(self):
  self.failed_native_turn()
  before=self.inspect()
  self.assertEqual(before['phase'],'error')
  self.assertTrue(before['clear_context_supported'],before['clear_context_reason'])
  self.assertFalse(before['compact_context_supported'])
  self.assertEqual(self.clear()['status'],'confirmed')
  self.assertEqual(self.received(),b'/clear\r')

 def test_standalone_error_and_newer_native_work_never_authorize_clear(self):
  for kind,newer in [('error',False),('task_complete',True)]:
   with self.subTest(kind=kind,newer=newer):
    self.failed_native_turn(kind,newer)
    self.assertFalse(self.inspect()['clear_context_supported'])
    self.clear(success=False)
    self.assertEqual(self.received(),b'')

 def test_failed_native_turn_with_active_tools_still_keeps_context_actions_blocked(self):
  self.failed_native_turn()
  self.record['active_tools']={'still-owned':dict(name='Bash',started=time.time())};self.write_record()
  self.assertFalse(self.inspect()['clear_context_supported'])
  self.clear(success=False)
  self.assertEqual(self.received(),b'')
 def test_deferred_codex_clear_resets_display_without_inventing_identity(self):
  self.record.update(cwd='/fixture',prompt='Previous request',last_message='Previous answer',
                     transcript=self.usage_history(self.conversation_id,109000,100000));self.write_record()
  self.assertEqual(self.inspect()['session_usage']['context']['used'],109000)
  (self.root/'defer-clear-hook').touch()
  self.assertEqual(self.clear()['status'],'confirmed');self.assertEqual(self.received(),b'/clear\r')
  fresh=self.inspect();self.assertEqual(fresh['conversation_id'],self.conversation_id)
  self.assertEqual(fresh['session_clear']['type'],'SessionCleared');self.assertEqual(fresh['session_usage']['status'],'unavailable')
  self.assertFalse(fresh.get('cache_hint'));self.assertFalse(fresh.get('provider_messages'));self.assertFalse(fresh.get('prompt'))
  self.assertEqual(sum(e['type']=='SessionCleared' for e in fresh['events']),1)
  self.assertEqual(self.inspect()['session_clear'],fresh['session_clear']);self.assertEqual(self.received(),b'/clear\r')
  new=str(uuid.uuid4());path=self.usage_history(new,32,8)
  self.hook(dict(hook_event_name='SessionStart',session_id=new,source='clear',cwd='/fixture',transcript_path=path))
  started=self.inspect();self.assertEqual(started['conversation_id'],new)
  self.assertEqual(started['session_clear']['at'],fresh['session_clear']['at'])
  self.assertEqual(started['session_usage']['context']['used'],32);self.assertEqual(started['session_usage']['totals']['cache_read'],8)
  self.assertEqual(sum(e['type']=='SessionCleared' for e in started['events']),1)
 def test_terminal_clear_is_observed_once_without_a_gui_request(self):
  self.record.update(cwd='/fixture',last_message='Previous answer',transcript=self.usage_history(self.conversation_id,109000,100000));self.write_record()
  (self.root/'defer-clear-hook').touch();self.tmux('send-keys','-t',self.pane,'-l','/clear');self.tmux('send-keys','-t',self.pane,'Enter')
  self.wait_for(lambda:(self.root/'native-cleared').exists());time.sleep(.1)
  fresh=self.inspect();self.assertEqual(fresh['session_clear']['type'],'SessionCleared')
  self.assertEqual(fresh['session_usage']['status'],'unavailable');self.assertEqual(fresh['conversation_id'],self.conversation_id)
  self.assertEqual(sum(e['type']=='SessionCleared' for e in self.inspect()['events']),1)
 def test_native_hook_records_clear_and_counters_belong_to_the_new_conversation(self):
  self.record.update(transcript=self.usage_history(self.conversation_id,109000,100000));self.write_record()
  new=str(uuid.uuid4());event=dict(hook_event_name='SessionStart',session_id=new,source='clear',transcript_path=self.usage_history(new,25,5))
  self.hook(event);self.hook(event)
  fresh=self.inspect();self.assertEqual(sum(e['type']=='SessionCleared' for e in fresh['events']),1)
  self.assertEqual(fresh['session_usage']['context']['used'],25);self.assertEqual(fresh['session_usage']['prompt_cache']['cache_read'],5)
 def test_activity_keeps_the_earlier_conversation_after_a_clear(self):
  old=self.conversation_id;new=str(uuid.uuid4())
  self.hook(dict(hook_event_name='UserPromptSubmit',session_id=old,prompt='Earlier request'))
  self.hook(dict(hook_event_name='Stop',session_id=old,last_assistant_message='Earlier answer'))
  cursor=self.inspect()['cursor']
  self.hook(dict(hook_event_name='SessionStart',session_id=new,source='clear',transcript_path=self.usage_history(new,25,5)))
  self.hook(dict(hook_event_name='UserPromptSubmit',session_id=new,prompt='Fresh request'))
  fresh=self.inspect();self.assertEqual(fresh['conversation_id'],new);self.assertEqual(fresh['cleared_conversations'],[old])
  timeline=[e['detail'] or e['type'] for e in fresh['events'] if e['type'] in ('UserPromptSubmit','Stop','SessionCleared')]
  self.assertEqual(timeline,['Earlier request','Earlier answer','SessionCleared','Fresh request'])
  self.assertEqual([e['detail'] or e['type'] for e in fresh['message_events']],timeline)
  # Sequence numbers are global: a reader's cursor continues across the clear.
  later=self.inspect('--after',str(cursor))
  self.assertEqual([e['detail'] or e['type'] for e in later['events'] if e['type']!='SessionStart'],['SessionCleared','Fresh request'])
  # Another conversation starts its own timeline.
  other=str(uuid.uuid4());self.hook(dict(hook_event_name='SessionStart',session_id=other,source='resume',transcript_path=self.usage_history(other,25,5)))
  resumed=self.inspect();self.assertEqual(resumed['cleared_conversations'],[])
  self.assertFalse(any(e['detail'] in ('Earlier request','Fresh request') for e in resumed['events']))
 def test_unconfirmed_clear_keeps_reported_usage_and_has_no_success_notice(self):
  self.record.update(transcript=self.usage_history(self.conversation_id,109000,100000),
                     clear_context_request=dict(request_id=str(uuid.uuid4()),conversation_id=self.conversation_id));self.write_record()
  fresh=self.inspect();self.assertFalse(fresh.get('session_clear'));self.assertEqual(fresh['session_usage']['context']['used'],109000)
  self.assertFalse(any(e['type']=='SessionCleared' for e in fresh['events']))
 def test_reset_observation_rejects_stale_identity_foreign_directory_and_native_draft(self):
  self.record.update(cwd='/fixture',last_message='Previous answer',transcript=self.usage_history(self.conversation_id,109000,100000))
  original=self.record.copy()
  panel='\x1b[2J\x1b[2;1H >_ OpenAI Codex (v0.162.0)\x1b[3;1H /fixture\x1b[4;1H permissions: default\x1b[27;1H\x1b[48;5;234m› '+' '*80+'\x1b[28;1H   \x1b[49m\x1b[29;1H  GPT-6-Astra high · /fixture\x1b[30;1H  ? for shortcuts\x1b[27;3H'
  cases=[(dict(process_start='stale'),panel,'2:26'),(dict(cwd='/other'),panel,'2:26'),
         (dict(activity='busy',phase='working'),panel,'2:26'),({},panel.replace('› '+' '*80,'› unsent native draft'+' '*61).replace('\x1b[27;3H','\x1b[27;22H'),'21:26')]
  for patch,screen,cursor in cases:
   with self.subTest(patch=patch,cursor=cursor):
    self.record=dict(original,**patch);self.write_record();test_input.InputTransport.screen(self,screen,cursor)
    fresh=self.inspect();self.assertFalse(fresh.get('session_clear'))
    self.assertEqual(fresh['session_usage']['context']['used'],109000)
 def test_exact_native_command_and_confirmed_identity(self):
  for agent in ['codex','claude','kimi']:
   with self.subTest(agent=agent):
    import signal
    self.record['agent']=agent;self.write_record();(self.root/'provider').write_text(agent);os=__import__('os');os.kill(self.record['pid'],signal.SIGUSR1);time.sleep(.12)
    before=self.received();self.assertEqual(self.clear()['status'],'confirmed');self.assertEqual(self.received()[len(before):],b'/clear\r')
 def interrupted_claude(self):
  # Claude sends no hook when Esc stops a turn: the session stays Interrupted
  # until its next message, while its native prompt may already take /clear.
  import os,signal
  (self.root/'provider').write_text('claude');os.kill(self.record['pid'],signal.SIGUSR1)
  self.wait_for(lambda:'❯' in self.tmux('capture-pane','-p','-t',self.pane))
  self.record.update(agent='claude',activity='unknown',phase='interrupted',main_done=False,last_error='Interrupted')
  self.write_record()
 def test_claude_turn_stopped_with_escape_clears_from_its_empty_prompt(self):
  self.interrupted_claude();initial=self.record.copy();waiting='Wait until the agent reports Ready'
  # Remaining work and another provider keep the clear waiting.
  for patch,reason in [(dict(subagents={'worker':dict(name='Explore',state='working')}),
                         waiting+'. Still working: subagent \u201cExplore\u201d'),(dict(agent='codex'),waiting)]:
   with self.subTest(patch=patch):
    self.record=dict(initial,**patch);self.write_record();info=self.inspect()
    self.assertFalse(info['clear_context_supported']);self.assertEqual(info['clear_context_reason'],reason)
    self.clear(success=False);self.assertEqual(self.received(),b'')
  self.record=initial;self.write_record();info=self.inspect()
  self.assertTrue(info['interrupted_message_can_send'],info.get('interrupted_message_reason'))
  self.assertTrue(info['clear_context_supported']);self.assertEqual(info['clear_context_reason'],'')
  # Compaction keeps waiting for Ready.
  self.assertFalse(info['compact_context_supported']);self.assertEqual(info['compact_context_reason'],waiting)
  self.assertEqual(self.clear()['status'],'confirmed');self.assertEqual(self.received(),b'/clear\r')
 def test_native_draft_keeps_an_interrupted_claude_turn_from_clearing(self):
  self.interrupted_claude()
  self.tmux('send-keys','-t',self.pane,'-l','Keep this draft')
  self.wait_for(lambda:'Keep this draft' in self.tmux('capture-pane','-p','-t',self.pane))
  # The clear names the draft that keeps the message from sending.
  info=self.inspect();self.assertFalse(info['clear_context_supported']);self.assertFalse(info['interrupted_message_can_send'])
  self.assertIn('draft',info['clear_context_reason']);self.assertEqual(info['clear_context_reason'],info['interrupted_message_reason'])
  self.clear(success=False);self.assertEqual(self.received(),b'Keep this draft')
 def test_stale_busy_draft_and_reused_request_do_not_submit(self):
  p=dict(request_id=str(uuid.uuid4()),expected_run_id='old',expected_conversation_id=self.conversation_id)
  self.clear(p,False);self.assertEqual(self.received(),b'')
  self.record['activity']='busy';self.record['phase']='working';self.write_record();self.clear(success=False);self.assertEqual(self.received(),b'')
  self.record['activity']='idle';self.record['phase']='idle';self.write_record()
  test_input.InputTransport.screen(self,'\x1b[2J\x1b[H› unsent draft', '14:0');self.clear(success=False);self.assertEqual(self.received(),b'')
 def test_replaced_kimi_hook_entry_keeps_ready_actions_consistent(self):
  # Kimi's native catalog replaces the profile hooks; a missed profile Stop
  # leaves their entry working. The listing reports Ready, and inspect,
  # dispatch, forks and settings agree, while a working catalog child still
  # keeps every action waiting.
  import os,signal
  (self.root/'provider').write_text('kimi');os.kill(self.record['pid'],signal.SIGUSR1)
  self.wait_for(lambda:'│ >' in self.tmux('capture-pane','-p','-t',self.pane))
  home=self.root/'kimi-home';session=home/'sessions'/'workspace'/self.conversation_id
  for agent in ('main','agent-1'):(session/'agents'/agent).mkdir(parents=True)
  (session/'agents'/'main'/'wire.jsonl').write_text('{}\n')
  started=time.time()*1000
  (session/'state.json').write_text(json.dumps(dict(id=self.conversation_id,version=2,updatedAt=started,agents={
      'main':dict(homedir=str(session/'agents'/'main')),
      'agent-1':dict(homedir=str(session/'agents'/'agent-1'),parentAgentId='main',labels=dict(profileName='coder'))})))
  def child(*kinds):
   rows=[dict(type=kind,agentId='agent-1',turnId=0,outcome='done',time=started+index+1) for index,kind in enumerate(kinds)]
   (session/'agents'/'agent-1'/'wire.jsonl').write_text(''.join(json.dumps(row)+'\n' for row in rows))
  child('agent.turn.started')
  self.record.update(agent='kimi',agent_home=str(home),base=['kimi'],main_done=True,
                     subagent_groups=dict(coder=dict(label='coder',active_count=1,completed_count=0,total_count=1)),
                     subagents={'coder':dict(name='coder',state='working')})
  self.write_record()
  info=self.inspect();waiting='Wait until the agent reports Ready. Still working: subagent \u201ccoder\u201d'
  self.assertEqual(info['subagent_source'],'kimi_native')
  for action in ('clear_context','compact_context'):
   self.assertFalse(info[action+'_supported'],action);self.assertEqual(info[action+'_reason'],waiting,action)
  self.assertFalse(info['fork_supported']);self.assertEqual(info['settings_apply_when'],'ready')
  self.assertIn('busy',self.clear(success=False));self.assertEqual(self.received(),b'')
  child('agent.turn.started','agent.turn.ended')
  ls=subprocess.run([str(test_input.HGS),'ls','--local','--json'],env=self.env,text=True,capture_output=True,timeout=12)
  self.assertEqual(ls.returncode,0,ls.stderr)
  row=next(r for r in json.loads(ls.stdout)['sessions'] if r['name']==self.name)
  self.assertEqual((row['activity'],row['activity_summary'],row['subagent_active_count']),('idle','Ready',0))
  self.assertEqual(json.loads(self.record_path.read_text())['subagents']['coder']['state'],'working')
  info=self.inspect()
  for action in ('clear_context','compact_context','fork'):
   self.assertTrue(info[action+'_supported'],action);self.assertEqual(info[action+'_reason'],'',action)
  self.assertEqual(info['settings_apply_when'],'now')
  self.assertEqual(self.clear()['status'],'confirmed');self.assertEqual(self.received(),b'/clear\r')
 def test_uncertain_input_does_not_press_enter_or_retry(self):
  (self.root/'freeze').touch();p=dict(request_id=str(uuid.uuid4()),expected_run_id=self.run_id,expected_conversation_id=self.conversation_id)
  self.assertIn('uncertain',self.clear(p,False));self.assertEqual(self.received(),b'/clear')
  self.clear(p,False);self.assertEqual(self.received(),b'/clear')
class ResumedClearContext(unittest.TestCase):
 """Codex defers SessionStart after resume until the next turn; the binding still expects the saved conversation."""
 script=test_input.InputTransport.script
 write_record=test_input.InputTransport.write_record
 tmux=test_input.InputTransport.tmux
 wait_for=test_input.InputTransport.wait_for
 received=test_input.InputTransport.received
 tearDown=test_input.InputTransport.tearDown
 setUp=ClearContext.setUp
 clear=ClearContext.clear
 inspect=ClearContext.inspect
 hook=ClearContext.hook
 usage_history=ClearContext.usage_history
 def resumed(self):
  test_input.InputTransport.resumed_unconfirmed(self,native=RESUMED_CODEX)
  self.wait_for(lambda:'Previous fixture answer' in self.tmux('capture-pane','-p','-t',self.pane))
  self.record.update(cwd='/fixture',prompt='Previous fixture request',last_message='Previous fixture answer');self.write_record()
 def started(self,source='clear'):
  new=str(uuid.uuid4())
  self.hook(dict(hook_event_name='SessionStart',session_id=new,source=source,cwd='/fixture',transcript_path=self.usage_history(new,32,8)))
  return new
 def assert_cleared(self,new):
  current=json.loads(self.record_path.read_text())
  self.assertEqual(current['conversation_id'],new);self.assertNotIn('expected_id',current);self.assertFalse(current.get('error'))
  self.assertEqual((current['run_id'],current['pid']),(self.run_id,self.record['pid']))
  fresh=self.inspect();self.assertEqual(fresh['cleared_conversations'],[self.conversation_id])
  self.assertEqual(fresh['session_usage']['context']['used'],32)
  self.assertEqual(sum(e['type']=='SessionCleared' for e in fresh['events']),1)
  return fresh
 def restored(self):
  self.resumed()
  self.archive_id=str(uuid.uuid4())
  self.archive_path=self.state/'archives'/(self.archive_id+'.json')
  self.archive_path.parent.mkdir(exist_ok=True)
  archived=dict(self.record,run_id=str(uuid.uuid4()),archive_id=self.archive_id,archived_at=time.time(),state='archived')
  for key in ('expected_id','restore_archive_id','resume_pending'):
   archived.pop(key,None)
  self.archive_path.write_text(json.dumps(archived)+'\n')
  self.archive_bytes=self.archive_path.read_bytes()
  self.record.update(restore_archive_id=self.archive_id)
  self.record.pop('resume_pending',None)
  self.write_record()
 def test_archive_restored_clear_preserves_source_until_its_new_session_start(self):
  self.restored();info=self.inspect()
  self.assertTrue(info['resume_message_can_send'],info.get('resume_message_reason'))
  self.assertTrue(info['clear_context_supported'],info.get('clear_context_reason'))
  self.assertFalse(info['compact_context_supported']);self.assertFalse(info['fork_supported'])
  self.assertNotIn('pause',info['allowed_actions'])
  request=dict(request_id=str(uuid.uuid4()),expected_run_id=self.run_id,expected_conversation_id=self.conversation_id)
  self.assertEqual(self.clear(request)['status'],'confirmed')
  self.assertEqual(self.received(),b'/clear\r')
  current=json.loads(self.record_path.read_text())
  self.assertEqual(self.archive_path.read_bytes(),self.archive_bytes)
  self.assertEqual((current['conversation_id'],current['expected_id']),(self.conversation_id,self.conversation_id))
  self.assertEqual(current['restore_archive_id'],self.archive_id);self.assertNotIn('resume_pending',current)
  self.assertTrue(current['session_clear']['awaiting_session_start'])
  self.assertEqual((current['run_id'],current['pid'],current['supervisor']),(self.run_id,self.record['pid'],self.record['supervisor']))
  self.clear(request,False);self.assertEqual(self.received(),b'/clear\r')
  fresh=self.inspect();self.assertFalse(fresh['clear_context_supported'])
  receipt=test_input.InputTransport.send(self,test_input.InputTransport.payload(self,text='Fresh restored fixture request'))
  self.assertEqual(receipt['conversation_id'],self.conversation_id)
  self.wait_for(lambda:(self.root/'submitted').exists())
  self.assertEqual((self.root/'submitted').read_text(),'Fresh restored fixture request\n')
  self.assert_cleared(self.started())
  current=json.loads(self.record_path.read_text())
  self.assertFalse(self.archive_path.exists())
  self.assertNotIn('restore_archive_id',current);self.assertNotIn('resume_pending',current)
 def test_real_deferred_start_can_settle_an_older_unmarked_resume_clear_end(self):
  self.resumed()
  self.record['clear_context_request']=dict(request_id=str(uuid.uuid4()),conversation_id=self.conversation_id,at=time.time())
  self.write_record()
  self.hook(dict(hook_event_name='SessionEnd',session_id=self.conversation_id,reason='other'))
  self.record=json.loads(self.record_path.read_text())
  for key in ('clear_request_id','clear_event_at'):self.record['session_end'].pop(key,None)
  self.write_record()
  self.assertFalse(self.inspect().get('resume_message_can_send'))
  self.assertFalse(self.inspect().get('session_clear'))
  self.assertEqual(self.received(),b'')
  self.assert_cleared(self.started())
 def test_unmarked_restore_end_refuses_successor_without_losing_archive(self):
  self.restored();self.assertEqual(self.clear()['status'],'confirmed')
  self.hook(dict(hook_event_name='SessionEnd',session_id=self.conversation_id,reason='other'))
  self.record=json.loads(self.record_path.read_text())
  for key in ('clear_request_id','clear_event_at'):self.record['session_end'].pop(key,None)
  self.write_record()
  self.started()
  current=json.loads(self.record_path.read_text())
  self.assertEqual((current['conversation_id'],current['expected_id']),(self.conversation_id,self.conversation_id))
  self.assertEqual(current['restore_archive_id'],self.archive_id)
  self.assertEqual(self.archive_path.read_bytes(),self.archive_bytes)
  self.assertNotIn('restore_archive_cleanup',current)
 def test_archive_launch_reservation_does_not_change_clear_or_pause_eligibility(self):
  self.restored();self.record['resume_pending']=True;self.write_record()
  self.assertTrue(self.inspect()['clear_context_supported'])
  self.assertEqual(self.clear()['status'],'confirmed')
  self.assertTrue(json.loads(self.record_path.read_text())['resume_pending'])
  self.assertEqual(self.archive_path.read_bytes(),self.archive_bytes)
  self.assert_cleared(self.started());self.assertFalse(self.archive_path.exists())
 def test_archive_original_conversation_still_settles_without_clear(self):
  self.restored()
  self.hook(dict(hook_event_name='SessionStart',session_id=self.conversation_id,source='resume',cwd='/fixture',transcript_path=str(self.rollout)))
  current=json.loads(self.record_path.read_text())
  self.assertEqual(current['conversation_id'],self.conversation_id)
  self.assertFalse(self.archive_path.exists());self.assertNotIn('restore_archive_id',current)
  self.assertNotIn('session_clear',current);self.assertEqual(self.received(),b'')
 def test_archive_clear_refuses_changed_source_sent_input_and_lost_native_identity(self):
  self.restored();original=self.record.copy()
  mutations=[dict(input_pending_at=time.time()),dict(expected_id=str(uuid.uuid4())),
    dict(supervisor=dict(pid=original['supervisor']['pid'],start='changed')),
    dict(active_tools={'fixture-tool':dict(name='fixture',started=time.time())}),
    dict(restore_archive_id=str(uuid.uuid4()))]
  for patch in mutations:
   with self.subTest(patch=patch):
    self.record=dict(original,**patch);self.write_record()
    self.assertFalse(self.inspect()['clear_context_supported'])
    self.clear(success=False);self.assertEqual(self.received(),b'')
    self.assertEqual(self.archive_path.read_bytes(),self.archive_bytes)
  self.record=original;self.write_record()
  for key,value in [('conversation_id',str(uuid.uuid4())),('agent','claude')]:
   with self.subTest(archive_field=key):
    archived=json.loads(self.archive_bytes);archived[key]=value;self.archive_path.write_text(json.dumps(archived))
    changed=self.archive_path.read_bytes();self.assertFalse(self.inspect()['clear_context_supported'])
    self.clear(success=False);self.assertEqual(self.received(),b'');self.assertEqual(self.archive_path.read_bytes(),changed)
  self.archive_path.write_bytes(self.archive_bytes)
 def test_archive_clear_wrong_start_and_stale_request_preserve_original(self):
  self.restored();self.assertEqual(self.clear()['status'],'confirmed')
  pending=json.loads(self.record_path.read_text())
  for field,value,source in [('run_id','stale-run','clear'),('restore_archive_id',str(uuid.uuid4()),'clear'),
       ('restore_archive_run_id','stale-archive-run','clear'),('conversation_id',str(uuid.uuid4()),'clear'),
       ('request_id','invalid-request','clear'),('run_id',self.run_id,'resume')]:
   with self.subTest(field=field,source=source):
    self.record=json.loads(json.dumps(pending));self.record['clear_context_request'][field]=value;self.write_record()
    self.started(source);current=json.loads(self.record_path.read_text())
    self.assertEqual(current['conversation_id'],self.conversation_id)
    self.assertEqual(current['expected_id'],self.conversation_id)
    self.assertEqual(self.archive_path.read_bytes(),self.archive_bytes)
  self.record=pending;self.write_record()
  self.hook(dict(hook_event_name='SessionStart',agent_id='fixture-child',session_id=str(uuid.uuid4()),source='clear'))
  self.assertEqual(json.loads(self.record_path.read_text())['conversation_id'],self.conversation_id)
  self.assertEqual(self.archive_path.read_bytes(),self.archive_bytes)
 def test_unrequested_archive_terminal_clear_never_settles_restore(self):
  self.restored();self.tmux('send-keys','-t',self.pane,'-l','/clear');self.tmux('send-keys','-t',self.pane,'Enter')
  self.wait_for(lambda:(self.root/'native-cleared').exists())
  self.assertFalse(self.inspect().get('session_clear'))
  self.started();current=json.loads(self.record_path.read_text())
  self.assertEqual(current['conversation_id'],self.conversation_id)
  self.assertEqual(self.archive_path.read_bytes(),self.archive_bytes)
 def test_unconfirmed_archive_clear_keeps_source_and_no_replay_receipt(self):
  self.restored();(self.root/'refuse-clear').touch()
  request=dict(request_id=str(uuid.uuid4()),expected_run_id=self.run_id,expected_conversation_id=self.conversation_id)
  self.assertEqual(self.clear(request)['status'],'submitted')
  self.assertEqual(self.archive_path.read_bytes(),self.archive_bytes)
  self.assertEqual(json.loads(self.record_path.read_text())['expected_id'],self.conversation_id)
  self.clear(request,False);self.assertEqual(self.received(),b'/clear\r')
  self.assertFalse(self.inspect()['clear_context_supported'])
 def test_verified_resume_clears_before_its_deferred_session_start(self):
  self.resumed();info=self.inspect()
  self.assertTrue(info['resume_message_can_send'],info.get('resume_message_reason'))
  self.assertTrue(info['clear_context_supported']);self.assertFalse(info['compact_context_supported'])
  self.assertEqual(info['clear_context_reason'],'')
  self.assertEqual(info['compact_context_reason'],'The agent has not confirmed the resumed conversation yet')
  self.assertIn('pause',info['allowed_actions'])
  self.assertEqual(self.clear()['status'],'confirmed');self.assertEqual(self.received(),b'/clear\r')
  current=json.loads(self.record_path.read_text())
  self.assertEqual((current['conversation_id'],current['expected_id']),(self.conversation_id,self.conversation_id))
  self.assertTrue(current['session_clear']['awaiting_session_start']);self.assertFalse(current.get('error'))
  fresh=self.inspect();self.assertEqual(fresh['session_clear']['type'],'SessionCleared')
  self.assertEqual(fresh['session_usage']['status'],'unavailable');self.assertFalse(fresh['clear_context_supported'])
  self.assertEqual(fresh['clear_context_reason'],'The cleared conversation starts with the next message')
  # The next message starts the cleared conversation, whose SessionStart names it.
  self.assertTrue(fresh['resume_message_can_send'],fresh.get('resume_message_reason'))
  receipt=test_input.InputTransport.send(self,test_input.InputTransport.payload(self,text='Fresh request'))
  self.assertEqual(receipt['conversation_id'],self.conversation_id)
  self.wait_for(lambda:(self.root/'submitted').exists());self.assertEqual((self.root/'submitted').read_text(),'Fresh request\n')
  started=self.assert_cleared(self.started())
  self.assertEqual(started['session_clear']['at'],fresh['session_clear']['at'])
 def test_terminal_clear_is_confirmed_by_the_process_that_owns_the_saved_conversation(self):
  self.resumed()
  self.tmux('send-keys','-t',self.pane,'-l','/clear');self.tmux('send-keys','-t',self.pane,'Enter')
  self.wait_for(lambda:(self.root/'native-cleared').exists())
  # No inspection saw the reset panel before the deferred hook arrived.
  self.assertNotIn('session_clear',json.loads(self.record_path.read_text()))
  self.assert_cleared(self.started())
 def test_resumed_clear_keeps_its_deferred_start_after_the_native_session_end(self):
  self.resumed();self.clear_across_main_end()
 def test_marked_restore_clear_keeps_its_transaction_across_the_native_session_end(self):
  self.restored();self.clear_across_main_end()
  self.assertFalse(self.archive_path.exists())
 def test_main_end_after_a_confirmed_reset_keeps_its_deferred_start(self):
  self.resumed();self.assertEqual(self.clear()['status'],'confirmed')
  self.hook(dict(hook_event_name='SessionEnd',session_id=self.conversation_id,reason='other'))
  info=self.inspect();self.assertTrue(info['session_clear']);self.assertTrue(info['resume_message_can_send'])
  self.assert_cleared(self.started())
 def test_main_end_after_a_confirmed_restore_reset_keeps_its_transaction(self):
  self.restored();self.assertEqual(self.clear()['status'],'confirmed')
  self.hook(dict(hook_event_name='SessionEnd',session_id=self.conversation_id,reason='other'))
  self.assert_cleared(self.started());self.assertFalse(self.archive_path.exists())
 def clear_across_main_end(self):
  import concurrent.futures
  (self.root/'end-before-reset').touch()
  with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
   clear=pool.submit(self.clear)
   self.wait_for(lambda:(self.root/'clear-waiting').exists())
   self.hook(dict(hook_event_name='SessionEnd',session_id=self.conversation_id,reason='other'))
   ended=json.loads(self.record_path.read_text())
   self.assertEqual(ended['conversation_state'],'ended')
   (self.root/'end-observed').touch()
   self.assertEqual(clear.result(timeout=12)['status'],'confirmed')
  self.assertEqual(self.received(),b'/clear\r')
  info=self.inspect();self.assertTrue(info['session_clear'])
  self.assertTrue(info['resume_message_can_send'],info.get('resume_message_reason'))
  self.assertFalse(info['clear_context_supported']);self.assertFalse(info['compact_context_supported'])
  self.assertEqual(info['conversation_id'],self.conversation_id)
  test_input.InputTransport.send(self,test_input.InputTransport.payload(self,text='Fresh request after end'))
  self.wait_for(lambda:(self.root/'submitted').exists())
  self.assertEqual((self.root/'submitted').read_text(),'Fresh request after end\n')
  self.assert_cleared(self.started())
 def test_inspection_observes_a_terminal_clear_only_while_codex_owns_the_saved_conversation(self):
  self.resumed();initial=self.record.copy()
  self.tmux('send-keys','-t',self.pane,'-l','/clear');self.tmux('send-keys','-t',self.pane,'Enter')
  self.wait_for(lambda:(self.root/'native-cleared').exists())
  self.wait_for(lambda:'OpenAI Codex' in self.tmux('capture-pane','-p','-t',self.pane))
  # The reset panel alone is no clear: a copy of the history that the exact
  # Codex does not hold open confirms nothing.
  other=self.root/'other.jsonl';other.write_bytes(self.rollout.read_bytes())
  self.record=dict(initial,transcript=str(other));self.write_record()
  self.assertFalse(self.inspect().get('session_clear'))
  self.assertNotIn('session_clear',json.loads(self.record_path.read_text()))
  self.record=initial;self.write_record()
  observed=self.inspect();self.assertEqual(observed['session_clear']['type'],'SessionCleared')
  self.assertFalse(observed['clear_context_supported'])
  self.assertTrue(json.loads(self.record_path.read_text())['session_clear']['awaiting_session_start'])
  # The deferred hook keeps the boundary observed in Terminal.
  started=self.assert_cleared(self.started())
  self.assertEqual(started['session_clear']['at'],observed['session_clear']['at'])
 def test_other_starts_still_name_a_different_conversation(self):
  self.resumed();initial=self.record.copy()
  other=self.root/'other.jsonl';other.write_bytes(self.rollout.read_bytes())
  for patch,source in [({},'startup'),({},''),({},'unknown'),(dict(restore_archive_id=str(uuid.uuid4())),'clear'),(dict(transcript=str(other)),'clear')]:
   with self.subTest(patch=patch,source=source):
    self.record=dict(initial,**patch);self.write_record();self.started(source)
    current=json.loads(self.record_path.read_text())
    self.assertEqual(current['error'],'agent resumed a different conversation')
    self.assertEqual((current['conversation_id'],current['expected_id']),(self.conversation_id,self.conversation_id))
 def test_native_resume_and_compaction_rebind_without_clearing_or_accepting_stale_sends(self):
  # H7: the current run's native SessionStart can name its new transcript.
  # That is a visible rebind, not a clear or permission to send to the old ID.
  self.resumed();initial=self.record.copy()
  for source in ['resume','compact']:
   with self.subTest(source=source):
    self.record=initial.copy();self.write_record();new=self.started(source)
    current=json.loads(self.record_path.read_text())
    self.assertEqual(current['conversation_id'],new);self.assertNotIn('expected_id',current)
    self.assertFalse(current.get('error'));self.assertFalse(current.get('session_clear'))
    self.assertEqual((current['run_id'],current['pid'],current['supervisor']),
                     (self.run_id,initial['pid'],initial['supervisor']))
    rebound=current['session_rebound']
    self.assertEqual((rebound['previous_conversation_id'],rebound['conversation_id'],rebound['source']),
                     (self.conversation_id,new,source))
    fresh=self.inspect();self.assertFalse(fresh.get('session_clear'))
    self.assertEqual(sum(e['type']=='SessionRebound' for e in fresh['events']),1)
    self.hook(dict(hook_event_name='SessionStart',session_id=new,source=source,cwd='/fixture',transcript_path=current['transcript']))
    self.assertEqual(sum(e['type']=='SessionRebound' for e in self.inspect()['events']),1)
    refused=test_input.InputTransport.send(self,test_input.InputTransport.payload(self),success=False)
    self.assertIn('identity changed',refused);self.assertEqual(self.received(),b'')
 def test_resume_rebind_keeps_run_and_fork_fresh_identity_guards(self):
  self.resumed();initial=self.record.copy()
  for source in ['resume','compact']:
   with self.subTest(source=source):
    self.record=initial.copy();self.write_record();before=self.record_path.read_bytes()
    new=str(uuid.uuid4());event=dict(hook_event_name='SessionStart',session_id=new,source=source,cwd='/fixture',
                                   transcript_path=self.usage_history(new,32,8))
    result=subprocess.run([str(test_input.HGS),'__state','hook'],
                          env=dict(self.env,HGS_SESSION=self.name,HGS_RUN_ID=str(uuid.uuid4())),
                          input=json.dumps(event),text=True,capture_output=True,timeout=12)
    self.assertEqual(result.returncode,0,result.stderr);self.assertEqual(self.record_path.read_bytes(),before)
    for patch,error in [(dict(fork_parent_id=new),'provider opened the source conversation instead of a new fork'),
                        (dict(fresh_from=dict(conversation_id=new)),'provider opened the previous conversation instead of a fresh one')]:
     with self.subTest(patch=patch):
      self.record=dict(initial,**patch);self.write_record();self.hook(event)
      current=json.loads(self.record_path.read_text())
      self.assertEqual(current['error'],error)
      self.assertEqual((current['conversation_id'],current['expected_id']),(self.conversation_id,self.conversation_id))
      self.assertFalse(current.get('session_rebound'));self.assertFalse(current.get('session_clear'))
 def test_draft_and_sent_message_prevent_a_resumed_clear(self):
  self.resumed();initial=self.record.copy()
  self.record['input_pending_at']=time.time();self.write_record()
  self.assertFalse(self.inspect()['clear_context_supported']);self.clear(success=False);self.assertEqual(self.received(),b'')
  self.record=initial;self.write_record()
  self.tmux('send-keys','-t',self.pane,'-l','Keep this draft')
  self.wait_for(lambda:'Keep this draft' in self.tmux('capture-pane','-p','-t',self.pane))
  info=self.inspect();self.assertFalse(info['clear_context_supported']);self.assertNotIn('pause',info['allowed_actions'])
  self.clear(success=False);self.assertEqual(self.received(),b'Keep this draft')
  self.assertNotIn('session_clear',json.loads(self.record_path.read_text()))
if __name__=='__main__':unittest.main()
