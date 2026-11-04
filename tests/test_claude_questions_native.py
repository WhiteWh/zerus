#!/usr/bin/env python3
"""Real Claude TUI against a localhost-only API fixture, never a user's account.
Run with HGS_CLAUDE_TEST_BIN=/absolute/path/to/claude (otherwise skipped).
"""
import hashlib,json,os,re,shlex,shutil,signal,subprocess,sys,tempfile,threading,time,unittest,uuid
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
import test_input as fixtures
import test_questions as questions

CLAUDE=os.environ.get('HGS_CLAUDE_TEST_BIN')
ITEMS=[dict(question='Which release should we prepare?',header='Release',options=[dict(label='Stable',description='Use stable build'),dict(label='Preview',description='Use preview build')]),
 dict(question='Which checks should we run?',header='Checks',multiSelect=True,options=[dict(label='Unit',description='Unit checks'),dict(label='Integration',description='Integration checks'),dict(label='UI',description='UI checks')]),
 dict(question='What else should we know?',header='Notes',options=[dict(label='Nothing',description='No extra notes'),dict(label='Later',description='Discuss later')]),
 dict(question='Which extra checks should we include?',header='Extras',multiSelect=True,options=[dict(label='Mac',description='Check macOS'),dict(label='Linux',description='Check Linux')])]
# Synthetic text Claude wraps at the 87 columns of a user's terminal: a token
# longer than a row, a row that begins "N. ", key hints that wrap once Other
# has focus, and a review where only the long third question has a gutter.
WRAPPED_ITEMS=[dict(ITEMS[0],options=[dict(label='Stable',description='Use stable build from the release branch of the example repository, then run step 2. Verify the signed tag.'),
   dict(label='Preview',description='Use preview build 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef')]),ITEMS[1],
 dict(ITEMS[2],question='What else should we know? Compare /work/example/projects/a-really-long-directory-name/contracts/template-with-a-long-name.html after step 1. review.'),ITEMS[3]]

# A single multi-select question with long Cyrillic descriptions: taller than
# the 24 rows of a pane no client has attached to (tmux's default-size).
TALL_ITEMS=[dict(question='Which release should we prepare?',header='Провайдер',multiSelect=True,options=[
 dict(label='API-ключи',description='Любой провайдер из списка (Z.AI, OpenRouter, DeepSeek и другие): ввести, заменить или удалить ключ на выбранной машине. Дополнительные поля провайдер описывает сам.'),
 dict(label='Вход через браузер',description='Подписки: показываем ссылку и код, вход выполняется в своём браузере, даже если машина удалённая. Для этого нужен запущенный сервер на той машине.'),
 dict(label='Свои адреса',description='Свой совместимый адрес (локальная модель, корпоративный прокси): URL, ключ и список моделей, которые появятся в выборе модели.'),
 dict(label='Копировать на другую машину',description='Перенести уже настроенного провайдера с одной машины на другую без повторного ввода ключа, с подтверждением на обеих сторонах.')])]

def records(path):
 """JSON Lines records of a file that a native agent may still be appending
 to. A last line without its newline is not written yet and waits for the next
 read, so the file is cut there before decoding; a complete malformed record
 still fails. Only newlines end records: JSON keeps U+2028 and NEL unescaped."""
 data=Path(path).read_bytes()
 return [json.loads(line) for line in data[:data.rfind(b'\n')+1].decode().split('\n') if line.strip()]

def shows_prefix(rows,text):
 """Whether Other editor rows show exactly text. A row ends at a hard newline
 or, unless it is blank, at a wrap; tmux trims, and Ink may swallow, the
 spaces before either boundary."""
 if not text.startswith(rows[0]):return False
 rest=text[len(rows[0]):]
 if len(rows)==1:return rest==''
 spaces=len(rest)-len(rest.lstrip(' '))
 return any(shows_prefix(rows[1:],tail) for i in range(spaces+1) for tail in [rest[i:]]*bool(rows[0])+[rest[i+1:]]*rest[i:].startswith('\n'))

def failure_evidence(fixture):
 """Bounded synthetic API evidence when Claude's collapsed TUI hides a result.

 A completed turn does not prove a shell command succeeded. Keep its actual
 tool result beside the screen, including child results, before owned cleanup.
 """
 results=[]
 requests=getattr(fixture,'native_requests',getattr(fixture,'requests',[]))
 for request in requests[-8:]:
  for message in request.get('messages',[])[-8:]:
   content=message.get('content',[])
   if not isinstance(content,list):continue
   for part in content[-8:]:
    if part.get('type')=='tool_result':
     result={key:part[key] for key in ('tool_use_id','is_error') if key in part}
     result['content']=str(part.get('content',''))[:4096]
     if result not in results:results.append(result)
 return json.dumps(dict(work=str(fixture.root/'work'),tool_results=results[-8:]),ensure_ascii=False)

