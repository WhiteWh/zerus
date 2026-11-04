#!/usr/bin/python3
"""Native Worktrees walk in private D-Bus/Xvfb/state, with fake hgs only."""
import argparse, ctypes, json, os, signal, subprocess, sys, tempfile, time
from pathlib import Path
REPO = Path(__file__).resolve().parents[2]
MACHINE = '11111111-1111-4111-8111-111111111111'
if '--worker' not in sys.argv:
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--app',type=Path,required=True);parser.add_argument('--shell-badge',action='store_true');parser.add_argument('--projects',action='store_true');parser.add_argument('--overview-native',action='store_true');parser.add_argument('--overview-ui',action='store_true');parser.add_argument('--swarm-native',action='store_true');parser.add_argument('--swarm-ui',action='store_true');parser.add_argument('--swarm-conflicts-ui',choices=['ordinary','large']);options=parser.parse_args();app=options.app.resolve();assert app.is_file()
    root=Path(tempfile.mkdtemp(prefix='zerus-worktree-review-'))
    for name in ('home','config','data','cache','runtime','tmp','tmux','state','bin','repo'):(root/name).mkdir(mode=0o700)
    (root/'repo/.git').mkdir()
    for tag in ('clean','ignored','unknown','changed','blocked'):
        (root/'repo'/tag).mkdir();(root/'repo'/tag/'fixture.txt').write_text('Synthetic fixture; fake hgs never deletes it.\n')
    frames=[
        {'v':1,'seq':1,'t':'hello','machine_id':MACHINE,'host':'example-host','hgs_version':'1.56.0','stream':'synthetic-review','heartbeat_ms':10000,'max_frame_bytes':1048576,'topics':['catalog','catalog.meta'],'limits':{'activity_subscriptions':16,'page_bytes':524288,'queue_bytes':4194304,'activity_tail':100,'activity_read':200,'control_line_bytes':65536,'git_subscriptions':64,'processes_subscriptions':16}},
        {'v':1,'seq':2,'t':'snapshot','topic':'catalog','items':[{'key':'run:probe','row':{'name':'codex/example/probe','cmd':'codex','agent':'codex','project':'example','tag':'probe','run_id':'run-probe','conversation_id':'conv-probe','tracked':True,'resumable':True,'state':'running','runtime_state':'live','phase':'idle','activity':'idle','cwd':str(root/'repo'),'cwd_canonical':str(root/'repo'),'git_root':str(root/'repo'),'git_branch':'main'}}],'page':0,'last':True,'cursor':1},
        {'v':1,'seq':3,'t':'snapshot','topic':'catalog.meta','value':{'host':'example-host','ok':True,'hgs_version':'1.56.0','projects':{},'peers':[]},'page':0,'last':True,'cursor':1},
        {'v':1,'seq':4,'t':'heartbeat','at':1793000000.0},
    ]
    if options.projects or options.overview_native or options.overview_ui or options.swarm_native or options.swarm_ui or options.swarm_conflicts_ui:
        organization=next(json.loads(line) for line in (REPO/'client/fixtures/replay.ndjson').read_text().splitlines() if json.loads(line).get('topic')=='organization')
        organization['seq']=4
        for item in organization['items']:
            if item['key']=='project:p-sample-project':
                item['row']['folders']=[{'id':'fixture-folder','machine':'example-host','machine_id':MACHINE,'path':str(root/'repo'),'name':'Example folder'}]
        frames[0]['topics'].append('organization');frames[3]['seq']=5;frames.insert(3,organization)
    if options.swarm_native or options.swarm_ui or options.swarm_conflicts_ui:
        import copy
        rows={item['key']:item['row'] for item in organization['items']}
        summary=rows['organization'];summary.update(node_id=MACHINE,swarm_id='fixture-swarm',initialized=True,machines=[{'id':MACHINE,'name':'example-host','local':True},{'id':'22222222-2222-4222-8222-222222222222','name':'fixture-peer','connection':'fixture-peer','local':False}],peers={'fixture-peer':{'node':'22222222-2222-4222-8222-222222222222','last_sync':1793000000}})
        if options.swarm_conflicts_ui:
            ids=summary['organization']['projects'];primary=ids[0];secondary=ids[1]
            peer='22222222-2222-4222-8222-222222222222'
            def conflict(field,values):
                return {'key':json.dumps(field,separators=(',',':')),'field':field,'selected':'v0','variants':[{'id':f'v{i}','actor':MACHINE if i%2==0 else peer,'value':v} for i,v in enumerate(values)]}
            ordinary=[conflict({'kind':'session','machine':peer,'session':'checkout-ui'},[primary,secondary]),conflict({'kind':'folder','id':'fixture-folder'},[{'name':'Runners','project':primary,'machine':peer,'path':'/srv/example/runners'},None]),conflict({'kind':'project','id':primary,'field':'alive'},[True,False])]
            if options.swarm_conflicts_ui=='large':
                ordinary[0]=conflict({'kind':'project','id':primary,'field':'name'},[f'Value {i}' for i in range(75)])
                ordinary += [conflict({'kind':'project','id':f'extra-{i}','field':'name'},['First name','Other name']) for i in range(3,130)]
            summary['conflicts']=ordinary
            (root/'swarm-conflicts-mode').write_text(options.swarm_conflicts_ui)
        if options.swarm_native:
            field={'kind':'project','id':'p-sample-project','field':'name'}
            summary['conflicts']=[{'key':json.dumps(field,separators=(',',':')),'field':field,'selected':'v0','variants':[{'id':'v0','actor':MACHINE,'value':'Sample project'},{'id':'v1','actor':MACHINE,'value':'Recovered project'}]}]
        if options.swarm_ui:
            summary['machines'].append({'id':'33333333-3333-4333-8333-333333333333','name':'Other computer','local':False})
            summary['peers']['fixture-peer']['tasks_outdated']=True
            frames[2]['value']['peers']=['fixture-uninitialized']
            (root/'swarm-ui').touch()
        state=copy.deepcopy(summary);state['organization']['projects']=[]
        for project in summary['organization']['projects']:
            value=copy.deepcopy(rows['project:'+project]);chunks=value.pop('session_chunks',0);value['sessions']=[session for index in range(chunks) for session in rows[f'members:{project}:{index}']['sessions']];state['organization']['projects'].append(value)
        state['versions']={key.removeprefix('version:'):copy.deepcopy(value) for key,value in rows.items() if key.startswith('version:')}
        (root/'swarm-state.json').write_text(json.dumps(state));(root/'swarm-native').touch()
    if options.shell_badge:
        first=frames[1]['items'][0]
        comparison=json.loads(json.dumps(first));comparison['key']='run:comparison';comparison['row'].update({'name':'codex/example/compare','run_id':'run-comparison','conversation_id':'conv-comparison','tag':'compare'})
        first['row'].update({'name':'sh/example/probe','cmd':'sh','agent':'sh'})
        frames[1]['items'].append(comparison)
    (root/'replay.ndjson').write_text(''.join(json.dumps(f)+'\n' for f in frames))
    if options.swarm_ui:
        import copy
        peer=copy.deepcopy(frames[:3]);peer[0].update(machine_id='44444444-4444-4444-8444-444444444444',host='fixture-uninitialized',topics=['catalog','catalog.meta']);peer[1]['items']=[];peer[2]['value'].update(host='fixture-uninitialized',peers=[])
        peer.append({'v':1,'seq':4,'t':'heartbeat','at':1793000000.0})
        (root/'peer-replay.ndjson').write_text(''.join(json.dumps(f)+'\n' for f in peer))

    if options.overview_ui:(root/'overview-ui').touch()
    fake=root/'bin/fake-hgs';fake.write_text('''#!/usr/bin/python3
import json,sys,time
from pathlib import Path
root=Path(__file__).resolve().parent.parent;args=sys.argv[1:]
with (root/'calls.jsonl').open('a') as f:f.write(json.dumps(args)+'\\n')
if args==['--help']:print('guarded-create-v1');sys.exit(0)
guarded=args[:2]==['--expect-machine','11111111-1111-4111-8111-111111111111']
if guarded:args=args[2:]
def arg(key):return args[args.index(key)+1]
def extra():return [f'extra-{i:03d}' for i in range(300)] if (root/'overview-ui').exists() else []
def row(tag):
 value={'path':str(root/'repo'/tag),'kind':'linked','available':tag!='missing','branch':tag,'head':'head-'+tag,'merged':tag in ('clean','ignored','changed'),'verdict':'missing' if tag=='missing' else 'ready','fingerprint':'fp-'+tag,'reasons':[],'ignored_count':0,'changes':0,'untracked':0,'integration':{'kind':'ancestor','base':'main'}}
 if tag=='ignored':value.update({'verdict':'review','ignored_count':1,'ignored_bytes':12288,'ignored_fingerprint':'ignored-proof','ignored':[{'path':'target/','bytes':12288,'complete':True}]})
 if tag=='blocked':value.update({'verdict':'blocked','reasons':[{'code':'processes','message':'A running program uses this folder.'}]})
 return value
if (root/'swarm-native').exists():
 state=json.loads((root/'swarm-state.json').read_text());peer='22222222-2222-4222-8222-222222222222'
 if args==['machine','ls']:
  aliases=['fixture-peer','fixture-uninitialized','fixture-failed'] if (root/'swarm-ui').exists() else ['fixture-peer']
  print(json.dumps({'local':'example-host','revision':'a'*64,'machines':[{'alias':a,'enabled':False} for a in aliases]}));sys.exit(0)
 if (root/'swarm-ui').exists() and args[:2]==['swarm','preview']:
  time.sleep(.8);alias=args[2]
  if alias=='fixture-failed':print('Fixture peer is unavailable.',file=sys.stderr);sys.exit(1)
  count=root/'peer-preview-count';n=int(count.read_text()) if count.exists() else 0
  if alias=='fixture-peer':n+=1;count.write_text(str(n))
  same=alias=='fixture-peer' and n>1
  print(json.dumps({'node_id':peer,'swarm_id':state['swarm_id'] if same else 'another-catalog','same_swarm':same,'initialized':alias!='fixture-uninitialized','projects':[] if alias=='fixture-uninitialized' else [{'id':'remote-project','name':'Remote project'},{'id':'ungrouped','name':'Inbox'}]}));sys.exit(0)
 if args==['swarm','get']:print(json.dumps(state));sys.exit(0)
 if args==['swarm','preview','fixture-peer']:print(json.dumps({'node_id':peer,'swarm_id':state['swarm_id'],'same_swarm':True,'initialized':True,'projects':[{'id':p['id'],'name':p['name']} for p in state['organization']['projects']]}));sys.exit(0)
 if args==['swarm','join','fixture-peer']:
  request=json.load(sys.stdin);assert request=={'node_id':peer,'swarm_id':state['swarm_id'],'project_map':{},'prefer_peer_memberships':(root/'swarm-ui').exists()};(root/'swarm-request.json').write_text(json.dumps(request));print(json.dumps({'joined':True,'snapshot':state}));sys.exit(0)
 if args==['swarm','sync']:print('{"schema":1}');sys.exit(0)
 if args==['swarm','resolve'] and (root/'swarm-conflicts-mode').exists():
  request=json.load(sys.stdin);original=next(c for c in state['conflicts'] if c['key']==request['key'])
  assert request['versions']==[v['id'] for v in original['variants']] and any(v['value']==request['value'] for v in original['variants'])
  (root/'resolve-request.json').write_text(json.dumps(request));state['conflicts']=[c for c in state['conflicts'] if c['key']!=request['key']];state['digest']='resolved-conflict-fixture';state['versions'][request['key']]=['resolved-op']
  (root/'swarm-state.json').write_text(json.dumps(state));print(json.dumps(state));sys.exit(0)
 if args==['swarm','resolve']:
  request=json.load(sys.stdin);assert request=={'key':state['conflicts'][0]['key'],'versions':['v0','v1'],'value':'Recovered project'}
  (root/'resolve-request.json').write_text(json.dumps(request));state['conflicts']=[];state['digest']='resolved-fixture';state['versions']={request['key']:['resolved-op']}
  for project in state['organization']['projects']:
   if project['id']=='p-sample-project':project['name']=request['value']
  (root/'swarm-state.json').write_text(json.dumps(state));print(json.dumps(state));sys.exit(0)
if args and args[0]=='worktrees' and (len(args)<2 or args[1].startswith('--')):
 path=arg('--path');assert Path(path).is_relative_to(root)
 tags=[t for t in ('clean','ignored','missing','unknown',*extra()) if not (root/('removed-'+t)).exists()]
 value={'state':'ok','path':path,'selected_root':str(root/'repo'),'common_dir':str(root/'repo/.git'),'sampled_at':time.time(),'worktrees':[{'path':str(root/'repo'),'kind':'main','available':True,'branch':'main'},*[row(t) for t in tags]]}
 print(json.dumps(value));sys.exit(0)
if args[:2]==['worktrees','review']:
 assert guarded and arg('--path')==str(root/'repo/.git')
 tags=[t for t in ('clean','ignored','unknown','changed','missing','blocked',*extra()) if not (root/('removed-'+t)).exists()]
 if '--worktree' in args:tags=[t for t in tags if str(root/'repo'/t)==arg('--worktree')]
 print(json.dumps({'state':'ok','common_dir':str(root/'repo/.git'),'worktrees':[row(t) for t in tags]}));sys.exit(0)
if args[:2]==['worktrees','inspect']:
 assert guarded and arg('--common-dir')==str(root/'repo/.git')
 path=arg('--path');assert Path(path).is_relative_to(root/'repo')
 file=arg('--file') if '--file' in args else '';assert file in ('','.','fixture.txt')
 value={'state':'ok','path':path,'common_dir':arg('--common-dir'),'files':[{'path':'fixture.txt','status':' M'}],'changes':{'text':'Synthetic file difference'},'comparison':{'text':'Synthetic base comparison'},'commits':{'text':'No additional commits.'},'review':{'sessions':['codex/example/probe']},'file':{'path':file,'offset':int(arg('--offset')) if '--offset' in args else 0}}
 if file=='.':value['file']['entries']=[{'path':'fixture.txt','directory':False,'symlink':False}]
 if file=='fixture.txt':value['file']['text']='Synthetic fixture preview'
 print(json.dumps(value));sys.exit(0)
if args[:2]==['worktrees','remove']:
 assert guarded and arg('--common-dir')==str(root/'repo/.git')
 path=Path(arg('--path'));assert path.is_relative_to(root);tag=path.name
 assert tag!='blocked' and '--discard-fingerprint' not in args and '--force' not in args
 assert '--preserve-archive-restore' not in args and '--process-fingerprint' not in args
 assert arg('--fingerprint')=='fp-'+tag
 assert ('--delete-branch' in args)==(tag in ('clean','ignored','changed'))
 if tag=='ignored':assert arg('--ignored-fingerprint')=='ignored-proof'
 if tag=='clean':time.sleep(1)
 if tag=='unknown':print('{}');print('Synthetic unknown after issue',file=sys.stderr);sys.exit(1)
 if tag=='changed':print(json.dumps({'request_id':arg('--request-id'),'path':str(path),'common_dir':arg('--common-dir'),'operation':'worktree_remove','outcome_version':1,'status':'kept','detail':'Worktree changed since the review. Review it again.'}));sys.exit(1)
 (root/('removed-'+tag)).touch()
 print(json.dumps({'request_id':arg('--request-id'),'path':str(path),'common_dir':arg('--common-dir'),'status':'forgotten' if tag=='missing' else 'removed'}));sys.exit(0)
if 'create' in args or 'start' in args or 'session-action' in args:raise RuntimeError('inspection attempted creation/mutation: '+repr(args))
print('{}')
''');fake.chmod(0o700)
    (root/'bus.conf').write_text('<busconfig><type>session</type><listen>unix:tmpdir='+str(root/'runtime')+'</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
    env={'PATH':str(root/'bin')+':/usr/bin:/bin','LANG':'C.UTF-8','HOME':str(root/'home'),'XDG_CONFIG_HOME':str(root/'config'),'XDG_DATA_HOME':str(root/'data'),'XDG_CACHE_HOME':str(root/'cache'),'XDG_RUNTIME_HOME':str(root/'runtime'),'TMPDIR':str(root/'tmp'),'TMUX_TMPDIR':str(root/'tmux'),'HGS_CONFIG_DIR':str(root/'config'),'HGS_STATE_DIR':str(root/'state'),'GDK_BACKEND':'x11','WAYLAND_DISPLAY':'zerus-nonexistent','WEBKIT_DISABLE_DMABUF_RENDERER':'1','NO_AT_BRIDGE':'1','GSETTINGS_BACKEND':'memory','ZERUS_CLIENT_HEADLESS':'1','ZERUS_CLIENT_REPLAY':str(root/'replay.ndjson'),'ZERUS_CLIENT_HGS':str(fake),'ZERUS_CLIENT_QUERY':'page=sessions&skin=zerus-light&worktree-review-smoke=1','ZERUS_CLIENT_PERF':'smoke'}
    if options.projects:env['ZERUS_CLIENT_QUERY']='page=projects&skin=zerus-light&worktree-review-smoke=projects'
    if options.overview_native:env['ZERUS_CLIENT_QUERY']='page=projects&skin=zerus-light&worktree-review-smoke=overview-native'
    if options.overview_ui:env['ZERUS_CLIENT_QUERY']='page=projects&skin=zerus-light&worktree-review-smoke=overview-ui'
    if options.swarm_native:env['ZERUS_CLIENT_QUERY']='page=projects&skin=zerus-light&project-swarm-smoke=native'
    if options.swarm_ui:
        env['ZERUS_CLIENT_QUERY']='page=projects&skin=zerus-light&project-swarm-ui-smoke=1'
        env['ZERUS_CLIENT_REPLAY']+=os.pathsep+'fixture-uninitialized='+str(root/'peer-replay.ndjson')
    if options.swarm_conflicts_ui:env['ZERUS_CLIENT_QUERY']='page=projects&skin=zerus-light&project-swarm-conflicts-smoke='+options.swarm_conflicts_ui
    if options.shell_badge:env['ZERUS_CLIENT_QUERY']='page=sessions&skin=zerus-light&shell-badge-smoke=1'
    print('Private Worktrees walk:',root,flush=True)
    child=subprocess.Popen(['/usr/bin/dbus-run-session','--config-file',str(root/'bus.conf'),'--','/usr/bin/xvfb-run','-a','-s','-screen 0 1440x900x24','/usr/bin/python3',str(Path(__file__).resolve()),'--worker',str(root),str(app),*(['--shell-badge'] if options.shell_badge else []),*(['--overview-native'] if options.overview_native else []),*(['--overview-ui'] if options.overview_ui else []),*(['--swarm-native'] if options.swarm_native else []),*(['--swarm-ui'] if options.swarm_ui else []),*(['--swarm-conflicts-ui',options.swarm_conflicts_ui] if options.swarm_conflicts_ui else [])],env=env,start_new_session=True)
    try:result=child.wait(timeout=110)
    except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGTERM);child.wait(timeout=5);raise
    raise SystemExit(result)
