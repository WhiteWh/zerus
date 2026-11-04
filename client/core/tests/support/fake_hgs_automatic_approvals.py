#!/usr/bin/env python3
import json,os,select,sqlite3,subprocess,sys,time
from pathlib import Path
# Execute through a symlink in a fresh private fixture root. Only approval-log
# calls reach real-hgs; inspect and answer are synthetic and never run an agent.
root=Path(__file__).parent
if not (root/'real-hgs').is_symlink() or not all((root/name).is_dir() for name in ['home','state','config']):
 raise RuntimeError('A private automatic-approval fixture root is required')
args=sys.argv[1:];host=args.pop(0)[1:] if args and args[0].startswith('@') else ''
def log(op,**data):
 with (root/'hgs-log.ndjson').open('a') as out:out.write(json.dumps({'op':op,'host':host,**data})+'\n')
def result(value):print(json.dumps(value));sys.exit()
def node():return '20000000-0000-4000-8000-000000000002' if host else '10000000-0000-4000-8000-000000000001'
def rows():
 return [{'name':p+'/example/permission','cmd':p,'agent':p,'run_id':p+'-'+(host or 'local')+'-run','conversation_id':p+'-'+(host or 'local')+'-conversation','created':10,'tracked':True,'state':'running','runtime_state':'live','process_state':'running','phase':'approval' if p=='claude' else 'input','machine':host or 'workstation'} for p in ['claude','opencode','dsh']]
def question(row):
 p=row['cmd'];item={'id':{'claude':'approval','opencode':'permission','dsh':'decision'}[p],'question':'Synthetic tool permission','body':'Synthetic fixture only. No command runs.','allow_other':False,'multi_select':False,'options':[{'id':'yes','label':'Yes','scope':'once'},{'id':'always','label':'Always allow','scope':'broader'}] if p=='claude' else [{'id':'once','label':'Allow once'},{'id':'always','label':'Always allow'}] if p=='opencode' else [{'id':'allow','label':'Allow once'},{'id':'deny','label':'Deny'}]}
 return {'approval':True,'can_answer':True,'approval_choices':True,'source':{'claude':'claude_tool_approval','opencode':'opencode_native','dsh':'dsh_native'}[p],'answer_transport':{'claude':'claude_tui','opencode':'native_opencode','dsh':'native_dsh'}[p],'kind':'opencode_permission' if p=='opencode' else '', 'question_id':p+'-'+(host or 'local')+'-permission','question_hash':'a'*64,'run_id':row['run_id'],'conversation_id':row['conversation_id'],'questions':[item]}
log(' '.join(args[:2]))
if args in [['-h'],['--help']]:print('Event stream ABI: watch-v1\nhgs approval-log begin|finish|list|detail');sys.exit()
if args[:1]==['approval-log']:
 data=sys.stdin.buffer.read();env={k:v for k,v in os.environ.items() if not k.startswith('HGS_')};env['HGS_CONFIG_DIR']=str(root/'config/hgs')
 child=subprocess.run([str((root/'real-hgs').resolve()),*args],input=data,capture_output=True,env=env)
 if child.returncode:print('Private fixture journal failed',file=sys.stderr);sys.exit(1)
 sys.stdout.buffer.write(child.stdout);sys.exit()
if args==['machine','ls']:result({'local':'workstation','machines':[{'alias':'example-peer','enabled':True,'source':'config','connection':{}}]})
if args[:2]==['account','ls']:result({'revision':'fixture','profiles':[]})
org={'schema':1,'initialized':True,'node_id':node(),'swarm_id':'fixture-swarm','digest':'fixture-catalog','organization':{'version':2,'default_project':'ungrouped','projects':[{'id':'ungrouped','name':'Inbox','folders':[],'sessions':[]}]},'versions':{},'peers':{},'conflicts':[]}
if args==['swarm','get']:result(org)
if args[:1]==['inspect']:
 row=next(r for r in rows() if r['name']==args[1]);result({**row,'pending_questions':[question(row)],'events':[],'cursor':1})
if args[:1]==['answer']:
 data=json.load(sys.stdin);row=next(r for r in rows() if r['name']==args[1]);q=question(row)
 for field,value in [('expected_run_id',row['run_id']),('expected_conversation_id',row['conversation_id']),('expected_question_hash',q['question_hash']),('question_id',q['question_id'])]:assert data[field]==value
 paths=[p for folder in ['state','home','config'] for p in (root/folder).rglob('journal.sqlite3')];assert len(paths)==1
 with sqlite3.connect(paths[0]) as db:
  found=db.execute('select status,question from attempts where request_id=?',(data['request_id'],)).fetchone();assert found and found[0]=='prepared';saved=json.loads(found[1]);assert saved['question']==q and saved['answers']==data['answers']
 assert data['answers'][0]['selected_option_ids']==[{'claude':'yes','opencode':'once','dsh':'allow'}[row['cmd']]]
 log('native-answer',request=data['request_id'],question=q['question_id'],durable_before_input=True)
 if host and row['cmd']=='dsh':print('delivery uncertain',file=sys.stderr);sys.exit(3)
 result({'status':'answered','name':row['name'],'request_id':data['request_id'],'question_id':q['question_id'],'question_hash':q['question_hash'],'run_id':row['run_id'],'conversation_id':row['conversation_id']})
if args[:1]==['ls']:result({'ok':True,'sessions':rows(),'peers':['example-peer'] if not host else []})
if args!=['watch','--json']:sys.exit(2)
seq=0
def frame(**v):
 global seq
 seq+=1;print(json.dumps({'v':1,'seq':seq,**v}),flush=True)
frame(t='hello',machine_id=node(),host=host or 'workstation',platform='linux',hgs_version='fixture',stream='auto-fixture-'+(host or 'local'),heartbeat_ms=10000,max_frame_bytes=1048576,topics=['catalog','catalog.meta','organization'],limits={'activity_subscriptions':16,'page_bytes':524288,'queue_bytes':4194304,'activity_tail':100,'activity_read':200,'control_line_bytes':65536})
frame(t='snapshot',topic='catalog',items=[{'key':'run:'+r['run_id'],'row':r} for r in rows()],page=0,last=True,cursor=1)
frame(t='snapshot',topic='catalog.meta',value={'host':host or 'workstation','ok':True,'metrics':{'platform':'linux'},'peers':['example-peer'] if not host else []},page=0,last=True,cursor=1)
last=time.monotonic()
while True:
 if select.select([sys.stdin],[],[],.05)[0]:
  line=b''
  while not line.endswith(b'\n'):
   part=os.read(sys.stdin.fileno(),1)
   if not part:break
   line+=part
  if not line:break
  control=json.loads(line)
  if control.get('topic')=='organization' and control.get('op')=='subscribe':
   summary=json.loads(json.dumps(org));summary.pop('versions');summary['organization']['projects']=['ungrouped'];p={**org['organization']['projects'][0],'session_chunks':0}
   frame(t='snapshot',topic='organization',items=[{'key':'organization','row':summary},{'key':'project:ungrouped','row':p}],page=0,last=True,cursor=1)
 if time.monotonic()-last>1:frame(t='heartbeat');last=time.monotonic()