@unittest.skipUnless(CLAUDE,'Set HGS_CLAUDE_TEST_BIN to test the installed native Claude against a local fake API')
class NativeClaudeQuestions(unittest.TestCase):
 script=fixtures.InputTransport.script
 tmux=fixtures.InputTransport.tmux
 write_record=fixtures.InputTransport.write_record
 inspect=questions.Questions.inspect
 def answer(self,*args,**kwargs):
  # Every capture passes through the Python transport fixture; a retried
  # answer takes about 12 s on Linux and up to three times longer on macOS.
  kwargs.setdefault('timeout',60)
  try:return questions.Questions.answer(self,*args,**kwargs)
  except AssertionError as error:
   styled=self.tmux('capture-pane','-p','-e','-t',self.pane)
   choices=[row for row in styled.splitlines() if re.match(r'^(?:❯\s*)?\d+\. ',re.sub(r'\x1b\[[0-9;:]*m','',row).strip())]
   raise AssertionError(str(error)+"\n"+self.screen()+"\nFixture transport: "+json.dumps(self.events())+"\nFixture styled choices: "+json.dumps(choices)) from error
 hook=questions.Questions.hook
 def wait_for(self,callback):
  for _ in range(250):
   result=callback()
   if result:return result
   time.sleep(.04)
  self.fail('Native fixture timed out:\n'+self.tmux('capture-pane','-p','-t',self.pane,check=False)+'\nNative tool evidence: '+failure_evidence(self))
 def setUp(self):
  self.temp=tempfile.TemporaryDirectory(prefix='hgs-claude-native-');self.addCleanup(self.temp.cleanup)
  self.root=Path(self.temp.name).resolve();self.bin=self.root/'.local/bin';self.bin.mkdir(parents=True);self.state=self.root/'state';self.state.mkdir()
  work=self.root/'work';work.mkdir();(self.root/'.claude').mkdir()
  self.tool_name=getattr(self,'tool_name','AskUserQuestion');self.tool_input=self.native_tool_input(work) if hasattr(self,'native_tool_input') else getattr(self,'tool_input',None);self.message_fixture=getattr(self,'message_fixture',False);self.approval_fixture=self.tool_name!='AskUserQuestion' and not self.message_fixture
  wrapped=self._testMethodName.startswith('test_wrapped_')
  # Claude's own PermissionRequest hook answers test_channel_* questions.
  channel=self._testMethodName.startswith('test_channel_');tall='_tall_' in self._testMethodName
  if self._testMethodName.startswith('test_channel_single_deadline_'):self.channel_seconds=4
  self.items=TALL_ITEMS if tall else ITEMS[:1] if self._testMethodName.startswith(('test_single_','test_channel_single_')) else WRAPPED_ITEMS if wrapped else ITEMS
  # Claude Code 2.1.296 and later offer auto mode once; only its own suite keeps that offer.
  offer={} if getattr(self,'auto_mode_offer',False) else dict(hasSeenAutoDefaultNudge=True)
  (self.root/'.claude.json').write_text(json.dumps(dict(hasCompletedOnboarding=True,**offer,bypassPermissionsModeAccepted=True,theme='dark',customApiKeyResponses=dict(approved=['fixture-key'],rejected=[]),projects={str(work):dict(hasTrustDialogAccepted=True)})))
  permission_mode=getattr(self,'native_permission_mode','default' if self.approval_fixture or self.message_fixture else 'bypassPermissions')
  settings={'permissions':{'defaultMode':permission_mode}}
  if hasattr(self,'native_settings'):settings.update(self.native_settings(work))
  if channel:settings.setdefault('hooks',{})['PermissionRequest']=[{'matcher':'AskUserQuestion','hooks':[{'type':'command','command':shlex.quote(str(fixtures.HGS))+' __state answer-channel','timeout':86400}]}]
  (self.root/'.claude/settings.json').write_text(json.dumps(settings))
  fixture=self;self.native_requests=[]
  class Server(BaseHTTPRequestHandler):
   def log_message(self,*args):pass
   def do_POST(self):
    body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
    fixture.native_requests.append(body)
    asks=not fixture.message_fixture and any(t.get('name')==fixture.tool_name for t in body.get('tools',[]))
    answered=any(isinstance(m.get('content'),list) and any(b.get('type')=='tool_result' for b in m['content']) for m in body.get('messages',[]))
    blocks=[dict(type='tool_use',id='toolu_native_fixture',name=fixture.tool_name,input=fixture.tool_input or dict(questions=fixture.items))] if asks and not answered else [dict(type='text',text='Fixture complete.')]
    if hasattr(fixture,'native_response_blocks'):blocks=fixture.native_response_blocks(body)
    asks=any(block['type']=='tool_use' for block in blocks)
    message=dict(id='msg_fixture',type='message',role='assistant',model=body.get('model','claude-sonnet-4-6'),content=blocks,stop_reason='tool_use' if asks else 'end_turn',stop_sequence=None,usage=getattr(fixture,'native_response_usage',dict(input_tokens=100,output_tokens=30)))
    self.send_response(200)
    if body.get('stream'):
     self.send_header('Content-Type','text/event-stream');self.end_headers()
     def emit(kind,**data):self.wfile.write(('event: '+kind+'\ndata: '+json.dumps(dict(type=kind,**data))+'\n\n').encode());self.wfile.flush()
     emit('message_start',message=dict(message,content=[],stop_reason=None))
     for i,b in enumerate(blocks):
      emit('content_block_start',index=i,content_block=dict(b,input={}) if b['type']=='tool_use' else dict(b,text=''))
      delta=dict(type='input_json_delta',partial_json=json.dumps(b['input'])) if b['type']=='tool_use' else dict(type='text_delta',text=b['text'])
      emit('content_block_delta',index=i,delta=delta);emit('content_block_stop',index=i)
     emit('message_delta',delta=dict(stop_reason=message['stop_reason'],stop_sequence=None),usage=dict(output_tokens=30));emit('message_stop')
    else:self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(json.dumps(message).encode())
  server=ThreadingHTTPServer(('127.0.0.1',0),Server);self.addCleanup(server.server_close);self.addCleanup(server.shutdown)
  threading.Thread(target=server.serve_forever,daemon=True).start()
  self.socket=self.root/'socket';self.script('tmux','#!/bin/sh\nexec '+shlex.quote(fixtures.TMUX)+' -S '+shlex.quote(str(self.socket))+' -f /dev/null "$@"\n')
  self.env=dict(os.environ,HOME=str(self.root),HGS_STATE_DIR=str(self.state),HGS_CONFIG_DIR=str(self.root/'config'),PATH=str(self.bin)+':'+os.environ['PATH'],HGS_SELF='test')
  for v in ('TMUX','TMUX_PANE','HGS_RUN_ID','HGS_SESSION','HGS_EXECUTABLE'):self.env.pop(v,None)
  self.name='claude/question-test';self.run_id=str(uuid.uuid4())
  # Reuse the transport fixture's bounded drain of this socket's pane PIDs.
  # Claude can flush files after kill-server returns; remove HOME only after
  # its owned process has exited. This also runs when native startup fails.
  self.addCleanup(lambda:fixtures.InputTransport.tearDown(self))
  # tmux's default size for a session no client has attached to yet.
  columns,rows=('80','24') if tall or '_80x24' in self._testMethodName else ('87' if wrapped else '120','45')
  hgs_env=['HGS_SESSION='+self.name,'HGS_RUN_ID='+self.run_id,'HGS_STATE_DIR='+str(self.state),'HGS_CONFIG_DIR='+str(self.root/'config')]+(['HGS_ANSWER_CHANNEL_SECONDS='+str(self.channel_seconds)] if hasattr(self,'channel_seconds') else [])
  self.tmux('new-session','-d','-s',self.name,'-x',columns,'-y',rows,'-c',str(work),'env','-i','HOME='+str(self.root),'PATH='+os.environ['PATH'],'SHELL=/bin/bash','TERM=xterm-256color','LANG=C.UTF-8','CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1','ANTHROPIC_API_KEY=fixture-key','ANTHROPIC_BASE_URL=http://127.0.0.1:'+str(server.server_port),*getattr(self,'native_env',()),*(hgs_env if channel else []),CLAUDE,*(['--dangerously-skip-permissions'] if permission_mode=='bypassPermissions' else []),'--model','claude-sonnet-4-6','Ask questions with the tool now.')
  self.pane=self.tmux('display-message','-p','-t','='+self.name+':','#{pane_id}').strip()
  # A drawn panel does not prove Claude has created its transcript yet. Both
  # must be ready within the existing startup wait before the fixture binds it.
  def ready():
   shown=('Fixture complete.' if self.message_fixture else getattr(self,'panel_question','Do you want to proceed?') if self.approval_fixture else 'Enter to select' if tall else 'Which release should we prepare?') in self.screen()
   return next((self.root/'.claude/projects').glob('*/*.jsonl'),None) if shown else None
  transcript=self.wait_for(ready);self.conversation_id=transcript.stem
  pid=int(self.tmux('display-message','-p','-t',self.pane,'#{pane_pid}'))
  start=subprocess.check_output(['ps','-p',str(pid),'-o','lstart='],env=dict(self.env,LC_ALL='C',TZ='UTC'),text=True).strip()
  self.tmux('set-option','-t','='+self.name+':','@hgs_run',self.run_id)
  self.record_path=self.state/(hashlib.sha256(self.name.encode()).hexdigest()+'.json')
  self.record=dict(version=1,name=self.name,agent='claude',run_id=self.run_id,conversation_id=self.conversation_id,pane=self.pane,pid=pid,process_start=start,activity='busy',phase='input',active_tools={},subagents={},last_event_at=time.time(),created=time.time(),transcript=str(transcript))
  if self.message_fixture:
   self.record.update(activity='idle',phase='idle');self.write_record();return
  self.write_record();self.hook(dict(hook_event_name='PreToolUse',tool_name=self.tool_name,tool_use_id='toolu_native_fixture',tool_input=self.tool_input or dict(questions=self.items)))
  if self.approval_fixture:
   # Claude's real PermissionRequest carries no tool_use_id.
   self.hook(dict(hook_event_name='PermissionRequest',tool_name=self.tool_name,tool_input=self.tool_input))
   self.card=self.inspect()['pending_questions'][0]
   return
  inspection=self.inspect()
  if not inspection.get('pending_questions'):
   diagnostics={'inspection':inspection,'record':json.loads(self.record_path.read_text()),'live':self.tmux('list-panes','-a','-F','#{session_name} #{pane_id} #{pane_dead} #{@hgs_run} #{pane_pid}'),'process':subprocess.run(['ps','-p',str(pid),'-o','pid=,lstart=,command='],env=dict(self.env,LC_ALL='C',TZ='UTC'),capture_output=True,text=True).stdout,'cli':str(fixtures.HGS)}
   self.fail(json.dumps(diagnostics,ensure_ascii=False))
  self.card=inspection['pending_questions'][0]
 def screen(self):return self.tmux('capture-pane','-p','-t',self.pane,'-J')
 def key(self,key):self.tmux('send-keys','-t',self.pane,key);time.sleep(.15)
 def enable_gutter_capture(self,gutter=True):
  # Reproduce the dim gutter observed in the user's Claude terminal
  # on captures only; all input, transcript acknowledgements and UI state still
  # come from the real CLI against our isolated localhost API.
  self.script('tmux-fixture.py','#!'+sys.executable+'\n'+f'TMUX={fixtures.TMUX!r}\nSOCKET={str(self.socket)!r}\nROOT={str(self.root)!r}\nGUTTER={gutter!r}\nRECORD={str(self.record_path)!r}\n'+r'''
import json,pathlib,re,subprocess,sys,time
args=sys.argv[1:]
root=pathlib.Path(ROOT)
if args and args[0] in ('send-keys','paste-buffer','load-buffer'):
 with (root/'transport-events').open('a') as log:log.write(json.dumps(args)+'\n')
if args and args[0]=='send-keys':
 with (root/'sent-keys').open('a') as log:log.write(json.dumps(args)+'\n')
staged=sys.stdin.buffer.read() if args and args[0]=='load-buffer' else None
if staged is not None:
 with (root/'staged-text').open('a') as log:log.write(json.dumps(staged.decode())+'\n')
result=subprocess.run([TMUX,'-S',SOCKET,'-f','/dev/null',*args],input=staged,stdout=subprocess.PIPE)
focus_fault=root/'change-focus-after-paste'
if focus_fault.exists() and args and args[0]=='paste-buffer':
 pane=args[args.index('-t')+1]
 subprocess.run([TMUX,'-S',SOCKET,'send-keys','-t',pane,'Up'],check=True)
 time.sleep(.15);focus_fault.unlink();(root/'focus-changed').touch()
# Change only this fixture's durable record between preparation and the next
# write, after the real native clear or buffer stage has happened.
fault=root/'change-identity'
if fault.exists():
 config=json.loads(fault.read_text())
 triggered=(config['stage']=='clear' and args and args[0]=='send-keys' and args[-1]=='C-u') or (config['stage']=='paste' and args and args[0]=='load-buffer')
 if triggered:
  record=pathlib.Path(RECORD); value=json.loads(record.read_text())
  if config['field']=='question':value['pending_questions']={}
  else:value['run_id']='00000000-0000-4000-8000-000000000000'
  temporary=record.with_suffix('.changed');temporary.write_text(json.dumps(value));temporary.replace(record)
  fault.unlink();(root/'identity-changed').touch()
if GUTTER and args and args[0]=='capture-pane' and result.returncode==0:
 rows=result.stdout.decode().splitlines()
 plain=lambda row:re.sub(r'\x1b\[[0-9;:]*m','',row).strip()
 starts=[i for i,r in enumerate(rows) if plain(r).startswith('←') and '✔ Submit' in plain(r)]
 if starts:
  start=starts[-1]+1; reviewing=any(plain(r)=='Review your answers' for r in rows[start:])
  question=False
  for i in range(start,len(rows)):
   row=plain(rows[i])
   if reviewing:
    if row=='Ready to submit your answers?':break
    if row.startswith('● '):question=True
    elif row.startswith('→ '):question=False
    if row and question:rows[i]='\x1b[2m│\x1b[0m '+rows[i]
   else:
    if re.match(r'^(?:❯\s*)?\d+\. ',row):break
    if row:rows[i]='\x1b[2m│\x1b[0m '+rows[i]
  with (pathlib.Path(ROOT)/'gutter-observed').open('a') as log:log.write(('review' if reviewing else 'question')+'\n')
  if reviewing and (pathlib.Path(ROOT)/'changed-review').exists():
   rows=[r.replace('Which release should we prepare?','A different question?') for r in rows]
 sys.stdout.write('\n'.join(rows)+'\n')
else:sys.stdout.buffer.write(result.stdout)
sys.exit(result.returncode)
''')
  # Read-only guards need no Python logging or injection when the gutter is
  # disabled. Exec native tmux directly so interpreter startup does not consume
  # the answer deadline on macOS; mutations retain the exact fixture wrapper.
  intercepted='send-keys|paste-buffer|load-buffer'+('|capture-pane' if gutter else '')
  wrapper=shlex.quote(str(self.bin/'tmux-fixture.py'))
  native=shlex.quote(fixtures.TMUX)+' -S '+shlex.quote(str(self.socket))+' -f /dev/null'
  self.script('tmux','#!/bin/sh\ncase "$1" in\n '+intercepted+') exec '+wrapper+' "$@" ;;\n *) exec '+native+' "$@" ;;\nesac\n')
 def payload(self,**changes):
  answers=[dict(question_id='q_0',selected_option_ids=['opt_0_1'],text=''),dict(question_id='q_1',selected_option_ids=['opt_1_0','opt_1_2'],text=''),dict(question_id='q_2',selected_option_ids=[],text='Привет from Zerus'),dict(question_id='q_3',selected_option_ids=['opt_3_1'],text='Browser check')][:len(self.items)]
  value=dict(request_id=str(uuid.uuid4()),expected_run_id=self.run_id,expected_conversation_id=self.conversation_id,question_id=self.card['question_id'],expected_question_hash=self.card['question_hash'],answers=answers);value.update(changes);return value
 def events(self):
  path=self.root/'transport-events'
  return [json.loads(row) for row in path.read_text().splitlines()] if path.exists() else []
 def staged(self):
  path=self.root/'staged-text'
  return [json.loads(row) for row in path.read_text().splitlines()] if path.exists() else []
 def reset_events(self):
  for name in ('transport-events','staged-text','sent-keys'):(self.root/name).unlink(missing_ok=True)
 def native_draft(self,value,cursor='end',choice='3'):
  for key in ([choice] if isinstance(choice,str) else choice):self.key(key)
  result=subprocess.run([fixtures.TMUX,'-S',str(self.socket),'load-buffer','-b','fixture-draft','-'],input=value,text=True,capture_output=True)
  self.assertEqual(result.returncode,0,result.stderr)
  self.tmux('paste-buffer','-p','-r','-d','-b','fixture-draft','-t',self.pane)
  time.sleep(.15)
  # The paste leaves the caret at the end. Move it only leftwards and exactly as
  # far as needed: a frame Claude draws while still applying these keys never
  # shows the caret at the requested place, as a caret passing through would.
  position=0 if cursor=='start' else len(value)//2 if cursor=='middle' else len(value)
  if position<len(value):self.tmux('send-keys','-t',self.pane,'-N',str(len(value)-position),'Left')
  # Claude applies the paste and queued caret keys after tmux returns. A loaded
  # host can still be moving the caret when hgs starts, which hgs rightly
  # reports as a changed terminal: begin once Claude shows the whole draft
  # with its caret at the requested place.
  self.wait_for(lambda:(rows:=self.draft_rows()) and shows_prefix(rows[0],value.rstrip(' ')) and shows_prefix(rows[1],value[:position]))
 def draft_rows(self):
  # The focused Other editor's rows, and its rows up to Claude's inverse caret.
  plain=lambda row:re.sub(r'\x1b\[[0-9;:]*m','',row)
  rows=self.tmux('capture-pane','-p','-e','-t',self.pane).splitlines()
  start=next((i for i,row in enumerate(rows) if re.match(r'❯ \d+\.',plain(row))),None)
  if start is None:return None
  # Text follows "❯ N. " and any checkbox; tmux trims the space ending a blank row.
  number,box=re.match(r'(❯ \d+\.) ?(\[[ ✔]\])?',plain(rows[start])).groups()
  column=len(number)+1+(len(box)+1 if box else 0)
  shown,before=[],None
  for row in rows[start:]:
   if plain(row).startswith('─') or plain(row).strip() in ('Next','Submit','❯ Next','❯ Submit'):break
   if before is None and '\x1b[7m' in row:before=shown+[plain(row.split('\x1b[7m')[0])[column:]]
   shown.append(plain(row)[column:].rstrip(' '))
  return None if before is None else (shown,before)
 def assert_native_answer(self,payload):
  expected={}
  for item,answer in zip(self.items,payload['answers']):
   labels=[option['label'] for i,option in enumerate(item['options']) if 'opt_'+str(len(expected))+'_'+str(i) in answer['selected_option_ids']]
   expected[item['question']]=', '.join(labels+([answer['text']] if answer['text'] else []))
  def recorded():
   for event in records(self.record['transcript']):
    # Claude 2.1.304+ also writes api-request-blob rows; only user rows carry toolUseResult.
    if event.get('sessionId')!=self.conversation_id or event.get('type')!='user':continue
    parts=event.get('message',{}).get('content',[])
    if isinstance(parts,list) and any(p.get('type')=='tool_result' and p.get('tool_use_id')=='toolu_native_fixture' for p in parts):return event.get('toolUseResult')
  result=self.wait_for(recorded)
  self.assertEqual(result['questions'],[dict(item,multiSelect=item.get('multiSelect',False)) for item in self.items])
  self.assertEqual(result['answers'],expected)
 def assert_pastes(self,texts):
  self.assertEqual(self.staged(),texts)
  self.assertEqual(sum(event[0]=='paste-buffer' for event in self.events()),len(texts))
 def replace_single_draft(self,draft,cursor):
  self.enable_gutter_capture(False);self.native_draft(draft,cursor);self.reset_events()
  before=self.screen();card=self.inspect()['pending_questions'][0]
  self.assertTrue(card['can_answer'],card['answer_unavailable_reason'])
  self.assertEqual(self.events(),[]);self.assertEqual(before.partition('☐')[2],self.screen().partition('☐')[2])
  payload=self.payload();payload['answers'][0]=dict(question_id='q_0',selected_option_ids=[],text='GUI   authoritative answer | Привет')
  receipt=self.answer(payload);self.assertEqual(receipt['status'],'answered');self.assert_native_answer(payload)
  self.assert_pastes([payload['answers'][0]['text']])
  before=self.events();self.assertEqual(self.answer(payload),receipt);self.assertEqual(self.events(),before)
 def test_single_overwrites_draft_from_cursor_start(self):
  self.replace_single_draft('Different native draft','start')
 def test_single_overwrites_wrapped_draft_from_cursor_middle(self):
  self.replace_single_draft('native   spaced words '+('wrapped content '*12),'middle')
 def test_single_overwrites_draft_from_cursor_end(self):
  self.replace_single_draft('Native one space remains different','end')
 def test_single_overwrites_hard_newlines(self):
  self.replace_single_draft('First native line\nSecond native line\nThird native line','middle')
 def test_single_overwrites_all_blank_newlines(self):
  self.replace_single_draft('\n\n','middle')
 def test_single_overwrites_literal_placeholder_text(self):
  self.replace_single_draft('Type something','start')
 def test_single_overwrites_matching_gui_text_once(self):
  self.replace_single_draft('GUI   authoritative answer | Привет','middle')
 def interrupted_override(self,stage,field):
  self.enable_gutter_capture(False);self.native_draft('Existing terminal answer','middle');self.reset_events()
  original=self.record_path.read_bytes()
  (self.root/'change-identity').write_text(json.dumps(dict(stage=stage,field=field)))
  payload=self.payload();payload['answers'][0]=dict(question_id='q_0',selected_option_ids=[],text='GUI replacement')
  self.assertIn('delivery uncertain',self.answer(payload,success=False))
  self.assertTrue((self.root/'identity-changed').exists())
  before=self.events();self.assertEqual(sum(event[0]=='paste-buffer' for event in before),0)
  self.assertEqual(self.staged(),['GUI replacement'] if stage=='paste' else [])
  # Restore only the synthetic identity so the durable same-request guard,
  # rather than stale identity alone, must refuse replay after partial clearing.
  temporary=self.record_path.with_suffix('.restored');temporary.write_bytes(original);temporary.replace(self.record_path)
  self.assertIn('may already be in Terminal',self.answer(payload,success=False))
  self.assertEqual(self.events(),before);self.assertNotIn('Fixture complete.',self.screen())
 def test_single_question_change_during_clear_stops_before_paste(self):
  self.interrupted_override('clear','question')
 def test_single_run_change_during_clear_stops_before_paste(self):
  self.interrupted_override('clear','run')
 def test_single_question_change_during_paste_staging_sends_no_paste(self):
  self.interrupted_override('paste','question')
 def test_single_run_change_during_paste_staging_sends_no_paste(self):
  self.interrupted_override('paste','run')
 def test_single_focus_change_after_paste_stops_before_submission(self):
  self.enable_gutter_capture(False);self.native_draft('Existing native answer');self.reset_events()
  original=self.record_path.read_bytes();(self.root/'change-focus-after-paste').touch()
  payload=self.payload();payload['answers'][0]=dict(question_id='q_0',selected_option_ids=[],text='GUI replacement')
  self.assertIn('delivery uncertain',self.answer(payload,success=False))
  self.assertTrue((self.root/'focus-changed').exists());self.assertEqual(self.record_path.read_bytes(),original)
  self.assert_pastes(['GUI replacement']);before=self.events()
  self.assertNotIn('Enter',[event[-1] for event in before if event[0]=='send-keys'])
  self.assertEqual(len(self.inspect()['pending_questions']),1);self.assertNotIn('Fixture complete.',self.screen())
  self.assertIn('may already be in Terminal',self.answer(payload,success=False));self.assertEqual(self.events(),before)
 def test_tabs_multi_other_and_exact_native_receipt(self):
  self.assertTrue(self.card['can_answer'],self.card['answer_unavailable_reason'])
  payload=self.payload();receipt=self.answer(payload);self.assertEqual(receipt['status'],'answered')
  self.assertEqual(self.answer(payload),receipt)
  self.assertIn('Fixture complete.',self.wait_for(lambda:self.screen() if 'Fixture complete.' in self.screen() else ''))
 def test_wrapped_text_and_key_hints_at_a_narrow_terminal(self):
  self.assertTrue(self.card['can_answer'],self.card['answer_unavailable_reason'])
  payload=self.payload();receipt=self.answer(payload);self.assertEqual(receipt['status'],'answered')
  self.assert_native_answer(payload);self.assertEqual(self.answer(payload),receipt)
 def test_gutter_on_questions_and_review_confirms_exact_native_receipt(self):
  self.enable_gutter_capture()
  self.assertTrue(self.inspect()['pending_questions'][0]['can_answer'])
  payload=self.payload();receipt=self.answer(payload);self.assertEqual(receipt['status'],'answered')
  self.assertEqual(self.answer(payload),receipt)
  observed=(self.root/'gutter-observed').read_text().splitlines()
  self.assertIn('question',observed);self.assertIn('review',observed)
 def test_retry_from_gutter_review_keeps_exact_choices_and_confirms_once(self):
  self.enable_gutter_capture()
  (self.root/'changed-review').touch();first=self.payload()
  self.assertIn('delivery uncertain',self.answer(first,success=False))
  (self.root/'changed-review').unlink()
  before=(self.root/'sent-keys').read_text().splitlines()
  self.assertIn('may already be in Terminal',self.answer(first,success=False))
  self.assertEqual(before,(self.root/'sent-keys').read_text().splitlines())
  retry=dict(first,request_id=str(uuid.uuid4()))
  receipt=self.answer(retry);self.assertEqual(receipt['status'],'answered')
  keys=(self.root/'sent-keys').read_text().splitlines()[len(before):]
  self.assertGreaterEqual([json.loads(row)[-1] for row in keys].count('C-u'),2)
  self.assert_native_answer(retry)
  custom=[answer['text'] for answer in first['answers'] if answer['text']]
  self.assert_pastes(custom+custom)
  self.assertEqual(self.answer(retry),receipt)
  self.assertEqual(keys,(self.root/'sent-keys').read_text().splitlines()[len(before):])
 def test_gutter_review_can_change_preselected_single_choices(self):
  self.enable_gutter_capture()
  self.key('2');self.key('1');self.key('3');self.key('Tab')
  self.key('1');self.key('2');self.key('Tab')
  self.wait_for(lambda:'Review your answers' in self.screen())
  payload=self.payload();payload['answers'][0]['selected_option_ids']=['opt_0_0']
  payload['answers'][2]=dict(question_id='q_2',selected_option_ids=['opt_2_1'],text='')
  payload['answers'][3]=dict(question_id='q_3',selected_option_ids=['opt_3_1'],text='')
  receipt=self.answer(payload);self.assertEqual(receipt['status'],'answered')
  self.assertEqual(self.answer(payload),receipt)
 def test_single_question_direct_submission(self):
  self.assertTrue(self.card['can_answer'],self.card['answer_unavailable_reason']);self.assertEqual(self.answer()['status'],'answered')
 def test_stale_request_sends_no_input(self):
  self.enable_gutter_capture(False);before=self.screen()
  for field,value in [('expected_question_hash','0'*64),('expected_run_id',str(uuid.uuid4())),('expected_conversation_id',str(uuid.uuid4())),('question_id','different-question')]:
   with self.subTest(field=field):self.answer(self.payload(**{field:value}),success=False);self.assertEqual(self.events(),[])
  self.assertEqual(before.partition('←')[2],self.screen().partition('←')[2])
 def test_preselected_options_are_reconciled_in_order(self):
  self.key('Tab');self.key('3');self.key('1');self.key('2');self.key('Left')
  self.assertEqual(self.answer()['status'],'answered')
 def test_foreign_other_draft_is_overridden_by_explicit_preset(self):
  self.enable_gutter_capture(False);self.native_draft('Foreign native draft','middle');self.key('Enter');self.reset_events()
  before=self.screen();card=self.inspect()['pending_questions'][0]
  self.assertTrue(card['can_answer'],card['answer_unavailable_reason'])
  self.assertEqual(self.events(),[]);self.assertEqual(self.screen().partition('←')[2],before.partition('←')[2])
  payload=self.payload();self.assertEqual(self.answer(payload)['status'],'answered');self.assert_native_answer(payload)
  self.assertIn('C-u',[event[-1] for event in self.events() if event[0]=='send-keys'])
  self.assert_pastes([answer['text'] for answer in payload['answers'] if answer['text']])
 def test_committed_matching_other_is_cleared_and_pasted_once(self):
  payload=self.payload();payload['answers'][0]=dict(question_id='q_0',selected_option_ids=[],text='GUI   committed answer')
  self.enable_gutter_capture(False);self.native_draft(payload['answers'][0]['text']);self.key('Enter');self.reset_events()
  self.assertTrue(self.inspect()['pending_questions'][0]['can_answer']);self.assertEqual(self.events(),[])
  self.assertEqual(self.answer(payload)['status'],'answered');self.assert_native_answer(payload)
  self.assert_pastes([answer['text'] for answer in payload['answers'] if answer['text']])
 def test_existing_multi_other_is_overridden_and_presets_reconciled(self):
  self.key('Tab');self.enable_gutter_capture(False)
  self.native_draft('Native multi   draft '+('wrapped detail '*12),'middle',('Down',)*3);self.reset_events()
  payload=self.payload();payload['answers'][1]['text']='GUI multi   answer'
  self.assertTrue(self.inspect()['pending_questions'][0]['can_answer'])
  self.assertEqual(self.answer(payload)['status'],'answered');self.assert_native_answer(payload)
  self.assert_pastes([answer['text'] for answer in payload['answers'] if answer['text']])
 def test_existing_later_tab_other_is_overridden(self):
  self.key('Tab');self.key('Tab');self.enable_gutter_capture(False)
  self.native_draft('Old later-tab answer','start');self.reset_events()
  payload=self.payload();self.assertTrue(self.inspect()['pending_questions'][0]['can_answer'])
  self.assertEqual(self.answer(payload)['status'],'answered');self.assert_native_answer(payload)
  self.assert_pastes([answer['text'] for answer in payload['answers'] if answer['text']])
 def test_existing_review_custom_answers_can_be_changed(self):
  self.key('2');self.key('1');self.key('3');self.key('Tab')
  # Both existing answers normalize to the GUI form, but their literal bytes
  # differ. Reusing a normalized Review would submit the wrong native receipt.
  self.native_draft('Привет   from Zerus');self.key('Enter')
  self.native_draft('  Browser check  ',choice=('Down',)*2);self.key('Up');self.key('Tab')
  self.wait_for(lambda:'Review your answers' in self.screen());self.enable_gutter_capture(False);self.reset_events()
  payload=self.payload();self.assertEqual(self.answer(payload)['status'],'answered');self.assert_native_answer(payload)
  self.assert_pastes([answer['text'] for answer in payload['answers'] if answer['text']])
 def test_review_custom_preset_label_is_cleared_for_exact_gui_preset(self):
  self.native_draft('  Preview  ');self.key('Enter')
  self.key('1');self.key('3');self.key('Tab');self.key('2');self.key('2');self.key('Tab')
  self.wait_for(lambda:'Review your answers' in self.screen());self.enable_gutter_capture(False);self.reset_events()
  payload=self.payload()
  payload['answers'][2]=dict(question_id='q_2',selected_option_ids=['opt_2_1'],text='')
  payload['answers'][3]=dict(question_id='q_3',selected_option_ids=['opt_3_1'],text='')
  self.assertEqual(self.answer(payload)['status'],'answered');self.assert_native_answer(payload)
  self.assert_pastes([])
  self.assertIn('C-u',[event[-1] for event in self.events() if event[0]=='send-keys'])
 def channel(self):
  path=self.wait_for(lambda:next(iter(self.state.glob('native_answers/*/*/waiting.json')),None))
  return json.loads(path.read_text()),path.parent
 def channel_card(self):
  return self.wait_for(lambda:next((card for card in self.inspect()['pending_questions'] if card.get('answer_transport')=='claude_hook'),None))
 def height(self):return self.tmux('display-message','-p','-t',self.pane,'#{window_height}').strip()
 def running(self,pid):
  try:os.kill(pid,0);return True
  except ProcessLookupError:return False
 def test_channel_tall_question_answered_at_80x24_without_growth(self):
  card=self.channel_card();self.assertTrue(card['can_answer'])
  # A listing before the channel bound may have grown the pane; the channel needs no growth.
  self.tmux('resize-window','-t',self.pane,'-y','24');self.tmux('set-option','-w','-u','-t',self.pane,'window-size')
  self.assertEqual(self.channel_card()['answer_transport'],'claude_hook');self.assertEqual(self.height(),'24')
  payload=self.payload(answers=[dict(question_id='q_0',selected_option_ids=['opt_0_0','opt_0_2'],text='')])
  receipt=self.answer(payload);self.assertEqual((receipt['status'],receipt['transport']),('answered','claude_hook'))
  self.assert_native_answer(payload);self.assertEqual(self.height(),'24');self.assertEqual(self.answer(payload),receipt)
  waiting,folder=self.channel();self.assertEqual(json.loads((folder/'closed.json').read_text())['emitted'],True)
 def test_channel_multi_and_other(self):
  self.channel_card();payload=self.payload();payload['answers'][2]=dict(question_id='q_2',selected_option_ids=[],text='Later, after review | Привет')
  receipt=self.answer(payload);self.assertEqual((receipt['status'],receipt['transport']),('answered','claude_hook'))
  self.assert_native_answer(payload)
  self.assertIn('Fixture complete.',self.wait_for(lambda:self.screen() if 'Fixture complete.' in self.screen() else ''))
 def test_channel_single_terminal_answer_first(self):
  self.channel_card();waiting,folder=self.channel();self.assertTrue(self.running(waiting['pid']))
  self.key('Enter')
  self.wait_for(lambda:not self.running(waiting['pid']))
  self.assertIn(json.loads((folder/'closed.json').read_text())['reason'],['answered in Terminal','question resolved'])
  self.assertFalse((folder/'emitted.json').exists())
  # The fixture records no PostToolUse; Claude's transcript already has the answer.
  self.assertIn('already answered',self.answer(success=False))
 def test_channel_single_terminal_decline_ends_the_channel(self):
  self.channel_card();waiting,folder=self.channel();self.key('Escape')
  self.wait_for(lambda:not self.running(waiting['pid']))
  closed=json.loads((folder/'closed.json').read_text())
  self.assertIn(closed['reason'],['terminated','answered in Terminal','question resolved']);self.assertFalse(closed['emitted'])
  self.assertFalse((folder/'emitted.json').exists())
  # Before Claude writes its transcript the panel is simply gone; either way nothing is sent.
  self.assertRegex(self.answer(success=False),'already answered|rejected the question response|not fully visible')
 def test_channel_single_second_worker_leaves_the_owner(self):
  self.channel_card();waiting,folder=self.channel()
  event=dict(hook_event_name='PermissionRequest',tool_name='AskUserQuestion',session_id=self.conversation_id,tool_input=dict(questions=self.items))
  env=dict(self.env,HGS_SESSION=self.name,HGS_RUN_ID=self.run_id)
  started=time.monotonic()
  second=subprocess.run([str(fixtures.HGS),'__state','answer-channel'],input=json.dumps(event),text=True,capture_output=True,env=env,timeout=30)
  self.assertEqual((second.returncode,second.stdout,second.stderr),(0,'',''));self.assertLess(time.monotonic()-started,8)
  self.assertEqual(json.loads((folder/'waiting.json').read_text()),waiting);self.assertFalse((folder/'closed.json').exists())
  receipt=self.answer();self.assertEqual((receipt['status'],receipt['transport']),('answered','claude_hook'))
 def test_channel_single_claimed_answer_is_never_sent_again(self):
  self.channel_card();waiting,folder=self.channel();os.kill(waiting['pid'],signal.SIGKILL)
  self.wait_for(lambda:not self.running(waiting['pid']))
  # An answer the channel had claimed may still reach Claude: no transport sends another.
  (folder/'claimed.json').write_text(json.dumps(dict(request_id=str(uuid.uuid4()),answers={})))
  card=self.inspect()['pending_questions'][0]
  self.assertFalse(card['can_answer']);self.assertIn("Claude's answer channel",card['answer_unavailable_reason'])
  self.enable_gutter_capture(False)
  def question_region(screen):
   rows=screen.splitlines();first=next((i for i,row in enumerate(rows) if 'Which release should we prepare?' in row),-1)
   self.assertGreaterEqual(first,0)
   return first,rows[first:]
  before=question_region(self.screen());self.reset_events()
  self.assertIn("Claude's answer channel",self.answer(success=False))
  # The logo above the question animates; every question/editor/footer row and
  # its position must stay exact, and no input or staged text may be emitted.
  self.assertEqual(question_region(self.screen()),before);self.assertIn('Which release should we prepare?',self.screen())
  self.assertEqual(self.events(),[]);self.assertEqual(self.staged(),[])
 def test_channel_single_killed_falls_back_to_screen(self):
  self.channel_card();waiting,folder=self.channel();os.kill(waiting['pid'],signal.SIGKILL)
  self.wait_for(lambda:not self.running(waiting['pid']))
  card=self.wait_for(lambda:next((card for card in self.inspect()['pending_questions'] if card.get('answer_transport')=='claude_tui' and card['can_answer']),None))
  receipt=self.answer();self.assertEqual((receipt['status'],receipt['transport']),('answered','claude_tui'))
  self.assert_native_answer(self.payload())
 def test_channel_single_double_submit_answers_once(self):
  self.channel_card();first,second=self.payload(),self.payload(answers=[dict(question_id='q_0',selected_option_ids=['opt_0_0'],text='')])
  results={}
  def run(name,payload):results[name]=subprocess.run([str(fixtures.HGS),'answer',self.name,'--json'],input=json.dumps(payload),text=True,capture_output=True,env=self.env,timeout=60)
  threads=[threading.Thread(target=run,args=item) for item in (('first',first),('second',second))]
  for thread in threads:thread.start()
  for thread in threads:thread.join()
  self.assertEqual(sorted(result.returncode==0 for result in results.values()),[False,True],{k:(v.returncode,v.stdout,v.stderr) for k,v in results.items()})
  winner=next(name for name,result in results.items() if result.returncode==0)
  self.assert_native_answer(first if winner=='first' else second)
 def test_channel_single_deadline_leaves_the_panel(self):
  self.channel_card();waiting,folder=self.channel()
  self.wait_for(lambda:(folder/'closed.json').exists() and not self.running(waiting['pid']))
  self.assertEqual(json.loads((folder/'closed.json').read_text())['reason'],'deadline')
  card=self.wait_for(lambda:next((card for card in self.inspect()['pending_questions'] if card['can_answer']),None))
  self.assertEqual(card['answer_transport'],'claude_tui')
  receipt=self.answer();self.assertEqual((receipt['status'],receipt['transport']),('answered','claude_tui'))
  self.assert_native_answer(self.payload())
 def test_tall_question_grows_its_unviewed_80x24_pane(self):
  # Without the answer channel, inspection grows the pane no client views
  # until the exact screen match can run.
  card=self.wait_for(lambda:next((card for card in self.inspect()['pending_questions'] if card['can_answer']),None))
  self.assertEqual(card['answer_transport'],'claude_tui');self.assertGreater(int(self.height()),24)
  self.assertEqual(self.tmux('display-message','-p','-t',self.pane,'#{window_width}').strip(),'80')
  payload=self.payload(answers=[dict(question_id='q_0',selected_option_ids=['opt_0_1','opt_0_3'],text='')])
  receipt=self.answer(payload);self.assertEqual((receipt['status'],receipt['transport']),('answered','claude_tui'))
  self.assert_native_answer(payload)
 def test_missing_native_receipt_is_uncertain_and_never_retried(self):
  self.enable_gutter_capture(False)
  other=self.root/'unconfirmed';other.mkdir();path=other/(self.conversation_id+'.jsonl');path.write_text('')
  self.record=json.loads(self.record_path.read_text())
  self.record['transcript']=str(path);self.write_record();payload=self.payload()
  self.assertIn('delivery uncertain',self.answer(payload,success=False))
  before=self.events()
  self.assertIn('may already be in Terminal',self.answer(payload,success=False))
  self.assertEqual(self.events(),before)
  self.assertNotIn('Review your answers',self.screen())

if __name__=='__main__':unittest.main()