root,app=Path(sys.argv[2]),Path(sys.argv[3]);assert os.environ['HOME']==str(root/'home')
shell_badge='--shell-badge' in sys.argv
native='--overview-native' in sys.argv
overview_ui='--overview-ui' in sys.argv
swarm_native='--swarm-native' in sys.argv
swarm_ui='--swarm-ui' in sys.argv
conflicts_mode=sys.argv[sys.argv.index('--swarm-conflicts-ui')+1] if '--swarm-conflicts-ui' in sys.argv else None
resized=False
prefix='swarm-conflicts-smoke: ' if conflicts_mode else 'swarm-ui-smoke: ' if swarm_ui else 'swarm-smoke: ' if swarm_native else 'shell-badge-smoke: ' if shell_badge else 'worktree-review-smoke: '
env={k:v for k,v in os.environ.items() if k in {'PATH','LANG','HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_CACHE_HOME','XDG_RUNTIME_HOME','TMPDIR','TMUX_TMPDIR','HGS_CONFIG_DIR','HGS_STATE_DIR','GDK_BACKEND','WAYLAND_DISPLAY','WEBKIT_DISABLE_DMABUF_RENDERER','NO_AT_BRIDGE','GSETTINGS_BACKEND','DISPLAY','DBUS_SESSION_BUS_ADDRESS','XAUTHORITY'} or k.startswith('ZERUS_CLIENT_')}
x=ctypes.CDLL('libX11.so.6');t=ctypes.CDLL('libXtst.so.6');x.XOpenDisplay.argtypes=[ctypes.c_char_p];x.XOpenDisplay.restype=ctypes.c_void_p;x.XStringToKeysym.argtypes=[ctypes.c_char_p];x.XStringToKeysym.restype=ctypes.c_ulong;x.XKeysymToKeycode.argtypes=[ctypes.c_void_p,ctypes.c_ulong];x.XKeysymToKeycode.restype=ctypes.c_uint;x.XFlush.argtypes=[ctypes.c_void_p];t.XTestFakeKeyEvent.argtypes=[ctypes.c_void_p,ctypes.c_uint,ctypes.c_int,ctypes.c_ulong];display=x.XOpenDisplay(None);assert display
with (root/'app.log').open('wb') as log:
    child=subprocess.Popen([str(app)],cwd=REPO/'client',env=env,stdout=log,stderr=log);stages=set();deadline=time.monotonic()+100
    try:
        while time.monotonic()<deadline:
            output=(root/'app.log').read_text()
            for stage in (('CONFLICTS','CHOICE','CONFIRM','CANCEL','VALUES','PAGERS','MINIMUM','RESOLVE_HELD') if conflicts_mode else ('PRESELECTED','COMPUTERS','FILTERED','LOADING','MAP','UNINITIALIZED','FAILED','SAME','CONNECTED') if swarm_ui else ('PROJECTS','OPEN','PREVIEW','JOIN','CONFLICT_REVIEW','RESOLVE_HELD') if swarm_native else ('SHELL',) if shell_badge else ('PANEL','REVIEW','CONFIRMATION','REMOVING','OUTCOMES','RECOUNT','UNKNOWN_REOPEN','OVERVIEW','PAGER','MANUAL_REVIEW','INSPECTION','MANUAL_OUTCOMES')):
                if prefix+stage in output and stage not in stages:
                    stages.add(stage);time.sleep(.35);subprocess.run(['/usr/bin/import','-window','root',str(root/(stage.lower()+'.png'))],check=True,env=env)
            if conflicts_mode and prefix+'RESIZE_MINIMUM' in output and not resized:
                import re
                tree=subprocess.check_output(['/usr/bin/xwininfo','-root','-tree'],env=env,text=True)
                candidates=re.findall(r'(0x[0-9a-f]+) "[^"\n]*Zerus[^"\n]*"',tree)
                owned=[w for w in candidates if str(child.pid) in subprocess.check_output(['/usr/bin/xprop','-id',w,'_NET_WM_PID'],env=env,text=True)]
                assert owned,'owned private main window not found'
                x.XResizeWindow.argtypes=[ctypes.c_void_p,ctypes.c_ulong,ctypes.c_uint,ctypes.c_uint]
                x.XResizeWindow(display,int(owned[0],16),900,560);x.XFlush(display);resized=True
            for stage in ('ESCAPE_CONFIRM','ESCAPE_REVIEW','ESCAPE_SWARM','ESCAPE_SWARM_CONFIRM'):
                if (prefix if stage.startswith('ESCAPE_SWARM') else 'worktree-review-smoke: ')+stage+'\n' in output and stage not in stages:
                    stages.add(stage);key=x.XKeysymToKeycode(display,x.XStringToKeysym(b'Escape'));t.XTestFakeKeyEvent(display,key,1,0);t.XTestFakeKeyEvent(display,key,0,0);x.XFlush(display)
            if child.poll() is not None:break
            time.sleep(.03)
        else:raise RuntimeError('private Worktrees walk timed out')
        output=(root/'app.log').read_text()
        print('\n'.join(line for line in output.splitlines() if prefix in line));assert child.returncode==0,output
        calls=[json.loads(line) for line in (root/'calls.jsonl').read_text().splitlines()];effects=[a for a in calls if 'remove' in a]
        if conflicts_mode:
            assert not effects and calls.count(['swarm','resolve'])==1 and calls.count(['swarm','get'])==2,calls
            assert not any('create' in a or 'start' in a or 'session-action' in a for a in calls)
            assert all(prefix+name in output for name in (['CONFLICTS','CHOICE','VALUES','PAGERS','MINIMUM','RESOLVE_HELD'] if conflicts_mode=='large' else ['CONFLICTS','CHOICE','CONFIRM','CANCEL','RESOLVE_HELD']))
            assert (root/'resolve-request.json').is_file()
            print('PASS private Conflicts '+conflicts_mode+': exact pages/selection/confirmation/focus, one guarded fake Resolve, held source barrier, no replay/create/remove')
            raise SystemExit(0)
        if swarm_ui:
            assert not effects and not any('create' in a or 'start' in a or 'session-action' in a for a in calls)
            assert calls.count(['swarm','preview','fixture-peer'])==2 and calls.count(['swarm','preview','fixture-uninitialized'])==1 and calls.count(['swarm','preview','fixture-failed'])==1
            assert calls.count(['swarm','join','fixture-peer'])==1 and calls.count(['swarm','sync'])==1 and calls.count(['swarm','get'])==4,calls
            assert {'PRESELECTED','COMPUTERS','FILTERED','LOADING','MAP','UNINITIALIZED','FAILED','SAME','CONNECTED','ESCAPE_SWARM'} <= stages,stages
            print('PASS private mockup Swarm UI: read-only opening/preview, exact one Join/Sync, failed/uninitialized refusal, four fresh reads, no replay/creation/removal, real Escape/focus and Manage connections')
            raise SystemExit(0)
        if swarm_native:
            assert not effects and not any('create' in a or 'start' in a or 'session-action' in a for a in calls)
            assert calls.count(['swarm','preview','fixture-peer'])==1 and calls.count(['swarm','join','fixture-peer'])==1 and calls.count(['swarm','sync'])==1
            assert calls.count(['swarm','get'])==6 and calls.count(['swarm','resolve'])==1,calls
            assert (root/'swarm-request.json').is_file() and (root/'resolve-request.json').is_file() and 'PROJECTS' in stages
            assert all(prefix+stage in output for stage in ('CONFLICT_REVIEW','RESOLVE_HELD')),'final native stages missing'
            print('PASS main-window native Swarm API: one saved preview/Join/Sync/Resolve, six fresh reads, exact native review choice, source barrier held, no replay/creation/removal and private rendered Projects')
            raise SystemExit(0)
        if shell_badge:
            assert not effects and not any('create' in a or 'start' in a or 'session-action' in a for a in calls)
            assert 'SHELL' in stages
            print('PASS synthetic sh list/header Shell label, terminal glyph and existing default tint; Codex unchanged; no agent or operation started')
            raise SystemExit(0)
        assert len(effects)==(2 if native or overview_ui else 5),effects
        assert all('--expect-machine' in a for a in effects)
        assert len([a for a in effects if a[a.index('--path')+1].endswith('/unknown')])==1,'unknown replayed'
        assert all((root/'repo'/tag/'fixture.txt').is_file() for tag in ('clean','ignored','unknown','changed','blocked'))
        assert not any('create' in a or 'start' in a or 'session-action' in a for a in calls),'opening/inspection created work'
        if native or overview_ui:
            print('PASS native overview opaque selection, guarded manual review/removal, terminal unknown across refresh/reopen, unchanged synthetic files, no creation')
            raise SystemExit(0)
        print('PASS Worktrees menu/copy/New session without creation; focused missing review; explicit ignored/branch consent; exact guarded fake removal; kept and unknown outcomes; no replay; real Escape/focus; fixture files unchanged')
    finally:
        if child.poll() is None:child.terminate();child.wait(timeout=5)
